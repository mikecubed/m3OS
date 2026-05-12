/*
 * test_m3os_sound.c — Phase 63a Track D.6: host-side C unit tests
 * for the sound module state machine. Built with
 * `-DM3OS_SOUND_HOST_TEST` so m3os_sound.c omits the
 * doomgeneric-dependent sound_module_t adapter block; we exercise
 * the `_inner` test seam directly.
 *
 * audio_mixer_* and the FFI submitter are stubbed in this file so
 * the test runs without linking the Rust staticlibs.
 */
/* Use the POSIX feature-test macro so dup/dup2/fileno/close show up
 * under `-std=c11` (which is `__STRICT_ANSI__` by default). Must be
 * before any system headers. */
#define _POSIX_C_SOURCE 200809L

#include "../m3os_sound.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define ASSERT(cond, msg)                                                       \
    do {                                                                        \
        if (!(cond)) {                                                          \
            fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, msg);            \
            return 1;                                                           \
        }                                                                       \
    } while (0)

/* AUDIO_FFI_ERR_* constants we depend on. Kept in sync with
 * audio_client.h by audio_client_ffi/build.rs. */
#define AUDIO_FFI_ERR_BUSY -1
#define AUDIO_FFI_ERR_WOULD_BLOCK -2

/* -----------------------------------------------------------------
 * Stubs for the audio_mixer C-ABI.
 * ---------------------------------------------------------------*/
typedef struct {
    size_t idx;
    uint32_t rate_hz;
    uint8_t left_vol;
    uint8_t right_vol;
    int active;
    size_t set_call_count;
    size_t clear_call_count;
} fake_channel_t;

#define FAKE_MIXER_CAP 32
static fake_channel_t g_fake_channels[FAKE_MIXER_CAP];
static int g_mixer_new_calls;
static int g_mixer_drop_calls;
static size_t g_mixer_step_call_count;

/* Sentinel pointer returned by audio_mixer_new so the production code
 * stores it as the mixer handle. Any non-NULL value works. */
static const intptr_t k_fake_mixer_sentinel = 0xC0DECAFEu;

typedef struct audio_mixer_t audio_mixer_t;
audio_mixer_t *audio_mixer_new(size_t channel_count) {
    g_mixer_new_calls++;
    (void)channel_count;
    return (audio_mixer_t *)k_fake_mixer_sentinel;
}
void audio_mixer_drop(audio_mixer_t *mixer) {
    (void)mixer;
    g_mixer_drop_calls++;
}
int audio_mixer_set_channel(audio_mixer_t *mixer, size_t idx,
                            const uint8_t *samples, size_t len,
                            uint32_t source_rate_hz, uint8_t left_vol,
                            uint8_t right_vol) {
    (void)mixer; (void)samples; (void)len;
    if (idx >= FAKE_MIXER_CAP) return -1;
    g_fake_channels[idx].idx = idx;
    g_fake_channels[idx].rate_hz = source_rate_hz;
    g_fake_channels[idx].left_vol = left_vol;
    g_fake_channels[idx].right_vol = right_vol;
    g_fake_channels[idx].active = 1;
    g_fake_channels[idx].set_call_count++;
    return 0;
}
int audio_mixer_clear_channel(audio_mixer_t *mixer, size_t idx) {
    (void)mixer;
    if (idx >= FAKE_MIXER_CAP) return -1;
    g_fake_channels[idx].active = 0;
    g_fake_channels[idx].clear_call_count++;
    return 0;
}
ptrdiff_t audio_mixer_step(audio_mixer_t *mixer, uint8_t *out,
                            size_t byte_capacity, size_t frames) {
    (void)mixer;
    g_mixer_step_call_count++;
    size_t needed = frames * 4;
    if (byte_capacity < needed) return -3;
    memset(out, 0, needed);
    return (ptrdiff_t)needed;
}

/* -----------------------------------------------------------------
 * Stubs for audio_ffi_* — referenced by m3os_sound.c's production
 * adapter block but never linked in the test build (the block is
 * compiled out by M3OS_SOUND_HOST_TEST). We still need the symbol
 * names available because the test driver invokes the `_inner`
 * functions which never touch these stubs.
 *
 * The fake submitter that tests inject is wired entirely through
 * the m3os_audio_submitter_t function-table — these globals are
 * unused.
 * ---------------------------------------------------------------*/

/* -----------------------------------------------------------------
 * FakeSubmitter — captures every connect / open / submit / get_stats
 * / close call and exposes scripted return codes.
 * ---------------------------------------------------------------*/
typedef struct {
    int connect_should_fail;
    int open_return_code;
    intptr_t submit_return;
    int get_stats_return;
    m3os_audio_stats_t scripted_stats;
    /* Recorded call counts. */
    int connect_calls;
    int open_calls;
    int submit_calls;
    size_t submit_total_bytes;
    int get_stats_calls;
    int close_calls;
    /* Sentinel handle returned to the module. */
    int sentinel_handle;
} fake_submitter_state_t;

