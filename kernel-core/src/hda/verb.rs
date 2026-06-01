//! HDA verb encoding and CORB ring-pointer arithmetic — Phase 80b (B.3).
//!
//! The HDA command-verb format uses two encodings depending on the verb size
//! (HDA spec §7.3):
//!
//! **12-bit verb** (the common "GET" and "SET" form):
//! ```text
//! Bits [31:28] → Codec address (CAd, 4 bits)
//! Bits [27:20] → Node ID (NID, 8 bits)
//! Bits [19:8]  → Verb (12 bits)
//! Bits  [7:0]  → Payload (8 bits)
//! ```
//!
//! **4-bit verb** (`SET_STREAM_FORMAT`, `SET_AMP_GAIN_MUTE`):
//! ```text
//! Bits [31:28] → Codec address (CAd, 4 bits)
//! Bits [27:20] → Node ID (NID, 8 bits)
//! Bits [19:16] → Verb nibble (4 bits)
//! Bits [15:0]  → Payload (16 bits)
//! ```
//!
//! The CORB (Command Output Ring Buffer) is a circular buffer of up to 256
//! 32-bit verb entries. The hardware's read pointer (`CORBRP`) and the
//! driver's write pointer (`CORBWP`) both wrap modulo `RING_ENTRIES_256`.
//! The CORBRP reset sequence (HDA spec §3.3.21): write `CORBRP_RST` (bit15)
//! high → read back 1 (asserted) → write 0 → read back 0 (cleared).

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// 12-bit verb encoding (HDA spec §7.3, general GET/SET form)
// ---------------------------------------------------------------------------

/// Encode a 12-bit-verb command word.
///
/// Layout:
/// ```text
/// [31:28] CAd  = codec (4 bits)
/// [27:20] NID  = nid   (8 bits)
/// [19:8]  verb = verb & 0xFFF (12 bits)
/// [7:0]   payload              (8 bits)
/// ```
///
/// # Example
/// ```
/// # use kernel_core::hda::verb::encode_verb12;
/// # use kernel_core::hda::VERB_GET_PARAMETER;
/// // GET_PARAMETER(PARAM_AUDIO_WIDGET_CAPS=0x09) on codec 1, node 0x02
/// assert_eq!(encode_verb12(1, 0x02, VERB_GET_PARAMETER, 0x09), 0x102F0009);
/// ```
#[inline]
pub fn encode_verb12(codec: u8, nid: u8, verb: u32, payload: u8) -> u32 {
    ((codec as u32) << 28) | ((nid as u32) << 20) | ((verb & 0xFFF) << 8) | (payload as u32)
}

// ---------------------------------------------------------------------------
// 4-bit verb encoding (SET_STREAM_FORMAT, SET_AMP_GAIN_MUTE)
// ---------------------------------------------------------------------------

/// Encode a 4-bit-verb command word.
///
/// Layout:
/// ```text
/// [31:28] CAd    = codec       (4 bits)
/// [27:20] NID    = nid         (8 bits)
/// [19:16] verb4  = verb4 & 0xF (4 bits)
/// [15:0]  payload              (16 bits)
/// ```
///
/// Used for [`super::VERB4_SET_STREAM_FORMAT`] (`0x2`) and
/// [`super::VERB4_SET_AMP_GAIN_MUTE`] (`0x3`).
///
/// # Example
/// ```
/// # use kernel_core::hda::verb::encode_verb4;
/// # use kernel_core::hda::VERB4_SET_STREAM_FORMAT;
/// let v = encode_verb4(0, 0x02, VERB4_SET_STREAM_FORMAT, 0x0011);
/// // Verb nibble lands in bits [19:16]
/// assert_eq!((v >> 16) & 0xF, VERB4_SET_STREAM_FORMAT);
/// // Payload lands in bits [15:0]
/// assert_eq!(v & 0xFFFF, 0x0011);
/// ```
#[inline]
pub fn encode_verb4(codec: u8, nid: u8, verb4: u32, payload: u16) -> u32 {
    ((codec as u32) << 28) | ((nid as u32) << 20) | ((verb4 & 0xF) << 16) | (payload as u32)
}

// ---------------------------------------------------------------------------
// CORB ring-pointer arithmetic (HDA spec §3.3.18–§3.3.21)
// ---------------------------------------------------------------------------

/// Advance the CORB write pointer by one entry, wrapping at 256.
///
/// The driver writes the next verb into `corb[corb_next_wp(wp)]` and then
/// writes the returned value to the `CORBWP` MMIO register to notify the
/// controller.
#[inline]
pub fn corb_next_wp(wp: u16) -> u16 {
    (wp.wrapping_add(1)) % super::RING_ENTRIES_256 as u16
}

