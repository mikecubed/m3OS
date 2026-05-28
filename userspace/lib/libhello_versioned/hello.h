/*
 * libhello_versioned — Phase 76d.G demo shared library.
 *
 * Exports `hello_str` under the symbol-version `LIBHELLO_1.0` so
 * the consumer's `DT_VERNEED` records a version constraint and the
 * runtime linker (Phase 76d.D2.2) must match it against the
 * provider's `DT_VERSYM` + `DT_VERDEF`.
 */

#ifndef LIBHELLO_VERSIONED_HELLO_H
#define LIBHELLO_VERSIONED_HELLO_H

const char *hello_str(void);

#endif
