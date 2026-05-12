/*
 * m3os_music.h — Phase 63a Track E: Tier 2a MUS synth.
 *
 * Public surface for the patches/i_sound.c overlay and the
 * test_m3os_music.c host test driver. The synth runs at the MUS
 * native 140 Hz tickrate; the dispatcher advances 4 ticks per
 * DOOM tic (35 Hz × 4 = 140 Hz).
 */
#ifndef M3OS_MUSIC_H
#define M3OS_MUSIC_H

#include <stddef.h>
#include <stdint.h>

#include "m3os_sound.h"

#ifdef __cplusplus
extern "C" {
#endif

#ifndef M3OS_SOUND_HOST_TEST
#include "doomgeneric/i_sound.h"
extern music_module_t m3os_music_module;
#endif

/* MUS event types — high nibble of the event control byte. */
#define M3OS_MUS_EVT_RELEASE_NOTE 0
#define M3OS_MUS_EVT_PLAY_NOTE 1
#define M3OS_MUS_EVT_PITCH_BEND 2
#define M3OS_MUS_EVT_SYSTEM 3
#define M3OS_MUS_EVT_CONTROLLER 4
#define M3OS_MUS_EVT_END_OF_MEASURE 5
#define M3OS_MUS_EVT_SCORE_END 6

/* Tier 2a waveforms. Per task spec, MUS instrument numbers 1..63 use
 * square; 64..127 use triangle. */
typedef enum {
    M3OS_WAVEFORM_SQUARE = 0,
    M3OS_WAVEFORM_TRIANGLE = 1,
} m3os_waveform_t;

typedef struct {
    int active;
    int channel; /* MUS channel 0..15 — used to track NoteOff lookup */
    int note;    /* MIDI note number 0..127 */
    int velocity; /* 0..127 */
    m3os_waveform_t waveform;
} m3os_voice_t;

/* MUS state — owned by m3os_music.c, exposed for tests. */
typedef struct m3os_mus_state m3os_mus_state_t;

/* Returns NULL on malformed input (magic mismatch, score offsets out
 * of range). On success, the state is heap-allocated; free with
 * m3os_mus_state_free. */
m3os_mus_state_t *m3os_mus_parse_header(const uint8_t *lump, size_t lump_len);
void m3os_mus_state_free(m3os_mus_state_t *state);

/* Advance one MUS tick. Returns 0 on success, 1 on ScoreEnd
 * (caller may rewind for looping or stop playback). */
int m3os_music_tick(m3os_mus_state_t *state);

/* Test seam — set MUS volume scaling (0..127). */
void m3os_music_set_master_volume(uint8_t vol);
uint8_t m3os_music_get_master_volume(void);

/* Returns the count of currently-sounding voices. */
int m3os_music_active_voice_count(const m3os_mus_state_t *state);

/* Test seam — silence every voice (used by Shutdown / StopSong). */
void m3os_music_stop_all_inner(m3os_mus_state_t *state);

#ifdef __cplusplus
}
#endif

#endif /* M3OS_MUSIC_H */
