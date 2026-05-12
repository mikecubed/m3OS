/*
 * m3os_music.c — Phase 63a Track E: Tier 2a MUS synth driving the
 * shared audio_mixer instance (channels 16..31).
 *
 * Tier 2a is intentionally crude:
 *   - 16 voices, each a square or triangle wave (per MUS instrument
 *     bucket: 1..63 → square, 64..127 → triangle).
 *   - No envelope, no pitch bend, no controller routing.
 *   - Each note seeds a multi-cycle waveform buffer once on NoteOn;
 *     long notes truncate when the buffer ends (acceptable Tier 2a
 *     trade-off). NoteOff explicitly silences the voice.
 *   - MIDI fallthrough: not implemented in Tier 2a (RegisterSong
 *     returns NULL for non-MUS data; engine silently skips music).
 *
 * The synth feeds the same audio_mixer instance as SFX so there's
 * exactly one mix path and one submit path. m3os_sound.c owns the
 * mixer handle; this module fetches it via the test seam, or via
 * the audio_mixer FFI directly in the production block.
 */

#include "m3os_music.h"

#include "../lib/audio_mixer/include/audio_mixer.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define M3OS_MUS_MAGIC0 'M'
#define M3OS_MUS_MAGIC1 'U'
#define M3OS_MUS_MAGIC2 'S'
#define M3OS_MUS_MAGIC3 0x1A

#define M3OS_VOICES 16
/* One period of the waveform at SOURCE_RATE_HZ. The mixer's
 * resampler walks through this buffer; setting source_rate_hz to
 * `frequency_hz * VOICE_PERIOD_SAMPLES` gives the desired pitch. */
#define VOICE_PERIOD_SAMPLES 128

struct m3os_mus_state {
    const uint8_t *lump;
    size_t lump_len;
    size_t score_offset;
    size_t cursor;
    uint8_t tick_remaining; /* delay ticks pending before next event */
    int finished;           /* ScoreEnd reached */
    m3os_voice_t voices[M3OS_VOICES];
    /* Pre-computed per-voice waveform buffers — populated lazily on
     * first NoteOn for that voice. */
    uint8_t voice_buf[M3OS_VOICES][VOICE_PERIOD_SAMPLES];
    int voice_buf_seeded[M3OS_VOICES];
};

static uint8_t g_master_volume = 100;
/* Mixer accessor — wired by the production module init; tests
 * substitute a stub via `m3os_music_set_mixer_for_tests`. */
static audio_mixer_t *(*g_mixer_accessor)(void) = NULL;

void m3os_music_set_master_volume(uint8_t vol) {
    g_master_volume = (vol > 127) ? 127 : vol;
}
uint8_t m3os_music_get_master_volume(void) {
    return g_master_volume;
}

void m3os_music_set_mixer_accessor(audio_mixer_t *(*fn)(void));

void m3os_music_set_mixer_accessor(audio_mixer_t *(*fn)(void)) {
    g_mixer_accessor = fn;
}

/* MIDI note → frequency (Hz). A4 = 69 = 440 Hz. Returns integer Hz
 * (Tier 2a precision is fine; the resampler interpolates). */
static uint32_t midi_to_freq_hz(int note) {
    /* freq = 440 * 2^((note - 69) / 12). Tier 2a uses a tiny
     * lookup table for the 0..127 range to avoid pulling libm. */
    static const uint32_t k_freq_table[128] = {
        8,    9,    9,    10,   10,   11,   12,   12,   13,   14,
        15,   15,   16,   17,   18,   19,   21,   22,   23,   24,
        26,   28,   29,   31,   33,   35,   37,   39,   41,   44,
        46,   49,   52,   55,   58,   62,   65,   69,   73,   78,
        82,   87,   92,   98,   104,  110,  117,  123,  131,  139,
        147,  156,  165,  175,  185,  196,  208,  220,  233,  247,
        262,  277,  294,  311,  330,  349,  370,  392,  415,  440,
        466,  494,  523,  554,  587,  622,  659,  698,  740,  784,
        831,  880,  932,  988,  1047, 1109, 1175, 1245, 1319, 1397,
        1480, 1568, 1661, 1760, 1865, 1976, 2093, 2217, 2349, 2489,
        2637, 2794, 2960, 3136, 3322, 3520, 3729, 3951, 4186, 4435,
        4699, 4978, 5274, 5588, 5920, 6272, 6645, 7040, 7459, 7902,
        8372, 8870, 9397, 9956, 10548, 11175, 11840, 12544,
    };
    if (note < 0) note = 0;
    if (note > 127) note = 127;
    return k_freq_table[note];
}

