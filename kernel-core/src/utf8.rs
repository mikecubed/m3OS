//! Phase 69b Track A — strict UTF-8 byte-stream decoder.
//!
//! [`Utf8Decoder`] is a pure-logic state machine that consumes one
//! byte at a time and emits a typed [`DecoderOutput`]:
//!
//! - `Pending` — the byte advanced the state but no codepoint has been
//!   completed; more bytes are needed.
//! - `Codepoint(u32)` — a full Unicode scalar value was decoded.
//! - `Invalid` — the byte broke the in-flight (or starting) sequence;
//!   the caller emits U+FFFD and the decoder resyncs on the next
//!   valid leading byte (the W3C / WHATWG replacement-character
//!   contract).
//!
//! The decoder is `no_std`, allocation-free, and lives in
//! `kernel-core` so the kernel framebuffer console can use it once a
//! follow-up phase widens that path. It is the only piece of Phase 69b
//! the userspace `term` crate consumes before pushing decoded
//! codepoints through Phase 22b's [`crate::fb::AnsiParser`].
//!
//! ## Contract summary
//!
//! - Well-formed 1/2/3/4-byte sequences decode to the matching
//!   codepoint.
//! - Overlong encodings (e.g. `\xC0\xAF` for `/`) are rejected.
//! - UTF-16 surrogates U+D800..U+DFFF are rejected when emitted in
//!   3-byte UTF-8 form.
//! - Codepoints above U+10FFFF are rejected.
//! - On `Invalid`, the decoder returns to the initial state without
//!   consuming the offending byte's payload — the next valid leading
//!   byte starts a fresh sequence. If the offending byte is itself a
//!   valid leading byte (not a continuation), the decoder honours the
//!   W3C resync rule and reuses it to start the next sequence.

/// Output of [`Utf8Decoder::decode_byte`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderOutput {
    /// Partial sequence — more bytes needed.
    Pending,
    /// Complete codepoint decoded.
    Codepoint(u32),
    /// Malformed input — caller should emit U+FFFD. The decoder is
    /// already reset; the next byte starts a fresh sequence. This
    /// variant is returned when the offending byte cannot itself
    /// start a fresh sequence (a stray continuation, an invalid
    /// leading byte, or when an in-flight sequence broke on an
    /// equally-invalid leading byte).
    Invalid,
    /// Two-output resync: the in-flight sequence is replaced (caller
    /// emits U+FFFD) AND the offending byte was a complete ASCII
    /// codepoint. Caller emits both: replacement first, then the
    /// codepoint. Preserves valid trailing ASCII data that a strict
    /// resync would otherwise drop.
    InvalidThenCodepoint(u32),
    /// Two-output resync: the in-flight sequence is replaced (caller
    /// emits U+FFFD) AND the offending byte started a fresh multi-byte
    /// sequence. The decoder is now Pending on that new sequence; the
    /// next byte continues it.
    InvalidThenPending,
}

/// Internal state of the decoder. The number embedded in each multi-
/// byte variant is the value of the codepoint accumulated so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No bytes seen for the current codepoint.
    Initial,
    /// One leading byte of a 2-byte sequence consumed; one continuation
    /// byte remaining.
    Awaiting2 { value: u32 },
    /// One leading byte of a 3-byte sequence consumed; two continuation
    /// bytes remaining.
    Awaiting3a { value: u32 },
    /// Two bytes of a 3-byte sequence consumed; one continuation byte
    /// remaining.
    Awaiting3b { value: u32 },
    /// One leading byte of a 4-byte sequence consumed; three continuation
    /// bytes remaining.
    Awaiting4a { value: u32 },
    /// Two bytes of a 4-byte sequence consumed; two continuation bytes
    /// remaining.
    Awaiting4b { value: u32 },
    /// Three bytes of a 4-byte sequence consumed; one continuation byte
    /// remaining.
    Awaiting4c { value: u32 },
}

