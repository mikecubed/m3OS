//! Phase 57 Track F.5 — control-socket verbs codec + dispatcher tests.
//!
//! These tests pin the typed wire codec and authenticated dispatcher
//! contract that `session_manager` (F.5) uses to expose its three
//! control-socket verbs:
//!
//! - `session-state`   — return the current `SessionState`
//! - `session-stop`    — graceful shutdown, falls through to `text-fallback`
//! - `session-restart` — graceful stop + start
//!
//! Per the F.5 acceptance:
//!
//! - Failing tests commit first.
//! - Control socket lives on a separate AF_UNIX path consistent with
//!   the Phase 56 control-socket precedent.
//! - Verbs: `session-state`, `session-stop`, `session-restart`.
//! - Access control follows the Phase 56 m3ctl precedent: capability-
//!   based — the connecting peer must hold the `session_manager`
//!   control-socket cap, granted to `m3ctl` at session-manager startup
//!   and to no other process. **No UID-based access control** in
//!   Phase 57.
//!
//! The codec lives in [`kernel_core::session_control`] (new module)
//! and is host-tested here. The userspace IPC dispatcher in
//! `userspace/session_manager/src/control.rs` consumes the codec.
//!
//! ## Why a separate module from `session_supervisor`
//!
//! `session_supervisor` (F.3) is the **internal** verb surface that
//! `session_manager` issues against init. `session_control` (F.5) is
//! the **external** verb surface that `m3ctl` issues against
//! `session_manager`. Different actors, different cap, different
//! direction — separate modules, even though both share the
//! "tag-prefixed bytes" wire shape. SOLID SRP and ISP.

use kernel_core::session::SessionState;
use kernel_core::session_control::{
    ControlReply, ControlSocketCap, ControlVerb, SessionControlError, decode_reply, decode_verb,
    dispatch_authenticated, encode_reply, encode_verb,
};

// ---------------------------------------------------------------------------
// Codec round-trip — every verb decodes back to its source value.
// ---------------------------------------------------------------------------

#[test]
fn verb_session_state_round_trips() {
    let verb = ControlVerb::SessionState;
    let mut buf = [0u8; 4];
    let len = encode_verb(&verb, &mut buf).expect("encode");
    let decoded = decode_verb(&buf[..len]).expect("decode");
    assert_eq!(decoded, verb);
}

#[test]
fn verb_session_stop_round_trips() {
    let verb = ControlVerb::SessionStop;
    let mut buf = [0u8; 4];
    let len = encode_verb(&verb, &mut buf).expect("encode");
    let decoded = decode_verb(&buf[..len]).expect("decode");
    assert_eq!(decoded, verb);
}

#[test]
fn verb_session_restart_round_trips() {
    let verb = ControlVerb::SessionRestart;
    let mut buf = [0u8; 4];
    let len = encode_verb(&verb, &mut buf).expect("encode");
    let decoded = decode_verb(&buf[..len]).expect("decode");
    assert_eq!(decoded, verb);
}

// ---------------------------------------------------------------------------
// Reply codec round-trip.
// ---------------------------------------------------------------------------

#[test]
fn reply_state_running_round_trips() {
    let reply = ControlReply::State {
        state: SessionState::Running,
    };
    let mut buf = [0u8; 16];
    let len = encode_reply(&reply, &mut buf).expect("encode");
    let decoded = decode_reply(&buf[..len]).expect("decode");
    assert_eq!(decoded, reply);
}

#[test]
fn reply_state_text_fallback_round_trips() {
    let reply = ControlReply::State {
        state: SessionState::TextFallback,
    };
    let mut buf = [0u8; 16];
    let len = encode_reply(&reply, &mut buf).expect("encode");
    let decoded = decode_reply(&buf[..len]).expect("decode");
    assert_eq!(decoded, reply);
}

#[test]
fn reply_state_recovering_round_trips() {
    let reply = ControlReply::State {
        state: SessionState::Recovering {
            step_name: "kbd_server",
            retry_count: 2,
        },
    };
    let mut buf = [0u8; 64];
    let len = encode_reply(&reply, &mut buf).expect("encode");
    let decoded = decode_reply(&buf[..len]).expect("decode");
    assert_eq!(decoded, reply);
}

#[test]
fn reply_ack_round_trips() {
    let reply = ControlReply::Ack;
    let mut buf = [0u8; 4];
    let len = encode_reply(&reply, &mut buf).expect("encode");
    let decoded = decode_reply(&buf[..len]).expect("decode");
    assert_eq!(decoded, reply);
}

#[test]
fn reply_error_round_trips_for_every_variant() {
    for err in [
        SessionControlError::CapabilityMissing,
        SessionControlError::MalformedRequest,
        SessionControlError::Internal,
    ] {
        let reply = ControlReply::Error(err);
        let mut buf = [0u8; 4];
        let len = encode_reply(&reply, &mut buf).expect("encode");
        let decoded = decode_reply(&buf[..len]).expect("decode");
        assert_eq!(decoded, reply, "round trip for {:?}", err);
    }
}

// ---------------------------------------------------------------------------
// Encoder error surface.
// ---------------------------------------------------------------------------

#[test]
fn encode_verb_into_empty_buffer_returns_malformed_request() {
    let mut buf = [];
    let result = encode_verb(&ControlVerb::SessionState, &mut buf);
    assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
}

#[test]
fn decode_verb_from_empty_buffer_returns_malformed_request() {
    let result = decode_verb(&[]);
    assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
}

#[test]
fn decode_verb_unknown_tag_returns_malformed_request() {
    let result = decode_verb(&[0xFF]);
    assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
}

