//! Net-driver IPC protocol schema — Phase 55b Track A.3.
//!
//! The schema is the single source of truth for the IPC seam between the
//! kernel net stack (`RemoteNic` facade, Track E.4) and the userspace e1000
//! driver (Track E). The driver forwards `send_frame` requests over the
//! `NET_SEND_FRAME` label, received frames flow back to the kernel through
//! `NET_RX_FRAME` notifications, and PHY link transitions propagate as
//! typed `NET_LINK_STATE` events so TCP retransmit logic can react to a
//! link-down without polling.
//!
//! Ethernet-frame payloads themselves ride a bulk-memory grant, not the IPC
//! register payload — only the [`NetFrameHeader`] (8 bytes) and the
//! [`NetLinkEvent`] body (11 bytes, excluding the 2-byte kind prefix) need
//! a stable wire encoding, which is what this module provides.
//!
//! The module is `no_std` + `alloc`-only; every decoder returns a typed
//! [`NetDriverError`] rather than panicking on malformed input.

use alloc::vec;
use alloc::vec::Vec;

/// Message label for a driver-process-initiated frame send.
pub const NET_SEND_FRAME: u16 = 0x5511;

/// Message label for the RX notification the driver sends the kernel net
/// stack once a received frame has been staged into a bulk-memory grant.
pub const NET_RX_FRAME: u16 = 0x5512;

/// Message label for a link-state change event.
pub const NET_LINK_STATE: u16 = 0x5513;

/// Maximum permitted Ethernet frame length (in bytes) carried on the driver
/// IPC seam — a standard 1518-byte Ethernet frame plus a 4-byte 802.1Q VLAN
/// tag. Frames longer than this bound decode to
/// [`NetDriverError::InvalidFrame`]; drivers are expected to drop oversize
/// frames at ingress rather than forward them.
pub const MAX_FRAME_BYTES: u16 = 1522;

/// Serialized size of a [`NetFrameHeader`] in bytes.
///
/// Layout (little-endian):
///
/// - `[0..2]` — `kind: u16` (label)
/// - `[2..4]` — `frame_len: u16`
/// - `[4..8]` — `flags: u32`
pub const NET_FRAME_HEADER_SIZE: usize = 8;

/// Serialized size of the [`NetLinkEvent`] **body**, i.e. everything after
/// the 2-byte kind prefix that [`encode_net_link_event`] prepends.
///
/// Breakdown: 1 byte `up` + 6 bytes `mac` + 4 bytes `speed_mbps` = 11 bytes.
pub const NET_LINK_EVENT_BODY_SIZE: usize = 11;

/// Serialized size of a full link-event payload (kind prefix + body).
pub const NET_LINK_EVENT_SIZE: usize = 2 + NET_LINK_EVENT_BODY_SIZE;

/// Header that precedes an Ethernet frame payload on the driver IPC seam.
///
/// The payload itself travels as a bulk-memory grant, not in-line in the IPC
/// register payload — the header carries just enough metadata for the kernel
/// net stack (or the driver, for RX) to validate and dispatch the frame.
///
/// `kind` must equal [`NET_SEND_FRAME`] on the TX path or [`NET_RX_FRAME`]
/// on the RX path; decoders enforce this to prevent crossed messages.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NetFrameHeader {
    pub kind: u16,
    pub frame_len: u16,
    pub flags: u32,
}

/// Link-state event emitted by the driver when the PHY transitions up / down
/// or renegotiates speed.
///
/// `mac` is included so the kernel net stack does not have to round-trip a
/// separate query on every bring-up. `speed_mbps == 0` when `up == false` is
/// not enforced in the encoding (a flapping link can legitimately report
/// zero speed during renegotiation), but the consumer is expected to treat
/// a `0` speed on a `up == true` event as a driver bug.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NetLinkEvent {
    pub up: bool,
    pub mac: [u8; 6],
    pub speed_mbps: u32,
}

