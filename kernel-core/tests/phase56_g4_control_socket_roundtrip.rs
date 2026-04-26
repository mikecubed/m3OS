//! Phase 56 Track G.4 — Control socket round-trip regression (PARTIAL).
//!
//! ## What G.4 wants end-to-end
//!
//! 1. `m3ctl version` returns a non-empty version string matching the
//!    Phase 56 protocol version.
//! 2. `m3ctl list-surfaces` is empty at startup; after a client creates
//!    a `Toplevel`, a second `m3ctl list-surfaces` lists it.
//! 3. `m3ctl subscribe SurfaceCreated` receives an event when a client
//!    creates a new surface.
//! 4. `m3ctl frame-stats` returns a non-empty sample window with
//!    strictly-increasing frame indices and per-sample composition
//!    durations greater than zero.
//! 5. Malformed framing closes the control connection with a named
//!    reason; unknown verbs return an `UnknownCommand` error without
//!    closing.
//!
//! ## What is doable today
//!
//! Bullet 1's *protocol-version constant* path is testable on the host
//! against the production codec: the caller-side encoder produces a
//! `Version` request whose round-trip matches the verb arm `m3ctl`
//! emits, and the server-side `ControlEvent::VersionReply` carries the
//! same `PROTOCOL_VERSION` constant the dispatcher returns. The
//! pure-logic codec is what guarantees "non-empty, well-formed".
//!
//! Bullet 5's *unknown-verb-without-close* invariant is partly testable
//! on the host: the codec defines an `UnknownVerb` error variant that
//! the dispatcher emits without dropping the connection.
//!
//! ## What is deferred (Phase 56 close-out)
//!
//! The userspace bulk-drain syscall has now landed
//! (`SYS_IPC_TAKE_PENDING_BULK = 0x1112` /
//! `syscall_lib::ipc_take_pending_bulk`) and `m3ctl` decodes real
//! reply bulk via `decode_event(&reply_buf[..n])` instead of the
//! `synthetic_reply_for` seam (the synthetic path now only handles
//! legitimate zero-bulk replies). The remaining deferral is purely
//! the *integration harness*:
//!
//! Bullets 2 and 3 (`list-surfaces` after `Toplevel` create,
//! `subscribe SurfaceCreated` event delivery) need a QEMU-based
//! regression that drives a multi-process scenario: launch
//! `display_server`, launch `m3ctl subscribe SurfaceCreated`, launch
//! a separate test client that creates a `Toplevel`, then assert the
//! event lands. The host-process `cargo test` cannot orchestrate
//! that — the codec round-trip tests below pin the wire-format
//! invariants the runtime path will rely on, but the cross-process
//! verification belongs in a separate `cargo xtask regression
//! --test display-control-socket` smoke binary.
//!
//! Bullet 4's runtime-frame-stats portion is similarly QEMU-only:
//! it requires actual frames to be composed (clock-driven), which
//! the host pure-logic harness does not produce.
//!
//! Bullet 5's *malformed-framing-closes-connection* path also
//! belongs in the QEMU smoke since it requires observing a
//! kernel-side connection close.
//!
//! Subscription delivery is still the pull/queue-driven model
//! (control-socket subscribers poll); a richer event-push design is
//! deferred to a separate follow-on phase.
//!
//! See also `phase56_g1_multi_client_coexistence.rs` for the matching
//! G.1 narrative on the same QEMU-harness gap.

#![cfg(feature = "std")]

use kernel_core::display::control::{decode_command, decode_event, encode_command, encode_event};
use kernel_core::display::protocol::{
    ControlCommand, ControlErrorCode, ControlEvent, FrameStatSample, PROTOCOL_VERSION,
};

#[test]
fn version_command_round_trips_through_codec() {
    // G.4 acceptance bullet 1, partial — the codec round-trip is the
    // pre-condition for `m3ctl version` returning a non-empty string.
    // If this regresses, the runtime regression cannot succeed even
    // when bulk-drain is in place.
    let cmd = ControlCommand::Version;
    let mut buf = [0u8; 16];
    let written = encode_command(&cmd, &mut buf).expect("encode_command should succeed");
    let (decoded, consumed) =
        decode_command(&buf[..written]).expect("decode_command should succeed");
    assert_eq!(decoded, cmd);
    assert_eq!(consumed, written);
}

