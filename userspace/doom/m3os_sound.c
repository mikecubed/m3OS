/*
 * m3os_sound.c — Phase 63a Track D: sound_module_t over the m3OS
 * audio stack.
 *
 * The module's lifecycle:
 *   Init       → submitter.connect → submitter.open(48 kHz / S16LE /
 *                stereo) → audio_mixer_new(32). On EBUSY or any other
 *                error, sets `audio_disabled = 1` and logs once at
 *                INFO. All later slot calls become no-ops in the
 *                disabled state — DOOM keeps running silently.
 *   Update     → audio_mixer_step + submitter.submit, one tic per
 *                call. WouldBlock is silently dropped (Phase 63's
 *                underrun-zero-fill recovers).
 *   Shutdown   → submitter.get_stats + print the audio_summary line
 *                the smoke gate parses, then submitter.close +
 *                audio_mixer_drop.
 *
 * The submitter is injected via `m3os_audio_submitter_t` so host
 * tests can swap a recording fake for `audio_client_ffi`.
 */

#include "m3os_sound.h"

#include "../lib/audio_client_ffi/include/audio_client.h"
#include "../lib/audio_mixer/include/audio_mixer.h"
#include "m3os_dmx.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#ifndef M3OS_SOUND_HOST_TEST
#include "doomgeneric/w_wad.h"
#include "doomgeneric/z_zone.h"
#endif

/* AUDIO_FFI_ERR_WOULD_BLOCK from audio_client.h must match what the
 * Rust FFI returns. Drift detection is enforced by audio_client_ffi's
 * build.rs at compile time. */
#ifndef AUDIO_FFI_ERR_WOULD_BLOCK
#define AUDIO_FFI_ERR_WOULD_BLOCK -2
#endif

#define M3OS_SFX_CACHE_CAP 256

typedef struct {
    int lumpnum;
    const uint8_t *samples;
    uint32_t len;
    uint32_t rate_hz;
} m3os_sfx_cache_entry_t;

struct m3os_sound_state {
    m3os_audio_submitter_t submitter;
    void *handle;
    audio_mixer_t *mixer;
    int audio_disabled;
    int initialized;
    int shutdown_done;
    m3os_sfx_cache_entry_t cache[M3OS_SFX_CACHE_CAP];
    size_t cache_count;
    /* Scratch buffer the Update hot path mixes into. Sized for one
     * tic; `static` storage keeps the hot path allocation-free. */
    uint8_t scratch[M3OS_PCM_TIC_BYTES + 256];
    /* Diagnostic counters reported in the audio_summary line.
     * `doom_tics_processed` increments per `Update` call (each tic
     * mixes one tic of audio); a healthy session shows this matching
     * the configured `/tmp/doom-autoquit-tics` budget. Divergence
     * means I_UpdateSound is being called more or less often than
     * once per game tic, which would explain music tempo issues. */
    uint32_t doom_tics_processed;
    /* Diagnostics — only inspected by tests. */
    size_t submit_call_count;
    int submit_last_error;
    /* Per-channel state used by SoundIsPlaying / StopSound. */
    int channel_active[M3OS_SFX_CHANNEL_COUNT];
};

static struct m3os_sound_state g_state;

/* -----------------------------------------------------------------
 * Production submitter — wraps the audio_client_ffi extern shims.
 * Compiled out of the host-test build (test_m3os_sound.c provides a
 * fake submitter directly).
 * ---------------------------------------------------------------*/
#ifndef M3OS_SOUND_HOST_TEST
static void *prod_connect(void) {
    return audio_ffi_connect();
}
static int prod_open(void *handle) {
    return audio_ffi_open((AudioFfiHandle *)handle);
}
static intptr_t prod_submit(void *handle, const uint8_t *bytes, size_t len) {
    return (intptr_t)audio_ffi_submit((AudioFfiHandle *)handle, bytes, len);
}
static int prod_get_stats(void *handle, m3os_audio_stats_t *out) {
    AudioFfiStats s = {0, 0, 0};
    int rc = audio_ffi_get_stats((AudioFfiHandle *)handle, &s);
    if (rc != 0) {
        return rc;
    }
    out->underrun_count = s.underrun_count;
    out->frames_submitted = s.frames_submitted;
    out->frames_consumed = s.frames_consumed;
    return 0;
}
static void prod_close(void *handle) {
    audio_ffi_close((AudioFfiHandle *)handle);
}