/* Seed `buf` with one period of `waveform` in DMX format
 * (unsigned-8 PCM, 128 = silence). */
static void seed_waveform(uint8_t *buf, m3os_waveform_t waveform) {
    switch (waveform) {
    case M3OS_WAVEFORM_SQUARE:
        for (size_t i = 0; i < VOICE_PERIOD_SAMPLES / 2; ++i) {
            buf[i] = 200; /* high */
        }
        for (size_t i = VOICE_PERIOD_SAMPLES / 2; i < VOICE_PERIOD_SAMPLES; ++i) {
            buf[i] = 56; /* low */
        }
        break;
    case M3OS_WAVEFORM_TRIANGLE:
    default: {
        /* Triangle: ramp up, ramp down. Linear over half-periods. */
        for (size_t i = 0; i < VOICE_PERIOD_SAMPLES / 2; ++i) {
            int v = 56 + (int)((i * (200 - 56)) / (VOICE_PERIOD_SAMPLES / 2));
            buf[i] = (uint8_t)v;
        }
        for (size_t i = VOICE_PERIOD_SAMPLES / 2; i < VOICE_PERIOD_SAMPLES; ++i) {
            int j = i - VOICE_PERIOD_SAMPLES / 2;
            int v = 200 - (int)((j * (200 - 56)) / (VOICE_PERIOD_SAMPLES / 2));
            buf[i] = (uint8_t)v;
        }
        break;
    }
    }
}

/* MUS instrument number → waveform bucket. 1..63 square, 64..127
 * triangle. The MUS channel doesn't directly carry instrument until
 * a Controller(0)=patch event, but for Tier 2a we infer the waveform
 * from the channel id itself: even channels square, odd channels
 * triangle — close enough to differentiate melody from harmony. */
static m3os_waveform_t channel_waveform(int channel) {
    return (channel & 1) ? M3OS_WAVEFORM_TRIANGLE : M3OS_WAVEFORM_SQUARE;
}

/* Claim a voice for a NoteOn. Returns -1 if no voice is free.
 * Tier 2a: first inactive voice wins. */
static int claim_voice(m3os_mus_state_t *state, int channel, int note,
                       int velocity) {
    for (int i = 0; i < M3OS_VOICES; ++i) {
        if (!state->voices[i].active) {
            state->voices[i].active = 1;
            state->voices[i].channel = channel;
            state->voices[i].note = note;
            state->voices[i].velocity = velocity;
            state->voices[i].waveform = channel_waveform(channel);
            if (!state->voice_buf_seeded[i]) {
                seed_waveform(state->voice_buf[i], state->voices[i].waveform);
                state->voice_buf_seeded[i] = 1;
            }
            return i;
        }
    }
    return -1;
}

/* Release any voice playing `note` on `channel`. */
static void release_voice(m3os_mus_state_t *state, int channel, int note) {
    for (int i = 0; i < M3OS_VOICES; ++i) {
        if (state->voices[i].active && state->voices[i].channel == channel &&
            state->voices[i].note == note) {
            state->voices[i].active = 0;
            audio_mixer_t *m = g_mixer_accessor ? g_mixer_accessor() : NULL;
            if (m != NULL) {
                audio_mixer_clear_channel(m, (size_t)(M3OS_MUSIC_CHANNEL_BASE + i));
            }
            return;
        }
    }
}

static void seed_voice_in_mixer(m3os_mus_state_t *state, int voice_idx) {
    audio_mixer_t *m = g_mixer_accessor ? g_mixer_accessor() : NULL;
    if (m == NULL) {
        return;
    }
    uint32_t freq = midi_to_freq_hz(state->voices[voice_idx].note);
    uint32_t source_rate = freq * VOICE_PERIOD_SAMPLES;
    /* Master + velocity → per-channel volume. Both scales are
     * 0..127. Triangle pan is unused at Tier 2a (mono music). */
    uint8_t vol = (uint8_t)((g_master_volume * state->voices[voice_idx].velocity) / 127);
    if (vol > 127) vol = 127;
    audio_mixer_set_channel(m, (size_t)(M3OS_MUSIC_CHANNEL_BASE + voice_idx),
                            state->voice_buf[voice_idx],
                            (size_t)VOICE_PERIOD_SAMPLES, source_rate, vol,
                            vol);
}

/* -----------------------------------------------------------------
 * MUS header / event parsing
 * ---------------------------------------------------------------*/

