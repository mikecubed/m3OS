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
//! ## What is deferred
//!
//! Bullets 2, 3, and the runtime-frame-stats portion of bullet 4 all
//! require the userspace bulk-drain syscall to land — without it the
//! caller cannot decode the kernel-staged reply bulk and must fall
//! back to `m3ctl`'s `synthetic_reply_for(&cmd)` arm. The
//! `// TODO(C.5-bulk-drain)` markers in `userspace/m3ctl/src/main.rs`
//! pin the swap point.
//!
//! Bullet 5's *malformed-framing-closes-connection* path also requires
//! a runtime caller observing a kernel-side close; that lands when
//! bulk-drain ships and the regression can wire a deliberately-bad
//! frame through the live socket.
//!
//! ## How to lift the deferral
//!
//! When `syscall_lib::ipc_take_pending_bulk` (or equivalent) lands:
//!
//! 1. Replace `m3ctl::synthetic_reply_for` with `decode_event(reply_bulk)`.
//! 2. Add a `cargo xtask regression --test control-socket` that boots
//!    QEMU, runs `m3ctl version`, asserts the *real* reply path
//!    (not the synthetic seam) printed the protocol-version string.
//! 3. Add the `list-surfaces` / `subscribe SurfaceCreated` /
//!    `frame-stats` runtime checks per the bullets above.
//!
//! See also `phase56_g1_multi_client_coexistence.rs` for the full
//! bulk-drain deferral rationale shared across G.1, G.2, and G.4.

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
            (list-surfaces / subscribe / frame-stats live data) deferred \
            behind the userspace bulk-drain gap (TODO(C.5-bulk-drain)); \
            see file header for the lift plan."]
fn runtime_list_surfaces_subscribe_and_frame_stats_deferred() {
    panic!(
        "G.4 runtime control-socket round-trip regression is deferred \
         behind the userspace bulk-drain gap. The codec round-trip \
         tests in this file pin the wire-format invariants the runtime \
         path will depend on; the runtime-byte-flow portion lifts \
         when bulk-drain ships."
    );
}