/// Strict UTF-8 decoder. Allocation-free, `no_std`, single-byte feed.
#[derive(Debug, Clone, Copy)]
pub struct Utf8Decoder {
    state: State,
}

impl Default for Utf8Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Utf8Decoder {
    /// Construct a fresh decoder in the initial state.
    pub const fn new() -> Self {
        Self {
            state: State::Initial,
        }
    }

    /// Reset the decoder to the initial state. Used internally on
    /// `Invalid`; callers do not normally need this.
    pub fn reset(&mut self) {
        self.state = State::Initial;
    }

    /// Feed one byte through the state machine. Returns the typed
    /// [`DecoderOutput`].
    pub fn decode_byte(&mut self, byte: u8) -> DecoderOutput {
        match self.state {
            State::Initial => self.decode_leading(byte),
            State::Awaiting2 { value } => self.decode_cont(byte, value, |v| {
                // Two-byte sequence. Reject overlong: the minimum
                // value for a 2-byte sequence is U+0080.
                if v < 0x80 {
                    DecoderOutput::Invalid
                } else {
                    DecoderOutput::Codepoint(v)
                }
            }),
            State::Awaiting3a { value } => {
                self.decode_cont_to(byte, value, |v| State::Awaiting3b { value: v })
            }
            State::Awaiting3b { value } => self.decode_cont(byte, value, |v| {
                // Three-byte sequence: reject overlong (< U+0800) and
                // UTF-16 surrogates (U+D800..U+DFFF).
                if v < 0x0800 || (0xD800..=0xDFFF).contains(&v) {
                    DecoderOutput::Invalid
                } else {
                    DecoderOutput::Codepoint(v)
                }
            }),
            State::Awaiting4a { value } => {
                self.decode_cont_to(byte, value, |v| State::Awaiting4b { value: v })
            }
            State::Awaiting4b { value } => {
                self.decode_cont_to(byte, value, |v| State::Awaiting4c { value: v })
            }
            State::Awaiting4c { value } => self.decode_cont(byte, value, |v| {
                // Four-byte sequence: reject overlong (< U+10000) and
                // out-of-range (> U+10FFFF).
                if !(0x10000..=0x10FFFF).contains(&v) {
                    DecoderOutput::Invalid
                } else {
                    DecoderOutput::Codepoint(v)
                }
            }),
        }
    }

    /// Decode the first byte of a fresh sequence.
    fn decode_leading(&mut self, byte: u8) -> DecoderOutput {
        // 0xxxxxxx — 1-byte ASCII.
        if byte & 0x80 == 0 {
            // Decoder already in Initial — no state change.
            return DecoderOutput::Codepoint(byte as u32);
        }
        // 10xxxxxx — stray continuation; not a valid leading byte.
        if byte & 0xC0 == 0x80 {
            return DecoderOutput::Invalid;
        }
        // 110xxxxx — 2-byte leading.
        if byte & 0xE0 == 0xC0 {
            // Compute partial value: low 5 bits.
            let value = (byte & 0x1F) as u32;
            // Eagerly reject the two overlong 2-byte leading bytes
            // (0xC0, 0xC1). These can only produce U+0000..U+007F
            // which must be encoded as 1 byte. Rejecting at the
            // leading byte matches the W3C "Best Practices for U+FFFD
            // Substitution" recommendation — one replacement
            // character, no follow-on consumption.
            if byte == 0xC0 || byte == 0xC1 {
                return DecoderOutput::Invalid;
            }
            self.state = State::Awaiting2 { value };
            return DecoderOutput::Pending;
        }
        // 1110xxxx — 3-byte leading.
        if byte & 0xF0 == 0xE0 {
            let value = (byte & 0x0F) as u32;
            self.state = State::Awaiting3a { value };
            return DecoderOutput::Pending;
        }
        // 11110xxx — 4-byte leading. Reject obvious out-of-range
        // leaders (0xF5..=0xFF) which can only produce codepoints
        // > U+10FFFF (or the invalid 0xFE/0xFF prefixes).
        if byte & 0xF8 == 0xF0 {
            if byte > 0xF4 {
                return DecoderOutput::Invalid;
            }
            let value = (byte & 0x07) as u32;
            self.state = State::Awaiting4a { value };
            return DecoderOutput::Pending;
        }
        // 0xF8..=0xFF — not a valid UTF-8 leading byte under RFC 3629.
        DecoderOutput::Invalid
    }

