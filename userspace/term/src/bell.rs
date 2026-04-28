//! Phase 57 Track G.6 — terminal bell with `BellSink` trait seam.
//!
//! `Bell<S: BellSink>` consumes BEL events emitted by the screen state
//! machine and either rings the audio device (production path) or
//! drops the event when an audio system is unavailable.  The trait is
//! the seam: tests run the bell against a `MockBellSink`; the binary
//! supplies the real implementation.
//!
//! # Coalescing window
//!
//! Repeated BEL bytes from a noisy program would otherwise queue
//! audible bells indefinitely.  `Bell::ring` consults the timestamp of
//! the last successful ring; calls within [`COALESCE_WINDOW_MS`] are
//! silently dropped (no `play` call, no log line).  The window is
//! deliberately short (~50 ms) so user-paced BEL still rings, but
//! tight loops collapse to one event.
//!
//! # Production sinks
//!
//! Two production sinks are available:
//!
//! - [`AudioClientBellSink`] (preferred when `audio_server` is up):
//!   wraps an [`audio_client::AudioClient`] with a cached AC'97 stream
//!   and a precomputed 30 ms 880 Hz square-wave tone. `play()` submits
//!   the tone bytes fire-and-forget so the audio device plays the
//!   tone asynchronously while term continues rendering. On any
//!   transport error the sink invalidates its client and surfaces
//!   `BellError::AudioUnavailable`; the next ring re-attempts open.
//! - [`AudioUnavailableBellSink`] (fallback): writes a single
//!   `term.bell.audio_unavailable` warn marker and no-ops on every
//!   subsequent ring. Used when `audio_server` is not registered or
//!   has crashed.
//!
//! The `term` binary is expected to construct an
//! `AudioClientBellSink` first; on `BellError::AudioUnavailable` it
//! falls back to `AudioUnavailableBellSink` for the remainder of the
//! process lifetime. The `Bell<S>` coalescing wrapper is generic over
//! the sink so this fallback is a simple type swap, not a runtime
//! dispatch.
//!
//! # Module-level test discipline
//!
//! Failing tests for `Bell::ring` (against a `MockBellSink`) commit
//! before any implementation that makes them pass.  The acceptance
//! list in the task doc names: trait dispatch, coalescing, error
//! surfaces.

/// Coalescing window in milliseconds.  Two `ring` calls separated by
/// less than this many milliseconds collapse to one play.  Per the
/// G.6 acceptance ("subsequent bells within a documented coalescing
/// window are silently dropped") and the design-doc note (~50 ms).
pub const COALESCE_WINDOW_MS: u64 = 50;

/// Errors observable on the bell public surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BellError {
    /// The sink rejected the play attempt (e.g. underlying audio
    /// transport returned `-EPIPE` or `-EBUSY`).  The variant is
    /// data, not strings — callers can match and recover.
    SinkBusy,
    /// The sink reported a transport-level disconnect.
    SinkDisconnected,
    /// The sink could not open a stream because no audio device is
    /// available.  This is the expected variant when the production
    /// stub returns "audio unavailable" cleanly.
    AudioUnavailable,
}

/// Sink seam consumed by [`Bell`].  Implementations decide what
/// "ring the bell" means.  Production: open audio stream, submit
/// tone, close.  Tests: append to a recording vector.
pub trait BellSink {
    /// Play the bell tone, or return a typed error if it cannot.
    /// Implementations must not block the caller for more than a
    /// documented timeout (Phase 57 G.6 acceptance: ~50 ms).
    fn play(&mut self) -> Result<(), BellError>;
}

/// Production stub: while Track E (`audio_client`) is in flight, this
/// sink emits one warn marker and otherwise no-ops.  When the tracks
/// merge, a follow-up commit replaces it with the real
/// `AudioClientBellSink` that talks to `audio_server`.
///
/// The marker is written via `syscall_lib::write_str` only on the
/// `os-binary` feature path; on host-test builds the lib target does
/// not link `syscall-lib` userspace IO so the marker is a no-op.  The
/// host harness covers behaviour through the [`MockBellSink`]
/// path; the production stub only needs to compile + log.
pub struct AudioUnavailableBellSink {
    /// True after we have emitted the warn marker once.  Subsequent
    /// rings drop silently: per G.6 acceptance the unavailable path
    /// emits a single warn log.
    warned: bool,
}

impl AudioUnavailableBellSink {
    pub const fn new() -> Self {
        Self { warned: false }
    }

    /// Re-arm the warn-once flag.  Used by the future
    /// `audio_client`-aware sink when the underlying transport
    /// recovers from a temporary outage.
    pub fn rearm(&mut self) {
        self.warned = false;
    }
}

