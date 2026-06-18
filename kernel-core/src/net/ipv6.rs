use alloc::vec::Vec;

use crate::types::Ipv6Addr;

/// Parsed IPv6 fixed header (40 bytes, RFC 8200 §3).
#[derive(Debug, Clone, Copy)]
pub struct Ipv6Header {
    pub version: u8, // always 6
    pub traffic_class: u8,
    pub flow_label: u32, // 20-bit
    pub payload_length: u16,
    pub next_header: u8,
    pub hop_limit: u8,
    pub src: Ipv6Addr,
    pub dst: Ipv6Addr,
}

/// ICMPv6 next-header value.
pub const PROTO_ICMPV6: u8 = 58;

/// Extension-header next-header values (RFC 8200 §4).
pub const EXT_HOPOPT: u8 = 0;
pub const EXT_ROUTING: u8 = 43;
pub const EXT_FRAGMENT: u8 = 44;
pub const EXT_DSTOPTS: u8 = 60;

/// Re-export the shared upper-layer protocol numbers (identical across v4/v6).
pub use super::ipv4::{PROTO_TCP, PROTO_UDP};

/// Parse a 40-byte IPv6 header. Returns the header and a slice of the payload.
///
/// Returns `None` on a truncated header (<40 bytes), a version nibble != 6, or a
/// payload shorter than `payload_length`. Never panics.
pub fn parse(data: &[u8]) -> Option<(Ipv6Header, &[u8])> {
    if data.len() < 40 {
        return None;
    }

    let version = data[0] >> 4;
    if version != 6 {
        return None;
    }

    let traffic_class = ((data[0] & 0x0F) << 4) | (data[1] >> 4);
    let flow_label = (((data[1] & 0x0F) as u32) << 16) | ((data[2] as u32) << 8) | (data[3] as u32);

    let payload_length = u16::from_be_bytes([data[4], data[5]]);
    let next_header = data[6];
    let hop_limit = data[7];

    let mut src = [0u8; 16];
    let mut dst = [0u8; 16];
    src.copy_from_slice(&data[8..24]);
    dst.copy_from_slice(&data[24..40]);

    let payload = &data[40..];
    if payload.len() < payload_length as usize {
        return None;
    }
    let payload = &payload[..payload_length as usize];

    Some((
        Ipv6Header {
            version,
            traffic_class,
            flow_label,
            payload_length,
            next_header,
            hop_limit,
            src,
            dst,
        },
        payload,
    ))
}

/// Build a 40-byte IPv6 header followed by `payload`.
///
/// `hop_limit` defaults to 64, `traffic_class` and `flow_label` to 0. Sets
/// `payload_length` = `payload.len()`.
pub fn build(src: Ipv6Addr, dst: Ipv6Addr, next_header: u8, payload: &[u8]) -> Vec<u8> {
    let max_payload = u16::MAX as usize;
    let payload = if payload.len() > max_payload {
        &payload[..max_payload]
    } else {
        payload
    };
    let payload_length = payload.len() as u16;

    let mut pkt = Vec::with_capacity(40 + payload.len());

    // Word 0: version(6) | traffic_class(0) | flow_label(0).
    pkt.push(0x60); // (version << 4) | (traffic_class >> 4)
    pkt.push(0x00); // ((traffic_class & 0x0f) << 4) | ((flow_label >> 16) & 0x0f)
    pkt.push(0x00); // (flow_label >> 8)
    pkt.push(0x00); // flow_label
    pkt.extend_from_slice(&payload_length.to_be_bytes());
    pkt.push(next_header);
    pkt.push(64); // hop_limit
    pkt.extend_from_slice(&src);
    pkt.extend_from_slice(&dst);

    pkt.extend_from_slice(payload);

    pkt
}

/// Compute the ones-complement checksum over the RFC 8200 §8.1 IPv6
/// pseudo-header followed by `upper_data`.
///
/// Every upper layer over IPv6 (ICMPv6, UDP, TCP) MUST checksum over this
/// pseudo-header. Reuses [`super::ipv4::checksum`] for the final RFC 1071 fold.
pub fn pseudo_header_checksum(
    src: Ipv6Addr,
    dst: Ipv6Addr,
    next_header: u8,
    upper_data: &[u8],
) -> u16 {
    // Accumulate the ones-complement sum in place — no temporary buffer. The
    // pseudo-header is exactly 40 bytes (16 src + 16 dst + 4 length + 3 zero +
    // 1 next-header), an even length, so the seam into `upper_data` stays
    // 16-bit aligned and the running sum equals the concatenated form.
    let len_be = (upper_data.len() as u32).to_be_bytes();
    let tail = [
        len_be[0],
        len_be[1],
        len_be[2],
        len_be[3],
        0,
        0,
        0,
        next_header,
    ];
    let mut sum: u32 = 0;
    super::ipv4::checksum_accumulate(&mut sum, &src);
    super::ipv4::checksum_accumulate(&mut sum, &dst);
    super::ipv4::checksum_accumulate(&mut sum, &tail);
    super::ipv4::checksum_accumulate(&mut sum, upper_data);
    super::ipv4::checksum_fold(sum)
}

