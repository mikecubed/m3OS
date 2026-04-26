//! Phase 56 client-protocol and control-socket wire format.
//!
//! This module is the single declaration site for every Phase 56 protocol
//! message, opcode, and binary layout. `display_server`, every graphical
//! client library, and the `m3ctl` control-socket client all import from
//! here — searching for a message-type name must return exactly one hit.
//!
//! ## Framing
//!
//! All four message families (client → server, server → client, control
//! command, control event) share a 4-byte frame header:
//!
//! ```text
//! [body_len: u16 LE] [opcode: u16 LE] [body: body_len bytes]
//! ```
//!
//! `body_len` does not include the header itself. Frames larger than
//! [`MAX_FRAME_BODY_LEN`] are rejected as [`ProtocolError::BodyTooLarge`]
//! to bound decoder memory on adversarial input. The decoder never
//! allocates on client / server message paths; control-event decoders
//! allocate bounded `Vec`s for list payloads (`SurfaceListReply`,
//! `FrameStatsReply`), which are not on the pixel-adjacent hot path.
//!
//! ## Per-variant wire layouts
//!
//! Each variant's body is fixed-shape and documented inline on its encoder.
//! Multi-byte scalars are little-endian. Enum discriminants on the wire
//! are explicit `u8` tags; unknown tags decode to a typed `ProtocolError`
//! rather than panicking.

use alloc::vec::Vec;

use crate::input::events::{
    EventCodecError, KEY_EVENT_WIRE_SIZE, KeyEvent, POINTER_EVENT_WIRE_SIZE, PointerEvent,
};

// ---------------------------------------------------------------------------
// Protocol version and framing constants
// ---------------------------------------------------------------------------

/// Current protocol version negotiated in [`ClientMessage::Hello`] and
/// echoed back in [`ServerMessage::Welcome`]. Any other value closes the
/// connection with [`DisconnectReason::VersionMismatch`].
pub const PROTOCOL_VERSION: u32 = 1;

/// Fixed frame-header size: `body_len (u16) + opcode (u16)`.
pub const FRAME_HEADER_SIZE: usize = 4;

/// Maximum permitted body length on any frame. 4 KiB comfortably covers
/// every fixed-size message plus the bounded list payloads; adversarial
/// inputs claiming a larger body fail fast with
/// [`ProtocolError::BodyTooLarge`].
pub const MAX_FRAME_BODY_LEN: u16 = 4096;

/// Hard upper bound on `SurfaceListReply` / `FrameStatsReply` entry count.
/// Decoder rejects larger counts with
/// [`ProtocolError::ListTooLong`] so a malformed control-socket peer
/// cannot coerce a large allocation.
pub const MAX_LIST_ENTRIES: u32 = 256;

// ---------------------------------------------------------------------------
// Opcodes
// ---------------------------------------------------------------------------

// Client → server (0x0000..=0x00FF)
const OP_CLIENT_HELLO: u16 = 0x0001;
const OP_CLIENT_GOODBYE: u16 = 0x0002;
const OP_CLIENT_CREATE_SURFACE: u16 = 0x0010;
const OP_CLIENT_DESTROY_SURFACE: u16 = 0x0011;
const OP_CLIENT_SET_SURFACE_ROLE: u16 = 0x0012;
const OP_CLIENT_ATTACH_BUFFER: u16 = 0x0013;
const OP_CLIENT_DAMAGE_SURFACE: u16 = 0x0014;
const OP_CLIENT_COMMIT_SURFACE: u16 = 0x0015;
const OP_CLIENT_ACK_CONFIGURE: u16 = 0x0016;

// Server → client (0x0100..=0x01FF)
const OP_SERVER_WELCOME: u16 = 0x0101;
const OP_SERVER_DISCONNECT: u16 = 0x0102;
const OP_SERVER_SURFACE_CONFIGURED: u16 = 0x0110;
const OP_SERVER_SURFACE_DESTROYED: u16 = 0x0111;
const OP_SERVER_FOCUS_IN: u16 = 0x0120;
const OP_SERVER_FOCUS_OUT: u16 = 0x0121;
const OP_SERVER_KEY_EVENT: u16 = 0x0130;
const OP_SERVER_POINTER_EVENT: u16 = 0x0131;
const OP_SERVER_BUFFER_RELEASED: u16 = 0x0140;

// Control commands (0x0200..=0x02FF)
const OP_CTL_VERSION: u16 = 0x0201;
const OP_CTL_LIST_SURFACES: u16 = 0x0202;
const OP_CTL_FOCUS: u16 = 0x0203;
const OP_CTL_REGISTER_BIND: u16 = 0x0204;
const OP_CTL_UNREGISTER_BIND: u16 = 0x0205;
const OP_CTL_SUBSCRIBE: u16 = 0x0206;
const OP_CTL_FRAME_STATS: u16 = 0x0207;

// Control events (0x0300..=0x03FF)
const OP_CTL_EVT_VERSION_REPLY: u16 = 0x0301;
const OP_CTL_EVT_SURFACE_LIST_REPLY: u16 = 0x0302;
const OP_CTL_EVT_ACK: u16 = 0x0303;
const OP_CTL_EVT_ERROR: u16 = 0x0304;
const OP_CTL_EVT_FRAME_STATS_REPLY: u16 = 0x0305;
const OP_CTL_EVT_SURFACE_CREATED: u16 = 0x0310;
const OP_CTL_EVT_SURFACE_DESTROYED: u16 = 0x0311;
const OP_CTL_EVT_FOCUS_CHANGED: u16 = 0x0312;
const OP_CTL_EVT_BIND_TRIGGERED: u16 = 0x0313;

// ---------------------------------------------------------------------------
// Value types
// ---------------------------------------------------------------------------

/// Stable identifier for a compositor-tracked surface. 32-bit integer
/// minted by the server on `CreateSurface` and cleared on destroy.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, PartialOrd, Ord, Hash)]
pub struct SurfaceId(pub u32);

/// Stable identifier for a client-provided shared-memory buffer (Phase 50
/// page grant). Present in `AttachBuffer` / `BufferReleased`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, PartialOrd, Ord, Hash)]
pub struct BufferId(pub u32);

/// Rectangle in surface-local or output coordinates. `w` / `h` are
/// unsigned and may be zero.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Anchor-edge flag: top. Combined bitwise in [`LayerConfig::anchor_mask`].
pub const ANCHOR_TOP: u8 = 1 << 0;
/// Anchor-edge flag: bottom.
pub const ANCHOR_BOTTOM: u8 = 1 << 1;
/// Anchor-edge flag: left.
pub const ANCHOR_LEFT: u8 = 1 << 2;
/// Anchor-edge flag: right.
pub const ANCHOR_RIGHT: u8 = 1 << 3;
/// Anchor-edge flag: centered (no edge anchoring). Mutually exclusive with
/// the four edge flags; the decoder rejects mixed usage.
pub const ANCHOR_CENTER: u8 = 1 << 4;

/// Union of the four edge-anchor bits (top, bottom, left, right), excluding
/// [`ANCHOR_CENTER`]. Any bit in [`ANCHOR_CENTER`] combined with any bit in
/// `ANCHOR_EDGES` on the wire is a protocol violation — see the mutual-
/// exclusivity rule on [`ANCHOR_CENTER`].
pub const ANCHOR_EDGES: u8 = ANCHOR_TOP | ANCHOR_BOTTOM | ANCHOR_LEFT | ANCHOR_RIGHT;

/// Union of all defined anchor bits.
pub const ANCHOR_ALL: u8 = ANCHOR_EDGES | ANCHOR_CENTER;

/// True iff `mask` is a legal [`LayerConfig::anchor_mask`]: contains no bits
/// outside [`ANCHOR_ALL`] and does not mix [`ANCHOR_CENTER`] with any edge
/// anchor.
pub const fn is_valid_anchor_mask(mask: u8) -> bool {
    if (mask & !ANCHOR_ALL) != 0 {
        return false;
    }
    !((mask & ANCHOR_CENTER) != 0 && (mask & ANCHOR_EDGES) != 0)
}

/// Layer-stacking ordering for `Layer` surfaces. Wire tag is the explicit
/// discriminant value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Layer {
    Background = 0,
    Bottom = 1,
    Top = 2,
    Overlay = 3,
}

/// Keyboard-interactivity mode of a `Layer` surface (A.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum KeyboardInteractivity {
    /// Never receives keyboard input.
    None = 0,
    /// Receives input only when the input dispatcher routes focus to it.
    OnDemand = 1,
    /// Claims exclusive keyboard focus while mapped.
    Exclusive = 2,
}