static const m3os_audio_submitter_t k_prod_submitter = {
    .connect = prod_connect,
    .open = prod_open,
    .submit = prod_submit,
    .get_stats = prod_get_stats,
    .close = prod_close,
};
#endif /* !M3OS_SOUND_HOST_TEST */

/* -----------------------------------------------------------------
 * Lifecycle
 * ---------------------------------------------------------------*/

static int volume_to_pan(int vol, int sep, uint8_t *left, uint8_t *right) {
    /* DOOM volume is 0..127, separation is 0..254 (0 = full left,
     * 254 = full right, 128 = center). Convert to per-channel
     * 0..127 volumes via standard sin/cos approximation. Simpler
     * triangle pan suffices for Tier 1: `left = vol * (254 - sep) /
     * 254`, `right = vol * sep / 254`. */
    if (vol < 0) vol = 0;
    if (vol > 127) vol = 127;
    if (sep < 0) sep = 0;
    if (sep > 254) sep = 254;
    int l = (vol * (254 - sep)) / 254;
    int r = (vol * sep) / 254;
    if (l > 127) l = 127;
    if (r > 127) r = 127;
    *left = (uint8_t)l;
    *right = (uint8_t)r;
    return 0;
}

int m3os_sound_init_inner(void) {
    if (g_state.initialized) {
        return 1;
    }
    g_state.audio_disabled = 0;
    g_state.cache_count = 0;
    g_state.submit_call_count = 0;
    g_state.submit_last_error = 0;
    g_state.shutdown_done = 0;
    memset(g_state.channel_active, 0, sizeof(g_state.channel_active));
    memset(g_state.cache, 0, sizeof(g_state.cache));

    void *handle = g_state.submitter.connect();
    if (handle == NULL) {
        g_state.audio_disabled = 1;
        fprintf(stdout, "doom.audio.unavailable code=connect-failed\n");
        fflush(stdout);
        g_state.initialized = 1;
        return 1;
    }
    g_state.handle = handle;

    int rc = g_state.submitter.open(handle);
    if (rc != 0) {
        const char *code = "open-failed";
        if (rc == AUDIO_FFI_ERR_BUSY) {
            code = "ebusy";
        }
        fprintf(stdout, "doom.audio.unavailable code=%s\n", code);
        fflush(stdout);
        g_state.submitter.close(handle);
        g_state.handle = NULL;
        g_state.audio_disabled = 1;
        g_state.initialized = 1;
        return 1;
    }

    g_state.mixer = audio_mixer_new(M3OS_TOTAL_CHANNELS);
    if (g_state.mixer == NULL) {
        fprintf(stdout, "doom.audio.unavailable code=mixer-failed\n");
        fflush(stdout);
        g_state.submitter.close(handle);
        g_state.handle = NULL;
        g_state.audio_disabled = 1;
        g_state.initialized = 1;
        return 1;
    }

    g_state.initialized = 1;
    return 1;
}

