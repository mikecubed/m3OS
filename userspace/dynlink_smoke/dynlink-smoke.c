/*
 * Phase 76 — dynamic-linker scaffolding smoke test.
 *
 * This binary is the minimum end-to-end proof that the kernel's
 * `PT_INTERP` branch hands control to the dynamic linker stub
 * (`/lib/ld-musl-x86_64.so.1`), which then transfers to our `_start`
 * via the auxv's `AT_ENTRY` slot. We print a fixed sentinel to
 * stderr (fd 2, the serial console) and exit cleanly.
 *
 * Deliberate omissions, all of which Phase 76b will close:
 *
 *   - No `<unistd.h>` / `<stdio.h>` includes — we go straight to
 *     `syscall` so the binary has zero `DT_NEEDED` entries. ld.so
 *     in Phase 76 does not yet resolve any DT_NEEDED, so any libc
 *     reference would break the smoke gate.
 *
 *   - No `_init` / `_fini` / constructor calls — ld.so in Phase 76
 *     does not yet walk `DT_INIT_ARRAY`.
 *
 *   - No `main` — `_start` is the binary's entry point and the
 *     linker is built with `-nostdlib -nostartfiles` so crt1.o is
 *     not pulled in.
 *
 * Built by `cargo xtask`'s `build_dynlink_smoke` helper with
 * `musl-gcc -pie -nostdlib -nostartfiles
 *   -Wl,-dynamic-linker=/lib/ld-musl-x86_64.so.1`.
 *
 * The sentinel goes to fd 2 (serial / stderr) rather than fd 1 so
 * the smoke runner's serial-log pattern match works even when the
 * binary runs under a shell whose stdout is redirected.
 */

#include <stddef.h>

static long sys_write(long fd, const char *buf, long len) {
    long ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(1), "D"(fd), "S"(buf), "d"(len)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static void sys_exit(long code) {
    __asm__ volatile (
        "syscall"
        :
        : "a"(60), "D"(code)
        : "rcx", "r11", "memory"
    );
    __builtin_unreachable();
}

/* Sentinel pattern the smoke runner looks for on the serial log. */
static const char MSG[] = "DYNLINK_SMOKE:PASS\n";

void _start(void) {
    sys_write(2, MSG, sizeof(MSG) - 1);
    sys_exit(0);
}
