//! `audio_client` — Phase 57 Track E.1 userspace audio client library.
//!
//! Every userspace consumer of audio (`audio-demo`, `term`'s bell,
//! future media clients) goes through this library. The library
//! consumes [`kernel_core::audio::protocol`] for encode / decode — no
//! parallel protocol byte definitions live in this crate.
//!
//! ## Public surface (locked)
//!
//! - [`AudioClient::open`] — connect to `audio_server` and open a
//!   stream.
//! - [`AudioClient::submit_frames`] — write PCM bytes into the open
//!   stream's ring; the bytes ride the same IPC socket as the
//!   encoded `SubmitFrames` frame.
//! - [`AudioClient::drain`] — block until every submitted frame has
//!   been consumed by the device.
//! - [`AudioClient::close`] — close the stream and release the slot.
//! - [`AudioClientError`] — typed error returned by every verb.
//!
//! Anything else is private. The protocol byte format is private to
//! `kernel-core::audio::protocol`.

#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

use kernel_core::audio::{
    AudioError, ChannelLayout, ClientMessage, MAX_SUBMIT_BYTES, PcmFormat, ProtocolError,
    SampleRate, ServerMessage,
};

/// Service name used by `audio_server` to register its command
/// endpoint. Mirrors `audio_server::SERVICE_NAME`.
pub const SERVICE_NAME: &str = "audio.cmd";

/// IPC label used on every client → audio_server call. Carries no
/// semantic load — the wire frame is the message — but the kernel
/// requires a non-zero label to distinguish notification wakes.
const LABEL_AUDIO_CMD: u64 = 0x000A_0D10_C0DE;

/// Maximum encoded reply size. The largest Phase 57 server reply is
/// `ControlEvent(Stats)` at 4 (header) + 4 + 8 + 8 + 4 (inner header) =
/// 28 bytes. 64 is comfortably above that and stays heap-free.
const MAX_REPLY_BYTES: usize = 64;

/// Maximum encoded request frame size. Largest is `SubmitFrames` body
/// (4 bytes) + header (4 bytes) = 8. 16 keeps the buffer aligned and
/// leaves room for future fixed-size fields without protocol drift.
const MAX_REQUEST_BYTES: usize = 16;

// ---------------------------------------------------------------------------
// AudioClientError
// ---------------------------------------------------------------------------

/// Errors returned by [`AudioClient`] verbs. Variants are *data*; the
/// caller pattern-matches.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[non_exhaustive]
pub enum AudioClientError {
    /// IPC syscall failure. The carried `i32` is a stable label.
    Io(i32),
    /// Wire-codec rejected a frame.
    Protocol(ProtocolError),
    /// `audio_server` returned a typed error in a reply.
    Server(AudioError),
    /// `open` was called on a client that already holds a stream.
    AlreadyOpen,
    /// `submit_frames` / `drain` / `close` before any successful `open`.
    NotOpen,
    /// `audio_server` returned an unexpected message kind for the
    /// verb (e.g. `DrainAck` reply to a `SubmitFrames` request).
    UnexpectedReply,
}

impl From<ProtocolError> for AudioClientError {
    fn from(err: ProtocolError) -> Self {
        AudioClientError::Protocol(err)
    }
}

// ---------------------------------------------------------------------------
// AudioSocket — pluggable IPC transport (private surface)
// ---------------------------------------------------------------------------

/// IPC transport used by [`AudioClient`]. Production wiring uses the
/// `SyscallSocket` type defined below; host tests inject a mock.
///
/// `frame` is the encoded control frame. `bulk` is any payload bytes
/// that ride the same call (currently only `SubmitFrames` uses this
/// — the PCM bytes follow the frame on the wire).
pub(crate) trait AudioSocket {
    fn call(&mut self, frame: &[u8], bulk: &[u8])
    -> Result<heapless_buf::ReplyBuf, AudioClientError>;
}

mod heapless_buf {
    use super::MAX_REPLY_BYTES;

    /// Heap-free reply buffer with a baked-in cap. Replaces a `Vec<u8>`
    /// in the trait signature so the library stays `#![no_std]` on the
    /// hot path.
    #[derive(Clone, Copy)]
    pub struct ReplyBuf {
        bytes: [u8; MAX_REPLY_BYTES],
        len: usize,
    }

    impl Default for ReplyBuf {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ReplyBuf {
        pub const fn new() -> Self {
            Self {
                bytes: [0u8; MAX_REPLY_BYTES],
                len: 0,
            }
        }

        pub fn from_slice(src: &[u8]) -> Self {
            let mut buf = Self::new();
            let n = core::cmp::min(src.len(), MAX_REPLY_BYTES);
            buf.bytes[..n].copy_from_slice(&src[..n]);
            buf.len = n;
            buf
        }

