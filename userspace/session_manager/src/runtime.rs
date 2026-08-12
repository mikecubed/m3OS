//! Phase 64 — production syscall-backed adapters for the host-tested
//! `lifecycle` state machines.
//!
//! `lifecycle.rs` declares three pure-logic seams — `KernelClock`,
//! `SignalSink`, and `Reaper` — that let the stop state machine be
//! exercised under `cargo test` without booting QEMU. This module
//! provides the three production implementations the binary wires
//! into the daemon's event loop:
//!
//! - [`SyscallClock`] — `clock_gettime(CLOCK_MONOTONIC)` → milliseconds.
//! - [`SyscallSignalSink`] — `sys_kill(pid, sig)`.
//! - [`KillProbeReaper`] — non-blocking `kill(pid, 0)` probe: when the
//!   PID no longer exists in the kernel's task table, the call returns
//!   `-ESRCH`. `session_manager` is **not** the parent of the
//!   supervised children (init is, via its existing
//!   manifest-driven boot), so `waitpid` is not available — only init
//!   can reap. The `kill(pid, 0)` probe is the standard idiom for "is
//!   this PID still alive?" and is what production session managers in
//!   environments without strict parent/child reaping use.
//!
//! The deviation from the Phase 64 task list's "drain `sys_waitpid(-1,
//! ..., WNOHANG)`" wording is intentional: the task list reads as if
//! `session_manager` were the parent of its supervised children, but
//! in this codebase init is the parent. Adopting `waitpid` would
//! require either (a) moving every `display_server` / `kbd_server` /
//! `mouse_server` / `audio_server` / `term` `fork`+`execve` from init
//! into `session_manager` (a much larger blast-radius change), or
//! (b) introducing init→session_manager exit-notification IPC. The
//! `kill(0)` probe is functionally equivalent for Phase 64's stop and
//! restart-budget contracts — the only piece of information it cannot
//! recover is the child's exit code, which neither acceptance item nor
//! production user surfaces consume.
//!
//! ## Why three separate types
//!
//! Each adapter is a unit struct with no mutable state, so the daemon
//! can borrow them all simultaneously from the event loop without an
//! interior-mutability dance. Each implements one trait; SRP is
//! observed at the struct level.

use core::sync::atomic::{AtomicU64, Ordering};

use session_manager::lifecycle::{KernelClock, ReapOutcome, Reaper, SignalError, SignalSink};
use session_manager::table::Pid;

/// Production [`KernelClock`] implementation. Reads the monotonic
/// kernel clock via `clock_gettime` and converts to milliseconds.
///
/// On a transient `clock_gettime` failure (`tv_sec < 0`) the
/// implementation falls back to a monotonically-increasing internal
/// counter so the stop state machine's `deadline_ms` comparisons keep
/// making forward progress. Returning a constant `0` would freeze the
/// grace and reap windows in [`crate::stop_service_blocking`] and hang
/// the supervisor indefinitely — that's worse than a clock that ticks
/// at the poll cadence.
pub struct SyscallClock;

/// Fallback monotonic counter (milliseconds) used when `clock_gettime`
/// reports a transport-level failure. Increments by the stop-machine
/// poll interval on each fallback observation; the resulting virtual
/// clock advances strictly forward so [`lifecycle::tick`]'s
/// `now >= deadline_ms` checks still resolve.
static FALLBACK_NOW_MS: AtomicU64 = AtomicU64::new(0);

/// Step size used by the fallback clock. Derived from
/// [`STOP_POLL_INTERVAL_NS`] so the two cadences cannot drift apart if
/// the poll interval is retuned later. Floor-1 ms so the fallback
/// always makes forward progress even if the poll interval were ever
/// reduced below one millisecond.
const FALLBACK_STEP_MS: u64 = {
    let ms = (STOP_POLL_INTERVAL_NS as u64) / 1_000_000;
    if ms == 0 { 1 } else { ms }
};

