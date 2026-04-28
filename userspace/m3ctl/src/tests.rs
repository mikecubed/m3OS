//! Phase 57 Track I.2 host tests for the m3ctl verb parser.
//!
//! TDD discipline: this file commits **before** the implementation
//! that makes the session-* arms pass. The RED commit's
//! `parse_verb` does not yet recognize `session-state` /
//! `session-stop` / `session-restart`; the tests below assert the
//! correct typed `ParsedVerb::Session(_)` outcome and therefore fail
//! until the GREEN commit lands the arms.
//!
//! Tests run on the host via
//! `cargo test -p m3ctl --target x86_64-unknown-linux-gnu`.
//!
//! # Coverage
//!
//! - **Phase 57 I.2 verbs.** Every new verb maps to the correct
//!   [`ControlVerb`] variant; the dispatch target is the session
//!   service, not the display service.
//! - **DRY check.** The session codec (`encode_verb` / `decode_verb`)
//!   is the single source of truth — the m3ctl crate must not redefine
//!   the verb tags. We assert the parsed verb round-trips through the
//!   codec.
//! - **Phase 56 verb regression.** The display verbs that already
//!   worked must keep working unchanged — the I.2 refactor must not
//!   regress the Phase 56 verb surface.

extern crate std;

use super::*;
use kernel_core::display::control::{ControlCommand, EventKind, SurfaceId};
use kernel_core::session_control::{ControlVerb, decode_verb, encode_verb};

// ---------------------------------------------------------------------------
// Phase 57 I.2 — session control verbs (RED until the GREEN commit lands)
// ---------------------------------------------------------------------------

#[test]
fn session_state_parses_to_session_state_verb() {
    let parsed = parse_verb("session-state", &[]).expect("session-state should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Session(ControlVerb::SessionState),
        "session-state must produce ParsedVerb::Session(ControlVerb::SessionState)"
    );
}

#[test]
fn session_stop_parses_to_session_stop_verb() {
    let parsed = parse_verb("session-stop", &[]).expect("session-stop should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Session(ControlVerb::SessionStop),
        "session-stop must produce ParsedVerb::Session(ControlVerb::SessionStop)"
    );
}

#[test]
fn session_restart_parses_to_session_restart_verb() {
    let parsed = parse_verb("session-restart", &[]).expect("session-restart should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Session(ControlVerb::SessionRestart),
        "session-restart must produce ParsedVerb::Session(ControlVerb::SessionRestart)"
    );
}

#[test]
fn session_verbs_take_no_arguments() {
    // Extra arguments are tolerated (parser is permissive — argument
    // parsing is per-verb). The three session verbs do not consume
    // their args; passing extras must not change the parsed verb.
    let parsed = parse_verb("session-state", &["unused", "args"])
        .expect("extra args should not block session-state");
    assert_eq!(parsed, ParsedVerb::Session(ControlVerb::SessionState));
}

// ---------------------------------------------------------------------------
// DRY — the session-control codec lives once in kernel_core. We verify
// every parsed session verb round-trips through encode_verb /
// decode_verb. If anyone reintroduces a parallel byte definition in
// m3ctl, this test catches it because it would diverge from the
// kernel_core codec output.
// ---------------------------------------------------------------------------

#[test]
fn session_state_round_trips_through_codec() {
    let parsed = parse_verb("session-state", &[]).expect("session-state should parse");
    let verb = match parsed {
        ParsedVerb::Session(v) => v,
        other => panic!("expected Session, got {:?}", other),
    };
    let mut buf = [0u8; 4];
    let n = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..n]).expect("decode_verb");
    assert_eq!(decoded, ControlVerb::SessionState);
}

#[test]
fn session_stop_round_trips_through_codec() {
    let parsed = parse_verb("session-stop", &[]).expect("session-stop should parse");
    let verb = match parsed {
        ParsedVerb::Session(v) => v,
        other => panic!("expected Session, got {:?}", other),
    };
    let mut buf = [0u8; 4];
    let n = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..n]).expect("decode_verb");
    assert_eq!(decoded, ControlVerb::SessionStop);
}

