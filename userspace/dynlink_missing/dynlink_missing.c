/*
 * dynlink_missing — Phase 76b F1.4 negative gate.
 *
 * Built dynamic with `DT_NEEDED = libdoesnotexist.so`. At runtime
 * the bring-up linker tries to open `/usr/lib/libdoesnotexist.so`,
 * gets ENOENT from the kernel, and exits with `exit(2)` (ENOENT).
 * The smoke gate asserts `WEXITSTATUS(status) == 2`.
 *
 * `dynlink_missing` itself never gets to `_start` — the linker
 * exits before transferring control. The body of `_start` below is
 * therefore unreachable but the symbol must exist so the linker
 * link step produces a valid PIE ELF.
 */

extern int doesnotexist_sym;

static void sys_exit(long code) {
    __asm__ volatile(
        "syscall"
        :
        : "a"(60), "D"(code)
        : "rcx", "r11", "memory");
    __builtin_unreachable();
}

void _start(void) {
    /* If we ever reach here the linker did NOT fail — exit
     * with a distinguishable code so the gate sees a failure. */
    sys_exit(99 + (int)(unsigned long)&doesnotexist_sym);
}
