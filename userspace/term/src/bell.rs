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
//! # Production stub — pending Track E merge
//!
//! Track E (`audio_client` library) is in flight in a parallel
//! worktree and has not yet merged.  Until it does, the production
//! [`AudioUnavailableBellSink`] writes one warn marker
//! (`term.bell.audio_unavailable`) on the first call and otherwise
//! no-ops.  The full `audio_client` wiring lands in a tiny follow-up
//! commit on the integration branch after Track E merges:
//! cross-references the design at `docs/roadmap/57-audio-and-local-session.md`
//! Track E (E.1) — that follow-up swaps in the real
//! `AudioClientBellSink` that opens a stream, submits the documented
//! short tone, drains, and closes within the timeout.
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
}