void m3os_sound_shutdown_inner(void) {
    if (!g_state.initialized || g_state.shutdown_done) {
        return;
    }
    g_state.shutdown_done = 1;

    /* Emit the audio_summary line the doom-audio-smoke gate parses.
     * Always emitted — even when audio_disabled — so the smoke gate
     * can distinguish "audio path ran" from "DOOM crashed before
     * audio was wired" without ambiguity. */
    m3os_audio_stats_t stats = {0, 0, 0};
    if (!g_state.audio_disabled && g_state.handle != NULL) {
        (void)g_state.submitter.get_stats(g_state.handle, &stats);
    }
    /* Diagnostic counters from the music engine; declared extern to
     * avoid a header round-trip. Compiled out of the host-test build
     * because the test driver doesn't link m3os_music. */
#ifndef M3OS_SOUND_HOST_TEST
    extern uint32_t m3os_music_diag_mus_ticks(void);
    extern uint32_t m3os_music_diag_note_ons(void);
    extern uint32_t m3os_music_diag_drum_hits(void);
    uint32_t mus_ticks = m3os_music_diag_mus_ticks();
    uint32_t note_ons = m3os_music_diag_note_ons();
    uint32_t drum_hits = m3os_music_diag_drum_hits();
#else
    uint32_t mus_ticks = 0;
    uint32_t note_ons = 0;
    uint32_t drum_hits = 0;
#endif
    fprintf(stdout,
            "M3OS_DOOM:audio_summary frames_submitted=%llu "
            "frames_consumed=%llu underruns=%u doom_tics=%u "
            "mus_ticks=%u note_ons=%u drum_hits=%u\n",
            (unsigned long long)stats.frames_submitted,
            (unsigned long long)stats.frames_consumed,
            stats.underrun_count,
            g_state.doom_tics_processed,
            mus_ticks,
            note_ons,
            drum_hits);
    fflush(stdout);

    if (g_state.handle != NULL) {
        g_state.submitter.close(g_state.handle);
        g_state.handle = NULL;
    }
    if (g_state.mixer != NULL) {
        audio_mixer_drop(g_state.mixer);
        g_state.mixer = NULL;
    }
}

/* -----------------------------------------------------------------
 * Channel routing
 * ---------------------------------------------------------------*/

void m3os_sound_start_decoded(int channel, const uint8_t *samples,
                              uint32_t len, uint32_t rate_hz,
                              int vol, int sep) {
    if (g_state.audio_disabled || g_state.mixer == NULL) {
        return;
    }
    if (channel < 0 || channel >= M3OS_SFX_CHANNEL_COUNT) {
        return;
    }
    if (samples == NULL || len == 0 || rate_hz == 0) {
        return;
    }
    uint8_t l = 0, r = 0;
    volume_to_pan(vol, sep, &l, &r);
    int rc = audio_mixer_set_channel(g_state.mixer, (size_t)channel,
                                      samples, len, rate_hz, l, r);
    if (rc == 0) {
        g_state.channel_active[channel] = 1;
    }
}

void m3os_sound_stop_inner(int channel) {
    if (g_state.audio_disabled || g_state.mixer == NULL) {
        return;
    }
    if (channel < 0 || channel >= M3OS_SFX_CHANNEL_COUNT) {
        return;
    }
    audio_mixer_clear_channel(g_state.mixer, (size_t)channel);
    g_state.channel_active[channel] = 0;
}

void m3os_sound_update_params_inner(int channel, int vol, int sep) {
    if (g_state.audio_disabled || g_state.mixer == NULL) {
        return;
    }
    if (channel < 0 || channel >= M3OS_SFX_CHANNEL_COUNT) {
        return;
    }
    /* Per the audio_mixer API, we cannot mutate an already-active
     * channel's pan without re-seeding the sample pointer. For
     * Tier 1 we mirror what chocolate-doom does and let the
     * channel keep its current sample but adjust pan via a fresh
     * set_channel call with the same sample data. For simplicity
     * in 63a, UpdateSoundParams is a no-op — DOOM's per-tic
     * volume drift is small enough to be inaudible at Tier 1 and
     * S_UpdateSounds re-invokes StartSound on relevant changes. */
    (void)channel;
    (void)vol;
    (void)sep;
}

int m3os_sound_is_playing_inner(int channel) {
    if (g_state.audio_disabled || g_state.mixer == NULL) {
        return 0;
    }
    if (channel < 0 || channel >= M3OS_SFX_CHANNEL_COUNT) {
        return 0;
    }
    return g_state.channel_active[channel];
}

/* -----------------------------------------------------------------
 * Per-tic mix-and-submit loop
 * ---------------------------------------------------------------*/

