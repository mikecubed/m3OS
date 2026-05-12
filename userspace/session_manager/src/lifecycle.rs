//! Phase 64 Tracks B + C — `stop_service` and `restart_service`.
//!
//! Real lifecycle methods that replace the Phase 57 stubs (which
//! returned `Ack` unconditionally). `stop_service` is driven as a
//! state machine across event-loop iterations so the daemon never
//! suspends — other IPC continues to be serviced while a stop is in
//! flight. `restart_service` chains `stop` + `start` and enforces the
//! [`kernel_core::session::MAX_RETRIES_PER_STEP`] and
//! [`kernel_core::session::MAX_RESTART_COUNT`] budgets; budget
//! exhaustion on a [`DISPLAY_CRITICAL_SERVICES`] entry triggers the
//! text-fallback recovery motion.
//!
//! ## Pure-logic seams
//!
//! Three traits abstract the side effects so the state machine is
//! host-testable without QEMU:
//!
//! - [`KernelClock::now_ms`] reads the supervisor's monotonic time
//!   source. Production: `syscall_lib::clock_gettime`. Test: a fake
//!   that returns whatever the test advances.
//! - [`SignalSink::send_signal`] delivers `sys_kill`. Production:
//!   `syscall_lib::kill`. Test: a fake that records calls and lets
//!   the test verify the issued signal.
//! - [`Reaper::try_reap`] non-blockingly polls `sys_waitpid(pid, ..,
//!   WNOHANG)`. Production: `syscall_lib::waitpid`. Test: a fake that
//!   returns `NotYet` until the test marks the child as exited.
//!
//! The state machine itself ([`StopMachine::tick`]) consumes all three
//! traits and is the only thing the lifecycle methods need to be
//! correct — the syscall wiring is in `main.rs` and is intentionally
//! thin.

use crate::table::{Pid, ServiceState, ServiceTable};
use kernel_core::session::{MAX_RESTART_COUNT, MAX_RETRIES_PER_STEP};

/// Grace period between SIGTERM delivery and the SIGKILL escalation.
/// Matches the Phase 64 design doc (5 seconds). A child that does not
/// exit within this window receives SIGKILL.
pub const SIGTERM_GRACE_MS: u64 = 5_000;

/// Maximum time to wait for `sys_waitpid` to observe a reap after
/// SIGKILL has been delivered. A SIGKILL that does not produce a reap
/// within this window is treated as a kernel-side failure and the
/// stop request returns [`StopError::KillFailed`].
pub const SIGKILL_REAP_MS: u64 = 1_000;

/// Services whose budget exhaustion triggers
/// [`recover::run_text_fallback`]. The graphical session cannot run
/// without any of these three, so a permanently-failing one regresses
/// the session to text mode rather than leaving the user staring at a
/// dead framebuffer.
///
/// `audio_server` and `term` are intentionally absent: an audio
/// failure is annoying but not session-killing, and `term` failing
/// keeps the session running for any other surface clients.
pub const DISPLAY_CRITICAL_SERVICES: &[&str] = &["display_server", "kbd_server", "mouse_server"];

/// Signal numbers used by the stop state machine. Mirrored from
/// `syscall_lib::SIGTERM` / `syscall_lib::SIGKILL` so this module is
/// `kernel-core`-only — the syscall_lib types are not available in
/// the host-test build path.
pub const SIGTERM: i32 = 15;
pub const SIGKILL: i32 = 9;

/// Wall-clock source for the stop state machine. The trait is the
/// pure-logic seam that lets host tests inject a clock the test
/// controls; production wires it to `syscall_lib::clock_gettime`.
pub trait KernelClock {
    /// Monotonic time in milliseconds. Need not be wall-time; the
    /// state machine only ever computes deltas.
    fn now_ms(&self) -> u64;
}

/// Signal-delivery seam. Production: `syscall_lib::kill`. Test: a
/// recording fake.
pub trait SignalSink {
    /// Send `sig` to `pid`. Return `Ok(())` if the kernel accepted
    /// the request; `Err(())` is treated by the state machine as a
    /// transport-level failure that aborts the stop.
    fn send_signal(&mut self, pid: Pid, sig: i32) -> Result<(), ()>;
}

