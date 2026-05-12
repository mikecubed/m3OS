//! `audio_client_ffi` — Phase 63a Track B: C-ABI veneer over
//! [`audio_client::AudioClient`].
//!
//! The doomgeneric platform layer (`m3os_sound.c`) drives the audio
//! protocol through this crate, which means the protocol is
//! single-sourced in `audio_client` — no parallel byte definitions
//! live on the C side. The C-ABI surface is intentionally small:
//! `connect / open / submit / drain / get_stats / close` plus an
//! opaque handle.
//!
//! Error codes are negative integers from the stable
//! `AUDIO_FFI_ERR_*` table in `include/audio_client.h`. The
//! `audio_client_ffi/build.rs` enforces drift-free header / Rust
//! constants at compile time.

// `no_std` outside of host tests so the staticlib path links into
// the DOOM musl-static binary without pulling in precompiled std
// (panic=unwind metadata would trigger `_dl_find_object` link
// errors against musl). The companion `staticlib_runtime` module
// installs the `#[panic_handler]` + `#[global_allocator]` exactly
// once at the staticlib root — `audio_mixer` is pulled in as a
// transitive rlib dependency so its `audio_mixer_*` C-ABI symbols
// are present in the same .a file.
#![cfg_attr(not(test), no_std)]

#[cfg(all(not(test), target_env = "musl"))]
mod staticlib_runtime;

#[cfg(all(not(test), target_env = "musl"))]
mod mixer_reexport;

extern crate alloc;

use core::ffi::c_int;

use alloc::boxed::Box;
use audio_client::{AudioClient, AudioClientError, AudioStats, SyscallSocket};
use kernel_core::audio::{AudioError, ChannelLayout, PcmFormat, SampleRate};

// ---------------------------------------------------------------------------
// Stable error-code table — mirrored byte-for-byte in
// include/audio_client.h. build.rs asserts the constants are equal.
// ---------------------------------------------------------------------------

pub const AUDIO_FFI_OK: c_int = 0;
pub const AUDIO_FFI_ERR_BUSY: c_int = -1;
pub const AUDIO_FFI_ERR_WOULD_BLOCK: c_int = -2;
pub const AUDIO_FFI_ERR_FORMAT: c_int = -3;
pub const AUDIO_FFI_ERR_INTERNAL: c_int = -4;
pub const AUDIO_FFI_ERR_NO_DEVICE: c_int = -5;
pub const AUDIO_FFI_ERR_BROKEN_PIPE: c_int = -6;
pub const AUDIO_FFI_ERR_INVALID_ARG: c_int = -7;
pub const AUDIO_FFI_ERR_IO: c_int = -8;
pub const AUDIO_FFI_ERR_PROTOCOL: c_int = -9;
pub const AUDIO_FFI_ERR_ALREADY_OPEN: c_int = -10;
pub const AUDIO_FFI_ERR_NOT_OPEN: c_int = -11;
pub const AUDIO_FFI_ERR_UNEXPECTED_REPLY: c_int = -12;
pub const AUDIO_FFI_ERR_NULL_HANDLE: c_int = -13;
pub const AUDIO_FFI_ERR_PANIC: c_int = -14;

// ---------------------------------------------------------------------------
// Error mapping — pure function so the host tests exhaustively verify
// each branch without booting `audio_server`.
// ---------------------------------------------------------------------------

/// Flat-table mapping from [`AudioClientError`] (and its inner
/// [`AudioError`] payload when the outer variant is `Server(_)`) to a
/// stable `c_int` exposed in `audio_client.h`.
pub fn map_error(err: AudioClientError) -> c_int {
    match err {
        AudioClientError::Server(inner) => match inner {
            AudioError::Busy => AUDIO_FFI_ERR_BUSY,
            AudioError::WouldBlock => AUDIO_FFI_ERR_WOULD_BLOCK,
            AudioError::InvalidFormat => AUDIO_FFI_ERR_FORMAT,
            AudioError::Internal => AUDIO_FFI_ERR_INTERNAL,
            AudioError::NoDevice => AUDIO_FFI_ERR_NO_DEVICE,
            AudioError::BrokenPipe => AUDIO_FFI_ERR_BROKEN_PIPE,
            AudioError::InvalidArgument => AUDIO_FFI_ERR_INVALID_ARG,
            // `AudioError` is `#[non_exhaustive]` — future variants
            // fall through to `Internal` until the table is updated.
            _ => AUDIO_FFI_ERR_INTERNAL,
        },
        AudioClientError::Io(_) => AUDIO_FFI_ERR_IO,
        AudioClientError::Protocol(_) => AUDIO_FFI_ERR_PROTOCOL,
        AudioClientError::AlreadyOpen => AUDIO_FFI_ERR_ALREADY_OPEN,
        AudioClientError::NotOpen => AUDIO_FFI_ERR_NOT_OPEN,
        AudioClientError::UnexpectedReply => AUDIO_FFI_ERR_UNEXPECTED_REPLY,
        // `AudioClientError` is `#[non_exhaustive]` — future variants
        // map to `Internal` until the table is updated.
        _ => AUDIO_FFI_ERR_INTERNAL,
    }
}

