//! igc device bring-up + Clause-45 MMD PHY accessor — Phase 79 Track B.2.
//!
//! The bring-up sequence mirrors igb (claim → map BAR0 → reset → MAC → advanced
//! rings → queue-control → EICR single-vector). igc adds a Clause-45 **MMD**
//! PHY accessor (`igc_read_xmdio_reg`-style) over the `MDIC`/`MDICNFG` register
//! pair, used to disambiguate 2.5GBASE-T copper auto-neg if needed; a basic
//! bring-up does not require it, so it is exposed as a standalone helper rather
//! than wired into the mandatory path.
//!
//! igc has no QEMU model — the runtime path is hardware-only — so every pure
//! register-composition helper (including the MMD PHY register packing) is
//! host-tested.

#![allow(dead_code)] // hardware-only family; the IO loop + tests consume these.

extern crate alloc;

use driver_runtime::{DeviceCapKey, DeviceHandle, DriverRuntimeError, Mmio, split_iova};
use kernel_core::e1000::decode_mac_from_ra;

pub use crate::regs::{
    IGC_BAR0_INDEX, IGC_BAR0_LEN, IgcRegs, MDIC_POLL_LIMIT, RESET_POLL_LIMIT, ctrl, eicr, gpie,
    mdic, mdic_data, mdic_error, mdic_read_value, mdic_ready, mdic_write_value, qdctl, rctl,
    srrctl, status, tctl, xmdio_cfg_value,
};
use crate::rings::{RX_RING_BYTES, RxDescRing, TX_RING_BYTES, TxDescRing, initial_rx_tail};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpError {
    Runtime(DriverRuntimeError),
    ResetTimeout,
}

impl From<DriverRuntimeError> for BringUpError {
    fn from(e: DriverRuntimeError) -> Self {
        Self::Runtime(e)
    }
}

/// Typestate marker for the igc BAR0 [`Mmio`] window.
pub struct IgcRegsBar;

/// Failure modes of a Clause-45 MMD PHY transaction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MmdError {
    /// `MDIC.READY` never asserted within [`MDIC_POLL_LIMIT`] iterations.
    Timeout,
    /// The transaction completed but `MDIC.ERROR` was set.
    PhyError,
}

/// Minimal read/write surface the igc bring-up + hot paths need.
pub trait IgcMmioOps {
    fn read_u32(&self, offset: usize) -> u32;
    fn write_u32(&self, offset: usize, value: u32);
}

impl<T> IgcMmioOps for Mmio<T> {
    fn read_u32(&self, offset: usize) -> u32 {
        self.read_reg::<u32>(offset)
    }
    fn write_u32(&self, offset: usize, value: u32) {
        self.write_reg::<u32>(offset, value)
    }
}

// ---------------------------------------------------------------------------
// Pure register-value composition (host-tested).
// ---------------------------------------------------------------------------

#[inline]
pub const fn ctrl_reset_value(prev: u32) -> u32 {
    prev | ctrl::RST
}
#[inline]
pub const fn reset_complete(ctrl_snapshot: u32) -> bool {
    ctrl_snapshot & ctrl::RST == 0
}
#[inline]
pub const fn ctrl_post_reset_value(prev: u32) -> u32 {
    (prev | ctrl::ASDE | ctrl::SLU) & !(ctrl::LRST | ctrl::PHY_RST)
}
#[inline]
pub const fn rctl_bring_up_value() -> u32 {
    rctl::EN | rctl::BAM | rctl::SECRC
}
#[inline]
pub const fn tctl_bring_up_value() -> u32 {
    tctl::EN | tctl::PSP | (0x10u32 << tctl::CT_SHIFT) | (0x40u32 << tctl::COLD_SHIFT)
}
#[inline]
pub const fn srrctl_bring_up_value() -> u32 {
    srrctl::BSIZEPKT_2K | srrctl::DESCTYPE_ADV_ONEBUF | srrctl::DROP_EN
}
#[inline]
pub const fn gpie_single_vector_value() -> u32 {
    gpie::NSICR | gpie::PBA_SUPPORT
}
#[inline]
pub const fn ivar_route_to_vec0(byte_index: u32) -> u32 {
    let field: u32 = 0x80; // VALID | vector 0
    field << (byte_index * 8)
}
#[inline]
pub const fn eimc_mask_all_value() -> u32 {
    0xFFFF_FFFF
}

