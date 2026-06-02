//! MediaTek mt792x Wi-Fi — host-testable pure logic — Phase 81 Track KC.
//!
//! This module is `no_std` + host-testable. It captures every piece of the
//! mt792x bring-up that does not touch hardware directly:
//!
//! * WFDMA register offsets and reset/GLO_CFG predicates (`regs`),
//! * connac2 ROM-patch and RAM-code firmware parsers (`firmware`),
//! * MCU command-frame encoding, TLV framing, and STA_REC_KEY encoder (`mcu`),
//! * WFDMA descriptor layout, token pool, and DMA helpers (`dma`).
//!
//! The hardware-touching bring-up (BAR map, MSI-X, register writes, IRQ loop)
//! lives in the `mt792x` driver binary and consumes the constants and builders
//! defined here so the bit-level logic stays host-tested.
//!
//! Mirroring the `hda/` layout used for Phase 80.

#![allow(dead_code)]

pub mod dma;
pub mod firmware;
pub mod mcu;
pub mod regs;
