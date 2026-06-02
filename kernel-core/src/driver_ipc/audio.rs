//! Audio-driver IPC protocol schema — Phase 80 Track A.1.
//!
//! Single source of truth for the control protocol spoken between
//! `audio_server` (the policy/mixer server, the *client* of this protocol)
//! and an out-of-process audio hardware driver (`userspace/drivers/ac97`,
//! `userspace/drivers/hda`, the *servers*). It is the audio sibling of
//! [`super::block`] (NVMe) and [`super::net`] (e1000): declaring it in
//! `kernel-core` keeps it host-testable and guarantees the client and every
//! driver compile against the same message layout, so divergence is a
//! compile error rather than a runtime corruption bug.
//!
//! # Bulk PCM never travels inline
//!
//! Per the AGENTS.md IPC rule ("bulk data: page capability grants, never IPC
//! payloads"), the sample bytes of a [`AudioRequest::SubmitFrames`] do **not**
//! appear in any message field. Instead the bytes live in a **persistent
//! page-capability-backed shared region** established once at stream open via
//! `sys_shm_*`; each `SubmitFrames` carries only that region's id (in
//! `grant_handle`) plus a byte offset/length window into it. The driver maps the
//! region — reused every period, **not** a single-use move — copies the window
//! into its own `sys_device_dma_alloc` IOMMU-domain buffer, and the region is
//! refcount-released on `CloseStream`/exit. See `driver_runtime::audio_pcm`,
//! which explains why a persistent ring is used rather than a per-submission
//! `sys_page_grant_*` move. The absence of any `&[u8]`/`Vec<u8>` sample field in
//! [`AudioRequest`] is enforced by the type — grep-verifiable.
//!
//! # Wire format
//!
//! Tag-dispatched, packed little-endian. Byte 0 of every encoded message is a
//! variant tag; the remaining bytes are the variant's fixed-width fields. The
//! `encode_request`/`decode_request` and `encode_response`/`decode_response`
//! pairs are pure, `no_std`, allocation-free, and round-trip byte-for-byte
//! (see the test module).

#![allow(clippy::needless_range_loop)]

use crate::audio::{ChannelLayout, PcmFormat, SampleRate};

// ------------------------------------------------------------------------
// Message-label constants (IPC frame `kind`)
// ------------------------------------------------------------------------

/// IPC message label for an [`AudioRequest`] envelope (client → driver).
///
/// Reserved from the `0x5600` range, kept clear of the Phase 55b block
/// (`0x5500`) and net driver labels so audio frames never collide.
pub const AUDIO_REQUEST: u16 = 0x5601;

/// IPC message label for an [`AudioResponse`] envelope (driver → client).
pub const AUDIO_RESPONSE: u16 = 0x5602;

/// IPC notification label the driver signals when a submitted buffer
/// completes (BDL/BCIS interrupt). Word-sized notification, no payload.
pub const AUDIO_COMPLETION: u16 = 0x5603;

// ------------------------------------------------------------------------
// Buffer sizing
// ------------------------------------------------------------------------

/// Maximum serialized size of any [`AudioRequest`] (the `SubmitFrames`
/// variant: 1 tag + four `u32` = 17 bytes). Callers stack-allocate a buffer
/// of this size.
pub const AUDIO_REQUEST_MAX_SIZE: usize = 17;

/// Maximum serialized size of any [`AudioResponse`] (the `Ack` variant:
/// 1 tag + one `u64` = 9 bytes).
pub const AUDIO_RESPONSE_MAX_SIZE: usize = 9;

// ------------------------------------------------------------------------
// Value-type wire tags (mirror of `kernel_core::audio`'s private tags)
// ------------------------------------------------------------------------

const TAG_FMT_S16LE: u8 = 0;
const TAG_RATE_48000: u8 = 0;
const TAG_LAYOUT_MONO: u8 = 0;
const TAG_LAYOUT_STEREO: u8 = 1;