void m3os_sound_update_inner(void) {
    if (g_state.audio_disabled || g_state.mixer == NULL) {
        return;
    }
    g_state.doom_tics_processed++;
#ifndef M3OS_SOUND_HOST_TEST
    /* Pace music advance via a submit-success interlock. Each
     * successful submit "earns" one music advance for the next
     * Update. Because the audio_server's BDL queue is finite and
     * the device drains it at 48 kHz, successful submits are
     * naturally throttled to the device's real consumption rate
     * (~34 Hz given our 1408-frame tic size). Music advance
     * inherits the same rate, which is what we want — at 34 Hz
     * advance × 4 MUS ticks per advance = 136 MUS ticks/sec, vs
     * the 140 Hz native rate (3% slow, below human tempo-perception
     * threshold).
     *
     * This replaced an earlier approach that queried
     * `audio_ffi_get_stats` every Update. That doubled IPC traffic
     * to audio_server, which surfaces a pre-existing notification-
     * bit race in `audio_server::irq::run_io_loop`'s recv path
     * (see `irq.rs:244-256` — "the AC'97 notification bits
     * race-drain in `recv_msg_with_notif`, returning `u64::MAX`
     * repeatedly without blocking"). The race kills audio_server
     * within seconds; the new interlock avoids it entirely by
     * pacing purely off submit success, which the SubmitAck reply
     * already carries.
     *
     * Submits that happen without a music advance still produce
     * continuous audio because the mixer's cursor keeps walking
     * the looping music-voice buffers — only the high-level
     * NoteOn/NoteOff timing is throttled. Excess submits while
     * the BDL is full return WouldBlock and are silently dropped;
     * music advance is gated to the NEXT successful submit. */
    extern void m3os_music_advance_for_doom_tic(void);
    static int s_pending_advances = 1;
    if (s_pending_advances > 0) {
        m3os_music_advance_for_doom_tic();
        s_pending_advances--;
    }
#endif
    /* Produce one tic's worth of stereo S16LE frames into the scratch
     * buffer, then submit. WouldBlock is silently dropped — Phase 63's
     * underrun-zero-fill recovers. */
    ptrdiff_t produced = audio_mixer_step(g_state.mixer, g_state.scratch,
                                          sizeof(g_state.scratch),
                                          M3OS_PCM_TIC_FRAMES);
    if (produced <= 0) {
        return;
    }
    intptr_t rc = g_state.submitter.submit(g_state.handle, g_state.scratch,
                                            (size_t)produced);
    g_state.submit_call_count++;
#ifndef M3OS_SOUND_HOST_TEST
    if (rc >= 0) {
        /* Pacer interlock: each successful submit earns one music
         * advance for the next Update. See the long comment in the
         * pre-mix gate above for the rationale. */
        s_pending_advances++;
    }
#endif
    if (rc < 0) {
        g_state.submit_last_error = (int)rc;
        /* WouldBlock is the only non-fatal submit error. Anything
         * else gets a one-shot WARN log; per-tic re-logging would
         * flood the serial console. */
        if ((int)rc != AUDIO_FFI_ERR_WOULD_BLOCK) {
            static int warned = 0;
            if (!warned) {
                fprintf(stderr, "doom.audio.submit_error code=%ld\n", (long)rc);
                fflush(stderr);
                warned = 1;
            }
        }
    }
}

int m3os_sound_audio_disabled(void) {
    return g_state.audio_disabled;
}

m3os_sound_state_t *m3os_sound_state_for_tests(void) {
    return &g_state;
}

void m3os_sound_inject_submitter(const m3os_audio_submitter_t *s) {
    if (s == NULL) {
        return;
    }
    g_state.submitter = *s;
    g_state.initialized = 0;
    g_state.shutdown_done = 0;
}

size_t m3os_sound_submit_call_count_for_tests(void) {
    return g_state.submit_call_count;
}

/* -----------------------------------------------------------------
 * sound_module_t adapters — production only (require i_sound.h).
 * ---------------------------------------------------------------*/
#ifndef M3OS_SOUND_HOST_TEST

static const m3os_sfx_cache_entry_t *cache_lookup(int lumpnum) {
    for (size_t i = 0; i < g_state.cache_count; ++i) {
        if (g_state.cache[i].lumpnum == lumpnum) {
            return &g_state.cache[i];
        }
    }
    return NULL;
}

