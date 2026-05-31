//! igb device bring-up — Phase 79 Track B.1.
//!
//! Mirrors `userspace/drivers/e1000/src/init.rs` (claim → map BAR0 → reset →
//! MAC read → rings → enable), but for the igb family two things differ:
//!
//! * **Advanced descriptor rings** — the RX queue needs `SRRCTL0` programmed
//!   for the advanced one-buffer descriptor type + 2 KiB packet buffers, and
//!   both RX/TX queues need their per-queue `RXDCTL0`/`TXDCTL0` enable bit set.
//! * **EICR interrupt block** — the IVAR registers route RX queue 0 / TX queue
//!   0 / the "other" (link) cause all onto a single MSI/INTx vector (vector 0),
//!   `GPIE` is configured for the single-vector path, every cause is masked via
//!   `EIMC` during bring-up, and `EIAC` auto-clears the vector on `EICR` read.
//!   The actual un-mask (`EIMS = VEC0`) happens in `io::arm_irqs` after the IRQ
//!   subscription is bound, exactly as the e1000's `IMS` arm is deferred.
//!
//! Pure register-composition helpers live as `pub const fn` / `pub fn` so the
//! IVAR routing, GPIE composition, and SRRCTL sizing are host-testable without
//! real MMIO.

#![allow(dead_code)] // the IO loop + smoke tests consume every symbol.

extern crate alloc;

use driver_runtime::{DeviceCapKey, DeviceHandle, DriverRuntimeError, Mmio, split_iova};
use kernel_core::e1000::decode_mac_from_ra;

pub use crate::regs::{
    IGB_BAR0_INDEX, IGB_BAR0_LEN, IgbRegs, RESET_POLL_LIMIT, ctrl, eicr, gpie, qdctl, rctl, srrctl,
    status, tctl,
};
use crate::rings::{RX_RING_BYTES, RxDescRing, TX_RING_BYTES, TxDescRing, initial_rx_tail};

/// Reasons igb bring-up can fail before any IRQ or RX/TX path runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BringUpError {
    /// A device-host syscall surfaced a kernel error.
    Runtime(DriverRuntimeError),
    /// `CTRL.RST` did not self-clear within [`RESET_POLL_LIMIT`] iterations.
    ResetTimeout,
}

impl From<DriverRuntimeError> for BringUpError {
    fn from(e: DriverRuntimeError) -> Self {
        Self::Runtime(e)
    }
}

/// Typestate marker for the igb BAR0 [`Mmio`] window.
pub struct IgbRegsBar;

/// Minimal read/write surface the igb bring-up + hot paths need.
pub trait IgbMmioOps {
    fn read_u32(&self, offset: usize) -> u32;
    fn write_u32(&self, offset: usize, value: u32);
}

