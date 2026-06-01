//! HDA stream-format (`SDnFMT`) encoding and BDL packing — host-testable
//! pure logic (Phase 80b, Track C.2).
//!
//! ## `SDnFMT` — HDA spec §3.3.41
//!
//! The 16-bit `SD_FMT` register (also used as the SET_STREAM_FORMAT verb
//! payload) encodes:
//!
//! ```text
//! [15]     BASE:  0 = 48 kHz family, 1 = 44.1 kHz family
//! [14:11]  MULT:  sample-rate multiplier minus 1 (0 → ×1, 1 → ×2, …)
//!  Wait — HDA spec actually uses [13:11] for MULT and [10:8] for DIV:
//! [14]     reserved / TYPE (PCM = 0)
//! [13:11]  MULT:  multiplier − 1
//! [10:8]   DIV:   divisor − 1
//! [7]      reserved
//! [6:4]    BITS:  000=8, 001=16, 010=20, 011=24, 100=32
//! [3:0]    CHAN:  channels − 1
//! ```
//!
//! Base rates: 48 000 Hz (BASE=0) and 44 100 Hz (BASE=1).  The effective
//! sample rate is `base × (MULT+1) / (DIV+1)`.
//!
//! ## BDL — HDA spec §3.6.2
//!
//! The Buffer Descriptor List is an array of 16-byte entries in
//! IOMMU-accessible (IOVA) memory.  Each entry has a 64-bit physical/IOVA
//! address, a 32-bit byte count, and a 32-bit flags word (bit 0 = IOC:
//! interrupt on completion).  The BDL base register `SDnBDPL` requires
//! 128-byte alignment (low 7 bits reserved).

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// SDnFMT helpers
// ---------------------------------------------------------------------------

// Bit layout constants (HDA spec §3.3.41, table 31).
const SDNFMT_BASE_SHIFT: u16 = 14;
const SDNFMT_MULT_SHIFT: u16 = 11;
const SDNFMT_DIV_SHIFT: u16 = 8;
const SDNFMT_BITS_SHIFT: u16 = 4;

/// BITS field encoding for each sample width (bits[6:4]).
const BITS_8: u16 = 0b000;
const BITS_16: u16 = 0b001;
const BITS_20: u16 = 0b010;
const BITS_24: u16 = 0b011;
const BITS_32: u16 = 0b100;

/// Encode a sample rate, bit depth, and channel count into the 16-bit
/// `SDnFMT` register / `SET_STREAM_FORMAT` verb payload.
///
/// # Rate encoding
///
/// HDA has two base rate families:
/// - **48 kHz family** (`BASE = 0`): base = 48 000 Hz.
/// - **44.1 kHz family** (`BASE = 1`): base = 44 100 Hz.
///
/// The effective rate is `base × (MULT + 1) / (DIV + 1)`.  This function
/// supports MULT ∈ {0, 1, 2, 3, 4} (i.e. ×1 through ×5) and
/// DIV ∈ {0, 1, 2, 3, 4, 5, 6, 7} (i.e. ÷1 through ÷8).
///
/// If no exact MULT/DIV pair is found in either family, the function falls
/// back to the 48 kHz base with MULT = 0 / DIV = 0 (48 000 Hz, 16-bit,
/// stereo-equivalent layout) to guarantee a valid register value.  The
/// caller is responsible for choosing a rate that the hardware actually
/// supports per `PARAM_SUPPORTED_PCM_RATES`.
///
/// # Bit-depth encoding (bits[6:4])
///
/// | `bits` | BITS field |
/// |--------|-----------|
/// | 8      | 0b000     |
/// | 16     | 0b001     |
/// | 20     | 0b010     |
/// | 24     | 0b011     |
/// | 32     | 0b100     |
///
/// Unknown depths fall back to 16-bit.
///
/// # Verified values
///
/// - `encode_sdnfmt(48000, 16, 2) == 0x0011`
/// - `encode_sdnfmt(44100, 16, 2)` has bit 14 set (`& 0x4000 != 0`)
pub fn encode_sdnfmt(rate_hz: u32, bits: u8, channels: u8) -> u16 {
    // --- BITS field ---
    let bits_field: u16 = match bits {
        8 => BITS_8,
        16 => BITS_16,
        20 => BITS_20,
        24 => BITS_24,
        32 => BITS_32,
        _ => BITS_16, // safe fallback
    };

    // --- CHAN field (channels − 1) ---
    let chan_field: u16 = channels.saturating_sub(1) as u16 & 0xF;

    // --- BASE / MULT / DIV ---
    let (base_bit, mult, div) = encode_rate(rate_hz);

    // Assemble: [14]=base, [13:11]=mult, [10:8]=div, [6:4]=bits, [3:0]=chan
    let fmt: u16 = ((base_bit as u16) << SDNFMT_BASE_SHIFT)
        | ((mult as u16) << SDNFMT_MULT_SHIFT)
        | ((div as u16) << SDNFMT_DIV_SHIFT)
        | (bits_field << SDNFMT_BITS_SHIFT)
        | chan_field;

    fmt
}

