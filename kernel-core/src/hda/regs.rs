//! HDA controller register-level pure logic — Phase 80b (B.1, B.2).
//!
//! Covers:
//! * **GCAP decode** — unpacking OSS/ISS/BSS/NSDO/64OK from the 16-bit GCAP
//!   register (HDA spec §3.3.2).
//! * **STATESTS decode** — converting the 16-bit codec-present bitfield into an
//!   iterator of codec addresses (HDA spec §3.3.7).
//! * **Controller reset predicates** — modelling the GCTL.CRST handshake
//!   (HDA spec §3.3.8): clear CRST → poll until 0 (controller entered reset) →
//!   set CRST → poll until 1 (controller left reset and is ready).
//! * **Output-stream descriptor index** — HDA stream descriptors are laid out
//!   in BAR0 as: [ISS input descriptors][OSS output descriptors][BSS bidirectional
//!   descriptors].  The first output-stream descriptor therefore sits at absolute
//!   index `ISS`.
//!
//! All functions are pure; no hardware I/O, no syscalls.

#![allow(dead_code)]

extern crate alloc;

// ---------------------------------------------------------------------------
// GCAP register (HDA spec §3.3.2)
// ---------------------------------------------------------------------------

/// Decoded contents of the 16-bit `GCAP` register.
///
/// | Field   | Bits    | Description                                   |
/// |---------|---------|-----------------------------------------------|
/// | `oss`   | [15:12] | Number of Output Stream Descriptors (0–15)    |
/// | `iss`   | [11:8]  | Number of Input Stream Descriptors (0–15)     |
/// | `bss`   | [7:3]   | Number of Bidirectional Stream Descriptors    |
/// | `nsdo`  | [2:1]   | Number of Serial Data Out signals (0–3)       |
/// | `addr64`| [0]     | 64-bit address support                        |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcapInfo {
    /// Number of Output Stream Descriptors.
    pub oss: u8,
    /// Number of Input Stream Descriptors.
    pub iss: u8,
    /// Number of Bidirectional Stream Descriptors.
    pub bss: u8,
    /// Number of Serial Data Out signals (`NSDO` field, 2 bits → value 0–3).
    pub nsdo: u8,
    /// True if the controller supports 64-bit address pointers (`64OK` bit 0).
    pub addr64: bool,
}

/// Decode the `GCAP` register value (HDA spec §3.3.2).
///
/// ```text
/// Bits [15:12] → OSS   (number of output stream descriptors)
/// Bits [11:8]  → ISS   (number of input stream descriptors)
/// Bits  [7:3]  → BSS   (number of bidirectional stream descriptors)
/// Bits  [2:1]  → NSDO  (number of serial data out signals)
/// Bit     [0]  → 64OK  (64-bit address support)
/// ```
#[inline]
pub fn decode_gcap(gcap: u16) -> GcapInfo {
    GcapInfo {
        oss: ((gcap >> 12) & 0x0F) as u8,
        iss: ((gcap >> 8) & 0x0F) as u8,
        bss: ((gcap >> 3) & 0x1F) as u8,
        nsdo: ((gcap >> 1) & 0x03) as u8,
        addr64: (gcap & 0x01) != 0,
    }
}

// ---------------------------------------------------------------------------
// STATESTS register (HDA spec §3.3.7)
// ---------------------------------------------------------------------------

/// Return the codec addresses (0–14) whose corresponding bit in `STATESTS` is set.
///
/// Bit *n* of the 15-bit `STATESTS` register is set when codec address *n* has
/// reported a state-change event (typically "codec present" during enumeration).
///
/// # Example
/// ```
/// # use kernel_core::hda::regs::codecs_from_statests;
/// let addrs: Vec<u8> = codecs_from_statests(0b0000_0101).collect();
/// assert_eq!(addrs, vec![0, 2]);
/// ```
pub fn codecs_from_statests(statests: u16) -> impl Iterator<Item = u8> {
    // Only the lower 15 bits are defined; bit 15 is reserved.
    let bits = statests & 0x7FFF;
    (0u8..15).filter(move |i| (bits >> i) & 1 != 0)
}

// ---------------------------------------------------------------------------
// Controller reset predicates (GCTL.CRST, HDA spec §3.3.8)
// ---------------------------------------------------------------------------
//
// The documented reset sequence for `GCTL` (32-bit):
//   1. Clear CRST (bit 0) — write 0 to bit 0.
//   2. Poll `GCTL` until CRST reads 0 → controller has entered reset.
//   3. Set CRST — write 1 to bit 0.
//   4. Poll `GCTL` until CRST reads 1 → controller has left reset and is ready.

/// True when `GCTL` reads back with bit 0 (`CRST`) clear — controller is in reset.
///
/// Use this predicate after *clearing* CRST to wait for the controller to
/// acknowledge entry into reset.
#[inline]
pub fn crst_deasserted(gctl: u32) -> bool {
    gctl & super::GCTL_CRST == 0
}