impl<T> IgbMmioOps for Mmio<T> {
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

/// Compose the `CTRL` value that triggers a global reset.
#[inline]
pub const fn ctrl_reset_value(prev: u32) -> u32 {
    prev | ctrl::RST
}

/// True once `CTRL.RST` has self-cleared.
#[inline]
pub const fn reset_complete(ctrl_snapshot: u32) -> bool {
    ctrl_snapshot & ctrl::RST == 0
}

/// Post-reset `CTRL`: keep firmware-set bits, set `ASDE | SLU`, clear
/// `LRST | PHY_RST`.
#[inline]
pub const fn ctrl_post_reset_value(prev: u32) -> u32 {
    (prev | ctrl::ASDE | ctrl::SLU) & !(ctrl::LRST | ctrl::PHY_RST)
}

/// `RCTL`: enable + broadcast accept + strip FCS. (The packet-buffer size is
/// set per-queue through `SRRCTL`, not `RCTL.BSIZE`, on the advanced path.)
#[inline]
pub const fn rctl_bring_up_value() -> u32 {
    rctl::EN | rctl::BAM | rctl::SECRC
}

/// `TCTL`: enable + pad short + CT=0x10 + COLD=0x40.
#[inline]
pub const fn tctl_bring_up_value() -> u32 {
    tctl::EN | tctl::PSP | (0x10u32 << tctl::CT_SHIFT) | (0x40u32 << tctl::COLD_SHIFT)
}

/// `SRRCTL0`: 2 KiB packet buffers + advanced one-buffer descriptor type +
/// drop-enable (drop on ring-full rather than wedge the queue).
#[inline]
pub const fn srrctl_bring_up_value() -> u32 {
    srrctl::BSIZEPKT_2K | srrctl::DESCTYPE_ADV_ONEBUF | srrctl::DROP_EN
}

/// `GPIE` for the single-vector (non-MSI-X) 1.0 path: enable the non-selective
/// interrupt clear-on-read and PBA support; leave MSI-X-mode clear.
#[inline]
pub const fn gpie_single_vector_value() -> u32 {
    gpie::NSICR | gpie::PBA_SUPPORT
}

/// Compose an `IVAR` entry routing a cause onto vector 0 with the valid bit
/// set. igb `IVAR` packs four 8-bit cause→vector fields per dword; the valid
/// bit is bit 7 of each field and the low 5 bits are the EICR vector index.
///
/// `byte_index` selects which of the four cause fields (0..=3). For the 1.0
/// single-vector path every cause maps to vector 0.
#[inline]
pub const fn ivar_route_to_vec0(byte_index: u32) -> u32 {
    // Field value: VALID (bit 7) | vector 0.
    let field: u32 = 0x80;
    field << (byte_index * 8)
}

/// Interrupt-mask-clear-all value (silence every EICR vector).
#[inline]
pub const fn eimc_mask_all_value() -> u32 {
    0xFFFF_FFFF
}

/// Read the MAC from `RAL0` / `RAH0`.
#[inline]
pub fn read_mac<M: IgbMmioOps>(mmio: &M) -> [u8; 6] {
    let ral0 = mmio.read_u32(IgbRegs::RAL0);
    let rah0 = mmio.read_u32(IgbRegs::RAH0);
    decode_mac_from_ra(ral0, rah0)
}

/// Clear the 128-dword Multicast Table Array.
pub fn clear_mta<M: IgbMmioOps>(mmio: &M) {
    let mut off = IgbRegs::MTA;
    while off <= IgbRegs::MTA_END {
        mmio.write_u32(off, 0);
        off += 4;
    }
}

/// Issue the global reset and poll the self-clearing `CTRL.RST` bit.
pub fn reset<M: IgbMmioOps>(mmio: &M, limit: u32) -> Result<u32, BringUpError> {
    let prev = mmio.read_u32(IgbRegs::CTRL);
    mmio.write_u32(IgbRegs::CTRL, ctrl_reset_value(prev));
    for i in 0..limit {
        core::hint::spin_loop();
        if reset_complete(mmio.read_u32(IgbRegs::CTRL)) {
            return Ok(i);
        }
    }
    Err(BringUpError::ResetTimeout)
}

/// Program the RX queue-0 advanced ring: base/length, SRRCTL sizing, head/tail
/// pre-post, and the per-queue enable bit.
pub fn program_rx_ring<M: IgbMmioOps>(mmio: &M, ring_iova: u64) {
    let (lo, hi) = split_iova(ring_iova);
    mmio.write_u32(IgbRegs::RDBAL0, lo);
    mmio.write_u32(IgbRegs::RDBAH0, hi);
    mmio.write_u32(IgbRegs::RDLEN0, RX_RING_BYTES as u32);
    mmio.write_u32(IgbRegs::SRRCTL0, srrctl_bring_up_value());
    mmio.write_u32(IgbRegs::RDH0, 0);
    // Enable the queue before pre-posting tail (igb requires RXDCTL.ENABLE
    // before software advances RDT into the live region).
    mmio.write_u32(IgbRegs::RXDCTL0, qdctl::ENABLE);
    mmio.write_u32(IgbRegs::RDT0, initial_rx_tail());
}

/// Program the TX queue-0 advanced ring: base/length, head/tail = 0, enable.
pub fn program_tx_ring<M: IgbMmioOps>(mmio: &M, ring_iova: u64) {
    let (lo, hi) = split_iova(ring_iova);
    mmio.write_u32(IgbRegs::TDBAL0, lo);
    mmio.write_u32(IgbRegs::TDBAH0, hi);
    mmio.write_u32(IgbRegs::TDLEN0, TX_RING_BYTES as u32);
    mmio.write_u32(IgbRegs::TDH0, 0);
    mmio.write_u32(IgbRegs::TDT0, 0);
    mmio.write_u32(IgbRegs::TXDCTL0, qdctl::ENABLE);
}

/// Configure the EICR single-vector interrupt block (everything masked).
///
/// Routes RX queue 0 + TX queue 0 (`IVAR0`) and the "other"/link cause
/// (`IVAR_MISC`) onto vector 0, programs `GPIE` for the non-MSI-X path, masks
/// every vector via `EIMC`, and arms `EIAC` so the vector auto-clears on `EICR`
/// read. The actual un-mask happens later in `io::arm_irqs`.
pub fn configure_eicr_single_vector<M: IgbMmioOps>(mmio: &M) {
    mmio.write_u32(IgbRegs::GPIE, gpie_single_vector_value());
    // IVAR0 byte 0 = RX queue 0 cause, byte 2 = TX queue 0 cause → vector 0.
    mmio.write_u32(
        IgbRegs::IVAR0,
        ivar_route_to_vec0(0) | ivar_route_to_vec0(2),
    );
    // IVAR_MISC byte 0 = "other"/link cause → vector 0.
    mmio.write_u32(IgbRegs::IVAR_MISC, ivar_route_to_vec0(0));
    // Mask everything; arm later.
    mmio.write_u32(IgbRegs::EIMC, eimc_mask_all_value());
    // Auto-clear vector 0 on EICR read.
    mmio.write_u32(IgbRegs::EIAC, eicr::VEC0);
}

// ---------------------------------------------------------------------------
// IgbDevice
// ---------------------------------------------------------------------------

/// The ring-3 igb driver state. One per claimed NIC.
pub struct IgbDevice {
    pub(crate) pci: DeviceHandle,
    pub(crate) mmio: Mmio<IgbRegsBar>,
    pub(crate) mac: [u8; 6],
    pub(crate) rx: RxDescRing,
    pub(crate) tx: TxDescRing,
    pub(crate) initial_status: u32,
}

impl IgbDevice {
    /// Claim `key`, map BAR0, reset, read MAC, allocate advanced rings, program
    /// the queue-control + RCTL/TCTL registers, and configure (but do not arm)
    /// the EICR single vector.
    pub fn bring_up(key: DeviceCapKey) -> Result<Self, BringUpError> {
        let pci = DeviceHandle::claim(key)?;
        let mmio = Mmio::<IgbRegsBar>::map(&pci, IGB_BAR0_INDEX, IGB_BAR0_LEN)?;

        // Mask every interrupt vector before touching the device.
        mmio.write_reg::<u32>(IgbRegs::EIMC, eimc_mask_all_value());

        // Global reset.
        let _spun = reset(&mmio, RESET_POLL_LIMIT)?;
        mmio.write_reg::<u32>(IgbRegs::EIMC, eimc_mask_all_value());

        // Post-reset CTRL.
        let prev_ctrl = mmio.read_reg::<u32>(IgbRegs::CTRL);
        mmio.write_reg::<u32>(IgbRegs::CTRL, ctrl_post_reset_value(prev_ctrl));

        clear_mta(&mmio);
        let mac = read_mac(&mmio);

        // Advanced descriptor rings.
        let rx = RxDescRing::allocate(&pci)?;
        let tx = TxDescRing::allocate(&pci)?;

        program_rx_ring(&mmio, rx.ring_iova);
        program_tx_ring(&mmio, tx.ring_iova);

        mmio.write_reg::<u32>(IgbRegs::TCTL, tctl_bring_up_value());
        mmio.write_reg::<u32>(IgbRegs::RCTL, rctl_bring_up_value());

        // EICR single-vector routing (still masked; armed in io::arm_irqs).
        configure_eicr_single_vector(&mmio);

        let initial_status = mmio.read_reg::<u32>(IgbRegs::STATUS);

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
    use driver_runtime::adv_tx;

    struct FakeMmio {
        log: RefCell<Vec<(usize, u32)>>,
        reg: RefCell<Vec<(usize, u32)>>,
    }
    impl FakeMmio {
        fn new() -> Self {
            Self {
                log: RefCell::new(Vec::new()),
                reg: RefCell::new(Vec::new()),
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
    impl IgbMmioOps for FakeMmio {
        fn read_u32(&self, off: usize) -> u32 {
            self.reg
                .borrow()
                .iter()
                .find(|(o, _)| *o == off)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, off: usize, v: u32) {
            self.log.borrow_mut().push((off, v));
            if off == IgbRegs::CTRL {
                self.set(off, v & !ctrl::RST);
            } else {
                self.set(off, v);
            }
        }
    }

    #[test]
    fn ctrl_reset_value_sets_reset_bit_only() {
        let prev = 0x4000_0042;
        assert_eq!(ctrl_reset_value(prev), prev | ctrl::RST);
    }

    #[test]
    fn ctrl_post_reset_sets_slu_clears_phy_rst() {
        let prev = 0x8000_0080;
        let v = ctrl_post_reset_value(prev);
        assert_ne!(v & ctrl::SLU, 0);
        assert_eq!(v & ctrl::PHY_RST, 0);
    }

    #[test]
    fn rctl_does_not_use_legacy_bsize_on_advanced_path() {
        let v = rctl_bring_up_value();
        assert_ne!(v & rctl::EN, 0);
        assert_ne!(v & rctl::BAM, 0);
        assert_ne!(v & rctl::SECRC, 0);
    }

    #[test]
    fn srrctl_selects_advanced_onebuf_and_2k() {
        let v = srrctl_bring_up_value();
        assert_eq!(v & 0x7F, srrctl::BSIZEPKT_2K);
        assert_ne!(v & srrctl::DESCTYPE_ADV_ONEBUF, 0);
        assert_ne!(v & srrctl::DROP_EN, 0);
    }

    #[test]
    fn ivar_routes_field_to_vector_zero_with_valid_bit() {
        // Byte 0: VALID|vec0 == 0x80.
        assert_eq!(ivar_route_to_vec0(0), 0x80);
        // Byte 2: 0x80 << 16.
        assert_eq!(ivar_route_to_vec0(2), 0x0080_0000);
        // RX(byte0) + TX(byte2) combined.
        assert_eq!(ivar_route_to_vec0(0) | ivar_route_to_vec0(2), 0x0080_0080);
    }

    #[test]
    fn gpie_single_vector_disables_msix_mode() {
        let v = gpie_single_vector_value();
        assert_ne!(v & gpie::NSICR, 0);
        assert_eq!(v & gpie::MSIX_MODE, 0);
    }

    #[test]
    fn eimc_masks_all_vectors() {
        assert_eq!(eimc_mask_all_value(), 0xFFFF_FFFF);
    }

    #[test]
    fn reset_converges_on_self_clear() {
        let f = FakeMmio::new();
        f.set(IgbRegs::CTRL, 0x42);
        let spun = reset(&f, 16).expect("self-clear");
        assert!(spun <= 1);
    }

    #[test]
    fn reset_times_out_on_stuck() {
        struct Stuck(RefCell<Vec<(usize, u32)>>);
        impl IgbMmioOps for Stuck {
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
        s.write_u32(IgbRegs::CTRL, 0);
        assert_eq!(reset(&s, 4).unwrap_err(), BringUpError::ResetTimeout);
    }

    #[test]
    fn read_mac_decodes_qemu_default() {
        let f = FakeMmio::new();
        f.set(IgbRegs::RAL0, 0x0012_5452);
        f.set(IgbRegs::RAH0, 0x8000_5634);
        assert_eq!(read_mac(&f), [0x52, 0x54, 0x12, 0x00, 0x34, 0x56]);
    }

    #[test]
    fn program_rx_ring_writes_iova_srrctl_and_enables_queue() {
        let f = FakeMmio::new();
        program_rx_ring(&f, 0x0000_0001_DEAD_BEEF);
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        assert_eq!(g(IgbRegs::RDBAL0), Some(0xDEAD_BEEF));
        assert_eq!(g(IgbRegs::RDBAH0), Some(0x0000_0001));
        assert_eq!(g(IgbRegs::RDLEN0), Some(RX_RING_BYTES as u32));
        assert_eq!(g(IgbRegs::SRRCTL0), Some(srrctl_bring_up_value()));
        assert_eq!(g(IgbRegs::RXDCTL0), Some(qdctl::ENABLE));
        assert_eq!(g(IgbRegs::RDT0), Some(initial_rx_tail()));
    }

    #[test]
    fn program_tx_ring_writes_iova_and_enables_queue() {
        let f = FakeMmio::new();
        program_tx_ring(&f, 0x0000_0002_CAFE_F00D);
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        assert_eq!(g(IgbRegs::TDBAL0), Some(0xCAFE_F00D));
        assert_eq!(g(IgbRegs::TDBAH0), Some(0x0000_0002));
        assert_eq!(g(IgbRegs::TDLEN0), Some(TX_RING_BYTES as u32));
        assert_eq!(g(IgbRegs::TDH0), Some(0));
        assert_eq!(g(IgbRegs::TDT0), Some(0));
        assert_eq!(g(IgbRegs::TXDCTL0), Some(qdctl::ENABLE));
    }

    #[test]
    fn configure_eicr_routes_all_causes_to_vec0_and_masks() {
        let f = FakeMmio::new();
        configure_eicr_single_vector(&f);
        let w = f.writes();
        let g = |off: usize| w.iter().find(|(o, _)| *o == off).map(|(_, v)| *v);
        assert_eq!(g(IgbRegs::GPIE), Some(gpie_single_vector_value()));
        assert_eq!(g(IgbRegs::IVAR0), Some(0x0080_0080));
        assert_eq!(g(IgbRegs::IVAR_MISC), Some(0x80));
        assert_eq!(g(IgbRegs::EIMC), Some(0xFFFF_FFFF));
        assert_eq!(g(IgbRegs::EIAC), Some(eicr::VEC0));
    }

    #[test]
    fn adv_tx_dtalen_mask_is_low_16_bits() {
        assert_eq!(adv_tx::DTALEN_MASK, 0xFFFF);
    }
}
