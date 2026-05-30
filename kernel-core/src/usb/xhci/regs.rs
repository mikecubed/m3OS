//! xHCI Host Controller **Capability** register decoders (xHCI 1.2b §5.3).
//!
//! The Capability registers sit at the start of the controller's MMIO BAR and
//! are read-only. They tell the driver the geometry of everything else: where
//! the Operational registers begin (`CAPLENGTH`), how many device slots /
//! interrupters / root-hub ports exist (`HCSPARAMS1`), the scratchpad-buffer
//! requirement (`HCSPARAMS2`), the 64-bit/context-size/extended-capability
//! layout (`HCCPARAMS1`), and the offsets of the Doorbell array (`DBOFF`) and
//! Runtime register block (`RTSOFF`).
//!
//! This module performs **no MMIO**: every function takes the already-read raw
//! `u32` dword and decodes its bit-fields. The driver is responsible for the
//! actual volatile reads.

// ---------------------------------------------------------------------------
// Capability register offsets (relative to the controller's MMIO BAR base)
// ---------------------------------------------------------------------------

/// Byte offset of the dword holding `CAPLENGTH` (7:0) and `HCIVERSION`
/// (31:16). xHCI §5.3.1 / §5.3.2.
pub const CAP_CAPLENGTH_HCIVERSION: usize = 0x00;
/// Byte offset of `HCSPARAMS1` — Structural Parameters 1. xHCI §5.3.3.
pub const CAP_HCSPARAMS1: usize = 0x04;
/// Byte offset of `HCSPARAMS2` — Structural Parameters 2. xHCI §5.3.4.
pub const CAP_HCSPARAMS2: usize = 0x08;
/// Byte offset of `HCSPARAMS3` — Structural Parameters 3. xHCI §5.3.5.
pub const CAP_HCSPARAMS3: usize = 0x0C;
/// Byte offset of `HCCPARAMS1` — Capability Parameters 1. xHCI §5.3.6.
pub const CAP_HCCPARAMS1: usize = 0x10;
/// Byte offset of `DBOFF` — Doorbell array offset. xHCI §5.3.7.
pub const CAP_DBOFF: usize = 0x14;
/// Byte offset of `RTSOFF` — Runtime register space offset. xHCI §5.3.8.
pub const CAP_RTSOFF: usize = 0x18;

// ---------------------------------------------------------------------------
// CAPLENGTH / HCIVERSION (dword at offset 0x00)
// ---------------------------------------------------------------------------

/// `CAPLENGTH` (xHCI §5.3.1) — length in bytes of the Capability register
/// space, i.e. the BAR-relative byte offset at which the Operational registers
/// begin. Lives in bits 7:0 of the dword at offset 0x00.
pub const fn caplength(dword0: u32) -> u8 {
    (dword0 & 0xFF) as u8
}

/// `HCIVERSION` (xHCI §5.3.2) — BCD interface version number (e.g. `0x0110`
/// for 1.1, `0x0120` for 1.2). Lives in bits 31:16 of the dword at offset 0x00.
pub const fn hciversion(dword0: u32) -> u16 {
    (dword0 >> 16) as u16
}

// ---------------------------------------------------------------------------
// HCSPARAMS1 — Structural Parameters 1 (xHCI §5.3.3)
// ---------------------------------------------------------------------------

/// Decoder for the `HCSPARAMS1` register (offset 0x04).
///
/// * `MaxSlots`  (7:0)   — number of Device Slot contexts the controller
///   supports (sizes the Device Context Base Address Array).
/// * `MaxIntrs`  (18:8)  — number of Interrupter register sets.
/// * `MaxPorts`  (31:24) — number of root-hub ports (sizes the PORTSC array).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hcsparams1(pub u32);

