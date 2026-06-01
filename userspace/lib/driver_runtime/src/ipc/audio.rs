//! Audio-driver IPC glue — Phase 80 Track A.
//!
//! Binds both halves of the `audio_server` ⇄ audio-driver seam to a single
//! wire implementation so the client (`AudioProxyBackend` in `audio_server`)
//! and the servers (`userspace/drivers/{ac97,hda}`) cannot diverge. The
//! authoritative message schema lives once in
//! [`kernel_core::driver_ipc::audio`]; this module only turns it into a
//! synchronous request/response over a Phase 50 endpoint.
//!
//! # Framing
//!
//! - **Request** (client → driver): `ipc_call_buf(ep, AUDIO_REQUEST, 0, bulk)`
//!   where `bulk = encode_request(req)`. Bulk PCM is **not** here — it lives
//!   in the `sys_shm` region named by `SubmitFrames { grant_handle }` (see
//!   [`crate::audio_pcm`]); no IPC cap-slot transfer is needed.
//! - **Reply** (driver → client): the driver stages
//!   `encode_response(rsp)` via `store_reply_bulk` then `reply(AUDIO_RESPONSE,
//!   0)`; the client retrieves it with `ipc_take_pending_bulk` and
//!   `decode_response`.

use kernel_core::driver_ipc::audio::{
    self, AUDIO_REQUEST, AUDIO_REQUEST_MAX_SIZE, AUDIO_RESPONSE, AUDIO_RESPONSE_MAX_SIZE,
    AudioRequest, AudioResponse, DecodeError,
};

/// Errors surfaced by the audio IPC glue (transport-level, distinct from the
/// protocol-level [`kernel_core::driver_ipc::audio::AudioDriverError`] a
/// driver returns inside [`AudioResponse::Err`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum AudioIpcError {
    /// `encode_request` did not fit its buffer (cannot happen for the fixed
    /// variants, but surfaced rather than panicking).
    Encode,
    /// `ipc_call_buf` returned the `u64::MAX` error sentinel — the endpoint
    /// is gone (driver crashed / not yet up). The proxy treats this as a
    /// reconnect trigger.
    CallFailed,
    /// `ipc_take_pending_bulk` failed to retrieve the reply bulk.
    ReplyFailed,
    /// The reply bulk did not decode as an [`AudioResponse`].
    Decode(DecodeError),
}

// ---------------------------------------------------------------------------
// Client side — AudioProxyBackend in audio_server
// ---------------------------------------------------------------------------

/// Synchronous client over a discovered `audio.hw` endpoint. One
/// [`request`](AudioDriverClient::request) is one `call` round-trip.
#[cfg(not(test))]
#[derive(Clone, Copy)]
pub struct AudioDriverClient {
    ep: u32,
}

#[cfg(not(test))]
impl AudioDriverClient {
    /// Wrap a raw endpoint capability handle obtained from
    /// `ipc_lookup_service("audio.hw")`.
    pub const fn new(ep: u32) -> Self {
        Self { ep }
    }

    /// The raw endpoint handle (for liveness checks / re-discovery).
    pub const fn endpoint(&self) -> u32 {
        self.ep
    }

    /// Send `req` and return the driver's decoded [`AudioResponse`].
    pub fn request(&self, req: &AudioRequest) -> Result<AudioResponse, AudioIpcError> {
        let mut buf = [0u8; AUDIO_REQUEST_MAX_SIZE];
        let n = audio::encode_request(req, &mut buf).ok_or(AudioIpcError::Encode)?;
        let reply_label =
            syscall_lib::ipc_call_buf(self.ep, u64::from(AUDIO_REQUEST), 0, &buf[..n]);
        if reply_label == u64::MAX {
            return Err(AudioIpcError::CallFailed);
        }
        let mut reply = [0u8; AUDIO_RESPONSE_MAX_SIZE];
        let m = syscall_lib::ipc_take_pending_bulk(&mut reply);
        if m == u64::MAX {
            return Err(AudioIpcError::ReplyFailed);
        }
        let used = (m as usize).min(reply.len());
        audio::decode_response(&reply[..used]).map_err(AudioIpcError::Decode)
    }
}

// ---------------------------------------------------------------------------
// Server side — ac97 / hda driver processes
// ---------------------------------------------------------------------------

/// Decode a received request frame's bulk into an [`AudioRequest`].
///
/// The driver server loop calls `SyscallBackend::recv_with_capacity(ep,
/// AUDIO_REQUEST_MAX_SIZE)` itself (requests are ≤ 17 bytes), then passes the
/// frame's `bulk` here.
pub fn decode_request_bulk(bulk: &[u8]) -> Result<AudioRequest, DecodeError> {
    audio::decode_request(bulk)
}

/// Stage + send an [`AudioResponse`] as the reply to the in-flight request.
///
/// Mirrors `BlockServer`/`NetServer`: `store_reply_bulk` then `reply` on the
/// kernel-staged reply capability.
#[cfg(not(test))]
pub fn reply_response(
    backend: &mut super::SyscallBackend,
    rsp: &AudioResponse,
) -> Result<(), crate::DriverRuntimeError> {
    use super::IpcBackend;
    let mut buf = [0u8; AUDIO_RESPONSE_MAX_SIZE];
    let n = audio::encode_response(rsp, &mut buf).unwrap_or(0);
    backend.store_reply_bulk(&buf[..n])?;
    backend.reply(u64::from(AUDIO_RESPONSE), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_request_bulk_round_trips_via_a1_codec() {
        // The glue's decode delegates to the A.1 codec; spot-check one verb.
        let mut buf = [0u8; AUDIO_REQUEST_MAX_SIZE];
        let req = AudioRequest::SubmitFrames {
            stream_id: 3,
            grant_handle: 42,
            offset: 0,
            len: 4096,
        };
        let n = audio::encode_request(&req, &mut buf).unwrap();
        assert_eq!(decode_request_bulk(&buf[..n]), Ok(req));
    }
}
