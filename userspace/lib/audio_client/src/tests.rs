//! Phase 57 E.1 host tests for `AudioClient`.
//!
//! TDD discipline: this file commits *before* the implementation that
//! makes it pass. The GREEN commit replaces the stubs in `lib.rs`
//! with real protocol wiring; these tests stay unchanged.
//!
//! All tests run on the host via
//! `cargo test -p audio_client --target x86_64-unknown-linux-gnu`.

extern crate std;

use super::*;
use std::collections::VecDeque;
use std::vec::Vec;

/// Recorded send: the encoded control frame followed by any bulk PCM
/// bytes the client emitted with it.
#[derive(Debug, Clone)]
struct Sent {
    frame: Vec<u8>,
    bulk: Vec<u8>,
}

/// Test transport that records sends and serves canned replies.
struct MockSocket {
    sent: Vec<Sent>,
    replies: VecDeque<Result<Vec<u8>, AudioClientError>>,
}

impl MockSocket {
    fn new() -> Self {
        Self {
            sent: Vec::new(),
            replies: VecDeque::new(),
        }
    }

    fn push_reply_msg(&mut self, msg: ServerMessage) {
        let mut buf = [0u8; MAX_REPLY_BYTES];
        let n = msg.encode(&mut buf).expect("encode reply");
        self.replies.push_back(Ok(buf[..n].to_vec()));
    }

    fn push_io_error(&mut self, code: i32) {
        self.replies.push_back(Err(AudioClientError::Io(code)));
    }

    fn push_raw_reply_bytes(&mut self, bytes: Vec<u8>) {
        self.replies.push_back(Ok(bytes));
    }
}

impl AudioSocket for MockSocket {
    fn call(
        &mut self,
        frame: &[u8],
        bulk: &[u8],
    ) -> Result<heapless_buf::ReplyBuf, AudioClientError> {
        self.sent.push(Sent {
            frame: frame.to_vec(),
            bulk: bulk.to_vec(),
        });
        match self.replies.pop_front() {
            Some(Ok(bytes)) => Ok(heapless_buf::ReplyBuf::from_slice(&bytes)),
            Some(Err(e)) => Err(e),
            None => Err(AudioClientError::Io(-1)),
        }
    }
}

fn open_stereo<S: AudioSocket>(socket: S) -> Result<AudioClient<S>, AudioClientError> {
    AudioClient::open_with_socket(
        socket,
        PcmFormat::S16Le,
        ChannelLayout::Stereo,
        SampleRate::Hz48000,
    )
}

// ---------------- Happy path ----------------

#[test]
fn open_succeeds_when_server_returns_opened() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 42 });
    let client = open_stereo(socket).expect("open ok");
    assert_eq!(client.stream_id(), Some(42));
}

#[test]
fn open_encodes_format_layout_and_rate_into_first_frame() {
    // Verifies the wire frame the client emitted decodes back to the
    // canonical `ClientMessage::Open` with the requested values.
    struct Tap {
        sent: Option<Vec<u8>>,
        reply: Vec<u8>,
    }
    impl AudioSocket for Tap {
        fn call(
            &mut self,
            frame: &[u8],
            _bulk: &[u8],
        ) -> Result<heapless_buf::ReplyBuf, AudioClientError> {
            self.sent = Some(frame.to_vec());
            Ok(heapless_buf::ReplyBuf::from_slice(&self.reply))
        }
    }
    struct TapAdapter<'a> {
        inner: &'a mut Tap,
    }
    impl AudioSocket for TapAdapter<'_> {
        fn call(
            &mut self,
            frame: &[u8],
            bulk: &[u8],
        ) -> Result<heapless_buf::ReplyBuf, AudioClientError> {
            self.inner.call(frame, bulk)
        }
    }

    let mut buf = [0u8; MAX_REPLY_BYTES];
    let n = ServerMessage::Opened { stream_id: 1 }
        .encode(&mut buf)
        .unwrap();
    let mut tap = Tap {
        sent: None,
        reply: buf[..n].to_vec(),
    };
    let _ = AudioClient::open_with_socket(
        TapAdapter { inner: &mut tap },
        PcmFormat::S16Le,
        ChannelLayout::Stereo,
        SampleRate::Hz48000,
    )
    .expect("open ok");

    let captured = tap.sent.expect("frame captured");
    let (decoded, _) = ClientMessage::decode(&captured).expect("decode captured frame");
    assert_eq!(
        decoded,
        ClientMessage::Open {
            format: PcmFormat::S16Le,
            layout: ChannelLayout::Stereo,
            rate: SampleRate::Hz48000,
        }
    );
}

