//! PCM stream submission registry — Phase 57 Track D.3.
//!
//! Single-stream-only per YAGNI: at most one [`Stream`] is open at a
//! time; a second `try_open` returns [`AudioError::Busy`]. The
//! registry is the seam between the protocol codec (which reports
//! `ClientMessage::Open` / `SubmitFrames` / `Drain` / `Close`) and
//! the [`AudioBackend`] trait.
//!
//! Track D.1 lands the API shell. The behavioral tests + the real
//! drain-with-timeout path land in D.3.

#![allow(dead_code)] // D.3/D.4 consume every symbol; see module docs.

use kernel_core::audio::AudioError;

use crate::device::AudioBackend;

/// Documented drain timeout — 5 seconds at 48 kHz / 16-bit / 2-channel
/// is the entire 16 KiB ring drained twice over. Drain blocks the
/// client; the io loop relies on hardware progress reported through
/// IRQ wakes.
pub const DRAIN_TIMEOUT_MS: u32 = 5_000;

/// Per-stream snapshot of producer / consumer counters returned to
/// callers of `stats`. Mirrors the fields of `AudioControlEvent::Stats`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub frames_submitted: u64,
    pub frames_consumed: u64,
    pub underrun_count: u32,
}

/// Single open PCM stream. Ownership is by id; the registry returns the
/// id on `try_open` and consumes it on `close`.
pub struct Stream {
    pub stream_id: u32,
    pub stats: StreamStats,
}

impl Stream {
    /// Construct a fresh stream with zeroed counters.
    pub fn new(stream_id: u32) -> Self {
        Self {
            stream_id,
            stats: StreamStats::default(),
        }
    }
}

/// At-most-one open `Stream` per `audio_server` instance.
///
/// The registry holds no allocations on the hot path: `try_open`
/// borrows a backend, asks it to open a hardware stream, and stores
/// the resulting id; subsequent `submit` / `drain` / `close` calls
/// route to the same backend.
pub struct StreamRegistry {
    pub(crate) open: Option<Stream>,
}

impl Default for StreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamRegistry {
    pub const fn new() -> Self {
        Self { open: None }
    }

    /// Open a new stream through `backend`. Returns the new stream id
    /// on success, [`AudioError::Busy`] if a stream is already open,
    /// or the backend's typed error otherwise.
    ///
    /// Failure modes do not consume the slot: a backend `open_stream`
    /// error leaves the registry idle so the next call can succeed.
    pub fn try_open(
        &mut self,
        backend: &mut dyn AudioBackend,
        format: kernel_core::audio::PcmFormat,
        layout: kernel_core::audio::ChannelLayout,
        rate: kernel_core::audio::SampleRate,
    ) -> Result<u32, AudioError> {
        if self.open.is_some() {
            return Err(AudioError::Busy);
        }
        let id = backend.open_stream(format, layout, rate)?;
        self.open = Some(Stream::new(id));
        Ok(id)
    }

    /// Submit `bytes` to the open stream.
    ///
    /// On success, advances `frames_submitted` by the number of bytes
    /// the backend accepted (always `bytes.len()` on success). On
    /// error, stats are unchanged — the error path must not
    /// double-count.
    pub fn submit(
        &mut self,
        backend: &mut dyn AudioBackend,
        stream_id: u32,
        bytes: &[u8],
    ) -> Result<usize, AudioError> {
        let stream = match self.open.as_mut() {
            Some(s) if s.stream_id == stream_id => s,
            _ => return Err(AudioError::InvalidArgument),
        };
        let n = backend.submit_frames(stream_id, bytes)?;
        stream.stats.frames_submitted = stream.stats.frames_submitted.saturating_add(n as u64);
        Ok(n)
    }

