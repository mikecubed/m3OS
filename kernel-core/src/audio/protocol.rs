//! Audio wire-format codec — Phase 57 B.3.
//!
//! Mimics `kernel-core::display::protocol`: length-prefixed framing,
//! opcode-dispatched encoders, per-variant body layouts. The four
//! message families are:
//!
//! - [`ClientMessage`] — client → audio_server.
//! - [`ServerMessage`] — audio_server → client.
//! - [`AudioControlCommand`] — control client → audio_server's control
//!   endpoint (administration verb).
//! - [`AudioControlEvent`] — audio_server → subscribed control clients
//!   (subscribed-stream events; in Phase 57 only `Stats`).
//!
//! ## Framing
//!
//! Every frame has a 4-byte header:
//!
//! ```text
//! [body_len: u16 LE] [opcode: u16 LE] [body: body_len bytes]
//! ```
//!
//! `body_len` does not include the header. Frames larger than
//! [`MAX_FRAME_BODY_LEN`] are rejected with [`ProtocolError::BodyTooLarge`].
//!
//! ## Bulk payload
//!
//! [`ClientMessage::SubmitFrames`] declares a `len` field; the actual
//! PCM bytes are NOT carried in the codec — they ride the same socket
//! immediately after the encoded frame. The codec round-trip property
//! covers the control frame only. The Phase 57 audio ABI memo pins the
//! upper bound at [`MAX_SUBMIT_BYTES`]; encoding a `SubmitFrames` with
//! `len > MAX_SUBMIT_BYTES` returns [`ProtocolError::PayloadTooLarge`].
//!
//! ## Adversarial input
//!
//! Decoders never panic, infinite-loop, or allocate unboundedly. The
//! corrupted-framing property test feeds arbitrary `&[u8]` into every
//! decoder; the test runs with 1024 generated cases per build.

use crate::audio::{ChannelLayout, PcmFormat, SampleRate};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Frame header size: `body_len (u16) + opcode (u16)`.
pub const FRAME_HEADER_SIZE: usize = 4;

/// Maximum body size in any frame. Conservatively above the largest
/// fixed-shape body in the Phase 57 surface (`SubmitAck` is 8 bytes,
/// `Stats` is 20 bytes, etc.). This bounds the decoder's input claim.
pub const MAX_FRAME_BODY_LEN: u16 = 256;

/// Maximum bulk payload size on a single `SubmitFrames`. Pinned by the
/// Phase 57 audio ABI memo at 64 KiB. Drift in this constant breaks
/// the audio_server / audio_client ring-budget contract.
pub const MAX_SUBMIT_BYTES: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Opcodes
// ---------------------------------------------------------------------------

// Client → server (0x0000..=0x00FF)
const OP_CLIENT_OPEN: u16 = 0x0001;
const OP_CLIENT_SUBMIT_FRAMES: u16 = 0x0002;
const OP_CLIENT_DRAIN: u16 = 0x0003;
const OP_CLIENT_CLOSE: u16 = 0x0004;
const OP_CLIENT_CONTROL_COMMAND: u16 = 0x0005;

// Server → client (0x0100..=0x01FF)
const OP_SERVER_OPENED: u16 = 0x0101;
const OP_SERVER_OPEN_ERROR: u16 = 0x0102;
const OP_SERVER_SUBMIT_ACK: u16 = 0x0103;
const OP_SERVER_SUBMIT_ERROR: u16 = 0x0104;
const OP_SERVER_DRAIN_ACK: u16 = 0x0105;
const OP_SERVER_CLOSED: u16 = 0x0106;
const OP_SERVER_CONTROL_EVENT: u16 = 0x0107;

// Control commands (0x0200..=0x02FF)
const OP_CTL_GET_STATS: u16 = 0x0201;

// Control events (0x0300..=0x03FF)
const OP_CTL_EVT_STATS: u16 = 0x0301;

// ---------------------------------------------------------------------------
// AudioError — single source of truth, consumed by errno.rs (B.5)
// ---------------------------------------------------------------------------