/// Non-blocking reap seam. Production: `syscall_lib::waitpid(pid, &,
/// WNOHANG)`. Test: a fake that returns `NotYet` until the test marks
/// the child as exited.
pub trait Reaper {
    /// Try to reap `pid`. `NotYet` means the child has not exited;
    /// `Reaped(code)` means it did and the wait status is `code`;
    /// `Error` means the kernel returned an unexpected status.
    fn try_reap(&mut self, pid: Pid) -> ReapOutcome;
}

/// Outcome of one [`Reaper::try_reap`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapOutcome {
    /// Child has not yet exited; the stop state machine should keep
    /// ticking on the next event-loop iteration.
    NotYet,
    /// Child has exited; `exit_code` is the wait status word.
    Reaped { exit_code: i32 },
    /// `sys_waitpid` returned an unexpected error code. The state
    /// machine surfaces this as [`StopError::ReapFailed`].
    Error,
}

/// One stop attempt's state. The machine drives across event-loop
/// iterations: an iteration that finds `SentTerm` polls the reaper and
/// may transition into `SentKill` once `deadline_ms <= now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopState {
    /// SIGTERM has been sent; the machine is waiting for the child to
    /// exit cleanly. The `deadline_ms` is the wall-clock instant at
    /// which the machine will escalate to SIGKILL.
    SentTerm { deadline_ms: u64 },
    /// SIGKILL has been sent after the grace period elapsed. The
    /// `deadline_ms` bounds the post-SIGKILL reap window.
    SentKill { deadline_ms: u64 },
    /// Child has been reaped. Terminal state; the state machine no
    /// longer ticks once it reaches this value.
    Reaped { exit_code: i32 },
}

/// Errors returned by [`StopMachine::tick`]. The state machine itself
/// never panics; every failure becomes one of these variants and the
/// caller (the deferred-reply machinery in `main.rs`) maps them to a
/// typed IPC error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopError {
    /// `SignalSink::send_signal` failed when issuing SIGTERM. The
    /// child is presumed still alive.
    TermFailed,
    /// `SignalSink::send_signal` failed when issuing SIGKILL after
    /// the grace period elapsed.
    KillFailed,
    /// The post-SIGKILL reap window elapsed without a reap. This
    /// almost certainly indicates a kernel-side bug.
    ReapFailed,
    /// The caller asked to stop a service with no live PID.
    NoSuchPid,
}

/// One `stop()` in flight.
///
/// Constructed by [`begin_stop`] when the operator (or the recovery
/// motion) invokes `stop_service`. Driven by [`tick`] on each
/// event-loop iteration until [`StopState::Reaped`] is reached.
#[derive(Debug, Clone, Copy)]
pub struct StopMachine {
    pub pid: Pid,
    pub state: StopState,
}

impl StopMachine {
    /// Whether the machine has reached its terminal state and the
    /// caller can resolve the deferred IPC reply.
    pub fn is_done(&self) -> bool {
        matches!(self.state, StopState::Reaped { .. })
    }
}

/// Begin a new stop on `pid`: send SIGTERM, return the in-flight
/// [`StopMachine`]. Errors if the signal cannot be delivered (e.g.
/// the PID no longer exists in the kernel's process table).
pub fn begin_stop<C: KernelClock, S: SignalSink>(
    clock: &C,
    sink: &mut S,
    pid: Pid,
) -> Result<StopMachine, StopError> {
    sink.send_signal(pid, SIGTERM)
        .map_err(|_| StopError::TermFailed)?;
    let deadline_ms = clock.now_ms() + SIGTERM_GRACE_MS;
    Ok(StopMachine {
        pid,
        state: StopState::SentTerm { deadline_ms },
    })
}

