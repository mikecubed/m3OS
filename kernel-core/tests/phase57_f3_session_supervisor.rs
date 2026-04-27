//! Phase 57 Track F.3 — Supervisor verbs for `session_manager`.
//!
//! These integration tests pin the typed verb codec and dispatcher
//! contract that `session_manager` (F.2) uses to drive the existing
//! `init` supervisor. Per the Track F.3 task list:
//!
//! - The new supervision verbs are visible only to processes holding
//!   the `session_manager` capability — no broad public surface.
//! - Existing service supervision tests stay green (this file does not
//!   touch `kernel-core/src/service.rs`).
//! - No new syscall is added; `session_manager` consumes existing IPC +
//!   capabilities. The codec round-trips on the host so the contract is
//!   pinned without booting QEMU.
//!
//! The pure-logic types live in [`kernel_core::session_supervisor`]:
//!
//! - [`SupervisorVerb`] — `Start`, `Stop`, `Restart`, `AwaitReady`,
//!   `OnExitObserved` — the four verbs `session_manager` needs (per the
//!   F.3 acceptance list) plus `Restart` for symmetry with init's
//!   existing control verbs.
//! - [`SupervisorReply`] — `Ack`, `ReadyState`, `ExitObserved`,
//!   `Error(SupervisorError)`.
//! - [`SupervisorError`] — typed error surface; no stringly-typed
//!   variants.
//! - [`SupervisorCap`] — capability tag granted to `session_manager`
//!   only. The codec refuses to dispatch a verb without it.

use kernel_core::session_supervisor::{
    SupervisorCap, SupervisorError, SupervisorReply, SupervisorVerb, decode_reply, decode_verb,
    encode_reply, encode_verb,
};

// --------------------------------------------------------------------------
// Codec round-trip — every verb decodes back to its source value.
// --------------------------------------------------------------------------

#[test]
fn verb_start_round_trips() {
    let verb = SupervisorVerb::Start {
        service: "display_server",
    };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..len]).expect("decode_verb");
    assert_eq!(decoded, verb);
}

#[test]
fn verb_stop_round_trips() {
    let verb = SupervisorVerb::Stop {
        service: "audio_server",
    };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..len]).expect("decode_verb");
    assert_eq!(decoded, verb);
}

#[test]
fn verb_restart_round_trips() {
    let verb = SupervisorVerb::Restart { service: "term" };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..len]).expect("decode_verb");
    assert_eq!(decoded, verb);
}

#[test]
fn verb_await_ready_round_trips() {
    let verb = SupervisorVerb::AwaitReady {
        service: "kbd_server",
        timeout_ms: 1500,
    };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..len]).expect("decode_verb");
    assert_eq!(decoded, verb);
}

#[test]
fn verb_on_exit_observed_round_trips() {
    let verb = SupervisorVerb::OnExitObserved {
        service: "mouse_server",
    };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..len]).expect("decode_verb");
    assert_eq!(decoded, verb);
}

#[test]
fn reply_ack_round_trips() {
    let reply = SupervisorReply::Ack;
    let mut buf = [0u8; 32];
    let len = encode_reply(&reply, &mut buf).expect("encode_reply");
    let decoded = decode_reply(&buf[..len]).expect("decode_reply");
    assert_eq!(decoded, reply);
}

#[test]
fn reply_ready_state_round_trips() {
    let reply = SupervisorReply::ReadyState { ready: true };
    let mut buf = [0u8; 32];
    let len = encode_reply(&reply, &mut buf).expect("encode_reply");
    let decoded = decode_reply(&buf[..len]).expect("decode_reply");
    assert_eq!(decoded, reply);

    let reply2 = SupervisorReply::ReadyState { ready: false };
    let len2 = encode_reply(&reply2, &mut buf).expect("encode_reply");
    let decoded2 = decode_reply(&buf[..len2]).expect("decode_reply");
    assert_eq!(decoded2, reply2);
}

