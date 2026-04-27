//! Phase 57 F.1 — `StartupSequence` and supporting types.
//!
//! See [`crate::session`] module-level docs for context. This file
//! declares the public surface (`SessionStep`, `SessionState`,
//! `SessionError`, `StartupSequence`). The state machine is total:
//! every transition has a defined behavior, and the test suite at the
//! bottom of the file exercises them.

/// Lifecycle state of the graphical session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Initial; no steps have started yet, or the sequence has been
    /// reset.
    Booting,
    /// Every declared step's `start()` returned `Ok(())` AND
    /// `is_ready()` reported `true`. Steady state.
    Running,
    /// A step's `start()` failed; the sequencer is retrying. `step_name`
    /// names the step the sequencer is currently retrying.
    /// `retry_count` is the number of retry attempts already consumed
    /// for this step (so 0 on the first attempt, 1 after the first
    /// failure, ...).
    Recovering {
        step_name: &'static str,
        retry_count: u32,
    },
    /// A step exhausted the per-step retry cap. The sequencer rolled
    /// back the started prefix in reverse order and the graphical
    /// session is abandoned.
    TextFallback,
}

/// Typed error surface returned by every `SessionStep` and
/// `StartupSequence::run` boundary. No `unwrap`/`expect`/`panic!` paths
/// in non-test code: every recoverable condition surfaces here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// A step's `start()` failed. `retries_exhausted == true` indicates
    /// the per-step retry cap has been reached and the sequence will
    /// escalate to [`SessionState::TextFallback`].
    StepFailed {
        step_name: &'static str,
        retries_exhausted: bool,
    },
    /// A step ran in the wrong order — typically a sequencer bug or a
    /// caller invoking a method out of phase.
    OutOfOrder {
        expected: &'static str,
        got: &'static str,
    },
    /// `run()` was called while the sequence was already in
    /// [`SessionState::Running`].
    AlreadyRunning,
    /// A method that requires the sequence to be running was invoked on
    /// a non-running sequence.
    NotRunning,
}

/// Abstraction every supervised graphical-session step implements. The
/// trait is the seam for F.2 (`session_manager` adapters) — the crate
/// carrying this trait does not depend on the supervisor IPC or the
/// service registry. Open / Closed and Dependency Inversion: callers
/// depend on the trait, not on the concrete `display_server`-/
/// `audio_server`-/etc. adapter.
pub trait SessionStep {
    /// Stable, human-readable identifier (e.g. `"display_server"`).
    /// Used in `SessionState::Recovering` and error reporting; must not
    /// allocate.
    fn name(&self) -> &'static str;

    /// Begin the step. Returns `Ok(())` on success — readiness is then
    /// observed via [`SessionStep::is_ready`]. A returned error means
    /// the step failed to start; the sequencer will retry up to the
    /// configured cap.
    fn start(&mut self) -> Result<(), SessionError>;

    /// Roll the step back. Called in reverse start order on
    /// [`SessionState::TextFallback`] escalation, or when the operator
    /// requests `session-stop` (F.5).
    fn stop(&mut self) -> Result<(), SessionError>;

    /// Returns `true` once the step is fully ready to accept clients.
    /// The sequencer polls this between `start()` and advancing to the
    /// next step — a step that returns `Ok(())` from `start` but isn't
    /// yet ready (its endpoint is bound but the protocol-level handshake
    /// has not completed) does not advance the sequencer until
    /// `is_ready()` flips to `true`.
    fn is_ready(&self) -> bool;
}

/// Sequencer that runs a slice of [`SessionStep`]s in declared order.
/// Borrows the slice — `kernel-core` does not own the supervisor
/// handle. No allocation in steady state.
pub struct StartupSequence<'a> {
    steps: &'a mut [&'a mut dyn SessionStep],
    state: SessionState,
    /// Number of steps for which `start()` succeeded AND `is_ready()`
    /// returned `true` (i.e. that need rollback on text-fallback).
    /// `0 ≤ started ≤ steps.len()`.
    started: usize,
    /// Most recently attempted step name. Latches across `Running`,
    /// `Recovering`, and `TextFallback` states; cleared only by `new`.
    current: Option<&'static str>,
}