/// Configuration attached to a `SurfaceRole::Layer`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LayerConfig {
    pub layer: Layer,
    pub anchor_mask: u8,
    pub exclusive_zone: u32,
    pub keyboard_interactivity: KeyboardInteractivity,
    /// Margins in pixels: `[top, right, bottom, left]`.
    pub margin: [i32; 4],
}

/// Configuration attached to a `SurfaceRole::Cursor`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CursorConfig {
    pub hotspot_x: i32,
    pub hotspot_y: i32,
}

/// Surface role: what this surface *means* to the compositor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceRole {
    /// Normal application window.
    Toplevel,
    /// Anchored overlay — layer-shell equivalent.
    Layer(LayerConfig),
    /// Pointer image.
    Cursor(CursorConfig),
}

/// Wire-only tag variant of [`SurfaceRole`] used in control events where
/// carrying the full configuration is redundant (the consumer already
/// knows the role via `SurfaceCreated` or a later `list-surfaces` query).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum SurfaceRoleTag {
    Toplevel = 0,
    Layer = 1,
    Cursor = 2,
}

/// Named reason the server closes a connection. Carried in
/// [`ServerMessage::Disconnect`] so a client can log or react before the
/// socket closes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
#[non_exhaustive]
pub enum DisconnectReason {
    VersionMismatch = 0,
    UnknownOpcode = 1,
    MalformedFrame = 2,
    ResourceExhausted = 3,
    ServerShutdown = 4,
    ProtocolViolation = 5,
}

/// Control-socket subscribable event kinds (A.8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
#[non_exhaustive]
pub enum EventKind {
    SurfaceCreated = 0,
    SurfaceDestroyed = 1,
    FocusChanged = 2,
    BindTriggered = 3,
}

/// Control-socket error codes (A.8 / E.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
#[non_exhaustive]
pub enum ControlErrorCode {
    UnknownVerb = 0,
    MalformedFrame = 1,
    BadArgs = 2,
    UnknownSurface = 3,
    ResourceExhausted = 4,
}

/// Single frame-composition sample exposed by the observability `frame-stats`
/// control verb (Engineering Discipline → Observability).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameStatSample {
    pub frame_index: u64,
    pub compose_micros: u32,
}

// ---------------------------------------------------------------------------
// Message enums
// ---------------------------------------------------------------------------

/// Client → server message (A.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
        capabilities: u32,
    },
    Goodbye,
    CreateSurface {
        surface_id: SurfaceId,
    },
    DestroySurface {
        surface_id: SurfaceId,
    },
    SetSurfaceRole {
        surface_id: SurfaceId,
        role: SurfaceRole,
    },
    AttachBuffer {
        surface_id: SurfaceId,
        buffer_id: BufferId,
    },
    DamageSurface {
        surface_id: SurfaceId,
        rect: Rect,
    },
    CommitSurface {
        surface_id: SurfaceId,
    },
    AckConfigure {
        surface_id: SurfaceId,
        serial: u32,
    },
}

/// Server → client message (A.3 / A.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ServerMessage {
    Welcome {
        protocol_version: u32,
        capabilities: u32,
    },
    Disconnect {
        reason: DisconnectReason,
    },
    SurfaceConfigured {
        surface_id: SurfaceId,
        rect: Rect,
        serial: u32,
    },
    SurfaceDestroyed {
        surface_id: SurfaceId,
    },
    FocusIn {
        surface_id: SurfaceId,
    },
    FocusOut {
        surface_id: SurfaceId,
    },
    Key(KeyEvent),
    Pointer(PointerEvent),
    BufferReleased {
        surface_id: SurfaceId,
        buffer_id: BufferId,
    },
}

/// Control-socket request verb (A.8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ControlCommand {
    Version,
    ListSurfaces,
    Focus { surface_id: SurfaceId },
    RegisterBind { modifier_mask: u16, keycode: u32 },
    UnregisterBind { modifier_mask: u16, keycode: u32 },
    Subscribe { event_kind: EventKind },
    FrameStats,
}

/// Control-socket reply or subscribed-stream event (A.8). Reply events
/// carrying lists — `SurfaceListReply`, `FrameStatsReply` — own a bounded
/// `Vec`. The decoder caps entry counts at [`MAX_LIST_ENTRIES`] so an
/// adversarial peer cannot coerce a large allocation.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ControlEvent {
    VersionReply {
        protocol_version: u32,
    },
    SurfaceListReply {
        ids: Vec<SurfaceId>,
    },
    Ack,
    Error {
        code: ControlErrorCode,
    },
    FrameStatsReply {
        samples: Vec<FrameStatSample>,
    },
    SurfaceCreated {
        surface_id: SurfaceId,
        role: SurfaceRoleTag,
    },
    SurfaceDestroyed {
        surface_id: SurfaceId,
    },
    FocusChanged {
        focused: Option<SurfaceId>,
    },
    BindTriggered {
        modifier_mask: u16,
        keycode: u32,
    },
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors emitted by the Phase 56 protocol codec. Variants are *data* so
/// callers pattern-match and recover — no stringly-typed errors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ProtocolError {
    /// Input buffer smaller than required or output buffer too small.
    Truncated,
    /// Declared body length exceeds [`MAX_FRAME_BODY_LEN`].
    BodyTooLarge,
    /// Unknown opcode on the wire. Carries the offending value so the
    /// caller can log it before closing the connection.
    UnknownOpcode(u16),
    /// Body length does not match the expected size for the opcode.
    BodyLengthMismatch,
    /// Enum discriminant not recognized (role tag, disconnect reason,
    /// event kind, control error code, etc.).
    InvalidEnum,
    /// Layer-config anchor bitmask contained undefined bits.
    InvalidAnchorMask,
    /// `SurfaceListReply` / `FrameStatsReply` entry count exceeds
    /// [`MAX_LIST_ENTRIES`].
    ListTooLong,
    /// Error propagated from the shared [`input::events`](crate::input::events)
    /// codec while encoding or decoding a wrapped event.
    Event(EventCodecError),
}

impl From<EventCodecError> for ProtocolError {
    fn from(err: EventCodecError) -> Self {
        ProtocolError::Event(err)
    }
}

// ---------------------------------------------------------------------------
// Framing + primitive read/write helpers
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
    let body = &buf[FRAME_HEADER_SIZE..total];
    Ok((body_len, opcode, body, total))
}

