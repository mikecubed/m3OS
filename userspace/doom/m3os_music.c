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
    /* Delay ticks pending before next event group. Width is
     * `uint32_t` because real songs encode inter-phrase rests of
     * hundreds-to-thousands of ticks; a `uint8_t` here clamped
     * everything above 255 and made the music race through long
     * pauses (perceived as the whole piece being "sped up"). */
    uint32_t tick_remaining;
    int finished;           /* ScoreEnd reached */
    m3os_voice_t voices[M3OS_VOICES];
    /* Pre-computed per-voice waveform buffers — populated lazily on
     * first NoteOn for that voice. */
    uint8_t voice_buf[M3OS_VOICES][VOICE_PERIOD_SAMPLES];
    int voice_buf_seeded[M3OS_VOICES];
    /* Per-voice cooldown counter in DOOM-tic units. After NoteOff
     * we mark the slot unclaimable for a few tics so the mixer's
     * release-fade can run to completion before the slot is
     * recycled. Otherwise a quickly-following NoteOn would replace
     * the in-flight fade and the listener would hear a volume-step
     * click on every voice-reclaim. */
    uint8_t voice_lockout_tics[M3OS_VOICES];
};

/* Lockout duration in DOOM-tic units (1 tic ≈ 29 ms). 2 tics is
 * comfortably longer than the mixer's 192-frame / 4 ms release
 * fade so the channel is truly silent before another note seeds
 * it. With 16 total voices and typical 4–8-voice polyphony, this
 * leaves plenty of slot headroom; only pathological note-dense
 * passages would saturate. */
#define M3OS_VOICE_LOCKOUT_TICS 2

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

/* Per-voice peak amplitude in DMX u8 space. AMP=20 puts each voice
 * at ±5120 in i16 space after the <<8 sign conversion. At the
 * default master/velocity vol of ~78, the per-voice clipping-domain
 * peak is ~3120; 8 simultaneous voices summed at peak land around
 * 24960 — comfortably below i16::MAX = 32767. AMP=32 (the prior
 * value) clipped at 7+ simultaneous voices, producing the
 * harmonic-distortion crackle the listener heard on dense
 * chord-changes; the lower amplitude trades raw loudness for a
 * clean composite signal. The DOOM engine's `S_AdjustSoundParams`
 * + `snd_MusicVolume` give the player further control. */
#define M3OS_MUSIC_AMP 20

/* Seed `buf` with one period of `waveform` in DMX format. Both
 * shapes are constructed so the buffer starts AND ends at silence
 * (sample value 128). This matters for two reasons:
 *
 *   1. NoteOn re-seeds the channel cursor to 0; if buf[0] != 128
 *      the listener hears a click as the mixer transitions from
 *      silence to peak amplitude in one output frame.
 *   2. NoteOff clears the channel; whichever sample the cursor was
 *      on becomes silence. If buf is silence-bordered, the cursor
 *      is statistically more likely to be near silence at NoteOff
 *      time and the resulting transition is softer.
 *
 * For Tier 2a both "square" and "triangle" use the same smooth
 * triangular envelope — the named waveforms are forward-compatible
 * placeholders for Tier 2b's SoundFont voices. */