        pub fn as_slice(&self) -> &[u8] {
            &self.bytes[..self.len]
        }
    }
}

// ---------------------------------------------------------------------------
// AudioClient
// ---------------------------------------------------------------------------

/// Audio client. Wraps an [`AudioSocket`] transport and tracks the
/// open stream id. The public verbs (`open`, `submit_frames`, `drain`,
/// `close`) are the only stable surface.
///
/// The generic `S: AudioSocket` parameter is intentionally bound to a
/// crate-private trait — production callers always materialise
/// `AudioClient<SyscallSocket>` via [`AudioClient::open`], and tests
/// inject mock sockets through the crate-internal helpers.
#[allow(private_bounds)]
pub struct AudioClient<S: AudioSocket> {
    socket: S,
    stream_id: Option<u32>,
}

#[allow(private_bounds)]
impl<S: AudioSocket> AudioClient<S> {
    /// Constructor for tests — wraps a socket without issuing `Open`.
    #[cfg(test)]
    pub(crate) fn new_with_socket(socket: S) -> Self {
        Self {
            socket,
            stream_id: None,
        }
    }

    /// Connect a pre-built socket and issue the `Open` verb. The
    /// public [`AudioClient::open`] wraps a [`SyscallSocket`] and
    /// delegates here.
    pub(crate) fn open_with_socket(
        mut socket: S,
        format: PcmFormat,
        layout: ChannelLayout,
        rate: SampleRate,
    ) -> Result<Self, AudioClientError> {
        let mut frame = [0u8; MAX_REQUEST_BYTES];
        let n = ClientMessage::Open {
            format,
            layout,
            rate,
        }
        .encode(&mut frame)?;
        let reply = socket.call(&frame[..n], &[])?;
        match decode_server_message(reply.as_slice())? {
            ServerMessage::Opened { stream_id } => Ok(Self {
                socket,
                stream_id: Some(stream_id),
            }),
            ServerMessage::OpenError(err) => Err(AudioClientError::Server(err)),
            _ => Err(AudioClientError::UnexpectedReply),
        }
    }

    /// Submit PCM bytes into the open stream's ring. Returns the
    /// number of bytes the server accepted (always equal to
    /// `bytes.len()` on success — partial accepts are surfaced via
    /// `Server(WouldBlock)` instead).
    ///
    /// Bytes longer than [`MAX_SUBMIT_BYTES`] are rejected with
    /// `Protocol(PayloadTooLarge)` *before* the IPC call — the cap is
    /// part of the audio ABI and the server would reject it anyway.
    pub fn submit_frames(&mut self, bytes: &[u8]) -> Result<usize, AudioClientError> {
        if self.stream_id.is_none() {
            return Err(AudioClientError::NotOpen);
        }
        if bytes.len() > MAX_SUBMIT_BYTES {
            return Err(AudioClientError::Protocol(ProtocolError::PayloadTooLarge));
        }
        // `bytes.len()` fits in `u32` because `MAX_SUBMIT_BYTES`
        // (64 KiB) fits, and the bound above guards the slice length.
        let len = bytes.len() as u32;
        let mut frame = [0u8; MAX_REQUEST_BYTES];
        let n = ClientMessage::SubmitFrames { len }.encode(&mut frame)?;
        let reply = self.socket.call(&frame[..n], bytes)?;
        match decode_server_message(reply.as_slice())? {
            ServerMessage::SubmitAck { .. } => Ok(bytes.len()),
            ServerMessage::SubmitError(err) => Err(AudioClientError::Server(err)),
            _ => Err(AudioClientError::UnexpectedReply),
        }
    }

    /// Block until every submitted frame has been consumed by the
    /// device.
    pub fn drain(&mut self) -> Result<(), AudioClientError> {
        if self.stream_id.is_none() {
            return Err(AudioClientError::NotOpen);
        }
        let mut frame = [0u8; MAX_REQUEST_BYTES];
        let n = ClientMessage::Drain.encode(&mut frame)?;
        let reply = self.socket.call(&frame[..n], &[])?;
        match decode_server_message(reply.as_slice())? {
            ServerMessage::DrainAck => Ok(()),
            ServerMessage::SubmitError(err) => Err(AudioClientError::Server(err)),
            _ => Err(AudioClientError::UnexpectedReply),
        }
    }

    /// Close the stream and release the device for the next opener.
    /// Consumes the client.
    pub fn close(mut self) -> Result<(), AudioClientError> {
        if self.stream_id.is_none() {
            return Err(AudioClientError::NotOpen);
        }
        let mut frame = [0u8; MAX_REQUEST_BYTES];
        let n = ClientMessage::Close.encode(&mut frame)?;
        let reply = self.socket.call(&frame[..n], &[])?;
        match decode_server_message(reply.as_slice())? {
            ServerMessage::Closed => {
                self.stream_id = None;
                Ok(())
            }
            ServerMessage::OpenError(err) => Err(AudioClientError::Server(err)),
            _ => Err(AudioClientError::UnexpectedReply),
        }
    }