#[test]
fn reply_exit_observed_round_trips() {
    let reply = SupervisorReply::ExitObserved {
        exit_code: 42,
        signaled: false,
    };
    let mut buf = [0u8; 32];
    let len = encode_reply(&reply, &mut buf).expect("encode_reply");
    let decoded = decode_reply(&buf[..len]).expect("decode_reply");
    assert_eq!(decoded, reply);

    let reply2 = SupervisorReply::ExitObserved {
        exit_code: 9,
        signaled: true,
    };
    let len2 = encode_reply(&reply2, &mut buf).expect("encode_reply");
    let decoded2 = decode_reply(&buf[..len2]).expect("decode_reply");
    assert_eq!(decoded2, reply2);
}

#[test]
fn reply_error_round_trips_for_every_variant() {
    for err in [
        SupervisorError::UnknownService,
        SupervisorError::PermissionDenied,
        SupervisorError::Timeout,
        SupervisorError::AlreadyRunning,
        SupervisorError::NotRunning,
        SupervisorError::MalformedRequest,
        SupervisorError::CapabilityMissing,
    ] {
        let reply = SupervisorReply::Error(err);
        let mut buf = [0u8; 32];
        let len = encode_reply(&reply, &mut buf).expect("encode_reply");
        let decoded = decode_reply(&buf[..len]).expect("decode_reply");
        assert_eq!(decoded, reply, "round-trip failed for {:?}", err);
    }
}

// --------------------------------------------------------------------------
// Buffer-too-small surfaces a typed error rather than a panic.
// --------------------------------------------------------------------------

#[test]
fn encode_verb_buffer_too_small_returns_error() {
    let verb = SupervisorVerb::Start {
        service: "display_server",
    };
    let mut buf = [0u8; 4]; // Way too small for the service name.
    let result = encode_verb(&verb, &mut buf);
    assert!(matches!(result, Err(SupervisorError::MalformedRequest)));
}

#[test]
fn decode_verb_truncated_returns_error() {
    let verb = SupervisorVerb::Start {
        service: "display_server",
    };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");
    // Truncate the buffer in the middle of the service name.
    let truncated = &buf[..len - 4];
    let decoded = decode_verb(truncated);
    assert!(matches!(decoded, Err(SupervisorError::MalformedRequest)));
}

#[test]
fn decode_verb_unknown_tag_returns_error() {
    // First byte is the verb tag. Use a value out of range.
    let bad = [0xFFu8, 0x00, 0x00];
    let decoded = decode_verb(&bad);
    assert!(matches!(decoded, Err(SupervisorError::MalformedRequest)));
}

// --------------------------------------------------------------------------
// Capability gate — verbs cannot be dispatched without the
// `SupervisorCap`.
// --------------------------------------------------------------------------

#[test]
fn dispatch_without_cap_returns_capability_missing() {
    let verb = SupervisorVerb::Start {
        service: "display_server",
    };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");

    // No `SupervisorCap` available — the dispatcher must reject the
    // request before invoking any side effect on the supervised service.
    let outcome = kernel_core::session_supervisor::dispatch_authenticated(
        &buf[..len],
        None,
        &mut TestBackend::default(),
    );
    assert!(matches!(outcome, Err(SupervisorError::CapabilityMissing)));
}

#[test]
fn dispatch_with_cap_invokes_backend() {
    let verb = SupervisorVerb::Start {
        service: "display_server",
    };
    let mut buf = [0u8; 64];
    let len = encode_verb(&verb, &mut buf).expect("encode_verb");

    let mut backend = TestBackend::default();
    let cap = SupervisorCap::granted_for_session_manager_only();
    let reply = kernel_core::session_supervisor::dispatch_authenticated(
        &buf[..len],
        Some(&cap),
        &mut backend,
    )
    .expect("dispatch ok");
    assert_eq!(reply, SupervisorReply::Ack);
    assert_eq!(backend.start_calls, &["display_server"]);
}