m3os_mus_state_t *m3os_mus_parse_header(const uint8_t *lump, size_t lump_len) {
    if (lump == NULL || lump_len < 16) {
        return NULL;
    }
    if (lump[0] != M3OS_MUS_MAGIC0 || lump[1] != M3OS_MUS_MAGIC1 ||
        lump[2] != M3OS_MUS_MAGIC2 || lump[3] != M3OS_MUS_MAGIC3) {
        return NULL;
    }
    uint16_t score_len = (uint16_t)(lump[4] | (lump[5] << 8));
    uint16_t score_start = (uint16_t)(lump[6] | (lump[7] << 8));
    if ((size_t)score_start + (size_t)score_len > lump_len) {
        return NULL;
    }
    if (score_len == 0) {
        return NULL;
    }
    m3os_mus_state_t *state = (m3os_mus_state_t *)calloc(1, sizeof(*state));
    if (state == NULL) {
        return NULL;
    }
    state->lump = lump;
    state->lump_len = lump_len;
    state->score_offset = score_start;
    state->cursor = score_start;
    state->tick_remaining = 0;
    state->finished = 0;
    return state;
}

void m3os_mus_state_free(m3os_mus_state_t *state) {
    if (state == NULL) return;
    /* Best-effort silence: clear any voices that were sounding. */
    m3os_music_stop_all_inner(state);
    free(state);
}

/* MUS varint decoder — high bit indicates "more bytes follow". */
static uint32_t read_varint(m3os_mus_state_t *state) {
    uint32_t value = 0;
    while (state->cursor < state->lump_len) {
        uint8_t b = state->lump[state->cursor++];
        value = (value << 7) | (b & 0x7F);
        if ((b & 0x80) == 0) {
            return value;
        }
    }
    return value;
}

/* Dispatch a single MUS event. Returns 1 if the event was the
 * last in its group (a delay varint follows), 0 otherwise. */
static int dispatch_event(m3os_mus_state_t *state) {
    if (state->cursor >= state->lump_len) {
        state->finished = 1;
        return 1;
    }
    uint8_t ctrl = state->lump[state->cursor++];
    int last = (ctrl & 0x80) ? 1 : 0;
    int event = (ctrl >> 4) & 0x07;
    int channel = ctrl & 0x0F;

    switch (event) {
    case M3OS_MUS_EVT_RELEASE_NOTE: {
        if (state->cursor >= state->lump_len) {
            state->finished = 1;
            break;
        }
        uint8_t note = state->lump[state->cursor++] & 0x7F;
        release_voice(state, channel, note);
        break;
    }
    case M3OS_MUS_EVT_PLAY_NOTE: {
        if (state->cursor >= state->lump_len) {
            state->finished = 1;
            break;
        }
        uint8_t note_byte = state->lump[state->cursor++];
        int note = note_byte & 0x7F;
        int velocity = -1;
        if (note_byte & 0x80) {
            if (state->cursor >= state->lump_len) {
                state->finished = 1;
                break;
            }
            velocity = state->lump[state->cursor++] & 0x7F;
        }
        if (velocity < 0) velocity = 127; /* sustain previous velocity */
        int v = claim_voice(state, channel, note, velocity);
        if (v >= 0) {
            seed_voice_in_mixer(state, v);
        }
        break;
    }
    case M3OS_MUS_EVT_PITCH_BEND:
    case M3OS_MUS_EVT_CONTROLLER: {
        /* Tier 2a: skip 1 or 2 body bytes. PitchBend = 1 byte;
         * Controller = 2 bytes. Bounds-checked. */
        size_t skip = (event == M3OS_MUS_EVT_CONTROLLER) ? 2 : 1;
        for (size_t i = 0; i < skip; ++i) {
            if (state->cursor < state->lump_len) {
                state->cursor++;
            }
        }
        break;
    }
    case M3OS_MUS_EVT_SYSTEM: {
        /* System events: 1 body byte. */
        if (state->cursor < state->lump_len) {
            state->cursor++;
        }
        break;
    }
    case M3OS_MUS_EVT_END_OF_MEASURE:
        /* No body, no synth effect. */
        break;
    case M3OS_MUS_EVT_SCORE_END:
        state->finished = 1;
        break;
    default:
        break;
    }
    return last;
}

int m3os_music_tick(m3os_mus_state_t *state) {
    if (state == NULL || state->finished) {
        return state ? (state->finished ? 1 : 0) : 0;
    }
    if (state->tick_remaining > 0) {
        state->tick_remaining--;
        return 0;
    }
    /* Dispatch events until we hit the "last in group" flag, then
     * read the delay varint and pause for that many ticks. */
    while (state->cursor < state->lump_len && !state->finished) {
        int last = dispatch_event(state);
        if (last) {
            uint32_t delay = read_varint(state);
            state->tick_remaining = (uint8_t)(delay > 255 ? 255 : delay);
            break;
        }
    }
    return state->finished ? 1 : 0;
}