const fn fmt_to_byte(f: PcmFormat) -> u8 {
    match f {
        PcmFormat::S16Le => TAG_FMT_S16LE,
    }
}

const fn fmt_from_byte(b: u8) -> Option<PcmFormat> {
    match b {
        TAG_FMT_S16LE => Some(PcmFormat::S16Le),
        _ => None,
    }
}

const fn rate_to_byte(r: SampleRate) -> u8 {
    match r {
        SampleRate::Hz48000 => TAG_RATE_48000,
    }
}

const fn rate_from_byte(b: u8) -> Option<SampleRate> {
    match b {
        TAG_RATE_48000 => Some(SampleRate::Hz48000),
        _ => None,
    }
}

const fn layout_to_byte(l: ChannelLayout) -> u8 {
    match l {
        ChannelLayout::Mono => TAG_LAYOUT_MONO,
        ChannelLayout::Stereo => TAG_LAYOUT_STEREO,
    }
}

const fn layout_from_byte(b: u8) -> Option<ChannelLayout> {
    match b {
        TAG_LAYOUT_MONO => Some(ChannelLayout::Mono),
        TAG_LAYOUT_STEREO => Some(ChannelLayout::Stereo),
        _ => None,
    }
}

// ------------------------------------------------------------------------
// AudioDriverError
// ------------------------------------------------------------------------

/// Error kinds emitted by the audio-driver IPC path, carried inside
/// [`AudioResponse::Err`].
///
/// Variants are *data*, never strings — both `audio_server`'s
/// `AudioProxyBackend` and the driver pattern-match on them without
/// allocation. `WouldBlock` is **not** here: ring-full backpressure is the
/// distinct [`AudioResponse::WouldBlock`] response so the existing
/// all-or-nothing client contract is preserved across the seam.
///
/// `#[non_exhaustive]` lets later phases add variants without forcing every
/// downstream `match` to be exhaustive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum AudioDriverError {
    /// A stream is already open and the driver enforces single-stream policy.
    Busy,
    /// The driver has not completed device claim / controller init.
    NoDevice,
    /// The requested PCM format / layout / rate is unsupported by the codec.
    InvalidFormat,
    /// A protocol argument was malformed (unknown stream id, bad grant
    /// offset/length, zero-length submit, etc.).
    InvalidArgument,
    /// The target device was removed or is no longer claimed.
    DeviceAbsent,
    /// The driver process crashed and is being restarted; the caller should
    /// re-discover the service and re-open its streams.
    DriverRestarting,
    /// Catch-all hard error (DMA fault, register-sequence violation).
    Internal,
}

impl AudioDriverError {
    /// Stable single-byte encoding used on the wire.
    pub const fn to_byte(self) -> u8 {
        match self {
            AudioDriverError::Busy => 0,
            AudioDriverError::NoDevice => 1,
            AudioDriverError::InvalidFormat => 2,
            AudioDriverError::InvalidArgument => 3,
            AudioDriverError::DeviceAbsent => 4,
            AudioDriverError::DriverRestarting => 5,
            AudioDriverError::Internal => 6,
        }
    }

    /// Inverse of [`Self::to_byte`]; `None` for unknown discriminants so a
    /// malformed payload produces a decode error rather than a silent
    /// substitution.
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(AudioDriverError::Busy),
            1 => Some(AudioDriverError::NoDevice),
            2 => Some(AudioDriverError::InvalidFormat),
            3 => Some(AudioDriverError::InvalidArgument),
            4 => Some(AudioDriverError::DeviceAbsent),
            5 => Some(AudioDriverError::DriverRestarting),
            6 => Some(AudioDriverError::Internal),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------------
// AudioCaps
// ------------------------------------------------------------------------

/// Fixed capability descriptor returned by [`AudioRequest::QueryCaps`].
///
/// For 1.0 the format/rate/layout are pinned (rate negotiation is deferred —
/// see the phase Documentation Notes). The driver validates this against the
/// codec's reported `SUPPORTED_PCM_RATES`/`SUPPORTED_STREAM_FORMATS` and
/// fails fast if the codec cannot produce it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AudioCaps {
    /// Sample encoding (S16Le for 1.0).
    pub format: PcmFormat,
    /// Sample rate (48 kHz for 1.0).
    pub rate: SampleRate,
    /// Channel layout (Stereo for 1.0).
    pub layout: ChannelLayout,
    /// Largest PCM submission (bytes) the driver accepts in one
    /// [`AudioRequest::SubmitFrames`].
    pub max_submit_bytes: u32,
}

