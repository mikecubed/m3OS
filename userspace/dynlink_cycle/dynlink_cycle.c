/*
 * dynlink_cycle — Phase 76b F1.4 negative gate (cycle).
 *
 * Built dynamic with `DT_NEEDED = libcyca.so`, where libcyca.so has
 * `DT_NEEDED = libcycb.so` and libcycb.so has
 * `DT_NEEDED = libcyca.so`. At runtime the bring-up linker walks the
 * dependency graph, runs `topo_sort` over it, detects the
 * `libcyca ↔ libcycb` cycle, and exits with `exit(80)` (ELIBBAD).
 * The smoke gate asserts `WEXITSTATUS(status) == 80`.
 */

extern int cyca_func(void);

static void sys_exit(long code) {
    __asm__ volatile(
        "syscall"
        :
        : "a"(60), "D"(code)
        : "rcx", "r11", "memory");
    __builtin_unreachable();
}

void _start(void) {
    /* Unreachable — the linker exits before transferring control. */
    sys_exit(99 + cyca_func());
}
