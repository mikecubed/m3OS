//! Intel High Definition Audio (HDA) — host-testable pure logic — Phase 80b.
//!
//! The HDA driver process lives in `userspace/drivers/hda/`; the register
//! decode, verb encoding, widget-graph parsing, `SDnFMT`/BDL packing, and
//! interrupt-status decode that need no hardware live here so they are
//! exercised by `cargo xtask check` without QEMU (mirroring how Phase 79 put
//! its `nic_ids`/`r8169` host logic in `kernel-core`).
//!
//! This module is the single source of truth for the HDA register map, verb
//! opcodes, and `GET_PARAMETER` ids; every submodule and the driver crate
//! reference these constants rather than redeclaring them.
//!
//! Offsets/ids are from the Intel HD Audio Specification rev 1.0a and the
//! Linux canonical `include/sound/hda_*.h` / Redox `ihdad` register maps.

#![allow(dead_code)] // submodule consumers + the out-of-tree driver crate use these.

pub mod fmt;
pub mod ids;
pub mod irq;
pub mod realtek;
pub mod regs;
pub mod verb;
pub mod widget;

// ---------------------------------------------------------------------------
// Controller register offsets (BAR0 MMIO), HDA spec §3.3
// ---------------------------------------------------------------------------

/// Global Capabilities (16-bit): OSS[15:12] ISS[11:8] BSS[7:3] NSDO[2:1] 64OK[0].
pub const REG_GCAP: usize = 0x00;
/// Minor / Major version (8-bit each).
pub const REG_VMIN: usize = 0x02;
pub const REG_VMAJ: usize = 0x03;
/// Global Control (32-bit): CRST = bit0, FCNTRL = bit1, UNSOL = bit8.
pub const REG_GCTL: usize = 0x08;
/// Wake Enable (16-bit).
pub const REG_WAKEEN: usize = 0x0C;
/// State Change Status (16-bit): one bit per SDI / codec address.
pub const REG_STATESTS: usize = 0x0E;
/// Global Interrupt Control (32-bit): GIE = bit31, CIE = bit30, per-stream SIE.
pub const REG_INTCTL: usize = 0x20;
/// Global Interrupt Status (32-bit): GIS = bit31, CIS = bit30, per-stream SIS.
pub const REG_INTSTS: usize = 0x24;

/// CORB (Command Output Ring Buffer) registers.
pub const REG_CORBLBASE: usize = 0x40;
pub const REG_CORBUBASE: usize = 0x44;
pub const REG_CORBWP: usize = 0x48; // 16-bit write pointer
pub const REG_CORBRP: usize = 0x4A; // 16-bit read pointer; CORBRPRST = bit15
pub const REG_CORBCTL: usize = 0x4C; // 8-bit: CMEIE = bit0, CORBRUN = bit1
pub const REG_CORBSTS: usize = 0x4D; // 8-bit
pub const REG_CORBSIZE: usize = 0x4E; // 8-bit: SIZE[1:0], SZCAP[7:4]

/// RIRB (Response Input Ring Buffer) registers.
pub const REG_RIRBLBASE: usize = 0x50;
pub const REG_RIRBUBASE: usize = 0x54;
pub const REG_RIRBWP: usize = 0x58; // 16-bit write pointer; RIRBWPRST = bit15
pub const REG_RINTCNT: usize = 0x5A; // 16-bit response-interrupt count
pub const REG_RIRBCTL: usize = 0x5C; // 8-bit: RINTCTL = bit0, RIRBDMAEN = bit1
pub const REG_RIRBSTS: usize = 0x5D; // 8-bit: RINTFL = bit0
pub const REG_RIRBSIZE: usize = 0x5E; // 8-bit

/// Immediate-command interface (single-verb fallback).
pub const REG_ICOI: usize = 0x60; // 32-bit immediate command output
pub const REG_IRII: usize = 0x64; // 32-bit immediate response input
pub const REG_ICS: usize = 0x68; // 16-bit: ICB = bit0, IRV = bit1

/// DMA position buffer base (deferred — `SDnLPIB` polling is used instead).
pub const REG_DPLBASE: usize = 0x70;
pub const REG_DPUBASE: usize = 0x74;

// --- Bit fields -----------------------------------------------------------

pub const GCTL_CRST: u32 = 1 << 0;
pub const CORBRP_RST: u16 = 1 << 15;
pub const CORBCTL_RUN: u8 = 1 << 1;
pub const RIRBWP_RST: u16 = 1 << 15;
pub const RIRBCTL_DMAEN: u8 = 1 << 1;
pub const RIRBCTL_RINTCTL: u8 = 1 << 0;
pub const ICS_ICB: u16 = 1 << 0;
pub const ICS_IRV: u16 = 1 << 1;
pub const INTCTL_GIE: u32 = 1 << 31;
pub const INTCTL_CIE: u32 = 1 << 30;
pub const INTSTS_GIS: u32 = 1 << 31;
/// `CORBSIZE`/`RIRBSIZE` low-2-bits value selecting 256 entries.
pub const RING_SIZE_256: u8 = 0b10;
/// CORB/RIRB entry count for the 256-entry configuration.
pub const RING_ENTRIES_256: usize = 256;