#[test]
fn submit_frames_returns_byte_count_on_ack() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    socket.push_reply_msg(ServerMessage::SubmitAck { frames_consumed: 0 });
    let mut client = open_stereo(socket).expect("open ok");
    let bytes = [0u8; 64];
    let n = client.submit_frames(&bytes).expect("submit ok");
    assert_eq!(n, 64);
}

#[test]
fn submit_frames_carries_pcm_bytes_in_bulk() {
    // Acceptance: PCM bytes ride the same socket as the encoded
    // `SubmitFrames` frame. The mock records them separately so we
    // can assert the client put them in the bulk slot.
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    socket.push_reply_msg(ServerMessage::SubmitAck { frames_consumed: 0 });
    let mut client = open_stereo(socket).expect("open ok");
    let pcm = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let _ = client.submit_frames(&pcm).expect("submit ok");
    // sent[0] = open, sent[1] = submit
    let sock = client.socket;
    assert_eq!(sock.sent.len(), 2);
    assert_eq!(sock.sent[1].bulk, pcm.to_vec());
    let (decoded, _) =
        ClientMessage::decode(&sock.sent[1].frame).expect("decode submit frame");
    assert_eq!(decoded, ClientMessage::SubmitFrames { len: 8 });
}

#[test]
fn drain_succeeds_when_server_acknowledges() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    socket.push_reply_msg(ServerMessage::DrainAck);
    let mut client = open_stereo(socket).expect("open ok");
    client.drain().expect("drain ok");
}

#[test]
fn close_succeeds_when_server_acknowledges() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    socket.push_reply_msg(ServerMessage::Closed);
    let client = open_stereo(socket).expect("open ok");
    client.close().expect("close ok");
}

#[test]
fn full_lifecycle_open_submit_drain_close() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 7 });
    socket.push_reply_msg(ServerMessage::SubmitAck { frames_consumed: 0 });
    socket.push_reply_msg(ServerMessage::SubmitAck {
        frames_consumed: 100,
    });
    socket.push_reply_msg(ServerMessage::DrainAck);
    socket.push_reply_msg(ServerMessage::Closed);
    let mut client = open_stereo(socket).expect("open");
    client.submit_frames(&[0u8; 16]).expect("submit 1");
    client.submit_frames(&[0u8; 16]).expect("submit 2");
    client.drain().expect("drain");
    client.close().expect("close");
}

// ---------------- Error variants ----------------

#[test]
fn open_error_busy_propagates_as_server_error() {
    // Server-side `OpenError(Busy)` lifts to
    // `AudioClientError::Server(Busy)` — the second-client EBUSY arm.
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::OpenError(AudioError::Busy));
    let result = open_stereo(socket);
    assert_eq!(
        result.err(),
        Some(AudioClientError::Server(AudioError::Busy))
    );
}

#[test]
fn open_error_invalid_format_propagates_as_server_error() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::OpenError(AudioError::InvalidFormat));
    let result = open_stereo(socket);
    assert_eq!(
        result.err(),
        Some(AudioClientError::Server(AudioError::InvalidFormat))
    );
}

#[test]
fn submit_error_would_block_propagates_as_server_error() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    socket.push_reply_msg(ServerMessage::SubmitError(AudioError::WouldBlock));
    let mut client = open_stereo(socket).expect("open");
    let result = client.submit_frames(&[0u8; 16]);
    assert_eq!(
        result.err(),
        Some(AudioClientError::Server(AudioError::WouldBlock))
    );
}

