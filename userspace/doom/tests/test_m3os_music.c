/*
 * test_m3os_music.c — Phase 63a Track E.4: host-side C unit tests
 * for the Tier 2a MUS synth. Built with `-DM3OS_SOUND_HOST_TEST`
 * so the doomgeneric-dependent music_module_t adapter is excluded.
 *
 * audio_mixer_* is stubbed in this file. The mixer accessor returns
 * a sentinel pointer so the synth's set_channel / clear_channel
 * calls land in our fake.
 */
#define _POSIX_C_SOURCE 200809L

#include "../m3os_music.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define ASSERT(cond, msg)                                                       \
    do {                                                                        \
        if (!(cond)) {                                                          \
            fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, msg);            \
            return 1;                                                           \
        }                                                                       \
    } while (0)

/* -----------------------------------------------------------------
 * audio_mixer stubs
 * ---------------------------------------------------------------*/
typedef struct {
    int active;
    uint32_t rate_hz;
    uint8_t left_vol;
    uint8_t right_vol;
    size_t set_calls;
    size_t clear_calls;
} fake_channel_t;

#define MIXER_CHANNELS 32
static fake_channel_t g_channels[MIXER_CHANNELS];

typedef struct audio_mixer_t audio_mixer_t;
static const intptr_t k_mixer_sentinel = 0xC0DECAFE;
static audio_mixer_t *fake_mixer_accessor(void) {
    return (audio_mixer_t *)k_mixer_sentinel;
}

audio_mixer_t *audio_mixer_new(size_t count) {
    (void)count;
    return (audio_mixer_t *)k_mixer_sentinel;
}
void audio_mixer_drop(audio_mixer_t *m) { (void)m; }
int audio_mixer_set_channel(audio_mixer_t *m, size_t idx, const uint8_t *s,
                            size_t len, uint32_t rate, uint8_t lv, uint8_t rv) {
    (void)m; (void)s; (void)len;
    if (idx >= MIXER_CHANNELS) return -1;
    g_channels[idx].active = 1;
    g_channels[idx].rate_hz = rate;
    g_channels[idx].left_vol = lv;
    g_channels[idx].right_vol = rv;
    g_channels[idx].set_calls++;
    return 0;
}
int audio_mixer_set_channel_loop(audio_mixer_t *m, size_t idx, const uint8_t *s,
                                 size_t len, uint32_t rate, uint8_t lv,
                                 uint8_t rv) {
    /* Music voices reach the mixer via the looping variant; for
     * test-recording purposes we treat it identically to the
     * non-looping path. */
    return audio_mixer_set_channel(m, idx, s, len, rate, lv, rv);
}
int audio_mixer_clear_channel(audio_mixer_t *m, size_t idx) {
    (void)m;
    if (idx >= MIXER_CHANNELS) return -1;
    g_channels[idx].active = 0;
    g_channels[idx].clear_calls++;
    return 0;
}
int audio_mixer_release_channel(audio_mixer_t *m, size_t idx, uint16_t fade_frames) {
    /* The release fade ramps the mixer channel to silence over
     * `fade_frames` output frames before deactivating; for test
     * accounting we treat it the same as a clear (the test fixtures
     * inspect `clear_calls` to verify NoteOff routed to the mixer). */
    (void)fade_frames;
    return audio_mixer_clear_channel(m, idx);
}
ptrdiff_t audio_mixer_step(audio_mixer_t *m, uint8_t *out, size_t cap, size_t frames) {
    (void)m; (void)out; (void)cap;
    return (ptrdiff_t)(frames * 4);
}

extern void m3os_music_set_mixer_accessor(audio_mixer_t *(*fn)(void));

static void reset_world(void) {
    memset(g_channels, 0, sizeof(g_channels));
    m3os_music_set_master_volume(100);
    m3os_music_set_mixer_accessor(fake_mixer_accessor);
}

/* Build a minimal MUS lump with header + score. The score is a
 * trailing buffer the caller controls. Header layout per
 * doom-wiki.org/wiki/MUS: 16-byte header + score body. */
