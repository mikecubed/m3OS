use alloc::vec::Vec;

use crate::types::MacAddr;

pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV4: u16 = 0x0800;

/// Broadcast MAC address.
pub const MAC_BROADCAST: MacAddr = [0xFF; 6];

/// Parsed Ethernet frame header.
#[derive(Debug, Clone)]
pub struct EthernetFrame {
    pub dst: MacAddr,
    pub src: MacAddr,
    pub ethertype: u16,
    pub payload: Vec<u8>,
}

/// Parse a raw Ethernet frame into header fields and payload.
pub fn parse(raw: &[u8]) -> Option<EthernetFrame> {
    if raw.len() < 14 {
        return None;
    }

    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&raw[0..6]);
    src.copy_from_slice(&raw[6..12]);
    let ethertype = u16::from_be_bytes([raw[12], raw[13]]);
    let payload = raw[14..].to_vec();

    Some(EthernetFrame {
        dst,
        src,
        ethertype,
        payload,
    })
}

/// Construct a raw Ethernet frame from header fields and payload.
pub fn build(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_frame() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]); // dst
        raw.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]); // src
        raw.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes()); // ethertype
        raw.extend_from_slice(b"payload");

        let frame = parse(&raw).unwrap();
        assert_eq!(frame.dst, [0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        assert_eq!(frame.src, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(frame.ethertype, ETHERTYPE_IPV4);
        assert_eq!(frame.payload, b"payload");
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 13]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn build_round_trip() {
        let dst = [1, 2, 3, 4, 5, 6];
        let src = [7, 8, 9, 10, 11, 12];
        let payload = b"test data";

        let raw = build(dst, src, ETHERTYPE_ARP, payload);
        let frame = parse(&raw).unwrap();

        assert_eq!(frame.dst, dst);
        assert_eq!(frame.src, src);
        assert_eq!(frame.ethertype, ETHERTYPE_ARP);
        assert_eq!(frame.payload, payload);
    }
}
