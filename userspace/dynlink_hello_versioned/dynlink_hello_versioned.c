/*
 * dynlink_hello_versioned — Phase 76d.G end-to-end smoke binary.
 *
 * Built with `--hash-style=gnu`, links `libhello_versioned.so`
 * which exports `hello_str` under symbol version `LIBHELLO_1.0`.
 * This consumer's `DT_VERNEED` therefore records a requirement
 * for `LIBHELLO_1.0` against `libhello_versioned.so`; the runtime
 * linker (Phase 76d.D2.2) must match it against the provider's
 * `DT_VERSYM` + `DT_VERDEF` and only then bind the symbol.
 *
 * Success sentinel: `HELLO_FROM_VERSIONED_LIB:OK` (printed from
 * the resolved symbol's return value).
 */

#include "../lib/libhello_versioned/hello.h"

typedef long i64;

static i64 sys_write(i64 fd, const char *buf, i64 len) {
    i64 ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "0"(1), "D"(fd), "S"(buf), "d"(len)
        : "rcx", "r11", "memory");
    return ret;
}
static void sys_exit(i64 code) {
    __asm__ volatile(
        "syscall"
        :
        : "a"(60), "D"(code)
        : "rcx", "r11", "memory");
    __builtin_unreachable();
}
static i64 c_strlen(const char *s) {
    i64 n = 0;
    while (s[n] != '\0') n++;
    return n;
}
void _start(void) {
    const char *msg = hello_str();
    i64 len = c_strlen(msg);
    sys_write(1, msg, len);
    sys_write(1, "\n", 1);
    sys_exit(0);
}
