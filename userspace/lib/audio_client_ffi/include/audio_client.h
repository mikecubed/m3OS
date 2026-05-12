/*
 * audio_client.h — Phase 63a Track B C-ABI header.
 *
 * Hand-written companion to userspace/lib/audio_client_ffi/src/lib.rs.
 * The crate's build.rs verifies every AUDIO_FFI_* #define against the
 * corresponding `pub const` in src/lib.rs at compile time; a mismatch
 * fails the build with a `panic!()`.
 */
#ifndef AUDIO_CLIENT_H
#define AUDIO_CLIENT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Sentinel — success (0). */
#define AUDIO_FFI_OK 0
/* `AudioClientError::Server(AudioError::Busy)` — single-client policy
 * already has a stream open. Doom's Init maps this to silent-fallback. */
#define AUDIO_FFI_ERR_BUSY -1
/* `AudioClientError::Server(AudioError::WouldBlock)` — DMA ring full,
 * retry later. Distinct so SubmitFrames can drop one tic without
 * confusing it with a fatal error. */
#define AUDIO_FFI_ERR_WOULD_BLOCK -2
/* `AudioClientError::Server(AudioError::InvalidFormat)` — requested
 * PCM format / layout / rate is not supported. */
#define AUDIO_FFI_ERR_FORMAT -3
/* `AudioClientError::Server(AudioError::Internal)`. */
#define AUDIO_FFI_ERR_INTERNAL -4
/* `AudioClientError::Server(AudioError::NoDevice)` — audio_server has
 * not completed device claim. */
#define AUDIO_FFI_ERR_NO_DEVICE -5
/* `AudioClientError::Server(AudioError::BrokenPipe)` — stream
 * disconnected (driver restart). */
#define AUDIO_FFI_ERR_BROKEN_PIPE -6
/* `AudioClientError::Server(AudioError::InvalidArgument)`. */
#define AUDIO_FFI_ERR_INVALID_ARG -7
/* `AudioClientError::Io(_)` — IPC syscall failure. */
#define AUDIO_FFI_ERR_IO -8
/* `AudioClientError::Protocol(_)` — wire-codec rejected a frame. */
#define AUDIO_FFI_ERR_PROTOCOL -9
/* `AudioClientError::AlreadyOpen`. */
#define AUDIO_FFI_ERR_ALREADY_OPEN -10
/* `AudioClientError::NotOpen`. */
#define AUDIO_FFI_ERR_NOT_OPEN -11
/* `AudioClientError::UnexpectedReply`. */
#define AUDIO_FFI_ERR_UNEXPECTED_REPLY -12
/* Null handle passed to an FFI verb. */
#define AUDIO_FFI_ERR_NULL_HANDLE -13
/* Internal panic captured by `catch_unwind`. */
#define AUDIO_FFI_ERR_PANIC -14

/* Opaque handle. */
typedef struct AudioFfiHandle AudioFfiHandle;

/* Stats mirror of `audio_client::AudioStats`. */
typedef struct {
    uint32_t underrun_count;
    uint64_t frames_submitted;
    uint64_t frames_consumed;
} AudioFfiStats;

/* Connect to audio_server's control socket; no Open is issued yet.
 * Returns NULL on connect failure. */
AudioFfiHandle *audio_ffi_connect(void);

/* Open a stream at 48 kHz / S16LE / stereo. Returns 0 on success or
 * a negative AUDIO_FFI_ERR_* code. */
int audio_ffi_open(AudioFfiHandle *handle);

/* Submit PCM bytes into the open stream's ring. Returns the count of
 * bytes accepted (always equals `len` on success — partial accepts
 * surface as AUDIO_FFI_ERR_WOULD_BLOCK), or a negative error. */
ptrdiff_t audio_ffi_submit(AudioFfiHandle *handle, const uint8_t *bytes, size_t len);

/* Block until every submitted frame has been consumed. */
int audio_ffi_drain(AudioFfiHandle *handle);

/* Populate `*out` with the latest stats. Returns 0 on success. */
int audio_ffi_get_stats(AudioFfiHandle *handle, AudioFfiStats *out);

/* Close the stream and release the handle. Idempotent: a second call
 * on the same pointer is a no-op (the handle is consumed). */
void audio_ffi_close(AudioFfiHandle *handle);

#ifdef __cplusplus
}
#endif

#endif /* AUDIO_CLIENT_H */
