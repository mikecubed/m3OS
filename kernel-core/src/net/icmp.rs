use alloc::vec::Vec;

use super::ipv4;

pub const ICMP_ECHO_REPLY: u8 = 0;
pub const ICMP_ECHO_REQUEST: u8 = 8;

/// Parsed ICMP header.
#[derive(Debug, Clone, Copy)]
pub struct IcmpHeader {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub rest: [u8; 4], // identifier (2) + sequence (2) for echo
}

/// Parse an ICMP packet.
pub fn parse(data: &[u8]) -> Option<(IcmpHeader, &[u8])> {
    if data.len() < 8 {
        return None;
    }
    let header = IcmpHeader {
        icmp_type: data[0],
        code: data[1],
        checksum: u16::from_be_bytes([data[2], data[3]]),
        rest: [data[4], data[5], data[6], data[7]],
    };
    Some((header, &data[8..]))
}

/// Build an ICMP packet with auto-computed checksum.
pub fn build(icmp_type: u8, code: u8, rest: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(8 + payload.len());
    pkt.push(icmp_type);
    pkt.push(code);
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    pkt.extend_from_slice(&rest);
    pkt.extend_from_slice(payload);

    let cksum = ipv4::checksum(&pkt);
    pkt[2] = (cksum >> 8) as u8;
    pkt[3] = cksum as u8;

    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        let pkt = build(ICMP_ECHO_REQUEST, 0, [0, 1, 0, 1], b"ping");
        let (header, payload) = parse(&pkt).unwrap();
        assert_eq!(header.icmp_type, ICMP_ECHO_REQUEST);
        assert_eq!(header.code, 0);
        assert_eq!(payload, b"ping");
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 7]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn build_checksum_verification() {
        let pkt = build(ICMP_ECHO_REPLY, 0, [0, 1, 0, 2], b"data");
        // Verify the checksum is correct: checksum of entire message should be 0
        let verify = ipv4::checksum(&pkt);
        assert_eq!(verify, 0);
    }

    #[test]
    fn echo_request_reply_type_codes() {
        assert_eq!(ICMP_ECHO_REPLY, 0);
        assert_eq!(ICMP_ECHO_REQUEST, 8);

        let req = build(ICMP_ECHO_REQUEST, 0, [0, 1, 0, 1], &[]);
        let (hdr, _) = parse(&req).unwrap();
        assert_eq!(hdr.icmp_type, ICMP_ECHO_REQUEST);

        let reply = build(ICMP_ECHO_REPLY, 0, [0, 1, 0, 1], &[]);
        let (hdr, _) = parse(&reply).unwrap();
        assert_eq!(hdr.icmp_type, ICMP_ECHO_REPLY);
    }
}