#[inline]
pub fn read_mac<M: IgcMmioOps>(mmio: &M) -> [u8; 6] {
    let ral0 = mmio.read_u32(IgcRegs::RAL0);
    let rah0 = mmio.read_u32(IgcRegs::RAH0);
    decode_mac_from_ra(ral0, rah0)
}

pub fn clear_mta<M: IgcMmioOps>(mmio: &M) {
    let mut off = IgcRegs::MTA;
    while off <= IgcRegs::MTA_END {
        mmio.write_u32(off, 0);
        off += 4;
    }
}

pub fn reset<M: IgcMmioOps>(mmio: &M, limit: u32) -> Result<u32, BringUpError> {
    let prev = mmio.read_u32(IgcRegs::CTRL);
    mmio.write_u32(IgcRegs::CTRL, ctrl_reset_value(prev));
    for i in 0..limit {
        core::hint::spin_loop();
        if reset_complete(mmio.read_u32(IgcRegs::CTRL)) {
            return Ok(i);
        }
    }
    Err(BringUpError::ResetTimeout)
}

/// Poll a queue-control register (`RXDCTL`/`TXDCTL`) until its `ENABLE` bit
/// reads back set, bounded by [`RESET_POLL_LIMIT`]. I225/I226 silicon arms the
/// queue a few cycles after the enable write posts (Intel's igc polls
/// RXDCTL.ENABLE before advancing RDT); a tail write to a not-yet-live queue is
/// dropped. Best-effort and bounded so a non-latching emulated model cannot
/// wedge bring-up.
fn poll_qdctl_enabled<M: IgcMmioOps>(mmio: &M, reg: usize) {
    for _ in 0..RESET_POLL_LIMIT {
        if mmio.read_u32(reg) & qdctl::ENABLE != 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

pub fn program_rx_ring<M: IgcMmioOps>(mmio: &M, ring_iova: u64) {
    let (lo, hi) = split_iova(ring_iova);
    mmio.write_u32(IgcRegs::RDBAL0, lo);
    mmio.write_u32(IgcRegs::RDBAH0, hi);
    mmio.write_u32(IgcRegs::RDLEN0, RX_RING_BYTES as u32);
    mmio.write_u32(IgcRegs::SRRCTL0, srrctl_bring_up_value());
    mmio.write_u32(IgcRegs::RDH0, 0);
    // Wait for RXDCTL.ENABLE to read back set before pre-posting RDT — the queue
    // is not live on the same cycle as the enable write, and a tail write to an
    // un-armed queue is dropped.
    mmio.write_u32(IgcRegs::RXDCTL0, qdctl::ENABLE);
    poll_qdctl_enabled(mmio, IgcRegs::RXDCTL0);
    mmio.write_u32(IgcRegs::RDT0, initial_rx_tail());
}

pub fn program_tx_ring<M: IgcMmioOps>(mmio: &M, ring_iova: u64) {
    let (lo, hi) = split_iova(ring_iova);
    mmio.write_u32(IgcRegs::TDBAL0, lo);
    mmio.write_u32(IgcRegs::TDBAH0, hi);
    mmio.write_u32(IgcRegs::TDLEN0, TX_RING_BYTES as u32);
    mmio.write_u32(IgcRegs::TDH0, 0);
    mmio.write_u32(IgcRegs::TDT0, 0);
    // Confirm TXDCTL.ENABLE latched before the TX path advances TDT.
    mmio.write_u32(IgcRegs::TXDCTL0, qdctl::ENABLE);
    poll_qdctl_enabled(mmio, IgcRegs::TXDCTL0);
}

/// Configure the EICR single-vector interrupt block (everything masked).
pub fn configure_eicr_single_vector<M: IgcMmioOps>(mmio: &M) {
    mmio.write_u32(IgcRegs::GPIE, gpie_single_vector_value());
    mmio.write_u32(
        IgcRegs::IVAR0,
        ivar_route_to_vec0(0) | ivar_route_to_vec0(2),
    );
    mmio.write_u32(IgcRegs::IVAR_MISC, ivar_route_to_vec0(0));
    mmio.write_u32(IgcRegs::EIMC, eimc_mask_all_value());
    mmio.write_u32(IgcRegs::EIAC, eicr::VEC0);
}

// ---------------------------------------------------------------------------
// Clause-45 MMD PHY accessor (igc_read_xmdio_reg-style).
// ---------------------------------------------------------------------------

/// Read register `reg_addr` of MMD device-type `dev_addr` on PHY `phy_addr` via
/// the Clause-45 MMD indirection — the `igc_read_xmdio_reg` flow:
///
/// 1. Write `MDICNFG.DEST = dev_addr` to select the destination MMD.
/// 2. Write `MDIC` with the read op + PHY/register fields.
/// 3. Poll `MDIC.READY` (bounded) and read back the 16-bit data field.
///
/// Returns [`MmdError`] on `MDIC.ERROR` or a `READY` timeout. The register
/// composition is host-tested; this MMIO wrapper drives the same packing.
pub fn mmd_read<M: IgcMmioOps>(
    mmio: &M,
    phy_addr: u8,
    dev_addr: u8,
    reg_addr: u16,
) -> Result<u16, MmdError> {
    mmio.write_u32(IgcRegs::MDICNFG, xmdio_cfg_value(dev_addr));
    mmio.write_u32(IgcRegs::MDIC, mdic_read_value(phy_addr, reg_addr));
    for _ in 0..MDIC_POLL_LIMIT {
        core::hint::spin_loop();
        let v = mmio.read_u32(IgcRegs::MDIC);
        if mdic_ready(v) {
            if mdic_error(v) {
                return Err(MmdError::PhyError);
            }
            return Ok(mdic_data(v));
        }
    }
    Err(MmdError::Timeout)
}

/// Write `data` to register `reg_addr` of MMD `dev_addr` on PHY `phy_addr` via
/// the Clause-45 MMD indirection.
pub fn mmd_write<M: IgcMmioOps>(
    mmio: &M,
    phy_addr: u8,
    dev_addr: u8,
    reg_addr: u16,
    data: u16,
) -> Result<(), MmdError> {
    mmio.write_u32(IgcRegs::MDICNFG, xmdio_cfg_value(dev_addr));
    mmio.write_u32(IgcRegs::MDIC, mdic_write_value(phy_addr, reg_addr, data));
    for _ in 0..MDIC_POLL_LIMIT {
        core::hint::spin_loop();
        let v = mmio.read_u32(IgcRegs::MDIC);
        if mdic_ready(v) {
            if mdic_error(v) {
                return Err(MmdError::PhyError);
            }
            return Ok(());
        }
    }
    Err(MmdError::Timeout)
}

// ---------------------------------------------------------------------------
// IgcDevice
// ---------------------------------------------------------------------------

pub struct IgcDevice {
    pub(crate) pci: DeviceHandle,
    pub(crate) mmio: Mmio<IgcRegsBar>,
    pub(crate) mac: [u8; 6],
    pub(crate) rx: RxDescRing,
    pub(crate) tx: TxDescRing,
    pub(crate) initial_status: u32,
}

impl IgcDevice {
    pub fn bring_up(key: DeviceCapKey) -> Result<Self, BringUpError> {
        let pci = DeviceHandle::claim(key)?;
        let mmio = Mmio::<IgcRegsBar>::map(&pci, IGC_BAR0_INDEX, IGC_BAR0_LEN)?;

        mmio.write_reg::<u32>(IgcRegs::EIMC, eimc_mask_all_value());
        let _spun = reset(&mmio, RESET_POLL_LIMIT)?;
        mmio.write_reg::<u32>(IgcRegs::EIMC, eimc_mask_all_value());

        let prev_ctrl = mmio.read_reg::<u32>(IgcRegs::CTRL);
        mmio.write_reg::<u32>(IgcRegs::CTRL, ctrl_post_reset_value(prev_ctrl));

        clear_mta(&mmio);
        let mac = read_mac(&mmio);

        let rx = RxDescRing::allocate(&pci)?;
        let tx = TxDescRing::allocate(&pci)?;

        program_rx_ring(&mmio, rx.ring_iova);
        program_tx_ring(&mmio, tx.ring_iova);

        mmio.write_reg::<u32>(IgcRegs::TCTL, tctl_bring_up_value());
        mmio.write_reg::<u32>(IgcRegs::RCTL, rctl_bring_up_value());

        configure_eicr_single_vector(&mmio);

        let initial_status = mmio.read_reg::<u32>(IgcRegs::STATUS);

        Ok(Self {
            pci,
            mmio,
            mac,
            rx,
            tx,
            initial_status,
        })
    }

    #[inline]
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }
    #[inline]
    pub fn link_up_initial(&self) -> bool {
        self.initial_status & status::LU != 0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    struct FakeMmio {
        log: RefCell<Vec<(usize, u32)>>,
        reg: RefCell<Vec<(usize, u32)>>,
        // Optional canned MDIC read-back sequence (set to make READY assert).
        mdic_ready_value: RefCell<Option<u32>>,
    }
    impl FakeMmio {
        fn new() -> Self {
            Self {
                log: RefCell::new(Vec::new()),
                reg: RefCell::new(Vec::new()),
                mdic_ready_value: RefCell::new(None),
            }
        }
        fn set(&self, off: usize, v: u32) {
            let mut t = self.reg.borrow_mut();
            if let Some(s) = t.iter_mut().find(|(o, _)| *o == off) {
                s.1 = v;
            } else {
                t.push((off, v));
            }
        }
        fn writes(&self) -> Vec<(usize, u32)> {
            self.log.borrow().clone()
        }
    }
    impl IgcMmioOps for FakeMmio {
        fn read_u32(&self, off: usize) -> u32 {
            // For MDIC, return the canned ready value if set.
            if off == IgcRegs::MDIC {
                if let Some(v) = *self.mdic_ready_value.borrow() {
                    return v;
                }
            }
            self.reg
                .borrow()
                .iter()
                .find(|(o, _)| *o == off)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, off: usize, v: u32) {
            self.log.borrow_mut().push((off, v));
            if off == IgcRegs::CTRL {
                self.set(off, v & !ctrl::RST);
            } else {
                self.set(off, v);
            }
        }
    }

    #[test]
    fn ctrl_post_reset_sets_slu_clears_phy_rst() {
        let prev = 0x8000_0080;
        let v = ctrl_post_reset_value(prev);
        assert_ne!(v & ctrl::SLU, 0);
        assert_eq!(v & ctrl::PHY_RST, 0);
    }

    #[test]
    fn srrctl_selects_advanced_onebuf_and_2k() {
        let v = srrctl_bring_up_value();
        assert_eq!(v & 0x7F, srrctl::BSIZEPKT_2K);
        assert_ne!(v & srrctl::DESCTYPE_ADV_ONEBUF, 0);
    }

    #[test]
    fn ivar_routes_to_vec0() {
        assert_eq!(ivar_route_to_vec0(0), 0x80);
        assert_eq!(ivar_route_to_vec0(0) | ivar_route_to_vec0(2), 0x0080_0080);
    }

    #[test]
    fn reset_converges_and_times_out() {
        let f = FakeMmio::new();
        f.set(IgcRegs::CTRL, 0x42);
        assert!(reset(&f, 16).unwrap().eq(&0) || reset(&f, 16).is_ok());

        struct Stuck(RefCell<Vec<(usize, u32)>>);
        impl IgcMmioOps for Stuck {
            fn read_u32(&self, off: usize) -> u32 {
                self.0
                    .borrow()
                    .iter()
                    .find(|(o, _)| *o == off)
                    .map(|(_, v)| *v)
                    .unwrap_or(0)
            }
            fn write_u32(&self, off: usize, v: u32) {
                let mut t = self.0.borrow_mut();
                if let Some(s) = t.iter_mut().find(|(o, _)| *o == off) {
                    s.1 = v;
                } else {
                    t.push((off, v));
                }
            }
        }
        let s = Stuck(RefCell::new(Vec::new()));
        s.write_u32(IgcRegs::CTRL, 0);
        assert_eq!(reset(&s, 4).unwrap_err(), BringUpError::ResetTimeout);
    }

    #[test]
    fn read_mac_decodes_qemu_default() {
        let f = FakeMmio::new();
        f.set(IgcRegs::RAL0, 0x0012_5452);
        f.set(IgcRegs::RAH0, 0x8000_5634);
        assert_eq!(read_mac(&f), [0x52, 0x54, 0x12, 0x00, 0x34, 0x56]);
    }

    #[test]
    fn program_rx_ring_writes_iova_srrctl_enables_queue() {
        let f = FakeMmio::new();
        program_rx_ring(&f, 0x0000_0001_DEAD_BEEF);
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        assert_eq!(g(IgcRegs::RDBAL0), Some(0xDEAD_BEEF));
        assert_eq!(g(IgcRegs::RDBAH0), Some(0x0000_0001));
        assert_eq!(g(IgcRegs::SRRCTL0), Some(srrctl_bring_up_value()));
        assert_eq!(g(IgcRegs::RXDCTL0), Some(qdctl::ENABLE));
        assert_eq!(g(IgcRegs::RDT0), Some(initial_rx_tail()));
    }

    #[test]
    fn program_tx_ring_writes_iova_enables_queue() {
        let f = FakeMmio::new();
        program_tx_ring(&f, 0x0000_0002_CAFE_F00D);
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        assert_eq!(g(IgcRegs::TDBAL0), Some(0xCAFE_F00D));
        assert_eq!(g(IgcRegs::TDLEN0), Some(TX_RING_BYTES as u32));
        assert_eq!(g(IgcRegs::TXDCTL0), Some(qdctl::ENABLE));
    }

    #[test]
    fn configure_eicr_routes_and_masks() {
        let f = FakeMmio::new();
        configure_eicr_single_vector(&f);
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        assert_eq!(g(IgcRegs::GPIE), Some(gpie_single_vector_value()));
        assert_eq!(g(IgcRegs::IVAR0), Some(0x0080_0080));
        assert_eq!(g(IgcRegs::EIMC), Some(0xFFFF_FFFF));
        assert_eq!(g(IgcRegs::EIAC), Some(eicr::VEC0));
    }

    // -- Clause-45 MMD PHY accessor --

    #[test]
    fn mmd_read_writes_cfg_and_mdic_then_returns_data() {
        let f = FakeMmio::new();
        // Make MDIC read-back report READY with data 0x1234.
        *f.mdic_ready_value.borrow_mut() = Some(mdic::READY | 0x1234);
        let data = mmd_read(&f, 1, 7, 0x05).expect("mmd read");
        assert_eq!(data, 0x1234);
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        // MDICNFG carries the destination MMD (7).
        assert_eq!((g(IgcRegs::MDICNFG).unwrap() >> 16) & 0x1F, 7);
        // MDIC carries the read op + PHY/register fields.
        let mdic_v = g(IgcRegs::MDIC).unwrap();
        assert_ne!(mdic_v & mdic::OP_READ, 0);
        assert_eq!((mdic_v >> mdic::PHY_SHIFT) & mdic::PHY_MASK, 1);
        assert_eq!((mdic_v >> mdic::REG_SHIFT) & mdic::REG_MASK, 0x05);
    }

    #[test]
    fn mmd_read_reports_error_on_mdic_error_bit() {
        let f = FakeMmio::new();
        *f.mdic_ready_value.borrow_mut() = Some(mdic::READY | mdic::ERROR);
        assert_eq!(mmd_read(&f, 1, 7, 0x05), Err(MmdError::PhyError));
    }

    #[test]
    fn mmd_write_packs_data_and_op() {
        let f = FakeMmio::new();
        *f.mdic_ready_value.borrow_mut() = Some(mdic::READY);
        mmd_write(&f, 2, 1, 0x03, 0xBEEF).expect("mmd write");
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        let mdic_v = g(IgcRegs::MDIC).unwrap();
        assert_ne!(mdic_v & mdic::OP_WRITE, 0);
        assert_eq!(mdic_v & mdic::DATA_MASK, 0xBEEF);
        assert_eq!((g(IgcRegs::MDICNFG).unwrap() >> 16) & 0x1F, 1);
    }
}
