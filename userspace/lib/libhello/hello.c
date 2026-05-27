/*
 * libhello — Phase 76b demo shared library.
 *
 * Smallest possible exported-symbol shape that exercises the
 * Phase 76b dynamic-linker bring-up path end-to-end:
 *
 *   - `DT_SONAME = libhello.so`        (set via -Wl,-soname)
 *   - `DT_HASH`  present, `DT_GNU_HASH` absent  (forced by --hash-style=sysv)
 *   - one exported symbol, `hello_str`, returning a string literal
 *     that ends with the sentinel the smoke gate greps for.
 *
 * The string is deliberately placed in .rodata (no constructors, no
 * mutable state) so this DSO needs no `R_X86_64_GLOB_DAT` /
 * `R_X86_64_64` writes of its own — its own internal `*ptr` accesses
 * resolve via `R_X86_64_RELATIVE` against the load bias. The
 * consumer (`dynlink_hello`) exercises `R_X86_64_GLOB_DAT` /
 * `R_X86_64_JUMP_SLOT` on its end when it calls `hello_str()`.
 */

#include "hello.h"

const char *hello_str(void) {
    return "HELLO_FROM_SHARED_LIB:OK";
}