static fake_submitter_state_t g_fake_sub;

static void *fake_connect(void) {
    g_fake_sub.connect_calls++;
    if (g_fake_sub.connect_should_fail) return NULL;
    return &g_fake_sub.sentinel_handle;
}
static int fake_open(void *handle) {
    (void)handle;
    g_fake_sub.open_calls++;
    return g_fake_sub.open_return_code;
}
static intptr_t fake_submit(void *handle, const uint8_t *bytes, size_t len) {
    (void)handle; (void)bytes;
    g_fake_sub.submit_calls++;
    g_fake_sub.submit_total_bytes += len;
    return g_fake_sub.submit_return < 0 ? g_fake_sub.submit_return
                                         : (intptr_t)len;
}
static int fake_get_stats(void *handle, m3os_audio_stats_t *out) {
    (void)handle;
    g_fake_sub.get_stats_calls++;
    if (g_fake_sub.get_stats_return == 0) {
        *out = g_fake_sub.scripted_stats;
    }
    return g_fake_sub.get_stats_return;
}
static void fake_close(void *handle) {
    (void)handle;
    g_fake_sub.close_calls++;
}

static m3os_audio_submitter_t make_fake_submitter(void) {
    m3os_audio_submitter_t s = {
        .connect = fake_connect,
        .open = fake_open,
        .submit = fake_submit,
        .get_stats = fake_get_stats,
        .close = fake_close,
    };
    return s;
}

static void reset_world(void) {
    memset(&g_fake_sub, 0, sizeof(g_fake_sub));
    memset(g_fake_channels, 0, sizeof(g_fake_channels));
    g_mixer_new_calls = 0;
    g_mixer_drop_calls = 0;
    g_mixer_step_call_count = 0;
    /* Clear the module-global state by re-initializing via the
     * test seam. */
    m3os_audio_submitter_t s = make_fake_submitter();
    m3os_sound_inject_submitter(&s);
}

/* -----------------------------------------------------------------
 * Tests
 * ---------------------------------------------------------------*/

