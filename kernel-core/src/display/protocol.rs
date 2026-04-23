// Note: the opcode table and wrapped-event imports below are referenced by
// the codec implementation landing in the follow-up A.0 commit; the stub
// bodies in this commit do not yet consume them. The `allow` below is
// removed when the implementation lands.
#![allow(dead_code, unused_imports)]

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
/// [`ProtocolError::BodyTooLarge`] so a malformed control-socket peer
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

/// Union of all defined anchor bits.
pub const ANCHOR_ALL: u8 = ANCHOR_TOP | ANCHOR_BOTTOM | ANCHOR_LEFT | ANCHOR_RIGHT | ANCHOR_CENTER;

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
// Codec impls — stubbed in the test-first commit; real implementation lands
// in the follow-up commit that makes these tests pass.
// ---------------------------------------------------------------------------

impl ClientMessage {
    /// Encode into a caller-supplied buffer; never allocates.
    pub fn encode(&self, _buf: &mut [u8]) -> Result<usize, ProtocolError> {
        Err(ProtocolError::Truncated)
    }

    /// Decode a single [`ClientMessage`] from the start of `buf`.
    pub fn decode(_buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        Err(ProtocolError::Truncated)
    }
}

impl ServerMessage {
    /// Encode into a caller-supplied buffer; never allocates.
    pub fn encode(&self, _buf: &mut [u8]) -> Result<usize, ProtocolError> {
        Err(ProtocolError::Truncated)
    }

    /// Decode a single [`ServerMessage`] from the start of `buf`.
    pub fn decode(_buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        Err(ProtocolError::Truncated)
    }
}

impl ControlCommand {
    pub fn encode(&self, _buf: &mut [u8]) -> Result<usize, ProtocolError> {
        Err(ProtocolError::Truncated)
    }

    pub fn decode(_buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        Err(ProtocolError::Truncated)
    }
}

impl ControlEvent {
    pub fn encode(&self, _buf: &mut [u8]) -> Result<usize, ProtocolError> {
        Err(ProtocolError::Truncated)
    }