/// Walk the IPv6 extension-header chain (RFC 8200 §4).
///
/// Given the fixed header's `next_header` value and the bytes that follow it
/// (the header's payload), locate the true upper-layer protocol by skipping
/// HOPOPT / ROUTING / FRAGMENT / DSTOPTS. Returns `(upper_layer_proto,
/// offset_into_payload_of_upper_header)`.
///
/// Bounded to at most 8 iterations — a malformed or cyclic chain terminates and
/// returns the last seen `(proto, offset)` rather than looping forever. If an
/// extension header's declared length runs past `payload`, the walk stops there.
/// Never panics.
pub fn walk_ext_headers(next_header: u8, payload: &[u8]) -> (u8, usize) {
    let mut proto = next_header;
    let mut offset: usize = 0;

    for _ in 0..8 {
        match proto {
            EXT_HOPOPT | EXT_ROUTING | EXT_DSTOPTS => {
                // [next_header(1), hdr_ext_len(1), ...]; length = (hdr_ext_len + 1) * 8.
                if offset + 2 > payload.len() {
                    break;
                }
                let next = payload[offset];
                let hdr_ext_len = payload[offset + 1];
                let skip = ((hdr_ext_len as usize) + 1) * 8;
                if offset + skip > payload.len() {
                    break;
                }
                proto = next;
                offset += skip;
            }
            EXT_FRAGMENT => {
                // Fixed 8 bytes; next header is the first byte.
                if offset + 8 > payload.len() {
                    break;
                }
                let next = payload[offset];
                proto = next;
                offset += 8;
            }
            // Any other value is the upper layer (TCP/UDP/ICMPv6/ESP/AH/unknown).
            _ => break,
        }
    }

    (proto, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_build_round_trip() {
        let src = [
            0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0x11, 0x22, 0xff, 0xfe, 0x33, 0x44, 0x55,
        ];
        let dst = [
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
        ];
        let payload = b"hello ipv6";
        let pkt = build(src, dst, PROTO_UDP, payload);

        let (header, data) = parse(&pkt).unwrap();
        assert_eq!(header.version, 6);
        assert_eq!(header.traffic_class, 0);
        assert_eq!(header.flow_label, 0);
        assert_eq!(header.payload_length, payload.len() as u16);
        assert_eq!(header.next_header, PROTO_UDP);
        assert_eq!(header.hop_limit, 64);
        assert_eq!(header.src, src);
        assert_eq!(header.dst, dst);
        assert_eq!(data, payload);
    }

    #[test]
    fn parse_nonzero_tc_flowlabel() {
        // Manually build a header with traffic_class=0xAB and flow_label=0x12345.
        let src = [1u8; 16];
        let dst = [2u8; 16];
        let payload = b"xy";
        let tc: u8 = 0xAB;
        let fl: u32 = 0x12345; // 20-bit

        let mut pkt = Vec::new();
        let byte0 = (6u8 << 4) | (tc >> 4);
        let byte1 = ((tc & 0x0F) << 4) | ((fl >> 16) as u8 & 0x0F);
        let byte2 = (fl >> 8) as u8;
        let byte3 = fl as u8;
        pkt.push(byte0);
        pkt.push(byte1);
        pkt.push(byte2);
        pkt.push(byte3);
        pkt.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        pkt.push(PROTO_TCP);
        pkt.push(33); // hop_limit
        pkt.extend_from_slice(&src);
        pkt.extend_from_slice(&dst);
        pkt.extend_from_slice(payload);

        let (header, data) = parse(&pkt).unwrap();
        assert_eq!(header.version, 6);
        assert_eq!(header.traffic_class, tc);
        assert_eq!(header.flow_label, fl);
        assert_eq!(header.next_header, PROTO_TCP);
        assert_eq!(header.hop_limit, 33);
        assert_eq!(header.src, src);
        assert_eq!(header.dst, dst);
        assert_eq!(data, payload);
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 39]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn parse_wrong_version() {
        let mut pkt = build([0; 16], [0; 16], 0, &[]);
        pkt[0] = 0x40; // version=4
        assert!(parse(&pkt).is_none());
    }

    #[test]
    fn parse_short_payload() {
        // Header claims a 10-byte payload but only 4 bytes follow.
        let mut pkt = build([0; 16], [0; 16], PROTO_UDP, &[1, 2, 3, 4]);
        pkt[4] = 0;
        pkt[5] = 10; // payload_length = 10
        assert!(parse(&pkt).is_none());
    }

    #[test]
    fn pseudo_header_checksum_self_cancels() {
        let src = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01];
        let dst = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02];
        // ICMPv6-like message: type, code, checksum(0,0), then some payload.
        let mut msg: Vec<u8> = alloc::vec![128, 0, 0, 0, 0xDE, 0xAD, 0xBE, 0xEF];

        // Compute the checksum with the field zeroed and place it.
        let cksum = pseudo_header_checksum(src, dst, PROTO_ICMPV6, &msg);
        msg[2] = (cksum >> 8) as u8;
        msg[3] = cksum as u8;

        // Recomputing over the now-complete message must yield 0.
        let verify = pseudo_header_checksum(src, dst, PROTO_ICMPV6, &msg);
        assert_eq!(verify, 0);
    }

    #[test]
    fn walk_hop_by_hop_then_icmpv6() {
        // Hop-by-Hop (next=58, hdr_ext_len=0 → 8 bytes total), then ICMPv6.
        let payload: Vec<u8> = alloc::vec![
            PROTO_ICMPV6, // next header
            0,            // hdr_ext_len → (0+1)*8 = 8 bytes
            0,
            0,
            0,
            0,
            0,
            0, // padding to fill the 8 bytes
            128,
            0,
            0,
            0, // ICMPv6 header begins here
        ];
        let (proto, offset) = walk_ext_headers(EXT_HOPOPT, &payload);
        assert_eq!(proto, PROTO_ICMPV6);
        assert_eq!(offset, 8);
    }

    #[test]
    fn walk_cyclic_chain_terminates() {
        // A Hop-by-Hop header that chains to another Hop-by-Hop forever.
        // 16 consecutive 8-byte HOPOPT headers, each pointing to HOPOPT.
        let mut payload: Vec<u8> = Vec::new();
        for _ in 0..16 {
            payload.push(EXT_HOPOPT); // next = HOPOPT again
            payload.push(0); // 8 bytes
            payload.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        }
        // This must terminate (the test completing is the proof).
        let (proto, offset) = walk_ext_headers(EXT_HOPOPT, &payload);
        assert_eq!(proto, EXT_HOPOPT); // still chained after the iteration cap
        assert!(offset <= payload.len(), "offset {offset} out of bounds");
    }

    #[test]
    fn walk_routing_then_tcp() {
        // An unsupported-but-well-formed ROUTING header is skipped to reach TCP.
        let mut payload: Vec<u8> = Vec::new();
        payload.push(PROTO_TCP); // next header
        payload.push(1); // hdr_ext_len=1 → (1+1)*8 = 16 bytes total
        payload.extend_from_slice(&[0u8; 14]); // fill out the 16 bytes
        payload.extend_from_slice(&[0u8; 4]); // start of the TCP header
        let (proto, offset) = walk_ext_headers(EXT_ROUTING, &payload);
        assert_eq!(proto, PROTO_TCP);
        assert_eq!(offset, 16);
    }

    #[test]
    fn walk_fragment_then_udp() {
        // Fragment header (fixed 8 bytes), next = UDP.
        let payload: Vec<u8> = alloc::vec![
            PROTO_UDP, // next header
            0,         // reserved
            0, 0, 0, 0, 0, 0, // rest of the fixed 8-byte fragment header
            0, 0, 0, 0, // UDP header begins
        ];
        let (proto, offset) = walk_ext_headers(EXT_FRAGMENT, &payload);
        assert_eq!(proto, PROTO_UDP);
        assert_eq!(offset, 8);
    }

    #[test]
    fn walk_direct_upper_layer() {
        // No extension headers — next_header is already the upper layer.
        let payload: Vec<u8> = alloc::vec![0, 0, 0, 0];
        let (proto, offset) = walk_ext_headers(PROTO_TCP, &payload);
        assert_eq!(proto, PROTO_TCP);
        assert_eq!(offset, 0);
    }
}