    /// Consume one continuation byte. If valid, fold its 6 low bits
    /// into `value` and call `done` to finalise the codepoint. If the
    /// byte is not a continuation byte (`10xxxxxx`), reset and report
    /// `Invalid`.
    fn decode_cont<F>(&mut self, byte: u8, value: u32, done: F) -> DecoderOutput
    where
        F: FnOnce(u32) -> DecoderOutput,
    {
        if byte & 0xC0 != 0x80 {
            // Resync per W3C: drop the in-flight sequence and reuse the
            // offending byte to start the next one.
            self.state = State::Initial;
            return self.resync_after_invalid(byte);
        }
        let combined = (value << 6) | ((byte & 0x3F) as u32);
        self.state = State::Initial;
        done(combined)
    }

    /// Same as [`decode_cont`] but transitions to a new intermediate
    /// state instead of finalising the codepoint.
    fn decode_cont_to<F>(&mut self, byte: u8, value: u32, next: F) -> DecoderOutput
    where
        F: FnOnce(u32) -> State,
    {
        if byte & 0xC0 != 0x80 {
            self.state = State::Initial;
            return self.resync_after_invalid(byte);
        }
        let combined = (value << 6) | ((byte & 0x3F) as u32);
        self.state = next(combined);
        DecoderOutput::Pending
    }

