/*
 * libhello — Phase 76b demo shared library.
 *
 * The Phase 76b dynamic linker resolves this declaration through
 * `DT_NEEDED = libhello.so` → DT_HASH lookup → `R_X86_64_GLOB_DAT`
 * write into the consumer's GOT slot.
 */

#ifndef LIBHELLO_H
#define LIBHELLO_H

const char *hello_str(void);

#endif /* LIBHELLO_H */
