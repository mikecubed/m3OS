/*
 * libhello_versioned_v2 — Phase 76d.G.3 mismatch-test demo lib.
 *
 * Real implementation defines `hello_str` under `LIBHELLO_2.0`
 * only. Paired with a separate link-time stub
 * (`libhello_versioned_v2_link.so`) that defines `hello_str` under
 * the OLDER `LIBHELLO_1.0` version; the consumer is linked against
 * the stub but at runtime loads this real v2 lib. The version
 * mismatch fires Phase 76d.D2.2's fallback warn (default mode) or
 * D2.3's strict error (`LD_BIND_NOW=1`).
 */

#ifndef LIBHELLO_VERSIONED_V2_HELLO_H
#define LIBHELLO_VERSIONED_V2_HELLO_H

const char *hello_str(void);

#endif
