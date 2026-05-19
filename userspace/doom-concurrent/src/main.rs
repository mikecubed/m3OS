//! Phase 70 follow-up — `doom-concurrent`.
//!
//! Tiny harness helper that forks two `doom` processes back-to-back
//! (so both run concurrently under a single `display_server`) and then
//! waits for both children with `waitpid`.  Prints structural sentinels
//! the `cargo xtask doom-concurrent-smoke` gate matches against.
//!
//! ## Why a dedicated helper
//!
//! The in-tree shell (`userspace/shell/src/main.rs`) has no `&` job
//! control (the tokenizer treats `&` as a regular word character and
//! `execute_external` always `waitpid`s — see PR 179 round-3 review
//! feedback) and no `wait` builtin or `;` separator.  Trying to drive
//! "fork two DOOMs concurrently, wait for both" purely from shell
//! syntax (`doom ... &; doom ... &; wait`) silently degrades to a
//! sequential run, voiding the concurrency assertion the gate is
//! supposed to make.
//!
//! This binary owns the fork/waitpid lifecycle directly so the
//! kernel-level concurrency property is independent of any shell
//! parsing surface.
//!
//! ## Sentinels
//!
//! - `M3OS_DOOM_CONCURRENT:spawn=1` — printed after the first fork
//!   succeeds, before any waitpid runs.  Proves DOOM #1 was forked
//!   into a live process.
//! - `M3OS_DOOM_CONCURRENT:spawn=2` — printed after the second fork
//!   succeeds, before any waitpid runs.  Both children are running
//!   concurrently at this point.
//! - `CONCURRENT_DOOM_DONE=<status>` — printed after both children
//!   have been reaped.  `<status>` is the bitwise OR of the two
//!   `waitpid` status words masked to the exit-code byte; `0` means
//!   both children exited cleanly (no crash, no kill).
//!
//! The smoke gate asserts `spawn=1`, then `spawn=2`, then
//! `CONCURRENT_DOOM_DONE=0` — together these prove that both DOOMs
//! reached the waitpid sink without `display_server` getting wedged.

#![no_std]
#![no_main]

use syscall_lib::{STDOUT_FILENO, execve, exit, fork, waitpid, write_str, write_u64};

/// argv for each DOOM child. Both share `/usr/share/doom/doom1.wad`
/// (Phase 47 staging) and rely on the shared `/tmp/doom-autoquit-tics`
/// file the smoke gate writes before invocation so each instance shuts
/// itself down after a bounded frame budget.
///
/// Different `-warp <episode> <map>` targets keep the two instances
/// visually distinguishable in serial logs if a future debugging pass
/// needs to triage which DOOM produced a given frame.
const DOOM_PATH: &[u8] = b"/bin/doom\0";

const DOOM1_ARG_DOOM: &[u8] = b"doom\0";
const DOOM1_ARG_IWAD: &[u8] = b"-iwad\0";
const DOOM1_ARG_WADPATH: &[u8] = b"/usr/share/doom/doom1.wad\0";
const DOOM1_ARG_WARP: &[u8] = b"-warp\0";
const DOOM1_ARG_E: &[u8] = b"1\0";
const DOOM1_ARG_M1: &[u8] = b"1\0";
const DOOM1_ARG_M2: &[u8] = b"2\0";

fn fail(reason: &[u8]) -> ! {
    write_str(STDOUT_FILENO, "doom-concurrent: ");
    let _ = syscall_lib::write(STDOUT_FILENO, reason);
    write_str(STDOUT_FILENO, "\n");
    exit(2);
}

/// Fork a single DOOM child. Returns the child's PID in the parent;
/// in the child this function never returns (it execs).
fn fork_doom(map_arg: &[u8]) -> isize {
    let pid = fork();
    if pid < 0 {
        fail(b"fork failed");
    }
    if pid == 0 {
        // Child — exec into /bin/doom. argv must be null-pointer
        // terminated; the kernel's execve copies argv[] until it hits
        // the trailing null. envp is empty (just a null sentinel).
        let argv: [*const u8; 7] = [
            DOOM1_ARG_DOOM.as_ptr(),
            DOOM1_ARG_IWAD.as_ptr(),
            DOOM1_ARG_WADPATH.as_ptr(),
            DOOM1_ARG_WARP.as_ptr(),
            DOOM1_ARG_E.as_ptr(),
            map_arg.as_ptr(),
            core::ptr::null(),
        ];
        let envp: [*const u8; 1] = [core::ptr::null()];
        execve(DOOM_PATH, &argv, &envp);
        // execve only returns on failure.
        fail(b"execve /bin/doom failed");
    }
    pid
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Fork DOOM #1 first. Parent continues immediately to fork #2 —
    // both DOOMs are running concurrently before the first waitpid.
    let pid1 = fork_doom(DOOM1_ARG_M1);
    write_str(STDOUT_FILENO, "M3OS_DOOM_CONCURRENT:spawn=1 pid=");
    write_u64(STDOUT_FILENO, pid1 as u64);
    write_str(STDOUT_FILENO, "\n");

    let pid2 = fork_doom(DOOM1_ARG_M2);
    write_str(STDOUT_FILENO, "M3OS_DOOM_CONCURRENT:spawn=2 pid=");
    write_u64(STDOUT_FILENO, pid2 as u64);
    write_str(STDOUT_FILENO, "\n");

    // Both children are live; reap them. waitpid blocks until each
    // specific child changes state. If either DOOM wedges in
    // `display_server` (e.g. BlockedOnReply with no one to respond),
    // the corresponding waitpid blocks forever and the smoke gate's
    // global timeout fires — that is the structural assertion this
    // helper makes.
    let mut status1: i32 = 0;
    if waitpid(pid1 as i32, &mut status1, 0) < 0 {
        fail(b"waitpid(pid1) failed");
    }
    let mut status2: i32 = 0;
    if waitpid(pid2 as i32, &mut status2, 0) < 0 {
        fail(b"waitpid(pid2) failed");
    }

    // Extract the exit code byte from each status word — matches the
    // shell's decoder at `userspace/shell/src/main.rs:389`
    // (`(status >> 8) & 0xff`). A non-zero result means at least one
    // child crashed or was killed by a signal.
    let code1 = (status1 >> 8) & 0xff;
    let code2 = (status2 >> 8) & 0xff;
    let aggregated = code1 | code2;

    write_str(STDOUT_FILENO, "CONCURRENT_DOOM_DONE=");
    write_u64(STDOUT_FILENO, aggregated as u64);
    write_str(STDOUT_FILENO, "\n");

    exit(if aggregated == 0 { 0 } else { 1 });
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "doom-concurrent: PANIC\n");
    exit(101)
}
