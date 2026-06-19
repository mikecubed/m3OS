//! USB Video Class (UVC 1.0/1.1) pure logic — Phase 92c Track E.2.
//!
//! Hardware-free helpers the ring-3 `usb-video` driver uses to bind a USB
//! camera and negotiate a capture stream:
//!
//! * [`find_video_stream`] walks a parsed configuration tree and locates the
//!   VideoStreaming interface alt-setting that carries an IN endpoint suitable
//!   for capture.  Bulk IN is preferred (no new xHCI isochronous-IN code
//!   needed); isochronous IN is the fallback for cameras that have no bulk
//!   alt-setting.
//! * [`set_interface_setup`] builds the standard `SET_INTERFACE` SETUP packet
//!   that activates an alt-setting (mirrors the UAC helper of the same name).
//! * [`probe_control_setup`] builds the UVC class-specific SETUP packet for
//!   both the `VS_PROBE_CONTROL` and `VS_COMMIT_CONTROL` selectors (the
//!   selector is a parameter) (UVC 1.1 §4.3.1.1), used by the `SET_CUR` /
//!   `GET_CUR` requests that negotiate the capture format and frame size.
//! * [`UvcStreamingControl`] is the 26-byte UVC 1.0/1.1 Probe/Commit control
//!   block with [`UvcStreamingControl::encode`] / [`UvcStreamingControl::parse`]
//!   round-trip helpers.
//! * [`negotiate_default`] returns a minimal control block requesting
//!   format index 1, frame index 1 at 30 fps — sufficient for a first-bring-up
//!   negotiation without parsing class-specific VS format/frame descriptors.
//!
//! # Deferred items
//!
//! Full parsing of UVC class-specific VS Format/Frame descriptors (for
//! selecting between YUY2 / MJPEG / H.264 payloads and enumerating supported
//! resolutions) is deferred to a later sub-phase.  The live capture path is
//! bare-metal / VFIO-only (QEMU has no UVC device model); this module is the
//! CI-verifiable host-tested codec deliverable.
//!
//! No MMIO/DMA — host-testable via
//! `cargo test -p kernel-core --target x86_64-unknown-linux-gnu usb::uvc`.

extern crate alloc;

use crate::usb::descriptor::{
    CLASS_VIDEO, ParsedConfig, SUBCLASS_VIDEO_STREAMING, TRANSFER_TYPE_BULK, TRANSFER_TYPE_ISOCH,
};
use crate::usb::xhci::trb::dci;

// ---------------------------------------------------------------------------
// UVC 1.1 class-specific request codes (UVC §A.8)
// ---------------------------------------------------------------------------

/// UVC class-specific `SET_CUR` request (UVC §A.8, value 0x01).
pub const UVC_SET_CUR: u8 = 0x01;
/// UVC class-specific `GET_CUR` request (UVC §A.8, value 0x81).
pub const UVC_GET_CUR: u8 = 0x81;
/// UVC class-specific `GET_MAX` request (UVC §A.8, value 0x83).
pub const UVC_GET_MAX: u8 = 0x83;

// ---------------------------------------------------------------------------
// UVC VideoStreaming control selectors (UVC §A.9.8)
// ---------------------------------------------------------------------------

/// `VS_PROBE_CONTROL` selector — negotiate streaming parameters without
/// committing (UVC §A.9.8, value 0x01).
pub const VS_PROBE_CONTROL: u8 = 0x01;
/// `VS_COMMIT_CONTROL` selector — commit the negotiated parameters and start
/// the stream (UVC §A.9.8, value 0x02).
pub const VS_COMMIT_CONTROL: u8 = 0x02;

// ---------------------------------------------------------------------------
// bmRequestType constants for UVC control requests
// ---------------------------------------------------------------------------

/// `bmRequestType` for a class SET (host→device, class, interface recipient).
const BM_REQUEST_TYPE_CLASS_SET: u8 = 0x21;
/// `bmRequestType` for a class GET (device→host, class, interface recipient).
const BM_REQUEST_TYPE_CLASS_GET: u8 = 0xA1;

// ---------------------------------------------------------------------------
// UvcStreamInfo
// ---------------------------------------------------------------------------

