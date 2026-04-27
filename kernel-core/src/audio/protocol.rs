//! Audio wire-format codec — Phase 57 B.3.
//!
//! Tests-only commit. The implementation lands in the next commit; these
//! tests are committed first to satisfy the red-before-green TDD rule.
//!
//! Mimics the shape of `kernel-core::display::protocol`: length-prefixed
//! framing, opcode-dispatched encoders, per-variant body layouts, with
//! `encode(msg, &mut buf) -> Result<usize, ProtocolError>` returning
//! bytes-written, and `decode(&[u8]) -> Result<(Message, usize),
//! ProtocolError>` returning parsed message and bytes consumed.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{ChannelLayout, PcmFormat, SampleRate};
    use proptest::prelude::*;

    // -----------------------------------------------------------------------
    // Per-variant round-trip tests
    // -----------------------------------------------------------------------

    fn roundtrip_client(msg: ClientMessage) {
        let mut buf = [0u8; 256];
        let written = msg.encode(&mut buf).expect("encode");
        let (decoded, consumed) = ClientMessage::decode(&buf[..written]).expect("decode");
        assert_eq!(decoded, msg);
        assert_eq!(consumed, written);
    }

    fn roundtrip_server(msg: ServerMessage) {
        let mut buf = [0u8; 256];
        let written = msg.encode(&mut buf).expect("encode");
        let (decoded, consumed) = ServerMessage::decode(&buf[..written]).expect("decode");
        assert_eq!(decoded, msg);
        assert_eq!(consumed, written);
    }

    fn roundtrip_ctl_cmd(msg: AudioControlCommand) {
        let mut buf = [0u8; 256];
        let written = msg.encode(&mut buf).expect("encode");
        let (decoded, consumed) =
            AudioControlCommand::decode(&buf[..written]).expect("decode");
        assert_eq!(decoded, msg);
        assert_eq!(consumed, written);
    }

    fn roundtrip_ctl_evt(msg: AudioControlEvent) {
        let mut buf = [0u8; 256];
        let written = msg.encode(&mut buf).expect("encode");
        let (decoded, consumed) =
            AudioControlEvent::decode(&buf[..written]).expect("decode");
        assert_eq!(decoded, msg);
        assert_eq!(consumed, written);
    }

    #[test]
    fn client_open_roundtrip() {
        roundtrip_client(ClientMessage::Open {
            format: PcmFormat::S16Le,
            layout: ChannelLayout::Stereo,
            rate: SampleRate::Hz48000,
        });
    }

    #[test]
    fn client_open_mono_roundtrip() {
        roundtrip_client(ClientMessage::Open {
            format: PcmFormat::S16Le,
            layout: ChannelLayout::Mono,
            rate: SampleRate::Hz48000,
        });
    }

    #[test]
    fn client_submit_frames_roundtrip() {
        roundtrip_client(ClientMessage::SubmitFrames { len: 4096 });
    }

    #[test]
    fn client_drain_roundtrip() {
        roundtrip_client(ClientMessage::Drain);
    }

    #[test]
    fn client_close_roundtrip() {
        roundtrip_client(ClientMessage::Close);
    }

    #[test]
    fn client_control_command_get_stats_roundtrip() {
        roundtrip_client(ClientMessage::ControlCommand(
            AudioControlCommand::GetStats,
        ));
    }

    #[test]
    fn server_opened_roundtrip() {
        roundtrip_server(ServerMessage::Opened { stream_id: 42 });
    }

    #[test]
    fn server_open_error_roundtrip() {
        for err in [
            AudioError::Busy,
            AudioError::WouldBlock,
            AudioError::NoDevice,
            AudioError::BrokenPipe,
            AudioError::InvalidFormat,
            AudioError::InvalidArgument,
            AudioError::Internal,
        ] {
            roundtrip_server(ServerMessage::OpenError(err));
        }
    }

    #[test]
    fn server_submit_ack_roundtrip() {
        roundtrip_server(ServerMessage::SubmitAck {
            frames_consumed: 1_234_567_890,
        });
    }

    #[test]
    fn server_submit_error_roundtrip() {
        roundtrip_server(ServerMessage::SubmitError(AudioError::WouldBlock));
    }

    #[test]
    fn server_drain_ack_roundtrip() {
        roundtrip_server(ServerMessage::DrainAck);
    }

    #[test]
    fn server_closed_roundtrip() {
        roundtrip_server(ServerMessage::Closed);
    }

    #[test]
    fn server_control_event_stats_roundtrip() {
        roundtrip_server(ServerMessage::ControlEvent(AudioControlEvent::Stats {
            underrun_count: 7,
            frames_submitted: 1_000_000,
            frames_consumed: 999_000,
        }));
    }

    #[test]
    fn ctl_command_get_stats_roundtrip() {
        roundtrip_ctl_cmd(AudioControlCommand::GetStats);
    }

    #[test]
    fn ctl_event_stats_roundtrip() {
        roundtrip_ctl_evt(AudioControlEvent::Stats {
            underrun_count: 0,
            frames_submitted: 0,
            frames_consumed: 0,
        });
    }

    // -----------------------------------------------------------------------
    // Buffer-shape tests
    // -----------------------------------------------------------------------

    #[test]
    fn encode_into_undersized_buffer_returns_truncated() {
        let mut buf = [0u8; 1];
        let result = ClientMessage::Drain.encode(&mut buf);
        assert!(matches!(result, Err(ProtocolError::Truncated)));
    }

    #[test]
    fn decode_from_empty_buffer_returns_truncated() {
        let result = ClientMessage::decode(&[]);
        assert!(matches!(result, Err(ProtocolError::Truncated)));
    }

    #[test]
    fn submit_frames_rejects_oversize_len() {
        // The protocol caps a single SubmitFrames at MAX_SUBMIT_BYTES.
        // Encoding a SubmitFrames with len > MAX_SUBMIT_BYTES is a
        // policy violation surfaced as ProtocolError::PayloadTooLarge.
        let result = ClientMessage::SubmitFrames {
            len: MAX_SUBMIT_BYTES as u32 + 1,
        }
        .encode(&mut [0u8; 64]);
        assert!(matches!(result, Err(ProtocolError::PayloadTooLarge)));
    }

    #[test]
    fn submit_frames_at_max_is_accepted() {
        // Boundary: exactly MAX_SUBMIT_BYTES is OK.
        roundtrip_client(ClientMessage::SubmitFrames {
            len: MAX_SUBMIT_BYTES as u32,
        });
    }

    #[test]
    fn max_submit_bytes_is_64k() {
        // The Phase 57 audio ABI memo pins MAX_SUBMIT_BYTES = 64 KiB.
        // Drift in this constant breaks the audio_server / audio_client
        // ring-budget contract.
        assert_eq!(MAX_SUBMIT_BYTES, 64 * 1024);
    }

    // -----------------------------------------------------------------------
    // Decoder hardness against arbitrary input
    // -----------------------------------------------------------------------

    #[test]
    fn decoder_rejects_unknown_opcode_without_panic() {
        // Frame: body_len = 0, opcode = 0xFFFF (definitely unknown).
        let buf = [0u8, 0u8, 0xFFu8, 0xFFu8];
        let result = ClientMessage::decode(&buf);
        assert!(matches!(result, Err(ProtocolError::UnknownOpcode(_))));
    }

    #[test]
    fn decoder_rejects_undersized_body_without_panic() {
        // Body length claim says 100 but the buffer only carries 4 bytes.
        let buf = [100u8, 0u8, 0u8, 0u8];
        let result = ClientMessage::decode(&buf);
        assert!(matches!(result, Err(ProtocolError::Truncated)));
    }

    // -----------------------------------------------------------------------
    // Property tests (≥1024 cases per family)
    // -----------------------------------------------------------------------

    fn any_audio_error() -> impl Strategy<Value = AudioError> {
        prop_oneof![
            Just(AudioError::Busy),
            Just(AudioError::WouldBlock),
            Just(AudioError::NoDevice),
            Just(AudioError::BrokenPipe),
            Just(AudioError::InvalidFormat),
            Just(AudioError::InvalidArgument),
            Just(AudioError::Internal),
        ]
    }

    fn any_ctl_command() -> impl Strategy<Value = AudioControlCommand> {
        Just(AudioControlCommand::GetStats)
    }

    fn any_ctl_event() -> impl Strategy<Value = AudioControlEvent> {
        (any::<u32>(), any::<u64>(), any::<u64>()).prop_map(
            |(underrun_count, frames_submitted, frames_consumed)| AudioControlEvent::Stats {
                underrun_count,
                frames_submitted,
                frames_consumed,
            },
        )
    }

    fn any_client_message() -> impl Strategy<Value = ClientMessage> {
        prop_oneof![
            Just(ClientMessage::Open {
                format: PcmFormat::S16Le,
                layout: ChannelLayout::Mono,
                rate: SampleRate::Hz48000,
            }),
            Just(ClientMessage::Open {
                format: PcmFormat::S16Le,
                layout: ChannelLayout::Stereo,
                rate: SampleRate::Hz48000,
            }),
            (0u32..=MAX_SUBMIT_BYTES as u32)
                .prop_map(|len| ClientMessage::SubmitFrames { len }),
            Just(ClientMessage::Drain),
            Just(ClientMessage::Close),
            any_ctl_command().prop_map(ClientMessage::ControlCommand),
        ]
    }

    fn any_server_message() -> impl Strategy<Value = ServerMessage> {
        prop_oneof![
            any::<u32>().prop_map(|stream_id| ServerMessage::Opened { stream_id }),
            any_audio_error().prop_map(ServerMessage::OpenError),
            any::<u64>()
                .prop_map(|frames_consumed| ServerMessage::SubmitAck { frames_consumed }),
            any_audio_error().prop_map(ServerMessage::SubmitError),
            Just(ServerMessage::DrainAck),
            Just(ServerMessage::Closed),
            any_ctl_event().prop_map(ServerMessage::ControlEvent),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn client_message_roundtrip(msg in any_client_message()) {
            let mut buf = [0u8; 256];
            let written = msg.encode(&mut buf).expect("encode");
            let (decoded, consumed) = ClientMessage::decode(&buf[..written]).expect("decode");
            prop_assert_eq!(decoded, msg);
            prop_assert_eq!(consumed, written);
        }

        #[test]
        fn server_message_roundtrip(msg in any_server_message()) {
            let mut buf = [0u8; 256];
            let written = msg.encode(&mut buf).expect("encode");
            let (decoded, consumed) = ServerMessage::decode(&buf[..written]).expect("decode");
            prop_assert_eq!(decoded, msg);
            prop_assert_eq!(consumed, written);
        }

        #[test]
        fn ctl_command_roundtrip(msg in any_ctl_command()) {
            let mut buf = [0u8; 64];
            let written = msg.encode(&mut buf).expect("encode");
            let (decoded, consumed) =
                AudioControlCommand::decode(&buf[..written]).expect("decode");
            prop_assert_eq!(decoded, msg);
            prop_assert_eq!(consumed, written);
        }

        #[test]
        fn ctl_event_roundtrip(msg in any_ctl_event()) {
            let mut buf = [0u8; 64];
            let written = msg.encode(&mut buf).expect("encode");
            let (decoded, consumed) =
                AudioControlEvent::decode(&buf[..written]).expect("decode");
            prop_assert_eq!(decoded, msg);
            prop_assert_eq!(consumed, written);
        }

        /// Corrupted-framing test: arbitrary input bytes feed into every
        /// decoder. The decoder must return a typed ProtocolError or a
        /// successful parse without panicking, infinite-looping, or
        /// allocating unboundedly. The test framework's wall-clock test
        /// timeout catches infinite loops; bounded buffers (1024 bytes
        /// max) cap the input size so unbounded allocation cannot
        /// silently succeed.
        #[test]
        fn decoders_never_panic_on_arbitrary_input(
            bytes in proptest::collection::vec(any::<u8>(), 0..=1024),
        ) {
            let _ = ClientMessage::decode(&bytes);
            let _ = ServerMessage::decode(&bytes);
            let _ = AudioControlCommand::decode(&bytes);
            let _ = AudioControlEvent::decode(&bytes);
        }
    }
}
