/*
 * m3os_dmx.c — Phase 63a Track C: WAD DMX SFX decoder.
 *
 * Pure C, no allocation, no I/O. Validates the 8-byte DMX header,
 * bounds-checks the declared sample count against the lump length,
 * and returns a zero-copy view (`(rate_hz, samples, len)`) for the
 * mixer to consume.
 */
#include "m3os_dmx.h"

/* DMX SFX lumps in real DOOM WADs use an **8-byte** header:
 *   bytes 0..1: format tag (u16le, must equal 3)
 *   bytes 2..3: sample rate (u16le, Hz)
 *   bytes 4..7: sample count (u32le)
 *   bytes 8..: unsigned-8 PCM samples
 *
 * Earlier revisions of this decoder assumed a 12-byte header
 * (treating the next 4 bytes as DMX-library padding). That made
 * every real-WAD SFX fail the `sample_count + 12 > lump_len`
 * bounds check — DOOM1.WAD's DSPISTOL has lump_len=3208 +
 * sample_count=3200, which fits exactly with an 8-byte header but
 * overflows a 12-byte one. The chocolate-doom and vanilla i_sound
 * loaders both use 8 bytes; we now match them.
 */
#define M3OS_DMX_HEADER 8

/* Smallest DMX lump we'll accept: 8-byte header + at least 1 byte
 * of body. Lumps shorter than this are malformed. */
#define M3OS_DMX_MIN_LUMP (M3OS_DMX_HEADER + 1)

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
    /* Rejecting `sample_count + 8 > lump_len` keeps the per-sample
     * reads in `audio_mixer_step` in-bounds. The cast to size_t
     * avoids an integer overflow on 32-bit platforms. */
    if ((size_t)sample_count + (size_t)M3OS_DMX_HEADER > lump_len) {
        return -1;
    }
    if (sample_count == 0) {
        return -1;
    }
    out->rate_hz = rate_hz;
    out->samples = lump + M3OS_DMX_HEADER;
    out->len = sample_count;
    /* Pad-stripping is intentionally NOT performed here. The
     * chocolate-doom and vanilla i_sound loaders both pass the
     * full `sample_count` window unchanged; the leading/trailing
     * sustain bytes (when present) hold the same value as the first
     * / last "real" sample so they don't introduce audible clicks
     * once the channel is seeded. */
    return 0;
}
