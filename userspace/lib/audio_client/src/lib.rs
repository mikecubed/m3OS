//! `audio_client` — Phase 57 Track E.1 userspace audio client library.
//!
//! Phase 57 E.1 RED commit: this is the test-first scaffold. The
//! verb implementations are intentionally stubbed so the tests fail.
//! E.1 GREEN replaces the stubs with the real codec + IPC wiring.
//!
//! Every userspace consumer of audio (`audio-demo`, `term`'s bell,
//! future media clients) will go through this library. The library
//! consumes [`kernel_core::audio::protocol`] for encode/decode — no
//! parallel protocol byte definitions live in this crate.

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

const MAX_REPLY_BYTES: usize = 64;

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
}

impl From<ProtocolError> for AudioClientError {
    fn from(err: ProtocolError) -> Self {
        AudioClientError::Protocol(err)
    }
}

// ---------------------------------------------------------------------------
// AudioSocket — pluggable IPC transport (private surface)
// ---------------------------------------------------------------------------

pub(crate) trait AudioSocket {
    fn call(
        &mut self,
        frame: &[u8],
        bulk: &[u8],
    ) -> Result<heapless_buf::ReplyBuf, AudioClientError>;
}

mod heapless_buf {
    use super::MAX_REPLY_BYTES;

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
// AudioClient — stub surface (E.1 GREEN replaces every body)
// ---------------------------------------------------------------------------

/// Audio client. Stubbed for E.1 RED — every verb returns
/// `AudioClientError::Io(-1)` until the GREEN implementation lands.
pub struct AudioClient<S: AudioSocket> {
    #[allow(dead_code)]
    socket: S,
    stream_id: Option<u32>,
}

impl<S: AudioSocket> AudioClient<S> {
    pub(crate) fn new_with_socket(socket: S) -> Self {
        Self {
            socket,
            stream_id: None,
        }
    }

    pub(crate) fn open_with_socket(
        socket: S,
        _format: PcmFormat,
        _layout: ChannelLayout,
        _rate: SampleRate,
    ) -> Result<Self, AudioClientError> {
        // RED stub: tests assert real protocol behaviour. This forces
        // every "happy path" test to fail until the GREEN commit.
        let _ = socket;
        Err(AudioClientError::Io(-1))
    }

    pub fn submit_frames(&mut self, _bytes: &[u8]) -> Result<usize, AudioClientError> {
        if self.stream_id.is_none() {
            return Err(AudioClientError::NotOpen);
        }
        Err(AudioClientError::Io(-1))
    }

    pub fn drain(&mut self) -> Result<(), AudioClientError> {
        if self.stream_id.is_none() {
            return Err(AudioClientError::NotOpen);
        }
        Err(AudioClientError::Io(-1))
    }

    pub fn close(self) -> Result<(), AudioClientError> {
        if self.stream_id.is_none() {
            return Err(AudioClientError::NotOpen);
        }
        Err(AudioClientError::Io(-1))
    }

    #[cfg(test)]
    pub(crate) fn stream_id(&self) -> Option<u32> {
        self.stream_id
    }
}

// Silence unused warnings for items the GREEN commit will exercise.
#[allow(dead_code)]
fn _silence_unused_imports() {
    let _ = MAX_SUBMIT_BYTES;
    let _ = ProtocolError::Truncated;
    let _ = ServerMessage::DrainAck;
    let _ = ClientMessage::Drain;
    let _: AudioError = AudioError::Busy;
}

#[cfg(test)]
mod tests;