static void seed_waveform(uint8_t *buf, m3os_waveform_t waveform) {
    (void)waveform; /* Tier 2a: single shape for both buckets */
    const size_t W = VOICE_PERIOD_SAMPLES;
    const size_t Q = W / 4;
    const int amp = M3OS_MUSIC_AMP;
    for (size_t i = 0; i < W; ++i) {
        int v;
        if (i < Q) {
            /* Rising edge: silence → +amp over Q samples. */
            v = 128 + (int)((i * (size_t)amp) / Q);
        } else if (i < 2 * Q) {
            /* Falling edge: +amp → silence over Q samples. */
            v = 128 + amp - (int)(((i - Q) * (size_t)amp) / Q);
        } else if (i < 3 * Q) {
            /* Falling edge below silence: silence → -amp over Q samples. */
            v = 128 - (int)(((i - 2 * Q) * (size_t)amp) / Q);
        } else {
            /* Rising edge back to silence: -amp → silence over Q samples. */
            v = 128 - amp + (int)(((i - 3 * Q) * (size_t)amp) / Q);
        }
        if (v < 0) v = 0;
        if (v > 255) v = 255;
        buf[i] = (uint8_t)v;
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
 * Tier 2a: first inactive AND not-locked-out voice wins. Voices
 * that just released are temporarily skipped so their mixer fade
 * can finish without being interrupted by a re-seed. */
static int claim_voice(m3os_mus_state_t *state, int channel, int note,
                       int velocity) {
    for (int i = 0; i < M3OS_VOICES; ++i) {
        if (!state->voices[i].active && state->voice_lockout_tics[i] == 0) {
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
    /* Fall-back: nothing free outside lockout — accept a still-locked
     * slot rather than drop the note entirely. The audible voice-reclaim
     * click is a much smaller artefact than a missing note in dense
     * music, and the cursor-preservation in `set_channel_with` keeps
     * the seam continuous in buffer phase. */
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
            state->voice_lockout_tics[i] = 0;
            return i;
        }
    }
    return -1;
}

/* Fade length applied to music NoteOff. 4 ms at 48 kHz = 192 output
 * frames is short enough to be perceived as "the note ended" rather
 * than "the note decayed", yet long enough to suppress the
 * step-discontinuity click that an immediate clear would produce
 * when the cursor was mid-cycle. */
#define M3OS_MUSIC_RELEASE_FRAMES 192

/* Release any voice playing `note` on `channel`. */
static void release_voice(m3os_mus_state_t *state, int channel, int note) {
    for (int i = 0; i < M3OS_VOICES; ++i) {
        if (state->voices[i].active && state->voices[i].channel == channel &&
            state->voices[i].note == note) {
            state->voices[i].active = 0;
            /* Lock the slot so the mixer's release-fade can run to
             * completion before another NoteOn re-seeds it (a
             * re-seed during fade would produce an audible
             * volume-step click). */
            state->voice_lockout_tics[i] = M3OS_VOICE_LOCKOUT_TICS;
            audio_mixer_t *m = g_mixer_accessor ? g_mixer_accessor() : NULL;
            if (m != NULL) {
                /* Linear fade-out instead of immediate clear so the
                 * mid-cycle waveform doesn't drop to silence in one
                 * frame (audible click; cumulative over many notes
                 * per second the listener hears as crackle). */
                audio_mixer_release_channel(
                    m, (size_t)(M3OS_MUSIC_CHANNEL_BASE + i),
                    M3OS_MUSIC_RELEASE_FRAMES);
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
    /* Loop the one-period waveform so the note sustains until
     * NoteOff. Without looping, the mixer plays exactly one period
     * (~2 ms at 440 Hz) and goes silent — each note becomes an
     * audible click instead of a held tone. */
    audio_mixer_set_channel_loop(m, (size_t)(M3OS_MUSIC_CHANNEL_BASE + voice_idx),
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

    /* MUS channel 15 (0-based) is the percussion / drum channel. In
     * a SoundFont synth, each NoteOn on this channel triggers a
     * different drum sample (e.g. note 49 = crash cymbal, 42 =
     * closed hi-hat). Tier 2a has no drum-synth, so playing them
     * as pitched tones at the note's frequency produces buzzy
     * artefacts the listener hears as crackle on cymbal/hi-hat
     * hits. We still consume the body bytes (otherwise the stream
     * parser desynchronises) but skip the synth interaction. */
#define M3OS_MUS_DRUM_CHANNEL 15

    switch (event) {
    case M3OS_MUS_EVT_RELEASE_NOTE: {
        if (state->cursor >= state->lump_len) {
            state->finished = 1;
            break;
        }
        uint8_t note = state->lump[state->cursor++] & 0x7F;
        if (channel != M3OS_MUS_DRUM_CHANNEL) {
            release_voice(state, channel, note);
        }
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
        if (channel == M3OS_MUS_DRUM_CHANNEL) {
            /* Body bytes consumed above; no synth voice claimed. */
            break;
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
            state->tick_remaining = read_varint(state);
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
    /* Tick down per-voice slot lockouts so released voices become
     * claimable again once their release-fade has had time to run
     * to completion (M3OS_VOICE_LOCKOUT_TICS DOOM-tics ≈ 58 ms,
     * comfortably longer than the 4 ms mixer fade). */
    for (int i = 0; i < M3OS_VOICES; ++i) {
        if (g_current_song->voice_lockout_tics[i] > 0) {
            g_current_song->voice_lockout_tics[i] -= 1;
        }
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
