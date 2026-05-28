/*
 * libhello_gnu — Phase 76d.F demo shared library.
 *
 * Identical exported-symbol shape to `libhello`, but built with
 * `-Wl,--hash-style=gnu` so `DT_GNU_HASH` is populated and
 * `DT_HASH` is absent. Exercises Phase 76d.D1's GNU-hash backend
 * end-to-end through the bring-up linker.
 */

#ifndef LIBHELLO_GNU_HELLO_H
#define LIBHELLO_GNU_HELLO_H

const char *hello_str(void);

#endif
