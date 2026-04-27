//! Phase 57 Track F.2 — `session_manager` boot ordering pure-logic tests.
//!
//! These tests pin the boot-adapter contract that the `session_manager`
//! daemon (a userspace `no_std` binary) implements. The host-testable
//! seam is the `BootSession` struct: it consumes an
//! [`kernel_core::session_supervisor::SupervisorBackend`] (F.3) and
//! drives a [`kernel_core::session::StartupSequence`] (F.1) through the
//! declared graphical-session order:
//!
//! `display_server → kbd_server → mouse_server → audio_server → term`
//!
//! Per the F.2 acceptance:
//!
//! - On boot, `session_manager` consumes the F.1 `StartupSequence` and
//!   runs the declared graphical-session steps in order.
//! - For Phase 57 F.2, `audio_server` and `term` may not yet have
//!   userspace binaries (Tracks D and G land later). The transitional
//!   contract: their step's `start()` returns
//!   `SessionError::StepFailed` for the first 3 attempts so the system
//!   escalates to `text-fallback` cleanly when those services are
//!   missing. Once Tracks D and G land, the steps will succeed.

use kernel_core::session::{MAX_RETRIES_PER_STEP, SessionState};
use kernel_core::session_supervisor::{
    SupervisorBackend, SupervisorError, SupervisorReply,
};

// ---------------------------------------------------------------------------
// The shape under test — `BootSession`.
//
// Implementation lives at
// `userspace/session_manager/src/boot.rs::BootSession`, but the
// pure-logic boot order policy + adapter trait shape are part of
// kernel-core via this test's import path. This is intentional: F.2's
// "boot ordering" rule belongs once in pure logic.
// ---------------------------------------------------------------------------

#[test]
fn declared_session_steps_match_a4_memo_order() {
    // The Phase 57 A.4 memo fixes the order:
    // display_server → kbd_server → mouse_server → audio_server → term.
    let names = kernel_core::session_supervisor::declared_session_step_names();
    assert_eq!(
        names,
        &[
            "display_server",
            "kbd_server",
            "mouse_server",
            "audio_server",
            "term",
        ]
    );
}

#[test]
fn happy_path_with_supervisor_backend_reaches_running() {
    // Backend reports every service as ready immediately. The
    // sequencer should reach `Running`.
    let mut backend = AlwaysReadyBackend::default();
    let outcome = run_session(&mut backend);
    assert_eq!(outcome, SessionState::Running);
    assert_eq!(
        backend.start_calls,
        &[
            "display_server",
            "kbd_server",
            "mouse_server",
            "audio_server",
            "term",
        ]
    );
}

#[test]
fn missing_audio_server_escalates_to_text_fallback() {
    // Phase 57 F.2 transitional: `audio_server` does not yet exist;
    // the step fails forever (mirrors what the production adapter
    // returns when the service is not registered). The retry budget of
    // 3 is exhausted and the sequencer escalates to text-fallback.
    let mut backend = MissingAudioBackend::default();
    let outcome = run_session(&mut backend);
    assert_eq!(outcome, SessionState::TextFallback);
    // display, kbd, mouse all succeeded; audio retried 3 times; term
    // never started.
    assert_eq!(
        backend.start_calls,
        &[
            "display_server",
            "kbd_server",
            "mouse_server",
            "audio_server",
            "audio_server",
            "audio_server",
        ]
    );
    // Rollback in reverse start order — display/kbd/mouse stopped.
    assert_eq!(
        backend.stop_calls,
        &["mouse_server", "kbd_server", "display_server"]
    );
}

#[test]
fn missing_term_escalates_after_audio_succeeds() {
    let mut backend = MissingTermBackend::default();
    let outcome = run_session(&mut backend);
    assert_eq!(outcome, SessionState::TextFallback);
    // term was attempted 3 times.
    let term_attempts = backend
        .start_calls
        .iter()
        .filter(|n| n == &"term")
        .count();
    assert_eq!(term_attempts, 3);
    // Rollback covers everything that started successfully.
    assert_eq!(
        backend.stop_calls,
        &[
            "audio_server",
            "mouse_server",
            "kbd_server",
            "display_server"
        ]
    );
}