/// Audio-error type used on every Phase 57 audio-error path.
///
/// The `audio_error_to_neg_errno` helper in [`super::errno`] (B.5) is
/// the **only** workspace site that translates these to negative
/// errno values. Wire encoding is by explicit `u8` discriminant on the
/// `OpenError` / `SubmitError` variants of [`ServerMessage`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum AudioError {
    /// Single-client policy: `audio_server` already has a stream open.
    Busy,
    /// Transient — retry. The DMA ring is currently full; the client
    /// should wait for `BufferReleased` notifications before retrying.
    WouldBlock,
    /// `audio_server` has not completed device claim. Maps to `-ENODEV`.
    NoDevice,
    /// The client's stream was disconnected (driver restart, supervisor
    /// kill, etc.). Maps to `-EPIPE`.
    BrokenPipe,
    /// The requested PCM format / layout / rate is not supported.
    InvalidFormat,
    /// A protocol argument was malformed (negative count, oversize
    /// payload, unknown stream, etc.).
    InvalidArgument,
    /// Catch-all hard error (DMA fault, register sequence violation).
    /// Maps to `-EIO`.
    Internal,
}

impl AudioError {
    /// Wire encoding (single byte) for inclusion in `OpenError` /
    /// `SubmitError` server messages. Adding a variant requires
    /// allocating a new byte and updating both encoders and decoders.
    const fn to_byte(self) -> u8 {
        match self {
            AudioError::Busy => 0,
            AudioError::WouldBlock => 1,
            AudioError::NoDevice => 2,
            AudioError::BrokenPipe => 3,
            AudioError::InvalidFormat => 4,
            AudioError::InvalidArgument => 5,
            AudioError::Internal => 6,
        }
    }

    fn from_byte(byte: u8) -> Result<Self, ProtocolError> {
        match byte {
            0 => Ok(AudioError::Busy),
            1 => Ok(AudioError::WouldBlock),
            2 => Ok(AudioError::NoDevice),
            3 => Ok(AudioError::BrokenPipe),
            4 => Ok(AudioError::InvalidFormat),
            5 => Ok(AudioError::InvalidArgument),
            6 => Ok(AudioError::Internal),
            _ => Err(ProtocolError::InvalidEnum),
        }
    }
}

// ---------------------------------------------------------------------------
// Wire enum tags for the value types declared in `format`
// ---------------------------------------------------------------------------

const TAG_FMT_S16LE: u8 = 0;
const TAG_LAYOUT_MONO: u8 = 0;
const TAG_LAYOUT_STEREO: u8 = 1;
const TAG_RATE_48000: u8 = 0;

const fn pcm_format_to_byte(format: PcmFormat) -> u8 {
    match format {
        PcmFormat::S16Le => TAG_FMT_S16LE,
    }
}