static int test_init_happy_path(void) {
    reset_world();
    g_fake_sub.open_return_code = 0;
    int rc = m3os_sound_init_inner();
    ASSERT(rc == 1, "init should return truthy on success");
    ASSERT(g_fake_sub.connect_calls == 1, "connect called exactly once");
    ASSERT(g_fake_sub.open_calls == 1, "open called exactly once");
    ASSERT(g_mixer_new_calls == 1, "audio_mixer_new called exactly once");
    ASSERT(m3os_sound_audio_disabled() == 0, "audio should not be disabled");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_init_ebusy_silent(void) {
    reset_world();
    g_fake_sub.open_return_code = AUDIO_FFI_ERR_BUSY;
    int rc = m3os_sound_init_inner();
    ASSERT(rc == 1, "init should still return success on EBUSY (silent-fallback)");
    ASSERT(g_fake_sub.connect_calls == 1, "connect called once");
    ASSERT(g_fake_sub.open_calls == 1, "open called once");
    ASSERT(g_fake_sub.close_calls == 1, "close called once on EBUSY fallback");
    ASSERT(m3os_sound_audio_disabled() == 1, "audio_disabled should be set on EBUSY");
    ASSERT(g_mixer_new_calls == 0, "mixer should not be created on EBUSY");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_init_connect_failure_silent(void) {
    reset_world();
    g_fake_sub.connect_should_fail = 1;
    int rc = m3os_sound_init_inner();
    ASSERT(rc == 1, "init should still return success on connect-failed");
    ASSERT(g_fake_sub.open_calls == 0, "open not called when connect fails");
    ASSERT(m3os_sound_audio_disabled() == 1, "audio_disabled on connect failure");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_start_sound_claims_channel(void) {
    reset_world();
    g_fake_sub.open_return_code = 0;
    m3os_sound_init_inner();
    uint8_t samples[8] = {128, 192, 128, 64, 128, 200, 100, 128};
    m3os_sound_start_decoded(3, samples, sizeof(samples), 11025, 100, 64);
    ASSERT(g_fake_channels[3].active == 1, "channel 3 should be active");
    ASSERT(g_fake_channels[3].rate_hz == 11025, "channel 3 rate mismatch");
    ASSERT(g_fake_channels[0].active == 0, "channel 0 should remain inactive");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_stop_sound_clears_channel(void) {
    reset_world();
    g_fake_sub.open_return_code = 0;
    m3os_sound_init_inner();
    uint8_t samples[4] = {128, 192, 128, 64};
    m3os_sound_start_decoded(3, samples, sizeof(samples), 11025, 100, 64);
    ASSERT(g_fake_channels[3].active == 1, "precondition: channel 3 active");
    m3os_sound_stop_inner(3);
    ASSERT(g_fake_channels[3].active == 0, "channel 3 should be cleared");
    ASSERT(g_fake_channels[3].clear_call_count == 1, "clear_channel called once");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_update_skips_when_audio_disabled(void) {
    reset_world();
    g_fake_sub.open_return_code = AUDIO_FFI_ERR_BUSY;
    m3os_sound_init_inner();
    /* audio_disabled is set — Update must be a no-op. */
    m3os_sound_update_inner();
    m3os_sound_update_inner();
    m3os_sound_update_inner();
    ASSERT(g_fake_sub.submit_calls == 0, "submit should not be called when disabled");
    ASSERT(g_mixer_step_call_count == 0, "mixer step should not be called when disabled");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_update_submits_when_enabled(void) {
    reset_world();
    g_fake_sub.open_return_code = 0;
    m3os_sound_init_inner();
    m3os_sound_update_inner();
    ASSERT(g_mixer_step_call_count == 1, "mixer step called once per Update");
    ASSERT(g_fake_sub.submit_calls == 1, "submit called once per Update");
    ASSERT(g_fake_sub.submit_total_bytes == M3OS_PCM_TIC_BYTES,
           "submit total bytes should equal one tic");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_update_swallows_wouldblock(void) {
    reset_world();
    g_fake_sub.open_return_code = 0;
    g_fake_sub.submit_return = AUDIO_FFI_ERR_WOULD_BLOCK;
    m3os_sound_init_inner();
    /* Several updates with WouldBlock — no crashes, no error log
     * spam (we can't easily assert log absence in a unit test, but
     * the fact that the test exits cleanly is the contract). */
    for (int i = 0; i < 5; i++) {
        m3os_sound_update_inner();
    }
    ASSERT(g_fake_sub.submit_calls == 5, "submit attempted every tic");
    m3os_sound_shutdown_inner();
    return 0;
}

static int test_shutdown_emits_audio_summary(void) {
    /* The shutdown summary prints to stdout. Capture by redirecting
     * stdout to a temp file and inspecting after shutdown. */
    reset_world();
    g_fake_sub.open_return_code = 0;
    g_fake_sub.scripted_stats.frames_submitted = 12345;
    g_fake_sub.scripted_stats.frames_consumed = 12000;
    g_fake_sub.scripted_stats.underrun_count = 2;
    m3os_sound_init_inner();

    /* Redirect stdout. */
    fflush(stdout);
    int saved = dup(fileno(stdout));
    FILE *tmp = tmpfile();
    if (tmp == NULL) {
        return 1;
    }
    dup2(fileno(tmp), fileno(stdout));

    m3os_sound_shutdown_inner();

    fflush(stdout);
    fseek(tmp, 0, SEEK_SET);
    char buf[512] = {0};
    fread(buf, 1, sizeof(buf) - 1, tmp);
    dup2(saved, fileno(stdout));
    close(saved);
    fclose(tmp);

    ASSERT(strstr(buf, "M3OS_DOOM:audio_summary") != NULL,
           "audio_summary line missing");
    ASSERT(strstr(buf, "frames_submitted=12345") != NULL,
           "frames_submitted not in summary");
    ASSERT(strstr(buf, "frames_consumed=12000") != NULL,
           "frames_consumed not in summary");
    ASSERT(strstr(buf, "underruns=2") != NULL, "underruns not in summary");
    return 0;
}

static int test_shutdown_idempotent(void) {
    reset_world();
    g_fake_sub.open_return_code = 0;
    m3os_sound_init_inner();
    m3os_sound_shutdown_inner();
    /* Second call must not double-close or double-drop. */
    int close_calls_before = g_fake_sub.close_calls;
    int drop_calls_before = g_mixer_drop_calls;
    m3os_sound_shutdown_inner();
    ASSERT(g_fake_sub.close_calls == close_calls_before,
           "close should not be called twice");
    ASSERT(g_mixer_drop_calls == drop_calls_before,
           "drop should not be called twice");
    return 0;
}

typedef struct {
    const char *name;
    int (*fn)(void);
} test_entry;

int main(void) {
    test_entry tests[] = {
        {"test_init_happy_path", test_init_happy_path},
        {"test_init_ebusy_silent", test_init_ebusy_silent},
        {"test_init_connect_failure_silent", test_init_connect_failure_silent},
        {"test_start_sound_claims_channel", test_start_sound_claims_channel},
        {"test_stop_sound_clears_channel", test_stop_sound_clears_channel},
        {"test_update_skips_when_audio_disabled", test_update_skips_when_audio_disabled},
        {"test_update_submits_when_enabled", test_update_submits_when_enabled},
        {"test_update_swallows_wouldblock", test_update_swallows_wouldblock},
        {"test_shutdown_emits_audio_summary", test_shutdown_emits_audio_summary},
        {"test_shutdown_idempotent", test_shutdown_idempotent},
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