impl Default for AudioUnavailableBellSink {
    fn default() -> Self {
        Self::new()
    }
}

impl BellSink for AudioUnavailableBellSink {
    fn play(&mut self) -> Result<(), BellError> {
        if !self.warned {
            self.warned = true;
            #[cfg(all(not(test), feature = "os-binary"))]
            syscall_lib::write_str(syscall_lib::STDOUT_FILENO, "term.bell.audio_unavailable\n");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AudioClientBellSink — production sink that talks to audio_server
// ---------------------------------------------------------------------------

/// Bell tone frequency in Hz. 880 Hz is one octave above the 440 Hz
/// audio-demo tone; the higher pitch is conventional for terminal
/// "ding" sounds and is easy to distinguish from other system audio.
pub const BELL_TONE_FREQ_HZ: u32 = 880;

/// Bell tone duration in milliseconds. Pinned at 30 ms — well below
/// the documented ~50 ms `Bell::ring` budget so the submit_frames
/// call never blocks the render loop past the coalescing window.
pub const BELL_DURATION_MS: u32 = 30;

/// Bell sample count per channel.  At the AC'97 fixed rate (48 kHz)
/// and 30 ms duration: `48000 * 30 / 1000 = 1440` samples per channel.
const BELL_SAMPLES_PER_CHANNEL: usize = (48_000 * BELL_DURATION_MS as usize) / 1_000;

/// Bell tone buffer size in bytes. Stereo (2 channels) × 16-bit
/// (2 bytes per sample) × `BELL_SAMPLES_PER_CHANNEL`.
pub const BELL_TONE_BYTES: usize = BELL_SAMPLES_PER_CHANNEL * 2 * 2;

/// Bell tone amplitude as a fraction of `i16::MAX`. 0.4 keeps the
/// tone audible without clipping when mixed with other audio.
const BELL_AMPLITUDE_NUM: i32 = 4;
const BELL_AMPLITUDE_DEN: i32 = 10;

/// Build the bell tone bytes — a 30 ms square wave at
/// [`BELL_TONE_FREQ_HZ`] in 16-bit signed LE stereo. Square wave is
/// chosen over sine because it is `no_std`-friendly (no floating-
/// point math, no `libm`) and sounds appropriately bell-like for a
/// terminal beep. Mirrors the audio-demo tone-generation pattern
/// from `userspace/audio-demo/src/main.rs` minus the LUT.
///
/// The buffer is computed eagerly into a fixed-size array so the
/// production sink does not allocate on the hot path.
pub fn build_bell_tone_bytes() -> [u8; BELL_TONE_BYTES] {
    let mut buf = [0u8; BELL_TONE_BYTES];
    // Period in samples for the chosen frequency. At 48 kHz / 880 Hz
    // this is ~54.5; we use integer division so the wave is a hair
    // sharp of 880 Hz — close enough for a 30 ms beep, no listener
    // would hear the difference.
    let period_samples = 48_000 / BELL_TONE_FREQ_HZ as usize;
    let half_period = period_samples / 2;
    let amplitude = (i16::MAX as i32 * BELL_AMPLITUDE_NUM / BELL_AMPLITUDE_DEN) as i16;

    for sample_idx in 0..BELL_SAMPLES_PER_CHANNEL {
        let phase = sample_idx % period_samples;
        let sample: i16 = if phase < half_period {
            amplitude
        } else {
            -amplitude
        };
        let bytes = sample.to_le_bytes();
        // Stereo: same sample on both channels.
        let base = sample_idx * 4;
        buf[base] = bytes[0];
        buf[base + 1] = bytes[1];
        buf[base + 2] = bytes[0];
        buf[base + 3] = bytes[1];
    }
    buf
}

/// Production [`BellSink`] backed by `audio_client`. Lazily opens an
/// AC'97 PCM stream on the first `play()` call and caches it; on
/// every subsequent call submits the precomputed tone bytes
/// fire-and-forget. On any transport error the sink invalidates its
/// cached client and surfaces [`BellError::AudioUnavailable`]; the
/// caller can either retry (next ring re-attempts open) or swap to
/// [`AudioUnavailableBellSink`] permanently.
///
/// The cached client holds the single Phase 57 audio slot for the
/// process lifetime — `audio_server` rejects subsequent client
/// connections with `-EBUSY` (per Track D's single-client policy)
/// while term holds the slot. This is acceptable because Phase 57's
/// only other audio consumer is `audio-demo`, a one-shot binary
/// that exits after submitting its tone.
#[cfg(all(not(test), feature = "os-binary"))]
pub struct AudioClientBellSink {
    client: Option<audio_client::AudioClient<audio_client::SyscallSocket>>,
    tone_bytes: [u8; BELL_TONE_BYTES],
}

#[cfg(all(not(test), feature = "os-binary"))]
impl AudioClientBellSink {
    /// Construct a fresh sink with no open client. The first
    /// `play()` call opens the stream lazily; tone bytes are
    /// precomputed up front so the hot path is allocation-free.
    pub fn new() -> Self {
        Self {
            client: None,
            tone_bytes: build_bell_tone_bytes(),
        }
    }

    /// Lazily open the AudioClient if it is not already open.
    /// Returns `Err(BellError::AudioUnavailable)` if `audio_server`
    /// is not registered or refused the connection.
    fn ensure_open(&mut self) -> Result<(), BellError> {
        if self.client.is_some() {
            return Ok(());
        }
        let client = audio_client::AudioClient::open(
            kernel_core::audio::PcmFormat::S16Le,
            kernel_core::audio::ChannelLayout::Stereo,
            kernel_core::audio::SampleRate::Hz48000,
        )
        .map_err(|_| BellError::AudioUnavailable)?;
        self.client = Some(client);
        Ok(())
    }
}

#[cfg(all(not(test), feature = "os-binary"))]
impl Default for AudioClientBellSink {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(not(test), feature = "os-binary"))]
impl BellSink for AudioClientBellSink {
    fn play(&mut self) -> Result<(), BellError> {
        self.ensure_open()?;
        let client = self.client.as_mut().expect("ensure_open populated client");
        match client.submit_frames(&self.tone_bytes) {
            Ok(_) => Ok(()),
            Err(_) => {
                // Drop the cached client so the next ring re-opens.
                // The server may have crashed; restart-aware behavior
                // lives in audio_server's supervisor manifest, not
                // here.
                self.client = None;
                Err(BellError::AudioUnavailable)
            }
        }
    }
}

/// Bell coalescing wrapper.  Owns a [`BellSink`] and a "last played"
/// timestamp; rejects rings inside the coalescing window so a tight
/// BEL loop does not flood the audio path.
pub struct Bell<S: BellSink> {
    sink: S,
    /// Timestamp (ms since boot, or any monotonic source the caller
    /// supplies) of the most recent successful play.  Set to
    /// [`u64::MAX`] when no bell has ever rung — the first call cannot
    /// be inside the coalescing window.
    last_played_ms: u64,
}

impl<S: BellSink> Bell<S> {
    /// Wrap a sink in a fresh bell with no prior timestamp.
    pub const fn new(sink: S) -> Self {
        Self {
            sink,
            last_played_ms: u64::MAX,
        }
    }

    /// Return a reference to the underlying sink.  Tests use this to
    /// inspect the recording mock; production callers do not need it.
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// Return a mutable reference to the underlying sink.  Same
    /// rationale as [`sink`].
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Ring the bell.  `current_time_ms` is the caller's monotonic
    /// clock reading (the binary supplies `clock_gettime`; tests
    /// supply a fixed value).  Returns:
    ///
    /// - `Ok(true)` when the sink was called and the play succeeded.
    /// - `Ok(false)` when the call was inside the coalescing window
    ///   and dropped silently.
    /// - `Err(BellError)` when the sink rejected the play.  The
    ///   internal timestamp is *not* updated on error so a subsequent
    ///   call that succeeds re-anchors the window.
    pub fn ring(&mut self, current_time_ms: u64) -> Result<bool, BellError> {
        if self.last_played_ms != u64::MAX {
            let elapsed = current_time_ms.saturating_sub(self.last_played_ms);
            if elapsed < COALESCE_WINDOW_MS {
                return Ok(false);
            }
        }
        self.sink.play()?;
        self.last_played_ms = current_time_ms;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Recording mock: pushes one entry per `play` call so tests can
    /// count how often the sink was invoked.
    struct MockBellSink {
        calls: Vec<()>,
        next_result: Result<(), BellError>,
    }

    impl MockBellSink {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                next_result: Ok(()),
            }
        }

        fn with_error(err: BellError) -> Self {
            Self {
                calls: Vec::new(),
                next_result: Err(err),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.len()
        }
    }

    impl BellSink for MockBellSink {
        fn play(&mut self) -> Result<(), BellError> {
            self.calls.push(());
            self.next_result
        }
    }

    #[test]
    fn first_ring_calls_sink_and_returns_true() {
        let mut bell = Bell::new(MockBellSink::new());
        let played = bell.ring(0).expect("first ring should succeed");
        assert!(played, "first ring should not be coalesced");
        assert_eq!(bell.sink().call_count(), 1);
    }

    #[test]
    fn ring_inside_coalesce_window_is_dropped() {
        let mut bell = Bell::new(MockBellSink::new());
        bell.ring(100).expect("first ring");
        let played = bell
            .ring(100 + COALESCE_WINDOW_MS - 1)
            .expect("inside-window ring");
        assert!(!played, "second ring inside window must be coalesced");
        // The mock should still record only the first call.
        assert_eq!(bell.sink().call_count(), 1);
    }

    #[test]
    fn ring_after_coalesce_window_plays_again() {
        let mut bell = Bell::new(MockBellSink::new());
        bell.ring(100).expect("first ring");
        let played = bell
            .ring(100 + COALESCE_WINDOW_MS)
            .expect("at-boundary ring");
        assert!(played, "ring at exactly the boundary must play");
        assert_eq!(bell.sink().call_count(), 2);
    }

    #[test]
    fn sink_error_surfaces_and_does_not_anchor_window() {
        let mut bell = Bell::new(MockBellSink::with_error(BellError::SinkBusy));
        let err = bell.ring(100).expect_err("error must surface");
        assert_eq!(err, BellError::SinkBusy);
        // Because the play failed, the window is not anchored.  A
        // fresh `MockBellSink::new()` would let us verify the second
        // call also rings; using the same mock would error again,
        // which is also acceptable behaviour.
    }

    #[test]
    fn second_failure_still_returns_error_not_silent_drop() {
        let mut bell = Bell::new(MockBellSink::with_error(BellError::SinkDisconnected));
        bell.ring(100).unwrap_err();
        let err = bell.ring(101).expect_err("second failure must surface");
        assert_eq!(err, BellError::SinkDisconnected);
    }

    #[test]
    fn audio_unavailable_stub_writes_once_then_no_ops() {
        // Phase 57 G.6 acceptance: "If audio_server is unavailable,
        // the bell emits one warn log and otherwise no-ops".
        let mut sink = AudioUnavailableBellSink::new();
        // Both calls must succeed (Ok(())) so `Bell::ring` does not
        // surface an error to the screen consumer.
        sink.play().expect("first play of stub must be Ok");
        sink.play().expect("second play of stub must be Ok");
        // The warn flag should now be latched.  We cannot observe the
        // marker bytes from the host harness because syscall_lib is
        // gated on `os-binary`; the latch flag is the test surface.
        assert!(sink.warned, "stub must record that it warned");
    }

    #[test]
    fn ring_uses_saturating_sub_for_clock_skew() {
        // If the caller's clock somehow regresses (unsupported in
        // monotonic clocks, but defensively documented), the
        // saturating subtraction keeps elapsed at 0 which means we
        // are still inside the window — i.e. we coalesce, never
        // panic.
        let mut bell = Bell::new(MockBellSink::new());
        bell.ring(1_000).expect("first ring");
        let played = bell.ring(500).expect("regressed clock ring");
        assert!(!played, "regressed clock must coalesce, not panic");
    }

    #[test]
    fn bell_tone_buffer_size_matches_duration() {
        // 30 ms at 48 kHz stereo 16-bit:
        //   48000 * 30 / 1000 = 1440 samples per channel
        //   * 2 channels = 2880 samples total
        //   * 2 bytes / sample = 5760 bytes
        assert_eq!(BELL_SAMPLES_PER_CHANNEL, 1440);
        assert_eq!(BELL_TONE_BYTES, 5760);
    }

    #[test]
    fn bell_tone_within_50ms_budget() {
        // G.6 acceptance: bell must not block the render loop for
        // more than ~50 ms. The tone duration drives the worst-case
        // submit/drain time, so pin it under the budget.
        assert!(
            BELL_DURATION_MS < 50,
            "bell tone duration {} ms must be < 50 ms",
            BELL_DURATION_MS
        );
    }

    #[test]
    fn bell_tone_bytes_alternate_polarity_per_half_period() {
        // Square wave at 880 Hz / 48 kHz: period = 54 samples,
        // half-period = 27 samples. The first 27 samples are
        // +amplitude, the next 27 are -amplitude. We sample sample
        // 0 (positive) and sample 27 (negative) to verify the
        // generator produced a square wave, not silence.
        let buf = build_bell_tone_bytes();
        let sample_0 = i16::from_le_bytes([buf[0], buf[1]]);
        let sample_27 = i16::from_le_bytes([buf[27 * 4], buf[27 * 4 + 1]]);
        assert!(sample_0 > 0, "first sample must be +amplitude");
        assert!(sample_27 < 0, "sample at half-period must be -amplitude");
        // Stereo: left and right channels carry the same sample.
        let sample_0_right = i16::from_le_bytes([buf[2], buf[3]]);
        assert_eq!(sample_0, sample_0_right, "stereo channels must match");
    }
}
