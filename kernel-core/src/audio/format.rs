//! PCM format value types — Phase 57 B.1.
//!
//! The chosen first audio target (AC'97; see
//! `docs/appendix/phase-57-audio-target-choice.md`) constrains the type
//! surface to exactly:
//!
//! - [`PcmFormat::S16Le`] — 16-bit signed little-endian (only format AC'97
//!   exposes in Phase 57 with the variable-rate extension disabled).
//! - [`SampleRate::Hz48000`] — 48 kHz fixed (`VRA` disabled).
//! - [`ChannelLayout::Mono`] / [`ChannelLayout::Stereo`].
//!
//! No speculative variants are added; widening the surface waits on a
//! later phase and a documented backend change.
//!
//! [`frame_size_bytes`] is the total, panic-free function consumers use
//! to size DMA rings, ring buffers, and submit windows. Adding a new
//! `PcmFormat` or `ChannelLayout` variant in the future requires
//! extending the match arms here and the unit-test matrix in the
//! `tests` module — the compiler enforces that obligation because the
//! enum match is exhaustive.

/// Sample-encoding for the PCM stream.
///
/// The chosen first audio target (AC'97) supports exactly one format in
/// Phase 57 — see the audio target memo. `S16Le` means: each sample is
/// a 16-bit signed integer, little-endian on the wire and in the DMA
/// ring buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum PcmFormat {
    /// 16-bit signed integer, little-endian.
    S16Le,
}

/// Sample rate.
///
/// AC'97 with the variable-rate extension (`VRA`) disabled exposes only
/// the fixed 48 kHz rate in Phase 57. Adding a new variant requires
/// re-enabling `VRA` and updating both the AC'97 register-program path
/// (Track D.2) and the format-test matrix below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum SampleRate {
    /// 48 000 samples per second.
    Hz48000,
}

impl SampleRate {
    /// Numeric sample rate in Hz.
    pub const fn as_hz(&self) -> u32 {
        match self {
            SampleRate::Hz48000 => 48_000,
        }
    }
}

/// Channel layout.
///
/// AC'97 PCM-out supports both mono (single channel) and interleaved
/// stereo (left / right). The driver path uses [`channel_count`] to
/// compute frame-size and DMA-ring stride.
///
/// [`channel_count`]: ChannelLayout::channel_count
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ChannelLayout {
    /// Single audio channel.
    Mono,
    /// Two interleaved channels (left, right).
    Stereo,
}

impl ChannelLayout {
    /// Number of audio channels in this layout.
    pub const fn channel_count(&self) -> u8 {
        match self {
            ChannelLayout::Mono => 1,
            ChannelLayout::Stereo => 2,
        }
    }
}

/// Bytes per frame, where a "frame" is one sample per channel.
///
/// Total function: every (format, layout) pair has a defined value and
/// the function never panics. Callers can use this to compute DMA
/// buffer sizes (`frames * frame_size_bytes(...)`) without unwrapping a
/// `Result`.
pub const fn frame_size_bytes(format: PcmFormat, layout: ChannelLayout) -> usize {
    let bytes_per_sample: usize = match format {
        PcmFormat::S16Le => 2,
    };
    bytes_per_sample * (layout.channel_count() as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // PcmFormat
    // -----------------------------------------------------------------------

    #[test]
    fn pcm_format_s16le_is_present() {
        let _ = PcmFormat::S16Le;
    }

    #[test]
    fn pcm_format_value_equality() {
        assert_eq!(PcmFormat::S16Le, PcmFormat::S16Le);
    }

    // -----------------------------------------------------------------------
    // SampleRate
    // -----------------------------------------------------------------------

    #[test]
    fn sample_rate_hz48000_is_present() {
        let _ = SampleRate::Hz48000;
    }

    #[test]
    fn sample_rate_as_hz_returns_48000() {
        assert_eq!(SampleRate::Hz48000.as_hz(), 48_000);
    }

    // -----------------------------------------------------------------------
    // ChannelLayout
    // -----------------------------------------------------------------------

    #[test]
    fn channel_layout_mono_count_is_one() {
        assert_eq!(ChannelLayout::Mono.channel_count(), 1);
    }

    #[test]
    fn channel_layout_stereo_count_is_two() {
        assert_eq!(ChannelLayout::Stereo.channel_count(), 2);
    }

    // -----------------------------------------------------------------------
    // frame_size_bytes — exhaustive (format, layout) matrix
    // -----------------------------------------------------------------------

    #[test]
    fn frame_size_bytes_s16le_mono() {
        // 16 bits = 2 bytes per sample × 1 channel = 2 bytes per frame.
        assert_eq!(frame_size_bytes(PcmFormat::S16Le, ChannelLayout::Mono), 2);
    }

    #[test]
    fn frame_size_bytes_s16le_stereo() {
        // 16 bits = 2 bytes per sample × 2 channels = 4 bytes per frame.
        assert_eq!(
            frame_size_bytes(PcmFormat::S16Le, ChannelLayout::Stereo),
            4
        );
    }

    #[test]
    fn frame_size_bytes_is_total_function_panic_free() {
        // Exhaustively call every combination; each must return a non-zero
        // size and never panic. This exercises the "total function,
        // panic-free" acceptance bullet for B.1.
        for format in [PcmFormat::S16Le] {
            for layout in [ChannelLayout::Mono, ChannelLayout::Stereo] {
                let n = frame_size_bytes(format, layout);
                assert!(
                    n > 0,
                    "frame_size_bytes({format:?}, {layout:?}) must be non-zero"
                );
            }
        }
    }

    #[test]
    fn types_are_copy_clone_and_debug() {
        // Type-system invariant: every PCM format value is cheap to copy
        // and observable in tests/log output. The trait bounds on the test
        // bindings fail to compile if any of the types lose Copy/Clone/Debug.
        fn requires_copy<T: Copy>(_: T) {}
        fn requires_clone<T: Clone>(_: T) {}
        fn requires_debug<T: core::fmt::Debug>(_: T) {}
        requires_copy(PcmFormat::S16Le);
        requires_clone(PcmFormat::S16Le);
        requires_debug(PcmFormat::S16Le);
        requires_copy(SampleRate::Hz48000);
        requires_clone(SampleRate::Hz48000);
        requires_debug(SampleRate::Hz48000);
        requires_copy(ChannelLayout::Mono);
        requires_clone(ChannelLayout::Mono);
        requires_debug(ChannelLayout::Mono);
    }
}