// ---------------------------------------------------------------------------
// CORBRP reset-handshake helpers (HDA spec §3.3.21)
// ---------------------------------------------------------------------------
//
// The documented CORBRP reset sequence:
//   1. Write CORBRP with bit 15 (`CORBRP_RST`) set.
//   2. Poll CORBRP until bit 15 reads back 1 → asserted.
//   3. Write CORBRP with bit 15 clear (write 0 to bit 15).
//   4. Poll CORBRP until bit 15 reads back 0 → cleared.
// After this, the hardware read pointer is reliably at entry 0.

/// Produce the value to write to `CORBRP` in step 1: set the reset bit.
///
/// Preserves no other bits (the spec requires writing only the RST bit during
/// the reset sequence).
#[inline]
pub fn corbrp_reset_step1(_rp: u16) -> u16 {
    super::CORBRP_RST
}

/// True when the `CORBRP` register reads back with bit 15 set — the reset has
/// been acknowledged by the controller (step 2 of the handshake).
#[inline]
pub fn corbrp_reset_asserted(rp: u16) -> bool {
    rp & super::CORBRP_RST != 0
}

/// True when the `CORBRP` register reads back with bit 15 clear — the reset
/// has completed and the read pointer is at 0 (step 4 of the handshake).
#[inline]
pub fn corbrp_reset_cleared(rp: u16) -> bool {
    rp & super::CORBRP_RST == 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical GET_PARAMETER example from the HDA spec.
    ///
    /// codec=1, nid=0x02, verb=0xF00 (GET_PARAMETER), payload=0x09
    /// Expected: 0x102F0009
    ///   [31:28] = 0x1   → 0x10000000
    ///   [27:20] = 0x02  → 0x00200000
    ///   [19:8]  = 0xF00 → 0x000F0000 … wait: 0xF00 << 8 = 0x000F0000? No:
    ///     0xF00 << 8 = 0x0F0000? Let's check: 0xF00 = 3840; 3840*256=983040=0x0F0000
    ///   Hmm: expected 0x102F0009.
    ///   Actually: [27:20]=0x02 → 0x00200000; [19:8]=0xF00 → (0xF00<<8) = 0x000F_0000
    ///   Sum: 0x1000_0000 + 0x0020_0000 + 0x000F_0000 + 0x09
    ///      = 0x102F_0009 ✓
    #[test]
    fn encode_verb12_get_param() {
        assert_eq!(
            encode_verb12(1, 0x02, super::super::VERB_GET_PARAMETER, 0x09),
            0x102F_0009,
            "encode_verb12 GET_PARAMETER canonical value"
        );
    }

    /// Verify the 4-bit-verb layout: verb nibble in [19:16], payload in [15:0].
    #[test]
    fn encode_verb4_form() {
        let v = encode_verb4(0, 0x02, super::super::VERB4_SET_STREAM_FORMAT, 0x0011);
        let verb_nibble = (v >> 16) & 0xF;
        let payload_bits = v & 0xFFFF;
        assert_eq!(
            verb_nibble,
            super::super::VERB4_SET_STREAM_FORMAT,
            "verb nibble must land in bits [19:16]"
        );
        assert_eq!(payload_bits, 0x0011, "payload must land in bits [15:0]");

        // Codec and NID fields must be zero for codec=0, nid=0x02.
        assert_eq!((v >> 28) & 0xF, 0, "codec field");
        assert_eq!((v >> 20) & 0xFF, 0x02, "nid field");
    }

    /// Verify the full CORBRP reset handshake predicate sequence.
    #[test]
    fn corbrp_handshake() {
        // Step 1: write CORBRP_RST.
        let written = corbrp_reset_step1(0x0000);
        assert_eq!(written, super::super::CORBRP_RST);

        // Step 2: hw echoes back with bit 15 set.
        let readback_asserted = super::super::CORBRP_RST; // hw mirrors the bit
        assert!(
            corbrp_reset_asserted(readback_asserted),
            "must be asserted after step 1"
        );
        assert!(!corbrp_reset_cleared(readback_asserted));

        // Step 3+4: clear bit 15, hw clears it back.
        let readback_cleared: u16 = 0x0000;
        assert!(
            corbrp_reset_cleared(readback_cleared),
            "must be cleared after step 3"
        );
        assert!(!corbrp_reset_asserted(readback_cleared));
    }

    /// The write pointer must wrap from 255 back to 0 (mod 256).
    #[test]
    fn corb_wp_wraps() {
        assert_eq!(corb_next_wp(255), 0, "wp 255 → 0");
        assert_eq!(corb_next_wp(0), 1, "wp 0 → 1");
        assert_eq!(corb_next_wp(127), 128, "wp 127 → 128");
    }
}