// ---------------------------------------------------------------------------
// AudioOps — the operations the FFI shims need. Implemented by the
// production holder (boxing an `AudioClient<SyscallSocket>`) and, in
// tests, by a fake driven through `audio_client::test_support`.
// ---------------------------------------------------------------------------

/// Operations the FFI shims require. Exposed `pub` so the test
/// module can implement it against a fake; production callers stay
/// on the `audio_ffi_*` extern "C" surface.
pub trait AudioOps {
    fn submit_frames(&mut self, bytes: &[u8]) -> Result<usize, AudioClientError>;
    fn drain(&mut self) -> Result<(), AudioClientError>;
    fn get_stats(&mut self) -> Result<AudioStats, AudioClientError>;
    fn close_inplace(&mut self) -> Result<(), AudioClientError>;
}

/// Production holder. Wraps an `AudioClient<SyscallSocket>` in an
/// `Option` so [`close_inplace`] can consume the inner client.
struct ProdHolder {
    inner: Option<AudioClient<SyscallSocket>>,
}

impl AudioOps for ProdHolder {
    fn submit_frames(&mut self, bytes: &[u8]) -> Result<usize, AudioClientError> {
        self.inner
            .as_mut()
            .ok_or(AudioClientError::NotOpen)?
            .submit_frames(bytes)
    }
    fn drain(&mut self) -> Result<(), AudioClientError> {
        self.inner
            .as_mut()
            .ok_or(AudioClientError::NotOpen)?
            .drain()
    }
    fn get_stats(&mut self) -> Result<AudioStats, AudioClientError> {
        self.inner
            .as_mut()
            .ok_or(AudioClientError::NotOpen)?
            .get_stats()
    }
    fn close_inplace(&mut self) -> Result<(), AudioClientError> {
        match self.inner.take() {
            Some(c) => c.close(),
            None => Err(AudioClientError::NotOpen),
        }
    }
}

/// Opaque handle exposed to C. Wraps a boxed [`AudioOps`] so the FFI
/// can substitute fakes during host tests.
pub struct AudioFfiHandle {
    ops: Box<dyn AudioOps>,
}

impl AudioFfiHandle {
    /// Construct a handle from an arbitrary `AudioOps` implementor.
    pub fn new(ops: Box<dyn AudioOps>) -> Self {
        Self { ops }
    }
}

// ---------------------------------------------------------------------------
// FFI shims
// ---------------------------------------------------------------------------

/// Connect to `audio_server`. The returned handle holds an
/// un-opened control-socket client; the C caller must invoke
/// [`audio_ffi_open`] before submitting frames. Returns NULL on
/// connect failure.
#[unsafe(no_mangle)]
pub extern "C" fn audio_ffi_connect() -> *mut AudioFfiHandle {
    // We hold the control-socket client only long enough to validate
    // the registry lookup; the production `open` path constructs a
    // fresh client via `AudioClient::open(...)` because that path
    // also issues the `Open` request in one shot.
    let _ctrl = match AudioClient::connect() {
        Ok(c) => c,
        Err(_) => return core::ptr::null_mut(),
    };
    let holder = ProdHolder { inner: None };
    let handle = AudioFfiHandle::new(Box::new(holder));
    Box::into_raw(Box::new(handle))
}

/// Open a stream at the locked Phase 63 format (48 kHz / S16LE /
/// stereo). Returns 0 on success or a negative `AUDIO_FFI_ERR_*`
/// constant.
///
/// # Safety
///
/// `handle` must be a pointer previously returned by
/// [`audio_ffi_connect`] and not yet closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_ffi_open(handle: *mut AudioFfiHandle) -> c_int {
    if handle.is_null() {
        return AUDIO_FFI_ERR_NULL_HANDLE;
    }
    // SAFETY: caller upholds validity.
    let h = unsafe { &mut *handle };
    // The handle's existing holder is replaced by a freshly opened
    // production client. Test handles never reach this path —
    // `audio_ffi_open` is the production-entry; tests construct the
    // FFI handle with an already-opened fake.
    match AudioClient::open(PcmFormat::S16Le, ChannelLayout::Stereo, SampleRate::Hz48000) {
        Ok(client) => {
            h.ops = Box::new(ProdHolder {
                inner: Some(client),
            });
            AUDIO_FFI_OK
        }
        Err(e) => map_error(e),
    }
}

