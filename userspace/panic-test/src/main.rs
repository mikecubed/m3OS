//! Phase 99 Track C.1 — `panic-test`.
//!
//! End-to-end demonstration of the panic-path **AP-quiesce**
//! (`smp::panic_quiesce_aps`): the 2026-06-05 4 GiB handoff's blocking ask was a
//! *readable* (uninterleaved) `KERNEL PANIC at …` banner, because without
//! quiescing sibling cores the banner comes out byte-interleaved with whatever
//! the other cores are writing to COM1.
//!
//! This probe makes that contention deterministic: it forks several children
//! that spam COM1 (`PTSPAM`) forever on sibling cores, lets them saturate the
//! UART, then invokes `SYS_PANIC_TEST` (0x1151, present only when the kernel is
//! built with the `panic-test` feature) on the parent core. That syscall
//! `panic!()`s through the REAL `handle_panic` → `panic_quiesce_aps` path, which
//! must NMI-park the spammers' cores BEFORE printing — so the banner +
//! `PANICTEST_SENTINEL` message land contiguous on a now-quiet bus. The
//! `panic-test-smoke` gate captures serial and asserts no `PTSPAM` interleaves
//! the banner.
//!
//! Sentinels: `PANICTEST:begin` / `PANICTEST:triggering` then the kernel's
//! `KERNEL PANIC at …` + `PANICTEST_SENTINEL …` (machine halts after). When the
//! feature is absent the syscall returns `-ENOSYS` → `PANICTEST:skip` (exit 42).
//!
//! Uses raw syscalls (no libc), like `kstack-overflow-test` / `pku-smoke`.

#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, exit, nanosleep_for, syscall0, write};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    let _ = write(STDOUT_FILENO, b"PANICTEST:panic\n");
    exit(101)
}

const SYS_FORK: u64 = 57;

/// m3OS-native Track C.1 demo syscall (mirrors the kernel-side constant). Returns
/// `-ENOSYS` when the kernel lacks the `panic-test` feature.
const SYS_PANIC_TEST: u64 = 0x1151;

/// Exit code the parent uses to signal "syscall returned (feature absent)".
const SKIP_EXIT_CODE: i32 = 42;

/// Number of sibling-core COM1 spammers to fork. Several so that on `-smp 8` the
/// scheduler spreads them across cores other than the panicking parent's.
const SPAMMERS: usize = 6;

fn emit(msg: &[u8]) {
    let _ = write(STDOUT_FILENO, msg);
}

// Naked `_start` trampoline; this binary ignores argv/envp (mirrors pku-smoke).
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym panic_test_main,
    );
}

fn panic_test_main() -> ! {
    emit(b"PANICTEST:begin\n");

    // Fork sibling-core COM1 spammers so the AP-quiesce has something to silence.
    for _ in 0..SPAMMERS {
        let pid = unsafe { syscall0(SYS_FORK) } as i64;
        if pid == 0 {
            // Child: spam COM1 forever. These run on sibling cores; the kernel
            // panic below NMI-parks those cores, freezing this loop — which is
            // exactly the quiesce the gate verifies. (Killed at machine halt.)
            loop {
                let _ = write(STDOUT_FILENO, b"PTSPAM");
            }
        }
        // Parent keeps forking the rest.
    }

    // Let the spammers saturate the UART before we panic.
    let _ = nanosleep_for(0, 200_000_000); // 200 ms

    emit(b"\nPANICTEST:triggering\n");

    // Trigger the deliberate panic. With the `panic-test` feature this NEVER
    // returns: `handle_panic` quiesces the spammers' cores, then prints the
    // banner + `PANICTEST_SENTINEL` on a quiet COM1, then halts. Without the
    // feature the syscall returns `-ENOSYS` and we fall through to skip.
    let _ = unsafe { syscall0(SYS_PANIC_TEST) };

    emit(b"PANICTEST:skip feature-absent\n");
    exit(SKIP_EXIT_CODE);
}
