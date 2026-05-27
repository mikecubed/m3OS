/*
 * libhello — Phase 76b demo shared library.
 *
 * The Phase 76b dynamic linker resolves `hello_str` through
 * `DT_NEEDED = libhello.so` → DT_HASH lookup → `R_X86_64_JUMP_SLOT`
 * write into the consumer's PLT/GOT slot (PLT-routed calls use
 * JUMP_SLOT; data references would use `R_X86_64_GLOB_DAT`, but the
 * dynlink_hello consumer issues a function call, so only JUMP_SLOT
 * is exercised by the Phase 76b smoke gate).
 */

#ifndef LIBHELLO_H
#define LIBHELLO_H

const char *hello_str(void);

#endif /* LIBHELLO_H */