/// Error kinds emitted by the net-driver IPC path.
///
/// Variants are *data* (not strings) so both the kernel-side `RemoteNic`
/// facade and the userspace driver can pattern-match on them without string
/// parsing. `Ok` is included for parity with the sentinel return value the
/// real IPC syscalls emit on success.
///
/// `#[non_exhaustive]` so follow-up tracks may add variants without forcing
/// downstream crates into an exhaustive match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum NetDriverError {
    Ok,
    LinkDown,
    RingFull,
    DeviceAbsent,
    DriverRestarting,
    InvalidFrame,
}

impl NetDriverError {
    /// Encode the error as a single discriminant byte for IPC wire transport.
    ///
    /// | Variant          | Byte |
    /// |------------------|------|
    /// | `Ok`             | 0    |
    /// | `LinkDown`       | 1    |
    /// | `RingFull`       | 2    |
    /// | `DeviceAbsent`   | 3    |
    /// | `DriverRestarting` | 4  |
    /// | `InvalidFrame`   | 5    |
    pub const fn to_byte(self) -> u8 {
        match self {
            NetDriverError::Ok => 0,
            NetDriverError::LinkDown => 1,
            NetDriverError::RingFull => 2,
            NetDriverError::DeviceAbsent => 3,
            NetDriverError::DriverRestarting => 4,
            NetDriverError::InvalidFrame => 5,
        }
    }
}

/// Map a `NetDriverError` byte to a negated POSIX errno for syscall returns.
///
/// | Error byte | Errno | Rationale |
/// |------------|-------|-----------|
/// | `Ok` (0)   | 0     | success   |
/// | `DriverRestarting` (4) | `NEG_EAGAIN` (-11) | caller should retry |
/// | `RingFull` (2)         | `NEG_EAGAIN` (-11) | caller should retry |
/// | everything else        | `NEG_EIO`   (-5)   | hard error |
///
/// Phase 55b Track F.3d-3 — mirrors `block_error_to_neg_errno` for the net path.
pub const fn net_error_to_neg_errno(error_byte: u8) -> i64 {
    match error_byte {
        0 => 0,       // Ok
        2 | 4 => -11, // RingFull | DriverRestarting → NEG_EAGAIN
        _ => -5,      // everything else → NEG_EIO
    }
}

/// Map the `Result<(), NetDriverError>` returned by `RemoteNic::send_frame`
/// to a syscall return value.
///
/// This is the pure-logic bridge used by `sys_net_send` in
/// `kernel/src/syscall/net.rs`.  Keeping the mapping in `kernel-core`
/// makes it host-testable without QEMU.
///
/// | Outcome | Return |
/// |---------|--------|
/// | `Ok(())` | 0 (success) |
/// | `Err(DriverRestarting)` | `-11` (`NEG_EAGAIN`) |
/// | `Err(RingFull)` | `-11` (`NEG_EAGAIN`) |
/// | `Err(_)` | `-5` (`NEG_EIO`) |
///
/// Phase 55c Track G.3 — single source of truth for the R1 EAGAIN surface.
pub const fn net_send_result_to_syscall_ret(result: Result<(), NetDriverError>) -> i64 {
    match result {
        Ok(()) => 0,
        Err(e) => net_error_to_neg_errno(e.to_byte()),
    }
}

