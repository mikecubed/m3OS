//! Phase 57 Track F.4 — Text-fallback rollback executor tests.
//!
//! These tests pin the contract `session_manager` consumes when it
//! escalates to text-fallback: stop every declared graphical service in
//! reverse start order via the supplied `SupervisorBackend`, then ask
//! a `FramebufferRestorer` (DI seam) to release the framebuffer back
//! to the kernel console.
//!
//! Per the F.4 acceptance:
//!
//! > On `text-fallback`: `session_manager` stops the graphical services
//! > in reverse start order, releases the framebuffer back to the
//! > kernel console (the existing Phase 47 `restore_console` path),
//! > and surfaces an admin shell on the serial console.
//!
//! Splitting the executor into a host-testable function (with a
//! `FramebufferRestorer` trait for the FB release seam) means the
//! rollback motion is verified without booting QEMU. The userspace
//! `session_manager` daemon wraps the kernel-core function with the
//! `framebuffer_release` syscall as the production restorer; the smoke
//! test (H.3) covers the end-to-end kill-display-server path under
//! QEMU.

use kernel_core::session::recover::{
    FramebufferRestorer, TextFallbackOutcome, execute_text_fallback_rollback,
};
use kernel_core::session_supervisor::{SupervisorBackend, SupervisorError, SupervisorReply};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RecordingBackend {
    stop_calls: Vec<String>,
}

impl SupervisorBackend for RecordingBackend {
    fn start(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
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
    fn on_exit_observed(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ExitObserved {
            exit_code: 0,
            signaled: false,
        })
    }
}

#[derive(Default)]
struct RecordingRestorer {
    invoked: bool,
    fail: bool,
}

impl FramebufferRestorer for RecordingRestorer {
    fn restore_console(&mut self) -> Result<(), ()> {
        self.invoked = true;
        if self.fail { Err(()) } else { Ok(()) }
    }
}

// Backend whose `stop` always errors — exercises the swallow-and-continue
// rollback policy: every stop is attempted regardless of prior errors.
#[derive(Default)]
struct FailingStopBackend {
    stop_attempts: Vec<String>,
}

impl SupervisorBackend for FailingStopBackend {
    fn start(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::Ack)
    }
    fn stop(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.stop_attempts.push(service.to_string());
        Err(SupervisorError::UnknownService)
    }
    fn restart(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::Ack)
    }
    fn await_ready(
        &mut self,
        _service: &str,
        _timeout_ms: u64,
    ) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ReadyState { ready: false })
    }
    fn on_exit_observed(&mut self, _service: &str) -> Result<SupervisorReply, SupervisorError> {
        Ok(SupervisorReply::ExitObserved {
            exit_code: 0,
            signaled: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Reverse-order rollback contract
// ---------------------------------------------------------------------------

#[test]
fn stops_every_declared_service_in_reverse_order() {
    let mut backend = RecordingBackend::default();
    let mut restorer = RecordingRestorer::default();
    let outcome = execute_text_fallback_rollback(&mut backend, &mut restorer);

    // The declared order is display → kbd → mouse → audio → term;
    // rollback runs in reverse, so we expect term, audio, mouse, kbd,
    // display.
    assert_eq!(
        backend.stop_calls,
        vec![
            "term".to_string(),
            "audio_server".to_string(),
            "mouse_server".to_string(),
            "kbd_server".to_string(),
            "display_server".to_string(),
        ]
    );
    assert_eq!(outcome.stops_attempted, 5);
    assert!(outcome.restorer_ok);
}

#[test]
fn invokes_framebuffer_restorer_after_stops() {
    let mut backend = RecordingBackend::default();
    let mut restorer = RecordingRestorer::default();
    let _ = execute_text_fallback_rollback(&mut backend, &mut restorer);
    assert!(restorer.invoked);
}

// ---------------------------------------------------------------------------
// Swallow-and-continue: stop errors must not abort the rollback.
// ---------------------------------------------------------------------------

#[test]
fn rollback_completes_even_when_every_stop_errors() {
    // Even if every `stop` fails, the rollback continues — every
    // service in the declared order still gets a `stop` attempt.
    let mut backend = FailingStopBackend::default();
    let mut restorer = RecordingRestorer::default();
    let outcome = execute_text_fallback_rollback(&mut backend, &mut restorer);
    assert_eq!(
        backend.stop_attempts,
        vec![
            "term".to_string(),
            "audio_server".to_string(),
            "mouse_server".to_string(),
            "kbd_server".to_string(),
            "display_server".to_string(),
        ]
    );
    // The framebuffer restorer is invoked regardless of stop errors.
    assert!(restorer.invoked);
    assert_eq!(outcome.stops_attempted, 5);
}

// ---------------------------------------------------------------------------
// Restorer failure surfaces in the outcome.
// ---------------------------------------------------------------------------

#[test]
fn restorer_failure_surfaces_in_outcome() {
    let mut backend = RecordingBackend::default();
    let mut restorer = RecordingRestorer {
        fail: true,
        ..Default::default()
    };
    let outcome = execute_text_fallback_rollback(&mut backend, &mut restorer);
    assert!(restorer.invoked);
    assert_eq!(outcome.stops_attempted, 5);
    assert!(!outcome.restorer_ok);
}

// ---------------------------------------------------------------------------
// Outcome shape parity with the public type.
// ---------------------------------------------------------------------------

#[test]
fn outcome_is_value_typed_and_equatable() {
    // Re-construct the outcome by hand to guard the public field shape.
    let outcome = TextFallbackOutcome {
        stops_attempted: 5,
        restorer_ok: true,
    };
    assert_eq!(outcome.stops_attempted, 5);
    assert!(outcome.restorer_ok);
    // Equatable / copy: derived in production.
    let copy = outcome;
    assert_eq!(copy, outcome);
}
