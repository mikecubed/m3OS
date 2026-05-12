/*
 * m3os_dmx.c — Phase 63a Track C: WAD DMX SFX decoder.
 *
 * Pure C, no allocation, no I/O. Validates the 12-byte DMX header,
 * bounds-checks the declared sample count against the lump length,
 * and returns a zero-copy view (`(rate_hz, samples, len)`) for the
 * mixer to consume.
 */
#include "m3os_dmx.h"

/* Smallest DMX lump we'll accept: 12-byte header + at least 4 bytes
 * of body. Lumps shorter than this are malformed. */
#define M3OS_DMX_MIN_LUMP 16

/* DMX format tag is 3 for the unsigned-8 PCM container used by every
 * shipping DOOM SFX. */
#define M3OS_DMX_FORMAT_TAG 3

/* Helpers — little-endian field readers. The DMX header is fixed
 * size so we can address fields by offset rather than walking a
 * cursor. */
static uint16_t m3os_dmx_read_u16le(const uint8_t *p) {
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static uint32_t m3os_dmx_read_u32le(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) |
           ((uint32_t)p[3] << 24);
}

/* DMX SFX lumps in real DOOM WADs reserve 16 sustain-pad bytes at
 * the start and 16 at the end of the sample body (chocolate-doom
 * precedent — the pads hold the sample's starting/ending volume to
 * prevent DMA double-buffer clicks on legacy SoundBlaster cards).
 * Our mixer interpolates so the pads serve no purpose here, but
 * playing them DOES introduce an audible click at the SFX boundary
 * because the silence-before-sample transition isn't smooth.
 *
 * Strip the pads when the lump is large enough to have them
 * (real WADs always are); smaller lumps — typically synthetic test
 * fixtures — pass through verbatim. */
#define M3OS_DMX_PAD 16

int m3os_dmx_decode(const uint8_t *lump, size_t lump_len, m3os_dmx_decoded *out) {
    if (lump == NULL || out == NULL) {
        return -1;
    }
    if (lump_len < M3OS_DMX_MIN_LUMP) {
        return -1;
    }
    uint16_t format_tag = m3os_dmx_read_u16le(lump);
    if (format_tag != M3OS_DMX_FORMAT_TAG) {
        return -1;
    }
    uint16_t rate_hz = m3os_dmx_read_u16le(lump + 2);
    if (rate_hz == 0) {
        return -1;
    }
    uint32_t sample_count = m3os_dmx_read_u32le(lump + 4);
    /* The 12-byte header lives in front of `sample_count` bytes of
     * unsigned-8 PCM. Rejecting `sample_count + 12 > lump_len` keeps
     * the per-sample reads in `audio_mixer_step` in-bounds. The cast
     * to size_t avoids an integer overflow on 32-bit platforms. */
    if ((size_t)sample_count + 12u > lump_len) {
        return -1;
    }
    if (sample_count == 0) {
        return -1;
    }
    out->rate_hz = rate_hz;
    if (sample_count > (uint32_t)(2 * M3OS_DMX_PAD)) {
        out->samples = lump + 12 + M3OS_DMX_PAD;
        out->len = sample_count - (uint32_t)(2 * M3OS_DMX_PAD);
    } else {
        out->samples = lump + 12;
        out->len = sample_count;
    }
    return 0;
}