/// The VideoStreaming capture endpoint discovered on a UVC device, plus the
/// interface / alt-setting that must be activated to use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UvcStreamInfo {
    /// `bInterfaceNumber` of the VideoStreaming interface (the `SET_INTERFACE`
    /// `wIndex`).
    pub interface_num: u8,
    /// `bAlternateSetting` to activate.  Alt 0 is the zero-bandwidth idle
    /// setting for isochronous configurations; for bulk-only devices the
    /// capture endpoint lives on alt 0 and this field is 0.
    pub alt_setting: u8,
    /// `bEndpointAddress` of the capture IN endpoint.
    pub ep_address: u8,
    /// xHCI Device Context Index of the IN endpoint.
    pub ep_dci: u8,
    /// `wMaxPacketSize` of the IN endpoint.
    pub mps: u16,
    /// `true` if the endpoint is isochronous, `false` if bulk.
    pub is_isoch: bool,
}

/// Find the first VideoStreaming interface alt-setting that exposes an IN
/// endpoint (bulk preferred; isochronous as fallback), or `None` if the
/// device exposes no VideoStreaming interface.
///
/// # Selection policy
///
/// UVC cameras can expose multiple alt-settings on the VideoStreaming
/// interface.  Alt 0 is always zero-bandwidth (no capture endpoint) for
/// isochronous designs.  This function:
///
/// 1. Collects all `CLASS_VIDEO` / `SUBCLASS_VIDEO_STREAMING` interfaces.
/// 2. Among those that carry an IN endpoint, prefers the first **bulk** IN
///    (avoids new xHCI isochronous-IN TRB plumbing) over the first
///    isochronous IN.
/// 3. Falls back to isochronous if no bulk IN alt-setting exists.
/// 4. Returns `None` if no VideoStreaming IN endpoint is found at all.
pub fn find_video_stream(cfg: &ParsedConfig) -> Option<UvcStreamInfo> {
    let mut bulk_candidate: Option<UvcStreamInfo> = None;
    let mut isoch_candidate: Option<UvcStreamInfo> = None;

    for iface in &cfg.interfaces {
        let i = &iface.interface;
        if i.b_interface_class != CLASS_VIDEO || i.b_interface_sub_class != SUBCLASS_VIDEO_STREAMING
        {
            continue;
        }
        for ep in &iface.endpoints {
            if !ep.is_in() {
                continue;
            }
            let tt = ep.transfer_type();
            if tt == TRANSFER_TYPE_BULK && bulk_candidate.is_none() {
                bulk_candidate = Some(UvcStreamInfo {
                    interface_num: i.b_interface_number,
                    alt_setting: i.b_alternate_setting,
                    ep_address: ep.b_endpoint_address,
                    ep_dci: dci(ep.endpoint_number(), true),
                    mps: ep.w_max_packet_size,
                    is_isoch: false,
                });
            } else if tt == TRANSFER_TYPE_ISOCH && isoch_candidate.is_none() {
                isoch_candidate = Some(UvcStreamInfo {
                    interface_num: i.b_interface_number,
                    alt_setting: i.b_alternate_setting,
                    ep_address: ep.b_endpoint_address,
                    ep_dci: dci(ep.endpoint_number(), true),
                    mps: ep.w_max_packet_size,
                    is_isoch: true,
                });
            }
        }
    }

    bulk_candidate.or(isoch_candidate)
}

// ---------------------------------------------------------------------------
// SETUP packet builders
// ---------------------------------------------------------------------------

/// Build the standard `SET_INTERFACE` SETUP packet (USB 2.0 §9.4.10) that
/// selects alt-setting `alt` on interface `iface`.
///
/// `bmRequestType = 0x01` (host→device, standard, interface recipient),
/// `bRequest = 0x0B`, no data stage (`wLength = 0`).
pub const fn set_interface_setup(iface: u8, alt: u8) -> [u8; 8] {
    [
        0x01,  // bmRequestType: OUT | Standard | Interface
        0x0B,  // bRequest: SET_INTERFACE
        alt,   // wValue lo = alternate setting
        0x00,  // wValue hi
        iface, // wIndex lo = interface number
        0x00,  // wIndex hi
        0x00,  // wLength lo
        0x00,  // wLength hi
    ]
}

