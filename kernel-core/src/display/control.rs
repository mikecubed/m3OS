//! Phase 56 Track E.4 — control-socket codec convenience layer (failing-test stub).
//!
//! Tests for the `ControlError` shape and the `encode_command` /
//! `decode_command` / `encode_event` / `decode_event` free functions are
//! committed first so the failing-then-green discipline is auditable in
//! the git history.

// Re-exports of the protocol-side types so the test module compiles
// against the same names as the eventual implementation.
pub use crate::display::protocol::{
    ControlCommand, ControlErrorCode, ControlEvent, EventKind, FrameStatSample, MAX_LIST_ENTRIES,
    PROTOCOL_VERSION, ProtocolError, SurfaceId, SurfaceRoleTag,
};

// `ControlError` and the four codec functions are intentionally
// undeclared here — the tests below reference them and the build will
// fail with E0433 until the implementation lands in the next commit.

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
            modifier_mask: 0x0008,
            keycode: 0x10,
        });
    }

    #[test]
    fn round_trip_unregister_bind() {
        round_trip_command(ControlCommand::UnregisterBind {
            modifier_mask: 0x0001,
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

    #[test]
    fn round_trip_version_reply() {
        round_trip_event(ControlEvent::VersionReply {
            protocol_version: PROTOCOL_VERSION,
        });
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

    #[test]
    fn unknown_command_opcode_yields_unknown_verb() {
        let buf: [u8; 4] = [0x00, 0x00, 0xFE, 0x02];
        let err = decode_command(&buf).expect_err("must reject unknown opcode");
        assert_eq!(err, ControlError::UnknownVerb { opcode: 0x02FE });
    }

    #[test]
    fn truncated_command_yields_malformed_frame() {
        let buf: [u8; 4] = [0x04, 0x00, 0x03, 0x02];
        let err = decode_command(&buf).expect_err("must reject truncated body");
        assert_eq!(err, ControlError::MalformedFrame);
    }

    #[test]
    fn register_bind_truncated_yields_bad_args() {
        let mut buf = [0u8; 6];
        buf[0..2].copy_from_slice(&2u16.to_le_bytes());
        buf[2..4].copy_from_slice(&0x0204u16.to_le_bytes());
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
    fn buffer_too_small_for_encode_yields_malformed_frame() {
        let mut tiny = [0u8; 2];
        let err = encode_command(&ControlCommand::Version, &mut tiny)
            .expect_err("encode into buffer too small");
        assert_eq!(err, ControlError::MalformedFrame);
    }

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
            arb_event_kind().prop_map(|event_kind| ControlCommand::Subscribe { event_kind }),
            Just(ControlCommand::FrameStats),
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
        fn proptest_decode_command_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = decode_command(&bytes);
        }
    }
}