fn pcm_format_from_byte(byte: u8) -> Result<PcmFormat, ProtocolError> {
    match byte {
        TAG_FMT_S16LE => Ok(PcmFormat::S16Le),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

const fn layout_to_byte(layout: ChannelLayout) -> u8 {
    match layout {
        ChannelLayout::Mono => TAG_LAYOUT_MONO,
        ChannelLayout::Stereo => TAG_LAYOUT_STEREO,
    }
}

fn layout_from_byte(byte: u8) -> Result<ChannelLayout, ProtocolError> {
    match byte {
        TAG_LAYOUT_MONO => Ok(ChannelLayout::Mono),
        TAG_LAYOUT_STEREO => Ok(ChannelLayout::Stereo),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

const fn rate_to_byte(rate: SampleRate) -> u8 {
    match rate {
        SampleRate::Hz48000 => TAG_RATE_48000,
    }
}

fn rate_from_byte(byte: u8) -> Result<SampleRate, ProtocolError> {
    match byte {
        TAG_RATE_48000 => Ok(SampleRate::Hz48000),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors emitted by the audio protocol codec. Variants are *data*;
/// callers pattern-match. No stringly-typed errors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ProtocolError {
    /// Input buffer smaller than required, or output buffer too small.
    Truncated,
    /// Declared body length exceeds [`MAX_FRAME_BODY_LEN`].
    BodyTooLarge,
    /// SubmitFrames `len` exceeds [`MAX_SUBMIT_BYTES`].
    PayloadTooLarge,
    /// Unknown opcode on the wire.
    UnknownOpcode(u16),
    /// Body length does not match the size expected for the opcode.
    BodyLengthMismatch,
    /// Enum discriminant not recognized (PcmFormat tag, AudioError tag,
    /// etc.).
    InvalidEnum,
}

// ---------------------------------------------------------------------------
// Framing helpers
// ---------------------------------------------------------------------------

fn write_frame_header(buf: &mut [u8], body_len: u16, opcode: u16) -> Result<(), ProtocolError> {
    if buf.len() < FRAME_HEADER_SIZE {
        return Err(ProtocolError::Truncated);
    }
    buf[0..2].copy_from_slice(&body_len.to_le_bytes());
    buf[2..4].copy_from_slice(&opcode.to_le_bytes());
    Ok(())
}

fn parse_frame_header(buf: &[u8]) -> Result<(u16, u16, &[u8], usize), ProtocolError> {
    if buf.len() < FRAME_HEADER_SIZE {
        return Err(ProtocolError::Truncated);
    }
    let body_len = u16::from_le_bytes([buf[0], buf[1]]);
    if body_len > MAX_FRAME_BODY_LEN {
        return Err(ProtocolError::BodyTooLarge);
    }
    let opcode = u16::from_le_bytes([buf[2], buf[3]]);
    let total = FRAME_HEADER_SIZE + body_len as usize;
    if buf.len() < total {
        return Err(ProtocolError::Truncated);
    }
    Ok((body_len, opcode, &buf[FRAME_HEADER_SIZE..total], total))
}

fn expect_body_len(actual: u16, expected: u16) -> Result<(), ProtocolError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ProtocolError::BodyLengthMismatch)
    }
}

fn encode_fixed<F>(
    buf: &mut [u8],
    opcode: u16,
    body_len: usize,
    write_body: F,
) -> Result<usize, ProtocolError>
where
    F: FnOnce(&mut [u8]),
{
    if body_len > MAX_FRAME_BODY_LEN as usize {
        return Err(ProtocolError::BodyTooLarge);
    }
    let total = FRAME_HEADER_SIZE + body_len;
    if buf.len() < total {
        return Err(ProtocolError::Truncated);
    }
    write_frame_header(buf, body_len as u16, opcode)?;
    if body_len > 0 {
        write_body(&mut buf[FRAME_HEADER_SIZE..total]);
    }
    Ok(total)
}

fn read_u32_at(body: &[u8], offset: usize) -> Result<u32, ProtocolError> {
    let s = body
        .get(offset..offset + 4)
        .ok_or(ProtocolError::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64_at(body: &[u8], offset: usize) -> Result<u64, ProtocolError> {
    let s = body
        .get(offset..offset + 8)
        .ok_or(ProtocolError::Truncated)?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

// ---------------------------------------------------------------------------
// Message enums
// ---------------------------------------------------------------------------

/// Client → audio_server message family.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ClientMessage {
    /// Open a PCM-out stream with the given format / layout / rate.
    /// Phase 57's single-client policy means a second `Open` while a
    /// stream is active is rejected with `ServerMessage::OpenError(Busy)`.
    Open {
        format: PcmFormat,
        layout: ChannelLayout,
        rate: SampleRate,
    },
    /// Inform the server that the next `len` bytes on the socket are
    /// PCM frames for the open stream. `len > MAX_SUBMIT_BYTES` is
    /// rejected at encode time as [`ProtocolError::PayloadTooLarge`].
    SubmitFrames { len: u32 },
    /// Block until every submitted frame has been consumed by the device.
    Drain,
    /// Close the open stream and release the device for the next opener.
    Close,
    /// Forward a control verb on the same connection (small admin
    /// surface; in Phase 57 only `GetStats`).
    ControlCommand(AudioControlCommand),
}

/// audio_server → client message family.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ServerMessage {
    /// Stream-open succeeded; the client's stream id is `stream_id`.
    Opened { stream_id: u32 },
    /// Stream-open failed.
    OpenError(AudioError),
    /// Submit accepted; `frames_consumed` is the device's running
    /// total of frames drained out of the BDL.
    SubmitAck { frames_consumed: u64 },
    /// Submit failed (transient or permanent).
    SubmitError(AudioError),
    /// Drain finished — every submitted frame has been consumed.
    DrainAck,
    /// The stream is closed (graceful, or because the driver crashed).
    Closed,
    /// Control-event subscribed-stream payload (Phase 57: `Stats` only).
    ControlEvent(AudioControlEvent),
}

/// Control-socket request verb. Phase 57 surface is intentionally
/// single-verb; growing it requires a new opcode and matching
/// AudioControlEvent reply (Track G manifest).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum AudioControlCommand {
    /// Return the latest stream-stats sample.
    GetStats,
}

/// Control-socket reply / subscribed-event payload. Phase 57 surface
/// carries `Stats` only; the audio_server emits these in reply to
/// [`AudioControlCommand::GetStats`] and on each round of the io loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum AudioControlEvent {
    Stats {
        underrun_count: u32,
        frames_submitted: u64,
        frames_consumed: u64,
    },
}

// ---------------------------------------------------------------------------
// ClientMessage codec
// ---------------------------------------------------------------------------

impl ClientMessage {
    /// Encode into a caller-supplied buffer. Returns bytes written.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::Open {
                format,
                layout,
                rate,
            } => encode_fixed(buf, OP_CLIENT_OPEN, 3, |body| {
                body[0] = pcm_format_to_byte(*format);
                body[1] = layout_to_byte(*layout);
                body[2] = rate_to_byte(*rate);
            }),
            Self::SubmitFrames { len } => {
                if (*len as usize) > MAX_SUBMIT_BYTES {
                    return Err(ProtocolError::PayloadTooLarge);
                }
                encode_fixed(buf, OP_CLIENT_SUBMIT_FRAMES, 4, |body| {
                    body[0..4].copy_from_slice(&len.to_le_bytes());
                })
            }
            Self::Drain => encode_fixed(buf, OP_CLIENT_DRAIN, 0, |_| {}),
            Self::Close => encode_fixed(buf, OP_CLIENT_CLOSE, 0, |_| {}),
            Self::ControlCommand(cmd) => {
                // Control-command body is the inner command's full frame.
                let mut inner_buf = [0u8; FRAME_HEADER_SIZE + MAX_FRAME_BODY_LEN as usize];
                let inner_len = cmd.encode(&mut inner_buf)?;
                encode_fixed(buf, OP_CLIENT_CONTROL_COMMAND, inner_len, |body| {
                    body.copy_from_slice(&inner_buf[..inner_len]);
                })
            }
        }
    }

    /// Decode from a caller-supplied buffer. Returns the parsed
    /// message and bytes consumed.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, body, total) = parse_frame_header(buf)?;
        match opcode {
            OP_CLIENT_OPEN => {
                expect_body_len(body_len, 3)?;
                let format = pcm_format_from_byte(body[0])?;
                let layout = layout_from_byte(body[1])?;
                let rate = rate_from_byte(body[2])?;
                Ok((
                    Self::Open {
                        format,
                        layout,
                        rate,
                    },
                    total,
                ))
            }
            OP_CLIENT_SUBMIT_FRAMES => {
                expect_body_len(body_len, 4)?;
                let len = read_u32_at(body, 0)?;
                if (len as usize) > MAX_SUBMIT_BYTES {
                    return Err(ProtocolError::PayloadTooLarge);
                }
                Ok((Self::SubmitFrames { len }, total))
            }
            OP_CLIENT_DRAIN => {
                expect_body_len(body_len, 0)?;
                Ok((Self::Drain, total))
            }
            OP_CLIENT_CLOSE => {
                expect_body_len(body_len, 0)?;
                Ok((Self::Close, total))
            }
            OP_CLIENT_CONTROL_COMMAND => {
                let (cmd, inner_consumed) = AudioControlCommand::decode(body)?;
                if inner_consumed != body.len() {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                Ok((Self::ControlCommand(cmd), total))
            }
            _ => Err(ProtocolError::UnknownOpcode(opcode)),
        }
    }
}

