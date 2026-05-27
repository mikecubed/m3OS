/*
 * dynlink_hello — Phase 76b end-to-end smoke binary.
 *
 * Built as a dynamic ELF that:
 *   - carries `PT_INTERP = /lib/ld-musl-x86_64.so.1`
 *   - has `DT_NEEDED = libhello.so`
 *   - calls `hello_str()` from libhello and writes the returned
 *     string + newline to stdout via raw `syscall` (no libc — Phase
 *     76b's bring-up linker only resolves the symbols we name
 *     explicitly).
 *
 * Built with:
 *   musl-gcc -fPIC -Wl,-pie -lhello -Wl,-rpath,/usr/lib \
 *     -Wl,--hash-style=sysv -nostartfiles
 *
 * The `_start` entry exits via the raw `SYS_exit` syscall (Linux
 * syscall number 60 — single-thread exit, not `exit_group`); the
 * Phase 76b smoke binary is single-threaded so this is equivalent
 * to a normal termination. Phase 76b's ld.so transfers control to
 * this function via the AT_ENTRY auxv slot after applying all
 * relocations and running constructors.
 */

#include "../lib/libhello/hello.h"

static long sys_write(long fd, const char *buf, long len) {
    long ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "0"(1), "D"(fd), "S"(buf), "d"(len)
        : "rcx", "r11", "memory");
    return ret;
}

static void sys_exit(long code) {
    __asm__ volatile(
        "syscall"
        :
        : "a"(60), "D"(code)
        : "rcx", "r11", "memory");
    __builtin_unreachable();
}

/* Length of a NUL-terminated C string (no libc available). */
static long c_strlen(const char *s) {
    long n = 0;
    while (s[n] != '\0') n++;
    return n;
}

void _start(void) {
    const char *msg = hello_str();
    long len = c_strlen(msg);
    sys_write(1, msg, len);
    sys_write(1, "\n", 1);
    sys_exit(0);
}
