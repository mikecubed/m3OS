//! Phase 57 Track H.2 — stub reply logic for hardware-absent boot.
//!
//! When `audio_server` cannot claim the AC'97 sentinel BDF it falls
//! back to a no-op IPC loop rather than exiting.  The loop's
//! per-message reply decision is extracted here as a pure function so
//! the host test suite can exercise every protocol arm without a real
//! kernel endpoint.
//!
//! # Why a separate module?
//!
//! `main.rs` is gated on `#[cfg(not(test))]` at the function level so
//! the binary does not emit an `_start` symbol during host-side `cargo
//! test -p audio_server` runs.  Extracting `stub_reply_for` into this
//! `lib`-visible module lets tests import and exercise the exact same
//! dispatch table that the production stub loop uses, satisfying the
//! Phase 57a H.2 acceptance criterion without needing an in-QEMU
//! integration path for the no-hardware branch.

use kernel_core::audio::{AudioControlEvent, AudioError, ClientMessage, ServerMessage};

/// Compute the stub reply for a single decoded [`ClientMessage`].
///
/// This is the same dispatch table used inside `run_stub_loop` in
/// `main.rs`.  All PCM data is silently discarded — there is no
/// hardware to push it to.
///
/// | Input message            | Reply                              |
/// |--------------------------|-------------------------------------|
/// | `Open { .. }`            | `Opened { stream_id: 0 }`          |
/// | `SubmitFrames { .. }`    | `SubmitAck { frames_consumed: 0 }` |
/// | `Drain`                  | `DrainAck`                         |
/// | `Close`                  | `Closed`                           |
/// | `ControlCommand(_)`      | `ControlEvent(Stats { 0, 0, 0 })`  |
/// | everything else          | `SubmitError(InvalidArgument)`     |
pub fn stub_reply_for(msg: &ClientMessage) -> ServerMessage {
    match msg {
        ClientMessage::Open { .. } => ServerMessage::Opened { stream_id: 0 },
        ClientMessage::SubmitFrames { .. } => ServerMessage::SubmitAck { frames_consumed: 0 },
        ClientMessage::Drain => ServerMessage::DrainAck,
        ClientMessage::Close => ServerMessage::Closed,
        ClientMessage::ControlCommand(_) => ServerMessage::ControlEvent(AudioControlEvent::Stats {
            underrun_count: 0,
            frames_submitted: 0,
            frames_consumed: 0,
        }),
        _ => ServerMessage::SubmitError(AudioError::InvalidArgument),
    }
}

// ---------------------------------------------------------------------------
// Tests — H.2 acceptance: stub_reply_for returns the right reply shape
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::audio::{AudioControlCommand, ChannelLayout, PcmFormat, SampleRate};

    /// `Open` produces `Opened { stream_id: 0 }` regardless of format.
    #[test]
    fn stub_open_returns_opened_stream_zero() {
        let msg = ClientMessage::Open {
            format: PcmFormat::S16Le,
            layout: ChannelLayout::Stereo,
            rate: SampleRate::Hz48000,
        };
        let reply = stub_reply_for(&msg);
        assert_eq!(reply, ServerMessage::Opened { stream_id: 0 });
    }

    /// `SubmitFrames` returns `SubmitAck { frames_consumed: 0 }` — PCM
    /// data is discarded silently.
    #[test]
    fn stub_submit_discards_pcm_and_returns_ack_zero() {
        let reply = stub_reply_for(&ClientMessage::SubmitFrames { len: 4096 });
        assert_eq!(reply, ServerMessage::SubmitAck { frames_consumed: 0 });
    }

    /// A zero-length `SubmitFrames` is also a silent discard.
    #[test]
    fn stub_submit_zero_bytes_returns_ack_zero() {
        let reply = stub_reply_for(&ClientMessage::SubmitFrames { len: 0 });
        assert_eq!(reply, ServerMessage::SubmitAck { frames_consumed: 0 });
    }

    /// `Drain` returns `DrainAck`.
    #[test]
    fn stub_drain_returns_drain_ack() {
        let reply = stub_reply_for(&ClientMessage::Drain);
        assert_eq!(reply, ServerMessage::DrainAck);
    }

    /// `Close` returns `Closed`.
    #[test]
    fn stub_close_returns_closed() {
        let reply = stub_reply_for(&ClientMessage::Close);
        assert_eq!(reply, ServerMessage::Closed);
    }

    /// `ControlCommand(GetStats)` returns an all-zero `Stats` event.
    #[test]
    fn stub_get_stats_returns_zero_stats() {
        let reply = stub_reply_for(&ClientMessage::ControlCommand(
            AudioControlCommand::GetStats,
        ));
        match reply {
            ServerMessage::ControlEvent(AudioControlEvent::Stats {
                underrun_count,
                frames_submitted,
                frames_consumed,
            }) => {
                assert_eq!(underrun_count, 0, "stub reports zero underruns");
                assert_eq!(frames_submitted, 0, "stub reports zero frames_submitted");
                assert_eq!(frames_consumed, 0, "stub reports zero frames_consumed");
            }
            other => panic!("expected Stats ControlEvent, got {:?}", other),
        }
    }

    /// All stub replies encode to valid wire frames — no encode panic.
    #[test]
    fn stub_all_replies_encode_without_error() {
        let messages = [
            ClientMessage::Open {
                format: PcmFormat::S16Le,
                layout: ChannelLayout::Stereo,
                rate: SampleRate::Hz48000,
            },
            ClientMessage::SubmitFrames { len: 512 },
            ClientMessage::Drain,
            ClientMessage::Close,
            ClientMessage::ControlCommand(AudioControlCommand::GetStats),
        ];
        for msg in &messages {
            let reply = stub_reply_for(msg);
            let mut buf = [0u8; 64];
            let result = reply.encode(&mut buf);
            assert!(
                result.is_ok(),
                "encode failed for reply to {:?}: {:?}",
                msg,
                result
            );
        }
    }

    /// Verify the stub reply table is consistent with what `main.rs`'s
    /// `run_stub_loop` inline-decode produces: the same `ClientMessage`
    /// encoded to bytes then decoded returns the same reply.
    #[test]
    fn stub_reply_consistent_after_wire_roundtrip() {
        let messages = [
            ClientMessage::Open {
                format: PcmFormat::S16Le,
                layout: ChannelLayout::Mono,
                rate: SampleRate::Hz48000,
            },
            ClientMessage::SubmitFrames { len: 128 },
            ClientMessage::Drain,
            ClientMessage::Close,
        ];
        for msg in &messages {
            let expected = stub_reply_for(msg);
            let mut wire = [0u8; 64];
            let n = msg.encode(&mut wire).expect("encode");
            let (decoded, _) = ClientMessage::decode(&wire[..n]).expect("decode");
            let actual = stub_reply_for(&decoded);
            assert_eq!(
                expected, actual,
                "reply shape changed after wire round-trip for {:?}",
                msg
            );
        }
    }
}
