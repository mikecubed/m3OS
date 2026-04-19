//! E1000 device bring-up — Phase 55b Track E.2 (Red).
//!
//! Red commit: expose the symbol surface (pure helpers, `MmioOps`
//! seam, `BringUpError`) so the tests compile, but leave every helper
//! returning a placeholder so the assertions fail. The Green commit
//! replaces each helper body with the real register composition.

#![allow(dead_code)]

extern crate alloc;

use driver_runtime::{DriverRuntimeError, Mmio};
use kernel_core::e1000::{E1000Regs, decode_mac_from_ra};

use crate::rings::{RX_RING_BYTES, TX_RING_BYTES, initial_rdt, split_iova};

pub const E1000_BAR0_LEN: usize = 0x0002_0000;
pub const E1000_BAR0_INDEX: u8 = 0;
pub const RESET_POLL_LIMIT: u32 = 2_000_000;

/// Bring-up error surface. Wraps the authoritative
/// [`DriverRuntimeError`] plus a named reset-timeout mode.
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

/// Typestate marker for the BAR0 window.
pub struct E1000Regs128k;

/// Minimal read/write surface the bring-up sequence touches.
pub trait MmioOps {
    fn read_u32(&self, offset: usize) -> u32;
    fn write_u32(&self, offset: usize, value: u32);
}

impl<T> MmioOps for Mmio<T> {
    fn read_u32(&self, offset: usize) -> u32 {
        self.read_reg::<u32>(offset)
    }
    fn write_u32(&self, offset: usize, value: u32) {
        self.write_reg::<u32>(offset, value)
    }
}

// Red stubs — every helper returns a deliberately wrong value.

#[inline]
pub const fn ctrl_reset_value(_prev: u32) -> u32 {
    0
}
#[inline]
pub const fn reset_complete(_ctrl_snapshot: u32) -> bool {
    false
}
#[inline]
pub const fn ctrl_post_reset_value(_prev: u32) -> u32 {
    0
}
#[inline]
pub const fn rctl_bring_up_value() -> u32 {
    0
}
#[inline]
pub const fn tctl_bring_up_value() -> u32 {
    0
}
#[inline]
pub const fn tipg_bring_up_value() -> u32 {
    0
}
#[inline]
pub const fn imc_mask_all_value() -> u32 {
    0
}
#[inline]
pub const fn ims_bring_up_value() -> u32 {
    0
}

#[inline]
pub fn read_mac<M: MmioOps>(mmio: &M) -> [u8; 6] {
    let ral0 = mmio.read_u32(E1000Regs::RAL0);
    let rah0 = mmio.read_u32(E1000Regs::RAH0);
    // Red stub: zero the decoded output so the QEMU-default-MAC test fails.
    let _ = decode_mac_from_ra(ral0, rah0);
    [0; 6]
}

/// Clear the MTA. Red stub: no writes emitted, so the 128-dword test fails.
pub fn clear_mta<M: MmioOps>(_mmio: &M) {}

/// Issue reset. Red stub: always times out.
pub fn reset<M: MmioOps>(_mmio: &M, _limit: u32) -> Result<u32, BringUpError> {
    Err(BringUpError::ResetTimeout)
}

/// Program RX ring registers. Red stub: no writes emitted.
pub fn program_rx_ring<M: MmioOps>(_mmio: &M, _ring_iova: u64) {
    let _ = (RX_RING_BYTES, initial_rdt(), split_iova(0));
}