/// Build a UVC class-specific Probe/Commit control SETUP packet.
///
/// Used for both `VS_PROBE_CONTROL` and `VS_COMMIT_CONTROL` on `SET_CUR`
/// (host→device) and `GET_CUR`/`GET_MAX` (device→host) requests.
///
/// # Arguments
///
/// * `b_request` — `UVC_SET_CUR` (0x01), `UVC_GET_CUR` (0x81), or
///   `UVC_GET_MAX` (0x83).
/// * `control_selector` — `VS_PROBE_CONTROL` (0x01) or
///   `VS_COMMIT_CONTROL` (0x02).
/// * `iface` — `wIndex` = VideoStreaming `bInterfaceNumber`.
///
/// `wValue = control_selector << 8`, `wLength = 26` (the UVC 1.0/1.1
/// streaming-control block size).
pub const fn probe_control_setup(b_request: u8, control_selector: u8, iface: u8) -> [u8; 8] {
    let bm_request_type = if b_request == UVC_SET_CUR {
        BM_REQUEST_TYPE_CLASS_SET
    } else {
        BM_REQUEST_TYPE_CLASS_GET
    };
    [
        bm_request_type,  // bmRequestType
        b_request,        // bRequest
        0x00,             // wValue lo (reserved)
        control_selector, // wValue hi = control selector
        iface,            // wIndex lo = interface number
        0x00,             // wIndex hi
        26,               // wLength lo = 26 (UVC 1.0/1.1 control block)
        0x00,             // wLength hi
    ]
}

// ---------------------------------------------------------------------------
// UVC 1.0/1.1 Streaming Control block (26 bytes)
// ---------------------------------------------------------------------------

/// UVC 1.0/1.1 VideoStreaming Probe/Commit control block (UVC §4.3.1.1,
/// Table 4-75 in UVC 1.1).
///
/// The host sends this block to `VS_PROBE_CONTROL` to propose streaming
/// parameters (format, frame size, frame rate), the device adjusts any
/// parameters it cannot satisfy and replies, and the host then sends a
/// final `VS_COMMIT_CONTROL` to activate the stream.
///
/// Wire layout (26 bytes, all little-endian):
///
/// | Offset | Size | Field                  |
/// |--------|------|------------------------|
/// | 0      | 2    | `bmHint`               |
/// | 2      | 1    | `bFormatIndex`         |
/// | 3      | 1    | `bFrameIndex`          |
/// | 4      | 4    | `dwFrameInterval`      |
/// | 8      | 2    | `wKeyFrameRate`        |
/// | 10     | 2    | `wPFrameRate`          |
/// | 12     | 2    | `wCompQuality`         |
/// | 14     | 2    | `wCompWindowSize`      |
/// | 16     | 2    | `wDelay`               |
/// | 18     | 4    | `dwMaxVideoFrameSize`  |
/// | 22     | 4    | `dwMaxPayloadTransferSize` |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UvcStreamingControl {
    /// `bmHint` — hint bitmap telling the device which fields the host
    /// considers fixed (bit 0 = `dwFrameInterval`; bit 1 = `wKeyFrameRate`;
    /// etc.).  A value of 1 hints that `dwFrameInterval` is the primary
    /// constraint.
    pub bm_hint: u16,
    /// `bFormatIndex` — 1-based index of the VS Format descriptor to use.
    pub b_format_index: u8,
    /// `bFrameIndex` — 1-based index of the VS Frame descriptor to use.
    pub b_frame_index: u8,
    /// `dwFrameInterval` — desired frame interval in 100 ns units.
    /// 333_333 = 30 fps (`10_000_000 / 30`).
    pub dw_frame_interval: u32,
    /// `wKeyFrameRate` — key-frame rate (0 = default / unspecified).
    pub w_key_frame_rate: u16,
    /// `wPFrameRate` — P-frame rate (0 = default / unspecified).
    pub w_p_frame_rate: u16,
    /// `wCompQuality` — compression quality (0–10_000; 0 = default).
    pub w_comp_quality: u16,
    /// `wCompWindowSize` — compression window size (0 = default).
    pub w_comp_window_size: u16,
    /// `wDelay` — latency from capture to USB stream (ms; 0 = default).
    pub w_delay: u16,
    /// `dwMaxVideoFrameSize` — maximum frame payload size in bytes.
    pub dw_max_video_frame_size: u32,
    /// `dwMaxPayloadTransferSize` — maximum USB payload per transfer.
    pub dw_max_payload_transfer_size: u32,
}

impl UvcStreamingControl {
    /// Serialize the control block into the 26-byte wire format.
    pub fn encode(&self) -> [u8; 26] {
        let mut b = [0u8; 26];
        b[0..2].copy_from_slice(&self.bm_hint.to_le_bytes());
        b[2] = self.b_format_index;
        b[3] = self.b_frame_index;
        b[4..8].copy_from_slice(&self.dw_frame_interval.to_le_bytes());
        b[8..10].copy_from_slice(&self.w_key_frame_rate.to_le_bytes());
        b[10..12].copy_from_slice(&self.w_p_frame_rate.to_le_bytes());
        b[12..14].copy_from_slice(&self.w_comp_quality.to_le_bytes());
        b[14..16].copy_from_slice(&self.w_comp_window_size.to_le_bytes());
        b[16..18].copy_from_slice(&self.w_delay.to_le_bytes());
        b[18..22].copy_from_slice(&self.dw_max_video_frame_size.to_le_bytes());
        b[22..26].copy_from_slice(&self.dw_max_payload_transfer_size.to_le_bytes());
        b
    }

