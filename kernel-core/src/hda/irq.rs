//! HDA interrupt-status decode — Phase 80b (C.3).
//!
//! The `INTSTS` register (32-bit, HDA spec §3.3.14) has three logical fields:
//!
//! ```text
//! Bit 31      → GIS  (Global Interrupt Status) — set if any SIS or CIS is set.
//! Bit 30      → CIS  (Controller Interrupt Status) — unsolicited-response / CORB/RIRB.
//! Bits [29:0] → SIS  (Stream Interrupt Status) — one bit per stream descriptor.
//! ```
//!
//! The `SDSTS` register (8-bit, HDA spec §3.3.38) for each stream descriptor
//! includes:
//! * Bit 2: `BCIS` — Buffer Completion Interrupt Status (write-1-to-clear).
//! * Bit 3: `FIFOE` — FIFO Error (write-1-to-clear).
//! * Bit 4: `DESE` — Descriptor Error (write-1-to-clear).
//!
//! To clear `BCIS` the driver writes the value `SDSTS_BCIS` (bit 2 set) back
//! to the `SDnSTS` register; this is the write-1-to-clear convention.
//!
//! All functions are pure; no hardware I/O, no syscalls.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// INTSTS decode (HDA spec §3.3.14)
// ---------------------------------------------------------------------------

/// Decoded contents of the 32-bit `INTSTS` register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntStatus {
    /// True when bit 31 (`GIS`) is set — at least one stream or the controller
    /// has a pending interrupt.
    pub global: bool,
    /// True when bit 30 (`CIS`) is set — the controller has a pending interrupt
    /// (unsolicited response, CORB/RIRB error, etc.).
    pub controller: bool,
    /// The raw per-stream interrupt bits from bits [29:0] of `INTSTS`.
    /// Bit *n* corresponds to stream descriptor *n*.
    pub stream_bits: u32,
}

/// Decode the `INTSTS` register value (HDA spec §3.3.14).
///
/// ```text
/// Bit  31 → GIS  (global interrupt status)
/// Bit  30 → CIS  (controller interrupt status)
/// [29:0]  → per-stream SIS bits (stream_bits)
/// ```
#[inline]
pub fn decode_intsts(intsts: u32) -> IntStatus {
    IntStatus {
        global: intsts & super::INTSTS_GIS != 0,
        controller: intsts & (1 << 30) != 0,
        stream_bits: intsts & 0x3FFF_FFFF, // bits [29:0]
    }
}

/// True when the interrupt for stream descriptor `stream_index` is set in `INTSTS`.
///
/// `stream_index` must be in the range 0–29 (the 30 SIS bits); bit 30 is CIS
/// and bit 31 is GIS, both handled separately by [`decode_intsts`].
#[inline]
pub fn stream_fired(intsts: u32, stream_index: usize) -> bool {
    debug_assert!(stream_index < 30, "stream_index must be < 30");
    (intsts >> stream_index) & 1 != 0
}

// ---------------------------------------------------------------------------
// SDSTS BCIS clear value (HDA spec §3.3.38)
// ---------------------------------------------------------------------------

/// The value to write back to `SDnSTS` to acknowledge (clear) a Buffer
/// Completion Interrupt (`BCIS`, bit 2).
///
/// The HDA spec uses a write-1-to-clear (W1C) convention for the error and
/// interrupt bits in `SDSTS`; writing this value clears only `BCIS`.
#[inline]
pub fn bcis_clear_value() -> u8 {
    super::SDSTS_BCIS
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode an INTSTS value with GIS set and stream 0 firing.
    #[test]
    fn intsts_decode() {
        // GIS (bit31) + stream 0 (bit0).
        let intsts: u32 = super::super::INTSTS_GIS | (1 << 0);
        let s = decode_intsts(intsts);

        assert!(s.global, "GIS must be set");
        assert!(!s.controller, "CIS must not be set");
        assert_eq!(
            s.stream_bits & 1,
            1,
            "stream 0 bit must be set in stream_bits"
        );

        assert!(stream_fired(intsts, 0), "stream 0 must have fired");
        assert!(!stream_fired(intsts, 1), "stream 1 must not have fired");

        // GIS + CIS + stream 5.
        let intsts2: u32 = super::super::INTSTS_GIS | (1 << 30) | (1 << 5);
        let s2 = decode_intsts(intsts2);
        assert!(s2.global, "GIS");
        assert!(s2.controller, "CIS");
        assert!(stream_fired(intsts2, 5), "stream 5");
        assert!(!stream_fired(intsts2, 0), "stream 0 not fired");
    }

    /// `bcis_clear_value()` must equal bit 2 (0b100 = 4).
    #[test]
    fn bcis_clear_value_test() {
        let v = bcis_clear_value();
        assert_eq!(v, 0b0000_0100, "BCIS is bit 2 → 0b100");
        assert_eq!(v, super::super::SDSTS_BCIS);
    }
}