/// Program TX ring registers. Red stub: no writes emitted.
pub fn program_tx_ring<M: MmioOps>(_mmio: &M, _ring_iova: u64) {
    let _ = (TX_RING_BYTES, split_iova(0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use kernel_core::e1000::{E1000Regs, ctrl, irq_cause, rctl, tctl};

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
        fn set(&self, offset: usize, value: u32) {
            let mut table = self.reg.borrow_mut();
            if let Some(slot) = table.iter_mut().find(|(o, _)| *o == offset) {
                slot.1 = value;
            } else {
                table.push((offset, value));
            }
        }
        fn writes(&self) -> Vec<(usize, u32)> {
            self.log.borrow().clone()
        }
    }

    impl MmioOps for FakeMmio {
        fn read_u32(&self, offset: usize) -> u32 {
            self.reg
                .borrow()
                .iter()
                .find(|(o, _)| *o == offset)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, offset: usize, value: u32) {
            self.log.borrow_mut().push((offset, value));
            if offset == E1000Regs::CTRL {
                let stored = value & !ctrl::RST;
                self.set(offset, stored);
            } else {
                self.set(offset, value);
            }
        }
    }

    #[test]
    fn ctrl_reset_value_sets_only_the_reset_bit() {
        let prev = 0x4000_0042;
        let next = ctrl_reset_value(prev);
        assert_eq!(next, prev | ctrl::RST);
        assert_eq!(next & prev, prev);
    }

    #[test]
    fn reset_complete_detects_self_cleared_bit() {
        assert!(reset_complete(0));
        assert!(reset_complete(ctrl::SLU));
        assert!(!reset_complete(ctrl::RST));
        assert!(!reset_complete(ctrl::RST | ctrl::SLU));
    }

    #[test]
    fn ctrl_post_reset_value_matches_phase_55_composition() {
        let prev = 0x8000_0080;
        let expected = (prev | ctrl::ASDE | ctrl::SLU) & !(ctrl::LRST | ctrl::PHY_RST);
        assert_eq!(ctrl_post_reset_value(prev), expected);
        assert_eq!(ctrl_post_reset_value(prev) & ctrl::PHY_RST, 0);
        assert_ne!(ctrl_post_reset_value(prev) & ctrl::SLU, 0);
    }

    #[test]
    fn rctl_bring_up_value_matches_acceptance() {
        let value = rctl_bring_up_value();
        assert_ne!(value & rctl::EN, 0);
        assert_ne!(value & rctl::BAM, 0);
        assert_ne!(value & rctl::SECRC, 0);
        assert_eq!(value & rctl::BSIZE_1024, 0, "2048-byte buffers only");
        assert_eq!(value & rctl::BSIZE_512, 0);
        assert_eq!(value & rctl::BSIZE_256, 0);
    }

    #[test]
    fn tctl_bring_up_value_matches_acceptance() {
        let value = tctl_bring_up_value();
        assert_ne!(value & tctl::EN, 0);
        assert_ne!(value & tctl::PSP, 0);
        assert_eq!((value >> tctl::CT_SHIFT) & 0xFF, 0x10);
        assert_eq!((value >> tctl::COLD_SHIFT) & 0x3FF, 0x40);
    }

    #[test]
    fn imc_mask_all_silences_every_cause() {
        assert_eq!(imc_mask_all_value(), 0xFFFF_FFFF);
    }

    #[test]
    fn ims_bring_up_value_arms_rx_and_lsc_only() {
        let value = ims_bring_up_value();
        assert_ne!(value & irq_cause::RXT0, 0);
        assert_ne!(value & irq_cause::RXDMT0, 0);
        assert_ne!(value & irq_cause::RXO, 0);
        assert_ne!(value & irq_cause::LSC, 0);
        assert_eq!(value & irq_cause::TXDW, 0);
    }

    #[test]
    fn read_mac_matches_kernel_core_decoder_for_qemu_default() {
        let fake = FakeMmio::new();
        fake.set(E1000Regs::RAL0, 0x0012_5452);
        fake.set(E1000Regs::RAH0, 0x8000_5634);
        let mac = read_mac(&fake);
        assert_eq!(mac, [0x52, 0x54, 0x12, 0x00, 0x34, 0x56]);
    }

    #[test]
    fn reset_converges_after_one_iteration_on_fake_self_clear() {
        let fake = FakeMmio::new();
        fake.set(E1000Regs::CTRL, 0x0000_0042);
        let spun = reset(&fake, 16).expect("fake self-clears on first write");
        assert!(spun <= 1);
        let writes = fake.writes();
        let ctrl_writes: Vec<_> = writes
            .iter()
            .filter(|(o, _)| *o == E1000Regs::CTRL)
            .collect();
        assert_eq!(ctrl_writes.len(), 1);
        assert_ne!(ctrl_writes[0].1 & ctrl::RST, 0);
    }

    struct StuckMmio {
        reg: RefCell<Vec<(usize, u32)>>,
    }
    impl StuckMmio {
        fn new() -> Self {
            Self {
                reg: RefCell::new(Vec::new()),
            }
        }
    }
    impl MmioOps for StuckMmio {
        fn read_u32(&self, offset: usize) -> u32 {
            self.reg
                .borrow()
                .iter()
                .find(|(o, _)| *o == offset)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write_u32(&self, offset: usize, value: u32) {
            let mut table = self.reg.borrow_mut();
            if let Some(slot) = table.iter_mut().find(|(o, _)| *o == offset) {
                slot.1 = value;
            } else {
                table.push((offset, value));
            }
        }
    }

    #[test]
    fn reset_times_out_cleanly_on_stuck_nic() {
        let stuck = StuckMmio::new();
        stuck.write_u32(E1000Regs::CTRL, 0);
        let err = reset(&stuck, 4).expect_err("stuck NIC must not spin forever");
        assert_eq!(err, BringUpError::ResetTimeout);
    }

    #[test]
    fn clear_mta_writes_128_dwords_of_zero() {
        let fake = FakeMmio::new();
        clear_mta(&fake);
        let writes = fake.writes();
        let mta_writes: Vec<_> = writes
            .iter()
            .filter(|(o, _)| *o >= E1000Regs::MTA && *o <= E1000Regs::MTA_END)
            .collect();
        assert_eq!(mta_writes.len(), 128, "128 dwords of MTA to clear");
        for (off, v) in &mta_writes {
            assert_eq!(*v, 0);
            assert!(off.is_multiple_of(4));
        }
    }

    #[test]
    fn program_rx_ring_uses_iova_not_user_va() {
        let fake = FakeMmio::new();
        let iova: u64 = 0x0000_0001_DEAD_BEEF;
        program_rx_ring(&fake, iova);
        let writes = fake.writes();
        let rdbal = writes.iter().find(|(o, _)| *o == E1000Regs::RDBAL).unwrap().1;
        let rdbah = writes.iter().find(|(o, _)| *o == E1000Regs::RDBAH).unwrap().1;
        let rdlen = writes.iter().find(|(o, _)| *o == E1000Regs::RDLEN).unwrap().1;
        let rdh = writes.iter().find(|(o, _)| *o == E1000Regs::RDH).unwrap().1;
        let rdt = writes.iter().find(|(o, _)| *o == E1000Regs::RDT).unwrap().1;
        assert_eq!(rdbal, 0xDEAD_BEEF);
        assert_eq!(rdbah, 0x0000_0001);
        assert_eq!(rdlen, RX_RING_BYTES as u32);
        assert_eq!(rdh, 0);
        assert_eq!(rdt, initial_rdt());
        assert_eq!(rdt, (crate::rings::RX_RING_SIZE as u32) - 1);
    }

    #[test]
    fn program_tx_ring_uses_iova_not_user_va() {
        let fake = FakeMmio::new();
        let iova: u64 = 0x0000_0002_CAFE_F00D;
        program_tx_ring(&fake, iova);
        let writes = fake.writes();
        let tdbal = writes.iter().find(|(o, _)| *o == E1000Regs::TDBAL).unwrap().1;
        let tdbah = writes.iter().find(|(o, _)| *o == E1000Regs::TDBAH).unwrap().1;
        let tdlen = writes.iter().find(|(o, _)| *o == E1000Regs::TDLEN).unwrap().1;
        let tdh = writes.iter().find(|(o, _)| *o == E1000Regs::TDH).unwrap().1;
        let tdt = writes.iter().find(|(o, _)| *o == E1000Regs::TDT).unwrap().1;
        assert_eq!(tdbal, 0xCAFE_F00D);
        assert_eq!(tdbah, 0x0000_0002);
        assert_eq!(tdlen, TX_RING_BYTES as u32);
        assert_eq!(tdh, 0);
        assert_eq!(tdt, 0);
    }
}
