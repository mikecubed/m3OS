//! `AudioProxyBackend` — Phase 80 Track A.4 / A.6.
//!
//! After Phase 80, `audio_server` owns no hardware: it is a pure policy/mixer
//! server that talks to an out-of-process audio *driver* (`ac97`/`hda`) over
//! the [`kernel_core::driver_ipc::audio`] protocol. This module provides the
//! [`crate::device::AudioBackend`] implementation that forwards every backend
//! call across that seam, so the mixer, the stream registry, the client
//! registry, and the io loop ([`crate::irq::run_io_loop`]) are unchanged —
//! they keep driving a `&mut dyn AudioBackend`, which is now an
//! `AudioProxyBackend` instead of the (removed) in-process `Ac97Backend`.
//!
//! # Transport seam
//!
//! The backend is generic over [`ProxyTransport`] so the request sequencing +
//! `WouldBlock` mapping + reconnect logic are exercised on the host against a
//! mock (`tests::MockTransport`) with no syscalls. The production transport
//! ([`SyscallProxyTransport`], `cfg(not(test))`) wraps
//! [`driver_runtime::ipc::audio::AudioDriverClient`] (the control channel) and
//! [`driver_runtime::audio_pcm::PcmRing`] (the shared PCM ring).
//!
//! # Stream-id ownership (A.6)
//!
//! Stream ids are *driver-allocated*. `audio_server`'s registry stores
//! whatever [`AudioBackend::open_stream`] returns and passes it back on
//! submit/drain/close, so the proxy hands the registry a **stable facing id**
//! ([`FACING_STREAM_ID`]) and maps it to the driver's current id internally.
//! On reconnect the driver assigns a fresh id; the facing id is unchanged, so
//! the registry's in-flight references stay valid.

use kernel_core::audio::{AudioError, ChannelLayout, PcmFormat, SampleRate};
use kernel_core::driver_ipc::audio::{AudioDriverError, AudioRequest, AudioResponse};

use crate::device::{AudioBackend, IrqEvent};

/// The stable stream id the proxy presents to `audio_server`'s registry.
/// `audio_server` is single-stream, so one id suffices; the driver's
/// (possibly changing) id is tracked internally.
pub const FACING_STREAM_ID: u32 = 1;

/// Abstraction over the driver control channel + shared PCM ring, so the
/// proxy is host-testable. `request` performs one control round-trip; `stage`
/// copies PCM into the shared ring and returns the `(shm_id, offset, len)`
/// window to reference in `SubmitFrames`; `reconnect` re-establishes both
/// after a driver restart.
pub trait ProxyTransport {
    /// Send a control request and return the driver's response. Transport
    /// failure (endpoint gone) maps to `Err(AudioError::BrokenPipe)` so the
    /// proxy can trigger a reconnect.
    fn request(&mut self, req: &AudioRequest) -> Result<AudioResponse, AudioError>;

    /// Copy `bytes` into the shared PCM ring; return `(shm_id, offset, len)`.
    fn stage(&mut self, bytes: &[u8]) -> Result<(u32, u32, u32), AudioError>;

    /// Re-discover the driver service and re-establish the ring. Returns
    /// `Err(AudioError::NoDevice)` if no driver is available.
    fn reconnect(&mut self) -> Result<(), AudioError>;
}

/// Map a driver-side protocol error to the `audio_server` `AudioError`.
fn map_driver_error(e: AudioDriverError) -> AudioError {
    match e {
        AudioDriverError::Busy => AudioError::Busy,
        AudioDriverError::NoDevice => AudioError::NoDevice,
        AudioDriverError::InvalidFormat => AudioError::InvalidFormat,
        AudioDriverError::InvalidArgument => AudioError::InvalidArgument,
        AudioDriverError::DeviceAbsent | AudioDriverError::DriverRestarting => {
            AudioError::BrokenPipe
        }
        AudioDriverError::Internal => AudioError::Internal,
        // `#[non_exhaustive]` — unknown future errors surface as Internal.
        _ => AudioError::Internal,
    }
}

/// Forwards [`AudioBackend`] calls to an out-of-process driver over
/// [`ProxyTransport`].
pub struct AudioProxyBackend<T: ProxyTransport> {
    transport: T,
    /// Driver-allocated id of the currently-open stream, if any.
    driver_stream_id: Option<u32>,
    /// PCM shape of the open stream, replayed on reconnect.
    open_params: Option<(PcmFormat, ChannelLayout, SampleRate)>,
    /// Running device-side consumed-frames counter from the last `Ack`.
    last_consumed: u64,
}