/// The fixed 1.0 capability descriptor: 48 kHz / 2 ch / 16-bit.
pub const fn caps_v1() -> AudioCaps {
    AudioCaps {
        format: PcmFormat::S16Le,
        rate: SampleRate::Hz48000,
        layout: ChannelLayout::Stereo,
        max_submit_bytes: crate::audio::MAX_SUBMIT_BYTES as u32,
    }
}

// ------------------------------------------------------------------------
// AudioRequest / AudioResponse
// ------------------------------------------------------------------------

/// Request sent from `audio_server` (client) to an audio driver (server).
///
/// Note the deliberate absence of any sample-data field: bulk PCM crosses the
/// boundary only via the `grant_handle` of [`AudioRequest::SubmitFrames`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioRequest {
    /// Ask the driver for its fixed capability descriptor.
    QueryCaps,
    /// Open an output stream of the given PCM shape. Reply is
    /// [`AudioResponse::StreamOpened`].
    OpenStream {
        format: PcmFormat,
        rate: SampleRate,
        layout: ChannelLayout,
    },
    /// Point the driver at PCM to play. `grant_handle` is the id of the
    /// persistent `sys_shm` region shared at stream open (mapped once and reused
    /// every period — **not** a single-use grant); `offset`/`len` bound the bytes
    /// within that region. Reply is [`AudioResponse::Ack`] /
    /// [`AudioResponse::WouldBlock`] / [`AudioResponse::Err`].
    SubmitFrames {
        stream_id: u32,
        grant_handle: u32,
        offset: u32,
        len: u32,
    },
    /// Block until every submitted frame has been consumed by the device.
    Drain { stream_id: u32 },
    /// Halt the stream and release its slot. Reply is [`AudioResponse::Ok`].
    CloseStream { stream_id: u32 },
}

/// Reply sent from an audio driver (server) back to `audio_server` (client).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioResponse {
    /// Reply to [`AudioRequest::QueryCaps`].
    Caps(AudioCaps),
    /// Reply to [`AudioRequest::OpenStream`]: the driver-allocated stream id.
    StreamOpened(u32),
    /// Reply to [`AudioRequest::SubmitFrames`]: the submission was accepted
    /// and `frames_consumed` reports the running device-side consumed counter.
    Ack { frames_consumed: u64 },
    /// Reply to [`AudioRequest::SubmitFrames`]: the hardware ring is full;
    /// the client should retry. Maps back to `AudioError::WouldBlock`,
    /// preserving the all-or-nothing client submit contract.
    WouldBlock,
    /// Generic success acknowledgement (for `Drain` / `CloseStream`).
    Ok,
    /// Any request failed with the carried error.
    Err(AudioDriverError),
}

// ------------------------------------------------------------------------
// Request tags
// ------------------------------------------------------------------------

const REQ_QUERY_CAPS: u8 = 1;
const REQ_OPEN_STREAM: u8 = 2;
const REQ_SUBMIT_FRAMES: u8 = 3;
const REQ_DRAIN: u8 = 4;
const REQ_CLOSE_STREAM: u8 = 5;

