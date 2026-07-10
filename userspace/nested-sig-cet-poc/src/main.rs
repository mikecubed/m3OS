//! Nested-signal shadow-stack PoC (Phase 110 Track B.3, Block 4b).
//!
//! Exercises the CET risk the 2026-07-09 bring-up deferred (handoff §0.3 note +
//! §0.8 step 5) — **now fixed** (PR #328; the kernel sources the SSP from the
//! live `RDSSP` register at the seed + `sigreturn`, with no per-task slot). Prior
//! to the fix, signal delivery seeded the shadow stack via `WRUSS` (Fix #3) and
//! saved the interrupted SSP in a *single* `Task::cet_signal_ssp` slot: correct
//! for a non-nested handler, but a second signal taken *inside* a running handler
//! reused that slot — and (compounded by a stale `IA32_PL3_SSP` MSR on the
//! nested `SYSCALL` delivery) the process `#CP`-killed on real silicon. This PoC
//! stays as the regression witness: pre-fix it `#CP`-kills; post-fix it PASSes.
//!
//! We force the nesting without `sigprocmask`/`SIGALRM` (neither exists in
//! m3OS): install handlers for two *different* signals, raise the outer
//! (`SIGUSR1`) with `kill(self)`, and from inside that handler raise the inner
//! (`SIGUSR2`) with `kill(self)`. Because the inner signal differs from the one
//! being handled, default masking of `SIGUSR1` does not block it, so it is
//! delivered on the `kill` syscall's return — nested on top of the outer frame.
//!
//! - **PASS:** both handlers enter and both return, control reaches `main`
//!   after `kill(SIGUSR1)`, and no `#CP` fires → `NESTED_SIG_POC:PASS`. The
//!   single-slot design happens to unwind correctly for one level of nesting.
//! - **FAIL (the flagged risk realized):** the outer handler's `RET` `#CP`-kills
//!   the process — you'll see the kernel `#CP … process killed` line and NO
//!   `NESTED_SIG_POC:after`. That confirms the per-frame-SSP / `RSTORSSP`-token
//!   redesign is needed.
//! - **PARTIAL:** handlers ran and nothing crashed, but the print ordering shows
//!   the inner ran *after* the outer resumed (delivery was deferred, not truly
//!   nested) — the single slot was never double-used. Still a clean, recorded
//!   result; note the platform delivered sequentially.
//!
//! Read the interleaving of the printed lines to tell true nesting
//! (`outer-entered → inner-entered → inner-returning → outer-resumed`) from
//! deferred delivery (`outer-entered → outer-resumed → inner-entered`).
//!
//! Bench arm: Block 4b of the 2026-07-09 Dell validation runbook.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use syscall_lib::{
    SIGUSR1, SIGUSR2, STDOUT_FILENO, exit, getpid, kill, rt_sigaction_simple, write_str, write_u64,
};

syscall_lib::entry_point!(main);

/// Bit 0 = outer entered, bit 1 = inner entered, bit 2 = outer resumed.
static STAGE: AtomicU32 = AtomicU32::new(0);

/// Inner (nested) handler — raised from inside the outer handler.
extern "C" fn inner_handler(_sig: i32) {
    write_str(STDOUT_FILENO, "NESTED_SIG_POC:inner-entered\n");
    STAGE.fetch_or(0b010, Ordering::SeqCst);
    // This RET runs syscall-lib's sigrestorer -> rt_sigreturn. Under CET it is
    // shadow-stack-checked against the slot the kernel seeded at delivery. Pre-fix
    // this nested handler's own RET is where the bug surfaced (the seed used a
    // stale MSR on the nested SYSCALL delivery); the PR #328 live-RDSSP seed makes
    // it match.
    write_str(STDOUT_FILENO, "NESTED_SIG_POC:inner-returning\n");
}

/// Outer handler — raises the nested inner signal while running.
extern "C" fn outer_handler(_sig: i32) {
    write_str(STDOUT_FILENO, "NESTED_SIG_POC:outer-entered\n");
    STAGE.fetch_or(0b001, Ordering::SeqCst);
    let me = getpid() as i32;
    // Different signal number than the one being handled -> not masked -> nests
    // on this kill()'s syscall return.
    let _ = kill(me, SIGUSR2);
    // If SIGUSR2 nested, inner_handler already ran and returned by now.
    write_str(STDOUT_FILENO, "NESTED_SIG_POC:outer-resumed\n");
    STAGE.fetch_or(0b100, Ordering::SeqCst);
    // The outer handler's own final RET (after this returns) is where a clobbered
    // single-slot SSP surfaces as a #CP kill on CET silicon.
}

fn main(_args: &[&str]) -> i32 {
    if rt_sigaction_simple(SIGUSR1 as usize, outer_handler) < 0
        || rt_sigaction_simple(SIGUSR2 as usize, inner_handler) < 0
    {
        write_str(
            STDOUT_FILENO,
            "NESTED_SIG_POC:FAIL could not install handlers\n",
        );
        exit(1);
    }

    write_str(STDOUT_FILENO, "NESTED_SIG_POC:before\n");
    let me = getpid() as i32;
    // Deliver the outer signal; SIGUSR2 nests inside its handler.
    let _ = kill(me, SIGUSR1);
    // Reaching here means BOTH handlers returned without a #CP kill.
    write_str(STDOUT_FILENO, "NESTED_SIG_POC:after\n");

    let stage = STAGE.load(Ordering::SeqCst);
    if stage == 0b111 {
        write_str(
            STDOUT_FILENO,
            "NESTED_SIG_POC:PASS both handlers entered and returned, no #CP (single-slot SSP survived one nesting level)\n",
        );
    } else {
        write_str(STDOUT_FILENO, "NESTED_SIG_POC:PARTIAL stage=");
        write_u64(STDOUT_FILENO, stage as u64);
        write_str(
            STDOUT_FILENO,
            " (handlers ran, no #CP, but delivery was not fully nested)\n",
        );
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "nested-sig-cet-poc: PANIC\n");
    syscall_lib::exit(101)
}