/// Drive the state machine one tick. Called on every event-loop
/// iteration that might service the in-flight stop (typically once
/// per SIGCHLD `Notification` wake-up, plus once per idle tick to
/// catch the grace-period expiry even when no SIGCHLD arrives).
///
/// Returns `Ok(true)` if the machine transitioned to
/// [`StopState::Reaped`] this tick; `Ok(false)` if it is still in
/// progress; `Err` on a permanent failure.
pub fn tick<C: KernelClock, S: SignalSink, R: Reaper>(
    machine: &mut StopMachine,
    clock: &C,
    sink: &mut S,
    reaper: &mut R,
) -> Result<bool, StopError> {
    if machine.is_done() {
        return Ok(true);
    }

    // Always poll the reaper first — a child can exit between SIGTERM
    // and the deadline check, in which case we transition straight to
    // `Reaped` without ever escalating to SIGKILL.
    match reaper.try_reap(machine.pid) {
        ReapOutcome::Reaped { exit_code } => {
            machine.state = StopState::Reaped { exit_code };
            return Ok(true);
        }
        ReapOutcome::Error => return Err(StopError::ReapFailed),
        ReapOutcome::NotYet => {}
    }

    // Child has not yet exited; check the per-state deadline.
    let now = clock.now_ms();
    match machine.state {
        StopState::SentTerm { deadline_ms } if now >= deadline_ms => {
            sink.send_signal(machine.pid, SIGKILL)
                .map_err(|_| StopError::KillFailed)?;
            machine.state = StopState::SentKill {
                deadline_ms: now + SIGKILL_REAP_MS,
            };
            Ok(false)
        }
        StopState::SentKill { deadline_ms } if now >= deadline_ms => {
            // Post-SIGKILL reap window elapsed. Treat as a kernel-side
            // failure; the caller logs and reports `ReapFailed` to
            // the operator.
            Err(StopError::ReapFailed)
        }
        // Otherwise the deadline has not yet elapsed; continue ticking.
        _ => Ok(false),
    }
}

// ===========================================================================
// Track C — restart_service with budget enforcement
// ===========================================================================

/// Errors returned by [`record_restart_attempt`] and consumed by the
/// daemon's restart loop. `BudgetExhausted` is the gate that escalates
/// the service to `ServiceState::Failed` and (for
/// `DISPLAY_CRITICAL_SERVICES`) triggers text-fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartError {
    /// The full-restart counter reached [`MAX_RESTART_COUNT`] or the
    /// in-attempt step-failure counter reached [`MAX_RETRIES_PER_STEP`].
    /// The caller transitions the service to `ServiceState::Failed`
    /// and, if it is in [`DISPLAY_CRITICAL_SERVICES`], invokes
    /// text-fallback.
    BudgetExhausted,
    /// `restart()` was issued for an unknown service.
    UnknownService,
}

/// Outcome of one step inside a restart attempt — used by the daemon
/// to feed the budget counters in [`record_restart_attempt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartStep {
    /// The stop or start call succeeded this attempt.
    Success,
    /// The stop or start call failed this attempt.
    Failure,
}

/// Budget-aware bookkeeping for one restart attempt.
///
/// Called by the daemon's restart loop after each `stop` / `start`
/// step. Increments `step_failures` on `Failure`; on a successful full
/// restart (the start step succeeded) increments `restart_count` and
/// clears `step_failures`. Returns `Err(BudgetExhausted)` once either
/// counter reaches its budget.
///
/// Splitting this from the syscall-driven loop keeps the policy
/// host-testable without weaving in `fork` / `execve`.
pub fn record_restart_attempt(
    table: &mut ServiceTable,
    service: &str,
    step: RestartStep,
) -> Result<(), RestartError> {
    if table.get(service).is_none() {
        return Err(RestartError::UnknownService);
    }
    match step {
        RestartStep::Failure => {
            let new_steps = table
                .bump_step_failures(service)
                .ok_or(RestartError::UnknownService)?;
            if new_steps >= MAX_RETRIES_PER_STEP {
                table.update_state(service, ServiceState::Failed);
                return Err(RestartError::BudgetExhausted);
            }
            Ok(())
        }
        RestartStep::Success => {
            // A successful restart bumps the steady-state counter and
            // resets the in-attempt counter. The order matters: bump
            // FIRST so the budget check sees the new value.
            let new_restarts = table
                .bump_restart_count(service)
                .ok_or(RestartError::UnknownService)?;
            table.clear_step_failures(service);
            if new_restarts >= MAX_RESTART_COUNT {
                table.update_state(service, ServiceState::Failed);
                return Err(RestartError::BudgetExhausted);
            }
            Ok(())
        }
    }
}

