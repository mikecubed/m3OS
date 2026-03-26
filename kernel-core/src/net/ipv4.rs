use alloc::vec::Vec;

use crate::types::Ipv4Addr;

/// Parsed IPv4 header (20 bytes, no options).
#[derive(Debug, Clone, Copy)]
pub struct Ipv4Header {
    pub version: u8,
    pub ihl: u8,
    pub total_length: u16,
    pub identification: u16,
    pub flags_fragment: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
}

/// IP protocol numbers.
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

/// Parse an IPv4 packet. Returns the header and a slice of the payload.
pub fn parse(data: &[u8]) -> Option<(Ipv4Header, &[u8])> {
    if data.len() < 20 {
        return None;
    }

    let version = data[0] >> 4;
    let ihl = data[0] & 0x0F;

    if version != 4 || ihl < 5 {
        return None;
    }

    let header_len = (ihl as usize) * 4;
    if data.len() < header_len {
        return None;
    }

    let total_length = u16::from_be_bytes([data[2], data[3]]);
    let identification = u16::from_be_bytes([data[4], data[5]]);
    let flags_fragment = u16::from_be_bytes([data[6], data[7]]);
    let ttl = data[8];
    let protocol = data[9];
    let chk = u16::from_be_bytes([data[10], data[11]]);

    let mut src = [0u8; 4];
    let mut dst = [0u8; 4];
    src.copy_from_slice(&data[12..16]);
    dst.copy_from_slice(&data[16..20]);

    if (total_length as usize) < header_len {
        return None;
    }
    let payload_end = (total_length as usize).min(data.len());
    let payload = &data[header_len..payload_end];

    Some((
        Ipv4Header {
            version,
            ihl,
            total_length,
            identification,
            flags_fragment,
            ttl,
            protocol,
            checksum: chk,
            src,
            dst,
        },
        payload,
    ))
}

/// Compute the IPv4 header checksum (RFC 1071).
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build a raw IPv4 packet with the given protocol and payload.
pub fn build(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    let max_payload = (u16::MAX as usize) - 20;
    let payload = if payload.len() > max_payload {
        &payload[..max_payload]
    } else {
        payload
    };
    let total_length = 20 + payload.len() as u16;

    let mut pkt = Vec::with_capacity(total_length as usize);

    pkt.push(0x45); // Version (4) + IHL (5)
    pkt.push(0x00); // DSCP + ECN
    pkt.extend_from_slice(&total_length.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // Identification
    pkt.extend_from_slice(&0x4000u16.to_be_bytes()); // Flags (DF=1)
    pkt.push(64); // TTL
    pkt.push(protocol);
    pkt.extend_from_slice(&0u16.to_be_bytes()); // Checksum placeholder
    pkt.extend_from_slice(&src);
    pkt.extend_from_slice(&dst);

    let cksum = checksum(&pkt[..20]);
    pkt[10] = (cksum >> 8) as u8;
    pkt[11] = cksum as u8;

    pkt.extend_from_slice(payload);

    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_packet() {
        let src = [10, 0, 2, 15];
        let dst = [10, 0, 2, 1];
        let payload = b"hello";
        let pkt = build(src, dst, PROTO_UDP, payload);

        let (header, data) = parse(&pkt).unwrap();
        assert_eq!(header.version, 4);
        assert_eq!(header.ihl, 5);
        assert_eq!(header.protocol, PROTO_UDP);
        assert_eq!(header.src, src);
        assert_eq!(header.dst, dst);
        assert_eq!(data, payload);
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 19]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn parse_wrong_version() {
        // Version 6 instead of 4
        let mut pkt = build([0; 4], [0; 4], 0, &[]);
        pkt[0] = 0x65; // version=6, ihl=5
        assert!(parse(&pkt).is_none());
    }

    #[test]
    fn checksum_rfc_vectors() {
        // Verify that checksum of a correct header is 0
        let pkt = build([10, 0, 0, 1], [10, 0, 0, 2], PROTO_ICMP, &[]);
        let verify = checksum(&pkt[..20]);
        assert_eq!(verify, 0);
    }

    #[test]
    fn build_round_trip() {
        let src = [192, 168, 1, 1];
        let dst = [192, 168, 1, 2];
        let payload = b"test payload data";

        let pkt = build(src, dst, PROTO_TCP, payload);
        let (header, data) = parse(&pkt).unwrap();

        assert_eq!(header.src, src);
        assert_eq!(header.dst, dst);
        assert_eq!(header.protocol, PROTO_TCP);
        assert_eq!(header.ttl, 64);
        assert_eq!(data, payload);
        // Verify checksum
        assert_eq!(checksum(&pkt[..20]), 0);
    }
}