/// Submit PCM bytes. Returns the byte count (always equals `len` on
/// success per the all-or-nothing contract) or a negative error.
///
/// # Safety
///
/// `bytes` must point to `len` readable bytes; `handle` must be a
/// valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_ffi_submit(
    handle: *mut AudioFfiHandle,
    bytes: *const u8,
    len: usize,
) -> isize {
    if handle.is_null() {
        return AUDIO_FFI_ERR_NULL_HANDLE as isize;
    }
    if bytes.is_null() && len > 0 {
        return AUDIO_FFI_ERR_INVALID_ARG as isize;
    }
    // SAFETY: caller upholds validity / readability.
    let h = unsafe { &mut *handle };
    let slice = if len == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(bytes, len) }
    };
    match h.ops.submit_frames(slice) {
        Ok(n) => n as isize,
        Err(e) => map_error(e) as isize,
    }
}

/// Block until every submitted frame has been consumed.
///
/// # Safety
///
/// `handle` must be a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_ffi_drain(handle: *mut AudioFfiHandle) -> c_int {
    if handle.is_null() {
        return AUDIO_FFI_ERR_NULL_HANDLE;
    }
    // SAFETY: caller upholds validity.
    let h = unsafe { &mut *handle };
    match h.ops.drain() {
        Ok(()) => AUDIO_FFI_OK,
        Err(e) => map_error(e),
    }
}

/// Populate `*out` with the latest stats.
///
/// # Safety
///
/// `handle` must be a valid pointer; `out` must point to a writable
/// [`AudioFfiStats`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_ffi_get_stats(
    handle: *mut AudioFfiHandle,
    out: *mut AudioFfiStats,
) -> c_int {
    if handle.is_null() {
        return AUDIO_FFI_ERR_NULL_HANDLE;
    }
    if out.is_null() {
        return AUDIO_FFI_ERR_INVALID_ARG;
    }
    // SAFETY: caller upholds validity / writability.
    let h = unsafe { &mut *handle };
    match h.ops.get_stats() {
        Ok(stats) => {
            unsafe {
                core::ptr::write(
                    out,
                    AudioFfiStats {
                        underrun_count: stats.underrun_count,
                        frames_submitted: stats.frames_submitted,
                        frames_consumed: stats.frames_consumed,
                    },
                );
            }
            AUDIO_FFI_OK
        }
        Err(e) => map_error(e),
    }
}

/// Close the stream and free the handle.
///
/// # Safety
///
/// `handle` must be a pointer previously returned by
/// [`audio_ffi_connect`] and not yet closed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn audio_ffi_close(handle: *mut AudioFfiHandle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller upholds the handle came from
    // `audio_ffi_connect` and is unfreed.
    let mut boxed = unsafe { Box::from_raw(handle) };
    let _ = boxed.ops.close_inplace();
}

/// C-callable mirror of `AudioStats` populated by
/// [`audio_ffi_get_stats`].
#[repr(C)]
pub struct AudioFfiStats {
    pub underrun_count: u32,
    pub frames_submitted: u64,
    pub frames_consumed: u64,
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use audio_client::test_support::{FakeAudioSocket, FakeSocketAdapter, ReplyBuf};
    use kernel_core::audio::{AudioControlEvent, ServerMessage};

    /// Replayable fake socket — pops one canned reply (or error) per
    /// `call` and records the encoded request frame plus any bulk.
    struct ScriptedSocket {
        replies: Vec<Result<Vec<u8>, AudioClientError>>,
        calls: Vec<(Vec<u8>, Vec<u8>)>,
    }

    impl ScriptedSocket {
        fn new(replies: Vec<Result<Vec<u8>, AudioClientError>>) -> Self {
            Self {
                replies,
                calls: Vec::new(),
            }
        }
    }

    impl FakeAudioSocket for ScriptedSocket {
        fn call(&mut self, frame: &[u8], bulk: &[u8]) -> Result<ReplyBuf, AudioClientError> {
            self.calls.push((frame.to_vec(), bulk.to_vec()));
            if self.replies.is_empty() {
                return Err(AudioClientError::UnexpectedReply);
            }
            match self.replies.remove(0) {
                Ok(bytes) => Ok(ReplyBuf::from_slice(&bytes)),
                Err(e) => Err(e),
            }
        }
    }

