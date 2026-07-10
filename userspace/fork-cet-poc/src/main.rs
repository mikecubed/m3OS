//! Fork CoW-of-shadow-stack stress PoC (Phase 110 Track B.3, Block 4a).
//!
//! Regression-confirms **Fix #5** of the 2026-07-09 CET bring-up: *fork must
//! eagerly copy shadow-stack pages, not share them.* A CET shadow-stack page is
//! deliberately non-writable (WRITABLE=0, DIRTY=1) yet it still mutates (a
//! `CALL` pushes a return address without trapping on WRITABLE=0). The generic
//! `cow_clone_user_pages` path shares non-writable pages verbatim, so before the
//! fix a fork parent and child *aliased one shadow-stack frame* — the first
//! post-fork `CALL`/`RET` chain in either process corrupted the other's shadow
//! stack and the mismatched `ret` raised `#CP` (the original `ion _Fork` kill,
//! handoff §0.6). Fix #5 detects a shadow-stack leaf and eagerly copies it so
//! parent and child get independent shadow stacks.
//!
//! This PoC forks several children; each — and the parent between spawns — runs
//! a deep-ish recursion so both actively push/pop their shadow stacks. If the
//! pages were shared, a child dies on a post-fork `RET` (`#CP`) and never prints
//! `child-ok`; the parent's reap then sees a *signal* wait-status instead of a
//! clean exit.
//!
//! - **CET on + Fix #5 (Tiger Lake, default image):** every child survives →
//!   `FORK_CET_POC:PASS`. The regression is closed.
//! - **CET on WITHOUT the fix:** ≥1 child is `#CP`-killed on its first post-fork
//!   `RET` → `FORK_CET_POC:FAIL` and a kernel `#CP … process killed` line.
//! - **CET off (QEMU / `mitigations=off`):** no shadow stacks exist, so this is
//!   just a fork+recurse smoke — it completes and prints PASS (run-to-completion
//!   proof only; the CET arm is bench-only).
//!
//! Bench arm: Block 4a of the 2026-07-09 Dell validation runbook.
#![no_std]
#![no_main]

use core::hint::black_box;
use syscall_lib::{STDOUT_FILENO, exit, fork, waitpid, write_str, write_u64};

syscall_lib::entry_point!(main);

/// Number of children to fork. Each is an independent post-fork shadow-stack
/// user; a shared page would corrupt across all of them.
const CHILDREN: usize = 8;
/// Recursion depth per round. Kept well under a single shadow-stack page (512
/// 8-byte slots / 4 KiB) so this never *overflows* the shadow stack — the test
/// is about page *independence* after fork, not depth. Tunable at the bench.
const DEPTH: u32 = 200;
/// Rounds of `recurse(DEPTH)` a child runs — churns the shadow stack hard so a
/// shared-page alias corrupts quickly rather than probabilistically.
const STRESS_ROUNDS: u32 = 32;

/// Non-tail recursion: the result is consumed *after* the recursive call, so a
/// real return address lives on both the data stack and the CET shadow stack for
/// every frame. `#[inline(never)]` + `black_box` defeat tail-call / inlining
/// optimizations that would erase the `CALL`/`RET` pairs we need.
#[inline(never)]
fn recurse(depth: u32) -> u64 {
    if depth == 0 {
        return black_box(0);
    }
    let deeper = recurse(depth - 1);
    black_box(deeper.wrapping_add(depth as u64))
}

#[inline(never)]
fn churn_shadow_stack(rounds: u32) {
    let mut acc = 0u64;
    for _ in 0..rounds {
        acc = acc.wrapping_add(recurse(DEPTH));
    }
    black_box(acc);
}

fn main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "FORK_CET_POC:before\n");

    let mut forked = 0usize;
    for _ in 0..CHILDREN {
        let pid = fork();
        if pid < 0 {
            write_str(STDOUT_FILENO, "FORK_CET_POC:fork-failed\n");
            break;
        }
        if pid == 0 {
            // Child. Its inherited shadow-stack pages must have been eagerly
            // copied (Fix #5); the first CALL in churn_shadow_stack forces the
            // duplication. A wrong copy => this RET chain #CP-kills the child, so
            // it never reaches the child-ok print and the parent reaps a signal
            // status.
            churn_shadow_stack(STRESS_ROUNDS);
            write_str(STDOUT_FILENO, "FORK_CET_POC:child-ok\n");
            exit(0);
        }
        // Parent: mutate our own (must-be-independent) shadow stack between
        // spawns, so a shared page would corrupt in both directions.
        churn_shadow_stack(4);
        forked += 1;
    }

    // Reap. Clean child => status==0 (WIFEXITED, code 0); a #CP-killed child has
    // (status & 0x7f) != 0 (terminating signal). Decoding matches
    // userspace/shell/src/main.rs.
    let mut survived = 0usize;
    let mut killed = 0usize;
    for _ in 0..forked {
        let mut status: i32 = 0;
        let w = waitpid(-1, &mut status, 0);
        if w <= 0 {
            continue;
        }
        if status & 0x7f == 0 && (status >> 8) & 0xff == 0 {
            survived += 1;
        } else {
            killed += 1;
        }
    }

    write_str(STDOUT_FILENO, "FORK_CET_POC:survived=");
    write_u64(STDOUT_FILENO, survived as u64);
    write_str(STDOUT_FILENO, " killed=");
    write_u64(STDOUT_FILENO, killed as u64);
    write_str(STDOUT_FILENO, "\n");

    if survived == CHILDREN && forked == CHILDREN {
        write_str(
            STDOUT_FILENO,
            "FORK_CET_POC:PASS all children survived fork+recurse — shadow-stack CoW is independent (Fix #5 holds)\n",
        );
        0
    } else {
        write_str(
            STDOUT_FILENO,
            "FORK_CET_POC:FAIL a forked child died on a post-fork RET — fork CoW-of-shadow-stack regression (check dmesg for '#CP … process killed')\n",
        );
        1
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "fork-cet-poc: PANIC\n");
    syscall_lib::exit(101)
}