fn read_u16(buf: &[u8], offset: usize) -> Result<u16, ProtocolError> {
    let s = buf
        .get(offset..offset + 2)
        .ok_or(ProtocolError::Truncated)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32(buf: &[u8], offset: usize) -> Result<u32, ProtocolError> {
    let s = buf
        .get(offset..offset + 4)
        .ok_or(ProtocolError::Truncated)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_i32(buf: &[u8], offset: usize) -> Result<i32, ProtocolError> {
    let s = buf
        .get(offset..offset + 4)
        .ok_or(ProtocolError::Truncated)?;
    Ok(i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn read_u64(buf: &[u8], offset: usize) -> Result<u64, ProtocolError> {
    let s = buf
        .get(offset..offset + 8)
        .ok_or(ProtocolError::Truncated)?;
    Ok(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

fn write_rect(buf: &mut [u8], rect: Rect) {
    buf[0..4].copy_from_slice(&rect.x.to_le_bytes());
    buf[4..8].copy_from_slice(&rect.y.to_le_bytes());
    buf[8..12].copy_from_slice(&rect.w.to_le_bytes());
    buf[12..16].copy_from_slice(&rect.h.to_le_bytes());
}

fn read_rect(buf: &[u8], offset: usize) -> Result<Rect, ProtocolError> {
    Ok(Rect {
        x: read_i32(buf, offset)?,
        y: read_i32(buf, offset + 4)?,
        w: read_u32(buf, offset + 8)?,
        h: read_u32(buf, offset + 12)?,
    })
}

fn layer_from_u8(v: u8) -> Result<Layer, ProtocolError> {
    match v {
        0 => Ok(Layer::Background),
        1 => Ok(Layer::Bottom),
        2 => Ok(Layer::Top),
        3 => Ok(Layer::Overlay),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

fn keyboard_interactivity_from_u8(v: u8) -> Result<KeyboardInteractivity, ProtocolError> {
    match v {
        0 => Ok(KeyboardInteractivity::None),
        1 => Ok(KeyboardInteractivity::OnDemand),
        2 => Ok(KeyboardInteractivity::Exclusive),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

fn disconnect_reason_from_u8(v: u8) -> Result<DisconnectReason, ProtocolError> {
    match v {
        0 => Ok(DisconnectReason::VersionMismatch),
        1 => Ok(DisconnectReason::UnknownOpcode),
        2 => Ok(DisconnectReason::MalformedFrame),
        3 => Ok(DisconnectReason::ResourceExhausted),
        4 => Ok(DisconnectReason::ServerShutdown),
        5 => Ok(DisconnectReason::ProtocolViolation),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

fn event_kind_from_u8(v: u8) -> Result<EventKind, ProtocolError> {
    match v {
        0 => Ok(EventKind::SurfaceCreated),
        1 => Ok(EventKind::SurfaceDestroyed),
        2 => Ok(EventKind::FocusChanged),
        3 => Ok(EventKind::BindTriggered),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

fn control_error_code_from_u8(v: u8) -> Result<ControlErrorCode, ProtocolError> {
    match v {
        0 => Ok(ControlErrorCode::UnknownVerb),
        1 => Ok(ControlErrorCode::MalformedFrame),
        2 => Ok(ControlErrorCode::BadArgs),
        3 => Ok(ControlErrorCode::UnknownSurface),
        4 => Ok(ControlErrorCode::ResourceExhausted),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

fn surface_role_tag_from_u8(v: u8) -> Result<SurfaceRoleTag, ProtocolError> {
    match v {
        0 => Ok(SurfaceRoleTag::Toplevel),
        1 => Ok(SurfaceRoleTag::Layer),
        2 => Ok(SurfaceRoleTag::Cursor),
        _ => Err(ProtocolError::InvalidEnum),
    }
}

/// Size on the wire of a surface-role payload (tag + role body), in bytes.
fn role_wire_size(role: &SurfaceRole) -> usize {
    match role {
        SurfaceRole::Toplevel => 1,
        SurfaceRole::Layer(_) => 24,
        SurfaceRole::Cursor(_) => 9,
    }
}

fn write_role(buf: &mut [u8], role: &SurfaceRole) -> usize {
    match role {
        SurfaceRole::Toplevel => {
            buf[0] = 0;
            1
        }
        SurfaceRole::Layer(cfg) => {
            buf[0] = 1;
            buf[1] = cfg.layer as u8;
            buf[2] = cfg.anchor_mask;
            buf[3..7].copy_from_slice(&cfg.exclusive_zone.to_le_bytes());
            buf[7] = cfg.keyboard_interactivity as u8;
            buf[8..12].copy_from_slice(&cfg.margin[0].to_le_bytes());
            buf[12..16].copy_from_slice(&cfg.margin[1].to_le_bytes());
            buf[16..20].copy_from_slice(&cfg.margin[2].to_le_bytes());
            buf[20..24].copy_from_slice(&cfg.margin[3].to_le_bytes());
            24
        }
        SurfaceRole::Cursor(cfg) => {
            buf[0] = 2;
            buf[1..5].copy_from_slice(&cfg.hotspot_x.to_le_bytes());
            buf[5..9].copy_from_slice(&cfg.hotspot_y.to_le_bytes());
            9
        }
    }
}

fn read_role(buf: &[u8]) -> Result<(SurfaceRole, usize), ProtocolError> {
    let tag = *buf.first().ok_or(ProtocolError::Truncated)?;
    match tag {
        0 => Ok((SurfaceRole::Toplevel, 1)),
        1 => {
            if buf.len() < 24 {
                return Err(ProtocolError::Truncated);
            }
            let layer = layer_from_u8(buf[1])?;
            let anchor_mask = buf[2];
            if !is_valid_anchor_mask(anchor_mask) {
                return Err(ProtocolError::InvalidAnchorMask);
            }
            let exclusive_zone = read_u32(buf, 3)?;
            let keyboard_interactivity = keyboard_interactivity_from_u8(buf[7])?;
            let margin = [
                read_i32(buf, 8)?,
                read_i32(buf, 12)?,
                read_i32(buf, 16)?,
                read_i32(buf, 20)?,
            ];
            Ok((
                SurfaceRole::Layer(LayerConfig {
                    layer,
                    anchor_mask,
                    exclusive_zone,
                    keyboard_interactivity,
                    margin,
                }),
                24,
            ))
        }
        2 => {
            if buf.len() < 9 {
                return Err(ProtocolError::Truncated);
            }
            let hotspot_x = read_i32(buf, 1)?;
            let hotspot_y = read_i32(buf, 5)?;
            Ok((
                SurfaceRole::Cursor(CursorConfig {
                    hotspot_x,
                    hotspot_y,
                }),
                9,
            ))
        }
        _ => Err(ProtocolError::InvalidEnum),
    }
}

fn expect_body_len(actual: u16, expected: u16) -> Result<(), ProtocolError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ProtocolError::BodyLengthMismatch)
    }
}

fn encode_fixed_body<F>(
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

// ---------------------------------------------------------------------------
// ClientMessage codec
// ---------------------------------------------------------------------------

impl ClientMessage {
    /// Encode into a caller-supplied buffer; never allocates.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::Hello {
                protocol_version,
                capabilities,
            } => encode_fixed_body(buf, OP_CLIENT_HELLO, 8, |body| {
                body[0..4].copy_from_slice(&protocol_version.to_le_bytes());
                body[4..8].copy_from_slice(&capabilities.to_le_bytes());
            }),
            Self::Goodbye => encode_fixed_body(buf, OP_CLIENT_GOODBYE, 0, |_| {}),
            Self::CreateSurface { surface_id } => {
                encode_fixed_body(buf, OP_CLIENT_CREATE_SURFACE, 4, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                })
            }
            Self::DestroySurface { surface_id } => {
                encode_fixed_body(buf, OP_CLIENT_DESTROY_SURFACE, 4, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                })
            }
            Self::SetSurfaceRole { surface_id, role } => {
                if let SurfaceRole::Layer(cfg) = role
                    && !is_valid_anchor_mask(cfg.anchor_mask)
                {
                    return Err(ProtocolError::InvalidAnchorMask);
                }
                let role_size = role_wire_size(role);
                let body_len = 4 + role_size;
                encode_fixed_body(buf, OP_CLIENT_SET_SURFACE_ROLE, body_len, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                    let _ = write_role(&mut body[4..], role);
                })
            }
            Self::AttachBuffer {
                surface_id,
                buffer_id,
            } => encode_fixed_body(buf, OP_CLIENT_ATTACH_BUFFER, 8, |body| {
                body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                body[4..8].copy_from_slice(&buffer_id.0.to_le_bytes());
            }),
            Self::DamageSurface { surface_id, rect } => {
                encode_fixed_body(buf, OP_CLIENT_DAMAGE_SURFACE, 20, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                    write_rect(&mut body[4..20], *rect);
                })
            }
            Self::CommitSurface { surface_id } => {
                encode_fixed_body(buf, OP_CLIENT_COMMIT_SURFACE, 4, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                })
            }
            Self::AckConfigure { surface_id, serial } => {
                encode_fixed_body(buf, OP_CLIENT_ACK_CONFIGURE, 8, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                    body[4..8].copy_from_slice(&serial.to_le_bytes());
                })
            }
        }
    }

    /// Decode a single [`ClientMessage`] from the start of `buf`.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, body, total) = parse_frame_header(buf)?;
        let msg = match opcode {
            OP_CLIENT_HELLO => {
                expect_body_len(body_len, 8)?;
                let protocol_version = read_u32(body, 0)?;
                let capabilities = read_u32(body, 4)?;
                Self::Hello {
                    protocol_version,
                    capabilities,
                }
            }
            OP_CLIENT_GOODBYE => {
                expect_body_len(body_len, 0)?;
                Self::Goodbye
            }
            OP_CLIENT_CREATE_SURFACE => {
                expect_body_len(body_len, 4)?;
                Self::CreateSurface {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_CLIENT_DESTROY_SURFACE => {
                expect_body_len(body_len, 4)?;
                Self::DestroySurface {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_CLIENT_SET_SURFACE_ROLE => {
                if body_len < 5 {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                let surface_id = SurfaceId(read_u32(body, 0)?);
                let (role, consumed) = read_role(&body[4..])?;
                if 4 + consumed != body_len as usize {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                Self::SetSurfaceRole { surface_id, role }
            }
            OP_CLIENT_ATTACH_BUFFER => {
                expect_body_len(body_len, 8)?;
                Self::AttachBuffer {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                    buffer_id: BufferId(read_u32(body, 4)?),
                }
            }
            OP_CLIENT_DAMAGE_SURFACE => {
                expect_body_len(body_len, 20)?;
                Self::DamageSurface {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                    rect: read_rect(body, 4)?,
                }
            }
            OP_CLIENT_COMMIT_SURFACE => {
                expect_body_len(body_len, 4)?;
                Self::CommitSurface {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_CLIENT_ACK_CONFIGURE => {
                expect_body_len(body_len, 8)?;
                Self::AckConfigure {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                    serial: read_u32(body, 4)?,
                }
            }
            _ => return Err(ProtocolError::UnknownOpcode(opcode)),
        };
        Ok((msg, total))
    }
}

// ---------------------------------------------------------------------------
// ServerMessage codec
// ---------------------------------------------------------------------------

impl ServerMessage {
    /// Encode into a caller-supplied buffer; never allocates.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::Welcome {
                protocol_version,
                capabilities,
            } => encode_fixed_body(buf, OP_SERVER_WELCOME, 8, |body| {
                body[0..4].copy_from_slice(&protocol_version.to_le_bytes());
                body[4..8].copy_from_slice(&capabilities.to_le_bytes());
            }),
            Self::Disconnect { reason } => {
                encode_fixed_body(buf, OP_SERVER_DISCONNECT, 1, |body| {
                    body[0] = *reason as u8;
                })
            }
            Self::SurfaceConfigured {
                surface_id,
                rect,
                serial,
            } => encode_fixed_body(buf, OP_SERVER_SURFACE_CONFIGURED, 24, |body| {
                body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                write_rect(&mut body[4..20], *rect);
                body[20..24].copy_from_slice(&serial.to_le_bytes());
            }),
            Self::SurfaceDestroyed { surface_id } => {
                encode_fixed_body(buf, OP_SERVER_SURFACE_DESTROYED, 4, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                })
            }
            Self::FocusIn { surface_id } => encode_fixed_body(buf, OP_SERVER_FOCUS_IN, 4, |body| {
                body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
            }),
            Self::FocusOut { surface_id } => {
                encode_fixed_body(buf, OP_SERVER_FOCUS_OUT, 4, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                })
            }
            Self::Key(ev) => {
                let body_len = KEY_EVENT_WIRE_SIZE;
                let total = FRAME_HEADER_SIZE + body_len;
                if buf.len() < total {
                    return Err(ProtocolError::Truncated);
                }
                write_frame_header(buf, body_len as u16, OP_SERVER_KEY_EVENT)?;
                ev.encode(&mut buf[FRAME_HEADER_SIZE..total])?;
                Ok(total)
            }
            Self::Pointer(ev) => {
                let body_len = POINTER_EVENT_WIRE_SIZE;
                let total = FRAME_HEADER_SIZE + body_len;
                if buf.len() < total {
                    return Err(ProtocolError::Truncated);
                }
                write_frame_header(buf, body_len as u16, OP_SERVER_POINTER_EVENT)?;
                ev.encode(&mut buf[FRAME_HEADER_SIZE..total])?;
                Ok(total)
            }
            Self::BufferReleased {
                surface_id,
                buffer_id,
            } => encode_fixed_body(buf, OP_SERVER_BUFFER_RELEASED, 8, |body| {
                body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                body[4..8].copy_from_slice(&buffer_id.0.to_le_bytes());
            }),
        }
    }

    /// Decode a single [`ServerMessage`] from the start of `buf`.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, body, total) = parse_frame_header(buf)?;
        let msg = match opcode {
            OP_SERVER_WELCOME => {
                expect_body_len(body_len, 8)?;
                Self::Welcome {
                    protocol_version: read_u32(body, 0)?,
                    capabilities: read_u32(body, 4)?,
                }
            }
            OP_SERVER_DISCONNECT => {
                expect_body_len(body_len, 1)?;
                Self::Disconnect {
                    reason: disconnect_reason_from_u8(body[0])?,
                }
            }
            OP_SERVER_SURFACE_CONFIGURED => {
                expect_body_len(body_len, 24)?;
                Self::SurfaceConfigured {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                    rect: read_rect(body, 4)?,
                    serial: read_u32(body, 20)?,
                }
            }
            OP_SERVER_SURFACE_DESTROYED => {
                expect_body_len(body_len, 4)?;
                Self::SurfaceDestroyed {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_SERVER_FOCUS_IN => {
                expect_body_len(body_len, 4)?;
                Self::FocusIn {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_SERVER_FOCUS_OUT => {
                expect_body_len(body_len, 4)?;
                Self::FocusOut {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_SERVER_KEY_EVENT => {
                expect_body_len(body_len, KEY_EVENT_WIRE_SIZE as u16)?;
                let (ev, _) = KeyEvent::decode(body)?;
                Self::Key(ev)
            }
            OP_SERVER_POINTER_EVENT => {
                expect_body_len(body_len, POINTER_EVENT_WIRE_SIZE as u16)?;
                let (ev, _) = PointerEvent::decode(body)?;
                Self::Pointer(ev)
            }
            OP_SERVER_BUFFER_RELEASED => {
                expect_body_len(body_len, 8)?;
                Self::BufferReleased {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                    buffer_id: BufferId(read_u32(body, 4)?),
                }
            }
            _ => return Err(ProtocolError::UnknownOpcode(opcode)),
        };
        Ok((msg, total))
    }
}

// ---------------------------------------------------------------------------
// ControlCommand codec
// ---------------------------------------------------------------------------

impl ControlCommand {
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::Version => encode_fixed_body(buf, OP_CTL_VERSION, 0, |_| {}),
            Self::ListSurfaces => encode_fixed_body(buf, OP_CTL_LIST_SURFACES, 0, |_| {}),
            Self::Focus { surface_id } => encode_fixed_body(buf, OP_CTL_FOCUS, 4, |body| {
                body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
            }),
            Self::RegisterBind {
                modifier_mask,
                keycode,
            } => encode_fixed_body(buf, OP_CTL_REGISTER_BIND, 6, |body| {
                body[0..2].copy_from_slice(&modifier_mask.to_le_bytes());
                body[2..6].copy_from_slice(&keycode.to_le_bytes());
            }),
            Self::UnregisterBind {
                modifier_mask,
                keycode,
            } => encode_fixed_body(buf, OP_CTL_UNREGISTER_BIND, 6, |body| {
                body[0..2].copy_from_slice(&modifier_mask.to_le_bytes());
                body[2..6].copy_from_slice(&keycode.to_le_bytes());
            }),
            Self::Subscribe { event_kind } => encode_fixed_body(buf, OP_CTL_SUBSCRIBE, 1, |body| {
                body[0] = *event_kind as u8;
            }),
            Self::FrameStats => encode_fixed_body(buf, OP_CTL_FRAME_STATS, 0, |_| {}),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, body, total) = parse_frame_header(buf)?;
        let cmd = match opcode {
            OP_CTL_VERSION => {
                expect_body_len(body_len, 0)?;
                Self::Version
            }
            OP_CTL_LIST_SURFACES => {
                expect_body_len(body_len, 0)?;
                Self::ListSurfaces
            }
            OP_CTL_FOCUS => {
                expect_body_len(body_len, 4)?;
                Self::Focus {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_CTL_REGISTER_BIND => {
                expect_body_len(body_len, 6)?;
                Self::RegisterBind {
                    modifier_mask: read_u16(body, 0)?,
                    keycode: read_u32(body, 2)?,
                }
            }
            OP_CTL_UNREGISTER_BIND => {
                expect_body_len(body_len, 6)?;
                Self::UnregisterBind {
                    modifier_mask: read_u16(body, 0)?,
                    keycode: read_u32(body, 2)?,
                }
            }
            OP_CTL_SUBSCRIBE => {
                expect_body_len(body_len, 1)?;
                Self::Subscribe {
                    event_kind: event_kind_from_u8(body[0])?,
                }
            }
            OP_CTL_FRAME_STATS => {
                expect_body_len(body_len, 0)?;
                Self::FrameStats
            }
            _ => return Err(ProtocolError::UnknownOpcode(opcode)),
        };
        Ok((cmd, total))
    }
}

// ---------------------------------------------------------------------------
// ControlEvent codec
// ---------------------------------------------------------------------------

impl ControlEvent {
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, ProtocolError> {
        match self {
            Self::VersionReply { protocol_version } => {
                encode_fixed_body(buf, OP_CTL_EVT_VERSION_REPLY, 4, |body| {
                    body[0..4].copy_from_slice(&protocol_version.to_le_bytes());
                })
            }
            Self::SurfaceListReply { ids } => {
                if ids.len() as u32 > MAX_LIST_ENTRIES {
                    return Err(ProtocolError::ListTooLong);
                }
                let body_len = 4 + ids.len() * 4;
                encode_fixed_body(buf, OP_CTL_EVT_SURFACE_LIST_REPLY, body_len, |body| {
                    body[0..4].copy_from_slice(&(ids.len() as u32).to_le_bytes());
                    for (i, id) in ids.iter().enumerate() {
                        let start = 4 + i * 4;
                        body[start..start + 4].copy_from_slice(&id.0.to_le_bytes());
                    }
                })
            }
            Self::Ack => encode_fixed_body(buf, OP_CTL_EVT_ACK, 0, |_| {}),
            Self::Error { code } => encode_fixed_body(buf, OP_CTL_EVT_ERROR, 1, |body| {
                body[0] = *code as u8;
            }),
            Self::FrameStatsReply { samples } => {
                if samples.len() as u32 > MAX_LIST_ENTRIES {
                    return Err(ProtocolError::ListTooLong);
                }
                let body_len = 4 + samples.len() * 12;
                encode_fixed_body(buf, OP_CTL_EVT_FRAME_STATS_REPLY, body_len, |body| {
                    body[0..4].copy_from_slice(&(samples.len() as u32).to_le_bytes());
                    for (i, s) in samples.iter().enumerate() {
                        let start = 4 + i * 12;
                        body[start..start + 8].copy_from_slice(&s.frame_index.to_le_bytes());
                        body[start + 8..start + 12]
                            .copy_from_slice(&s.compose_micros.to_le_bytes());
                    }
                })
            }
            Self::SurfaceCreated { surface_id, role } => {
                encode_fixed_body(buf, OP_CTL_EVT_SURFACE_CREATED, 5, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                    body[4] = *role as u8;
                })
            }
            Self::SurfaceDestroyed { surface_id } => {
                encode_fixed_body(buf, OP_CTL_EVT_SURFACE_DESTROYED, 4, |body| {
                    body[0..4].copy_from_slice(&surface_id.0.to_le_bytes());
                })
            }
            Self::FocusChanged { focused } => {
                encode_fixed_body(buf, OP_CTL_EVT_FOCUS_CHANGED, 5, |body| {
                    let (flag, id) = match focused {
                        Some(id) => (1u8, id.0),
                        None => (0u8, 0u32),
                    };
                    body[0] = flag;
                    body[1..5].copy_from_slice(&id.to_le_bytes());
                })
            }
            Self::BindTriggered {
                modifier_mask,
                keycode,
            } => encode_fixed_body(buf, OP_CTL_EVT_BIND_TRIGGERED, 6, |body| {
                body[0..2].copy_from_slice(&modifier_mask.to_le_bytes());
                body[2..6].copy_from_slice(&keycode.to_le_bytes());
            }),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        let (body_len, opcode, body, total) = parse_frame_header(buf)?;
        let evt = match opcode {
            OP_CTL_EVT_VERSION_REPLY => {
                expect_body_len(body_len, 4)?;
                Self::VersionReply {
                    protocol_version: read_u32(body, 0)?,
                }
            }
            OP_CTL_EVT_SURFACE_LIST_REPLY => {
                if body_len < 4 {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                let count = read_u32(body, 0)?;
                if count > MAX_LIST_ENTRIES {
                    return Err(ProtocolError::ListTooLong);
                }
                let expected = 4 + count as usize * 4;
                if body_len as usize != expected {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                let mut ids = Vec::with_capacity(count as usize);
                for i in 0..count as usize {
                    let start = 4 + i * 4;
                    ids.push(SurfaceId(read_u32(body, start)?));
                }
                Self::SurfaceListReply { ids }
            }
            OP_CTL_EVT_ACK => {
                expect_body_len(body_len, 0)?;
                Self::Ack
            }
            OP_CTL_EVT_ERROR => {
                expect_body_len(body_len, 1)?;
                Self::Error {
                    code: control_error_code_from_u8(body[0])?,
                }
            }
            OP_CTL_EVT_FRAME_STATS_REPLY => {
                if body_len < 4 {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                let count = read_u32(body, 0)?;
                if count > MAX_LIST_ENTRIES {
                    return Err(ProtocolError::ListTooLong);
                }
                let expected = 4 + count as usize * 12;
                if body_len as usize != expected {
                    return Err(ProtocolError::BodyLengthMismatch);
                }
                let mut samples = Vec::with_capacity(count as usize);
                for i in 0..count as usize {
                    let start = 4 + i * 12;
                    samples.push(FrameStatSample {
                        frame_index: read_u64(body, start)?,
                        compose_micros: read_u32(body, start + 8)?,
                    });
                }
                Self::FrameStatsReply { samples }
            }
            OP_CTL_EVT_SURFACE_CREATED => {
                expect_body_len(body_len, 5)?;
                Self::SurfaceCreated {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                    role: surface_role_tag_from_u8(body[4])?,
                }
            }
            OP_CTL_EVT_SURFACE_DESTROYED => {
                expect_body_len(body_len, 4)?;
                Self::SurfaceDestroyed {
                    surface_id: SurfaceId(read_u32(body, 0)?),
                }
            }
            OP_CTL_EVT_FOCUS_CHANGED => {
                expect_body_len(body_len, 5)?;
                let flag = body[0];
                let id = SurfaceId(read_u32(body, 1)?);
                let focused = match flag {
                    0 => None,
                    1 => Some(id),
                    _ => return Err(ProtocolError::InvalidEnum),
                };
                Self::FocusChanged { focused }
            }
            OP_CTL_EVT_BIND_TRIGGERED => {
                expect_body_len(body_len, 6)?;
                Self::BindTriggered {
                    modifier_mask: read_u16(body, 0)?,
                    keycode: read_u32(body, 2)?,
                }
            }
            _ => return Err(ProtocolError::UnknownOpcode(opcode)),
        };
        Ok((evt, total))
    }
}

// ---------------------------------------------------------------------------
// Tests — committed before the implementation that makes them pass.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::events::{KeyEventKind, MOD_ALT, MOD_SUPER, ModifierState, PointerButton};
    use alloc::vec;
    use proptest::prelude::*;

    const SCRATCH_BUF_LEN: usize = 512;

    fn encode_decode_round_trip_client(msg: ClientMessage) {
        let mut buf = [0u8; SCRATCH_BUF_LEN];
        let n = msg.encode(&mut buf).expect("encode");
        let (back, consumed) = ClientMessage::decode(&buf[..n]).expect("decode");
        assert_eq!(consumed, n);
        assert_eq!(back, msg);
    }

    fn encode_decode_round_trip_server(msg: ServerMessage) {
        let mut buf = [0u8; SCRATCH_BUF_LEN];
        let n = msg.encode(&mut buf).expect("encode");
        let (back, consumed) = ServerMessage::decode(&buf[..n]).expect("decode");
        assert_eq!(consumed, n);
        assert_eq!(back, msg);
    }

    fn encode_decode_round_trip_ctl_cmd(cmd: ControlCommand) {
        let mut buf = [0u8; SCRATCH_BUF_LEN];
        let n = cmd.encode(&mut buf).expect("encode");
        let (back, consumed) = ControlCommand::decode(&buf[..n]).expect("decode");
        assert_eq!(consumed, n);
        assert_eq!(back, cmd);
    }

    fn encode_decode_round_trip_ctl_evt(evt: ControlEvent) {
        let mut buf = [0u8; SCRATCH_BUF_LEN];
        let n = evt.encode(&mut buf).expect("encode");
        let (back, consumed) = ControlEvent::decode(&buf[..n]).expect("decode");
        assert_eq!(consumed, n);
        assert_eq!(back, evt);
    }

    // ---- ClientMessage round-trips, one per variant --------------------

    #[test]
    fn client_hello_round_trips() {
        encode_decode_round_trip_client(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
        });
    }

    #[test]
    fn client_goodbye_round_trips() {
        encode_decode_round_trip_client(ClientMessage::Goodbye);
    }

    #[test]
    fn client_create_surface_round_trips() {
        encode_decode_round_trip_client(ClientMessage::CreateSurface {
            surface_id: SurfaceId(1),
        });
    }

    #[test]
    fn client_destroy_surface_round_trips() {
        encode_decode_round_trip_client(ClientMessage::DestroySurface {
            surface_id: SurfaceId(7),
        });
    }

    #[test]
    fn client_set_surface_role_toplevel_round_trips() {
        encode_decode_round_trip_client(ClientMessage::SetSurfaceRole {
            surface_id: SurfaceId(3),
            role: SurfaceRole::Toplevel,
        });
    }

    #[test]
    fn client_set_surface_role_layer_round_trips() {
        let role = SurfaceRole::Layer(LayerConfig {
            layer: Layer::Top,
            anchor_mask: ANCHOR_TOP | ANCHOR_LEFT | ANCHOR_RIGHT,
            exclusive_zone: 24,
            keyboard_interactivity: KeyboardInteractivity::OnDemand,
            margin: [0, 0, 0, 0],
        });
        encode_decode_round_trip_client(ClientMessage::SetSurfaceRole {
            surface_id: SurfaceId(11),
            role,
        });
    }

    #[test]
    fn client_set_surface_role_cursor_round_trips() {
        let role = SurfaceRole::Cursor(CursorConfig {
            hotspot_x: 4,
            hotspot_y: 4,
        });
        encode_decode_round_trip_client(ClientMessage::SetSurfaceRole {
            surface_id: SurfaceId(12),
            role,
        });
    }

    #[test]
    fn client_attach_buffer_round_trips() {
        encode_decode_round_trip_client(ClientMessage::AttachBuffer {
            surface_id: SurfaceId(9),
            buffer_id: BufferId(42),
        });
    }

    #[test]
    fn client_damage_surface_round_trips() {
        encode_decode_round_trip_client(ClientMessage::DamageSurface {
            surface_id: SurfaceId(9),
            rect: Rect {
                x: -10,
                y: -20,
                w: 30,
                h: 40,
            },
        });
    }

    #[test]
    fn client_commit_surface_round_trips() {
        encode_decode_round_trip_client(ClientMessage::CommitSurface {
            surface_id: SurfaceId(9),
        });
    }

    #[test]
    fn client_ack_configure_round_trips() {
        encode_decode_round_trip_client(ClientMessage::AckConfigure {
            surface_id: SurfaceId(9),
            serial: 0xdead_beef,
        });
    }

    #[test]
    fn client_short_buffer_encode_returns_truncated() {
        let mut tiny = [0u8; FRAME_HEADER_SIZE - 1];
        let err = ClientMessage::Goodbye.encode(&mut tiny).unwrap_err();
        assert_eq!(err, ProtocolError::Truncated);
    }

    #[test]
    fn client_empty_buffer_decode_returns_truncated() {
        let err = ClientMessage::decode(&[]).unwrap_err();
        assert_eq!(err, ProtocolError::Truncated);
    }

    #[test]
    fn client_body_too_large_is_rejected() {
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        buf[0..2].copy_from_slice(&(MAX_FRAME_BODY_LEN + 1).to_le_bytes());
        buf[2..4].copy_from_slice(&OP_CLIENT_HELLO.to_le_bytes());
        let err = ClientMessage::decode(&buf).unwrap_err();
        assert_eq!(err, ProtocolError::BodyTooLarge);
    }

    #[test]
    fn client_unknown_opcode_reports_offending_value() {
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        buf[0..2].copy_from_slice(&0u16.to_le_bytes());
        buf[2..4].copy_from_slice(&0xEEEEu16.to_le_bytes());
        let err = ClientMessage::decode(&buf).unwrap_err();
        assert_eq!(err, ProtocolError::UnknownOpcode(0xEEEE));
    }

    #[test]
    fn client_set_surface_role_rejects_bad_role_tag() {
        let mut buf = [0u8; FRAME_HEADER_SIZE + 5];
        buf[0..2].copy_from_slice(&5u16.to_le_bytes());
        buf[2..4].copy_from_slice(&OP_CLIENT_SET_SURFACE_ROLE.to_le_bytes());
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        buf[8] = 9;
        let err = ClientMessage::decode(&buf).unwrap_err();
        assert_eq!(err, ProtocolError::InvalidEnum);
    }

    fn layer_role_frame_with_anchor(mask: u8) -> [u8; FRAME_HEADER_SIZE + 28] {
        let mut buf = [0u8; FRAME_HEADER_SIZE + 28];
        buf[0..2].copy_from_slice(&28u16.to_le_bytes());
        buf[2..4].copy_from_slice(&OP_CLIENT_SET_SURFACE_ROLE.to_le_bytes());
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        buf[8] = 1; // role_tag = Layer
        buf[9] = Layer::Top as u8;
        buf[10] = mask;
        buf[11..15].copy_from_slice(&0u32.to_le_bytes());
        buf[15] = KeyboardInteractivity::None as u8;
        // margins already zeroed
        buf
    }

    #[test]
    fn is_valid_anchor_mask_rejects_center_plus_edge() {
        assert!(is_valid_anchor_mask(0));
        assert!(is_valid_anchor_mask(ANCHOR_TOP));
        assert!(is_valid_anchor_mask(ANCHOR_TOP | ANCHOR_RIGHT));
        assert!(is_valid_anchor_mask(ANCHOR_CENTER));
        assert!(!is_valid_anchor_mask(ANCHOR_CENTER | ANCHOR_TOP));
        assert!(!is_valid_anchor_mask(ANCHOR_CENTER | ANCHOR_EDGES));
        assert!(!is_valid_anchor_mask(1 << 7));
    }

    #[test]
    fn client_layer_role_decode_rejects_center_plus_edge_anchor() {
        let buf = layer_role_frame_with_anchor(ANCHOR_CENTER | ANCHOR_TOP);
        assert_eq!(
            ClientMessage::decode(&buf).unwrap_err(),
            ProtocolError::InvalidAnchorMask
        );
    }

    #[test]
    fn client_layer_role_decode_rejects_undefined_anchor_bit() {
        let buf = layer_role_frame_with_anchor(1 << 7);
        assert_eq!(
            ClientMessage::decode(&buf).unwrap_err(),
            ProtocolError::InvalidAnchorMask
        );
    }

    #[test]
    fn client_layer_role_encode_rejects_center_plus_edge_anchor() {
        let msg = ClientMessage::SetSurfaceRole {
            surface_id: SurfaceId(1),
            role: SurfaceRole::Layer(LayerConfig {
                layer: Layer::Top,
                anchor_mask: ANCHOR_CENTER | ANCHOR_RIGHT,
                exclusive_zone: 0,
                keyboard_interactivity: KeyboardInteractivity::None,
                margin: [0; 4],
            }),
        };
        let mut buf = [0u8; SCRATCH_BUF_LEN];
        assert_eq!(
            msg.encode(&mut buf).unwrap_err(),
            ProtocolError::InvalidAnchorMask
        );
    }

    // ---- ServerMessage round-trips, one per variant --------------------

    #[test]
    fn server_welcome_round_trips() {
        encode_decode_round_trip_server(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
        });
    }

    #[test]
    fn server_disconnect_round_trips_for_each_reason() {
        for reason in [
            DisconnectReason::VersionMismatch,
            DisconnectReason::UnknownOpcode,
            DisconnectReason::MalformedFrame,
            DisconnectReason::ResourceExhausted,
            DisconnectReason::ServerShutdown,
            DisconnectReason::ProtocolViolation,
        ] {
            encode_decode_round_trip_server(ServerMessage::Disconnect { reason });
        }
    }

    #[test]
    fn server_surface_configured_round_trips() {
        encode_decode_round_trip_server(ServerMessage::SurfaceConfigured {
            surface_id: SurfaceId(11),
            rect: Rect {
                x: 0,
                y: 24,
                w: 1920,
                h: 1056,
            },
            serial: 1,
        });
    }

    #[test]
    fn server_surface_destroyed_round_trips() {
        encode_decode_round_trip_server(ServerMessage::SurfaceDestroyed {
            surface_id: SurfaceId(1),
        });
    }

    #[test]
    fn server_focus_in_and_out_round_trip() {
        encode_decode_round_trip_server(ServerMessage::FocusIn {
            surface_id: SurfaceId(2),
        });
        encode_decode_round_trip_server(ServerMessage::FocusOut {
            surface_id: SurfaceId(2),
        });
    }

    #[test]
    fn server_key_event_round_trips() {
        let ev = KeyEvent {
            timestamp_ms: 100,
            keycode: 0x1E,
            symbol: b'a' as u32,
            modifiers: ModifierState(MOD_SUPER | MOD_ALT),
            kind: KeyEventKind::Down,
        };
        encode_decode_round_trip_server(ServerMessage::Key(ev));
    }

    #[test]
    fn server_pointer_event_round_trips() {
        let ev = PointerEvent {
            timestamp_ms: 200,
            dx: 1,
            dy: -1,
            abs_position: Some((400, 300)),
            button: PointerButton::Down(1),
            wheel_dx: 0,
            wheel_dy: 0,
            modifiers: ModifierState::empty(),
        };
        encode_decode_round_trip_server(ServerMessage::Pointer(ev));
    }

    #[test]
    fn server_buffer_released_round_trips() {
        encode_decode_round_trip_server(ServerMessage::BufferReleased {
            surface_id: SurfaceId(5),
            buffer_id: BufferId(77),
        });
    }

    // ---- ControlCommand round-trips, one per verb ----------------------

    #[test]
    fn ctl_cmd_version_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::Version);
    }

    #[test]
    fn ctl_cmd_list_surfaces_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::ListSurfaces);
    }

    #[test]
    fn ctl_cmd_focus_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::Focus {
            surface_id: SurfaceId(3),
        });
    }

    #[test]
    fn ctl_cmd_register_bind_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::RegisterBind {
            modifier_mask: MOD_SUPER,
            keycode: 0x10,
        });
    }

    #[test]
    fn ctl_cmd_unregister_bind_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::UnregisterBind {
            modifier_mask: MOD_SUPER,
            keycode: 0x10,
        });
    }

    #[test]
    fn ctl_cmd_subscribe_round_trips() {
        for kind in [
            EventKind::SurfaceCreated,
            EventKind::SurfaceDestroyed,
            EventKind::FocusChanged,
            EventKind::BindTriggered,
        ] {
            encode_decode_round_trip_ctl_cmd(ControlCommand::Subscribe { event_kind: kind });
        }
    }

    #[test]
    fn ctl_cmd_frame_stats_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::FrameStats);
    }

    // Phase 56 Track F.2 — debug-only crash verb. Round-trip must
    // succeed at the codec layer regardless of build flavor; the
    // dispatcher (display_server::control) is the seam that decides
    // whether to honor the verb (gated by env var) or short-circuit
    // it back to `ControlError::UnknownVerb`. Keeping the codec
    // unconditional avoids a parallel "debug" verb namespace and
    // matches the F.2 spec.
    #[test]
    fn ctl_cmd_debug_crash_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::DebugCrash);
    }

    #[test]
    fn ctl_cmd_debug_crash_uses_zero_body() {
        // F.2 wire-format guard: the verb carries no payload, so the
        // body_len on the wire must be exactly zero. Encoding into a
        // buffer that is exactly FRAME_HEADER_SIZE bytes long must
        // succeed.
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        let n = ControlCommand::DebugCrash
            .encode(&mut buf)
            .expect("encode debug-crash into header-only buffer");
        assert_eq!(n, FRAME_HEADER_SIZE);
        let body_len = u16::from_le_bytes([buf[0], buf[1]]);
        assert_eq!(body_len, 0);
    }

    // ---- ControlEvent round-trips, one per variant ---------------------

    #[test]
    fn ctl_evt_version_reply_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::VersionReply {
            protocol_version: PROTOCOL_VERSION,
        });
    }

    #[test]
    fn ctl_evt_surface_list_reply_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::SurfaceListReply {
            ids: vec![SurfaceId(1), SurfaceId(2), SurfaceId(3)],
        });
    }

    #[test]
    fn ctl_evt_ack_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::Ack);
    }

    #[test]
    fn ctl_evt_error_round_trips_for_each_code() {
        for code in [
            ControlErrorCode::UnknownVerb,
            ControlErrorCode::MalformedFrame,
            ControlErrorCode::BadArgs,
            ControlErrorCode::UnknownSurface,
            ControlErrorCode::ResourceExhausted,
        ] {
            encode_decode_round_trip_ctl_evt(ControlEvent::Error { code });
        }
    }

    #[test]
    fn ctl_evt_frame_stats_reply_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::FrameStatsReply {
            samples: vec![
                FrameStatSample {
                    frame_index: 1,
                    compose_micros: 120,
                },
                FrameStatSample {
                    frame_index: 2,
                    compose_micros: 130,
                },
            ],
        });
    }

    #[test]
    fn ctl_evt_surface_created_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::SurfaceCreated {
            surface_id: SurfaceId(8),
            role: SurfaceRoleTag::Toplevel,
        });
    }

    #[test]
    fn ctl_evt_surface_destroyed_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::SurfaceDestroyed {
            surface_id: SurfaceId(8),
        });
    }

    #[test]
    fn ctl_evt_focus_changed_round_trips_both_states() {
        encode_decode_round_trip_ctl_evt(ControlEvent::FocusChanged {
            focused: Some(SurfaceId(5)),
        });
        encode_decode_round_trip_ctl_evt(ControlEvent::FocusChanged { focused: None });
    }

    #[test]
    fn ctl_evt_bind_triggered_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::BindTriggered {
            modifier_mask: MOD_SUPER,
            keycode: 0x10,
        });
    }

    #[test]
    fn ctl_evt_surface_list_reply_caps_entry_count() {
        let mut buf = [0u8; FRAME_HEADER_SIZE + 4];
        let body_len = 4u16;
        buf[0..2].copy_from_slice(&body_len.to_le_bytes());
        buf[2..4].copy_from_slice(&OP_CTL_EVT_SURFACE_LIST_REPLY.to_le_bytes());
        let oversized = MAX_LIST_ENTRIES + 1;
        buf[4..8].copy_from_slice(&oversized.to_le_bytes());
        let err = ControlEvent::decode(&buf).unwrap_err();
        assert_eq!(err, ProtocolError::ListTooLong);
    }

    // ---- Property tests ------------------------------------------------

    fn arb_surface_id() -> impl Strategy<Value = SurfaceId> {
        any::<u32>().prop_map(SurfaceId)
    }

    fn arb_buffer_id() -> impl Strategy<Value = BufferId> {
        any::<u32>().prop_map(BufferId)
    }

    fn arb_rect() -> impl Strategy<Value = Rect> {
        (any::<i32>(), any::<i32>(), any::<u32>(), any::<u32>()).prop_map(|(x, y, w, h)| Rect {
            x,
            y,
            w,
            h,
        })
    }

    fn arb_layer() -> impl Strategy<Value = Layer> {
        prop_oneof![
            Just(Layer::Background),
            Just(Layer::Bottom),
            Just(Layer::Top),
            Just(Layer::Overlay),
        ]
    }

    fn arb_keyboard_interactivity() -> impl Strategy<Value = KeyboardInteractivity> {
        prop_oneof![
            Just(KeyboardInteractivity::None),
            Just(KeyboardInteractivity::OnDemand),
            Just(KeyboardInteractivity::Exclusive),
        ]
    }

    fn arb_anchor_mask() -> impl Strategy<Value = u8> {
        prop_oneof![
            // Any combination of edge anchors (including the empty mask).
            0u8..=ANCHOR_EDGES,
            // `CENTER` on its own — the only legal way CENTER appears.
            Just(ANCHOR_CENTER),
        ]
    }

    fn arb_layer_config() -> impl Strategy<Value = LayerConfig> {
        (
            arb_layer(),
            arb_anchor_mask(),
            any::<u32>(),
            arb_keyboard_interactivity(),
            any::<[i32; 4]>(),
        )
            .prop_map(
                |(layer, anchor_mask, exclusive_zone, keyboard_interactivity, margin)| {
                    LayerConfig {
                        layer,
                        anchor_mask,
                        exclusive_zone,
                        keyboard_interactivity,
                        margin,
                    }
                },
            )
    }

    fn arb_cursor_config() -> impl Strategy<Value = CursorConfig> {
        (any::<i32>(), any::<i32>()).prop_map(|(hotspot_x, hotspot_y)| CursorConfig {
            hotspot_x,
            hotspot_y,
        })
    }

    fn arb_surface_role() -> impl Strategy<Value = SurfaceRole> {
        prop_oneof![
            Just(SurfaceRole::Toplevel),
            arb_layer_config().prop_map(SurfaceRole::Layer),
            arb_cursor_config().prop_map(SurfaceRole::Cursor),
        ]
    }

    fn arb_surface_role_tag() -> impl Strategy<Value = SurfaceRoleTag> {
        prop_oneof![
            Just(SurfaceRoleTag::Toplevel),
            Just(SurfaceRoleTag::Layer),
            Just(SurfaceRoleTag::Cursor),
        ]
    }

    fn arb_disconnect_reason() -> impl Strategy<Value = DisconnectReason> {
        prop_oneof![
            Just(DisconnectReason::VersionMismatch),
            Just(DisconnectReason::UnknownOpcode),
            Just(DisconnectReason::MalformedFrame),
            Just(DisconnectReason::ResourceExhausted),
            Just(DisconnectReason::ServerShutdown),
            Just(DisconnectReason::ProtocolViolation),
        ]
    }

    fn arb_event_kind() -> impl Strategy<Value = EventKind> {
        prop_oneof![
            Just(EventKind::SurfaceCreated),
            Just(EventKind::SurfaceDestroyed),
            Just(EventKind::FocusChanged),
            Just(EventKind::BindTriggered),
        ]
    }

    fn arb_control_error_code() -> impl Strategy<Value = ControlErrorCode> {
        prop_oneof![
            Just(ControlErrorCode::UnknownVerb),
            Just(ControlErrorCode::MalformedFrame),
            Just(ControlErrorCode::BadArgs),
            Just(ControlErrorCode::UnknownSurface),
            Just(ControlErrorCode::ResourceExhausted),
        ]
    }

    fn arb_client_message() -> impl Strategy<Value = ClientMessage> {
        prop_oneof![
            (any::<u32>(), any::<u32>()).prop_map(|(protocol_version, capabilities)| {
                ClientMessage::Hello {
                    protocol_version,
                    capabilities,
                }
            }),
            Just(ClientMessage::Goodbye),
            arb_surface_id().prop_map(|surface_id| ClientMessage::CreateSurface { surface_id }),
            arb_surface_id().prop_map(|surface_id| ClientMessage::DestroySurface { surface_id }),
            (arb_surface_id(), arb_surface_role()).prop_map(|(surface_id, role)| {
                ClientMessage::SetSurfaceRole { surface_id, role }
            }),
            (arb_surface_id(), arb_buffer_id()).prop_map(|(surface_id, buffer_id)| {
                ClientMessage::AttachBuffer {
                    surface_id,
                    buffer_id,
                }
            }),
            (arb_surface_id(), arb_rect()).prop_map(|(surface_id, rect)| {
                ClientMessage::DamageSurface { surface_id, rect }
            }),
            arb_surface_id().prop_map(|surface_id| ClientMessage::CommitSurface { surface_id }),
            (arb_surface_id(), any::<u32>()).prop_map(|(surface_id, serial)| {
                ClientMessage::AckConfigure { surface_id, serial }
            }),
        ]
    }

    fn arb_modifier_bits() -> impl Strategy<Value = u16> {
        use crate::input::events::MOD_ALL;
        (0u16..=MOD_ALL).prop_map(|bits| bits & MOD_ALL)
    }

    fn arb_key_event_proto() -> impl Strategy<Value = KeyEvent> {
        (
            any::<u64>(),
            any::<u32>(),
            any::<u32>(),
            arb_modifier_bits(),
            prop_oneof![
                Just(KeyEventKind::Down),
                Just(KeyEventKind::Up),
                Just(KeyEventKind::Repeat),
            ],
        )
            .prop_map(|(timestamp_ms, keycode, symbol, mods, kind)| KeyEvent {
                timestamp_ms,
                keycode,
                symbol,
                modifiers: ModifierState(mods),
                kind,
            })
    }

    fn arb_pointer_event_proto() -> impl Strategy<Value = PointerEvent> {
        (
            any::<u64>(),
            any::<i32>(),
            any::<i32>(),
            prop::option::of((any::<i32>(), any::<i32>())),
            prop_oneof![
                Just(PointerButton::None),
                any::<u8>().prop_map(PointerButton::Down),
                any::<u8>().prop_map(PointerButton::Up),
            ],
            any::<i32>(),
            any::<i32>(),
            arb_modifier_bits(),
        )
            .prop_map(
                |(timestamp_ms, dx, dy, abs_position, button, wheel_dx, wheel_dy, mods)| {
                    PointerEvent {
                        timestamp_ms,
                        dx,
                        dy,
                        abs_position,
                        button,
                        wheel_dx,
                        wheel_dy,
                        modifiers: ModifierState(mods),
                    }
                },
            )
    }

    fn arb_server_message() -> impl Strategy<Value = ServerMessage> {
        prop_oneof![
            (any::<u32>(), any::<u32>()).prop_map(|(protocol_version, capabilities)| {
                ServerMessage::Welcome {
                    protocol_version,
                    capabilities,
                }
            }),
            arb_disconnect_reason().prop_map(|reason| ServerMessage::Disconnect { reason }),
            (arb_surface_id(), arb_rect(), any::<u32>()).prop_map(|(surface_id, rect, serial)| {
                ServerMessage::SurfaceConfigured {
                    surface_id,
                    rect,
                    serial,
                }
            }),
            arb_surface_id().prop_map(|surface_id| ServerMessage::SurfaceDestroyed { surface_id }),
            arb_surface_id().prop_map(|surface_id| ServerMessage::FocusIn { surface_id }),
            arb_surface_id().prop_map(|surface_id| ServerMessage::FocusOut { surface_id }),
            arb_key_event_proto().prop_map(ServerMessage::Key),
            arb_pointer_event_proto().prop_map(ServerMessage::Pointer),
            (arb_surface_id(), arb_buffer_id()).prop_map(|(surface_id, buffer_id)| {
                ServerMessage::BufferReleased {
                    surface_id,
                    buffer_id,
                }
            }),
        ]
    }

    fn arb_control_command() -> impl Strategy<Value = ControlCommand> {
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
            Just(ControlCommand::DebugCrash),
        ]
    }

    fn arb_frame_sample() -> impl Strategy<Value = FrameStatSample> {
        (any::<u64>(), any::<u32>()).prop_map(|(frame_index, compose_micros)| FrameStatSample {
            frame_index,
            compose_micros,
        })
    }

    fn arb_control_event() -> impl Strategy<Value = ControlEvent> {
        prop_oneof![
            any::<u32>()
                .prop_map(|protocol_version| ControlEvent::VersionReply { protocol_version }),
            prop::collection::vec(arb_surface_id(), 0..16)
                .prop_map(|ids| ControlEvent::SurfaceListReply { ids }),
            Just(ControlEvent::Ack),
            arb_control_error_code().prop_map(|code| ControlEvent::Error { code }),
            prop::collection::vec(arb_frame_sample(), 0..16)
                .prop_map(|samples| ControlEvent::FrameStatsReply { samples }),
            (arb_surface_id(), arb_surface_role_tag()).prop_map(|(surface_id, role)| {
                ControlEvent::SurfaceCreated { surface_id, role }
            }),
            arb_surface_id().prop_map(|surface_id| ControlEvent::SurfaceDestroyed { surface_id }),
            prop::option::of(arb_surface_id())
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
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn prop_client_message_round_trips(msg in arb_client_message()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = msg.encode(&mut buf).expect("encode");
            let (back, consumed) = ClientMessage::decode(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, msg);
        }

        #[test]
        fn prop_server_message_round_trips(msg in arb_server_message()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = msg.encode(&mut buf).expect("encode");
            let (back, consumed) = ServerMessage::decode(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, msg);
        }

        #[test]
        fn prop_control_command_round_trips(cmd in arb_control_command()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = cmd.encode(&mut buf).expect("encode");
            let (back, consumed) = ControlCommand::decode(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, cmd);
        }

        #[test]
        fn prop_control_event_round_trips(evt in arb_control_event()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = evt.encode(&mut buf).expect("encode");
            let (back, consumed) = ControlEvent::decode(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, evt);
        }

        #[test]
        fn prop_client_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4200)) {
            let _ = ClientMessage::decode(&bytes);
        }

        #[test]
        fn prop_server_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4200)) {
            let _ = ServerMessage::decode(&bytes);
        }

        #[test]
        fn prop_control_command_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4200)) {
            let _ = ControlCommand::decode(&bytes);
        }

        #[test]
        fn prop_control_event_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4200)) {
            let _ = ControlEvent::decode(&bytes);
        }
    }
}
