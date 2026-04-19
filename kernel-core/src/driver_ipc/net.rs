//! Net-driver IPC protocol schema — Phase 55b Track A.3 (red commit).
//!
//! This is the failing-test commit. Stubs below compile but produce wrong
//! answers; the matching green commit replaces them with the real encoder /
//! decoder implementation.

#![allow(dead_code, unused_variables)]

use alloc::vec::Vec;

/// Message label for a driver-process-initiated frame send.
pub const NET_SEND_FRAME: u16 = 0x5511;

/// Message label for the RX notification a driver sends the kernel net stack.
pub const NET_RX_FRAME: u16 = 0x5512;

/// Message label for a link-state change event.
pub const NET_LINK_STATE: u16 = 0x5513;

/// Maximum permitted Ethernet frame length (in bytes) carried on the driver
/// IPC seam — a standard 1518-byte Ethernet frame plus a 4-byte VLAN tag.
pub const MAX_FRAME_BYTES: u16 = 1522;

/// Serialized size of a [`NetFrameHeader`] in bytes.
pub const NET_FRAME_HEADER_SIZE: usize = 8;

/// Serialized size of a [`NetLinkEvent`] payload (the kind label prefix is
/// encoded alongside it by [`encode_net_link_event`], this constant covers the
/// body only).
pub const NET_LINK_EVENT_BODY_SIZE: usize = 11;

/// Header that precedes an Ethernet frame payload on the driver IPC seam.
///
/// The payload itself travels as a bulk-memory grant, not in-line in the IPC
/// register payload — the header carries just enough metadata for the kernel
/// net stack (or the driver, for RX) to validate and dispatch the frame.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NetFrameHeader {
    pub kind: u16,
    pub frame_len: u16,
    pub flags: u32,
}

/// Link-state event emitted by the driver when the PHY transitions up / down
/// or renegotiates speed.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NetLinkEvent {
    pub up: bool,
    pub mac: [u8; 6],
    pub speed_mbps: u32,
}

/// Error kinds emitted by the net-driver IPC path.
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

// ---------------------------------------------------------------------------
// Red stubs — deliberately wrong so tests fail until the green commit lands.
// ---------------------------------------------------------------------------

pub fn encode_net_send(_header: NetFrameHeader) -> Vec<u8> {
    Vec::new()
}

pub fn decode_net_send(_bytes: &[u8]) -> Result<NetFrameHeader, NetDriverError> {
    Err(NetDriverError::DeviceAbsent)
}

pub fn encode_net_rx_notify(_header: NetFrameHeader) -> Vec<u8> {
    Vec::new()
}

pub fn decode_net_rx_notify(_bytes: &[u8]) -> Result<NetFrameHeader, NetDriverError> {
    Err(NetDriverError::DeviceAbsent)
}

pub fn encode_net_link_event(_event: NetLinkEvent) -> Vec<u8> {
    Vec::new()
}

pub fn decode_net_link_event(_bytes: &[u8]) -> Result<NetLinkEvent, NetDriverError> {
    Err(NetDriverError::DeviceAbsent)
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
            prop_oneof![Just(0u32), Just(10u32), Just(100u32), Just(1000u32), Just(10_000u32)],
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