    pub fn decode(_buf: &[u8]) -> Result<(Self, usize), ProtocolError> {
        Err(ProtocolError::Truncated)
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_hello_round_trips() {
        encode_decode_round_trip_client(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_goodbye_round_trips() {
        encode_decode_round_trip_client(ClientMessage::Goodbye);
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_create_surface_round_trips() {
        encode_decode_round_trip_client(ClientMessage::CreateSurface {
            surface_id: SurfaceId(1),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_destroy_surface_round_trips() {
        encode_decode_round_trip_client(ClientMessage::DestroySurface {
            surface_id: SurfaceId(7),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_set_surface_role_toplevel_round_trips() {
        encode_decode_round_trip_client(ClientMessage::SetSurfaceRole {
            surface_id: SurfaceId(3),
            role: SurfaceRole::Toplevel,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_attach_buffer_round_trips() {
        encode_decode_round_trip_client(ClientMessage::AttachBuffer {
            surface_id: SurfaceId(9),
            buffer_id: BufferId(42),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_commit_surface_round_trips() {
        encode_decode_round_trip_client(ClientMessage::CommitSurface {
            surface_id: SurfaceId(9),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_ack_configure_round_trips() {
        encode_decode_round_trip_client(ClientMessage::AckConfigure {
            surface_id: SurfaceId(9),
            serial: 0xdead_beef,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_short_buffer_encode_returns_truncated() {
        let mut tiny = [0u8; FRAME_HEADER_SIZE - 1];
        let err = ClientMessage::Goodbye.encode(&mut tiny).unwrap_err();
        assert_eq!(err, ProtocolError::Truncated);
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_empty_buffer_decode_returns_truncated() {
        let err = ClientMessage::decode(&[]).unwrap_err();
        assert_eq!(err, ProtocolError::Truncated);
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_body_too_large_is_rejected() {
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        buf[0..2].copy_from_slice(&(MAX_FRAME_BODY_LEN + 1).to_le_bytes());
        buf[2..4].copy_from_slice(&OP_CLIENT_HELLO.to_le_bytes());
        let err = ClientMessage::decode(&buf).unwrap_err();
        assert_eq!(err, ProtocolError::BodyTooLarge);
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_unknown_opcode_reports_offending_value() {
        let mut buf = [0u8; FRAME_HEADER_SIZE];
        buf[0..2].copy_from_slice(&0u16.to_le_bytes());
        buf[2..4].copy_from_slice(&0xEEEEu16.to_le_bytes());
        let err = ClientMessage::decode(&buf).unwrap_err();
        assert_eq!(err, ProtocolError::UnknownOpcode(0xEEEE));
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn client_set_surface_role_rejects_bad_role_tag() {
        let mut buf = [0u8; FRAME_HEADER_SIZE + 5];
        buf[0..2].copy_from_slice(&5u16.to_le_bytes());
        buf[2..4].copy_from_slice(&OP_CLIENT_SET_SURFACE_ROLE.to_le_bytes());
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        buf[8] = 9;
        let err = ClientMessage::decode(&buf).unwrap_err();
        assert_eq!(err, ProtocolError::InvalidEnum);
    }

    // ---- ServerMessage round-trips, one per variant --------------------

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn server_welcome_round_trips() {
        encode_decode_round_trip_server(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            capabilities: 0,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn server_surface_destroyed_round_trips() {
        encode_decode_round_trip_server(ServerMessage::SurfaceDestroyed {
            surface_id: SurfaceId(1),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn server_focus_in_and_out_round_trip() {
        encode_decode_round_trip_server(ServerMessage::FocusIn {
            surface_id: SurfaceId(2),
        });
        encode_decode_round_trip_server(ServerMessage::FocusOut {
            surface_id: SurfaceId(2),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn server_buffer_released_round_trips() {
        encode_decode_round_trip_server(ServerMessage::BufferReleased {
            surface_id: SurfaceId(5),
            buffer_id: BufferId(77),
        });
    }

    // ---- ControlCommand round-trips, one per verb ----------------------

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_cmd_version_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::Version);
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_cmd_list_surfaces_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::ListSurfaces);
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_cmd_focus_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::Focus {
            surface_id: SurfaceId(3),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_cmd_register_bind_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::RegisterBind {
            modifier_mask: MOD_SUPER,
            keycode: 0x10,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_cmd_unregister_bind_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::UnregisterBind {
            modifier_mask: MOD_SUPER,
            keycode: 0x10,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_cmd_frame_stats_round_trips() {
        encode_decode_round_trip_ctl_cmd(ControlCommand::FrameStats);
    }

    // ---- ControlEvent round-trips, one per variant ---------------------

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_evt_version_reply_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::VersionReply {
            protocol_version: PROTOCOL_VERSION,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_evt_surface_list_reply_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::SurfaceListReply {
            ids: vec![SurfaceId(1), SurfaceId(2), SurfaceId(3)],
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_evt_ack_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::Ack);
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_evt_surface_created_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::SurfaceCreated {
            surface_id: SurfaceId(8),
            role: SurfaceRoleTag::Toplevel,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_evt_surface_destroyed_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::SurfaceDestroyed {
            surface_id: SurfaceId(8),
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_evt_focus_changed_round_trips_both_states() {
        encode_decode_round_trip_ctl_evt(ControlEvent::FocusChanged {
            focused: Some(SurfaceId(5)),
        });
        encode_decode_round_trip_ctl_evt(ControlEvent::FocusChanged { focused: None });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
    fn ctl_evt_bind_triggered_round_trips() {
        encode_decode_round_trip_ctl_evt(ControlEvent::BindTriggered {
            modifier_mask: MOD_SUPER,
            keycode: 0x10,
        });
    }

    #[test]
    #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
        (0u8..=ANCHOR_ALL).prop_map(|m| m & ANCHOR_ALL)
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
        #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
        fn prop_client_message_round_trips(msg in arb_client_message()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = msg.encode(&mut buf).expect("encode");
            let (back, consumed) = ClientMessage::decode(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, msg);
        }

        #[test]
        #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
        fn prop_server_message_round_trips(msg in arb_server_message()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = msg.encode(&mut buf).expect("encode");
            let (back, consumed) = ServerMessage::decode(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, msg);
        }

        #[test]
        #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
        fn prop_control_command_round_trips(cmd in arb_control_command()) {
            let mut buf = [0u8; SCRATCH_BUF_LEN];
            let n = cmd.encode(&mut buf).expect("encode");
            let (back, consumed) = ControlCommand::decode(&buf[..n]).expect("decode");
            prop_assert_eq!(consumed, n);
            prop_assert_eq!(back, cmd);
        }

        #[test]
        #[ignore = "A.0 stub: real codec impl lands in A.0 commit 2"]
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