impl<T: ProxyTransport> AudioProxyBackend<T> {
    /// Wrap an established transport.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            driver_stream_id: None,
            open_params: None,
            last_consumed: 0,
        }
    }

    /// Re-discover the driver and re-open the previously-open stream (A.6).
    /// Returns `Ok(())` only when the transport reconnected *and* (if a stream
    /// was open) it was re-opened.
    fn reconnect_and_reopen(&mut self) -> Result<(), AudioError> {
        self.transport.reconnect()?;
        if let Some((format, layout, rate)) = self.open_params {
            let id = self.open_stream_on_driver(format, layout, rate)?;
            self.driver_stream_id = Some(id);
        }
        Ok(())
    }

    /// Issue `OpenStream` and return the driver-allocated id.
    fn open_stream_on_driver(
        &mut self,
        format: PcmFormat,
        layout: ChannelLayout,
        rate: SampleRate,
    ) -> Result<u32, AudioError> {
        let rsp = self.transport.request(&AudioRequest::OpenStream {
            format,
            rate,
            layout,
        })?;
        match rsp {
            AudioResponse::StreamOpened(id) => Ok(id),
            AudioResponse::Err(e) => Err(map_driver_error(e)),
            _ => Err(AudioError::Internal),
        }
    }
}

impl<T: ProxyTransport> AudioBackend for AudioProxyBackend<T> {
    fn init(&mut self) -> Result<(), AudioError> {
        // The transport is already connected at construction; a QueryCaps
        // round-trip confirms the driver is live and speaks the protocol.
        match self.transport.request(&AudioRequest::QueryCaps)? {
            AudioResponse::Caps(_) => Ok(()),
            AudioResponse::Err(e) => Err(map_driver_error(e)),
            _ => Err(AudioError::Internal),
        }
    }

    fn open_stream(
        &mut self,
        format: PcmFormat,
        layout: ChannelLayout,
        rate: SampleRate,
    ) -> Result<u32, AudioError> {
        let id = self.open_stream_on_driver(format, layout, rate)?;
        self.driver_stream_id = Some(id);
        self.open_params = Some((format, layout, rate));
        Ok(FACING_STREAM_ID)
    }

    fn submit_frames(&mut self, stream_id: u32, bytes: &[u8]) -> Result<usize, AudioError> {
        if stream_id != FACING_STREAM_ID || self.driver_stream_id.is_none() {
            return Err(AudioError::InvalidArgument);
        }
        // Stage into the shared ring, then reference the window in the request.
        // Note we send the driver's id, not the facing id.
        let submit = |me: &mut Self| -> Result<AudioResponse, AudioError> {
            let (shm_id, offset, len) = me.transport.stage(bytes)?;
            let driver_id = me.driver_stream_id.ok_or(AudioError::InvalidArgument)?;
            me.transport.request(&AudioRequest::SubmitFrames {
                stream_id: driver_id,
                grant_handle: shm_id,
                offset,
                len,
            })
        };

        // A driver restart surfaces two ways: a transport-level failure
        // (`Err(BrokenPipe)` — endpoint gone) or a protocol-level
        // `Err(DriverRestarting)`/`Err(DeviceAbsent)` reply (which
        // `map_driver_error` folds to `BrokenPipe`). Both trigger one
        // reconnect-and-reopen + retry (A.6).
        let needs_reconnect = |r: &Result<AudioResponse, AudioError>| match r {
            Err(AudioError::BrokenPipe) => true,
            Ok(AudioResponse::Err(e)) => matches!(map_driver_error(*e), AudioError::BrokenPipe),
            _ => false,
        };

        let mut result = submit(self);
        if needs_reconnect(&result) {
            self.reconnect_and_reopen()?;
            result = submit(self);
        }
        let rsp = result?;

        match rsp {
            AudioResponse::Ack { frames_consumed } => {
                self.last_consumed = frames_consumed;
                Ok(bytes.len())
            }
            // Preserve the all-or-nothing client contract: ring-full
            // backpressure is surfaced as WouldBlock, never a short write.
            AudioResponse::WouldBlock => Err(AudioError::WouldBlock),
            AudioResponse::Err(e) => Err(map_driver_error(e)),
            _ => Err(AudioError::Internal),
        }
    }

    fn drain(&mut self, stream_id: u32) -> Result<(), AudioError> {
        if stream_id != FACING_STREAM_ID {
            return Err(AudioError::InvalidArgument);
        }
        let driver_id = self.driver_stream_id.ok_or(AudioError::InvalidArgument)?;
        match self.transport.request(&AudioRequest::Drain {
            stream_id: driver_id,
        })? {
            AudioResponse::Ok => Ok(()),
            AudioResponse::Err(e) => Err(map_driver_error(e)),
            _ => Err(AudioError::Internal),
        }
    }