/// Gate-and-route for `sys_net_send` — the full ABI/dispatch seam.
///
/// Encapsulates the two invariants that the syscall dispatch arm must enforce:
///
/// 1. **Socket capability boundary**: the caller must own an open socket fd.
///    If `has_socket` is `false` this function returns `NEG_EBADF` (-9)
///    without touching the driver path.  The arch-level dispatcher resolves
///    this flag by calling `current_fd_entry(arg0)` and checking that the
///    backend is `FdBackend::Socket`.
///
/// 2. **Driver-error → errno mapping**: delegates to
///    [`net_send_result_to_syscall_ret`], which is the single source of truth
///    for `DriverRestarting`/`RingFull` → `NEG_EAGAIN` and everything else →
///    `NEG_EIO`.
///
/// The kernel's `kernel/src/syscall/net.rs::sys_net_send` calls this after
/// the userspace buffer copy so both the socket-boundary and errno-mapping
/// invariants are exercised on the same code path that the tests cover.
///
/// | `has_socket` | `frame_result`         | Return |
/// |---|---|---|
/// | `false`      | any                    | `-9`  (`NEG_EBADF`)  |
/// | `true`       | `Ok(())`               | `0`                  |
/// | `true`       | `Err(DriverRestarting)` | `-11` (`NEG_EAGAIN`) |
/// | `true`       | `Err(RingFull)`        | `-11` (`NEG_EAGAIN`) |
/// | `true`       | `Err(_)`               | `-5`  (`NEG_EIO`)    |
///
/// Phase 55c Track G resend — tested in `kernel-core/tests/driver_restart.rs`.
pub const fn net_send_dispatch(has_socket: bool, frame_result: Result<(), NetDriverError>) -> i64 {
    if !has_socket {
        return -9; // NEG_EBADF
    }
    net_send_result_to_syscall_ret(frame_result)
}

// ---------------------------------------------------------------------------
// NetFrameHeader encoding — private helpers shared by send and rx paths.
// ---------------------------------------------------------------------------

fn encode_header_with_kind(kind: u16, header: NetFrameHeader) -> Vec<u8> {
    let mut out = vec![0u8; NET_FRAME_HEADER_SIZE];
    // Overwrite `kind` with the declared label; the caller is always the
    // authoritative side for which direction this header belongs to.
    out[0..2].copy_from_slice(&kind.to_le_bytes());
    out[2..4].copy_from_slice(&header.frame_len.to_le_bytes());
    out[4..8].copy_from_slice(&header.flags.to_le_bytes());
    out
}

fn decode_header_with_kind(
    expected_kind: u16,
    bytes: &[u8],
) -> Result<NetFrameHeader, NetDriverError> {
    if bytes.len() < NET_FRAME_HEADER_SIZE {
        return Err(NetDriverError::InvalidFrame);
    }
    let kind = u16::from_le_bytes([bytes[0], bytes[1]]);
    if kind != expected_kind {
        return Err(NetDriverError::InvalidFrame);
    }
    let frame_len = u16::from_le_bytes([bytes[2], bytes[3]]);
    if frame_len > MAX_FRAME_BYTES {
        return Err(NetDriverError::InvalidFrame);
    }
    let flags = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    Ok(NetFrameHeader {
        kind,
        frame_len,
        flags,
    })
}

// ---------------------------------------------------------------------------
// Public encode / decode — TX path.
// ---------------------------------------------------------------------------

/// Encode a [`NetFrameHeader`] for the TX (`NET_SEND_FRAME`) path.
///
/// The encoder always stamps `kind = NET_SEND_FRAME` regardless of what the
/// caller supplied, so a zeroed header is still a valid send request. The
/// matching [`decode_net_send`] enforces that the wire bytes carry that
/// label.
pub fn encode_net_send(header: NetFrameHeader) -> Vec<u8> {
    encode_header_with_kind(NET_SEND_FRAME, header)
}

/// Decode a wire-format TX header. Returns [`NetDriverError::InvalidFrame`]
/// if the payload is truncated, carries the wrong label, or declares a
/// `frame_len` above [`MAX_FRAME_BYTES`].
pub fn decode_net_send(bytes: &[u8]) -> Result<NetFrameHeader, NetDriverError> {
    decode_header_with_kind(NET_SEND_FRAME, bytes)
}

// ---------------------------------------------------------------------------
// Public encode / decode — RX path.
// ---------------------------------------------------------------------------

/// Encode a [`NetFrameHeader`] for the RX (`NET_RX_FRAME`) notification path.
pub fn encode_net_rx_notify(header: NetFrameHeader) -> Vec<u8> {
    encode_header_with_kind(NET_RX_FRAME, header)
}

/// Decode a wire-format RX header. Returns [`NetDriverError::InvalidFrame`]
/// if the payload is truncated, carries the wrong label, or declares a
/// `frame_len` above [`MAX_FRAME_BYTES`].
pub fn decode_net_rx_notify(bytes: &[u8]) -> Result<NetFrameHeader, NetDriverError> {
    decode_header_with_kind(NET_RX_FRAME, bytes)
}

