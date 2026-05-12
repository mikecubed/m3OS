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
    /* 12-byte header + 4-byte body. Format tag 3, rate 11025, count 4. */
    uint8_t lump[16] = {0};
    lump[0] = 3;  lump[1] = 0;            /* format tag = 3 */
    lump[2] = 0x11; lump[3] = 0x2B;       /* rate = 0x2B11 = 11025 */
    lump[4] = 4;  lump[5] = 0; lump[6] = 0; lump[7] = 0; /* count = 4 */
    /* padding bytes 8..11 ignored */
    lump[12] = 128; lump[13] = 192; lump[14] = 64; lump[15] = 200;

    m3os_dmx_decoded out;
    memset(&out, 0xCC, sizeof(out));
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == 0, "expected decode success");
    ASSERT(out.rate_hz == 11025, "rate mismatch");
    ASSERT(out.len == 4, "len mismatch");
    ASSERT(out.samples == lump + 12, "samples pointer should alias lump+12");
    ASSERT(out.samples[0] == 128 && out.samples[3] == 200, "sample bytes mismatch");
    return 0;
}

static int test_short_lump(void) {
    uint8_t lump[10] = {3, 0, 0x11, 0x2B, 4, 0, 0, 0, 0, 0};
    m3os_dmx_decoded out;
    int rc = m3os_dmx_decode(lump, sizeof(lump), &out);
    ASSERT(rc == -1, "lumps under 16 bytes must be rejected");
    return 0;
}

static int test_bad_format_tag(void) {
    uint8_t lump[16] = {0};
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
    uint8_t lump[16] = {0};
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
    uint8_t lump[16] = {3, 0, 0x11, 0x2B, 4, 0, 0, 0};
    ASSERT(m3os_dmx_decode(NULL, sizeof(lump), &out) == -1, "null lump rejected");
    ASSERT(m3os_dmx_decode(lump, sizeof(lump), NULL) == -1, "null out rejected");
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
