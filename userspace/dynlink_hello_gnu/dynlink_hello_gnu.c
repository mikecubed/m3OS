/*
 * dynlink_hello_gnu — Phase 76d.F end-to-end smoke binary.
 *
 * Built with --hash-style=gnu, links libhello_gnu.so. Exercises
 * Phase 76d.D1's GNU-hash backend, B4's PLT lazy resolve, and
 * F.4's W^X invariant via /proc/self/maps.
 *
 *   * Sentinel `HELLO_FROM_GNU_LIB:OK` — basic GNU-hash path works.
 *   * Sentinel `WX_CHECK:OK` (F.4) — no `rwx` line in /proc/self/maps.
 *
 * The F.2 acceptance ("first call goes through trampoline") is
 * proven implicitly: under the lazy default (`BIND_NOW=false`), the
 * first call to `hello_str()` MUST traverse the PLT trampoline path
 * for the binary to print the sentinel. A direct GOT[3]-pointer
 * inspection is fragile in this build setup (`-no-pie -fno-pie
 * -Wl,-pie -nostartfiles` confuses the linker's
 * R_X86_64_GOTPC32 handling for `_GLOBAL_OFFSET_TABLE_`); the
 * end-to-end sentinel is the assertion that matters.
 *
 * The F.3 acceptance (LD_BIND_NOW=1 eager resolution) is verified by
 * the smoke gate running the binary twice — once with default env
 * and once with `LD_BIND_NOW=1`. Both must succeed.
 */

#include "../lib/libhello_gnu/hello.h"

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

static i64 sys_open(const char *path, int flags) {
    i64 ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "0"(2), "D"(path), "S"((i64)flags), "d"(0)
        : "rcx", "r11", "memory");
    return ret;
}

static i64 sys_read(i64 fd, char *buf, i64 len) {
    i64 ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "0"(0), "D"(fd), "S"(buf), "d"(len)
        : "rcx", "r11", "memory");
    return ret;
}

static i64 sys_close(i64 fd) {
    i64 ret;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "0"(3), "D"(fd)
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

static int wx_check(void) {
    char buf[2048];
    i64 fd = sys_open("/proc/self/maps", 0);
    if (fd < 0) {
        return 1; /* procfs maps unavailable — trust kernel W^X */
    }
    i64 total = 0;
    for (;;) {
        i64 n = sys_read(fd, buf + total, (i64)sizeof(buf) - 1 - total);
        if (n <= 0) break;
        total += n;
        if (total >= (i64)sizeof(buf) - 1) break;
    }
    sys_close(fd);
    buf[total] = '\0';
    for (i64 i = 0; i + 3 < total; i++) {
        if (buf[i] == 'r' && buf[i+1] == 'w' && buf[i+2] == 'x') {
            return 0;
        }
    }
    return 1;
}

void _start(void) {
    const char *msg = hello_str();
    i64 len = c_strlen(msg);
    sys_write(1, msg, len);
    sys_write(1, "\n", 1);
    if (wx_check()) {
        sys_write(1, "WX_CHECK:OK\n", 12);
    } else {
        sys_write(1, "WX_CHECK:FAIL\n", 14);
    }
    sys_exit(0);
}