// ---------------------------------------------------------------------------
// Public encode / decode — link-state path.
// ---------------------------------------------------------------------------

/// Encode a link-state event as `NET_LINK_STATE` payload.
///
/// Layout (little-endian):
///
/// - `[0..2]` — kind = [`NET_LINK_STATE`]
/// - `[2]` — `up` (0 or 1)
/// - `[3..9]` — `mac[6]`
/// - `[9..13]` — `speed_mbps: u32`
pub fn encode_net_link_event(event: NetLinkEvent) -> Vec<u8> {
    let mut out = vec![0u8; NET_LINK_EVENT_SIZE];
    out[0..2].copy_from_slice(&NET_LINK_STATE.to_le_bytes());
    out[2] = u8::from(event.up);
    out[3..9].copy_from_slice(&event.mac);
    out[9..13].copy_from_slice(&event.speed_mbps.to_le_bytes());
    out
}

/// Decode a wire-format link-state event. Returns
/// [`NetDriverError::InvalidFrame`] if the payload is truncated, carries the
/// wrong label, or the `up` byte is anything other than 0 or 1.
pub fn decode_net_link_event(bytes: &[u8]) -> Result<NetLinkEvent, NetDriverError> {
    if bytes.len() < NET_LINK_EVENT_SIZE {
        return Err(NetDriverError::InvalidFrame);
    }
    let kind = u16::from_le_bytes([bytes[0], bytes[1]]);
    if kind != NET_LINK_STATE {
        return Err(NetDriverError::InvalidFrame);
    }
    let up = match bytes[2] {
        0 => false,
        1 => true,
        _ => return Err(NetDriverError::InvalidFrame),
    };
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&bytes[3..9]);
    let speed_mbps = u32::from_le_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]);
    Ok(NetLinkEvent {
        up,
        mac,
        speed_mbps,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---- Message labels --------------------------------------------------

    #[test]
    fn message_labels_match_spec() {
        assert_eq!(NET_SEND_FRAME, 0x5511);
        assert_eq!(NET_RX_FRAME, 0x5512);
        assert_eq!(NET_LINK_STATE, 0x5513);
    }

    #[test]
    fn max_frame_bytes_covers_ethernet_plus_vlan_tag() {
        // 1518 = 14 header + 1500 payload + 4 FCS; + 4 bytes VLAN tag.
        assert_eq!(MAX_FRAME_BYTES, 1522);
    }

    // ---- NetFrameHeader round-trip --------------------------------------

    #[test]
    fn send_frame_header_round_trips() {
        let header = NetFrameHeader {
            kind: NET_SEND_FRAME,
            frame_len: 64,
            flags: 0,
        };
        let bytes = encode_net_send(header);
        let back = decode_net_send(&bytes).expect("valid encoding");
        assert_eq!(back, header);
    }

    #[test]
    fn send_frame_header_serialised_size_is_fixed() {
        let header = NetFrameHeader {
            kind: NET_SEND_FRAME,
            frame_len: 1500,
            flags: 0x0000_0001,
        };
        assert_eq!(encode_net_send(header).len(), NET_FRAME_HEADER_SIZE);
    }

    #[test]
    fn rx_notify_header_round_trips() {
        let header = NetFrameHeader {
            kind: NET_RX_FRAME,
            frame_len: 128,
            flags: 0x0000_0002,
        };
        let bytes = encode_net_rx_notify(header);
        let back = decode_net_rx_notify(&bytes).expect("valid encoding");
        assert_eq!(back, header);
    }

    // ---- Kind-gating rejects crossed messages ---------------------------

    #[test]
    fn decode_net_send_rejects_rx_kind() {
        let header = NetFrameHeader {
            kind: NET_RX_FRAME,
            frame_len: 64,
            flags: 0,
        };
        let bytes = encode_net_rx_notify(header);
        assert_eq!(
            decode_net_send(&bytes).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    #[test]
    fn decode_net_rx_notify_rejects_send_kind() {
        let header = NetFrameHeader {
            kind: NET_SEND_FRAME,
            frame_len: 64,
            flags: 0,
        };
        let bytes = encode_net_send(header);
        assert_eq!(
            decode_net_rx_notify(&bytes).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    // ---- MTU enforcement -------------------------------------------------

    #[test]
    fn send_frame_length_exactly_at_mtu_decodes() {
        let header = NetFrameHeader {
            kind: NET_SEND_FRAME,
            frame_len: MAX_FRAME_BYTES,
            flags: 0,
        };
        let bytes = encode_net_send(header);
        assert_eq!(decode_net_send(&bytes).expect("at MTU"), header);
    }

    #[test]
    fn send_frame_length_above_mtu_rejects_as_invalid_frame() {
        // Build a wire-format payload by hand so we can violate MTU despite
        // encode_net_send refusing to on the happy path.
        let mut bytes = [0u8; NET_FRAME_HEADER_SIZE];
        bytes[0..2].copy_from_slice(&NET_SEND_FRAME.to_le_bytes());
        bytes[2..4].copy_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            decode_net_send(&bytes).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    #[test]
    fn rx_notify_length_above_mtu_rejects_as_invalid_frame() {
        let mut bytes = [0u8; NET_FRAME_HEADER_SIZE];
        bytes[0..2].copy_from_slice(&NET_RX_FRAME.to_le_bytes());
        bytes[2..4].copy_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            decode_net_rx_notify(&bytes).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    // ---- Truncation ------------------------------------------------------

    #[test]
    fn decode_net_send_rejects_truncated_payload() {
        let truncated = [0u8; NET_FRAME_HEADER_SIZE - 1];
        assert_eq!(
            decode_net_send(&truncated).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    #[test]
    fn decode_net_rx_notify_rejects_truncated_payload() {
        let truncated = [0u8; 3];
        assert_eq!(
            decode_net_rx_notify(&truncated).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    // ---- Link event ------------------------------------------------------

    #[test]
    fn link_event_round_trips_up_gigabit() {
        let event = NetLinkEvent {
            up: true,
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            speed_mbps: 1000,
        };
        let bytes = encode_net_link_event(event);
        let back = decode_net_link_event(&bytes).expect("valid encoding");
        assert_eq!(back, event);
    }

    #[test]
    fn link_event_round_trips_down() {
        let event = NetLinkEvent {
            up: false,
            mac: [0; 6],
            speed_mbps: 0,
        };
        let bytes = encode_net_link_event(event);
        let back = decode_net_link_event(&bytes).expect("valid encoding");
        assert_eq!(back, event);
    }

    #[test]
    fn link_event_rejects_bad_up_byte() {
        let event = NetLinkEvent {
            up: true,
            mac: [1, 2, 3, 4, 5, 6],
            speed_mbps: 100,
        };
        let mut bytes = encode_net_link_event(event);
        // Corrupt the `up` byte (first byte after the kind prefix).
        bytes[2] = 0x42;
        assert_eq!(
            decode_net_link_event(&bytes).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    #[test]
    fn link_event_rejects_wrong_kind() {
        let event = NetLinkEvent {
            up: true,
            mac: [1, 2, 3, 4, 5, 6],
            speed_mbps: 100,
        };
        let mut bytes = encode_net_link_event(event);
        bytes[0..2].copy_from_slice(&NET_SEND_FRAME.to_le_bytes());
        assert_eq!(
            decode_net_link_event(&bytes).unwrap_err(),
            NetDriverError::InvalidFrame
        );
    }

    // ---- NetDriverError --------------------------------------------------

    #[test]
    fn net_driver_error_has_six_documented_variants() {
        let all = [
            NetDriverError::Ok,
            NetDriverError::LinkDown,
            NetDriverError::RingFull,
            NetDriverError::DeviceAbsent,
            NetDriverError::DriverRestarting,
            NetDriverError::InvalidFrame,
        ];
        // Each variant is distinct from every other variant.
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn net_driver_error_match_covers_every_arm() {
        fn tag(e: NetDriverError) -> u8 {
            match e {
                NetDriverError::Ok => 0,
                NetDriverError::LinkDown => 1,
                NetDriverError::RingFull => 2,
                NetDriverError::DeviceAbsent => 3,
                NetDriverError::DriverRestarting => 4,
                NetDriverError::InvalidFrame => 5,
            }
        }
        assert_eq!(tag(NetDriverError::Ok), 0);
        assert_eq!(tag(NetDriverError::LinkDown), 1);
        assert_eq!(tag(NetDriverError::RingFull), 2);
        assert_eq!(tag(NetDriverError::DeviceAbsent), 3);
        assert_eq!(tag(NetDriverError::DriverRestarting), 4);
        assert_eq!(tag(NetDriverError::InvalidFrame), 5);
    }

    // ---- Property tests --------------------------------------------------

    fn arb_net_frame_header_with_kind(kind: u16) -> impl Strategy<Value = NetFrameHeader> {
        (0u16..=MAX_FRAME_BYTES, any::<u32>()).prop_map(move |(frame_len, flags)| NetFrameHeader {
            kind,
            frame_len,
            flags,
        })
    }

    fn arb_net_link_event() -> impl Strategy<Value = NetLinkEvent> {
        (
            any::<bool>(),
            any::<[u8; 6]>(),
            prop_oneof![
                Just(0u32),
                Just(10u32),
                Just(100u32),
                Just(1000u32),
                Just(10_000u32)
            ],
        )
            .prop_map(|(up, mac, speed_mbps)| NetLinkEvent {
                up,
                mac,
                speed_mbps,
            })
    }

    proptest! {
        #[test]
        fn prop_send_frame_header_round_trips(
            header in arb_net_frame_header_with_kind(NET_SEND_FRAME),
        ) {
            let bytes = encode_net_send(header);
            let back = decode_net_send(&bytes).expect("valid encoding");
            prop_assert_eq!(back, header);
        }

        #[test]
        fn prop_rx_notify_header_round_trips(
            header in arb_net_frame_header_with_kind(NET_RX_FRAME),
        ) {
            let bytes = encode_net_rx_notify(header);
            let back = decode_net_rx_notify(&bytes).expect("valid encoding");
            prop_assert_eq!(back, header);
        }

        #[test]
        fn prop_link_event_round_trips(event in arb_net_link_event()) {
            let bytes = encode_net_link_event(event);
            let back = decode_net_link_event(&bytes).expect("valid encoding");
            prop_assert_eq!(back, event);
        }

        #[test]
        fn prop_send_frame_above_mtu_decodes_to_invalid_frame(
            frame_len in (MAX_FRAME_BYTES + 1)..=u16::MAX,
            flags in any::<u32>(),
        ) {
            let mut bytes = [0u8; NET_FRAME_HEADER_SIZE];
            bytes[0..2].copy_from_slice(&NET_SEND_FRAME.to_le_bytes());
            bytes[2..4].copy_from_slice(&frame_len.to_le_bytes());
            bytes[4..8].copy_from_slice(&flags.to_le_bytes());
            prop_assert_eq!(
                decode_net_send(&bytes).unwrap_err(),
                NetDriverError::InvalidFrame
            );
        }

        #[test]
        fn prop_rx_notify_above_mtu_decodes_to_invalid_frame(
            frame_len in (MAX_FRAME_BYTES + 1)..=u16::MAX,
            flags in any::<u32>(),
        ) {
            let mut bytes = [0u8; NET_FRAME_HEADER_SIZE];
            bytes[0..2].copy_from_slice(&NET_RX_FRAME.to_le_bytes());
            bytes[2..4].copy_from_slice(&frame_len.to_le_bytes());
            bytes[4..8].copy_from_slice(&flags.to_le_bytes());
            prop_assert_eq!(
                decode_net_rx_notify(&bytes).unwrap_err(),
                NetDriverError::InvalidFrame
            );
        }
    }
}
