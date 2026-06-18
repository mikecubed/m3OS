use alloc::vec::Vec;

use crate::types::Ipv6Addr;

use super::ipv6;

/// Parsed ICMPv6 header (8 bytes: type, code, checksum, 4-byte rest), mirroring
/// `icmp::IcmpHeader`. `rest` is the 4 bytes after the checksum — for Echo it is
/// identifier(2)+sequence(2); for NDP NS/NA it is the reserved/flags word.
#[derive(Debug, Clone, Copy)]
pub struct Icmpv6Header {
    pub msg_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub rest: [u8; 4],
}

// ICMPv6 type constants (RFC 4443 + RFC 4861). Define ALL of these here —
// icmpv6.rs is the natural home and ndp.rs (a later task) will import them.
pub const ICMPV6_DEST_UNREACHABLE: u8 = 1;
pub const ICMPV6_PACKET_TOO_BIG: u8 = 2;
pub const ICMPV6_TIME_EXCEEDED: u8 = 3;
pub const ICMPV6_PARAM_PROBLEM: u8 = 4;
pub const ICMPV6_ECHO_REQUEST: u8 = 128;
pub const ICMPV6_ECHO_REPLY: u8 = 129;
pub const ICMPV6_ROUTER_SOLICITATION: u8 = 133;
pub const ICMPV6_ROUTER_ADVERTISEMENT: u8 = 134;
pub const ICMPV6_NEIGHBOR_SOLICITATION: u8 = 135;
pub const ICMPV6_NEIGHBOR_ADVERTISEMENT: u8 = 136;

/// Parse an ICMPv6 message into (header, body_after_rest). Returns `None` if
/// `data` is shorter than 8 bytes. Never panics. (Mirror `icmp::parse`.)
pub fn parse(data: &[u8]) -> Option<(Icmpv6Header, &[u8])> {
    if data.len() < 8 {
        return None;
    }
    let header = Icmpv6Header {
        msg_type: data[0],
        code: data[1],
        checksum: u16::from_be_bytes([data[2], data[3]]),
        rest: [data[4], data[5], data[6], data[7]],
    };
    Some((header, &data[8..]))
}

/// Build an ICMPv6 message with a correct pseudo-header checksum.
///
/// Layout: msg_type(1) code(1) checksum(2, computed) rest(4) body(..).
/// Computes the checksum via
/// [`ipv6::pseudo_header_checksum`]`(src, dst, PROTO_ICMPV6, &packet_with_zero_checksum)`
/// and writes it into bytes `[2..4]`.
pub fn build(
    msg_type: u8,
    code: u8,
    rest: [u8; 4],
    body: &[u8],
    src: Ipv6Addr,
    dst: Ipv6Addr,
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(8 + body.len());
    pkt.push(msg_type);
    pkt.push(code);
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    pkt.extend_from_slice(&rest);
    pkt.extend_from_slice(body);

    let ck = ipv6::pseudo_header_checksum(src, dst, ipv6::PROTO_ICMPV6, &pkt);
    pkt[2..4].copy_from_slice(&ck.to_be_bytes());

    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: Ipv6Addr = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01];
    const DST: Ipv6Addr = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02];

    #[test]
    fn echo_round_trip_and_checksum_correct() {
        let built = build(
            ICMPV6_ECHO_REQUEST,
            0,
            [0, 1, 0, 7],
            b"ping-payload",
            SRC,
            DST,
        );

        let (header, body) = parse(&built).unwrap();
        assert_eq!(header.msg_type, ICMPV6_ECHO_REQUEST);
        assert_eq!(header.code, 0);
        assert_eq!(header.rest, [0, 1, 0, 7]);
        assert_eq!(body, b"ping-payload");

        // A correct checksum makes the whole-message recompute fold to 0.
        assert_eq!(
            ipv6::pseudo_header_checksum(SRC, DST, ipv6::PROTO_ICMPV6, &built),
            0
        );
    }

    #[test]
    fn wrong_checksum_is_detectable() {
        let mut built = build(
            ICMPV6_ECHO_REQUEST,
            0,
            [0, 1, 0, 7],
            b"ping-payload",
            SRC,
            DST,
        );
        // Flip a byte in the body (after the 8-byte header).
        built[10] ^= 0xFF;
        // The corrupted message must no longer fold to 0, so the kernel drops it.
        assert_ne!(
            ipv6::pseudo_header_checksum(SRC, DST, ipv6::PROTO_ICMPV6, &built),
            0
        );
    }

    #[test]
    fn parse_rejects_too_short() {
        assert!(parse(&[0u8; 7]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn type_constants_have_rfc_values() {
        assert_eq!(ICMPV6_DEST_UNREACHABLE, 1);
        assert_eq!(ICMPV6_PACKET_TOO_BIG, 2);
        assert_eq!(ICMPV6_TIME_EXCEEDED, 3);
        assert_eq!(ICMPV6_PARAM_PROBLEM, 4);
        assert_eq!(ICMPV6_ECHO_REQUEST, 128);
        assert_eq!(ICMPV6_ECHO_REPLY, 129);
        assert_eq!(ICMPV6_ROUTER_SOLICITATION, 133);
        assert_eq!(ICMPV6_ROUTER_ADVERTISEMENT, 134);
        assert_eq!(ICMPV6_NEIGHBOR_SOLICITATION, 135);
        assert_eq!(ICMPV6_NEIGHBOR_ADVERTISEMENT, 136);
    }
}