#[test]
fn dispatch_with_cap_routes_each_verb_to_backend() {
    let mut backend = TestBackend::default();
    let cap = SupervisorCap::granted_for_session_manager_only();
    let mut buf = [0u8; 64];

    // Start
    let len = encode_verb(&SupervisorVerb::Start { service: "kbd" }, &mut buf).expect("encode");
    kernel_core::session_supervisor::dispatch_authenticated(&buf[..len], Some(&cap), &mut backend)
        .expect("ok");

    // Stop
    let len = encode_verb(&SupervisorVerb::Stop { service: "kbd" }, &mut buf).expect("encode");
    kernel_core::session_supervisor::dispatch_authenticated(&buf[..len], Some(&cap), &mut backend)
        .expect("ok");

    // AwaitReady
    let len = encode_verb(
        &SupervisorVerb::AwaitReady {
            service: "audio",
            timeout_ms: 1000,
        },
        &mut buf,
    )
    .expect("encode");
    kernel_core::session_supervisor::dispatch_authenticated(&buf[..len], Some(&cap), &mut backend)
        .expect("ok");

    // OnExitObserved
    let len = encode_verb(
        &SupervisorVerb::OnExitObserved { service: "term" },
        &mut buf,
    )
    .expect("encode");
    kernel_core::session_supervisor::dispatch_authenticated(&buf[..len], Some(&cap), &mut backend)
        .expect("ok");

    assert_eq!(backend.start_calls, &["kbd"]);
    assert_eq!(backend.stop_calls, &["kbd"]);
    assert_eq!(backend.await_ready_calls, &[("audio".to_string(), 1000u64)]);
    assert_eq!(backend.on_exit_calls, &["term"]);
}

#[test]
fn dispatch_propagates_backend_error() {
    let mut backend = TestBackend {
        start_should_fail: true,
        ..Default::default()
    };
    let cap = SupervisorCap::granted_for_session_manager_only();
    let mut buf = [0u8; 64];
    let len = encode_verb(&SupervisorVerb::Start { service: "missing" }, &mut buf).expect("encode");
    let outcome = kernel_core::session_supervisor::dispatch_authenticated(
        &buf[..len],
        Some(&cap),
        &mut backend,
    )
    .expect("dispatch ok");
    assert_eq!(
        outcome,
        SupervisorReply::Error(SupervisorError::UnknownService)
    );
}

// --------------------------------------------------------------------------
// SupervisorCap is opaque — `granted_for_session_manager_only` is the
// only constructor.
// --------------------------------------------------------------------------

#[test]
fn supervisor_cap_only_constructor_is_named_for_session_manager() {
    // Compiles and produces a usable cap. The name documents the
    // policy: the cap is only granted to `session_manager`.
    let _cap = SupervisorCap::granted_for_session_manager_only();
}

// --------------------------------------------------------------------------
// Test backend — an in-memory `SupervisorBackend` impl that records
// every call. Different shape than init's real supervisor so the
// contract test catches incidental coupling.
// --------------------------------------------------------------------------

#[derive(Default)]
struct TestBackend {
    start_calls: Vec<String>,
    stop_calls: Vec<String>,
    await_ready_calls: Vec<(String, u64)>,
    on_exit_calls: Vec<String>,
    start_should_fail: bool,
}

impl kernel_core::session_supervisor::SupervisorBackend for TestBackend {
    fn start(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        if self.start_should_fail {
            return Ok(SupervisorReply::Error(SupervisorError::UnknownService));
        }
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
        service: &str,
        timeout_ms: u64,
    ) -> Result<SupervisorReply, SupervisorError> {
        self.await_ready_calls
            .push((service.to_string(), timeout_ms));
        Ok(SupervisorReply::ReadyState { ready: true })
    }
    fn on_exit_observed(&mut self, service: &str) -> Result<SupervisorReply, SupervisorError> {
        self.on_exit_calls.push(service.to_string());
        Ok(SupervisorReply::ExitObserved {
            exit_code: 0,
            signaled: false,
        })
    }
}