// ---------------------------------------------------------------------------
// ServerMessage codec
// ---------------------------------------------------------------------------

impl ServerMessage {
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::Opened { stream_id } => encode_fixed(buf, OP_SERVER_OPENED, 4, |body| {
                body[0..4].copy_from_slice(&stream_id.to_le_bytes());
            }),
            Self::OpenError(err) => encode_fixed(buf, OP_SERVER_OPEN_ERROR, 1, |body| {
                body[0] = err.to_byte();
            }),
            Self::SubmitAck { frames_consumed } => {
                encode_fixed(buf, OP_SERVER_SUBMIT_ACK, 8, |body| {
                    body[0..8].copy_from_slice(&frames_consumed.to_le_bytes());
                })
            }
            Self::SubmitError(err) => encode_fixed(buf, OP_SERVER_SUBMIT_ERROR, 1, |body| {
                body[0] = err.to_byte();
            }),
            Self::DrainAck => encode_fixed(buf, OP_SERVER_DRAIN_ACK, 0, |_| {}),
            Self::Closed => encode_fixed(buf, OP_SERVER_CLOSED, 0, |_| {}),
            Self::ControlEvent(evt) => {
                let mut inner_buf = [0u8; FRAME_HEADER_SIZE + MAX_FRAME_BODY_LEN as usize];
                let inner_len = evt.encode(&mut inner_buf)?;
                encode_fixed(buf, OP_SERVER_CONTROL_EVENT, inner_len, |body| {
                    body.copy_from_slice(&inner_buf[..inner_len]);
                })
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, body, total) = parse_frame_header(buf)?;
        match opcode {
            OP_SERVER_OPENED => {
                expect_body_len(body_len, 4)?;
                Ok((
                    Self::Opened {
                        stream_id: read_u32_at(body, 0)?,
                    },
                    total,
                ))
            }
            OP_SERVER_OPEN_ERROR => {
                expect_body_len(body_len, 1)?;
                Ok((Self::OpenError(AudioError::from_byte(body[0])?), total))
            }
            OP_SERVER_SUBMIT_ACK => {
                expect_body_len(body_len, 8)?;
                Ok((
                    Self::SubmitAck {
                        frames_consumed: read_u64_at(body, 0)?,
                    },
                    total,
                ))
            }
            OP_SERVER_SUBMIT_ERROR => {
                expect_body_len(body_len, 1)?;
                Ok((Self::SubmitError(AudioError::from_byte(body[0])?), total))
            }
            OP_SERVER_DRAIN_ACK => {
                expect_body_len(body_len, 0)?;
                Ok((Self::DrainAck, total))
            }
            OP_SERVER_CLOSED => {
                expect_body_len(body_len, 0)?;
                Ok((Self::Closed, total))
            }
            OP_SERVER_CONTROL_EVENT => {
                let (evt, inner_consumed) = AudioControlEvent::decode(body)?;
                if inner_consumed != body.len() {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                Ok((Self::ControlEvent(evt), total))
            }
            _ => Err(ProtocolError::UnknownOpcode(opcode)),
        }
    }
}