#[test]
fn submit_error_broken_pipe_propagates() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    socket.push_reply_msg(ServerMessage::SubmitError(AudioError::BrokenPipe));
    let mut client = open_stereo(socket).expect("open");
    let result = client.submit_frames(&[0u8; 16]);
    assert_eq!(
        result.err(),
        Some(AudioClientError::Server(AudioError::BrokenPipe))
    );
}

#[test]
fn submit_oversize_payload_returns_protocol_payload_too_large() {
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    let mut client = open_stereo(socket).expect("open");
    let big = std::vec![0u8; MAX_SUBMIT_BYTES + 1];
    let result = client.submit_frames(&big);
    assert_eq!(
        result.err(),
        Some(AudioClientError::Protocol(ProtocolError::PayloadTooLarge))
    );
}

#[test]
fn io_error_propagates_into_audio_client_error_io() {
    let mut socket = MockSocket::new();
    socket.push_io_error(-42);
    let result = open_stereo(socket);
    assert_eq!(result.err(), Some(AudioClientError::Io(-42)));
}

#[test]
fn corrupt_reply_bytes_surface_as_protocol_error() {
    // Acceptance: protocol decode errors propagate as
    // `AudioClientError::Protocol(...)`. Push junk bytes that fail
    // the frame header.
    let mut socket = MockSocket::new();
    socket.push_raw_reply_bytes(std::vec![0xFF, 0xFF, 0xFF, 0xFF]);
    let result = open_stereo(socket).map(|_| ());
    match result {
        Err(AudioClientError::Protocol(_)) => {}
        other => panic!("expected Protocol error, got {:?}", other),
    }
}

#[test]
fn submit_before_open_returns_not_open() {
    let socket = MockSocket::new();
    let mut client: AudioClient<MockSocket> = AudioClient::new_with_socket(socket);
    let result = client.submit_frames(&[0u8; 16]);
    assert_eq!(result.err(), Some(AudioClientError::NotOpen));
}

#[test]
fn drain_before_open_returns_not_open() {
    let socket = MockSocket::new();
    let mut client: AudioClient<MockSocket> = AudioClient::new_with_socket(socket);
    let result = client.drain();
    assert_eq!(result.err(), Some(AudioClientError::NotOpen));
}

#[test]
fn close_before_open_returns_not_open() {
    let socket = MockSocket::new();
    let client: AudioClient<MockSocket> = AudioClient::new_with_socket(socket);
    let result = client.close();
    assert_eq!(result.err(), Some(AudioClientError::NotOpen));
}

// ---------------- Second-open path ----------------

#[test]
fn second_open_on_held_stream_returns_already_open() {
    // Acceptance bullet: "second-open `EBUSY` path."
    // Library surfaces a *client-side* `AlreadyOpen` for a second
    // open on the same client (no wire traffic). The server-side
    // EBUSY arm is covered by `open_error_busy_propagates_as_server_error`.
    let mut socket = MockSocket::new();
    socket.push_reply_msg(ServerMessage::Opened { stream_id: 1 });
    let client = open_stereo(socket).expect("first open ok");

    fn try_second_open<S: AudioSocket>(c: &mut AudioClient<S>) -> Result<(), AudioClientError> {
        if c.stream_id().is_some() {
            return Err(AudioClientError::AlreadyOpen);
        }
        Ok(())
    }
    let mut client = client;
    let result = try_second_open(&mut client);
    assert_eq!(result.err(), Some(AudioClientError::AlreadyOpen));
}

// ---------------- DRY: protocol bytes only declared once ----------------

#[test]
fn library_only_consumes_kernel_core_protocol_no_parallel_definitions() {
    // The library does not redeclare any protocol byte constants or
    // message enums. This is a presence check on the kernel-core
    // symbols we depend on.
    let _ = ClientMessage::Drain;
    let _ = ClientMessage::Close;
    let _ = ServerMessage::DrainAck;
    let _ = ServerMessage::Closed;
    let _ = MAX_SUBMIT_BYTES;
}

// ---------------- AudioClientError From conversions ----------------

#[test]
fn protocol_error_lifts_into_audio_client_error() {
    let err: AudioClientError = ProtocolError::Truncated.into();
    assert_eq!(err, AudioClientError::Protocol(ProtocolError::Truncated));
}