/// Compute (base_bit, mult, div) for `rate_hz`.
///
/// Returns the first exact match found across both base-rate families,
/// scanning MULT 0..=4 and DIV 0..=7.  Falls back to (0, 0, 0) = 48 kHz
/// if no exact match exists (with the limitation that the hardware will
/// output 48 000 Hz instead).
fn encode_rate(rate_hz: u32) -> (u8, u8, u8) {
    const BASE_48K: u32 = 48_000;
    const BASE_441: u32 = 44_100;

    for (base_bit, base) in [(0u8, BASE_48K), (1u8, BASE_441)] {
        for mult in 0u8..=4 {
            for div in 0u8..=7 {
                // effective = base * (mult+1) / (div+1)
                let num = base * (mult as u32 + 1);
                let den = div as u32 + 1;
                if num.is_multiple_of(den) && num / den == rate_hz {
                    return (base_bit, mult, div);
                }
            }
        }
    }

    // Fallback: 48 000 Hz (MULT=0, DIV=0) — document the limitation.
    // The caller must check PARAM_SUPPORTED_PCM_RATES and pick a supported rate.
    (0, 0, 0)
}

// ---------------------------------------------------------------------------
// BDL — Buffer Descriptor List (HDA spec §3.6.2)
// ---------------------------------------------------------------------------

/// IOC (Interrupt On Completion) flag bit in `BdlEntry::flags`.
pub const BDL_IOC: u32 = 1 << 0;

/// A single 16-byte Buffer Descriptor List entry.
///
/// The BDL base address (`SDnBDPL` / `SDnBDPU`) must be 128-byte aligned;
/// the low 7 bits of `SDnBDPL` are reserved (HDA spec §3.3.43).  Each
/// entry's `addr` must also be 128-byte aligned per the same spec section.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BdlEntry {
    /// IOVA (or physical address in non-IOMMU mode) of the PCM buffer
    /// chunk.  Must be 128-byte aligned.
    pub addr: u64,
    /// Length in bytes of this chunk.  Must be a multiple of 128.
    pub len: u32,
    /// Flags: bit 0 = `BDL_IOC` (interrupt on completion).
    pub flags: u32,
}

/// Partition `[buffer_iova, buffer_iova + total_len)` into BDL entries of
/// `chunk_len` bytes each (the last entry may be shorter if `total_len` is
/// not a multiple of `chunk_len`).
///
/// # Panics
///
/// Panics if:
/// - `buffer_iova % 128 != 0` — BDL base must be 128-byte aligned.
/// - `chunk_len % 128 != 0` — chunk boundaries must be 128-byte aligned.
/// - `chunk_len == 0` — zero-size chunks are invalid.
///
/// All entries have `BDL_IOC` set so the DMA engine fires a BCIS interrupt
/// at the end of every chunk (required for LPIB-based position tracking).
pub fn build_bdl(buffer_iova: u64, total_len: u32, chunk_len: u32) -> Vec<BdlEntry> {
    assert_eq!(
        buffer_iova % 128,
        0,
        "BDL buffer_iova {buffer_iova:#x} must be 128-byte aligned \
         (SDnBDPL low-7-bits are reserved — HDA spec §3.3.43)"
    );
    assert!(chunk_len > 0, "chunk_len must be non-zero");
    assert_eq!(
        chunk_len % 128,
        0,
        "chunk_len {chunk_len} must be a multiple of 128 bytes \
         (BDL entry addresses must be 128-byte aligned)"
    );

    let mut entries = Vec::new();
    let mut offset: u32 = 0;

    while offset < total_len {
        let remaining = total_len - offset;
        let this_len = remaining.min(chunk_len);
        entries.push(BdlEntry {
            addr: buffer_iova + offset as u64,
            len: this_len,
            flags: BDL_IOC,
        });
        offset += this_len;
    }

    entries
}

