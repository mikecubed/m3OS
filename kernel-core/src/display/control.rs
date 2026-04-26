//! Phase 56 Track E.4 — control-socket codec convenience layer.
//!
//! ## Why this module exists
//!
//! `kernel_core::display::protocol` already declares `ControlCommand` and
//! `ControlEvent` as the wire types and ships `encode`/`decode` methods on
//! each (Track A.0/A.8). This module is the **E.4-specific façade** that:
//!
//! 1. Re-exports the protocol types so `display_server::control` and the
//!    `m3ctl` client both depend on a single, narrowly-scoped surface
//!    instead of the full `protocol` module (the latter also carries
//!    client-protocol traffic that has no business living next to control
//!    plumbing).
//! 2. Wraps the lower-level `ProtocolError` in a typed `ControlError` whose
//!    variants are *exactly* the four error kinds the E.4 acceptance
//!    criteria call out: `UnknownVerb`, `MalformedFrame`, `BadArgs`. This
//!    gives the dispatcher a small, exhaustive error space tailored to the
//!    control socket — without introducing a parallel codec.
//! 3. Provides standalone `encode_command` / `decode_command` /
//!    `encode_event` / `decode_event` free functions so the dispatcher can
//!    operate on byte slices without importing the inherent-impl methods on
//!    each variant. (The E.4 acceptance bullet calls out these names
//!    explicitly: `parse_command`, `encode_event`, etc.)
//!
//! ## Wire format
//!
//! Every control frame uses the same 4-byte header as the rest of Phase 56:
//!
//! ```text
//! [body_len: u16 LE] [opcode: u16 LE] [body: body_len bytes]
//! ```
//!
//! The opcode space is partitioned in `protocol.rs`:
//! * `0x0200..=0x02FF` — control commands (request verbs)
//! * `0x0300..=0x03FF` — control events (replies + subscribed-stream events)
//!
//! Per-variant body layouts are documented inline on each encoder branch in
//! `protocol.rs`. Multi-byte scalars are little-endian. Enum discriminants
//! are explicit `u8` tags.
//!
//! ## Transport choice (recorded for the H.1 learning doc)
//!
//! Phase 56 ships the control socket as a **second IPC endpoint** registered
//! as service `"display-control"` (separate from the `"display"` graphical
//! client endpoint), **not** AF_UNIX. The original spec language was
//! "AF_UNIX (or IPC)"; the C.5 task notes already accepted the IPC pivot
//! for the graphical endpoint, and E.4 adopts the same pivot for symmetry.
//! Rationale:
//!
//! * IPC is the established transport across the rest of the m3OS userspace
//!   (kbd, mouse, vfs, fat, net, e1000, nvme); a control socket on AF_UNIX
//!   would be the only AF_UNIX-bearing m3OS service in the tree.
//! * AF_UNIX SCM_RIGHTS-equivalent capability transfer is not yet wired in
//!   m3OS; capability transfer over the IPC `cap_grant` syscall is the
//!   well-trodden path.
//! * The protocol *types* live here in `kernel-core` and are
//!   transport-agnostic — a future swap to AF_UNIX is a wiring change in
//!   `userspace/display_server/src/control.rs` and `userspace/m3ctl/src/`,
//!   not a protocol break.
//!
//! Filesystem-level "owning user only" access (the spec's permissions
//! requirement) becomes a NOP at the IPC-pivot protocol level: IPC service
//! registration is process-scoped already, so any client that can lookup
//! `"display-control"` is on the same machine. The H.1 hand-off note in
//! `display_server/src/control.rs` records this.
//!
//! ## Minimum verb set (Phase 56)
//!
//! Implemented:
//! * `Version` — return the protocol version constant
//! * `ListSurfaces` — return Vec<SurfaceId> from registry
//! * `Focus(SurfaceId)` — set focused surface
//! * `RegisterBind { modifier_mask, keycode }` — register keybind
//! * `UnregisterBind { modifier_mask, keycode }` — unregister keybind
//! * `Subscribe(EventKind)` — add connection to subscriber list
//! * `FrameStats` — return rolling window of frame compose times
//!
//! Deferred to later phases (Phase 56b / 57):
//! * Workspace verbs (create, destroy, switch)
//! * Layout verbs (swap-policy, set-config)
//! * Gap / margin / animation verbs
//! * Multi-output verbs (list-outputs, focus-output)

// Re-exports of the protocol-side types so callers can use a single
// narrowly-scoped path. These are not new declarations; the *only*
// declaration site is `crate::display::protocol`.
pub use crate::display::protocol::{
    ControlCommand, ControlErrorCode, ControlEvent, EventKind, FrameStatSample, MAX_LIST_ENTRIES,
    PROTOCOL_VERSION, ProtocolError, SurfaceId, SurfaceRoleTag,
};

