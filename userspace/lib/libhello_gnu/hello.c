/*
 * libhello_gnu — Phase 76d.F demo shared library.
 *
 * Same shape as libhello, but staged under a different basename
 * (`libhello_gnu.so`) so it can coexist on disk with `libhello.so`
 * and the smoke gates can resolve the two against different hash
 * tables.
 *
 *   - `DT_SONAME = libhello_gnu.so`           (set via -Wl,-soname)
 *   - `DT_GNU_HASH` present, `DT_HASH` absent (forced by --hash-style=gnu)
 *   - one exported symbol, `hello_str`, returning the GNU-specific
 *     sentinel `HELLO_FROM_GNU_LIB:OK`
 */

#include "hello.h"

const char *hello_str(void) {
    return "HELLO_FROM_GNU_LIB:OK";
}