    fn close_stream(&mut self, stream_id: u32) -> Result<(), AudioError> {
        if stream_id != FACING_STREAM_ID {
            return Err(AudioError::InvalidArgument);
        }
        let driver_id = match self.driver_stream_id.take() {
            Some(id) => id,
            None => return Err(AudioError::InvalidArgument),
        };
        self.open_params = None;
        match self.transport.request(&AudioRequest::CloseStream {
            stream_id: driver_id,
        })? {
            AudioResponse::Ok => Ok(()),
            AudioResponse::Err(e) => Err(map_driver_error(e)),
            _ => Err(AudioError::Internal),
        }
    }

    fn handle_irq(&mut self) -> Result<IrqEvent, AudioError> {
        // No hardware IRQ reaches the proxy — completion is observed via the
        // `frames_consumed` field of each SubmitFrames `Ack`. The driver owns
        // the device IRQ and its BDL repost/underrun policy.
        Ok(IrqEvent::None)
    }

    fn poll_frames_consumed(&mut self) -> u64 {
        self.last_consumed
    }
}

// ---------------------------------------------------------------------------
// Production transport — wraps AudioDriverClient + PcmRing
// ---------------------------------------------------------------------------

/// Service name the audio driver registers and the proxy resolves.
pub const DRIVER_SERVICE_NAME: &str = "audio.hw";

#[cfg(not(test))]
pub struct SyscallProxyTransport {
    client: driver_runtime::ipc::audio::AudioDriverClient,
    ring: driver_runtime::audio_pcm::PcmRing,
}

#[cfg(not(test))]
impl SyscallProxyTransport {
    /// Discover the `audio.hw` driver service and create the shared PCM ring.
    /// Returns `Err(AudioError::NoDevice)` if no driver is registered.
    pub fn connect() -> Result<Self, AudioError> {
        let ep = syscall_lib::ipc_lookup_service(DRIVER_SERVICE_NAME);
        if ep == 0 || ep == u64::MAX || ep > u64::from(u32::MAX) {
            return Err(AudioError::NoDevice);
        }
        let client = driver_runtime::ipc::audio::AudioDriverClient::new(ep as u32);
        let ring = driver_runtime::audio_pcm::PcmRing::create().map_err(|_| AudioError::Internal)?;
        Ok(Self { client, ring })
    }
}

#[cfg(not(test))]
impl ProxyTransport for SyscallProxyTransport {
    fn request(&mut self, req: &AudioRequest) -> Result<AudioResponse, AudioError> {
        use driver_runtime::ipc::audio::AudioIpcError;
        match self.client.request(req) {
            Ok(rsp) => Ok(rsp),
            // Endpoint gone / call failed → reconnect trigger.
            Err(AudioIpcError::CallFailed) | Err(AudioIpcError::ReplyFailed) => {
                Err(AudioError::BrokenPipe)
            }
            Err(_) => Err(AudioError::Internal),
        }
    }

    fn stage(&mut self, bytes: &[u8]) -> Result<(u32, u32, u32), AudioError> {
        let shm_id = self.ring.shm_id();
        let (offset, len) = self.ring.stage(bytes).map_err(|_| AudioError::Internal)?;
        Ok((shm_id, offset, len))
    }

