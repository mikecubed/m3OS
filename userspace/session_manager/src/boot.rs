//! Phase 57 Track F.2 — boot adapters for `session_manager`.
//!
//! This module owns one concern: drive the F.1
//! [`kernel_core::session::StartupSequence`] through the declared
//! graphical-session step order, with each step delegating to a F.3
//! [`kernel_core::session_supervisor::SupervisorBackend`].
//!
//! ## SOLID
//!
//! - **SRP.** Each `ServiceStep` impl is a small struct that holds
//!   one service name + a borrow of the shared backend. One impl per
//!   service (no big match in the sequencer hot path). The `start` /
//!   `stop` / `is_ready` methods translate F.3 verb results into F.1
//!   `SessionError` values.
//! - **OCP / DI.** `ServiceStep` consumes the `SupervisorBackend`
//!   trait, not a concrete type. Tests substitute an in-memory fake;
//!   production substitutes the file-based init adapter.
//! - **YAGNI.** No multi-session, no idle timeout, no per-step custom
//!   policy. The only knob is the order, which is fixed by A.4.
//!
//! ## Phase 57 transitional behaviour
//!
//! Tracks D (`audio_server`) and G (`term`) land later than F.2.
//! Until they exist, their corresponding `start()` calls return
//! `SessionError::StepFailed { retries_exhausted: false }` for the
//! first 3 attempts; the F.1 sequencer then escalates to
//! `SessionState::TextFallback` with a clean rollback. Once Tracks D
//! and G land, the steps will succeed and the same boot path reaches
//! `SessionState::Running` without changing this module.

use kernel_core::session::{SessionError, SessionStep};
use kernel_core::session_supervisor::{
    SupervisorBackend, SupervisorReply, declared_session_step_names,
};

/// Number of declared session steps (mirrors the length of
/// [`declared_session_step_names`]). Hard-coded so the array shape is
/// known at compile time; the runtime check below asserts the constant
/// matches the slice.
pub const SESSION_STEP_COUNT: usize = 5;

/// One supervised step in the graphical-session boot sequence. Holds
/// only the service name and a shared mutable borrow of the backend
/// (via `RefCell` because the F.1 trait's `is_ready` takes `&self`).
///
/// SRP: every responsibility lives in one method:
/// - `start` → backend.start + readiness probe
/// - `stop`  → backend.stop
/// - `is_ready` → backend.await_ready (zero timeout = nonblocking probe)
pub struct ServiceStep<'b, B: SupervisorBackend> {
    name: &'static str,
    backend: &'b core::cell::RefCell<&'b mut B>,
}

impl<'b, B: SupervisorBackend> ServiceStep<'b, B> {
    /// Construct a step bound to `name` and the shared backend.
    pub fn new(name: &'static str, backend: &'b core::cell::RefCell<&'b mut B>) -> Self {
        Self { name, backend }
    }
}

impl<'b, B: SupervisorBackend> SessionStep for ServiceStep<'b, B> {
    fn name(&self) -> &'static str {
        self.name
    }

    fn start(&mut self) -> Result<(), SessionError> {
        let mut b = self.backend.borrow_mut();
        match b.start(self.name) {
            Ok(SupervisorReply::Ack) => Ok(()),
            // Backend reported a typed error (UnknownService, etc.).
            // Translate to a F.1 step-failure so the sequencer counts
            // it against the per-step retry budget.
            Ok(SupervisorReply::Error(_)) => Err(SessionError::StepFailed {
                step_name: self.name,
                retries_exhausted: false,
            }),
            // Other reply shapes (ReadyState, ExitObserved) are not
            // expected from `start` — treat as a step failure so the
            // sequencer surfaces them as a startup error rather than
            // silently advancing.
            Ok(_) => Err(SessionError::StepFailed {
                step_name: self.name,
                retries_exhausted: false,
            }),
            Err(_) => Err(SessionError::StepFailed {
                step_name: self.name,
                retries_exhausted: false,
            }),
        }
    }

    fn stop(&mut self) -> Result<(), SessionError> {
        let mut b = self.backend.borrow_mut();
        // Stop errors during rollback are intentionally swallowed at the
        // F.1 sequencer level (see `escalate_to_text_fallback`); we
        // still call through so the backend records the attempt.
        let _ = b.stop(self.name);
        Ok(())
    }

    fn is_ready(&self) -> bool {
        let mut b = self.backend.borrow_mut();
        matches!(
            b.await_ready(self.name, 0),
            Ok(SupervisorReply::ReadyState { ready: true })
        )
    }
}

/// Construct an array of ServiceSteps, one per declared session step,
/// all sharing `backend_cell`. Returns the array by value so callers
/// can build a `&mut [&mut dyn SessionStep]` view via `split_at_mut`.
///
/// The returned array's element order matches
/// [`declared_session_step_names`].
pub fn build_session_steps<'b, B: SupervisorBackend>(
    backend_cell: &'b core::cell::RefCell<&'b mut B>,
) -> [ServiceStep<'b, B>; SESSION_STEP_COUNT] {
    // Compile-time invariant: the constant must match the slice length.
    // The const-eval doesn't cover this directly because `SESSION_STEP_COUNT`
    // is intended to *match* the slice; we assert at runtime.
    let names = declared_session_step_names();
    assert!(
        names.len() == SESSION_STEP_COUNT,
        "SESSION_STEP_COUNT must equal declared_session_step_names().len()"
    );
    [
        ServiceStep::new(names[0], backend_cell),
        ServiceStep::new(names[1], backend_cell),
        ServiceStep::new(names[2], backend_cell),
        ServiceStep::new(names[3], backend_cell),
        ServiceStep::new(names[4], backend_cell),
    ]
}