const RSP_CAPS: u8 = 1;
const RSP_STREAM_OPENED: u8 = 2;
const RSP_ACK: u8 = 3;
const RSP_WOULD_BLOCK: u8 = 4;
const RSP_OK: u8 = 5;
const RSP_ERR: u8 = 6;

// ------------------------------------------------------------------------
// Decode errors
// ------------------------------------------------------------------------

/// Reasons a decode call can fail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DecodeError {
    /// Input slice was shorter than the variant's fixed length.
    Truncated,
    /// The leading tag byte did not match any known variant.
    UnknownTag,
    /// An enum field (format/rate/layout/error) held an undefined byte.
    InvalidEnum,
}

// ------------------------------------------------------------------------
// Encode / decode — AudioRequest
// ------------------------------------------------------------------------

/// Encode `req` into `buf`. Returns the number of bytes written, or `None`
/// if `buf` is shorter than the variant requires (caller should size at
/// [`AUDIO_REQUEST_MAX_SIZE`]).
pub fn encode_request(req: &AudioRequest, buf: &mut [u8]) -> Option<usize> {
    match *req {
        AudioRequest::QueryCaps => {
            if buf.is_empty() {
                return None;
            }
            buf[0] = REQ_QUERY_CAPS;
            Some(1)
        }
        AudioRequest::OpenStream {
            format,
            rate,
            layout,
        } => {
            if buf.len() < 4 {
                return None;
            }
            buf[0] = REQ_OPEN_STREAM;
            buf[1] = fmt_to_byte(format);
            buf[2] = rate_to_byte(rate);
            buf[3] = layout_to_byte(layout);
            Some(4)
        }
        AudioRequest::SubmitFrames {
            stream_id,
            grant_handle,
            offset,
            len,
        } => {
            if buf.len() < 17 {
                return None;
            }
            buf[0] = REQ_SUBMIT_FRAMES;
            buf[1..5].copy_from_slice(&stream_id.to_le_bytes());
            buf[5..9].copy_from_slice(&grant_handle.to_le_bytes());
            buf[9..13].copy_from_slice(&offset.to_le_bytes());
            buf[13..17].copy_from_slice(&len.to_le_bytes());
            Some(17)
        }
        AudioRequest::Drain { stream_id } => {
            if buf.len() < 5 {
                return None;
            }
            buf[0] = REQ_DRAIN;
            buf[1..5].copy_from_slice(&stream_id.to_le_bytes());
            Some(5)
        }
        AudioRequest::CloseStream { stream_id } => {
            if buf.len() < 5 {
                return None;
            }
            buf[0] = REQ_CLOSE_STREAM;
            buf[1..5].copy_from_slice(&stream_id.to_le_bytes());
            Some(5)
        }
    }
}

/// Decode an [`AudioRequest`] from `buf`.
pub fn decode_request(buf: &[u8]) -> Result<AudioRequest, DecodeError> {
    if buf.is_empty() {
        return Err(DecodeError::Truncated);
    }
    match buf[0] {
        REQ_QUERY_CAPS => Ok(AudioRequest::QueryCaps),
        REQ_OPEN_STREAM => {
            if buf.len() < 4 {
                return Err(DecodeError::Truncated);
            }
            let format = fmt_from_byte(buf[1]).ok_or(DecodeError::InvalidEnum)?;
            let rate = rate_from_byte(buf[2]).ok_or(DecodeError::InvalidEnum)?;
            let layout = layout_from_byte(buf[3]).ok_or(DecodeError::InvalidEnum)?;
            Ok(AudioRequest::OpenStream {
                format,
                rate,
                layout,
            })
        }
        REQ_SUBMIT_FRAMES => {
            if buf.len() < 17 {
                return Err(DecodeError::Truncated);
            }
            Ok(AudioRequest::SubmitFrames {
                stream_id: u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]),
                grant_handle: u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]),
                offset: u32::from_le_bytes([buf[9], buf[10], buf[11], buf[12]]),
                len: u32::from_le_bytes([buf[13], buf[14], buf[15], buf[16]]),
            })
        }
        REQ_DRAIN => {
            if buf.len() < 5 {
                return Err(DecodeError::Truncated);
            }
            Ok(AudioRequest::Drain {
                stream_id: u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]),
            })
        }
        REQ_CLOSE_STREAM => {
            if buf.len() < 5 {
                return Err(DecodeError::Truncated);
            }
            Ok(AudioRequest::CloseStream {
                stream_id: u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]),
            })
        }
        _ => Err(DecodeError::UnknownTag),
    }
}