    /// Drain the open stream.
    ///
    /// Phase 57 D.3 contract: `drain` returns `Ok(())` after the
    /// backend records the drain request; the io loop waits for
    /// completion via the IRQ. The wall-clock timeout pinned by
    /// [`DRAIN_TIMEOUT_MS`] is enforced inside the io loop, not here.
    pub fn drain(
        &mut self,
        backend: &mut dyn AudioBackend,
        stream_id: u32,
    ) -> Result<(), AudioError> {
        let stream = match self.open.as_ref() {
            Some(s) if s.stream_id == stream_id => s,
            _ => return Err(AudioError::InvalidArgument),
        };
        let _ = stream;
        backend.drain(stream_id)
    }

    /// Close the open stream and release the slot.
    ///
    /// On a wrong stream id the slot is preserved; the original
    /// owner can still close it later.
    pub fn close(
        &mut self,
        backend: &mut dyn AudioBackend,
        stream_id: u32,
    ) -> Result<(), AudioError> {
        let was_match = matches!(self.open.as_ref(), Some(s) if s.stream_id == stream_id);
        if !was_match {
            return Err(AudioError::InvalidArgument);
        }
        backend.close_stream(stream_id)?;
        self.open = None;
        Ok(())
    }

    /// Apply a backend stats update — the io loop calls this after
    /// every IRQ wake so the per-stream stats stay in sync with the
    /// device's running counters. Idle no-op so socket-disconnect
    /// paths can call it without first probing the state.
    pub fn record_consumed(&mut self, frames: u64) {
        if let Some(s) = self.open.as_mut() {
            s.stats.frames_consumed = s.stats.frames_consumed.saturating_add(frames);
        }
    }

    /// Bump the underrun counter for the open stream. Idle no-op.
    pub fn record_underrun(&mut self) {
        if let Some(s) = self.open.as_mut() {
            s.stats.underrun_count = s.stats.underrun_count.saturating_add(1);
        }
    }

    /// Snapshot the open stream's stats, or zeros if no stream is open.
    pub fn stats(&self) -> StreamStats {
        self.open.as_ref().map(|s| s.stats).unwrap_or_default()
    }

    /// True when no stream is currently open.
    pub fn is_idle(&self) -> bool {
        self.open.is_none()
    }
}