/// True if `service` is in [`DISPLAY_CRITICAL_SERVICES`]. The caller
/// uses this gate to decide whether a `BudgetExhausted` outcome should
/// fire text-fallback.
pub fn is_display_critical(service: &str) -> bool {
    DISPLAY_CRITICAL_SERVICES.iter().any(|s| *s == service)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    // -----------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------

    struct FakeClock {
        now: core::cell::Cell<u64>,
    }
    impl FakeClock {
        fn new() -> Self {
            Self {
                now: core::cell::Cell::new(1_000),
            }
        }
        fn advance(&self, ms: u64) {
            self.now.set(self.now.get() + ms);
        }
    }
    impl KernelClock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
    }

    #[derive(Default)]
    struct FakeSink {
        sent: Vec<(Pid, i32)>,
        reject_next: bool,
    }
    impl SignalSink for FakeSink {
        fn send_signal(&mut self, pid: Pid, sig: i32) -> Result<(), ()> {
            if self.reject_next {
                self.reject_next = false;
                return Err(());
            }
            self.sent.push((pid, sig));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeReaper {
        // Returns NotYet until `ready` is true.
        ready: bool,
        // Drives ReapOutcome::Error one tick.
        error: bool,
        // Returned exit code once `ready` is set.
        exit_code: i32,
    }
    impl Reaper for FakeReaper {
        fn try_reap(&mut self, _pid: Pid) -> ReapOutcome {
            if self.error {
                return ReapOutcome::Error;
            }
            if self.ready {
                ReapOutcome::Reaped {
                    exit_code: self.exit_code,
                }
            } else {
                ReapOutcome::NotYet
            }
        }
    }

    // -----------------------------------------------------------------
    // Track B.1 acceptance: at least three host-side unit tests against
    // a mock KernelClock + SignalSink + Reaper:
    //   - normal SIGTERM exit
    //   - grace-period expiry → SIGKILL
    //   - nonexistent PID (immediate Err)
    // -----------------------------------------------------------------

    #[test]
    fn normal_sigterm_exit_reaps_without_escalation() {
        let clock = FakeClock::new();
        let mut sink = FakeSink::default();
        let mut reaper = FakeReaper::default();
        let pid = Pid(42);

        let mut m = begin_stop(&clock, &mut sink, pid).expect("SIGTERM ok");
        assert_eq!(sink.sent, [(pid, SIGTERM)]);
        assert!(matches!(m.state, StopState::SentTerm { .. }));

        // Child exits before the grace deadline.
        reaper.ready = true;
        reaper.exit_code = 0;
        let done = tick(&mut m, &clock, &mut sink, &mut reaper).expect("tick ok");
        assert!(done);
        assert!(m.is_done());
        assert_eq!(m.state, StopState::Reaped { exit_code: 0 });
        // SIGKILL was NOT issued — only the original SIGTERM.
        assert_eq!(sink.sent, [(pid, SIGTERM)]);
    }

    #[test]
    fn grace_period_expiry_escalates_to_sigkill_and_then_reaps() {
        let clock = FakeClock::new();
        let mut sink = FakeSink::default();
        let mut reaper = FakeReaper::default();
        let pid = Pid(99);

        let mut m = begin_stop(&clock, &mut sink, pid).expect("SIGTERM ok");
        assert!(matches!(m.state, StopState::SentTerm { .. }));

        // Tick once before grace expiry: still SentTerm, no SIGKILL.
        let done = tick(&mut m, &clock, &mut sink, &mut reaper).expect("tick ok");
        assert!(!done);
        assert!(matches!(m.state, StopState::SentTerm { .. }));
        assert_eq!(sink.sent, [(pid, SIGTERM)]);

        // Advance past the grace window. Reaper still says NotYet.
        clock.advance(SIGTERM_GRACE_MS + 1);
        let done = tick(&mut m, &clock, &mut sink, &mut reaper).expect("escalation ok");
        assert!(!done);
        assert!(matches!(m.state, StopState::SentKill { .. }));
        assert_eq!(sink.sent, [(pid, SIGTERM), (pid, SIGKILL)]);

        // Child finally exits inside the SIGKILL reap window.
        reaper.ready = true;
        let done = tick(&mut m, &clock, &mut sink, &mut reaper).expect("reap ok");
        assert!(done);
        assert!(m.is_done());
    }

    #[test]
    fn nonexistent_pid_returns_immediate_err() {
        let clock = FakeClock::new();
        let mut sink = FakeSink {
            reject_next: true,
            ..FakeSink::default()
        };
        let pid = Pid(1234);

        let err = begin_stop(&clock, &mut sink, pid).unwrap_err();
        assert_eq!(err, StopError::TermFailed);
        assert!(sink.sent.is_empty());
    }

    #[test]
    fn sigkill_reap_window_exhaustion_returns_kill_failed() {
        let clock = FakeClock::new();
        let mut sink = FakeSink::default();
        let mut reaper = FakeReaper::default();
        let pid = Pid(7);

        let mut m = begin_stop(&clock, &mut sink, pid).unwrap();
        // Advance past grace, tick → SIGKILL.
        clock.advance(SIGTERM_GRACE_MS + 1);
        tick(&mut m, &clock, &mut sink, &mut reaper).unwrap();
        assert!(matches!(m.state, StopState::SentKill { .. }));

        // Advance past the SIGKILL reap window without reaping.
        clock.advance(SIGKILL_REAP_MS + 1);
        let err = tick(&mut m, &clock, &mut sink, &mut reaper).unwrap_err();
        assert_eq!(err, StopError::ReapFailed);
    }

    #[test]
    fn reaper_error_surfaces_as_reap_failed() {
        let clock = FakeClock::new();
        let mut sink = FakeSink::default();
        let mut reaper = FakeReaper {
            error: true,
            ..FakeReaper::default()
        };
        let mut m = begin_stop(&clock, &mut sink, Pid(1)).unwrap();
        let err = tick(&mut m, &clock, &mut sink, &mut reaper).unwrap_err();
        assert_eq!(err, StopError::ReapFailed);
    }

    // -----------------------------------------------------------------
    // Track C.1 — restart-budget enforcement
    // -----------------------------------------------------------------

    fn table_with(name: &str) -> ServiceTable {
        let mut t = ServiceTable::new();
        t.insert(name);
        t.update_state(name, ServiceState::Running);
        t
    }

    #[test]
    fn step_failures_below_budget_succeed() {
        let mut t = table_with("display_server");
        for _ in 0..(MAX_RETRIES_PER_STEP - 1) {
            assert_eq!(
                record_restart_attempt(&mut t, "display_server", RestartStep::Failure),
                Ok(())
            );
        }
        // The state has not yet been transitioned to Failed.
        assert_eq!(t.get_state("display_server"), Some(ServiceState::Running));
    }

    #[test]
    fn step_failures_reaching_budget_transition_to_failed() {
        let mut t = table_with("display_server");
        // Bump MAX_RETRIES_PER_STEP times.
        for _ in 0..(MAX_RETRIES_PER_STEP - 1) {
            assert_eq!(
                record_restart_attempt(&mut t, "display_server", RestartStep::Failure),
                Ok(())
            );
        }
        // Final increment hits the budget.
        assert_eq!(
            record_restart_attempt(&mut t, "display_server", RestartStep::Failure),
            Err(RestartError::BudgetExhausted)
        );
        assert_eq!(t.get_state("display_server"), Some(ServiceState::Failed));
    }

    #[test]
    fn restart_count_at_budget_transitions_to_failed() {
        let mut t = table_with("display_server");
        for i in 1..MAX_RESTART_COUNT {
            assert_eq!(
                record_restart_attempt(&mut t, "display_server", RestartStep::Success),
                Ok(())
            );
            assert_eq!(t.get("display_server").unwrap().restart_count, i);
        }
        // The MAX_RESTART_COUNTth success exhausts the budget.
        assert_eq!(
            record_restart_attempt(&mut t, "display_server", RestartStep::Success),
            Err(RestartError::BudgetExhausted)
        );
        assert_eq!(t.get_state("display_server"), Some(ServiceState::Failed));
    }

    #[test]
    fn success_clears_step_failures() {
        let mut t = table_with("audio_server");
        record_restart_attempt(&mut t, "audio_server", RestartStep::Failure).unwrap();
        assert_eq!(t.get("audio_server").unwrap().step_failures, 1);
        record_restart_attempt(&mut t, "audio_server", RestartStep::Success).unwrap();
        assert_eq!(t.get("audio_server").unwrap().step_failures, 0);
        assert_eq!(t.get("audio_server").unwrap().restart_count, 1);
    }

    #[test]
    fn unknown_service_returns_unknown_service() {
        let mut t = ServiceTable::new();
        assert_eq!(
            record_restart_attempt(&mut t, "nonexistent", RestartStep::Failure),
            Err(RestartError::UnknownService)
        );
    }

    #[test]
    fn display_critical_predicate_covers_expected_set() {
        assert!(is_display_critical("display_server"));
        assert!(is_display_critical("kbd_server"));
        assert!(is_display_critical("mouse_server"));
        assert!(!is_display_critical("audio_server"));
        assert!(!is_display_critical("term"));
    }
}