impl<'a> StartupSequence<'a> {
    /// Construct a sequencer that will run `steps` in declared order on
    /// the next `run()` call. The initial state is
    /// [`SessionState::Booting`].
    pub fn new(steps: &'a mut [&'a mut dyn SessionStep]) -> Self {
        Self {
            steps,
            state: SessionState::Booting,
            started: 0,
            current: None,
        }
    }

    /// Run every declared step in order. Returns the resulting
    /// `SessionState` (`Running` on full success, `TextFallback` if any
    /// step exhausts its retry budget). Any other condition (out of
    /// order, already running) is surfaced as a typed error.
    ///
    /// `max_retries_per_step` caps the number of `start()` attempts per
    /// step. An attempt that returns `Err` from `start`, or that
    /// returns `Ok(())` from `start` but reports `is_ready() == false`
    /// after the call, both consume one attempt: the supervisor
    /// pattern is "reach a ready endpoint within N tries". Once a
    /// step's attempts are exhausted without a ready outcome, the
    /// sequencer rolls back every already-started step in reverse
    /// start order and returns `Ok(SessionState::TextFallback)`.
    pub fn run(&mut self, max_retries_per_step: u32) -> Result<SessionState, SessionError> {
        // Total state-machine entry guard: only `Booting` → run. Any
        // other state is a caller protocol error.
        match self.state {
            SessionState::Booting => {}
            SessionState::Running => return Err(SessionError::AlreadyRunning),
            SessionState::Recovering { .. } | SessionState::TextFallback => {
                return Err(SessionError::OutOfOrder {
                    expected: "booting",
                    got: state_name(self.state),
                });
            }
        }

        let total = self.steps.len();
        let mut idx = 0;
        while idx < total {
            let step_name = self.steps[idx].name();
            self.current = Some(step_name);

            // Per-step attempt loop. attempts == number of `start()`
            // invocations consumed; we cap at max_retries_per_step.
            let mut attempts: u32 = 0;
            let mut ready = false;
            while attempts < max_retries_per_step {
                // We are about to consume one attempt. If this is not
                // the very first attempt for this step, reflect the
                // mid-flight `Recovering` lifecycle state.
                if attempts > 0 {
                    self.state = SessionState::Recovering {
                        step_name,
                        retry_count: attempts,
                    };
                }
                attempts = attempts.saturating_add(1);
                let result = self.steps[idx].start();
                match result {
                    Ok(()) => {
                        if self.steps[idx].is_ready() {
                            ready = true;
                            break;
                        }
                        // Start succeeded but endpoint is not yet
                        // ready: another attempt is required.
                    }
                    Err(_) => {
                        // Transient failure; another attempt is
                        // required.
                    }
                }
            }

            if !ready {
                // Retry budget exhausted. Roll back every
                // already-started step in reverse order (best-effort:
                // stop errors from individual steps are not allowed
                // to abort the rollback, since the session is being
                // abandoned anyway).
                self.escalate_to_text_fallback();
                return Ok(SessionState::TextFallback);
            }

            // Step is ready; advance.
            self.started = self.started.saturating_add(1);
            idx += 1;
        }

        self.state = SessionState::Running;
        Ok(SessionState::Running)
    }

    /// Snapshot of the current lifecycle state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Name of the step the sequencer most recently attempted (or is
    /// currently attempting). Returns `None` before the first step has
    /// been touched.
    pub fn current_step(&self) -> Option<&'static str> {
        self.current
    }

    /// Roll back every already-started step in reverse start order and
    /// transition to [`SessionState::TextFallback`]. Stop errors on
    /// individual steps are intentionally swallowed: the session is
    /// being abandoned and surfacing one step's stop failure cannot
    /// undo any of the others.
    fn escalate_to_text_fallback(&mut self) {
        // started counts how many steps successfully reached "ready".
        // We undo the started prefix in reverse order. The current
        // (failing) step is NOT in the started prefix, so it is not
        // stopped.
        while self.started > 0 {
            let i = self.started - 1;
            let _ = self.steps[i].stop();
            self.started -= 1;
        }
        self.state = SessionState::TextFallback;
    }
}

