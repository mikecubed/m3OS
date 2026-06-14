//! Track D — `kstack-overflow-test`.
//!
//! Kernel-level regression for the controlled-kill recovery of a
//! userspace-task-attributable **kernel-stack overflow**
//! (docs/handoffs/2026-06-14-claude-smp-tlb-shootdown-kstack-panic.md, Track D).
//!
//! A child process invokes `SYS_KSTACK_OVERFLOW_TEST` (0x1150, present only when
//! the kernel is built with the `kstack-overflow-test` feature), which recurses
//! until it exhausts its per-task kernel stack and hits the slot's guard page.
//! The kernel must turn the resulting ring-0 #PF/#DF into a **SIGSEGV of the
//! child** (not a core wedge / machine halt). The parent then:
//!   - asserts the child was killed by signal 11 (`KSTACK_OVF:killed:ok`), and
//!   - keeps running afterwards (`KSTACK_OVF:survivor:ok`) — the survival proof:
//!     on single-core, if the child's overflow had wedged the core in
//!     `hlt_loop`, the scheduler would never run the parent again and `waitpid`
//!     would hang the whole test. `waitpid` returning at all IS the proof the
//!     core recovered.
//!
//! Sentinels: `KSTACK_OVF:begin` / `:child:overflowing` / `:killed:ok` /
//! `:survivor:ok` / `:done` on success; `:skip ...` when the feature is absent
//! (the syscall returns ENOSYS, so the child exits 42 instead of faulting);
//! `:FAIL <reason>` (exit 2) / `:panic` (exit 101) on failure.
//!
//! Uses raw syscalls (no libc), like `pku-smoke` / `wx-violation`.

#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, exit, syscall0, syscall4, write};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    let _ = write(STDOUT_FILENO, b"KSTACK_OVF:panic\n");
    exit(101)
}

const SYS_FORK: u64 = 57;
const SYS_WAITPID: u64 = 61;

/// m3OS-native Track D probe syscall (mirrors the kernel-side constant). Returns
/// `-ENOSYS` when the kernel lacks the `kstack-overflow-test` feature.
const SYS_KSTACK_OVERFLOW_TEST: u64 = 0x1150;

/// Exit code the child uses to signal "syscall returned (feature absent)".
const SKIP_EXIT_CODE: i32 = 42;

const SIGSEGV_SIGNALLED: i32 = 11;

fn fork() -> i64 {
    unsafe { syscall0(SYS_FORK) as i64 }
}

fn waitpid(pid: i64) -> i32 {
    let mut status: i32 = 0;
    // wait4(pid, &status, 0, rusage=0). Pass all four args so r10 (rusage) is a
    // defined 0 — see the identical note in pku-smoke.
    let _ = unsafe {
        syscall4(
            SYS_WAITPID,
            pid as u64,
            &mut status as *mut i32 as u64,
            0,
            0,
        )
    };
    status
}

fn emit(msg: &[u8]) {
    let _ = write(STDOUT_FILENO, msg);
}

fn fail(reason: &[u8]) -> ! {
    emit(b"KSTACK_OVF:FAIL ");
    emit(reason);
    emit(b"\n");
    exit(2)
}

// Naked `_start` trampoline; this binary ignores argv/envp (mirrors pku-smoke).
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym kstack_overflow_main,
    );
}

fn kstack_overflow_main() -> ! {
    emit(b"KSTACK_OVF:begin\n");

    let child = fork();
    if child < 0 {
        fail(b"fork() failed");
    }
    if child == 0 {
        // Child: deliberately overflow the kernel stack. If the feature is built
        // in, this never returns (the kernel SIGSEGVs us). If it is absent, the
        // syscall returns -ENOSYS and we exit with the SKIP code.
        emit(b"KSTACK_OVF:child:overflowing\n");
        let _ = unsafe { syscall0(SYS_KSTACK_OVERFLOW_TEST) };
        exit(SKIP_EXIT_CODE);
    }

    // Parent: reap the child and classify how it died.
    let status = waitpid(child);
    let termsig = status & 0x7f;
    let exited = termsig == 0;
    let exit_code = (status >> 8) & 0xff;

    if exited && exit_code == SKIP_EXIT_CODE {
        // Kernel built without the feature — the probe syscall ENOSYS'd. Nothing
        // to assert; report a clean SKIP so a default-feature run is benign.
        emit(b"KSTACK_OVF:skip (kernel built without kstack-overflow-test feature)\n");
        emit(b"KSTACK_OVF:done\n");
        exit(0);
    }

    if termsig != SIGSEGV_SIGNALLED {
        // Killed by the wrong signal, or exited normally — recovery did not
        // produce the expected SIGSEGV.
        fail(b"child not killed by SIGSEGV (recovery path did not fire)");
    }
    emit(b"KSTACK_OVF:killed:ok\n");

    // Survival proof: the parent is still scheduling and running after the
    // child's kstack overflow + kill. On single-core, reaching here at all means
    // the core recovered rather than wedging in hlt_loop.
    emit(b"KSTACK_OVF:survivor:ok\n");
    emit(b"KSTACK_OVF:done\n");
    exit(0)
}