impl Hcsparams1 {
    /// `MaxSlots` — maximum number of device slots (bits 7:0).
    pub const fn max_slots(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    /// `MaxIntrs` — number of interrupter register sets (bits 18:8).
    pub const fn max_interrupters(self) -> u16 {
        ((self.0 >> 8) & 0x7FF) as u16
    }

    /// `MaxPorts` — number of root-hub ports (bits 31:24).
    pub const fn max_ports(self) -> u8 {
        ((self.0 >> 24) & 0xFF) as u8
    }
}

// ---------------------------------------------------------------------------
// HCSPARAMS2 — Structural Parameters 2 (xHCI §5.3.4)
// ---------------------------------------------------------------------------

/// Decoder for the `HCSPARAMS2` register (offset 0x08).
///
/// * `IST`                 (3:0)   — Isochronous Scheduling Threshold.
/// * `ERSTMax`             (7:4)   — log2 of max Event Ring Segment Table size.
/// * `MaxScratchpadBufsLo` (25:21) — low 5 bits of the scratchpad-buffer count.
/// * `SPR`                 (26)    — Scratchpad Restore.
/// * `MaxScratchpadBufsHi` (31:27) — high 5 bits of the scratchpad-buffer count.
///
/// The total scratchpad-buffer requirement is split across two fields:
/// `(hi << 5) | lo`. See [`Hcsparams2::max_scratchpad_buffers`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hcsparams2(pub u32);

impl Hcsparams2 {
    /// `IST` — Isochronous Scheduling Threshold (bits 3:0).
    pub const fn ist(self) -> u8 {
        (self.0 & 0xF) as u8
    }

    /// `ERSTMax` — Event Ring Segment Table maximum size, log2 (bits 7:4).
    /// The maximum number of ERST entries the controller supports is
    /// `2^erst_max`.
    pub const fn erst_max(self) -> u8 {
        ((self.0 >> 4) & 0xF) as u8
    }

    /// `SPR` — Scratchpad Restore (bit 26).
    pub const fn scratchpad_restore(self) -> bool {
        (self.0 >> 26) & 1 != 0
    }

    /// Low 5 bits of the Max Scratchpad Buffers field (bits 25:21).
    pub const fn max_scratchpad_buffers_lo(self) -> u32 {
        (self.0 >> 21) & 0x1F
    }

    /// High 5 bits of the Max Scratchpad Buffers field (bits 31:27).
    pub const fn max_scratchpad_buffers_hi(self) -> u32 {
        (self.0 >> 27) & 0x1F
    }

    /// Total number of scratchpad buffers the controller requires the driver to
    /// allocate, reconstructed from the split hi/lo fields:
    /// `(hi << 5) | lo`. A value of zero means no scratchpad buffers are
    /// needed.
    pub const fn max_scratchpad_buffers(self) -> u32 {
        (self.max_scratchpad_buffers_hi() << 5) | self.max_scratchpad_buffers_lo()
    }
}

// ---------------------------------------------------------------------------
// HCCPARAMS1 — Capability Parameters 1 (xHCI §5.3.6)
// ---------------------------------------------------------------------------

/// Number of bytes occupied by a 32-byte context entry (CSZ=0).
pub const CONTEXT_SIZE_32: usize = 32;
/// Number of bytes occupied by a 64-byte context entry (CSZ=1).
pub const CONTEXT_SIZE_64: usize = 64;

/// Decoder for the `HCCPARAMS1` register (offset 0x10).
///
/// * `AC64` (0)     — 64-bit Addressing Capability.
/// * `CSZ`  (2)     — Context Size. 0 → 32-byte contexts, 1 → 64-byte contexts.
/// * `xECP` (31:16) — xHCI Extended Capabilities Pointer, expressed as an
///   offset **in 32-bit dwords** from the BAR base (multiply by 4 for a byte
///   offset). Zero means there are no extended capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hccparams1(pub u32);

impl Hccparams1 {
    /// `AC64` — true if the controller supports 64-bit addressing (bit 0).
    pub const fn ac64(self) -> bool {
        self.0 & 1 != 0
    }