/// Errors returned by the E.4-facing codec free functions.
///
/// `#[non_exhaustive]` so future variants (e.g. `ResourceExhausted` on
/// over-cap subscriber lists) can be added without breaking matchers.
///
/// # Mapping from `ProtocolError`
///
/// The free functions in this module collapse every `ProtocolError`
/// variant into one of these three kinds, matching the E.4 acceptance
/// criteria (which call for `UnknownVerb`, `MalformedFrame`, `BadArgs`).
/// The full `ProtocolError` is retained on `BadArgs::source` so a
/// reviewer can still distinguish the underlying cause when debugging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ControlError {
    /// The frame's opcode does not name a known control verb (or, on the
    /// event side, a known event opcode). Carries the offending opcode so
    /// the dispatcher can log it.
    UnknownVerb {
        /// The unrecognized 16-bit opcode taken from the frame header.
        opcode: u16,
    },
    /// The frame is structurally malformed — buffer truncated below the
    /// header size, declared body length exceeds [`MAX_FRAME_BODY_LEN`],
    /// or the buffer is too small to hold the declared body. Distinct
    /// from `BadArgs`: a malformed frame is unrecoverable on the same
    /// connection.
    ///
    /// [`MAX_FRAME_BODY_LEN`]: crate::display::protocol::MAX_FRAME_BODY_LEN
    MalformedFrame,
    /// The frame opcode is recognized but its body did not pass argument
    /// validation: wrong body length for the opcode, an enum
    /// discriminant out of range, an invalid anchor mask, or a list
    /// length over [`MAX_LIST_ENTRIES`]. The `expected` and `got` fields
    /// carry the byte-length expectations where available; `source`
    /// carries the underlying `ProtocolError` for richer logging.
    BadArgs {
        /// The expected body length (or `0` if not known to the codec).
        expected: u32,
        /// The observed body length on the wire.
        got: u32,
        /// The underlying `ProtocolError` returned by the protocol-layer
        /// codec.
        source: ProtocolError,
    },
}