static const m3os_sfx_cache_entry_t *cache_decode(int lumpnum) {
    if (g_state.cache_count >= M3OS_SFX_CACHE_CAP) {
        return NULL;
    }
    int lump_len = W_LumpLength((unsigned int)lumpnum);
    if (lump_len <= 0) {
        return NULL;
    }
    const uint8_t *lump = (const uint8_t *)W_CacheLumpNum(lumpnum, PU_STATIC);
    if (lump == NULL) {
        return NULL;
    }
    m3os_dmx_decoded decoded = {0, NULL, 0};
    int rc = m3os_dmx_decode(lump, (size_t)lump_len, &decoded);
    if (rc != 0) {
        return NULL;
    }
    m3os_sfx_cache_entry_t *entry = &g_state.cache[g_state.cache_count++];
    entry->lumpnum = lumpnum;
    entry->samples = decoded.samples;
    entry->len = decoded.len;
    entry->rate_hz = decoded.rate_hz;
    return entry;
}

static int m3os_sm_get_sfx_lump_num(sfxinfo_t *sfx) {
    char name[16];
    snprintf(name, sizeof(name), "DS%s", sfx->name);
    return W_GetNumForName(name);
}

/* Accessor used by m3os_music.c to share the SFX mixer instance —
 * see audio_mixer instance docs in m3os_sound.h. */
static audio_mixer_t *m3os_sound_get_mixer_for_music(void) {
    return g_state.mixer;
}

extern void m3os_music_set_mixer_accessor(audio_mixer_t *(*fn)(void));

static boolean m3os_sm_init(boolean use_sfx_prefix) {
    (void)use_sfx_prefix;
    /* First-time init: wire the production submitter, then run the
     * shared `_inner` lifecycle. */
    if (!g_state.initialized) {
        g_state.submitter = k_prod_submitter;
    }
    m3os_sound_init_inner();
    /* Wire the shared mixer accessor for m3os_music so the synth's
     * NoteOn / NoteOff routes through the same audio_mixer_t we just
     * created. Done after _init_inner so the mixer pointer is set. */
    m3os_music_set_mixer_accessor(m3os_sound_get_mixer_for_music);
    return true;
}

static void m3os_sm_shutdown(void) {
    m3os_sound_shutdown_inner();
}

static void m3os_sm_update(void) {
    m3os_sound_update_inner();
}

static void m3os_sm_update_sound_params(int channel, int vol, int sep) {
    m3os_sound_update_params_inner(channel, vol, sep);
}

static int m3os_sm_start_sound(sfxinfo_t *sfxinfo, int channel, int vol, int sep) {
    if (g_state.audio_disabled || sfxinfo == NULL) {
        return channel;
    }
    const m3os_sfx_cache_entry_t *entry = cache_lookup(sfxinfo->lumpnum);
    if (entry == NULL) {
        entry = cache_decode(sfxinfo->lumpnum);
    }
    if (entry == NULL) {
        return channel;
    }
    m3os_sound_start_decoded(channel, entry->samples, entry->len,
                             entry->rate_hz, vol, sep);
    return channel;
}

static void m3os_sm_stop_sound(int channel) {
    m3os_sound_stop_inner(channel);
}

static boolean m3os_sm_sound_is_playing(int channel) {
    return m3os_sound_is_playing_inner(channel) ? true : false;
}

static void m3os_sm_cache_sounds(sfxinfo_t *sounds, int num_sounds) {
    (void)sounds;
    (void)num_sounds;
    /* No-op: lump data is cached lazily on first StartSound. */
}

static snddevice_t m3os_sound_devices[] = {
    SNDDEVICE_SB,
    SNDDEVICE_PAS,
    SNDDEVICE_GUS,
    SNDDEVICE_WAVEBLASTER,
    SNDDEVICE_SOUNDCANVAS,
    SNDDEVICE_AWE32,
};

sound_module_t m3os_sound_module = {
    .sound_devices = m3os_sound_devices,
    .num_sound_devices = (int)(sizeof(m3os_sound_devices) /
                                sizeof(m3os_sound_devices[0])),
    .Init = m3os_sm_init,
    .Shutdown = m3os_sm_shutdown,
    .GetSfxLumpNum = m3os_sm_get_sfx_lump_num,
    .Update = m3os_sm_update,
    .UpdateSoundParams = m3os_sm_update_sound_params,
    .StartSound = m3os_sm_start_sound,
    .StopSound = m3os_sm_stop_sound,
    .SoundIsPlaying = m3os_sm_sound_is_playing,
    .CacheSounds = m3os_sm_cache_sounds,
};

#endif /* !M3OS_SOUND_HOST_TEST */