#[test]
fn transient_display_failure_recovers_within_retry_budget() {
    // display_server fails its first start, succeeds on the second.
    let mut backend = TransientDisplayFailureBackend::default();
    let outcome = run_session(&mut backend);
    assert_eq!(outcome, SessionState::Running);
    let display_attempts = backend
        .start_calls
        .iter()
        .filter(|n| n == &"display_server")
        .count();
    assert!(
        display_attempts >= 2 && display_attempts <= MAX_RETRIES_PER_STEP as usize,
        "expected 2–{} attempts, got {}",
        MAX_RETRIES_PER_STEP,
        display_attempts
    );
}

// ---------------------------------------------------------------------------
// Helpers — drive the boot session through the F.1 sequencer with the
// supplied F.3 backend.
// ---------------------------------------------------------------------------

/// Build `SessionStep`s for each declared service, drive the
/// sequencer, and return the final `SessionState`.
///
/// The shape mirrors what `session_manager`'s `boot.rs::run_boot`
/// does in production. The userspace binary's adapter is the same
/// trait — the only difference is the backend: this test substitutes
/// an in-memory recording fake; production substitutes a `/run/init.cmd`
/// + `/run/services.status` adapter.
fn run_session<B: SupervisorBackend>(backend: &mut B) -> SessionState {
    use kernel_core::session::{SessionStep, StartupSequence};

    /// Adapter: each `SessionStep` calls into the supplied backend.
    struct ServiceStep<'b, B: SupervisorBackend> {
        name: &'static str,
        backend: &'b core::cell::RefCell<&'b mut B>,
    }

    impl<'b, B: SupervisorBackend> SessionStep for ServiceStep<'b, B> {
        fn name(&self) -> &'static str {
            self.name
        }
        fn start(&mut self) -> Result<(), kernel_core::session::SessionError> {
            let mut b = self.backend.borrow_mut();
            match b.start(self.name) {
                Ok(SupervisorReply::Ack) => Ok(()),
                Ok(SupervisorReply::Error(_)) => {
                    Err(kernel_core::session::SessionError::StepFailed {
                        step_name: self.name,
                        retries_exhausted: false,
                    })
                }
                Ok(_) => Ok(()),
                Err(_) => Err(kernel_core::session::SessionError::StepFailed {
                    step_name: self.name,
                    retries_exhausted: false,
                }),
            }
        }
        fn stop(&mut self) -> Result<(), kernel_core::session::SessionError> {
            let mut b = self.backend.borrow_mut();
            let _ = b.stop(self.name);
            Ok(())
        }
        fn is_ready(&self) -> bool {
            // The fake backends in this test report ready iff the most
            // recent start succeeded. The production adapter polls
            // `/run/services.status`. The trait doesn't expose &mut so
            // we forward through a dedicated probe verb.
            let mut b = self.backend.borrow_mut();
            matches!(
                b.await_ready(self.name, 0),
                Ok(SupervisorReply::ReadyState { ready: true })
            )
        }
    }

    let backend_cell = core::cell::RefCell::new(backend);
    let names = kernel_core::session_supervisor::declared_session_step_names();
    let mut steps: [ServiceStep<'_, B>; 5] = [
        ServiceStep {
            name: names[0],
            backend: &backend_cell,
        },
        ServiceStep {
            name: names[1],
            backend: &backend_cell,
        },
        ServiceStep {
            name: names[2],
            backend: &backend_cell,
        },
        ServiceStep {
            name: names[3],
            backend: &backend_cell,
        },
        ServiceStep {
            name: names[4],
            backend: &backend_cell,
        },
    ];

    let (s0, rest) = steps.split_at_mut(1);
    let (s1, rest) = rest.split_at_mut(1);
    let (s2, rest) = rest.split_at_mut(1);
    let (s3, s4) = rest.split_at_mut(1);
    let mut step_refs: [&mut dyn SessionStep; 5] = [
        &mut s0[0],
        &mut s1[0],
        &mut s2[0],
        &mut s3[0],
        &mut s4[0],
    ];
    let mut seq = StartupSequence::new(&mut step_refs);
    seq.run(MAX_RETRIES_PER_STEP).expect("run is total")
}