    /// `CSZ` — Context Size. `false` → 32-byte contexts, `true` → 64-byte
    /// contexts (bit 2).
    pub const fn csz_64(self) -> bool {
        (self.0 >> 2) & 1 != 0
    }

    /// `xECP` — extended-capabilities pointer in **dword** units from the BAR
    /// base (bits 31:16). Zero indicates no extended capabilities.
    pub const fn xecp_dwords(self) -> u16 {
        (self.0 >> 16) as u16
    }

    /// Byte offset of the first extended-capability structure from the BAR
    /// base, or `None` when `xECP` is zero (no extended capabilities).
    pub const fn xecp_byte_offset(self) -> Option<usize> {
        let dwords = self.xecp_dwords();
        if dwords == 0 {
            None
        } else {
            Some((dwords as usize) * 4)
        }
    }

    /// Size in bytes of a single context structure, decoded from `CSZ`.
    pub const fn context_size_bytes(self) -> usize {
        if self.csz_64() {
            CONTEXT_SIZE_64
        } else {
            CONTEXT_SIZE_32
        }
    }
}

/// Free-function form of [`Hccparams1::context_size_bytes`]: decode the context
/// entry size in bytes directly from a raw `HCCPARAMS1` dword.
pub const fn context_size_bytes(hccparams1: u32) -> usize {
    Hccparams1(hccparams1).context_size_bytes()
}

// ---------------------------------------------------------------------------
// DBOFF / RTSOFF (xHCI §5.3.7 / §5.3.8)
// ---------------------------------------------------------------------------

/// `DBOFF` reserved-bit mask: bits 1:0 are reserved and read as zero, so the
/// doorbell array offset is the raw value with those bits masked off.
pub const DBOFF_OFFSET_MASK: u32 = !0x3;
/// `RTSOFF` reserved-bit mask: bits 4:0 are reserved, so the runtime register
/// space is 32-byte aligned.
pub const RTSOFF_OFFSET_MASK: u32 = !0x1F;

/// Decode `DBOFF` (xHCI §5.3.7) — BAR-relative byte offset of the Doorbell
/// array. Bits 1:0 are reserved (masked off): the array is dword-aligned.
pub const fn dboff(raw: u32) -> u32 {
    raw & DBOFF_OFFSET_MASK
}

/// Decode `RTSOFF` (xHCI §5.3.8) — BAR-relative byte offset of the Runtime
/// register block. Bits 4:0 are reserved (masked off): the block is 32-byte
/// aligned.
pub const fn rtsoff(raw: u32) -> u32 {
    raw & RTSOFF_OFFSET_MASK
}

// ---------------------------------------------------------------------------
// Base-offset helpers
// ---------------------------------------------------------------------------

/// BAR-relative byte offset at which the Operational register block begins.
/// This is exactly `CAPLENGTH` (xHCI §5.3.1).
pub const fn operational_offset(caplength: u8) -> usize {
    caplength as usize
}

/// BAR-relative byte offset of the Runtime register block, from an already
/// reserved-bit-masked `RTSOFF` value (see [`rtsoff`]).
pub const fn runtime_offset(rtsoff: u32) -> usize {
    rtsoff as usize
}

/// BAR-relative byte offset of the Doorbell array, from an already
/// reserved-bit-masked `DBOFF` value (see [`dboff`]).
pub const fn doorbell_offset(dboff: u32) -> usize {
    dboff as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caplength_and_hciversion() {
        // CAPLENGTH = 0x20, HCIVERSION = 0x0120 (xHCI 1.2).
        let dword0: u32 = (0x0120 << 16) | 0x20;
        assert_eq!(caplength(dword0), 0x20);
        assert_eq!(hciversion(dword0), 0x0120);
        assert_eq!(operational_offset(caplength(dword0)), 0x20);
    }

    #[test]
    fn hcsparams1_fields() {
        // MaxSlots=64 (0x40), MaxIntrs=8 (in 18:8), MaxPorts=10 (0x0A in 31:24).
        let raw: u32 = (0x0A << 24) | (8 << 8) | 0x40;
        let p = Hcsparams1(raw);
        assert_eq!(p.max_slots(), 64);
        assert_eq!(p.max_interrupters(), 8);
        assert_eq!(p.max_ports(), 10);
    }

    #[test]
    fn hcsparams1_field_widths() {
        // MaxIntrs is an 11-bit field; ensure a value using the full width
        // round-trips and does not bleed into MaxPorts.
        let raw: u32 = (0xFF << 24) | (0x7FF << 8) | 0xFF;
        let p = Hcsparams1(raw);
        assert_eq!(p.max_slots(), 0xFF);
        assert_eq!(p.max_interrupters(), 0x7FF);
        assert_eq!(p.max_ports(), 0xFF);
    }

    #[test]
    fn hcsparams2_split_scratchpad_encoding() {
        // hi = 2 (bits 31:27), lo = 3 (bits 25:21) => (2 << 5) | 3 = 67.
        let raw: u32 = (2u32 << 27) | (3u32 << 21);
        let p = Hcsparams2(raw);
        assert_eq!(p.max_scratchpad_buffers_hi(), 2);
        assert_eq!(p.max_scratchpad_buffers_lo(), 3);
        assert_eq!(p.max_scratchpad_buffers(), 67);
    }

    #[test]
    fn hcsparams2_other_fields() {
        // IST=5, ERSTMax=4, SPR=1.
        let raw: u32 = (1u32 << 26) | (4u32 << 4) | 5;
        let p = Hcsparams2(raw);
        assert_eq!(p.ist(), 5);
        assert_eq!(p.erst_max(), 4);
        assert!(p.scratchpad_restore());
    }

    #[test]
    fn hcsparams2_zero_scratchpad() {
        let p = Hcsparams2(0);
        assert_eq!(p.max_scratchpad_buffers(), 0);
        assert!(!p.scratchpad_restore());
    }

    #[test]
    fn hccparams1_csz_to_context_size() {
        // CSZ=0 -> 32-byte contexts.
        let csz0 = Hccparams1(0);
        assert!(!csz0.csz_64());
        assert_eq!(csz0.context_size_bytes(), 32);
        assert_eq!(context_size_bytes(0), 32);

        // CSZ=1 (bit 2) -> 64-byte contexts.
        let csz1 = Hccparams1(1 << 2);
        assert!(csz1.csz_64());
        assert_eq!(csz1.context_size_bytes(), 64);
        assert_eq!(context_size_bytes(1 << 2), 64);
    }

    #[test]
    fn hccparams1_ac64_and_xecp() {
        // AC64=1 (bit 0), xECP = 0x10 dwords (bits 31:16).
        let raw: u32 = (0x10 << 16) | 1;
        let p = Hccparams1(raw);
        assert!(p.ac64());
        assert_eq!(p.xecp_dwords(), 0x10);
        assert_eq!(p.xecp_byte_offset(), Some(0x40));

        // xECP == 0 -> no extended capabilities.
        assert_eq!(Hccparams1(0).xecp_byte_offset(), None);
        assert!(!Hccparams1(0).ac64());
    }

    #[test]
    fn dboff_masks_low_bits() {
        // Raw value with stray low bits set; bits 1:0 must be cleared.
        assert_eq!(dboff(0x3003), 0x3000);
        assert_eq!(doorbell_offset(dboff(0x3003)), 0x3000);
    }

    #[test]
    fn rtsoff_masks_low_bits() {
        // Bits 4:0 reserved; 0x201F -> 0x2000.
        assert_eq!(rtsoff(0x201F), 0x2000);
        assert_eq!(runtime_offset(rtsoff(0x201F)), 0x2000);
    }
}