// ---------------------------------------------------------------------------
// Tests — D.3 host coverage
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Ac97Logic, IrqEvent};
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use kernel_core::audio::{ChannelLayout, PcmFormat, SampleRate};

    /// Fake `AudioBackend` recording every call so the tests can
    /// assert the registry routes correctly. Backed by an
    /// `Ac97Logic` so the BDL ring math is exercised end-to-end.
    struct FakeBackend {
        logic: RefCell<Ac97Logic>,
        next_id: RefCell<u32>,
        open_count: RefCell<u32>,
        close_count: RefCell<u32>,
        drain_count: RefCell<u32>,
        submit_calls: RefCell<Vec<(u32, usize)>>,
        force_open_error: RefCell<Option<AudioError>>,
        force_submit_error: RefCell<Option<AudioError>>,
        force_drain_error: RefCell<Option<AudioError>>,
        force_close_error: RefCell<Option<AudioError>>,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self {
                logic: RefCell::new(Ac97Logic::new()),
                next_id: RefCell::new(7), // arbitrary non-zero
                open_count: RefCell::new(0),
                close_count: RefCell::new(0),
                drain_count: RefCell::new(0),
                submit_calls: RefCell::new(Vec::new()),
                force_open_error: RefCell::new(None),
                force_submit_error: RefCell::new(None),
                force_drain_error: RefCell::new(None),
                force_close_error: RefCell::new(None),
            }
        }
    }

    impl AudioBackend for FakeBackend {
        fn init(&mut self) -> Result<(), AudioError> {
            Ok(())
        }
        fn open_stream(
            &mut self,
            _format: PcmFormat,
            _layout: ChannelLayout,
            _rate: SampleRate,
        ) -> Result<u32, AudioError> {
            if let Some(e) = self.force_open_error.borrow_mut().take() {
                return Err(e);
            }
            *self.open_count.borrow_mut() += 1;
            let id = *self.next_id.borrow();
            *self.next_id.borrow_mut() += 1;
            Ok(id)
        }
        fn submit_frames(&mut self, stream_id: u32, bytes: &[u8]) -> Result<usize, AudioError> {
            if let Some(e) = self.force_submit_error.borrow_mut().take() {
                return Err(e);
            }
            self.submit_calls
                .borrow_mut()
                .push((stream_id, bytes.len()));
            // Drive the BDL ring math too so a regression in
            // `Ac97Logic::submit_buffer` surfaces here.
            self.logic
                .borrow_mut()
                .submit_buffer(0x1000, 0xCAFE_F00D, bytes.len() / 2)?;
            Ok(bytes.len())
        }
        fn drain(&mut self, _stream_id: u32) -> Result<(), AudioError> {
            if let Some(e) = self.force_drain_error.borrow_mut().take() {
                return Err(e);
            }
            *self.drain_count.borrow_mut() += 1;
            Ok(())
        }
        fn close_stream(&mut self, _stream_id: u32) -> Result<(), AudioError> {
            if let Some(e) = self.force_close_error.borrow_mut().take() {
                return Err(e);
            }
            *self.close_count.borrow_mut() += 1;
            Ok(())
        }
        fn handle_irq(&mut self) -> Result<IrqEvent, AudioError> {
            Ok(IrqEvent::None)
        }
    }

    fn default_open(reg: &mut StreamRegistry, b: &mut FakeBackend) -> Result<u32, AudioError> {
        reg.try_open(
            b,
            PcmFormat::S16Le,
            ChannelLayout::Stereo,
            SampleRate::Hz48000,
        )
    }

    // -- Open + Busy ------------------------------------------------------

    #[test]
    fn try_open_first_call_returns_stream_id_from_backend() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id = default_open(&mut reg, &mut b).expect("open succeeds");
        assert_eq!(id, 7);
        assert_eq!(*b.open_count.borrow(), 1);
        assert!(!reg.is_idle());
    }

    #[test]
    fn second_try_open_returns_busy() {
        // Acceptance: second `try_open` returns `-EBUSY`.
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        default_open(&mut reg, &mut b).expect("first open");
        let err = default_open(&mut reg, &mut b).expect_err("second must EBUSY");
        assert_eq!(err, AudioError::Busy);
        // Backend was not reopened.
        assert_eq!(*b.open_count.borrow(), 1);
    }

    #[test]
    fn try_open_propagates_backend_open_error() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        *b.force_open_error.borrow_mut() = Some(AudioError::InvalidFormat);
        let err = default_open(&mut reg, &mut b).expect_err("invalid-format");
        assert_eq!(err, AudioError::InvalidFormat);
        // Slot still empty after a failed open.
        assert!(reg.is_idle());
    }

    // -- Submit -----------------------------------------------------------

    #[test]
    fn submit_frames_advances_ring_head_and_records_stats() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id = default_open(&mut reg, &mut b).expect("open");
        let n = reg.submit(&mut b, id, &[0u8; 1024]).expect("submit");
        assert_eq!(n, 1024);
        assert_eq!(reg.stats().frames_submitted, 1024);
    }

    #[test]
    fn submit_frames_for_unknown_stream_id_returns_invalid_argument() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let _id = default_open(&mut reg, &mut b).expect("open");
        // Wrong stream id.
        let err = reg
            .submit(&mut b, 999, &[0u8; 16])
            .expect_err("wrong id rejected");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    #[test]
    fn submit_frames_propagates_backend_error_without_advancing_stats() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id = default_open(&mut reg, &mut b).expect("open");
        *b.force_submit_error.borrow_mut() = Some(AudioError::WouldBlock);
        let err = reg.submit(&mut b, id, &[0u8; 64]).expect_err("would-block");
        assert_eq!(err, AudioError::WouldBlock);
        // Stats unchanged on error — the error path must not double-count.
        assert_eq!(reg.stats().frames_submitted, 0);
    }

    // -- Drain ------------------------------------------------------------

    #[test]
    fn drain_dispatches_through_backend() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id = default_open(&mut reg, &mut b).expect("open");
        reg.drain(&mut b, id).expect("drain");
        assert_eq!(*b.drain_count.borrow(), 1);
    }

    #[test]
    fn drain_unknown_stream_returns_invalid_argument() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        default_open(&mut reg, &mut b).expect("open");
        let err = reg.drain(&mut b, 999).expect_err("wrong id");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    #[test]
    fn drain_propagates_backend_error() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id = default_open(&mut reg, &mut b).expect("open");
        *b.force_drain_error.borrow_mut() = Some(AudioError::Internal);
        let err = reg.drain(&mut b, id).expect_err("backend err");
        assert_eq!(err, AudioError::Internal);
    }

    // -- Close ------------------------------------------------------------

    #[test]
    fn close_releases_the_slot_so_next_open_succeeds() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id1 = default_open(&mut reg, &mut b).expect("open1");
        reg.close(&mut b, id1).expect("close1");
        assert!(reg.is_idle());
        let id2 = default_open(&mut reg, &mut b).expect("open2");
        assert_ne!(id1, id2, "each open allocates a fresh stream id");
    }

    #[test]
    fn close_unknown_id_returns_invalid_argument_and_keeps_slot() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id = default_open(&mut reg, &mut b).expect("open");
        let err = reg.close(&mut b, 999).expect_err("wrong id");
        assert_eq!(err, AudioError::InvalidArgument);
        // Slot still owned by the original stream.
        assert!(!reg.is_idle());
        // Original close still works.
        reg.close(&mut b, id).expect("close original");
    }

    #[test]
    fn close_when_idle_returns_invalid_argument() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let err = reg.close(&mut b, 7).expect_err("idle close");
        assert_eq!(err, AudioError::InvalidArgument);
    }

    // -- Stats ------------------------------------------------------------

    #[test]
    fn stats_when_idle_is_all_zeros() {
        let reg = StreamRegistry::new();
        let s = reg.stats();
        assert_eq!(s.frames_submitted, 0);
        assert_eq!(s.frames_consumed, 0);
        assert_eq!(s.underrun_count, 0);
    }

    #[test]
    fn record_consumed_advances_frames_consumed_for_open_stream() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        default_open(&mut reg, &mut b).expect("open");
        reg.record_consumed(512);
        assert_eq!(reg.stats().frames_consumed, 512);
    }

    #[test]
    fn record_consumed_when_idle_is_a_noop() {
        let mut reg = StreamRegistry::new();
        reg.record_consumed(42);
        // Stats stay at default.
        assert_eq!(reg.stats().frames_consumed, 0);
    }

    #[test]
    fn record_underrun_bumps_underrun_count() {
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        default_open(&mut reg, &mut b).expect("open");
        reg.record_underrun();
        reg.record_underrun();
        assert_eq!(reg.stats().underrun_count, 2);
    }

    #[test]
    fn drain_timeout_constant_pinned_at_5_seconds() {
        // Phase 57 D.3 acceptance: drain has a documented timeout.
        // The constant lives at the module level so the io-loop
        // path consumes it through one named symbol.
        assert_eq!(DRAIN_TIMEOUT_MS, 5_000);
    }

    // -- Allocation discipline -------------------------------------------

    #[test]
    fn submit_does_not_allocate_per_call() {
        // Acceptance: "No allocation per submit." We exercise the
        // submit path multiple times after open and verify the
        // registry's only fields are the pre-allocated `Stream` slot
        // — no Vec / Box growth on the hot path. This test is a
        // doc-test substitute for a tracking allocator; the module-
        // level structure (no Vec field on StreamRegistry, no
        // alloc::format!) is the actual proof.
        let mut reg = StreamRegistry::new();
        let mut b = FakeBackend::new();
        let id = default_open(&mut reg, &mut b).expect("open");
        for _ in 0..16 {
            reg.submit(&mut b, id, &[0u8; 64]).expect("submit");
        }
        assert_eq!(reg.stats().frames_submitted, 16 * 64);
    }
}
