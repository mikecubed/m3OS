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

    /// Drain the open stream. Phase 57 D.3 contract: `drain` returns
    /// `Ok(())` after the backend records the drain request; the io
    /// loop waits on the IRQ for completion.
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

    /// Apply a backend stats update. The io loop calls this after
    /// every `handle_irq` so the per-stream stats stay in sync with
    /// the device's running counters.
    pub fn record_consumed(&mut self, frames: u64) {
        if let Some(s) = self.open.as_mut() {
            s.stats.frames_consumed = s.stats.frames_consumed.saturating_add(frames);
        }
    }

    /// Bump the underrun counter for the open stream.
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
// Tests — D.3 host coverage (lands red in next commit)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // D.3 lands the failing-test commit. Track D.1 keeps the
    // `#[cfg(test)]` block compiling green so the scaffold ships.
}