// ---------------------------------------------------------------------------
// Test backends
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AlwaysReadyBackend {
    start_calls: Vec<String>,
    stop_calls: Vec<String>,
}

impl SupervisorBackend for AlwaysReadyBackend {
    fn start(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.start_calls.push(service.to_string());
        Ok(SupervisorReply::Ack)
    }
    fn stop(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.stop_calls.push(service.to_string());
        Ok(SupervisorReply::Ack)
    }
    fn restart(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::Ack)
    }
    fn await_ready(
        &mut self,
        _service: &str,
        _timeout_ms: u64,
    ) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ReadyState { ready: true })
    }
    fn on_exit_observed(
        &mut self,
        _service: &str,
    ) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ExitObserved {
            exit_code: 0,
            signaled: false,
        })
    }
}

#[derive(Default)]
struct MissingAudioBackend {
    start_calls: Vec<String>,
    stop_calls: Vec<String>,
}

impl SupervisorBackend for MissingAudioBackend {
    fn start(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.start_calls.push(service.to_string());
        if service == "audio_server" {
            return Ok(SupervisorReply::Error(SupervisorError::UnknownService));
        }
        Ok(SupervisorReply::Ack)
    }
    fn stop(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.stop_calls.push(service.to_string());
        Ok(SupervisorReply::Ack)
    }
    fn restart(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::Ack)
    }
    fn await_ready(
        &mut self,
        service: &str,
        _timeout_ms: u64,
    ) -> Result<SupervisorReply, SupervisorError> {
        if service == "audio_server" {
            return Ok(SupervisorReply::ReadyState { ready: false });
        }
        Ok(SupervisorReply::ReadyState { ready: true })
    }
    fn on_exit_observed(
        &mut self,
        _service: &str,
    ) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ExitObserved {
            exit_code: 0,
            signaled: false,
        })
    }
}

#[derive(Default)]
struct MissingTermBackend {
    start_calls: Vec<String>,
    stop_calls: Vec<String>,
}

impl SupervisorBackend for MissingTermBackend {
    fn start(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.start_calls.push(service.to_string());
        if service == "term" {
            return Ok(SupervisorReply::Error(SupervisorError::UnknownService));
        }
        Ok(SupervisorReply::Ack)
    }
    fn stop(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.stop_calls.push(service.to_string());
        Ok(SupervisorReply::Ack)
    }
    fn restart(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::Ack)
    }
    fn await_ready(
        &mut self,
        service: &str,
        _timeout_ms: u64,
    ) -> Result<SupervisorReply, SupervisorError> {
        if service == "term" {
            return Ok(SupervisorReply::ReadyState { ready: false });
        }
        Ok(SupervisorReply::ReadyState { ready: true })
    }
    fn on_exit_observed(
        &mut self,
        _service: &str,
    ) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ExitObserved {
            exit_code: 0,
            signaled: false,
        })
    }
}

#[derive(Default)]
struct TransientDisplayFailureBackend {
    start_calls: Vec<String>,
    display_attempts: u32,
}

impl SupervisorBackend for TransientDisplayFailureBackend {
    fn start(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.start_calls.push(service.to_string());
        if service == "display_server" {
            self.display_attempts += 1;
            if self.display_attempts == 1 {
                return Ok(SupervisorReply::Error(SupervisorError::UnknownService));
            }
        }
        Ok(SupervisorReply::Ack)
    }
    fn stop(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::Ack)
    }
    fn restart(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::Ack)
    }
    fn await_ready(
        &mut self,
        service: &str,
        _timeout_ms: u64,
    ) -> Result<SupervisorReply, SupervisorError> {
        if service == "display_server" && self.display_attempts < 2 {
            return Ok(SupervisorReply::ReadyState { ready: false });
        }
        Ok(SupervisorReply::ReadyState { ready: true })
    }
    fn on_exit_observed(
        &mut self,
        _service: &str,
    ) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ExitObserved {
            exit_code: 0,
            signaled: false,
        })
    }
}