    fn encode_message(msg: ServerMessage) -> Vec<u8> {
        let mut buf = vec![0u8; 64];
        let n = msg.encode(&mut buf).unwrap();
        buf.truncate(n);
        buf
    }

    /// Test holder wrapping `AudioClient<FakeSocketAdapter<ScriptedSocket>>`.
    /// Mirrors `ProdHolder` for the fake socket type.
    struct FakeHolder {
        inner: Option<AudioClient<FakeSocketAdapter<ScriptedSocket>>>,
    }

    impl AudioOps for FakeHolder {
        fn submit_frames(&mut self, bytes: &[u8]) -> Result<usize, AudioClientError> {
            self.inner
                .as_mut()
                .ok_or(AudioClientError::NotOpen)?
                .submit_frames(bytes)
        }
        fn drain(&mut self) -> Result<(), AudioClientError> {
            self.inner
                .as_mut()
                .ok_or(AudioClientError::NotOpen)?
                .drain()
        }
        fn get_stats(&mut self) -> Result<AudioStats, AudioClientError> {
            self.inner
                .as_mut()
                .ok_or(AudioClientError::NotOpen)?
                .get_stats()
        }
        fn close_inplace(&mut self) -> Result<(), AudioClientError> {
            match self.inner.take() {
                Some(c) => c.close(),
                None => Err(AudioClientError::NotOpen),
            }
        }
    }

    /// Build an `AudioFfiHandle` wrapping an already-opened fake.
    /// The first reply in `replies` must satisfy the `Open` call.
    fn handle_with_opened_fake(
        replies: Vec<Result<Vec<u8>, AudioClientError>>,
    ) -> *mut AudioFfiHandle {
        let mut all = vec![Ok(encode_message(ServerMessage::Opened { stream_id: 1 }))];
        all.extend(replies);
        let sock = ScriptedSocket::new(all);
        let client = AudioClient::open_with_fake(
            sock,
            PcmFormat::S16Le,
            ChannelLayout::Stereo,
            SampleRate::Hz48000,
        )
        .expect("open_with_fake");
        let holder = FakeHolder {
            inner: Some(client),
        };
        let handle = AudioFfiHandle::new(Box::new(holder));
        Box::into_raw(Box::new(handle))
    }

    #[test]
    fn map_error_table_covers_every_variant() {
        use kernel_core::audio::ProtocolError;

        assert_eq!(
            map_error(AudioClientError::Server(AudioError::Busy)),
            AUDIO_FFI_ERR_BUSY
        );
        assert_eq!(
            map_error(AudioClientError::Server(AudioError::WouldBlock)),
            AUDIO_FFI_ERR_WOULD_BLOCK
        );
        assert_eq!(
            map_error(AudioClientError::Server(AudioError::InvalidFormat)),
            AUDIO_FFI_ERR_FORMAT
        );
        assert_eq!(
            map_error(AudioClientError::Server(AudioError::Internal)),
            AUDIO_FFI_ERR_INTERNAL
        );
        assert_eq!(
            map_error(AudioClientError::Server(AudioError::NoDevice)),
            AUDIO_FFI_ERR_NO_DEVICE
        );
        assert_eq!(
            map_error(AudioClientError::Server(AudioError::BrokenPipe)),
            AUDIO_FFI_ERR_BROKEN_PIPE
        );
        assert_eq!(
            map_error(AudioClientError::Server(AudioError::InvalidArgument)),
            AUDIO_FFI_ERR_INVALID_ARG
        );
        assert_eq!(map_error(AudioClientError::Io(-22)), AUDIO_FFI_ERR_IO);
        assert_eq!(
            map_error(AudioClientError::Protocol(ProtocolError::Truncated)),
            AUDIO_FFI_ERR_PROTOCOL
        );
        assert_eq!(
            map_error(AudioClientError::AlreadyOpen),
            AUDIO_FFI_ERR_ALREADY_OPEN
        );
        assert_eq!(map_error(AudioClientError::NotOpen), AUDIO_FFI_ERR_NOT_OPEN);
        assert_eq!(
            map_error(AudioClientError::UnexpectedReply),
            AUDIO_FFI_ERR_UNEXPECTED_REPLY
        );
    }