int m3os_music_active_voice_count(const m3os_mus_state_t *state) {
    if (state == NULL) return 0;
    int n = 0;
    for (int i = 0; i < M3OS_VOICES; ++i) {
        if (state->voices[i].active) ++n;
    }
    return n;
}

void m3os_music_stop_all_inner(m3os_mus_state_t *state) {
    if (state == NULL) return;
    audio_mixer_t *m = g_mixer_accessor ? g_mixer_accessor() : NULL;
    for (int i = 0; i < M3OS_VOICES; ++i) {
        if (state->voices[i].active) {
            state->voices[i].active = 0;
            if (m != NULL) {
                audio_mixer_clear_channel(m, (size_t)(M3OS_MUSIC_CHANNEL_BASE + i));
            }
        }
    }
}

/* -----------------------------------------------------------------
 * music_module_t adapters — production only.
 * ---------------------------------------------------------------*/
#ifndef M3OS_SOUND_HOST_TEST

static m3os_mus_state_t *g_current_song = NULL;
static int g_looping = 0;

static boolean m3os_mm_init(void) {
    return true;
}
static void m3os_mm_shutdown(void) {
    if (g_current_song != NULL) {
        m3os_mus_state_free(g_current_song);
        g_current_song = NULL;
    }
}
static void m3os_mm_set_music_volume(int volume) {
    m3os_music_set_master_volume((uint8_t)(volume < 0 ? 0 : volume));
}
static void m3os_mm_pause_music(void) {
    /* Tier 2a: pause not implemented — voices keep their last state. */
}
static void m3os_mm_resume_music(void) {}
static void *m3os_mm_register_song(void *data, int len) {
    if (data == NULL || len <= 16) return NULL;
    return m3os_mus_parse_header((const uint8_t *)data, (size_t)len);
}
static void m3os_mm_unregister_song(void *handle) {
    m3os_mus_state_free((m3os_mus_state_t *)handle);
}
static void m3os_mm_play_song(void *handle, boolean looping) {
    g_current_song = (m3os_mus_state_t *)handle;
    g_looping = looping ? 1 : 0;
}
static void m3os_mm_stop_song(void) {
    if (g_current_song != NULL) {
        m3os_music_stop_all_inner(g_current_song);
    }
    g_current_song = NULL;
}
static boolean m3os_mm_music_is_playing(void) {
    return (g_current_song != NULL && !g_current_song->finished) ? true : false;
}
static void m3os_mm_poll(void) {
    /* Poll is a no-op — MUS ticks are driven from m3os_sound_update
     * to keep one mix path and one submit cadence. */
}

/* Bridge — m3os_sound_update calls this 4x per DOOM tic to advance
 * the MUS clock to 140 Hz. */
void m3os_music_advance_for_doom_tic(void) {
    if (g_current_song == NULL || g_current_song->finished) {
        if (g_looping && g_current_song != NULL) {
            g_current_song->cursor = g_current_song->score_offset;
            g_current_song->finished = 0;
            g_current_song->tick_remaining = 0;
        }
        return;
    }
    for (int i = 0; i < 4; ++i) {
        if (m3os_music_tick(g_current_song)) {
            if (g_looping) {
                g_current_song->cursor = g_current_song->score_offset;
                g_current_song->finished = 0;
                g_current_song->tick_remaining = 0;
            } else {
                break;
            }
        }
    }
}

static snddevice_t m3os_music_devices[] = {
    SNDDEVICE_SB,
    SNDDEVICE_PAS,
    SNDDEVICE_GUS,
    SNDDEVICE_WAVEBLASTER,
    SNDDEVICE_SOUNDCANVAS,
    SNDDEVICE_AWE32,
    SNDDEVICE_GENMIDI,
};

music_module_t m3os_music_module = {
    .sound_devices = m3os_music_devices,
    .num_sound_devices = (int)(sizeof(m3os_music_devices) /
                                sizeof(m3os_music_devices[0])),
    .Init = m3os_mm_init,
    .Shutdown = m3os_mm_shutdown,
    .SetMusicVolume = m3os_mm_set_music_volume,
    .PauseMusic = m3os_mm_pause_music,
    .ResumeMusic = m3os_mm_resume_music,
    .RegisterSong = m3os_mm_register_song,
    .UnRegisterSong = m3os_mm_unregister_song,
    .PlaySong = m3os_mm_play_song,
    .StopSong = m3os_mm_stop_song,
    .MusicIsPlaying = m3os_mm_music_is_playing,
    .Poll = m3os_mm_poll,
};

#endif /* !M3OS_SOUND_HOST_TEST */
