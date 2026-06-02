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