    /// Deserialize a 26-byte wire buffer into a `UvcStreamingControl`.
    ///
    /// Returns `None` if the slice is shorter than 26 bytes.
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 26 {
            return None;
        }
        Some(UvcStreamingControl {
            bm_hint: u16::from_le_bytes([b[0], b[1]]),
            b_format_index: b[2],
            b_frame_index: b[3],
            dw_frame_interval: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            w_key_frame_rate: u16::from_le_bytes([b[8], b[9]]),
            w_p_frame_rate: u16::from_le_bytes([b[10], b[11]]),
            w_comp_quality: u16::from_le_bytes([b[12], b[13]]),
            w_comp_window_size: u16::from_le_bytes([b[14], b[15]]),
            w_delay: u16::from_le_bytes([b[16], b[17]]),
            dw_max_video_frame_size: u32::from_le_bytes([b[18], b[19], b[20], b[21]]),
            dw_max_payload_transfer_size: u32::from_le_bytes([b[22], b[23], b[24], b[25]]),
        })
    }
}

/// Build a minimal default Probe/Commit control block requesting format index
/// 1, frame index 1 at 30 fps.
///
/// This is sufficient for a first-bring-up negotiation without parsing
/// class-specific VS Format/Frame descriptors.  The host sends this to
/// `GET_MAX(VS_PROBE_CONTROL)` first to learn the device's maximum, then
/// issues `SET_CUR(VS_PROBE_CONTROL)` with the negotiated result, and
/// finally `SET_CUR(VS_COMMIT_CONTROL)` to lock it in.
///
/// Full format/frame descriptor parsing (for enumerating supported YUY2,
/// MJPEG, and H.264 payload types and their resolutions) is deferred.
pub const fn negotiate_default() -> UvcStreamingControl {
    UvcStreamingControl {
        bm_hint: 0x0001, // hint: dwFrameInterval is the primary constraint
        b_format_index: 1,
        b_frame_index: 1,
        dw_frame_interval: 333_333, // 30 fps in 100-ns units
        w_key_frame_rate: 0,
        w_p_frame_rate: 0,
        w_comp_quality: 0,
        w_comp_window_size: 0,
        w_delay: 0,
        dw_max_video_frame_size: 0,
        dw_max_payload_transfer_size: 0,
    }
}

// ---------------------------------------------------------------------------
// Camera IPC codec
// ---------------------------------------------------------------------------

/// Minimal IPC protocol between `usb-video` (the producer) and
/// `camera_server` (the consumer).
///
/// This module is host-testable; it carries no MMIO/DMA dependency.
pub mod camera_ipc {
    extern crate alloc;
    use alloc::vec::Vec;

    /// IPC label for camera control messages.
    pub const CAMERA_REQ_LABEL: u64 = 0xCA_00;

    /// Maximum wire size of a `CameraRequest` or `CameraReply` (bytes).
    pub const CAMERA_MSG_MAX: usize = 64;

    // -----------------------------------------------------------------------
    // Request
    // -----------------------------------------------------------------------

