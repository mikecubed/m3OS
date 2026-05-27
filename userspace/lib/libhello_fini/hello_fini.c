/*
 * libhello_fini — Phase 76c destructor-pipeline demo shared library.
 *
 * The `__attribute__((destructor))` function is registered into
 * `DT_FINI_ARRAY` by GNU ld; the Phase 76c dynamic linker walks the
 * array in reverse order on `dlclose`'s last-close path.
 *
 * The destructor writes its sentinel via raw `syscall(2)` rather
 * than `printf` — `printf` depends on stdio flush, which is fragile
 * on the about-to-be-unmapped DSO. Writing one `write(2)` directly
 * is the cleanest shape for the smoke gate to observe.
 *
 * The destructor sentinel `LIBHELLO_FINI:RAN\n` must appear on the
 * serial console between the `DLOPEN_TEST:FINI_PENDING` and
 * `DLOPEN_TEST:PASS` bracket markers the test prints.
 */

#include "hello_fini.h"

/* Raw write(2) — same shape as the libhello_fini smoke binary uses. */
static long sys_write(long fd, const char *buf, long len) {
    long ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "0"(1), "D"(fd), "S"(buf), "d"(len)
        : "rcx", "r11", "memory");
    return ret;
}

const char *hello_fini_str(void) {
    return "HELLO_FROM_FINI_LIB:OK";
}

__attribute__((destructor))
static void __hello_fini_dtor(void) {
    static const char SENTINEL[] = "LIBHELLO_FINI:RAN\n";
    /* fd 2 = stderr. The kernel serial console mirrors both fd 1
     * and fd 2 in smoke-test mode. */
    sys_write(2, SENTINEL, sizeof(SENTINEL) - 1);
}