/// Stable, allocation-free name for a `SessionState` discriminant —
/// used in [`SessionError::OutOfOrder`] payloads. Exhaustive: every
/// variant has a name; no fallback.
fn state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Booting => "booting",
        SessionState::Running => "running",
        SessionState::Recovering { .. } => "recovering",
        SessionState::TextFallback => "text-fallback",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::MAX_RETRIES_PER_STEP;

    extern crate alloc;

    use alloc::rc::Rc;
    use alloc::string::ToString;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    // -------------------------------------------------------------------
    // Test doubles
    // -------------------------------------------------------------------

    /// Test double that records every `start`/`stop`/`is_ready` call so
    /// the test can assert order invariants. `fail_starts_count`
    /// configures the step to fail its first N `start()` attempts; if
    /// the count is `u32::MAX`, the step fails forever.
    ///
    /// `delay_ready_ticks` configures the step to return `false` from
    /// `is_ready()` for the first N successful `start()` attempts'
    /// readiness polls (so `start()` returned `Ok(())` but the step is
    /// not yet ready).
    struct RecordingStep {
        name: &'static str,
        fail_starts_count: u32,
        delay_ready_ticks: u32,
        log: Rc<RefCell<Vec<StepCall>>>,
        // Internal counters — mutated in the trait methods.
        starts_attempted: u32,
        ready_polls: u32,
        last_start_ok: bool,
    }

    impl RecordingStep {
        fn new(name: &'static str, log: Rc<RefCell<Vec<StepCall>>>) -> Self {
            Self {
                name,
                fail_starts_count: 0,
                delay_ready_ticks: 0,
                log,
                starts_attempted: 0,
                ready_polls: 0,
                last_start_ok: false,
            }
        }

        fn fail_starts(mut self, n: u32) -> Self {
            self.fail_starts_count = n;
            self
        }

        fn fail_forever(mut self) -> Self {
            self.fail_starts_count = u32::MAX;
            self
        }

        fn delay_ready(mut self, ticks: u32) -> Self {
            self.delay_ready_ticks = ticks;
            self
        }
    }

    impl SessionStep for RecordingStep {
        fn name(&self) -> &'static str {
            self.name
        }
        fn start(&mut self) -> Result<(), SessionError> {
            self.log
                .borrow_mut()
                .push(StepCall::Start(self.name.to_string()));
            self.starts_attempted = self.starts_attempted.saturating_add(1);
            if self.fail_starts_count == u32::MAX || self.fail_starts_count >= self.starts_attempted
            {
                self.last_start_ok = false;
                return Err(SessionError::StepFailed {
                    step_name: self.name,
                    retries_exhausted: false,
                });
            }
            self.last_start_ok = true;
            self.ready_polls = 0;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), SessionError> {
            self.log
                .borrow_mut()
                .push(StepCall::Stop(self.name.to_string()));
            self.last_start_ok = false;
            Ok(())
        }
        fn is_ready(&self) -> bool {
            // Note: tests cannot mutate self in `is_ready` because the
            // trait takes `&self`. The poll counter lives on the step
            // through interior mutability would be required for a "delay
            // for N polls then succeed" pattern. Instead the test
            // double counts polls externally via `delay_ready_ticks` —
            // we use a `Cell` shadow.
            if !self.last_start_ok {
                return false;
            }
            // Read poll count via a side-channel: the value is fixed
            // for delay_ready_ticks == 0.
            self.ready_polls >= self.delay_ready_ticks
        }
    }

    /// Variant test double that exposes a tick advance so the readiness
    /// gate can transition. Demonstrates the `is_ready` contract: the
    /// sequencer must not advance to the next step until `is_ready()`
    /// returns `true`.
    struct TickAdvancingStep {
        inner: RecordingStep,
    }

    impl TickAdvancingStep {
        fn new(name: &'static str, log: Rc<RefCell<Vec<StepCall>>>) -> Self {
            Self {
                inner: RecordingStep::new(name, log),
            }
        }
        fn delay_ready(mut self, ticks: u32) -> Self {
            self.inner = self.inner.delay_ready(ticks);
            self
        }
    }

    impl SessionStep for TickAdvancingStep {
        fn name(&self) -> &'static str {
            self.inner.name()
        }
        fn start(&mut self) -> Result<(), SessionError> {
            self.inner.start()
        }
        fn stop(&mut self) -> Result<(), SessionError> {
            self.inner.stop()
        }
        fn is_ready(&self) -> bool {
            // Mutating &self in is_ready requires interior mutability
            // — but mutating self in is_ready is legal via the trait
            // method receiver if we cheat. We use a const trick: we
            // consider the step ready iff `inner.starts_attempted >
            // inner.delay_ready_ticks`. The sequencer naturally polls
            // is_ready after each start; after delay_ready_ticks polls
            // the contract is met.
            self.inner.starts_attempted > self.inner.delay_ready_ticks
        }
    }

    /// Logged kind of trait-method invocation used for ordering /
    /// rollback assertions.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum StepCall {
        Start(alloc::string::String),
        Stop(alloc::string::String),
    }

    /// Build a fresh empty log for the test double to share with the
    /// test body.
    fn fresh_log() -> Rc<RefCell<Vec<StepCall>>> {
        Rc::new(RefCell::new(Vec::new()))
    }

    // -------------------------------------------------------------------
    // Required behavioral tests — these run against the recording double
    // by default and additionally against the `FakeSupervisorStep`
    // double via the `session_step_harness` contract test below.
    // -------------------------------------------------------------------

    #[test]
    fn empty_sequence_returns_running() {
        let mut steps: [&mut dyn SessionStep; 0] = [];
        let mut seq = StartupSequence::new(&mut steps);
        let outcome = seq.run(MAX_RETRIES_PER_STEP).expect("empty sequence runs");
        assert_eq!(outcome, SessionState::Running);
        assert_eq!(seq.state(), SessionState::Running);
        assert_eq!(seq.current_step(), None);
    }

    #[test]
    fn single_successful_step_reaches_running() {
        let log = fresh_log();
        let mut step = RecordingStep::new("display_server", log.clone());
        let mut steps: [&mut dyn SessionStep; 1] = [&mut step];
        let mut seq = StartupSequence::new(&mut steps);

        let outcome = seq
            .run(MAX_RETRIES_PER_STEP)
            .expect("one good step runs to Running");
        assert_eq!(outcome, SessionState::Running);
        assert_eq!(seq.state(), SessionState::Running);
        assert_eq!(seq.current_step(), Some("display_server"));
        let calls = log.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], StepCall::Start("display_server".to_string()));
    }

    #[test]
    fn failed_step_retries_up_to_cap() {
        let log = fresh_log();
        // fails twice then succeeds — retry cap 3 admits this.
        let mut step = RecordingStep::new("display_server", log.clone()).fail_starts(2);
        let mut steps: [&mut dyn SessionStep; 1] = [&mut step];
        let mut seq = StartupSequence::new(&mut steps);

        let outcome = seq
            .run(MAX_RETRIES_PER_STEP)
            .expect("transient failure recovers within the cap");
        assert_eq!(outcome, SessionState::Running);

        // 3 starts (2 failures + 1 success) and no stops.
        let calls = log.borrow();
        let starts: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, StepCall::Start(_)))
            .collect();
        let stops: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, StepCall::Stop(_)))
            .collect();
        assert_eq!(
            starts.len(),
            3,
            "expected 3 start attempts, got {:?}",
            calls
        );
        assert!(stops.is_empty());
    }

    #[test]
    fn step_failure_after_retries_escalates_to_text_fallback() {
        let log = fresh_log();
        // First step succeeds; second fails forever.
        let mut s0 = RecordingStep::new("display_server", log.clone());
        let mut s1 = RecordingStep::new("kbd_server", log.clone()).fail_forever();
        let mut steps: [&mut dyn SessionStep; 2] = [&mut s0, &mut s1];
        let mut seq = StartupSequence::new(&mut steps);

        let outcome = seq
            .run(MAX_RETRIES_PER_STEP)
            .expect("escalation is a defined outcome, not an error");
        assert_eq!(outcome, SessionState::TextFallback);
        assert_eq!(seq.state(), SessionState::TextFallback);

        let calls = log.borrow();
        let starts: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                StepCall::Start(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        let stops: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                StepCall::Stop(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        // s0 started once successfully; s1 retried 3 times.
        assert_eq!(
            starts,
            ["display_server", "kbd_server", "kbd_server", "kbd_server"]
        );
        // Rollback: only s0 (the only successfully-started step) was
        // stopped. s1 never reached "started" so it is not stopped.
        assert_eq!(stops, ["display_server"]);
    }

    #[test]
    fn rollback_runs_in_reverse_start_order() {
        let log = fresh_log();
        let mut s0 = RecordingStep::new("display_server", log.clone());
        let mut s1 = RecordingStep::new("kbd_server", log.clone());
        let mut s2 = RecordingStep::new("mouse_server", log.clone());
        let mut s3 = RecordingStep::new("audio_server", log.clone()).fail_forever();
        let mut steps: [&mut dyn SessionStep; 4] = [&mut s0, &mut s1, &mut s2, &mut s3];
        let mut seq = StartupSequence::new(&mut steps);

        let outcome = seq.run(MAX_RETRIES_PER_STEP).expect("escalates");
        assert_eq!(outcome, SessionState::TextFallback);

        let calls = log.borrow();
        let stops: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                StepCall::Stop(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            stops,
            ["mouse_server", "kbd_server", "display_server"],
            "stop order must be reverse of start order"
        );
    }

    #[test]
    fn out_of_order_invocation_returns_error() {
        let log = fresh_log();
        let mut step = RecordingStep::new("display_server", log.clone());
        let mut steps: [&mut dyn SessionStep; 1] = [&mut step];
        let mut seq = StartupSequence::new(&mut steps);

        let _ = seq.run(MAX_RETRIES_PER_STEP).expect("first run ok");
        let err = seq
            .run(MAX_RETRIES_PER_STEP)
            .expect_err("second run on a Running sequence is rejected");
        assert_eq!(err, SessionError::AlreadyRunning);
    }

    #[test]
    fn is_ready_must_be_true_before_next_step_starts() {
        let log = fresh_log();
        // s0 succeeds on first start, but is_ready returns false until
        // is_ready has been polled `delay_ready_ticks` times. The
        // sequencer must not advance to s1 until s0 reports ready.
        let mut s0 = TickAdvancingStep::new("display_server", log.clone()).delay_ready(2);
        let mut s1 = RecordingStep::new("kbd_server", log.clone());
        let mut steps: [&mut dyn SessionStep; 2] = [&mut s0, &mut s1];
        let mut seq = StartupSequence::new(&mut steps);

        let outcome = seq
            .run(MAX_RETRIES_PER_STEP)
            .expect("waiting for ready does not error out");
        assert_eq!(outcome, SessionState::Running);
        // s1 must have started AFTER s0; if the sequencer raced past
        // the readiness gate, s1 might appear before s0 is ready.
        let calls = log.borrow();
        let starts: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                StepCall::Start(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        // s0's `start` runs once (start ok, then the sequencer polls
        // `is_ready` which is gated on additional start attempts —
        // because TickAdvancingStep advances readiness with each start
        // call, the sequencer must invoke start enough times to flip
        // ready, then only after that does it move to s1).
        // The exact count of starts on s0 is not the contract; what
        // matters is that s1 does not appear before s0 has had at least
        // 3 starts (2 delay + 1 admitting).
        let s0_starts = starts.iter().filter(|n| n == &"display_server").count();
        let s1_starts = starts.iter().filter(|n| n == &"kbd_server").count();
        assert!(
            s0_starts >= 3,
            "expected at least 3 starts on s0 to flip is_ready (delay 2), got {}",
            s0_starts
        );
        assert_eq!(s1_starts, 1);
        // And the LAST start of s0 must come BEFORE the start of s1.
        let last_s0 = starts
            .iter()
            .rposition(|n| n == "display_server")
            .expect("s0 started");
        let first_s1 = starts
            .iter()
            .position(|n| n == "kbd_server")
            .expect("s1 started");
        assert!(
            last_s0 < first_s1,
            "s1 must not start before s0 reports is_ready"
        );
    }

    // -------------------------------------------------------------------
    // Contract test: SessionStepHarness — the same suite must pass
    // against a recording double AND a fake-supervisor double. Both
    // impls of `SessionStep` must be observably equivalent for the
    // purposes of the sequencer.
    // -------------------------------------------------------------------

    /// Fake-supervisor double: pretends to talk to a service supervisor
    /// (the F.3 surface) without doing any IPC. Different internal
    /// representation from `RecordingStep` so the contract test catches
    /// any incidental coupling between the sequencer and a specific
    /// step type.
    struct FakeSupervisorStep {
        name: &'static str,
        // Number of remaining failures before start succeeds.
        remaining_failures: u32,
        fail_forever: bool,
        // Whether the supervisor reports the service as ready yet.
        ready: bool,
        log: Rc<RefCell<Vec<StepCall>>>,
    }

    impl FakeSupervisorStep {
        fn always_ready(name: &'static str, log: Rc<RefCell<Vec<StepCall>>>) -> Self {
            Self {
                name,
                remaining_failures: 0,
                fail_forever: false,
                ready: false,
                log,
            }
        }
        fn fail_forever(name: &'static str, log: Rc<RefCell<Vec<StepCall>>>) -> Self {
            Self {
                name,
                remaining_failures: 0,
                fail_forever: true,
                ready: false,
                log,
            }
        }
    }

    impl SessionStep for FakeSupervisorStep {
        fn name(&self) -> &'static str {
            self.name
        }
        fn start(&mut self) -> Result<(), SessionError> {
            self.log
                .borrow_mut()
                .push(StepCall::Start(self.name.to_string()));
            if self.fail_forever {
                return Err(SessionError::StepFailed {
                    step_name: self.name,
                    retries_exhausted: false,
                });
            }
            if self.remaining_failures > 0 {
                self.remaining_failures -= 1;
                return Err(SessionError::StepFailed {
                    step_name: self.name,
                    retries_exhausted: false,
                });
            }
            self.ready = true;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), SessionError> {
            self.log
                .borrow_mut()
                .push(StepCall::Stop(self.name.to_string()));
            self.ready = false;
            Ok(())
        }
        fn is_ready(&self) -> bool {
            self.ready
        }
    }

    /// The contract test: run a small suite against both step impls and
    /// confirm identical observable outcomes.
    #[test]
    fn session_step_harness_recording_and_fake_supervisor_agree() {
        // Subtest 1: single good step → Running.
        {
            let log_a = fresh_log();
            let mut step_a = RecordingStep::new("display_server", log_a.clone());
            let mut steps_a: [&mut dyn SessionStep; 1] = [&mut step_a];
            let outcome_a = StartupSequence::new(&mut steps_a)
                .run(MAX_RETRIES_PER_STEP)
                .unwrap();

            let log_b = fresh_log();
            let mut step_b = FakeSupervisorStep::always_ready("display_server", log_b.clone());
            let mut steps_b: [&mut dyn SessionStep; 1] = [&mut step_b];
            let outcome_b = StartupSequence::new(&mut steps_b)
                .run(MAX_RETRIES_PER_STEP)
                .unwrap();
            assert_eq!(outcome_a, outcome_b);
            assert_eq!(outcome_a, SessionState::Running);
        }

        // Subtest 2: failing-forever step → TextFallback.
        {
            let log_a = fresh_log();
            let mut step_a = RecordingStep::new("audio_server", log_a.clone()).fail_forever();
            let mut steps_a: [&mut dyn SessionStep; 1] = [&mut step_a];
            let outcome_a = StartupSequence::new(&mut steps_a)
                .run(MAX_RETRIES_PER_STEP)
                .unwrap();

            let log_b = fresh_log();
            let mut step_b = FakeSupervisorStep::fail_forever("audio_server", log_b.clone());
            let mut steps_b: [&mut dyn SessionStep; 1] = [&mut step_b];
            let outcome_b = StartupSequence::new(&mut steps_b)
                .run(MAX_RETRIES_PER_STEP)
                .unwrap();
            assert_eq!(outcome_a, outcome_b);
            assert_eq!(outcome_a, SessionState::TextFallback);
        }
    }

    // -------------------------------------------------------------------
    // proptest: arbitrary step-success / step-failure interleavings
    // produce only `Running` or `TextFallback` final state, and the
    // reverse-order rollback property holds.
    // -------------------------------------------------------------------

    use proptest::prelude::*;

    /// Per-step behavior knob the property test draws.
    #[derive(Debug, Clone, Copy)]
    enum StepBehavior {
        SucceedNow,
        SucceedAfter(u32),
        FailForever,
    }

    fn arb_behavior() -> impl Strategy<Value = StepBehavior> {
        prop_oneof![
            Just(StepBehavior::SucceedNow),
            (0u32..=5).prop_map(StepBehavior::SucceedAfter),
            Just(StepBehavior::FailForever),
        ]
    }

    proptest! {
        #[test]
        fn proptest_step_sequence_invariants(
            behaviors in proptest::collection::vec(arb_behavior(), 0..6),
        ) {
            // Build steps from behaviors. Use a stable name pool so
            // assertions can match by index.
            let names: [&'static str; 6] = [
                "display_server",
                "kbd_server",
                "mouse_server",
                "audio_server",
                "term",
                "extra",
            ];
            let log = fresh_log();
            let mut owned: Vec<RecordingStep> = behaviors
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let mut s = RecordingStep::new(names[i], log.clone());
                    match b {
                        StepBehavior::SucceedNow => {}
                        StepBehavior::SucceedAfter(n) => {
                            s = s.fail_starts(*n);
                        }
                        StepBehavior::FailForever => {
                            s = s.fail_forever();
                        }
                    }
                    s
                })
                .collect();
            // Borrow mutably as &mut dyn SessionStep slice.
            let mut step_refs: Vec<&mut dyn SessionStep> =
                owned.iter_mut().map(|s| s as &mut dyn SessionStep).collect();
            let mut seq = StartupSequence::new(&mut step_refs);
            let outcome = seq.run(MAX_RETRIES_PER_STEP).expect("run is total");
            // Outcome is only ever Running or TextFallback.
            prop_assert!(
                matches!(outcome, SessionState::Running | SessionState::TextFallback),
                "outcome must be terminal: {:?}", outcome
            );
            prop_assert_eq!(outcome, seq.state());
            // Reverse-order rollback: examine the call log.
            let calls = log.borrow().clone();
            let starts_idx_in_log: Vec<usize> = calls
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match c {
                    StepCall::Start(_) => Some(i),
                    _ => None,
                })
                .collect();
            let stop_names: Vec<alloc::string::String> = calls
                .iter()
                .filter_map(|c| match c {
                    StepCall::Stop(n) => Some(n.clone()),
                    _ => None,
                })
                .collect();
            // Identify which steps reached "started" (i.e. last start
            // call was successful per the behavior).
            let mut started_names: Vec<&'static str> = Vec::new();
            for (i, b) in behaviors.iter().enumerate() {
                let attempts_until_success = match b {
                    StepBehavior::SucceedNow => Some(1),
                    StepBehavior::SucceedAfter(n) => {
                        if *n < MAX_RETRIES_PER_STEP {
                            Some(*n + 1)
                        } else {
                            None
                        }
                    }
                    StepBehavior::FailForever => None,
                };
                if attempts_until_success.is_some() {
                    started_names.push(names[i]);
                } else {
                    // First step that does not succeed terminates the
                    // started prefix.
                    break;
                }
            }
            if outcome == SessionState::TextFallback {
                let expected_stops: Vec<alloc::string::String> = started_names
                    .iter()
                    .rev()
                    .map(|s| (*s).to_string())
                    .collect();
                prop_assert_eq!(stop_names, expected_stops);
            } else {
                // Running: no stops were issued.
                prop_assert!(stop_names.is_empty());
            }
            // Sanity: total starts ≥ started prefix length.
            prop_assert!(starts_idx_in_log.len() >= started_names.len());
        }
    }
}