#[test]
fn session_restart_round_trips_through_codec() {
    let parsed = parse_verb("session-restart", &[]).expect("session-restart should parse");
    let verb = match parsed {
        ParsedVerb::Session(v) => v,
        other => panic!("expected Session, got {:?}", other),
    };
    let mut buf = [0u8; 4];
    let n = encode_verb(&verb, &mut buf).expect("encode_verb");
    let decoded = decode_verb(&buf[..n]).expect("decode_verb");
    assert_eq!(decoded, ControlVerb::SessionRestart);
}

// ---------------------------------------------------------------------------
// Phase 56 regression — pre-existing display verbs must keep working
// after the I.2 refactor (lib + bin split, parse_verb relocation).
// ---------------------------------------------------------------------------

#[test]
fn display_version_parses_to_display_command() {
    let parsed = parse_verb("version", &[]).expect("version should parse");
    assert_eq!(parsed, ParsedVerb::Display(ControlCommand::Version));
}

#[test]
fn display_focus_parses_with_surface_id() {
    let parsed = parse_verb("focus", &["7"]).expect("focus 7 should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::Focus {
            surface_id: SurfaceId(7)
        })
    );
}

#[test]
fn display_focus_accepts_hex_surface_id() {
    let parsed = parse_verb("focus", &["0x2a"]).expect("focus 0x2a should parse");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::Focus {
            surface_id: SurfaceId(42)
        })
    );
}

#[test]
fn display_focus_missing_id_returns_missing_argument() {
    let err = parse_verb("focus", &[]).expect_err("focus with no id should fail");
    assert!(matches!(err, ParseError::MissingArgument(_)));
}

#[test]
fn display_focus_bad_id_returns_bad_argument() {
    let err = parse_verb("focus", &["not-a-number"]).expect_err("non-numeric id should fail");
    assert!(matches!(err, ParseError::BadArgument(_)));
}

#[test]
fn display_register_bind_parses_with_mask_and_keycode() {
    let parsed = parse_verb("register-bind", &["0x0008", "65"]).expect("register-bind");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::RegisterBind {
            modifier_mask: 8,
            keycode: 65,
        })
    );
}

#[test]
fn display_subscribe_parses_event_kind() {
    let parsed = parse_verb("subscribe", &["focus-changed"]).expect("subscribe focus-changed");
    assert_eq!(
        parsed,
        ParsedVerb::Display(ControlCommand::Subscribe {
            event_kind: EventKind::FocusChanged
        })
    );
}

#[test]
fn display_subscribe_unknown_kind_returns_unknown_event_kind() {
    let err = parse_verb("subscribe", &["bogus"])
        .expect_err("subscribe with unknown kind should fail");
    assert!(matches!(err, ParseError::UnknownEventKind(_)));
}

// ---------------------------------------------------------------------------
// Unknown verb regression
// ---------------------------------------------------------------------------

#[test]
fn unknown_verb_returns_unknown_verb_error() {
    let err = parse_verb("not-a-real-verb", &[]).expect_err("unknown verb should fail");
    match err {
        ParseError::UnknownVerb(s) => assert_eq!(s, "not-a-real-verb"),
        other => panic!("expected UnknownVerb, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// Service name / IPC label invariants — must match the daemons.
// ---------------------------------------------------------------------------

#[test]
fn session_control_service_name_matches_daemon() {
    // The `userspace::session_manager::control::CONTROL_SERVICE_NAME`
    // constant is "session-control"; the m3ctl client must use the
    // same value. If anyone renames either side they MUST keep them
    // aligned — this test catches divergence at host-test time, before
    // a runtime lookup fails.
    assert_eq!(SESSION_CONTROL_SERVICE_NAME, "session-control");
}

#[test]
fn display_control_service_name_matches_daemon() {
    // Mirror invariant for the Phase 56 surface.
    assert_eq!(DISPLAY_CONTROL_SERVICE_NAME, "display-control");
}
