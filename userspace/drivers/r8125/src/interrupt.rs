//! r8125 V2 32-bit interrupt block (Track D.1).
//!
//! The classic r8169 uses the 16-bit IntrMask (0x3C) / IntrStatus (0x3E). The
//! 8125/8126 replace that with a 32-bit "V2" interrupt block:
//!
//! * IMR_V2_CLEAR (0x150) — write 1s to *mask* (disable) sources,
//! * ISR_V2       (0x154) — status, write-1-to-clear,
//! * IMR_V2_SET   (0x158) — write 1s to *unmask* (enable) sources,
//! * INT_CFG0_8125(0x34)  — interrupt configuration enable.
//!
//! The whole interrupt subsystem version-branches on the chip being an 8125
//! (`MacVersion::is_8125`). The register offsets + the cause bits live in
//! `kernel_core::r8169`; this module wraps them with the MMIO sequence (host-
//! testable via a register-log fake) and the version-branch decision. It is
//! built in both the host-test and os-binary configurations because it is pure
//! logic + a trait seam — no `syscall_lib` dependency.

extern crate alloc;

use kernel_core::r8169 as hw;

/// V2 interrupt cause bits the driver arms (RX OK | TX OK | link change).
/// The 8125 ISR widens the classic low-bit semantics to 32 bits for the bits
/// we consume.
pub const V2_RX_OK: u32 = 0x0000_0001;
pub const V2_TX_OK: u32 = 0x0000_0004;
pub const V2_LINK_CHG: u32 = 0x0000_0020;

/// The set of V2 causes armed at bring-up.
#[inline]
pub fn v2_arm_mask() -> u32 {
    V2_RX_OK | V2_TX_OK | V2_LINK_CHG
}

/// Minimal MMIO surface the V2 interrupt sequence needs — mirrors the
/// `driver_runtime::Mmio` register API so production plugs the real BAR in and
/// host tests plug a register-log fake.
pub trait V2MmioOps {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
    fn write8(&self, offset: usize, value: u8);
}

/// Whether the V2 interrupt block (32-bit) should be used for `version`.
/// Classic GbE parts use the 16-bit block; 8125/8126 use V2.
#[inline]
pub fn uses_v2(version: hw::MacVersion) -> bool {
    version.is_8125()
}

/// Mask (disable) every V2 interrupt source — write all-1s to IMR_V2_CLEAR and
/// ack any pending status. Called before reconfiguring the device.
pub fn mask_all_v2<M: V2MmioOps>(mmio: &M) {
    mmio.write32(hw::REG_IMR_V2_CLEAR as usize, 0xFFFF_FFFF);
    // Ack any latched status (write-1-to-clear).
    let isr = mmio.read32(hw::REG_ISR_V2 as usize);
    mmio.write32(hw::REG_ISR_V2 as usize, isr);
}

/// Arm the V2 interrupt block: enable INT_CFG0, ack pending status, then unmask
/// the RX/TX/link causes via IMR_V2_SET.
pub fn arm_v2<M: V2MmioOps>(mmio: &M) {
    mmio.write8(hw::REG_INT_CFG0_8125 as usize, hw::INT_CFG0_ENABLE);
    let isr = mmio.read32(hw::REG_ISR_V2 as usize);
    mmio.write32(hw::REG_ISR_V2 as usize, isr);
    mmio.write32(hw::REG_IMR_V2_SET as usize, v2_arm_mask());
}

/// Read + write-1-clear the V2 interrupt status; returns the snapshot the
/// caller decodes for RX/TX/link causes.
pub fn ack_v2<M: V2MmioOps>(mmio: &M) -> u32 {
    let isr = mmio.read32(hw::REG_ISR_V2 as usize);
    mmio.write32(hw::REG_ISR_V2 as usize, isr);
    isr
}