static void build_mus_lump(uint8_t *buf, size_t header_size, const uint8_t *score,
                           size_t score_len) {
    buf[0] = 'M'; buf[1] = 'U'; buf[2] = 'S'; buf[3] = 0x1A;
    buf[4] = (uint8_t)(score_len & 0xFF);
    buf[5] = (uint8_t)((score_len >> 8) & 0xFF);
    buf[6] = (uint8_t)(header_size & 0xFF);
    buf[7] = (uint8_t)((header_size >> 8) & 0xFF);
    /* bytes 8..15: primary chans, secondary chans, instrument count,
     * reserved — all zero for Tier 2a. */
    memset(buf + 8, 0, 8);
    memcpy(buf + header_size, score, score_len);
}

/* -----------------------------------------------------------------
 * Tests
 * ---------------------------------------------------------------*/

static int test_mus_header_valid(void) {
    reset_world();
    /* score: one ScoreEnd event (control byte 0x60 = event 6, last=0)
     * followed by a delay byte 0 (varint). */
    uint8_t score[] = {0xE0, 0x00}; /* score-end with last flag */
    uint8_t buf[32] = {0};
    build_mus_lump(buf, 16, score, sizeof(score));
    m3os_mus_state_t *state = m3os_mus_parse_header(buf, 16 + sizeof(score));
    ASSERT(state != NULL, "valid lump should parse");
    m3os_mus_state_free(state);
    return 0;
}

static int test_mus_header_bad_magic(void) {
    reset_world();
    uint8_t buf[32] = {'X', 'Y', 'Z', 0, 1, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                        0xE0, 0};
    m3os_mus_state_t *state = m3os_mus_parse_header(buf, sizeof(buf));
    ASSERT(state == NULL, "bad magic must be rejected");
    return 0;
}

static int test_mus_header_score_out_of_range(void) {
    reset_world();
    uint8_t buf[18] = {0};
    /* score_len = 100 (way beyond buf size of 18). */
    buf[0] = 'M'; buf[1] = 'U'; buf[2] = 'S'; buf[3] = 0x1A;
    buf[4] = 100; buf[5] = 0;
    buf[6] = 16; buf[7] = 0;
    m3os_mus_state_t *state = m3os_mus_parse_header(buf, sizeof(buf));
    ASSERT(state == NULL, "score range overflow must be rejected");
    return 0;
}

static int test_note_on_off_round_trip(void) {
    reset_world();
    /* Score:
     *   PlayNote channel=0, note=60 (middle C), velocity included = 100
     *     ctrl byte = 0x10 (event=1, last=0, channel=0)
     *     note byte = 60 | 0x80 = 0xBC (high bit = velocity follows)
     *     velocity = 100 = 0x64
     *   ReleaseNote channel=0, note=60, last
     *     ctrl byte = 0x80 (event=0, last=1, channel=0)
     *     note byte = 60 = 0x3C
     *   delay = 0
     */
    uint8_t score[] = {
        0x10, 0xBC, 0x64,    /* PlayNote (not last) */
        0x80, 0x3C,          /* ReleaseNote (last) */
        0x00,                /* delay varint = 0 */
    };
    uint8_t buf[64] = {0};
    build_mus_lump(buf, 16, score, sizeof(score));
    m3os_mus_state_t *state = m3os_mus_parse_header(buf, 16 + sizeof(score));
    ASSERT(state != NULL, "lump should parse");

    /* Advance one tick: dispatches PlayNote, then ReleaseNote
     * (because the next event has the last flag set). */
    int finished = m3os_music_tick(state);
    ASSERT(finished == 0, "tick should not signal finished");

    /* After dispatching: NoteOn then NoteOff both ran. The voice
     * should be released, so active voice count is 0. */
    ASSERT(m3os_music_active_voice_count(state) == 0,
           "after NoteOff round-trip no voice should be active");
    /* But the mixer's set_channel was called (NoteOn) and
     * clear_channel was also called (NoteOff). */
    /* Music voices live at indices M3OS_MUSIC_CHANNEL_BASE..+15.
     * Voice 0 = mixer channel 16. */
    ASSERT(g_channels[16].set_calls == 1, "set_channel should fire on NoteOn");
    ASSERT(g_channels[16].clear_calls == 1, "clear_channel should fire on NoteOff");
    m3os_mus_state_free(state);
    return 0;
}