    fn reconnect(&mut self) -> Result<(), AudioError> {
        // Re-resolve the (possibly restarted) driver. Reuse the existing ring
        // — its shm region is independent of the driver's lifetime and the
        // driver re-maps it lazily on the next SubmitFrames.
        let ep = syscall_lib::ipc_lookup_service(DRIVER_SERVICE_NAME);
        if ep == 0 || ep == u64::MAX || ep > u64::from(u32::MAX) {
            return Err(AudioError::NoDevice);
        }
        self.client = driver_runtime::ipc::audio::AudioDriverClient::new(ep as u32);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Host tests — mock transport, no syscalls
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec::Vec;

    /// Records every emitted request and returns queued responses. `stage`
    /// returns a deterministic window so the SubmitFrames request shape is
    /// assertable.
    struct MockTransport {
        requests: Vec<AudioRequest>,
        responses: VecDeque<AudioResponse>,
        reconnects: u32,
        shm_id: u32,
    }

    impl MockTransport {
        fn new(responses: Vec<AudioResponse>) -> Self {
            Self {
                requests: Vec::new(),
                responses: responses.into_iter().collect(),
                reconnects: 0,
                shm_id: 0xABCD,
            }
        }
    }

    impl ProxyTransport for MockTransport {
        fn request(&mut self, req: &AudioRequest) -> Result<AudioResponse, AudioError> {
            self.requests.push(*req);
            Ok(self.responses.pop_front().expect("a queued response"))
        }
        fn stage(&mut self, bytes: &[u8]) -> Result<(u32, u32, u32), AudioError> {
            Ok((self.shm_id, 0, bytes.len() as u32))
        }
        fn reconnect(&mut self) -> Result<(), AudioError> {
            self.reconnects += 1;
            Ok(())
        }
    }

    #[test]
    fn open_submit_wouldblock_then_ack_drain_close_sequence() {
        // Driver responses, in order: open→StreamOpened(7), submit→WouldBlock,
        // submit→Ack{42}, drain→Ok, close→Ok.
        let mut backend = AudioProxyBackend::new(MockTransport::new(alloc::vec![
            AudioResponse::StreamOpened(7),
            AudioResponse::WouldBlock,
            AudioResponse::Ack { frames_consumed: 42 },
            AudioResponse::Ok,
            AudioResponse::Ok,
        ]));

        // open → facing id is stable (1), driver id (7) tracked internally.
        let fid = backend
            .open_stream(PcmFormat::S16Le, ChannelLayout::Stereo, SampleRate::Hz48000)
            .unwrap();
        assert_eq!(fid, FACING_STREAM_ID);

        // first submit → WouldBlock maps to AudioError::WouldBlock.
        let pcm = [0u8; 64];
        assert_eq!(
            backend.submit_frames(fid, &pcm),
            Err(AudioError::WouldBlock)
        );

        // second submit → Ack; returns full byte count + records consumed.
        assert_eq!(backend.submit_frames(fid, &pcm), Ok(pcm.len()));
        assert_eq!(backend.poll_frames_consumed(), 42);

        backend.drain(fid).unwrap();
        backend.close_stream(fid).unwrap();

        // Assert the exact emitted AudioRequest sequence, including that
        // SubmitFrames carries the driver-allocated id (7), the shm handle,
        // and the staged window — never inline samples.
        let reqs = &backend.transport.requests;
        assert_eq!(
            reqs[0],
            AudioRequest::OpenStream {
                format: PcmFormat::S16Le,
                rate: SampleRate::Hz48000,
                layout: ChannelLayout::Stereo,
            }
        );
        assert_eq!(
            reqs[1],
            AudioRequest::SubmitFrames {
                stream_id: 7,
                grant_handle: 0xABCD,
                offset: 0,
                len: 64,
            }
        );
        assert_eq!(reqs[2], reqs[1]); // retry of the same window after WouldBlock
        assert_eq!(reqs[3], AudioRequest::Drain { stream_id: 7 });
        assert_eq!(reqs[4], AudioRequest::CloseStream { stream_id: 7 });
        assert_eq!(reqs.len(), 5);
    }

    #[test]
    fn submit_reconnects_and_reopens_on_broken_pipe() {
        // open→StreamOpened(7); submit→Err(DriverRestarting) [BrokenPipe];
        // reconnect re-opens→StreamOpened(9); retried submit→Ack{5}.
        let mut backend = AudioProxyBackend::new(MockTransport::new(alloc::vec![
            AudioResponse::StreamOpened(7),
            AudioResponse::Err(AudioDriverError::DriverRestarting),
            AudioResponse::StreamOpened(9),
            AudioResponse::Ack { frames_consumed: 5 },
        ]));
        let fid = backend
            .open_stream(PcmFormat::S16Le, ChannelLayout::Stereo, SampleRate::Hz48000)
            .unwrap();
        let pcm = [1u8; 32];
        assert_eq!(backend.submit_frames(fid, &pcm), Ok(pcm.len()));
        assert_eq!(backend.transport.reconnects, 1);
        // After reconnect the retried submit must carry the NEW driver id (9).
        let reqs = &backend.transport.requests;
        assert_eq!(
            reqs.last().unwrap(),
            &AudioRequest::SubmitFrames {
                stream_id: 9,
                grant_handle: 0xABCD,
                offset: 0,
                len: 32,
            }
        );
    }

    #[test]
    fn init_queries_caps() {
        let mut backend =
            AudioProxyBackend::new(MockTransport::new(alloc::vec![AudioResponse::Caps(
                kernel_core::driver_ipc::audio::caps_v1()
            )]));
        backend.init().unwrap();
        assert_eq!(backend.transport.requests[0], AudioRequest::QueryCaps);
    }
}
