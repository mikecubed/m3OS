/*
 * dynlink_hello_versioned_mismatch — Phase 76d.G.3 mismatch test.
 *
 * Linked at build time against `libhello_versioned_v2_link.so`
 * (a stub defining `hello_str@LIBHELLO_1.0` with SONAME
 * `libhello_versioned_v2.so`). At runtime the linker loads
 * `/usr/lib/libhello_versioned_v2.so`, which is the REAL v2 lib —
 * it defines `hello_str` only under `LIBHELLO_2.0`. The consumer's
 * `DT_VERNEED` requires `LIBHELLO_1.0`, so:
 *
 *   * Default mode (POSIX lazy) — D2.2 emits a serial warning,
 *     falls back to an unversioned scan, and resolves to v2's
 *     `hello_str`. Sentinel: `HELLO_FROM_V2_FALLBACK:OK`.
 *   * Strict mode (`LD_BIND_NOW=1`) — D2.3 emits a serial error
 *     and returns `None`. apply_rela then `exit(0)` returns from
 *     `dl_entry` and the asm caller `jmp 0`s → kernel kills the
 *     process. The smoke gate asserts a non-zero exit code.
 */

#include "../lib/libhello_versioned_v2/hello.h"

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