static int test_score_end_terminates(void) {
    reset_world();
    /* Score: just ScoreEnd with last flag. */
    uint8_t score[] = {0xE0, 0x00};
    uint8_t buf[32] = {0};
    build_mus_lump(buf, 16, score, sizeof(score));
    m3os_mus_state_t *state = m3os_mus_parse_header(buf, 16 + sizeof(score));
    ASSERT(state != NULL, "lump should parse");
    int finished = m3os_music_tick(state);
    ASSERT(finished == 1, "ScoreEnd must signal finished");
    /* Subsequent ticks should keep finished = 1 without crashing. */
    finished = m3os_music_tick(state);
    ASSERT(finished == 1, "finished state should be sticky");
    m3os_mus_state_free(state);
    return 0;
}

static int test_music_volume_scales_voices(void) {
    reset_world();
    m3os_music_set_master_volume(64); /* half */
    /* PlayNote with velocity 127. Expected per-channel vol = (64 * 127) / 127 = 64. */
    uint8_t score[] = {
        0x90, 0xBC, 0x7F,    /* PlayNote channel=0 note=60 vel=127 last=1 */
        0x00,
    };
    uint8_t buf[64] = {0};
    build_mus_lump(buf, 16, score, sizeof(score));
    m3os_mus_state_t *state = m3os_mus_parse_header(buf, 16 + sizeof(score));
    ASSERT(state != NULL, "lump should parse");

    m3os_music_tick(state);
    ASSERT(g_channels[16].set_calls == 1, "set_channel should fire");
    ASSERT(g_channels[16].left_vol == 64, "left_vol should scale with master");
    ASSERT(g_channels[16].right_vol == 64, "right_vol should scale with master");

    m3os_mus_state_free(state);
    return 0;
}

static int test_stop_all_clears_active_voices(void) {
    reset_world();
    /* Two PlayNote events on different channels then ScoreEnd. */
    uint8_t score[] = {
        0x10, 0xBC, 0x64,    /* PlayNote ch0 n60 v100 last=0 */
        0x11, 0xBE, 0x60,    /* PlayNote ch1 n62 v96  last=0 */
        0xE0,                /* ScoreEnd last=1 */
        0x00,                /* delay */
    };
    uint8_t buf[64] = {0};
    build_mus_lump(buf, 16, score, sizeof(score));
    m3os_mus_state_t *state = m3os_mus_parse_header(buf, 16 + sizeof(score));
    ASSERT(state != NULL, "lump should parse");
    m3os_music_tick(state);
    ASSERT(m3os_music_active_voice_count(state) == 2, "two voices should be active");
    m3os_music_stop_all_inner(state);
    ASSERT(m3os_music_active_voice_count(state) == 0, "stop_all should clear voices");
    m3os_mus_state_free(state);
    return 0;
}

typedef struct {
    const char *name;
    int (*fn)(void);
} test_entry;

int main(void) {
    test_entry tests[] = {
        {"test_mus_header_valid", test_mus_header_valid},
        {"test_mus_header_bad_magic", test_mus_header_bad_magic},
        {"test_mus_header_score_out_of_range", test_mus_header_score_out_of_range},
        {"test_note_on_off_round_trip", test_note_on_off_round_trip},
        {"test_score_end_terminates", test_score_end_terminates},
        {"test_music_volume_scales_voices", test_music_volume_scales_voices},
        {"test_stop_all_clears_active_voices", test_stop_all_clears_active_voices},
    };
    int failures = 0;
    for (size_t i = 0; i < sizeof(tests) / sizeof(tests[0]); ++i) {
        int rc = tests[i].fn();
        if (rc != 0) {
            fprintf(stderr, "FAIL: %s\n", tests[i].name);
            ++failures;
        } else {
            fprintf(stdout, "PASS: %s\n", tests[i].name);
        }
    }
    fprintf(stdout, "%zu tests, %d failures\n",
            sizeof(tests) / sizeof(tests[0]), failures);
    return failures;
}
