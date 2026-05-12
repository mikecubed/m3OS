/*
 * test_m3os_dmx.c — Phase 63a Track C.2: host-side C unit tests for
 * the DMX decoder. Built and run by `xtask::doom_c_test_step` as
 * part of `cargo xtask check`.
 *
 * Each `test_*` function returns 0 on pass, non-zero on failure.
 * `main` aggregates the results and exits with the count of
 * failures.
 */
#include "../m3os_dmx.h"

#include <stdio.h>
#include <string.h>

#define ASSERT(cond, msg)                                                       \
    do {                                                                        \
        if (!(cond)) {                                                          \
            fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, msg);            \
            return 1;                                                           \
        }                                                                       \
    } while (0)

static int test_valid_lump(void) {
    /* 8-byte header + 4-byte body. Format tag 3, rate 11025, count 4.
     * Mirrors the real DMX layout: vanilla DOOM / chocolate-doom both
     * use an 8-byte header (not 12 — earlier revisions of this
     * decoder mistakenly assumed 12 and rejected every real-WAD SFX). */
    uint8_t lump[12] = {0};
    lump[0] = 3;  lump[1] = 0;            /* format tag = 3 */
    lump[2] = 0x11; lump[3] = 0x2B;       /* rate = 0x2B11 = 11025 */
    lump[4] = 4;  lump[5] = 0; lump[6] = 0; lump[7] = 0; /* count = 4 */
    lump[8] = 128; lump[9] = 192; lump[10] = 64; lump[11] = 200;

    m3os_dmx_decoded out;
    memset(&out, 0xCC, sizeof(out));
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == 0, "expected decode success");
    ASSERT(out.rate_hz == 11025, "rate mismatch");
    ASSERT(out.len == 4, "len mismatch");
    ASSERT(out.samples == lump + 8, "samples pointer should alias lump+8");
    ASSERT(out.samples[0] == 128 && out.samples[3] == 200, "sample bytes mismatch");
    return 0;
}

static int test_short_lump(void) {
    /* Lump shorter than the 8-byte header. */
    uint8_t lump[5] = {3, 0, 0x11, 0x2B, 4};
    m3os_dmx_decoded out;
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == -1, "lumps under the 9-byte minimum must be rejected");
    return 0;
}

static int test_bad_format_tag(void) {
    uint8_t lump[12] = {0};
    lump[0] = 7; lump[1] = 0;             /* format tag != 3 */
    lump[2] = 0x11; lump[3] = 0x2B;
    lump[4] = 4;
    m3os_dmx_decoded out;
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == -1, "non-3 format tag must be rejected");
    return 0;
}

static int test_oversize_sample_count(void) {
    /* sample_count claims 100 bytes of body but lump is 16 bytes total. */
    uint8_t lump[16] = {0};
    lump[0] = 3; lump[1] = 0;
    lump[2] = 0x11; lump[3] = 0x2B;
    lump[4] = 100; lump[5] = 0; lump[6] = 0; lump[7] = 0;
    m3os_dmx_decoded out;
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == -1, "sample_count larger than lump body must be rejected");
    return 0;
}

static int test_zero_rate(void) {
    /* rate_hz = 0 would divide by zero in the mixer's `inc` math. */
    uint8_t lump[12] = {0};
    lump[0] = 3; lump[1] = 0;
    /* rate = 0 */
    lump[4] = 4;
    m3os_dmx_decoded out;
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == -1, "rate_hz == 0 must be rejected");
    return 0;
}

static int test_null_pointers(void) {
    m3os_dmx_decoded out;
    uint8_t lump[12] = {3, 0, 0x11, 0x2B, 4, 0, 0, 0};
    ASSERT(m3os_dmx_decode(NULL, sizeof(lump), &out) == -1, "null lump rejected");
    ASSERT(m3os_dmx_decode(lump, sizeof(lump), NULL) == -1, "null out rejected");
    return 0;
}

static int test_real_wad_dspistol_shape(void) {
    /* Models the DOOM1.WAD DSPISTOL lump exactly: 8-byte header +
     * 3200-byte sample body. The earlier (12-byte-header) decoder
     * rejected this with `sample_count + 12 > lump_len` because
     * 3200 + 12 = 3212 > 3208. The fix makes it pass. */
    static uint8_t lump[3208];
    memset(lump, 128, sizeof(lump));  /* fill body with silence */
    lump[0] = 3; lump[1] = 0;
    lump[2] = 0x22; lump[3] = 0x56;   /* rate = 0x5622 = 22050 */
    lump[4] = 0x80; lump[5] = 0x0C;   /* count = 0x0C80 = 3200 */
    lump[6] = 0; lump[7] = 0;
    m3os_dmx_decoded out;
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == 0, "DSPISTOL-shape lump must decode (regression: 8-byte header)");
    ASSERT(out.rate_hz == 22050, "rate should be 22050");
    ASSERT(out.len == 3200, "len should equal sample_count");
    ASSERT(out.samples == lump + 8, "samples must start at lump+8");
    return 0;
}

typedef struct {
    const char *name;
    int (*fn)(void);
} test_entry;

int main(void) {
    test_entry tests[] = {
        {"test_valid_lump", test_valid_lump},
        {"test_short_lump", test_short_lump},
        {"test_bad_format_tag", test_bad_format_tag},
        {"test_oversize_sample_count", test_oversize_sample_count},
        {"test_zero_rate", test_zero_rate},
        {"test_null_pointers", test_null_pointers},
        {"test_real_wad_dspistol_shape", test_real_wad_dspistol_shape},
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