// ------------------------------------------------------------------------
// Encode / decode — AudioResponse
// ------------------------------------------------------------------------

/// Encode `rsp` into `buf`. Returns bytes written, or `None` if `buf` is too
/// short (size at [`AUDIO_RESPONSE_MAX_SIZE`]).
pub fn encode_response(rsp: &AudioResponse, buf: &mut [u8]) -> Option<usize> {
    match *rsp {
        AudioResponse::Caps(caps) => {
            if buf.len() < 8 {
                return None;
            }
            buf[0] = RSP_CAPS;
            buf[1] = fmt_to_byte(caps.format);
            buf[2] = rate_to_byte(caps.rate);
            buf[3] = layout_to_byte(caps.layout);
            buf[4..8].copy_from_slice(&caps.max_submit_bytes.to_le_bytes());
            Some(8)
        }
        AudioResponse::StreamOpened(id) => {
            if buf.len() < 5 {
                return None;
            }
            buf[0] = RSP_STREAM_OPENED;
            buf[1..5].copy_from_slice(&id.to_le_bytes());
            Some(5)
        }
        AudioResponse::Ack { frames_consumed } => {
            if buf.len() < 9 {
                return None;
            }
            buf[0] = RSP_ACK;
            buf[1..9].copy_from_slice(&frames_consumed.to_le_bytes());
            Some(9)
        }
        AudioResponse::WouldBlock => {
            if buf.is_empty() {
                return None;
            }
            buf[0] = RSP_WOULD_BLOCK;
            Some(1)
        }
        AudioResponse::Ok => {
            if buf.is_empty() {
                return None;
            }
            buf[0] = RSP_OK;
            Some(1)
        }
        AudioResponse::Err(e) => {
            if buf.len() < 2 {
                return None;
            }
            buf[0] = RSP_ERR;
            buf[1] = e.to_byte();
            Some(2)
        }
    }
}

/// Decode an [`AudioResponse`] from `buf`.
pub fn decode_response(buf: &[u8]) -> Result<AudioResponse, DecodeError> {
    if buf.is_empty() {
        return Err(DecodeError::Truncated);
    }
    match buf[0] {
        RSP_CAPS => {
            if buf.len() < 8 {
                return Err(DecodeError::Truncated);
            }
            let format = fmt_from_byte(buf[1]).ok_or(DecodeError::InvalidEnum)?;
            let rate = rate_from_byte(buf[2]).ok_or(DecodeError::InvalidEnum)?;
            let layout = layout_from_byte(buf[3]).ok_or(DecodeError::InvalidEnum)?;
            Ok(AudioResponse::Caps(AudioCaps {
                format,
                rate,
                layout,
                max_submit_bytes: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            }))
        }
        RSP_STREAM_OPENED => {
            if buf.len() < 5 {
                return Err(DecodeError::Truncated);
            }
            Ok(AudioResponse::StreamOpened(u32::from_le_bytes([
                buf[1], buf[2], buf[3], buf[4],
            ])))
        }
        RSP_ACK => {
            if buf.len() < 9 {
                return Err(DecodeError::Truncated);
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&buf[1..9]);
            Ok(AudioResponse::Ack {
                frames_consumed: u64::from_le_bytes(b),
            })
        }
        RSP_WOULD_BLOCK => Ok(AudioResponse::WouldBlock),
        RSP_OK => Ok(AudioResponse::Ok),
        RSP_ERR => {
            if buf.len() < 2 {
                return Err(DecodeError::Truncated);
            }
            let e = AudioDriverError::from_byte(buf[1]).ok_or(DecodeError::InvalidEnum)?;
            Ok(AudioResponse::Err(e))
        }
        _ => Err(DecodeError::UnknownTag),
    }
}

