use alloc::vec::Vec;

use crate::types::{Ipv4Addr, MacAddr};

/// ARP hardware type: Ethernet = 1.
pub const ARP_HW_ETHERNET: u16 = 1;
/// ARP protocol type: IPv4 = 0x0800.
pub const ARP_PROTO_IPV4: u16 = 0x0800;

pub const ARP_OP_REQUEST: u16 = 1;
pub const ARP_OP_REPLY: u16 = 2;

/// Parsed ARP packet (Ethernet/IPv4 only).
#[derive(Debug, Clone)]
pub struct ArpPacket {
    pub operation: u16,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

/// Parse an ARP packet from raw payload bytes.
pub fn parse(payload: &[u8]) -> Option<ArpPacket> {
    if payload.len() < 28 {
        return None;
    }

    let hw_type = u16::from_be_bytes([payload[0], payload[1]]);
    let proto_type = u16::from_be_bytes([payload[2], payload[3]]);
    let hw_len = payload[4];
    let proto_len = payload[5];

    if hw_type != ARP_HW_ETHERNET || proto_type != ARP_PROTO_IPV4 || hw_len != 6 || proto_len != 4 {
        return None;
    }

    let operation = u16::from_be_bytes([payload[6], payload[7]]);

    let mut sender_mac = [0u8; 6];
    sender_mac.copy_from_slice(&payload[8..14]);
    let mut sender_ip = [0u8; 4];
    sender_ip.copy_from_slice(&payload[14..18]);
    let mut target_mac = [0u8; 6];
    target_mac.copy_from_slice(&payload[18..24]);
    let mut target_ip = [0u8; 4];
    target_ip.copy_from_slice(&payload[24..28]);

    Some(ArpPacket {
        operation,
        sender_mac,
        sender_ip,
        target_mac,
        target_ip,
    })
}

/// Build an ARP packet as raw bytes.
pub fn build(
    operation: u16,
    sender_mac: MacAddr,
    sender_ip: Ipv4Addr,
    target_mac: MacAddr,
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(28);
    pkt.extend_from_slice(&ARP_HW_ETHERNET.to_be_bytes());
    pkt.extend_from_slice(&ARP_PROTO_IPV4.to_be_bytes());
    pkt.push(6); // hw addr len
    pkt.push(4); // proto addr len
    pkt.extend_from_slice(&operation.to_be_bytes());
    pkt.extend_from_slice(&sender_mac);
    pkt.extend_from_slice(&sender_ip);
    pkt.extend_from_slice(&target_mac);
    pkt.extend_from_slice(&target_ip);
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        let pkt = build(
            ARP_OP_REQUEST,
            [0xAA; 6],
            [10, 0, 2, 15],
            [0; 6],
            [10, 0, 2, 1],
        );
        let parsed = parse(&pkt).unwrap();
        assert_eq!(parsed.operation, ARP_OP_REQUEST);
        assert_eq!(parsed.sender_mac, [0xAA; 6]);
        assert_eq!(parsed.sender_ip, [10, 0, 2, 15]);
        assert_eq!(parsed.target_ip, [10, 0, 2, 1]);
    }

    #[test]
    fn parse_wrong_hw_type() {
        let mut pkt = build(ARP_OP_REPLY, [0; 6], [0; 4], [0; 6], [0; 4]);
        // Corrupt hw_type to 2
        pkt[0] = 0;
        pkt[1] = 2;
        assert!(parse(&pkt).is_none());
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 27]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn build_round_trip() {
        let smac = [1, 2, 3, 4, 5, 6];
        let sip = [192, 168, 1, 1];
        let tmac = [7, 8, 9, 10, 11, 12];
        let tip = [192, 168, 1, 2];

        let raw = build(ARP_OP_REPLY, smac, sip, tmac, tip);
        let parsed = parse(&raw).unwrap();

        assert_eq!(parsed.operation, ARP_OP_REPLY);
        assert_eq!(parsed.sender_mac, smac);
        assert_eq!(parsed.sender_ip, sip);
        assert_eq!(parsed.target_mac, tmac);
        assert_eq!(parsed.target_ip, tip);
    }
}
