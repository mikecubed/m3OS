/*
 * libhello_versioned — Phase 76d.G demo shared library.
 *
 *   * `DT_SONAME = libhello_versioned.so`        (set via -Wl,-soname)
 *   * `DT_GNU_HASH` present, `DT_HASH` absent     (forced by --hash-style=gnu)
 *   * `DT_VERDEF` populated with one version node, `LIBHELLO_1.0`,
 *     under which `hello_str` is exported. Driven by the .ver
 *     version script next to this source.
 *   * `hello_str` returns the sentinel `HELLO_FROM_VERSIONED_LIB:OK`
 *     so the G smoke gate can grep for it.
 */

#include "hello.h"

const char *hello_str(void) {
    return "HELLO_FROM_VERSIONED_LIB:OK";
}