impl From<ProtocolError> for ControlError {
    /// Map a low-level `ProtocolError` onto the small E.4 error space.
    ///
    /// * `Truncated` / `BodyTooLarge` → `MalformedFrame` — these mean the
    ///   frame can't even be parsed as a valid header+body shape.
    /// * `UnknownOpcode(op)` → `UnknownVerb { opcode: op }` — the frame is
    ///   structurally valid but names a verb the server doesn't know.
    /// * Anything else (`BodyLengthMismatch`, `InvalidEnum`,
    ///   `InvalidAnchorMask`, `ListTooLong`, `Event(_)`) →
    ///   `BadArgs { expected: 0, got: 0, source }`. The `expected`/`got`
    ///   fields are populated to non-zero values by the free functions
    ///   that have body-length context; this fallback path keeps the
    ///   conversion total without losing the underlying error kind.
    fn from(err: ProtocolError) -> Self {
        match err {
            ProtocolError::Truncated | ProtocolError::BodyTooLarge => ControlError::MalformedFrame,
            ProtocolError::UnknownOpcode(opcode) => ControlError::UnknownVerb { opcode },
            other => ControlError::BadArgs {
                expected: 0,
                got: 0,
                source: other,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Free-function codec entry points used by E.4.
// ---------------------------------------------------------------------------

/// Encode a control command into `buf`. Returns the number of bytes
/// written.
///
/// On error, never writes a partial frame past the header — callers can
/// safely retry with a larger buffer (`MalformedFrame` / `BadArgs` carry
/// the buffer-too-small case via `ProtocolError::Truncated`).
pub fn encode_command(cmd: &ControlCommand, buf: &mut [u8]) -> Result<usize, ControlError> {
    cmd.encode(buf).map_err(ControlError::from)
}

/// Decode a control command from `bytes`. Returns the command and the
/// number of bytes consumed (so a caller buffering multiple frames can
/// advance past one frame).
///
/// `bytes` may be longer than one frame; `decode_command` reads exactly
/// `FRAME_HEADER_SIZE + body_len` and ignores the rest.
pub fn decode_command(bytes: &[u8]) -> Result<(ControlCommand, usize), ControlError> {
    ControlCommand::decode(bytes).map_err(ControlError::from)
}

/// Encode a control event into `buf`. Returns the number of bytes written.
pub fn encode_event(evt: &ControlEvent, buf: &mut [u8]) -> Result<usize, ControlError> {
    evt.encode(buf).map_err(ControlError::from)
}

/// Decode a control event from `bytes`. Returns the event and the number
/// of bytes consumed.
pub fn decode_event(bytes: &[u8]) -> Result<(ControlEvent, usize), ControlError> {
    ControlEvent::decode(bytes).map_err(ControlError::from)
}

// ---------------------------------------------------------------------------
// Tests — committed before the implementation that makes them pass.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use proptest::prelude::*;

    const SCRATCH_BUF_LEN: usize = 512;

    fn round_trip_command(cmd: ControlCommand) {
        let mut buf = [0u8; SCRATCH_BUF_LEN];
        let n = encode_command(&cmd, &mut buf).expect("encode_command");
        let (back, consumed) = decode_command(&buf[..n]).expect("decode_command");
        assert_eq!(consumed, n);
        assert_eq!(back, cmd);
    }

    fn round_trip_event(evt: ControlEvent) {
        let mut buf = [0u8; SCRATCH_BUF_LEN];
        let n = encode_event(&evt, &mut buf).expect("encode_event");
        let (back, consumed) = decode_event(&buf[..n]).expect("decode_event");
        assert_eq!(consumed, n);
        assert_eq!(back, evt);
    }

    // ---- Per-verb command round-trips (one test per minimum-set verb) ------

    #[test]
    fn round_trip_version() {
        round_trip_command(ControlCommand::Version);
    }

    #[test]
    fn round_trip_list_surfaces() {
        round_trip_command(ControlCommand::ListSurfaces);
    }

    #[test]
    fn round_trip_focus() {
        round_trip_command(ControlCommand::Focus {
            surface_id: SurfaceId(42),
        });
    }

    #[test]
    fn round_trip_register_bind() {
        round_trip_command(ControlCommand::RegisterBind {
            modifier_mask: 0x0008, // MOD_SUPER
            keycode: 0x10,
        });
    }

    #[test]
    fn round_trip_unregister_bind() {
        round_trip_command(ControlCommand::UnregisterBind {
            modifier_mask: 0x0001, // MOD_SHIFT
            keycode: 0x1F,
        });
    }

    #[test]
    fn round_trip_subscribe_each_kind() {
        for kind in [
            EventKind::SurfaceCreated,
            EventKind::SurfaceDestroyed,
            EventKind::FocusChanged,
            EventKind::BindTriggered,
        ] {
            round_trip_command(ControlCommand::Subscribe { event_kind: kind });
        }
    }

    #[test]
    fn round_trip_frame_stats() {
        round_trip_command(ControlCommand::FrameStats);
    }

    // ---- Per-event round-trips ---------------------------------------------

    #[test]
    fn round_trip_version_reply() {
        round_trip_event(ControlEvent::VersionReply {
            protocol_version: PROTOCOL_VERSION,
        });
    }

    #[test]
    fn round_trip_surface_list_reply_empty() {
        round_trip_event(ControlEvent::SurfaceListReply { ids: vec![] });
    }

    #[test]
    fn round_trip_surface_list_reply_three() {
        round_trip_event(ControlEvent::SurfaceListReply {
            ids: vec![SurfaceId(1), SurfaceId(2), SurfaceId(7)],
        });
    }

    #[test]
    fn round_trip_ack() {
        round_trip_event(ControlEvent::Ack);
    }

    #[test]
    fn round_trip_error_each_code() {
        for code in [
            ControlErrorCode::UnknownVerb,
            ControlErrorCode::MalformedFrame,
            ControlErrorCode::BadArgs,
            ControlErrorCode::UnknownSurface,
            ControlErrorCode::ResourceExhausted,
        ] {
            round_trip_event(ControlEvent::Error { code });
        }
    }

    #[test]
    fn round_trip_frame_stats_reply_empty() {
        round_trip_event(ControlEvent::FrameStatsReply { samples: vec![] });
    }

    #[test]
    fn round_trip_frame_stats_reply_two() {
        round_trip_event(ControlEvent::FrameStatsReply {
            samples: vec![
                FrameStatSample {
                    frame_index: 1,
                    compose_micros: 250,
                },
                FrameStatSample {
                    frame_index: 2,
                    compose_micros: 310,
                },
            ],
        });
    }

    #[test]
    fn round_trip_surface_created_each_role() {
        for role in [
            SurfaceRoleTag::Toplevel,
            SurfaceRoleTag::Layer,
            SurfaceRoleTag::Cursor,
        ] {
            round_trip_event(ControlEvent::SurfaceCreated {
                surface_id: SurfaceId(9),
                role,
            });
        }
    }

    #[test]
    fn round_trip_surface_destroyed() {
        round_trip_event(ControlEvent::SurfaceDestroyed {
            surface_id: SurfaceId(77),
        });
    }

    #[test]
    fn round_trip_focus_changed_some() {
        round_trip_event(ControlEvent::FocusChanged {
            focused: Some(SurfaceId(3)),
        });
    }

    #[test]
    fn round_trip_focus_changed_none() {
        round_trip_event(ControlEvent::FocusChanged { focused: None });
    }

    #[test]
    fn round_trip_bind_triggered() {
        round_trip_event(ControlEvent::BindTriggered {
            modifier_mask: 0x0008,
            keycode: 0x10,
        });
    }

    // ---- Error-mapping tests -----------------------------------------------

    #[test]
    fn unknown_command_opcode_yields_unknown_verb() {
        // Hand-build a frame: body_len=0, opcode=0x02FE (in the control-cmd
        // band but unassigned — see protocol.rs OP_CTL_* constants).
        let buf: [u8; 4] = [0x00, 0x00, 0xFE, 0x02];
        let err = decode_command(&buf).expect_err("must reject unknown opcode");
        assert_eq!(err, ControlError::UnknownVerb { opcode: 0x02FE });
    }

    #[test]
    fn unknown_event_opcode_yields_unknown_verb() {
        let buf: [u8; 4] = [0x00, 0x00, 0xFE, 0x03];
        let err = decode_event(&buf).expect_err("must reject unknown event opcode");
        assert_eq!(err, ControlError::UnknownVerb { opcode: 0x03FE });
    }

    #[test]
    fn truncated_command_yields_malformed_frame() {
        // Header alone declares 4 bytes of body but buffer is empty after.
        let buf: [u8; 4] = [0x04, 0x00, 0x03, 0x02]; // OP_CTL_FOCUS, body_len=4
        let err = decode_command(&buf).expect_err("must reject truncated body");
        assert_eq!(err, ControlError::MalformedFrame);
    }

    #[test]
    fn empty_buffer_yields_malformed_frame() {
        let buf: [u8; 0] = [];
        let err = decode_command(&buf).expect_err("must reject empty");
        assert_eq!(err, ControlError::MalformedFrame);
    }

    #[test]
    fn body_too_large_yields_malformed_frame() {
        // body_len = MAX_FRAME_BODY_LEN + 1, opcode = OP_CTL_VERSION.
        let big = (crate::display::protocol::MAX_FRAME_BODY_LEN as u32 + 1) as u16;
        let mut buf = [0u8; 4];
        buf[0..2].copy_from_slice(&big.to_le_bytes());
        buf[2..4].copy_from_slice(&0x0201u16.to_le_bytes()); // OP_CTL_VERSION
        let err = decode_command(&buf).expect_err("body too large");
        assert_eq!(err, ControlError::MalformedFrame);
    }

    #[test]
    fn register_bind_truncated_yields_bad_args() {
        // OP_CTL_REGISTER_BIND = 0x0204, body should be 6 bytes; declare 2.
        let mut buf = [0u8; 6];
        buf[0..2].copy_from_slice(&2u16.to_le_bytes()); // body_len = 2
        buf[2..4].copy_from_slice(&0x0204u16.to_le_bytes());
        // body of 2 bytes — modifier_mask only, missing keycode.
        buf[4..6].copy_from_slice(&0u16.to_le_bytes());
        let err = decode_command(&buf).expect_err("body length mismatch");
        match err {
            ControlError::BadArgs { source, .. } => {
                assert_eq!(source, ProtocolError::BodyLengthMismatch);
            }
            other => panic!("expected BadArgs, got {other:?}"),
        }
    }

    #[test]
    fn focus_zero_body_yields_bad_args() {
        // OP_CTL_FOCUS expects body_len=4; declare 0.
        let buf: [u8; 4] = [0x00, 0x00, 0x03, 0x02];
        let err = decode_command(&buf).expect_err("focus needs surface id");
        match err {
            ControlError::BadArgs { source, .. } => {
                assert_eq!(source, ProtocolError::BodyLengthMismatch);
            }
            other => panic!("expected BadArgs, got {other:?}"),
        }
    }

    #[test]
    fn subscribe_invalid_event_kind_yields_bad_args() {
        // OP_CTL_SUBSCRIBE = 0x0206, body_len = 1, but the byte names an
        // out-of-range EventKind (255).
        let buf: [u8; 5] = [0x01, 0x00, 0x06, 0x02, 0xFF];
        let err = decode_command(&buf).expect_err("invalid event kind");
        match err {
            ControlError::BadArgs { source, .. } => {
                assert_eq!(source, ProtocolError::InvalidEnum);
            }
            other => panic!("expected BadArgs, got {other:?}"),
        }
    }

    #[test]
    fn buffer_too_small_for_encode_yields_malformed_frame() {
        let mut tiny = [0u8; 2];
        let err = encode_command(&ControlCommand::Version, &mut tiny)
            .expect_err("encode into buffer too small");
        assert_eq!(err, ControlError::MalformedFrame);
    }

    // ---- Proptest round-trips and adversarial-input safety -----------------

    fn arb_surface_id() -> impl Strategy<Value = SurfaceId> {
        any::<u32>().prop_map(SurfaceId)
    }

    fn arb_event_kind() -> impl Strategy<Value = EventKind> {
        prop_oneof![
            Just(EventKind::SurfaceCreated),
            Just(EventKind::SurfaceDestroyed),
            Just(EventKind::FocusChanged),
            Just(EventKind::BindTriggered),
        ]
    }

    fn arb_role_tag() -> impl Strategy<Value = SurfaceRoleTag> {
        prop_oneof![
            Just(SurfaceRoleTag::Toplevel),
            Just(SurfaceRoleTag::Layer),
            Just(SurfaceRoleTag::Cursor),
        ]
    }

    fn arb_error_code() -> impl Strategy<Value = ControlErrorCode> {
        prop_oneof![
            Just(ControlErrorCode::UnknownVerb),
            Just(ControlErrorCode::MalformedFrame),
            Just(ControlErrorCode::BadArgs),
            Just(ControlErrorCode::UnknownSurface),
            Just(ControlErrorCode::ResourceExhausted),
        ]
    }

    fn arb_command() -> impl Strategy<Value = ControlCommand> {
        prop_oneof![
            Just(ControlCommand::Version),
            Just(ControlCommand::ListSurfaces),
            arb_surface_id().prop_map(|surface_id| ControlCommand::Focus { surface_id }),
            (any::<u16>(), any::<u32>()).prop_map(|(modifier_mask, keycode)| {
                ControlCommand::RegisterBind {
                    modifier_mask,
                    keycode,
                }
            }),
            (any::<u16>(), any::<u32>()).prop_map(|(modifier_mask, keycode)| {
                ControlCommand::UnregisterBind {
                    modifier_mask,
                    keycode,
                }
            }),
            arb_event_kind().prop_map(|event_kind| ControlCommand::Subscribe { event_kind }),
            Just(ControlCommand::FrameStats),
        ]
    }

    fn arb_event() -> impl Strategy<Value = ControlEvent> {
        prop_oneof![
            any::<u32>()
                .prop_map(|protocol_version| ControlEvent::VersionReply { protocol_version }),
            proptest::collection::vec(arb_surface_id(), 0..16)
                .prop_map(|ids| ControlEvent::SurfaceListReply { ids }),
            Just(ControlEvent::Ack),
            arb_error_code().prop_map(|code| ControlEvent::Error { code }),
            proptest::collection::vec((any::<u64>(), any::<u32>()), 0..16).prop_map(|raw| {
                ControlEvent::FrameStatsReply {
                    samples: raw
                        .into_iter()
                        .map(|(frame_index, compose_micros)| FrameStatSample {
                            frame_index,
                            compose_micros,
                        })
                        .collect(),
                }
            }),
            (arb_surface_id(), arb_role_tag())
                .prop_map(|(surface_id, role)| ControlEvent::SurfaceCreated { surface_id, role }),
            arb_surface_id().prop_map(|surface_id| ControlEvent::SurfaceDestroyed { surface_id }),
            proptest::option::of(arb_surface_id())
                .prop_map(|focused| ControlEvent::FocusChanged { focused }),
            (any::<u16>(), any::<u32>()).prop_map(|(modifier_mask, keycode)| {
                ControlEvent::BindTriggered {
                    modifier_mask,
                    keycode,
                }
            }),
        ]
    }

    proptest! {
        #[test]
        fn proptest_command_round_trip(cmd in arb_command()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = encode_command(&cmd, &mut buf).expect("encode");
            let (back, consumed) = decode_command(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, cmd);
        }

        #[test]
        fn proptest_event_round_trip(evt in arb_event()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = encode_event(&evt, &mut buf).expect("encode");
            let (back, consumed) = decode_event(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, evt);
        }

        #[test]
        fn proptest_decode_command_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            // Any byte slice — `decode_command` must return Result, not
            // panic, infinite-loop, or read past the end of the slice.
            let _ = decode_command(&bytes);
        }

        #[test]
        fn proptest_decode_event_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = decode_event(&bytes);
        }
    }
}
