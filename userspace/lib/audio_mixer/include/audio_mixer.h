/*
 * audio_mixer.h — Phase 63a Track A C-ABI header.
 *
 * Hand-written companion to userspace/lib/audio_mixer/src/ffi.rs.
 * The crate's build.rs verifies every AUDIO_MIXER_* #define against
 * the corresponding `pub const` in src/ffi.rs at compile time; a
 * mismatch fails the build with a `panic!()`.
 */
#ifndef AUDIO_MIXER_H
#define AUDIO_MIXER_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Stable error codes returned by the audio_mixer_* verbs. */
#define AUDIO_MIXER_OK 0
#define AUDIO_MIXER_ERR_INVAL -1
#define AUDIO_MIXER_ERR_EMPTY -2
#define AUDIO_MIXER_ERR_OUTPUT_TOO_SMALL -3
#define AUDIO_MIXER_ERR_NULL_HANDLE -4

/* Opaque mixer handle. */
typedef struct audio_mixer_t audio_mixer_t;

/* Allocate a mixer with `channel_count` slots. Returns NULL on
 * invalid argument (channel_count > 32). */
audio_mixer_t *audio_mixer_new(size_t channel_count);

/* Release a mixer previously returned by audio_mixer_new. */
void audio_mixer_drop(audio_mixer_t *mixer);

/* Seed a channel. `samples` must remain valid for `len` bytes for as
 * long as the channel is active. Volumes are 0..=127. Returns 0 on
 * success or a negative AUDIO_MIXER_ERR_* code. */
int audio_mixer_set_channel(audio_mixer_t *mixer,
                            size_t idx,
                            const uint8_t *samples,
                            size_t len,
                            uint32_t source_rate_hz,
                            uint8_t left_vol,
                            uint8_t right_vol);

/* Zero a channel. */
int audio_mixer_clear_channel(audio_mixer_t *mixer, size_t idx);

/* Mix `frames` stereo S16LE frames into `out` (capacity
 * `byte_capacity`). Returns the number of bytes written, or a
 * negative AUDIO_MIXER_ERR_* code on failure. */
ptrdiff_t audio_mixer_step(audio_mixer_t *mixer,
                           uint8_t *out,
                           size_t byte_capacity,
                           size_t frames);

#ifdef __cplusplus
}
#endif

#endif /* AUDIO_MIXER_H */