/// True when `GCTL` reads back with bit 0 (`CRST`) set — controller is out of
/// reset and ready to accept commands.
///
/// Use this predicate after *setting* CRST to wait for the controller to
/// acknowledge that it has left reset.
#[inline]
pub fn crst_asserted(gctl: u32) -> bool {
    gctl & super::GCTL_CRST != 0
}

/// Convenience alias: returns `true` when the controller-ready handshake is
/// complete, i.e. `GCTL` reads back with CRST == 1 after we wrote 1 to it.
#[inline]
pub fn reset_ready(gctl_after_set: u32) -> bool {
    crst_asserted(gctl_after_set)
}

// ---------------------------------------------------------------------------
// Output-stream descriptor index helpers (HDA spec §3.3.35+)
// ---------------------------------------------------------------------------
//
// Stream descriptor blocks are laid out in BAR0 as follows:
//   [0 .. ISS-1]        → Input Stream Descriptors
//   [ISS .. ISS+OSS-1]  → Output Stream Descriptors
//   [ISS+OSS .. total-1]→ Bidirectional Stream Descriptors
//
// For a single-output driver the natural choice is the *first* output stream,
// which lives at absolute descriptor index `ISS`.

/// Return the absolute descriptor index for the first output stream, or `None`
/// if the controller has no output streams (`OSS == 0`).
///
/// The returned index is the `n` passed to [`super::stream_desc_offset(n)`].
#[inline]
pub fn first_output_stream_index(gcap: &GcapInfo) -> Option<usize> {
    if gcap.oss >= 1 {
        Some(output_stream_descriptor_index(gcap))
    } else {
        None
    }
}

/// The absolute descriptor index for the first output stream (`ISS`).
///
/// Input streams occupy indices `[0, ISS)`.  The first output stream immediately
/// follows at index `ISS`.
#[inline]
pub fn output_stream_descriptor_index(gcap: &GcapInfo) -> usize {
    gcap.iss as usize
}

/// True when `idx` is a valid output-stream descriptor index for this controller.
///
/// Valid output indices are in `[ISS, ISS + OSS)`.
#[inline]
pub fn output_index_valid(gcap: &GcapInfo, idx: usize) -> bool {
    let iss = gcap.iss as usize;
    let oss = gcap.oss as usize;
    idx >= iss && idx < iss + oss
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify GCAP decode against a synthetic register value.
    ///
    /// Constructed value: OSS=2, ISS=1, BSS=4, NSDO=1, 64OK=1
    ///   bits[15:12] = 0b0010 (OSS=2)  → 0x2000
    ///   bits[11:8]  = 0b0001 (ISS=1)  → 0x0100
    ///   bits[7:3]   = 0b00100 (BSS=4) → 0x0020
    ///   bits[2:1]   = 0b01 (NSDO=1)   → 0x0002
    ///   bit[0]      = 1 (64OK)        → 0x0001
    #[test]
    fn gcap_decode() {
        let gcap: u16 = 0x2000 | 0x0100 | 0x0020 | 0x0002 | 0x0001;
        let info = decode_gcap(gcap);
        assert_eq!(info.oss, 2, "oss");
        assert_eq!(info.iss, 1, "iss");
        assert_eq!(info.bss, 4, "bss");
        assert_eq!(info.nsdo, 1, "nsdo");
        assert!(info.addr64, "addr64");

        // The chosen output-stream index must lie within [ISS, ISS+OSS).
        let out_idx = output_stream_descriptor_index(&info);
        assert_eq!(out_idx, 1, "first output index == ISS");
        assert!(
            output_index_valid(&info, out_idx),
            "output index {out_idx} must be valid for iss={} oss={}",
            info.iss,
            info.oss
        );
        // Confirm it is strictly < ISS + OSS
        assert!(out_idx < info.iss as usize + info.oss as usize);
    }

    /// Verify STATESTS decodes the correct set of bit indices.
    #[test]
    fn statests_decode() {
        let addrs: Vec<u8> = codecs_from_statests(0b0000_0101).collect();
        assert_eq!(addrs, vec![0, 2]);

        // Single codec at address 3.
        let addrs2: Vec<u8> = codecs_from_statests(0b0000_1000).collect();
        assert_eq!(addrs2, vec![3]);

        // No codecs present.
        let none: Vec<u8> = codecs_from_statests(0).collect();
        assert!(none.is_empty());
    }

    /// Verify the GCTL.CRST reset predicates.
    #[test]
    fn reset_predicate() {
        // CRST clear → controller in reset.
        assert!(crst_deasserted(0x0000_0000));
        assert!(!crst_asserted(0x0000_0000));

        // CRST set → controller out of reset / ready.
        assert!(crst_asserted(super::super::GCTL_CRST));
        assert!(!crst_deasserted(super::super::GCTL_CRST));

        // reset_ready mirrors crst_asserted.
        assert!(reset_ready(super::super::GCTL_CRST));
        assert!(!reset_ready(0));

        // Other bits do not affect the predicate.
        let gctl_with_extras = super::super::GCTL_CRST | (1 << 8); // UNSOL set too
        assert!(reset_ready(gctl_with_extras));
    }
}