// ---------------------------------------------------------------------------
// AudioControlCommand codec
// ---------------------------------------------------------------------------

impl AudioControlCommand {
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::GetStats => encode_fixed(buf, OP_CTL_GET_STATS, 0, |_| {}),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, _body, total) = parse_frame_header(buf)?;
        match opcode {
            OP_CTL_GET_STATS => {
                expect_body_len(body_len, 0)?;
                Ok((Self::GetStats, total))
            }
            _ => Err(ProtocolError::UnknownOpcode(opcode)),
        }
    }
}

// ---------------------------------------------------------------------------
// AudioControlEvent codec
// ---------------------------------------------------------------------------

impl AudioControlEvent {
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::Stats {
                underrun_count,
                frames_submitted,
                frames_consumed,
            } => encode_fixed(buf, OP_CTL_EVT_STATS, 4 + 8 + 8, |body| {
                body[0..4].copy_from_slice(&underrun_count.to_le_bytes());
                body[4..12].copy_from_slice(&frames_submitted.to_le_bytes());
                body[12..20].copy_from_slice(&frames_consumed.to_le_bytes());
            }),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, body, total) = parse_frame_header(buf)?;
        match opcode {
            OP_CTL_EVT_STATS => {
                expect_body_len(body_len, 20)?;
                Ok((
                    Self::Stats {
                        underrun_count: read_u32_at(body, 0)?,
                        frames_submitted: read_u64_at(body, 4)?,
                        frames_consumed: read_u64_at(body, 12)?,
                    },
                    total,
                ))
            }
            _ => Err(ProtocolError::UnknownOpcode(opcode)),
        }
    }
}

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
        let (decoded, consumed) = AudioControlCommand::decode(&buf[..written]).expect("decode");
        assert_eq!(decoded, msg);
        assert_eq!(consumed, written);
    }

    fn roundtrip_ctl_evt(msg: AudioControlEvent) {
        let mut buf = [0u8; 256];
        let written = msg.encode(&mut buf).expect("encode");
        let (decoded, consumed) = AudioControlEvent::decode(&buf[..written]).expect("decode");
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
        roundtrip_client(ClientMessage::ControlCommand(AudioControlCommand::GetStats));
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
            (0u32..=MAX_SUBMIT_BYTES as u32).prop_map(|len| ClientMessage::SubmitFrames { len }),
            Just(ClientMessage::Drain),
            Just(ClientMessage::Close),
            any_ctl_command().prop_map(ClientMessage::ControlCommand),
        ]
    }

    fn any_server_message() -> impl Strategy<Value = ServerMessage> {
        prop_oneof![
            any::<u32>().prop_map(|stream_id| ServerMessage::Opened { stream_id }),
            any_audio_error().prop_map(ServerMessage::OpenError),
            any::<u64>().prop_map(|frames_consumed| ServerMessage::SubmitAck { frames_consumed }),
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