/// Decode whether a V2 ISR snapshot indicates an RX-drain is warranted.
#[inline]
pub fn v2_rx_drain_needed(isr: u32) -> bool {
    isr & V2_RX_OK != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    struct FakeMmio {
        regs: RefCell<Vec<(usize, u32)>>,
        log: RefCell<Vec<(usize, u64)>>, // (offset, value) write log, width-agnostic
    }
    impl FakeMmio {
        fn new() -> Self {
            Self {
                regs: RefCell::new(Vec::new()),
                log: RefCell::new(Vec::new()),
            }
        }
        fn set(&self, off: usize, v: u32) {
            let mut r = self.regs.borrow_mut();
            if let Some(s) = r.iter_mut().find(|(o, _)| *o == off) {
                s.1 = v;
            } else {
                r.push((off, v));
            }
        }
        fn writes(&self) -> Vec<(usize, u64)> {
            self.log.borrow().clone()
        }
    }
    impl V2MmioOps for FakeMmio {
        fn read32(&self, off: usize) -> u32 {
            self.regs
                .borrow()
                .iter()
                .find(|(o, _)| *o == off)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        }
        fn write32(&self, off: usize, v: u32) {
            self.log.borrow_mut().push((off, v as u64));
            self.set(off, v);
        }
        fn write8(&self, off: usize, v: u8) {
            self.log.borrow_mut().push((off, v as u64));
        }
    }

    #[test]
    fn v2_register_offsets_are_the_8125_block() {
        assert_eq!(hw::REG_IMR_V2_CLEAR, 0x150);
        assert_eq!(hw::REG_ISR_V2, 0x154);
        assert_eq!(hw::REG_IMR_V2_SET, 0x158);
        assert_eq!(hw::REG_INT_CFG0_8125, 0x34);
    }

    #[test]
    fn uses_v2_branches_on_chip_version() {
        assert!(uses_v2(hw::MacVersion::Ver(61))); // 8125A
        assert!(uses_v2(hw::MacVersion::Ver(65))); // 8126A
        assert!(!uses_v2(hw::MacVersion::Ver(42))); // 8168GU (classic block)
        assert!(!uses_v2(hw::MacVersion::Ver(2))); // 8169
        assert!(!uses_v2(hw::MacVersion::Unknown));
    }

    #[test]
    fn mask_all_writes_all_ones_to_clear_reg() {
        let m = FakeMmio::new();
        m.set(hw::REG_ISR_V2 as usize, 0x0000_00FF);
        mask_all_v2(&m);
        let w = m.writes();
        // IMR_V2_CLEAR = all 1s.
        assert!(w.contains(&(hw::REG_IMR_V2_CLEAR as usize, 0xFFFF_FFFF)));
        // ISR write-1-clear echoes the latched status back.
        assert!(w.contains(&(hw::REG_ISR_V2 as usize, 0x0000_00FF)));
    }

    #[test]
    fn arm_v2_enables_cfg0_and_unmasks_causes() {
        let m = FakeMmio::new();
        arm_v2(&m);
        let w = m.writes();
        assert!(w.contains(&(hw::REG_INT_CFG0_8125 as usize, hw::INT_CFG0_ENABLE as u64)));
        assert!(w.contains(&(hw::REG_IMR_V2_SET as usize, v2_arm_mask() as u64)));
    }

    #[test]
    fn ack_v2_reads_and_write1clears() {
        let m = FakeMmio::new();
        m.set(hw::REG_ISR_V2 as usize, V2_RX_OK | V2_TX_OK);
        let isr = ack_v2(&m);
        assert_eq!(isr, V2_RX_OK | V2_TX_OK);
        assert!(
            m.writes()
                .contains(&(hw::REG_ISR_V2 as usize, (V2_RX_OK | V2_TX_OK) as u64))
        );
    }

    #[test]
    fn v2_arm_mask_covers_rx_tx_link() {
        let mask = v2_arm_mask();
        assert_ne!(mask & V2_RX_OK, 0);
        assert_ne!(mask & V2_TX_OK, 0);
        assert_ne!(mask & V2_LINK_CHG, 0);
        assert!(v2_rx_drain_needed(V2_RX_OK));
        assert!(!v2_rx_drain_needed(V2_TX_OK));
    }
}
