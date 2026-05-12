/*
 * m3os_sound.h — Phase 63a Track D: sound_module_t over the m3OS
 * audio stack.
 *
 * Public surface for the patches/i_sound.c overlay and the
 * test_m3os_sound.c host test driver.
 */
#ifndef M3OS_SOUND_H
#define M3OS_SOUND_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Forward decls so we can declare the module table without pulling
 * in the upstream doomgeneric headers from the host-test build. */
#ifndef M3OS_SOUND_HOST_TEST
#include "doomgeneric/i_sound.h"
extern sound_module_t m3os_sound_module;
#endif

/* Stats mirror — populated by submitter.get_stats. Mirrors
 * AudioFfiStats from audio_client.h byte-for-byte (different
 * filename to keep the doom translation units header-independent
 * of the Rust FFI headers). */
typedef struct {
    uint32_t underrun_count;
    uint64_t frames_submitted;
    uint64_t frames_consumed;
} m3os_audio_stats_t;

/* Pluggable audio submitter — production wires these to the
 * `audio_ffi_*` shims in audio_client_ffi; host tests wire them to
 * a recording fake. */
typedef struct {
    void *(*connect)(void);
    int (*open)(void *handle);
    intptr_t (*submit)(void *handle, const uint8_t *bytes, size_t len);
    int (*get_stats)(void *handle, m3os_audio_stats_t *out);
    void (*close)(void *handle);
} m3os_audio_submitter_t;

/* DOOM mixer-channel ranges. SFX claim 0..15 (DOOM's MAX_CHANNELS).
 * Music synth voices (m3os_music.c) claim 16..31. */
#define M3OS_SFX_CHANNEL_COUNT 16
#define M3OS_MUSIC_CHANNEL_BASE 16
#define M3OS_MUSIC_CHANNEL_COUNT 16
#define M3OS_TOTAL_CHANNELS 32

/* PCM transport: fixed 48 kHz S16LE stereo per Phase 63 ABI lock.
 * One tic of mixed audio is `PCM_TIC_FRAMES * 4` bytes. The exact
 * number is sized to cover one DOOM tic (35 Hz → ~1371 frames per
 * tic at 48 kHz), rounded up to a multiple of the BDL slot stride
 * (PCM_SLOT_STRIDE = 512 bytes = 128 frames). */
#define M3OS_PCM_TIC_FRAMES 1408
#define M3OS_PCM_TIC_BYTES (M3OS_PCM_TIC_FRAMES * 4)

/* Forward declaration of the state struct — opaque to the engine. */
typedef struct m3os_sound_state m3os_sound_state_t;

/* Test seam — directly invoke the underlying lifecycle / channel
 * routing without going through `sound_module_t` slots. Production
 * code never calls these. */
m3os_sound_state_t *m3os_sound_state_for_tests(void);
void m3os_sound_inject_submitter(const m3os_audio_submitter_t *s);
int m3os_sound_init_inner(void);
void m3os_sound_shutdown_inner(void);
void m3os_sound_start_decoded(int channel, const uint8_t *samples,
                              uint32_t len, uint32_t rate_hz,
                              int vol, int sep);
void m3os_sound_stop_inner(int channel);
void m3os_sound_update_params_inner(int channel, int vol, int sep);
int m3os_sound_is_playing_inner(int channel);
void m3os_sound_update_inner(void);
int m3os_sound_audio_disabled(void);
size_t m3os_sound_submit_call_count_for_tests(void);

#ifdef __cplusplus
}
#endif

#endif /* M3OS_SOUND_H */