    /// W3C "Best Practices for U+FFFD Substitution" — when a
    /// continuation byte is missing, emit one U+FFFD for the in-flight
    /// sequence and re-process the offending byte as a fresh leading
    /// byte. The decoder has already been reset to [`State::Initial`]
    /// by the caller. We re-feed the byte through [`decode_leading`]
    /// and fold its result into a combined two-output variant so the
    /// caller never loses valid trailing data:
    ///
    /// - ASCII byte → [`DecoderOutput::InvalidThenCodepoint`] carrying
    ///   the ASCII codepoint.
    /// - Valid 2/3/4-byte leading byte → [`DecoderOutput::InvalidThenPending`];
    ///   the decoder is now Pending on the new sequence.
    /// - Invalid byte (stray continuation excluded by the caller, plus
    ///   0xC0/0xC1/0xF5..=0xFF) → [`DecoderOutput::Invalid`] (single
    ///   replacement). No valid data is lost.
    fn resync_after_invalid(&mut self, byte: u8) -> DecoderOutput {
        match self.decode_leading(byte) {
            DecoderOutput::Codepoint(c) => DecoderOutput::InvalidThenCodepoint(c),
            DecoderOutput::Pending => DecoderOutput::InvalidThenPending,
            DecoderOutput::Invalid => DecoderOutput::Invalid,
            // decode_leading never produces the combined variants.
            DecoderOutput::InvalidThenCodepoint(_) | DecoderOutput::InvalidThenPending => {
                DecoderOutput::Invalid
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(decoder: &mut Utf8Decoder, bytes: &[u8]) -> alloc::vec::Vec<DecoderOutput> {
        bytes.iter().map(|b| decoder.decode_byte(*b)).collect()
    }

    /// New decoder starts in the initial state and accepts pure ASCII
    /// one byte at a time.
    #[test]
    fn new_decoder_is_initial_and_accepts_ascii() {
        let mut d = Utf8Decoder::new();
        assert!(matches!(d.state, State::Initial));
        assert_eq!(d.decode_byte(b'A'), DecoderOutput::Codepoint(b'A' as u32));
        assert!(matches!(d.state, State::Initial));
    }

    /// Every ASCII byte 0x00..=0x7F decodes as a 1-byte sequence.
    #[test]
    fn ascii_range_full_coverage() {
        let mut d = Utf8Decoder::new();
        for b in 0u8..=0x7F {
            assert_eq!(d.decode_byte(b), DecoderOutput::Codepoint(b as u32));
            assert!(matches!(d.state, State::Initial));
        }
    }

    /// 2-byte sequences: low-end (U+0080 → C2 80) and high-end (U+07FF
    /// → DF BF).
    #[test]
    fn two_byte_sequence_low_and_high() {
        let mut d = Utf8Decoder::new();
        // U+0080 — minimum 2-byte codepoint
        assert_eq!(d.decode_byte(0xC2), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0x80), DecoderOutput::Codepoint(0x0080));
        // U+07FF — maximum 2-byte codepoint
        assert_eq!(d.decode_byte(0xDF), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0xBF), DecoderOutput::Codepoint(0x07FF));
    }

    /// 3-byte sequences: low-end (U+0800 → E0 A0 80) and high-end
    /// (U+FFFF → EF BF BF).
    #[test]
    fn three_byte_sequence_low_and_high() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xE0), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0xA0), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0x80), DecoderOutput::Codepoint(0x0800));

        assert_eq!(d.decode_byte(0xEF), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0xBF), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0xBF), DecoderOutput::Codepoint(0xFFFF));
    }

    /// 4-byte sequences: low-end (U+10000 → F0 90 80 80) and high-end
    /// (U+10FFFF → F4 8F BF BF).
    #[test]
    fn four_byte_sequence_low_and_high() {
        let mut d = Utf8Decoder::new();
        let outs = feed_all(&mut d, &[0xF0, 0x90, 0x80, 0x80]);
        assert_eq!(
            outs,
            &[
                DecoderOutput::Pending,
                DecoderOutput::Pending,
                DecoderOutput::Pending,
                DecoderOutput::Codepoint(0x10000),
            ]
        );

        let outs = feed_all(&mut d, &[0xF4, 0x8F, 0xBF, 0xBF]);
        assert_eq!(
            outs,
            &[
                DecoderOutput::Pending,
                DecoderOutput::Pending,
                DecoderOutput::Pending,
                DecoderOutput::Codepoint(0x10FFFF),
            ]
        );
    }

    /// Common cases hit by real apps: U+2500 (─ box-drawing horizontal),
    /// U+00E9 (é), U+4E2D (中).
    #[test]
    fn well_known_codepoints_decode_correctly() {
        let mut d = Utf8Decoder::new();
        // U+2500 ─ 3 bytes: E2 94 80
        let outs = feed_all(&mut d, &[0xE2, 0x94, 0x80]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Codepoint(0x2500)));
        // U+00E9 é 2 bytes: C3 A9
        let outs = feed_all(&mut d, &[0xC3, 0xA9]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Codepoint(0x00E9)));
        // U+4E2D 中 3 bytes: E4 B8 AD
        let outs = feed_all(&mut d, &[0xE4, 0xB8, 0xAD]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Codepoint(0x4E2D)));
    }

    /// Overlong 2-byte encodings (lead 0xC0 / 0xC1) are rejected at
    /// the leading byte.
    #[test]
    fn overlong_two_byte_rejected_at_leading() {
        let mut d = Utf8Decoder::new();
        // 0xC0 0xAF would decode as `/` in an overlong encoding.
        assert_eq!(d.decode_byte(0xC0), DecoderOutput::Invalid);
        assert!(matches!(d.state, State::Initial));
        // 0xC1 0xBF — overlong U+007F.
        assert_eq!(d.decode_byte(0xC1), DecoderOutput::Invalid);
        assert!(matches!(d.state, State::Initial));
    }

    /// Surrogates (U+D800..U+DFFF) emitted as 3-byte UTF-8 must be
    /// rejected at the trailing byte.
    #[test]
    fn surrogates_rejected() {
        let mut d = Utf8Decoder::new();
        // U+D800 → ED A0 80
        let outs = feed_all(&mut d, &[0xED, 0xA0, 0x80]);
        assert_eq!(
            outs,
            &[
                DecoderOutput::Pending,
                DecoderOutput::Pending,
                DecoderOutput::Invalid,
            ]
        );
        // U+DFFF → ED BF BF
        let outs = feed_all(&mut d, &[0xED, 0xBF, 0xBF]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Invalid));
    }

    /// Codepoints above U+10FFFF are rejected at the trailing byte.
    #[test]
    fn above_max_codepoint_rejected() {
        let mut d = Utf8Decoder::new();
        // U+110000 would be F4 90 80 80 — strictly above the cap.
        let outs = feed_all(&mut d, &[0xF4, 0x90, 0x80, 0x80]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Invalid));
    }

    /// Out-of-range 4-byte leading bytes (>= 0xF5) are rejected
    /// immediately at the leading byte.
    #[test]
    fn out_of_range_four_byte_leader_rejected() {
        let mut d = Utf8Decoder::new();
        for b in 0xF5u8..=0xFF {
            assert_eq!(d.decode_byte(b), DecoderOutput::Invalid);
            assert!(matches!(d.state, State::Initial));
        }
    }

    /// A stray continuation byte (10xxxxxx) at the start of a sequence
    /// is rejected immediately.
    #[test]
    fn stray_continuation_rejected_at_leading() {
        let mut d = Utf8Decoder::new();
        for b in 0x80u8..=0xBF {
            assert_eq!(d.decode_byte(b), DecoderOutput::Invalid);
            assert!(matches!(d.state, State::Initial));
        }
    }

    /// Overlong 3-byte encoding (e.g. E0 80 80 for U+0000) is rejected
    /// at the trailing byte.
    #[test]
    fn overlong_three_byte_rejected() {
        let mut d = Utf8Decoder::new();
        // E0 80 80 — overlong U+0000.
        let outs = feed_all(&mut d, &[0xE0, 0x80, 0x80]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Invalid));
        // E0 9F BF — overlong U+07FF (border of 2-byte range).
        let outs = feed_all(&mut d, &[0xE0, 0x9F, 0xBF]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Invalid));
    }

    /// Overlong 4-byte encoding (F0 80 80 80 for U+0000) is rejected
    /// at the trailing byte.
    #[test]
    fn overlong_four_byte_rejected() {
        let mut d = Utf8Decoder::new();
        let outs = feed_all(&mut d, &[0xF0, 0x80, 0x80, 0x80]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Invalid));
        // F0 8F BF BF — overlong U+FFFF.
        let outs = feed_all(&mut d, &[0xF0, 0x8F, 0xBF, 0xBF]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Invalid));
    }

    /// W3C resync case 1: a leading byte interrupted by an ASCII byte.
    /// The offending ASCII byte is preserved by the combined
    /// [`DecoderOutput::InvalidThenCodepoint`] variant so its value is
    /// not lost.
    #[test]
    fn resync_two_byte_truncated_by_ascii_preserves_ascii() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xC2), DecoderOutput::Pending);
        assert_eq!(
            d.decode_byte(b'A'),
            DecoderOutput::InvalidThenCodepoint(b'A' as u32)
        );
        assert!(matches!(d.state, State::Initial));
        // Next byte parses normally as a fresh sequence.
        assert_eq!(d.decode_byte(b'B'), DecoderOutput::Codepoint(b'B' as u32));
    }

    /// W3C resync case 2: a 3-byte sequence interrupted by an invalid
    /// leading byte (0xFF) — neither a continuation nor a valid
    /// leading byte. The output is the single-replacement [`Invalid`].
    #[test]
    fn resync_after_truncated_three_byte_invalid_leader() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xE2), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0x94), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0xFF), DecoderOutput::Invalid);
        assert!(matches!(d.state, State::Initial));
        assert_eq!(d.decode_byte(b'X'), DecoderOutput::Codepoint(b'X' as u32));
    }

    /// W3C resync case 3: a 3-byte sequence interrupted by an ASCII
    /// byte. The ASCII byte is preserved via
    /// [`DecoderOutput::InvalidThenCodepoint`].
    #[test]
    fn resync_three_byte_truncated_by_ascii_preserves_ascii() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xE2), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0x94), DecoderOutput::Pending);
        assert_eq!(
            d.decode_byte(b'X'),
            DecoderOutput::InvalidThenCodepoint(b'X' as u32)
        );
        assert!(matches!(d.state, State::Initial));
    }

    /// W3C resync case 4: a 4-byte sequence interrupted by an ASCII
    /// byte. The ASCII byte is preserved.
    #[test]
    fn resync_four_byte_truncated_by_ascii_preserves_ascii() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xF0), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0x90), DecoderOutput::Pending);
        assert_eq!(d.decode_byte(0x80), DecoderOutput::Pending);
        assert_eq!(
            d.decode_byte(b'Q'),
            DecoderOutput::InvalidThenCodepoint(b'Q' as u32)
        );
        assert!(matches!(d.state, State::Initial));
        assert_eq!(d.decode_byte(b'q'), DecoderOutput::Codepoint(b'q' as u32));
    }

    /// W3C resync case 5: a truncated multi-byte sequence followed by
    /// a fresh valid multi-byte leading byte. The decoder emits
    /// [`InvalidThenPending`] and the next byte continues the new
    /// sequence — no valid data lost.
    #[test]
    fn resync_truncated_by_multibyte_leader_preserves_sequence() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xC2), DecoderOutput::Pending);
        // Start a fresh 2-byte sequence (U+00E9 → C3 A9). The first
        // byte yields InvalidThenPending to signal both replacements
        // for the in-flight sequence AND that a new sequence has
        // begun.
        assert_eq!(d.decode_byte(0xC3), DecoderOutput::InvalidThenPending);
        // Now in Awaiting2 state; the next continuation byte completes
        // the new codepoint.
        assert_eq!(d.decode_byte(0xA9), DecoderOutput::Codepoint(0x00E9));
    }

    /// W3C resync case 4: two back-to-back malformed sequences each
    /// produce exactly one `Invalid` and the decoder resyncs on the
    /// first valid leading byte that follows.
    #[test]
    fn resync_chained_invalid_sequences() {
        let mut d = Utf8Decoder::new();
        // Two stray continuation bytes — each is its own `Invalid`.
        assert_eq!(d.decode_byte(0x80), DecoderOutput::Invalid);
        assert_eq!(d.decode_byte(0x81), DecoderOutput::Invalid);
        // Decoder is still ready to decode a real sequence.
        let outs = feed_all(&mut d, &[0xC3, 0xA9]);
        assert_eq!(outs.last().copied(), Some(DecoderOutput::Codepoint(0x00E9)));
    }

    /// A two-byte sequence with a non-continuation second byte is
    /// rejected at the would-be continuation position. When the
    /// offending byte is itself a valid ASCII byte, the combined
    /// [`DecoderOutput::InvalidThenCodepoint`] preserves it so no
    /// valid data is lost.
    #[test]
    fn two_byte_missing_continuation_rejected() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xC2), DecoderOutput::Pending);
        // 0x40 is in the ASCII range — not a continuation byte. The
        // truncated in-flight sequence yields a U+FFFD AND the 0x40
        // codepoint is preserved by the combined variant.
        assert_eq!(
            d.decode_byte(0x40),
            DecoderOutput::InvalidThenCodepoint(0x40)
        );
        assert!(matches!(d.state, State::Initial));
    }

    /// Reset returns the decoder to the initial state mid-sequence.
    #[test]
    fn reset_returns_to_initial_mid_sequence() {
        let mut d = Utf8Decoder::new();
        assert_eq!(d.decode_byte(0xE2), DecoderOutput::Pending);
        d.reset();
        // Subsequent stray continuation is rejected, not folded into
        // the abandoned sequence.
        assert_eq!(d.decode_byte(0x94), DecoderOutput::Invalid);
    }
}