// ---------------------------------------------------------------------------
// Stream descriptor block: base 0x80, stride 0x20 (HDA spec §3.3.35+)
// ---------------------------------------------------------------------------

/// Base offset of the first stream descriptor.
pub const STREAM_DESC_BASE: usize = 0x80;
/// Per-stream descriptor stride.
pub const STREAM_DESC_STRIDE: usize = 0x20;

/// Byte offset of stream descriptor `n`'s register block within BAR0.
pub const fn stream_desc_offset(n: usize) -> usize {
    STREAM_DESC_BASE + n * STREAM_DESC_STRIDE
}

// Per-stream-descriptor register offsets (relative to the block base).
pub const SD_CTL: usize = 0x00; // 24-bit control (accessed as dword): SRST b0, RUN b1, IOCE b2, tag [23:20]
pub const SD_STS: usize = 0x03; // 8-bit status: BCIS b2, FIFOE b3, DESE b4
pub const SD_LPIB: usize = 0x04; // 32-bit link position in buffer
pub const SD_CBL: usize = 0x08; // 32-bit cyclic buffer length
pub const SD_LVI: usize = 0x0C; // 16-bit last valid index
pub const SD_FIFOS: usize = 0x10; // 16-bit FIFO size
pub const SD_FMT: usize = 0x12; // 16-bit format
pub const SD_BDPL: usize = 0x18; // 32-bit BDL pointer low
pub const SD_BDPU: usize = 0x1C; // 32-bit BDL pointer high

pub const SDCTL_SRST: u32 = 1 << 0;
pub const SDCTL_RUN: u32 = 1 << 1;
pub const SDCTL_IOCE: u32 = 1 << 2;
/// 4-bit stream tag occupies bits [23:20] of `SDnCTL`.
pub const SDCTL_STREAM_TAG_SHIFT: u32 = 20;
pub const SDSTS_BCIS: u8 = 1 << 2;
pub const SDSTS_FIFOE: u8 = 1 << 3;
pub const SDSTS_DESE: u8 = 1 << 4;
/// Write-1-to-clear mask for the per-stream status byte.
pub const SDSTS_W1C: u8 = SDSTS_BCIS | SDSTS_FIFOE | SDSTS_DESE;

// ---------------------------------------------------------------------------
// Verb opcodes (HDA spec §7.3) — 12-bit "get/set" form unless noted
// ---------------------------------------------------------------------------

pub const VERB_GET_PARAMETER: u32 = 0xF00;
pub const VERB_GET_CONNECTION_SELECT: u32 = 0xF01;
pub const VERB_GET_CONNECTION_LIST: u32 = 0xF02;
pub const VERB_GET_PIN_SENSE: u32 = 0xF09;
pub const VERB_GET_CONFIG_DEFAULT: u32 = 0xF1C;
pub const VERB_SET_CONNECTION_SELECT: u32 = 0x701;
pub const VERB_SET_POWER_STATE: u32 = 0x705;
pub const VERB_SET_CHANNEL_STREAMID: u32 = 0x706;
pub const VERB_SET_PIN_WIDGET_CONTROL: u32 = 0x707;
pub const VERB_SET_EAPD_BTLENABLE: u32 = 0x70C;
pub const VERB_SET_GPIO_DATA: u32 = 0x715;
pub const VERB_SET_GPIO_MASK: u32 = 0x716;
pub const VERB_SET_GPIO_DIRECTION: u32 = 0x717;
pub const VERB_SET_COEF_INDEX: u32 = 0x500;
pub const VERB_SET_PROC_COEF: u32 = 0x400;
/// `SET_STREAM_FORMAT` is the 4-bit-verb form (verb nibble `0x2`, 16-bit payload).
pub const VERB4_SET_STREAM_FORMAT: u32 = 0x2;
/// `SET_AMP_GAIN_MUTE` is the 4-bit-verb form (verb nibble `0x3`, 16-bit payload).
pub const VERB4_SET_AMP_GAIN_MUTE: u32 = 0x3;

// ---------------------------------------------------------------------------
// GET_PARAMETER parameter ids (HDA spec §7.3.6)
// ---------------------------------------------------------------------------

pub const PARAM_VENDOR_ID: u32 = 0x00;
pub const PARAM_REVISION_ID: u32 = 0x02;
pub const PARAM_SUBORDINATE_NODE_COUNT: u32 = 0x04;
pub const PARAM_FUNCTION_GROUP_TYPE: u32 = 0x05;
pub const PARAM_AUDIO_FG_CAPS: u32 = 0x08;
pub const PARAM_AUDIO_WIDGET_CAPS: u32 = 0x09;
pub const PARAM_SUPPORTED_PCM_RATES: u32 = 0x0A;
pub const PARAM_SUPPORTED_STREAM_FORMATS: u32 = 0x0B;
pub const PARAM_PIN_CAPS: u32 = 0x0C;
pub const PARAM_CONNECTION_LIST_LENGTH: u32 = 0x0E;

/// Function-group type value for an Audio Function Group (in `FUNCTION_GROUP_TYPE`).
pub const FN_GROUP_AUDIO: u8 = 0x01;

/// Power state D0 (fully on) payload for `SET_POWER_STATE`.
pub const POWER_STATE_D0: u32 = 0x00;