// ------------------------------------------------------------------------
// Grant-offset/length validation (shared with driver_runtime::audio_pcm)
// ------------------------------------------------------------------------

/// Validate that a `SubmitFrames` `[offset, offset+len)` window lands entirely
/// inside a granted region of `granted_len` bytes.
///
/// Rejects zero-length submissions and any window that overflows or spills
/// past the granted region — the driver-side guard against a malformed or
/// adversarial `audio_server` directing a copy out of bounds.
pub const fn submission_in_bounds(offset: u32, len: u32, granted_len: usize) -> bool {
    if len == 0 {
        return false;
    }
    // offset + len, checked against the granted length without overflow.
    match (offset as u64).checked_add(len as u64) {
        Some(end) => end <= granted_len as u64,
        None => false,
    }
}

// ------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn req_roundtrip(req: AudioRequest) {
        let mut buf = [0u8; AUDIO_REQUEST_MAX_SIZE];
        let n = encode_request(&req, &mut buf).expect("encode");
        let decoded = decode_request(&buf[..n]).expect("decode");
        assert_eq!(decoded, req, "request round-trip mismatch");
    }

    fn rsp_roundtrip(rsp: AudioResponse) {
        let mut buf = [0u8; AUDIO_RESPONSE_MAX_SIZE];
        let n = encode_response(&rsp, &mut buf).expect("encode");
        let decoded = decode_response(&buf[..n]).expect("decode");
        assert_eq!(decoded, rsp, "response round-trip mismatch");
    }

    #[test]
    fn request_roundtrip() {
        req_roundtrip(AudioRequest::QueryCaps);
        req_roundtrip(AudioRequest::OpenStream {
            format: PcmFormat::S16Le,
            rate: SampleRate::Hz48000,
            layout: ChannelLayout::Stereo,
        });
        req_roundtrip(AudioRequest::OpenStream {
            format: PcmFormat::S16Le,
            rate: SampleRate::Hz48000,
            layout: ChannelLayout::Mono,
        });
        req_roundtrip(AudioRequest::SubmitFrames {
            stream_id: 1,
            grant_handle: 0xDEAD_BEEF,
            offset: 0x1000,
            len: 0x2000,
        });
        req_roundtrip(AudioRequest::Drain { stream_id: 7 });
        req_roundtrip(AudioRequest::CloseStream { stream_id: 9 });
    }

    #[test]
    fn response_roundtrip() {
        rsp_roundtrip(AudioResponse::Caps(caps_v1()));
        rsp_roundtrip(AudioResponse::StreamOpened(1));
        rsp_roundtrip(AudioResponse::Ack {
            frames_consumed: 0x0102_0304_0506_0708,
        });
        rsp_roundtrip(AudioResponse::WouldBlock);
        rsp_roundtrip(AudioResponse::Ok);
        rsp_roundtrip(AudioResponse::Err(AudioDriverError::Internal));
    }

    #[test]
    fn would_block_roundtrip() {
        // The backpressure variant must survive the seam byte-for-byte so
        // `AudioProxyBackend` can map it back to `AudioError::WouldBlock`.
        let mut buf = [0u8; AUDIO_RESPONSE_MAX_SIZE];
        let n = encode_response(&AudioResponse::WouldBlock, &mut buf).unwrap();
        assert_eq!(n, 1);
        assert_eq!(buf[0], RSP_WOULD_BLOCK);
        assert_eq!(
            decode_response(&buf[..n]).unwrap(),
            AudioResponse::WouldBlock
        );
    }

    #[test]
    fn every_driver_error_byte_roundtrips() {
        for e in [
            AudioDriverError::Busy,
            AudioDriverError::NoDevice,
            AudioDriverError::InvalidFormat,
            AudioDriverError::InvalidArgument,
            AudioDriverError::DeviceAbsent,
            AudioDriverError::DriverRestarting,
            AudioDriverError::Internal,
        ] {
            assert_eq!(AudioDriverError::from_byte(e.to_byte()), Some(e));
            rsp_roundtrip(AudioResponse::Err(e));
        }
    }

    #[test]
    fn decode_rejects_truncated_and_unknown() {
        assert_eq!(decode_request(&[]), Err(DecodeError::Truncated));
        assert_eq!(
            decode_request(&[REQ_OPEN_STREAM, 0]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode_request(&[REQ_SUBMIT_FRAMES, 1, 2]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(decode_request(&[0xFE]), Err(DecodeError::UnknownTag));
        // Bad format byte in OpenStream.
        assert_eq!(
            decode_request(&[REQ_OPEN_STREAM, 0xFF, 0, 0]),
            Err(DecodeError::InvalidEnum)
        );
        assert_eq!(decode_response(&[]), Err(DecodeError::Truncated));
        assert_eq!(decode_response(&[0xFE]), Err(DecodeError::UnknownTag));
        assert_eq!(
            decode_response(&[RSP_ERR, 0xFF]),
            Err(DecodeError::InvalidEnum)
        );
    }

    #[test]
    fn submit_frames_has_no_inline_sample_field() {
        // Compile-time guarantee, asserted structurally: the largest request
        // is SubmitFrames at 17 bytes (tag + 4×u32). A request that inlined
        // even a single PCM frame (≥ 4 bytes/stereo-S16) into the message
        // would blow this bound. This pins the "no bulk in IPC" rule.
        assert_eq!(AUDIO_REQUEST_MAX_SIZE, 17);
        let mut buf = [0u8; AUDIO_REQUEST_MAX_SIZE];
        let n = encode_request(
            &AudioRequest::SubmitFrames {
                stream_id: 1,
                grant_handle: 2,
                offset: 3,
                len: 0x7FFF_FFFF,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(n, 17);
    }

    #[test]
    fn caps_v1_is_48k_stereo_s16() {
        let c = caps_v1();
        assert_eq!(c.format, PcmFormat::S16Le);
        assert_eq!(c.rate, SampleRate::Hz48000);
        assert_eq!(c.layout, ChannelLayout::Stereo);
        assert!(c.max_submit_bytes > 0);
    }

    #[test]
    fn submission_bounds() {
        // In-range windows are accepted.
        assert!(submission_in_bounds(0, 4096, 4096));
        assert!(submission_in_bounds(2048, 2048, 4096));
        assert!(submission_in_bounds(0, 1, 4096));
        // Out-of-range / zero / overflow are rejected.
        assert!(!submission_in_bounds(0, 0, 4096)); // zero-length
        assert!(!submission_in_bounds(4096, 1, 4096)); // offset at end
        assert!(!submission_in_bounds(2048, 2049, 4096)); // spills past end
        assert!(!submission_in_bounds(u32::MAX, 2, 4096)); // overflow
        assert!(!submission_in_bounds(0, 4097, 4096)); // len > granted
    }

    #[test]
    fn encode_returns_none_on_short_buffer() {
        let mut tiny = [0u8; 1];
        assert_eq!(
            encode_request(
                &AudioRequest::SubmitFrames {
                    stream_id: 1,
                    grant_handle: 2,
                    offset: 3,
                    len: 4
                },
                &mut tiny
            ),
            None
        );
        assert_eq!(
            encode_response(&AudioResponse::Ack { frames_consumed: 1 }, &mut tiny),
            None
        );
    }
}