/// Cyclic Buffer Length: the sum of all BDL entry lengths.
///
/// This is the value to program into `SDnCBL`.
pub fn bdl_cbl(entries: &[BdlEntry]) -> u32 {
    entries.iter().map(|e| e.len).sum()
}

/// Last Valid Index: `entries.len() - 1`, cast to `u16`.
///
/// This is the value to program into `SDnLVI`.  Panics if `entries` is
/// empty (an empty BDL is invalid).
pub fn bdl_lvi(entries: &[BdlEntry]) -> u16 {
    assert!(!entries.is_empty(), "BDL must have at least one entry");
    (entries.len() as u16) - 1
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- encode_sdnfmt ----

    /// Core compliance: encode_sdnfmt(48000, 16, 2) must equal 0x0011.
    ///
    /// Derivation:
    ///   BASE=0 (48k family) → bit14=0
    ///   MULT=0 (×1)         → bits[13:11]=0b000
    ///   DIV=0  (÷1)         → bits[10:8]=0b000
    ///   BITS=001 (16-bit)   → bits[6:4]=0b001
    ///   CHAN=1   (2−1)      → bits[3:0]=0b0001
    ///   Result: 0b0000_0000_0001_0001 = 0x0011
    #[test]
    fn sdnfmt_48k_stereo_16() {
        assert_eq!(
            encode_sdnfmt(48_000, 16, 2),
            0x0011,
            "48 kHz / 16-bit / 2-ch must encode to 0x0011"
        );

        // 44.1 kHz must have BASE bit set (bit 14 = 0x4000).
        let fmt_441 = encode_sdnfmt(44_100, 16, 2);
        assert_ne!(
            fmt_441 & 0x4000,
            0,
            "44.1 kHz must have bit 14 (BASE) set, got {fmt_441:#06x}"
        );
    }

    /// Verify BITS field encoding for each supported width.
    #[test]
    fn sdnfmt_bits_encoding() {
        // Extract BITS field (bits[6:4]) only.
        let bits_field = |w| (encode_sdnfmt(48_000, w, 1) >> 4) & 0x7;
        assert_eq!(bits_field(8), 0b000, "8-bit");
        assert_eq!(bits_field(16), 0b001, "16-bit");
        assert_eq!(bits_field(20), 0b010, "20-bit");
        assert_eq!(bits_field(24), 0b011, "24-bit");
        assert_eq!(bits_field(32), 0b100, "32-bit");
    }

    /// Channel field: channels − 1 in bits[3:0].
    #[test]
    fn sdnfmt_channel_field() {
        for ch in 1u8..=8 {
            let fmt = encode_sdnfmt(48_000, 16, ch);
            let chan_field = fmt & 0xF;
            assert_eq!(chan_field, (ch - 1) as u16, "channels={ch}");
        }
    }

    /// A few common rates that should encode exactly.
    #[test]
    fn sdnfmt_common_rates_exact() {
        // 96 kHz = 48k × 2 / 1 → BASE=0, MULT=1, DIV=0
        let fmt_96k = encode_sdnfmt(96_000, 16, 2);
        let base_bit = (fmt_96k >> 14) & 1;
        let mult = (fmt_96k >> 11) & 0x7;
        let div = (fmt_96k >> 8) & 0x7;
        assert_eq!(base_bit, 0, "96 kHz: BASE must be 0 (48k family)");
        assert_eq!(mult, 1, "96 kHz: MULT must be 1 (×2)");
        assert_eq!(div, 0, "96 kHz: DIV must be 0 (÷1)");

        // 88.2 kHz = 44.1k × 2 / 1 → BASE=1, MULT=1, DIV=0
        let fmt_882k = encode_sdnfmt(88_200, 16, 2);
        let base_bit = (fmt_882k >> 14) & 1;
        let mult = (fmt_882k >> 11) & 0x7;
        let div = (fmt_882k >> 8) & 0x7;
        assert_eq!(base_bit, 1, "88.2 kHz: BASE must be 1 (44.1k family)");
        assert_eq!(mult, 1, "88.2 kHz: MULT must be 1 (×2)");
        assert_eq!(div, 0, "88.2 kHz: DIV must be 0 (÷1)");

        // 16 kHz = 48k × 1 / 3 → BASE=0, MULT=0, DIV=2
        let fmt_16k = encode_sdnfmt(16_000, 16, 2);
        let base_bit = (fmt_16k >> 14) & 1;
        let mult = (fmt_16k >> 11) & 0x7;
        let div = (fmt_16k >> 8) & 0x7;
        assert_eq!(base_bit, 0, "16 kHz: BASE must be 0 (48k family)");
        assert_eq!(mult, 0, "16 kHz: MULT must be 0");
        assert_eq!(div, 2, "16 kHz: DIV must be 2 (÷3)");
    }

    /// An unsupported rate must still produce a valid (non-panicking) result
    /// that falls back to 48 kHz.
    #[test]
    fn sdnfmt_fallback_unsupported_rate() {
        // 31337 Hz is not representable — fallback to 48k (BASE=0, MULT=0, DIV=0).
        let fmt = encode_sdnfmt(31_337, 16, 2);
        let base_bit = (fmt >> 14) & 1;
        let mult = (fmt >> 11) & 0x7;
        let div = (fmt >> 8) & 0x7;
        assert_eq!(base_bit, 0, "fallback: BASE=0");
        assert_eq!(mult, 0, "fallback: MULT=0");
        assert_eq!(div, 0, "fallback: DIV=0");
        // BITS and CHAN must still be encoded correctly even on fallback.
        assert_eq!((fmt >> 4) & 0x7, 0b001, "fallback: BITS=16-bit");
        assert_eq!(fmt & 0xF, 1, "fallback: CHAN=1 (2-1)");
    }

    // ---- BDL ----

    /// Build a BDL and assert structural invariants.
    #[test]
    fn bdl_consistency() {
        let base: u64 = 0x0010_0000; // 128-byte aligned
        let total: u32 = 4 * 4096; // 16 KiB
        let chunk: u32 = 4096; // 4 KiB per entry (128-byte aligned)

        let entries = build_bdl(base, total, chunk);

        // 4 entries expected
        assert_eq!(entries.len(), 4, "4 chunks of 4 KiB in 16 KiB buffer");

        // Every entry must have IOC set and its addr 128-byte aligned.
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.flags & BDL_IOC, BDL_IOC, "entry {i}: IOC must be set");
            assert_eq!(
                e.addr % 128,
                0,
                "entry {i}: addr {:#x} must be 128-byte aligned",
                e.addr
            );
        }

        // CBL must equal total_len
        assert_eq!(bdl_cbl(&entries), total, "bdl_cbl must equal total_len");

        // LVI must be len−1
        assert_eq!(
            bdl_lvi(&entries),
            (entries.len() as u16) - 1,
            "bdl_lvi must be entries.len() - 1"
        );
    }

    /// A total_len that is not a multiple of chunk_len: last entry is shorter.
    #[test]
    fn bdl_last_entry_shorter() {
        let base: u64 = 0x0010_0000;
        let total: u32 = 5 * 128; // 640 bytes
        let chunk: u32 = 2 * 128; // 256 bytes per chunk → 2 full + 1 partial

        let entries = build_bdl(base, total, chunk);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].len, 256);
        assert_eq!(entries[1].len, 256);
        assert_eq!(entries[2].len, 128); // remainder
        assert_eq!(bdl_cbl(&entries), total);
    }

    /// Single-chunk BDL (total_len == chunk_len).
    #[test]
    fn bdl_single_chunk() {
        let base: u64 = 0x0020_0000;
        let total: u32 = 4096;
        let chunk: u32 = 4096;

        let entries = build_bdl(base, total, chunk);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].addr, base);
        assert_eq!(entries[0].len, total);
        assert_eq!(entries[0].flags, BDL_IOC);
        assert_eq!(bdl_lvi(&entries), 0);
    }

    /// Misaligned buffer_iova must panic.
    #[test]
    #[should_panic(expected = "128-byte aligned")]
    fn bdl_misaligned_base_panics() {
        build_bdl(0x0010_0001, 4096, 4096);
    }

    /// Misaligned chunk_len must panic.
    #[test]
    #[should_panic(expected = "multiple of 128")]
    fn bdl_misaligned_chunk_panics() {
        build_bdl(0x0010_0000, 4096, 100);
    }

    /// Zero-sized chunk must panic.
    #[test]
    #[should_panic(expected = "non-zero")]
    fn bdl_zero_chunk_panics() {
        build_bdl(0x0010_0000, 4096, 0);
    }

    /// BDL entry size must be exactly 16 bytes (spec §3.6.2 requirement).
    #[test]
    fn bdl_entry_size_is_16_bytes() {
        assert_eq!(
            core::mem::size_of::<BdlEntry>(),
            16,
            "BdlEntry must be 16 bytes (HDA spec §3.6.2)"
        );
    }
}
