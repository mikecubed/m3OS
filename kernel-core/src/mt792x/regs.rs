//! mt792x WFDMA register offsets and bit-field helpers — Task A.3.
//!
//! All offsets are CPU-side MMIO byte offsets into BAR0. Values are verified
//! against the upstream mt76 kernel driver
//! (`drivers/net/wireless/mediatek/mt76/mt7921/`).
//!
//! Nothing in this file touches hardware; it is pure `const` math and
//! predicate functions suitable for host testing.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// BAR0 region bases
// ---------------------------------------------------------------------------

/// WFDMA0 engine base offset within BAR0.
pub const MT_WFDMA0_BASE: usize = 0xD4000;

/// WFDMA extended CSR region base (used for host-DMA control on mt7921+).
pub const MT_WFDMA_EXT_CSR_BASE: usize = 0xD7000;

// ---------------------------------------------------------------------------
// WFDMA0 register offsets (relative to BAR0, i.e. MT_WFDMA0_BASE + offset)
// ---------------------------------------------------------------------------

/// WFDMA0 software reset register.
/// Write [`RST_LOGIC_RST`] and/or [`RST_DMASHDL_ALL_RST`] to trigger a reset.
pub const MT_WFDMA0_RST: usize = 0xD4100; // MT_WFDMA0_BASE + 0x100

/// Logic reset bit in [`MT_WFDMA0_RST`] (bit 4).
pub const RST_LOGIC_RST: u32 = 1 << 4;

/// DMA shared-header (DMASHDL) full reset bit in [`MT_WFDMA0_RST`] (bit 5).
pub const RST_DMASHDL_ALL_RST: u32 = 1 << 5;

/// WFDMA0 host interrupt status register (32-bit, write-1-to-clear).
pub const MT_WFDMA0_HOST_INT_STA: usize = 0xD4200; // MT_WFDMA0_BASE + 0x200

/// WFDMA0 global DMA configuration register.
///
/// Programs TX/RX DMA enable and carries the TX/RX busy-status read-back bits
/// used by the reset-complete poll predicate.
pub const MT_WFDMA0_GLO_CFG: usize = 0xD4208; // MT_WFDMA0_BASE + 0x208

/// TX DMA enable bit in [`MT_WFDMA0_GLO_CFG`] (bit 0).
pub const TX_DMA_EN: u32 = 1 << 0;
/// TX DMA busy bit in [`MT_WFDMA0_GLO_CFG`] (bit 1, read-only).
pub const TX_DMA_BUSY: u32 = 1 << 1;
/// RX DMA enable bit in [`MT_WFDMA0_GLO_CFG`] (bit 2).
pub const RX_DMA_EN: u32 = 1 << 2;
/// RX DMA busy bit in [`MT_WFDMA0_GLO_CFG`] (bit 3, read-only).
pub const RX_DMA_BUSY: u32 = 1 << 3;

/// WFDMA0 TX ring pointer reset register.
pub const MT_WFDMA0_RST_DTX_PTR: usize = 0xD420C; // MT_WFDMA0_BASE + 0x20C

/// WFDMA0 RX ring pointer reset register.
pub const MT_WFDMA0_RST_DRX_PTR: usize = 0xD4280; // MT_WFDMA0_BASE + 0x280

// ---------------------------------------------------------------------------
// Connac TOP / firmware-ready registers
// ---------------------------------------------------------------------------
//
// These live in the connac CSR address space (`0x1800_0000` bus range), NOT the
// raw BAR0 window — the chip reaches them through a fixed reg-remap window
// (`mt7921_reg_map[]` / `__mt7921_reg_addr` upstream). The numeric **offset and
// mask** below are well-established from upstream mt76 (`mt7921/regs.h`,
// `mt7921/mcu.c` `mt7921_load_firmware`); the BAR0-relative window that maps
// this bus range, and the live ready-transition timing, are hardware-only
// (E.3/E.4 capture).

/// Hardware chip-id register, bus address `0x7001_0200` (upstream mt76
/// `mt76_connac_reg.h: MT_HW_CHIPID`), reached via the connac CSR reg-remap
/// window — NOT a raw BAR0 offset. The BAR0-relative offset is resolved on
/// hardware (E.3 capture); driver attachment is gated by the PCI device-ID
/// match, not this readback.
pub const MT_HW_CHIPID_BUS_ADDR: u32 = 0x7001_0200;

