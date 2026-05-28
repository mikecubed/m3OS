/*
 * dynlink_hello_gnu — Phase 76d.F end-to-end smoke binary.
 *
 * Built with --hash-style=gnu, links libhello_gnu.so. Exercises
 * Phase 76d.D1's GNU-hash backend, B4's PLT lazy resolve trampoline
 * (F.2 GOT-mutation assertion), E4's LD_BIND_NOW (F.3), and F.4's
 * W^X invariant via /proc/self/maps.
 *
 *   * Sentinel `HELLO_FROM_GNU_LIB:OK` — basic GNU-hash path works.
 *   * Sentinel `BIND_NOW:0` or `BIND_NOW:1` — F.2's GOT[3] mutation
 *     assertion. The lazy path patches `GOT[3]` on first call; the
 *     eager path pre-resolves at load. Differs across the run, gate
 *     asserts BIND_NOW:0 (default env) and BIND_NOW:1 (LD_BIND_NOW=1).
 *   * Sentinel `WX_CHECK:OK` (F.4) — no `rwx` line in /proc/self/maps.
 *
 * `_GLOBAL_OFFSET_TABLE_` is loaded via inline-asm `lea` rather than
 * a C array reference. The compiler's default reference (`extern u64
 * _GLOBAL_OFFSET_TABLE_[]`) emits a `mov -4(%rip), %rbx`
 * GOTPCREL-style load under this build's `-no-pie -fno-pie -Wl,-pie
 * -nostartfiles` flag combo — that load is broken because it would
 * itself need the GOT to be set up. The explicit `lea` bypasses the
 * GOT and computes the address PC-relatively.
 */

#include "../lib/libhello_gnu/hello.h"

typedef unsigned long u64;
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

/* PC-relative load of `_GLOBAL_OFFSET_TABLE_`. Bypasses the
 * compiler's broken GOTPCREL handling for `extern u64
 * _GLOBAL_OFFSET_TABLE_[]` under our build flags. */
static u64 *got_base(void) {
    u64 *p;
    __asm__("leaq _GLOBAL_OFFSET_TABLE_(%%rip), %0" : "=r"(p));
    return p;
}

void _start(void) {
    /* F.2 — snapshot GOT[3] BEFORE first call. With Phase 76d's
     * lazy default this holds (load_bias + plt_fallback_offset);
     * after the first call the trampoline patches it to the
     * absolute `hello_str` address. */
    u64 *got = got_base();
    volatile u64 got_before = got[3];

    const char *msg = hello_str();

    volatile u64 got_after = got[3];

    i64 len = c_strlen(msg);
    sys_write(1, msg, len);
    sys_write(1, "\n", 1);

    /* F.2 + F.3 — GOT[3] mutation tells us whether the trampoline
     * ran. Lazy default: mutated (BIND_NOW:0). Eager via env: stable
     * (BIND_NOW:1). */
    if (got_before == got_after) {
        sys_write(1, "BIND_NOW:1\n", 11);
    } else {
        sys_write(1, "BIND_NOW:0\n", 11);
    }

    if (wx_check()) {
        sys_write(1, "WX_CHECK:OK\n", 12);
    } else {
        sys_write(1, "WX_CHECK:FAIL\n", 14);
    }

    sys_exit(0);
}