// ---------------------------------------------------------------------------
// Authenticated dispatcher — capability gate.
// ---------------------------------------------------------------------------

/// Test backend that records the verbs `dispatch_authenticated` forwards
/// and returns canned state values.
struct RecordingControlBackend {
    state_calls: u32,
    stop_calls: u32,
    restart_calls: u32,
    canned_state: SessionState,
}

impl RecordingControlBackend {
    fn new(state: SessionState) -> Self {
        Self {
            state_calls: 0,
            stop_calls: 0,
            restart_calls: 0,
            canned_state: state,
        }
    }
}

impl kernel_core::session_control::SessionControlBackend for RecordingControlBackend {
    fn current_state(&mut self) -> SessionState {
        self.state_calls += 1;
        self.canned_state
    }
    fn session_stop(&mut self) -> Result<(), SessionControlError> {
        self.stop_calls += 1;
        Ok(())
    }
    fn session_restart(&mut self) -> Result<(), SessionControlError> {
        self.restart_calls += 1;
        Ok(())
    }
}

#[test]
fn dispatch_session_state_returns_current_state() {
    let mut backend = RecordingControlBackend::new(SessionState::Running);
    let cap = ControlSocketCap::granted_for_m3ctl_only();
    let mut buf = [0u8; 4];
    let len = encode_verb(&ControlVerb::SessionState, &mut buf).unwrap();
    let reply = dispatch_authenticated(&buf[..len], Some(&cap), &mut backend).expect("dispatch");
    assert_eq!(
        reply,
        ControlReply::State {
            state: SessionState::Running
        }
    );
    assert_eq!(backend.state_calls, 1);
    assert_eq!(backend.stop_calls, 0);
}

#[test]
fn dispatch_session_stop_returns_ack_and_invokes_backend() {
    let mut backend = RecordingControlBackend::new(SessionState::Running);
    let cap = ControlSocketCap::granted_for_m3ctl_only();
    let mut buf = [0u8; 4];
    let len = encode_verb(&ControlVerb::SessionStop, &mut buf).unwrap();
    let reply = dispatch_authenticated(&buf[..len], Some(&cap), &mut backend).expect("dispatch");
    assert_eq!(reply, ControlReply::Ack);
    assert_eq!(backend.stop_calls, 1);
}

#[test]
fn dispatch_session_restart_returns_ack_and_invokes_backend() {
    let mut backend = RecordingControlBackend::new(SessionState::Running);
    let cap = ControlSocketCap::granted_for_m3ctl_only();
    let mut buf = [0u8; 4];
    let len = encode_verb(&ControlVerb::SessionRestart, &mut buf).unwrap();
    let reply = dispatch_authenticated(&buf[..len], Some(&cap), &mut backend).expect("dispatch");
    assert_eq!(reply, ControlReply::Ack);
    assert_eq!(backend.restart_calls, 1);
}

#[test]
fn dispatch_without_cap_returns_capability_missing() {
    let mut backend = RecordingControlBackend::new(SessionState::Running);
    let mut buf = [0u8; 4];
    let len = encode_verb(&ControlVerb::SessionState, &mut buf).unwrap();
    let result = dispatch_authenticated(&buf[..len], None, &mut backend);
    assert!(matches!(
        result,
        Err(SessionControlError::CapabilityMissing)
    ));
    assert_eq!(
        backend.state_calls, 0,
        "backend must not be invoked without cap"
    );
    assert_eq!(backend.stop_calls, 0);
    assert_eq!(backend.restart_calls, 0);
}

#[test]
fn dispatch_with_malformed_request_does_not_invoke_backend() {
    let mut backend = RecordingControlBackend::new(SessionState::Running);
    let cap = ControlSocketCap::granted_for_m3ctl_only();
    // 0xFF is not a recognized verb tag.
    let result = dispatch_authenticated(&[0xFFu8], Some(&cap), &mut backend);
    assert!(matches!(result, Err(SessionControlError::MalformedRequest)));
    assert_eq!(backend.state_calls, 0);
    assert_eq!(backend.stop_calls, 0);
    assert_eq!(backend.restart_calls, 0);
}

// ---------------------------------------------------------------------------
// Wire-shape stability — the verb tags are stable; reordering is a
// wire-incompatible change. This test latches the byte values so a
// future reorder fails CI before deployment.
// ---------------------------------------------------------------------------

#[test]
fn verb_session_state_encodes_to_tag_one() {
    let mut buf = [0u8; 4];
    let len = encode_verb(&ControlVerb::SessionState, &mut buf).unwrap();
    assert_eq!(len, 1);
    assert_eq!(buf[0], 0x01);
}

#[test]
fn verb_session_stop_encodes_to_tag_two() {
    let mut buf = [0u8; 4];
    let len = encode_verb(&ControlVerb::SessionStop, &mut buf).unwrap();
    assert_eq!(len, 1);
    assert_eq!(buf[0], 0x02);
}

#[test]
fn verb_session_restart_encodes_to_tag_three() {
    let mut buf = [0u8; 4];
    let len = encode_verb(&ControlVerb::SessionRestart, &mut buf).unwrap();
    assert_eq!(len, 1);
    assert_eq!(buf[0], 0x03);
}

// ---------------------------------------------------------------------------
// Cap is constructible only via the documented constructor — guards
// against accidental cap forgery via a `Default` derive or unit struct.
// ---------------------------------------------------------------------------

#[test]
fn cap_constructor_name_documents_the_grant_policy() {
    // The presence of this constructor as the only public path is the
    // policy. The compiler enforces that no caller can construct a
    // `ControlSocketCap` via any other path; this test simply asserts
    // the constructor exists and is callable.
    let _cap = ControlSocketCap::granted_for_m3ctl_only();
}