/// Connac TOP register base (bus address).
pub const MT_TOP_BASE: u32 = 0x1806_0000;

/// `MT_CONN_ON_MISC` (== `MT_TOP_MISC2`), bus address `0x1806_1140`. Polled for
/// the firmware-N9-ready bits after `FW_START_REQ`.
pub const MT_CONN_ON_MISC: u32 = MT_TOP_BASE + 0x1140;

/// Firmware-N9-ready field in `MT_CONN_ON_MISC` (`GENMASK(1, 0)`).
pub const MT_TOP_MISC2_FW_N9_RDY: u32 = 0x3;

/// Return `true` when a read of [`MT_CONN_ON_MISC`] reports the WM/N9 firmware
/// running (both [`MT_TOP_MISC2_FW_N9_RDY`] bits set).
#[inline]
pub fn fw_n9_ready(misc2: u32) -> bool {
    misc2 & MT_TOP_MISC2_FW_N9_RDY == MT_TOP_MISC2_FW_N9_RDY
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return `true` when the WFDMA0 DMA engine has come to a complete stop.
///
/// The driver polls this after de-asserting TX_DMA_EN / RX_DMA_EN to confirm
/// the hardware has drained all in-flight descriptors before touching ring
/// pointers. Both busy bits must be **clear** for the reset to be safe.
#[inline]
pub fn reset_complete(glo_cfg: u32) -> bool {
    glo_cfg & (TX_DMA_BUSY | RX_DMA_BUSY) == 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets() {
        // BAR0-absolute offsets (MT_WFDMA0_BASE + per-reg offset).
        assert_eq!(MT_WFDMA0_BASE, 0xD4000);
        assert_eq!(MT_WFDMA_EXT_CSR_BASE, 0xD7000);
        assert_eq!(MT_WFDMA0_RST, 0xD4100);
        assert_eq!(MT_WFDMA0_HOST_INT_STA, 0xD4200);
        assert_eq!(MT_WFDMA0_GLO_CFG, 0xD4208);
        assert_eq!(MT_WFDMA0_RST_DTX_PTR, 0xD420C);
        assert_eq!(MT_WFDMA0_RST_DRX_PTR, 0xD4280);
    }

    #[test]
    fn bit_constants() {
        assert_eq!(RST_LOGIC_RST, 1 << 4);
        assert_eq!(RST_DMASHDL_ALL_RST, 1 << 5);
        assert_eq!(TX_DMA_EN, 1 << 0);
        assert_eq!(TX_DMA_BUSY, 1 << 1);
        assert_eq!(RX_DMA_EN, 1 << 2);
        assert_eq!(RX_DMA_BUSY, 1 << 3);
    }

    #[test]
    fn fw_ready_register() {
        // Offset + mask are pinned to the upstream-known values; the BAR0
        // remap window is hardware-only and intentionally not encoded here.
        assert_eq!(MT_CONN_ON_MISC, 0x1806_1140);
        assert_eq!(MT_TOP_MISC2_FW_N9_RDY, 0x3);
        // Predicate truth table: both bits required.
        assert!(fw_n9_ready(0x3));
        assert!(fw_n9_ready(0xFFFF_FFFF));
        assert!(!fw_n9_ready(0x0));
        assert!(!fw_n9_ready(0x1));
        assert!(!fw_n9_ready(0x2));
    }

    #[test]
    fn reset_predicate() {
        // Both busy bits set → not complete.
        assert!(!reset_complete(TX_DMA_BUSY | RX_DMA_BUSY));
        // Only TX busy → not complete.
        assert!(!reset_complete(TX_DMA_BUSY));
        // Only RX busy → not complete.
        assert!(!reset_complete(RX_DMA_BUSY));
        // Both clear (DMA EN bits may still be set — only busy matters).
        assert!(reset_complete(TX_DMA_EN | RX_DMA_EN));
        // Both clear, no other bits.
        assert!(reset_complete(0));
        // Enable bits set but busy clear (transitional state — still "complete").
        assert!(reset_complete(TX_DMA_EN | RX_DMA_EN));
    }
}