impl KernelClock for SyscallClock {
    fn now_ms(&self) -> u64 {
        let (tv_sec, tv_nsec) = syscall_lib::clock_gettime(syscall_lib::CLOCK_MONOTONIC);
        if tv_sec < 0 {
            // Transport-level failure: advance the fallback counter so
            // the stop machine's deadlines still elapse deterministically.
            return FALLBACK_NOW_MS.fetch_add(FALLBACK_STEP_MS, Ordering::Relaxed)
                + FALLBACK_STEP_MS;
        }
        let s = tv_sec as u64;
        let ns = tv_nsec.max(0) as u64;
        let real_ms = s.saturating_mul(1_000).saturating_add(ns / 1_000_000);
        // Keep the fallback counter pinned at-or-above the real clock so
        // a subsequent failure cannot regress time below the value the
        // state machine already observed.
        let mut cur = FALLBACK_NOW_MS.load(Ordering::Relaxed);
        while real_ms > cur {
            match FALLBACK_NOW_MS.compare_exchange_weak(
                cur,
                real_ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
        real_ms
    }
}

/// Production [`SignalSink`] implementation. Wraps `syscall_lib::kill`
/// and maps a negative errno (`-ESRCH`, `-EPERM`, etc.) to a typed
/// [`SignalError`]. The state machine's deadline-driven escalation
/// handles the failure modes uniformly.
pub struct SyscallSignalSink;

impl SignalSink for SyscallSignalSink {
    fn send_signal(&mut self, pid: Pid, sig: i32) -> Result<(), SignalError> {
        let rc = syscall_lib::kill(pid.0, sig);
        if rc < 0 { Err(SignalError) } else { Ok(()) }
    }
}

/// Production [`Reaper`] implementation backed by `kill(pid, 0)`.
/// See the module-level doc comment for the rationale on using the
/// kill-probe rather than `waitpid`.
///
/// `kill(pid, 0)` returns `0` if the process exists, `-ESRCH` (`-3`)
/// if it has exited, and `-EPERM` (`-1`) if the caller lacks
/// permission. Since `session_manager` is part of the same boot
/// world as its children (root-uid), `-EPERM` should not occur in
/// production; it would still surface as `Error` to be conservative.
pub struct KillProbeReaper;

/// Errno returned by `kill(pid, 0)` when no process with the given
/// PID exists.  The kernel encodes errno as a negative `isize`.
const ESRCH_NEG: isize = -3;

impl Reaper for KillProbeReaper {
    fn try_reap(&mut self, pid: Pid) -> ReapOutcome {
        let rc = syscall_lib::kill(pid.0, 0);
        match rc {
            0 => ReapOutcome::NotYet,
            x if x == ESRCH_NEG => ReapOutcome::Reaped { exit_code: 0 },
            // Any other errno (typically -EPERM) is unexpected for a
            // root-owned session manager. Surface it as an error so
            // the lifecycle state machine returns `ReapFailed` rather
            // than spinning indefinitely.
            _ => ReapOutcome::Error,
        }
    }
}

// ===========================================================================
// Public synchronous wrapper around the host-tested state machine.
// ===========================================================================

use session_manager::lifecycle::{StopError, begin_stop, tick};
use syscall_lib::{STDOUT_FILENO, nanosleep_for, write_str};

/// Idle-tick poll interval used by [`stop_service_blocking`] when the
/// child has not yet exited. 25 ms balances responsiveness (a quick
/// SIGTERM-respecting child wakes the next tick) against not burning
/// CPU during the 5 s grace window. The full Phase 64 design defers
/// the IPC reply across event-loop ticks; this binary uses a simpler
/// synchronous spin-with-sleep until the more elaborate deferred-reply
/// machinery lands.
const STOP_POLL_INTERVAL_NS: u32 = 25_000_000;

/// Drive the host-tested [`session_manager::lifecycle::StopMachine`]
/// to completion using the production syscall-backed adapters in this
/// module. Returns `Ok(())` on a clean stop (whether via SIGTERM or
/// SIGKILL escalation) and a typed error on transport failure.
///
/// Phase 64 simplification: this helper polls in a `nanosleep` loop
/// rather than driving the state machine across the daemon's main
/// event-loop ticks. The trade-off is that one in-flight stop briefly
/// stalls other IPC for up to `STOP_POLL_INTERVAL_NS`; the daemon
/// resumes servicing other verbs once the stop completes. A future
/// follow-up can hoist the polling into the main event loop without
/// changing the lifecycle's pure-logic contract.
pub fn stop_service_blocking(pid: Pid) -> Result<(), StopError> {
    let clock = SyscallClock;
    let mut sink = SyscallSignalSink;
    let mut reaper = KillProbeReaper;

    let mut machine = begin_stop(&clock, &mut sink, pid)?;
    write_str(
        STDOUT_FILENO,
        "session_manager: lifecycle.stop: SIGTERM delivered\n",
    );

    loop {
        let done = tick(&mut machine, &clock, &mut sink, &mut reaper)?;
        if done {
            write_str(
                STDOUT_FILENO,
                "session_manager: lifecycle.stop: child reaped\n",
            );
            return Ok(());
        }
        // Brief sleep so we don't burn CPU between ticks. The state
        // machine's grace + reap deadlines are in seconds, so this
        // 25 ms cadence is well below the smallest deadline.
        let _ = nanosleep_for(0, STOP_POLL_INTERVAL_NS);
    }
}
