/*
 * Real v2 library — defines `hello_str` under `LIBHELLO_2.0` only.
 * SONAME is `libhello_versioned_v2.so` (matching the link-time
 * stub's SONAME so the consumer's `DT_NEEDED` resolves here).
 *
 * The unversioned-fallback sentinel proves D2.2's pass-2 path
 * found the symbol by name even after the version-aware pass-1
 * missed `LIBHELLO_1.0`.
 */

#include "hello.h"

const char *hello_str(void) {
    return "HELLO_FROM_V2_FALLBACK:OK";
}
