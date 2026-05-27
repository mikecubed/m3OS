/*
 * libhello_fini — Phase 76c destructor-pipeline demo shared library.
 *
 * Same shape as libhello but with a `__attribute__((destructor))`
 * function that writes a sentinel to fd 1 (stdout) on `dlclose` —
 * fd 1 rather than fd 2 keeps the destructor sentinel in the same
 * stream as the test's bracket markers under the smoke gate's
 * capture. The dlopen_test gate asserts the ordering
 * `DLOPEN_TEST:FINI_PENDING` → `LIBHELLO_FINI:RAN` → `DLOPEN_TEST:PASS`
 * to prove the DT_FINI_ARRAY pipeline actually runs.
 *
 *   - `DT_SONAME = libhello_fini.so` (via `-Wl,-soname`)
 *   - `DT_HASH` present, `DT_GNU_HASH` absent (via `--hash-style=sysv`)
 *   - one exported function `hello_fini_str()` so the consumer can
 *     dlsym it if it wants to (the smoke gate currently exercises
 *     only the destructor side).
 */

#ifndef LIBHELLO_FINI_H
#define LIBHELLO_FINI_H

const char *hello_fini_str(void);

#endif /* LIBHELLO_FINI_H */