    #[test]
    fn open_close_round_trip() {
        let replies = vec![
            Ok(encode_message(ServerMessage::SubmitAck {
                frames_consumed: 48,
            })),
            Ok(encode_message(ServerMessage::DrainAck)),
            Ok(encode_message(ServerMessage::ControlEvent(
                AudioControlEvent::Stats {
                    underrun_count: 0,
                    frames_submitted: 48,
                    frames_consumed: 48,
                },
            ))),
            Ok(encode_message(ServerMessage::Closed)),
        ];
        let handle = handle_with_opened_fake(replies);

        let payload = [0u8; 4];
        let n = unsafe { audio_ffi_submit(handle, payload.as_ptr(), payload.len()) };
        assert_eq!(n, 4);

        let rc = unsafe { audio_ffi_drain(handle) };
        assert_eq!(rc, AUDIO_FFI_OK);

        let mut stats = AudioFfiStats {
            underrun_count: 99,
            frames_submitted: 99,
            frames_consumed: 99,
        };
        let rc = unsafe { audio_ffi_get_stats(handle, &mut stats as *mut _) };
        assert_eq!(rc, AUDIO_FFI_OK);
        assert_eq!(stats.frames_submitted, 48);
        assert_eq!(stats.frames_consumed, 48);
        assert_eq!(stats.underrun_count, 0);

        unsafe { audio_ffi_close(handle) };
    }

    #[test]
    fn ebusy_maps_to_constant() {
        let replies = vec![Ok(encode_message(ServerMessage::SubmitError(
            AudioError::Busy,
        )))];
        let handle = handle_with_opened_fake(replies);
        let payload = [0u8; 4];
        let n = unsafe { audio_ffi_submit(handle, payload.as_ptr(), payload.len()) };
        assert_eq!(n as c_int, AUDIO_FFI_ERR_BUSY);
        unsafe { audio_ffi_close(handle) };
    }

    #[test]
    fn wouldblock_maps_to_constant() {
        let replies = vec![Ok(encode_message(ServerMessage::SubmitError(
            AudioError::WouldBlock,
        )))];
        let handle = handle_with_opened_fake(replies);
        let payload = [0u8; 4];
        let n = unsafe { audio_ffi_submit(handle, payload.as_ptr(), payload.len()) };
        assert_eq!(n as c_int, AUDIO_FFI_ERR_WOULD_BLOCK);
        assert_ne!(AUDIO_FFI_ERR_WOULD_BLOCK, AUDIO_FFI_ERR_BUSY);
        unsafe { audio_ffi_close(handle) };
    }

    #[test]
    fn submit_all_or_nothing() {
        let replies = vec![Ok(encode_message(ServerMessage::SubmitAck {
            frames_consumed: 128,
        }))];
        let handle = handle_with_opened_fake(replies);
        let payload = [0u8; 1024];
        let n = unsafe { audio_ffi_submit(handle, payload.as_ptr(), payload.len()) };
        assert_eq!(n, 1024);
        unsafe { audio_ffi_close(handle) };
    }

    #[test]
    fn open_error_busy_path() {
        // Verifies the Init silent-fallback path: an OpenError(Busy)
        // server reply translates to AUDIO_FFI_ERR_BUSY via map_error.
        let sock = ScriptedSocket::new(vec![Ok(encode_message(ServerMessage::OpenError(
            AudioError::Busy,
        )))]);
        let res = AudioClient::open_with_fake(
            sock,
            PcmFormat::S16Le,
            ChannelLayout::Stereo,
            SampleRate::Hz48000,
        );
        let err = match res {
            Ok(_) => panic!("open should fail with Busy"),
            Err(e) => e,
        };
        assert_eq!(map_error(err), AUDIO_FFI_ERR_BUSY);
    }

    #[test]
    fn null_handle_rejected() {
        let rc = unsafe { audio_ffi_open(core::ptr::null_mut()) };
        assert_eq!(rc, AUDIO_FFI_ERR_NULL_HANDLE);
        let rc = unsafe { audio_ffi_drain(core::ptr::null_mut()) };
        assert_eq!(rc, AUDIO_FFI_ERR_NULL_HANDLE);
        let n = unsafe { audio_ffi_submit(core::ptr::null_mut(), core::ptr::null(), 0) };
        assert_eq!(n as c_int, AUDIO_FFI_ERR_NULL_HANDLE);
        let rc = unsafe { audio_ffi_get_stats(core::ptr::null_mut(), core::ptr::null_mut()) };
        assert_eq!(rc, AUDIO_FFI_ERR_NULL_HANDLE);
        unsafe { audio_ffi_close(core::ptr::null_mut()) };
    }
}