#[test]
fn version_reply_carries_phase56_protocol_version() {
    // G.4 acceptance bullet 1, partial — the `VersionReply` event a
    // dispatcher emits is the same `PROTOCOL_VERSION` constant.
    // `PROTOCOL_VERSION > 0` is the minimum non-empty signal.
    let event = ControlEvent::VersionReply {
        protocol_version: PROTOCOL_VERSION,
    };
    let mut buf = [0u8; 16];
    let written = encode_event(&event, &mut buf).expect("encode_event should succeed");
    let (decoded, consumed) = decode_event(&buf[..written]).expect("decode_event should succeed");
    assert_eq!(decoded, event);
    assert_eq!(consumed, written);
    assert!(
        PROTOCOL_VERSION > 0,
        "non-empty protocol-version invariant the runtime caller relies on",
    );
}

#[test]
fn unknown_verb_error_is_codec_recognised() {
    // G.4 acceptance bullet 5, partial — the dispatcher emits
    // `Error { code: UnknownVerb }` for unknown verbs without closing
    // the connection. The host-side codec round-trip is the
    // pre-condition for `m3ctl` displaying a usable error message
    // when bulk-drain lands.
    let event = ControlEvent::Error {
        code: ControlErrorCode::UnknownVerb,
    };
    let mut buf = [0u8; 16];
    let written = encode_event(&event, &mut buf).expect("encode_event should succeed");
    let (decoded, _consumed) = decode_event(&buf[..written]).expect("decode_event should succeed");
    assert_eq!(decoded, event);
}

#[test]
fn frame_stats_reply_round_trips_with_strictly_increasing_indices() {
    // G.4 acceptance bullet 4, partial — the codec carries a
    // `FrameStatsReply` shaped exactly the way the runtime caller
    // expects. Strict-increasing-index + positive-compose-duration
    // are runtime invariants the *dispatcher* maintains; the codec
    // pin here is "the wire format does not lossily collapse the
    // sample order".
    let samples: Vec<FrameStatSample> = (1u32..=4u32)
        .map(|i| FrameStatSample {
            frame_index: i as u64,
            compose_micros: i * 100,
        })
        .collect();
    let event = ControlEvent::FrameStatsReply {
        samples: samples.clone(),
    };
    let mut buf = [0u8; 256];
    let written = encode_event(&event, &mut buf).expect("encode_event should succeed");
    let (decoded, consumed) = decode_event(&buf[..written]).expect("decode_event should succeed");
    assert_eq!(decoded, event);
    assert_eq!(consumed, written);

    // Strict-increasing-index invariant pinned at the codec level so a
    // future codec rewrite cannot silently re-order entries.
    if let ControlEvent::FrameStatsReply {
        samples: round_tripped,
    } = decoded
    {
        for window in round_tripped.windows(2) {
            assert!(
                window[1].frame_index > window[0].frame_index,
                "decoded samples must remain strictly increasing in frame_index",
            );
        }
    } else {
        panic!("decoded event variant should be FrameStatsReply");
    }
}

#[test]
#[ignore = "Phase 56 G.4 runtime control-socket round-trip regression \
            (list-surfaces / subscribe / frame-stats live data) belongs \
            in a QEMU smoke (`cargo xtask regression --test \
            display-control-socket`); the bulk-drain transport landed \
            in this PR but cross-process orchestration is out of scope \
            for host-process `cargo test`. See file header."]
fn runtime_list_surfaces_subscribe_and_frame_stats_belongs_in_qemu_smoke() {
    panic!(
        "G.4 runtime control-socket regression belongs in a QEMU smoke. \
         The bulk-drain transport (ipc_take_pending_bulk) is in place; \
         the codec round-trip tests in this file pin the wire-format \
         invariants the smoke depends on. The smoke binary itself is \
         tracked in the Phase 56 follow-up plan."
    );
}