    #[cfg(test)]
    pub(crate) fn stream_id(&self) -> Option<u32> {
        self.stream_id
    }
}

// ---------------------------------------------------------------------------
// Public open() — production entry point that wires `SyscallSocket`
// ---------------------------------------------------------------------------

impl AudioClient<SyscallSocket> {
    /// Open an audio stream against the running `audio_server`.
    ///
    /// Looks up `SERVICE_NAME` (`"audio.cmd"`), constructs a
    /// [`SyscallSocket`] holding the resulting endpoint cap handle,
    /// and issues `ClientMessage::Open` with the requested format.
    pub fn open(
        format: PcmFormat,
        layout: ChannelLayout,
        rate: SampleRate,
    ) -> Result<Self, AudioClientError> {
        let socket = SyscallSocket::connect()?;
        Self::open_with_socket(socket, format, layout, rate)
    }
}

// ---------------------------------------------------------------------------
// SyscallSocket — production AudioSocket implementation
// ---------------------------------------------------------------------------

/// Production [`AudioSocket`] backed by `syscall_lib::ipc_call_buf` +
/// `syscall_lib::ipc_take_pending_bulk`.
///
/// The kernel's bulk-data IPC delivers the request frame *and* any
/// PCM bytes in a single call. The server stages a reply bulk via
/// `ipc_store_reply_bulk`; we drain it with `ipc_take_pending_bulk`
/// immediately on return.
pub struct SyscallSocket {
    endpoint: u32,
}

impl SyscallSocket {
    /// Look up `audio.cmd` in the IPC service registry and return a
    /// socket bound to the resulting endpoint cap.
    fn connect() -> Result<Self, AudioClientError> {
        let handle = syscall_lib::ipc_lookup_service(SERVICE_NAME);
        if handle == u64::MAX {
            return Err(AudioClientError::Io(-2)); // ENOENT-shaped
        }
        let ep = u32::try_from(handle).map_err(|_| AudioClientError::Io(-22))?;
        Ok(Self { endpoint: ep })
    }
}

impl AudioSocket for SyscallSocket {
    fn call(
        &mut self,
        frame: &[u8],
        bulk: &[u8],
    ) -> Result<heapless_buf::ReplyBuf, AudioClientError> {
        // `ipc_call_buf` sends `frame ++ bulk` as a single bulk
        // payload. The server's io loop decodes the frame header,
        // reads the trailing PCM bytes from the same buffer, and
        // stages its reply via `ipc_store_reply_bulk`.
        //
        // We concatenate into a stack buffer to avoid an allocation.
        // The cap is `MAX_SUBMIT_BYTES + MAX_REQUEST_BYTES`, so the
        // buffer is small relative to the userspace stack budget but
        // large enough to carry every Phase 57 verb in one call.
        let mut combined = [0u8; MAX_SUBMIT_BYTES + MAX_REQUEST_BYTES];
        let total = frame.len() + bulk.len();
        if total > combined.len() {
            return Err(AudioClientError::Protocol(ProtocolError::PayloadTooLarge));
        }
        combined[..frame.len()].copy_from_slice(frame);
        combined[frame.len()..total].copy_from_slice(bulk);

        let reply_label = syscall_lib::ipc_call_buf(
            self.endpoint,
            LABEL_AUDIO_CMD,
            LABEL_AUDIO_CMD,
            &combined[..total],
        );
        if reply_label == u64::MAX {
            return Err(AudioClientError::Io(-32)); // EPIPE-shaped
        }

        let mut reply = [0u8; MAX_REPLY_BYTES];
        let n = syscall_lib::ipc_take_pending_bulk(&mut reply);
        if n == u64::MAX {
            return Err(AudioClientError::Io(-5)); // EIO-shaped
        }
        let used = n as usize;
        if used > reply.len() {
            return Err(AudioClientError::Protocol(ProtocolError::BodyTooLarge));
        }
        Ok(heapless_buf::ReplyBuf::from_slice(&reply[..used]))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Decode a server reply's bytes into a [`ServerMessage`], lifting
/// any [`ProtocolError`] into [`AudioClientError::Protocol`].
fn decode_server_message(bytes: &[u8]) -> Result<ServerMessage, AudioClientError> {
    let (msg, _consumed) = ServerMessage::decode(bytes)?;
    Ok(msg)
}

#[cfg(test)]
mod tests;
