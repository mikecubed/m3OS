/*
 * m3os_dmx.h — Phase 63a Track C: WAD DMX SFX decoder.
 *
 * The DMX format ships in every DOOM SFX lump:
 *   bytes 0..1   : format tag (u16le, must equal 3)
 *   bytes 2..3   : sample rate (u16le, Hz)
 *   bytes 4..7   : sample count (u32le)
 *   bytes 8..11  : padding (u16le[2], ignored)
 *   bytes 12..   : unsigned 8-bit PCM (DMX-format, 128 = silence)
 *
 * Decoder is zero-copy: `samples` points back into the caller's lump
 * buffer. The caller must keep the lump alive for as long as the
 * decoded triple is referenced by the mixer.
 */
#ifndef M3OS_DMX_H
#define M3OS_DMX_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint16_t rate_hz;
    const uint8_t *samples;
    uint32_t len;
} m3os_dmx_decoded;

/* Returns 0 on success, -1 on malformed input. `out` is untouched on
 * failure. */
int m3os_dmx_decode(const uint8_t *lump, size_t lump_len, m3os_dmx_decoded *out);

#ifdef __cplusplus
}
#endif

#endif /* M3OS_DMX_H */