    /// Requests from `usb-video` to `camera_server`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CameraRequest {
        /// Client wants to know the current capture format.
        QueryFormat,
        /// Producer pushes a newly captured frame.
        ///
        /// `seq` is the monotonically increasing frame sequence number;
        /// `len` is the number of bytes in the frame payload.  The actual
        /// pixel data is *not* inlined in the IPC message (too large for the
        /// kernel's message budget); the consumer reads it from shared memory
        /// via a separate bulk transfer once it receives this notification.
        PushFrame { seq: u64, len: u32 },
    }

    impl CameraRequest {
        /// Tag byte for `QueryFormat`.
        const TAG_QUERY_FORMAT: u8 = 0x01;
        /// Tag byte for `PushFrame`.
        const TAG_PUSH_FRAME: u8 = 0x02;

        /// Serialize to a wire buffer (≤ [`CAMERA_MSG_MAX`] bytes).
        pub fn encode(&self) -> Vec<u8> {
            match self {
                CameraRequest::QueryFormat => {
                    alloc::vec![Self::TAG_QUERY_FORMAT]
                }
                CameraRequest::PushFrame { seq, len } => {
                    let mut b = alloc::vec![Self::TAG_PUSH_FRAME];
                    b.extend_from_slice(&seq.to_le_bytes());
                    b.extend_from_slice(&len.to_le_bytes());
                    b
                }
            }
        }

        /// Deserialize from a wire buffer. Returns `None` on malformed input.
        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.is_empty() {
                return None;
            }
            match b[0] {
                Self::TAG_QUERY_FORMAT => Some(CameraRequest::QueryFormat),
                Self::TAG_PUSH_FRAME => {
                    if b.len() < 13 {
                        return None;
                    }
                    let seq = u64::from_le_bytes([b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]]);
                    let len = u32::from_le_bytes([b[9], b[10], b[11], b[12]]);
                    Some(CameraRequest::PushFrame { seq, len })
                }
                _ => None,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Reply
    // -----------------------------------------------------------------------

    /// Replies from `camera_server` to `usb-video` (or a viewer client).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CameraReply {
        /// Camera is active; fields describe the current capture format.
        Format {
            /// Frame width in pixels (0 if unknown / not yet negotiated).
            width: u16,
            /// Frame height in pixels (0 if unknown / not yet negotiated).
            height: u16,
            /// Pixel format fourcc (e.g. `b"YUY2"`, `b"MJPG"`).
            fmt: [u8; 4],
        },
        /// A frame is available at the given sequence number and byte length.
        Frame { seq: u64, len: u32 },
        /// No frame has been received yet.
        NoFrame,
        /// Frame push acknowledged.
        Ack,
    }

    impl CameraReply {
        const TAG_FORMAT: u8 = 0x01;
        const TAG_FRAME: u8 = 0x02;
        const TAG_NO_FRAME: u8 = 0x03;
        const TAG_ACK: u8 = 0x04;

        /// Serialize to a wire buffer (≤ [`CAMERA_MSG_MAX`] bytes).
        pub fn encode(&self) -> Vec<u8> {
            match self {
                CameraReply::Format { width, height, fmt } => {
                    let mut b = alloc::vec![Self::TAG_FORMAT];
                    b.extend_from_slice(&width.to_le_bytes());
                    b.extend_from_slice(&height.to_le_bytes());
                    b.extend_from_slice(fmt);
                    b
                }
                CameraReply::Frame { seq, len } => {
                    let mut b = alloc::vec![Self::TAG_FRAME];
                    b.extend_from_slice(&seq.to_le_bytes());
                    b.extend_from_slice(&len.to_le_bytes());
                    b
                }
                CameraReply::NoFrame => alloc::vec![Self::TAG_NO_FRAME],
                CameraReply::Ack => alloc::vec![Self::TAG_ACK],
            }
        }

        /// Deserialize from a wire buffer. Returns `None` on malformed input.
        pub fn decode(b: &[u8]) -> Option<Self> {
            if b.is_empty() {
                return None;
            }
            match b[0] {
                Self::TAG_FORMAT => {
                    if b.len() < 9 {
                        return None;
                    }
                    let width = u16::from_le_bytes([b[1], b[2]]);
                    let height = u16::from_le_bytes([b[3], b[4]]);
                    let fmt = [b[5], b[6], b[7], b[8]];
                    Some(CameraReply::Format { width, height, fmt })
                }
                Self::TAG_FRAME => {
                    if b.len() < 13 {
                        return None;
                    }
                    let seq = u64::from_le_bytes([b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]]);
                    let len = u32::from_le_bytes([b[9], b[10], b[11], b[12]]);
                    Some(CameraReply::Frame { seq, len })
                }
                Self::TAG_NO_FRAME => Some(CameraReply::NoFrame),
                Self::TAG_ACK => Some(CameraReply::Ack),
                _ => None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::descriptor::parse_config_tree;
    use crate::usb::xhci::trb::dci as compute_dci;

    // -----------------------------------------------------------------------
    // Synthetic UVC configuration blob
    //
    // Layout (all descriptor lengths in bytes):
    //   Config(9) = 0x09 / type 0x02
    //   + VideoControl  interface 0, alt 0, class 0x0E subclass 0x01, 0 eps  (9)
    //   + VideoStreaming interface 1, alt 0, class 0x0E subclass 0x02, 0 eps  (9)
    //     (zero-bandwidth idle alt-setting)
    //   + VideoStreaming interface 1, alt 1, class 0x0E subclass 0x02, 1 ep   (9)
    //   + Isochronous IN endpoint, 9-byte audio-endpoint-style:
    //     bEndpointAddress=0x81 (IN ep1), bmAttributes=0x05 (isoch/async),
    //     wMaxPacketSize=1024 (0x0400), bInterval=1
    //   Total = 9 + 9 + 9 + 9 + 7 = 43 = 0x2B
    // -----------------------------------------------------------------------
    const UVC_ISOCH_CONFIG: &[u8] = &[
        // Configuration descriptor
        0x09, 0x02, 0x2B, 0x00, 0x02, 0x01, 0x00, 0x80, 0x32,
        // VideoControl interface 0, alt 0, class 0x0E subclass 0x01, 0 endpoints
        0x09, 0x04, 0x00, 0x00, 0x00, 0x0E, 0x01, 0x00, 0x00,
        // VideoStreaming interface 1, alt 0, class 0x0E subclass 0x02, 0 endpoints
        0x09, 0x04, 0x01, 0x00, 0x00, 0x0E, 0x02, 0x00, 0x00,
        // VideoStreaming interface 1, alt 1, class 0x0E subclass 0x02, 1 endpoint
        0x09, 0x04, 0x01, 0x01, 0x01, 0x0E, 0x02, 0x00, 0x00,
        // Isochronous IN endpoint:
        // bEndpointAddress=0x81 (IN, ep1), bmAttributes=0x05 (isoch/async),
        // wMaxPacketSize=1024 (0x0400), bInterval=1
        0x07, 0x05, 0x81, 0x05, 0x00, 0x04, 0x01,
    ];

    // -----------------------------------------------------------------------
    // Synthetic UVC config with bulk IN on alt 0 (preferred over isoch).
    //
    // Layout:
    //   Config(9)
    //   + VideoControl interface 0 alt 0, class 0x0E subclass 0x01, 0 eps (9)
    //   + VideoStreaming interface 1 alt 0, class 0x0E subclass 0x02, 1 ep (9)
    //   + Bulk IN endpoint ep2 (7)
    //   Total = 9 + 9 + 9 + 7 = 34 = 0x22
    // -----------------------------------------------------------------------
    const UVC_BULK_CONFIG: &[u8] = &[
        // Configuration descriptor
        0x09, 0x02, 0x22, 0x00, 0x02, 0x01, 0x00, 0x80, 0x32,
        // VideoControl interface 0, alt 0, class 0x0E subclass 0x01, 0 endpoints
        0x09, 0x04, 0x00, 0x00, 0x00, 0x0E, 0x01, 0x00, 0x00,
        // VideoStreaming interface 1, alt 0, class 0x0E subclass 0x02, 1 endpoint
        0x09, 0x04, 0x01, 0x00, 0x01, 0x0E, 0x02, 0x00, 0x00,
        // Bulk IN endpoint: bEndpointAddress=0x82 (IN, ep2), bmAttributes=0x02,
        // wMaxPacketSize=512 (0x0200), bInterval=0
        0x07, 0x05, 0x82, 0x02, 0x00, 0x02, 0x00,
    ];

    // -----------------------------------------------------------------------
    // find_video_stream tests
    // -----------------------------------------------------------------------

    #[test]
    fn finds_isoch_in_stream_on_alt1() {
        let cfg = parse_config_tree(UVC_ISOCH_CONFIG).expect("parse");
        let info = find_video_stream(&cfg).expect("VideoStreaming IN stream found");
        assert_eq!(info.interface_num, 1, "VideoStreaming interface number");
        assert_eq!(info.alt_setting, 1, "alt 1 carries the isoch endpoint");
        assert_eq!(info.ep_address, 0x81, "IN endpoint 1");
        // EP1 IN → DCI = 2*1 + 1 = 3.
        assert_eq!(info.ep_dci, compute_dci(1, true));
        assert_eq!(info.mps, 1024);
        assert!(info.is_isoch, "endpoint is isochronous");
    }

    #[test]
    fn finds_bulk_in_stream_on_alt0() {
        let cfg = parse_config_tree(UVC_BULK_CONFIG).expect("parse");
        let info = find_video_stream(&cfg).expect("VideoStreaming bulk IN found");
        assert_eq!(info.interface_num, 1);
        assert_eq!(info.alt_setting, 0, "bulk endpoint lives on alt 0");
        assert_eq!(info.ep_address, 0x82, "IN endpoint 2");
        // EP2 IN → DCI = 2*2 + 1 = 5.
        assert_eq!(info.ep_dci, compute_dci(2, true));
        assert_eq!(info.mps, 512);
        assert!(!info.is_isoch, "endpoint is bulk");
    }

    #[test]
    fn prefers_bulk_over_isoch() {
        // Config with both a VideoStreaming alt 0 bulk IN and an alt 1 isoch IN.
        // The parser must return the bulk one.
        //
        // Layout:
        //   Config(9)
        //   + VideoControl 0/0 0x0E/0x01 (9)
        //   + VideoStreaming 1/0 0x0E/0x02 1ep (9) + Bulk IN ep2 (7)
        //   + VideoStreaming 1/1 0x0E/0x02 1ep (9) + Isoch IN ep1 (7)
        //   Total = 9+9+7+9+9+7 = 50 = 0x32
        const MIXED: &[u8] = &[
            0x09, 0x02, 0x32, 0x00, 0x02, 0x01, 0x00, 0x80, 0x32, 0x09, 0x04, 0x00, 0x00, 0x00,
            0x0E, 0x01, 0x00, 0x00, 0x09, 0x04, 0x01, 0x00, 0x01, 0x0E, 0x02, 0x00, 0x00, 0x07,
            0x05, 0x82, 0x02, 0x00, 0x02, 0x00, // Bulk IN ep2
            0x09, 0x04, 0x01, 0x01, 0x01, 0x0E, 0x02, 0x00, 0x00, 0x07, 0x05, 0x81, 0x05, 0x00,
            0x04, 0x01, // Isoch IN ep1
        ];
        let cfg = parse_config_tree(MIXED).expect("parse");
        let info = find_video_stream(&cfg).expect("stream found");
        assert!(!info.is_isoch, "bulk preferred over isoch");
        assert_eq!(info.ep_address, 0x82);
    }

    #[test]
    fn no_stream_when_no_video_streaming_iface() {
        // Only a VideoControl interface — no VideoStreaming alt with an IN endpoint.
        const NO_STREAM: &[u8] = &[
            0x09, 0x02, 0x12, 0x00, 0x01, 0x01, 0x00, 0x80, 0x32, 0x09, 0x04, 0x00, 0x00, 0x00,
            0x0E, 0x01, 0x00, 0x00,
        ];
        let cfg = parse_config_tree(NO_STREAM).expect("parse");
        assert_eq!(find_video_stream(&cfg), None);
    }

    // -----------------------------------------------------------------------
    // UvcStreamingControl encode/decode round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn streaming_control_encode_decode_roundtrip() {
        let ctrl = UvcStreamingControl {
            bm_hint: 0x0001,
            b_format_index: 2,
            b_frame_index: 3,
            dw_frame_interval: 333_333,
            w_key_frame_rate: 10,
            w_p_frame_rate: 5,
            w_comp_quality: 7000,
            w_comp_window_size: 4,
            w_delay: 100,
            dw_max_video_frame_size: 614_400,
            dw_max_payload_transfer_size: 3_072,
        };
        let encoded = ctrl.encode();
        assert_eq!(encoded.len(), 26);
        let decoded = UvcStreamingControl::parse(&encoded).expect("parse round-trip");
        assert_eq!(decoded, ctrl);
    }

    #[test]
    fn streaming_control_encode_known_bytes() {
        // Verify the byte layout against a known good encoding (checked by
        // hand against UVC 1.1 Table 4-75).
        let ctrl = negotiate_default();
        let b = ctrl.encode();
        // bmHint = 0x0001 (little-endian)
        assert_eq!(b[0], 0x01);
        assert_eq!(b[1], 0x00);
        // bFormatIndex = 1
        assert_eq!(b[2], 1);
        // bFrameIndex = 1
        assert_eq!(b[3], 1);
        // dwFrameInterval = 333_333 = 0x00051615 → LE bytes: 0x15, 0x16, 0x05, 0x00
        assert_eq!(b[4], 0x15);
        assert_eq!(b[5], 0x16);
        assert_eq!(b[6], 0x05);
        assert_eq!(b[7], 0x00);
        // wLength placeholder fields all zero
        assert_eq!(&b[8..26], &[0u8; 18]);
    }

    #[test]
    fn streaming_control_parse_returns_none_for_short_slice() {
        assert_eq!(UvcStreamingControl::parse(&[0u8; 25]), None);
        // Exactly 26 bytes must succeed.
        assert!(UvcStreamingControl::parse(&[0u8; 26]).is_some());
    }

    // -----------------------------------------------------------------------
    // SETUP packet layout tests
    // -----------------------------------------------------------------------

    #[test]
    fn set_interface_setup_encodes_alt() {
        let s = set_interface_setup(1, 1);
        assert_eq!(s, [0x01, 0x0B, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00]);
        assert_eq!(u16::from_le_bytes([s[6], s[7]]), 0, "wLength must be 0");
    }

    #[test]
    fn probe_control_setup_set_cur_probe() {
        let s = probe_control_setup(UVC_SET_CUR, VS_PROBE_CONTROL, 1);
        // bmRequestType = 0x21 (class | interface | OUT)
        assert_eq!(s[0], BM_REQUEST_TYPE_CLASS_SET);
        // bRequest = SET_CUR
        assert_eq!(s[1], UVC_SET_CUR);
        // wValue lo = 0, wValue hi = VS_PROBE_CONTROL
        assert_eq!(s[2], 0x00);
        assert_eq!(s[3], VS_PROBE_CONTROL);
        // wIndex = interface 1
        assert_eq!(s[4], 1);
        assert_eq!(s[5], 0);
        // wLength = 26
        assert_eq!(u16::from_le_bytes([s[6], s[7]]), 26);
    }

    #[test]
    fn probe_control_setup_get_cur_commit() {
        let s = probe_control_setup(UVC_GET_CUR, VS_COMMIT_CONTROL, 2);
        // bmRequestType = 0xA1 (class | interface | IN)
        assert_eq!(s[0], BM_REQUEST_TYPE_CLASS_GET);
        assert_eq!(s[1], UVC_GET_CUR);
        assert_eq!(s[3], VS_COMMIT_CONTROL);
        assert_eq!(s[4], 2, "wIndex lo = interface 2");
        assert_eq!(u16::from_le_bytes([s[6], s[7]]), 26);
    }

    #[test]
    fn probe_control_setup_get_max_probe() {
        let s = probe_control_setup(UVC_GET_MAX, VS_PROBE_CONTROL, 0);
        assert_eq!(s[0], BM_REQUEST_TYPE_CLASS_GET);
        assert_eq!(s[1], UVC_GET_MAX);
        assert_eq!(s[3], VS_PROBE_CONTROL);
    }

    // -----------------------------------------------------------------------
    // camera_ipc codec round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn camera_request_query_format_roundtrip() {
        use crate::usb::uvc::camera_ipc::CameraRequest;
        let req = CameraRequest::QueryFormat;
        let encoded = req.encode();
        let decoded = CameraRequest::decode(&encoded).expect("decode QueryFormat");
        assert_eq!(decoded, req);
    }

    #[test]
    fn camera_request_push_frame_roundtrip() {
        use crate::usb::uvc::camera_ipc::CameraRequest;
        let req = CameraRequest::PushFrame {
            seq: 0xDEAD_BEEF_1234_5678,
            len: 614_400,
        };
        let encoded = req.encode();
        assert_eq!(encoded.len(), 13, "1 tag + 8 seq + 4 len");
        let decoded = CameraRequest::decode(&encoded).expect("decode PushFrame");
        assert_eq!(decoded, req);
    }

    #[test]
    fn camera_reply_format_roundtrip() {
        use crate::usb::uvc::camera_ipc::CameraReply;
        let reply = CameraReply::Format {
            width: 1280,
            height: 720,
            fmt: *b"YUY2",
        };
        let encoded = reply.encode();
        assert_eq!(encoded.len(), 9, "1 tag + 2 w + 2 h + 4 fmt");
        let decoded = CameraReply::decode(&encoded).expect("decode Format");
        assert_eq!(decoded, reply);
    }

    #[test]
    fn camera_reply_frame_roundtrip() {
        use crate::usb::uvc::camera_ipc::CameraReply;
        let reply = CameraReply::Frame {
            seq: 42,
            len: 921_600,
        };
        let encoded = reply.encode();
        let decoded = CameraReply::decode(&encoded).expect("decode Frame");
        assert_eq!(decoded, reply);
    }

    #[test]
    fn camera_reply_no_frame_and_ack_roundtrip() {
        use crate::usb::uvc::camera_ipc::CameraReply;
        let no_frame = CameraReply::NoFrame;
        assert_eq!(
            CameraReply::decode(&no_frame.encode()),
            Some(CameraReply::NoFrame)
        );
        let ack = CameraReply::Ack;
        assert_eq!(CameraReply::decode(&ack.encode()), Some(CameraReply::Ack));
    }

    #[test]
    fn camera_codec_rejects_empty_and_unknown_tag() {
        use crate::usb::uvc::camera_ipc::{CameraReply, CameraRequest};
        assert_eq!(CameraRequest::decode(&[]), None);
        assert_eq!(CameraReply::decode(&[]), None);
        assert_eq!(CameraRequest::decode(&[0xFF]), None);
        assert_eq!(CameraReply::decode(&[0xFF]), None);
    }
}
