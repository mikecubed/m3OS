use alloc::vec::Vec;

use super::ipv4;
use crate::types::Ipv4Addr;

/// TCP flag bits.
pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;

/// Parsed TCP header.
#[derive(Debug, Clone, Copy)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub data_offset: u8,
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
}

/// Maximum TCP segment size that fits in an IPv4 packet.
pub const MAX_TCP_SEGMENT: usize = 65515;

/// Compute TCP checksum with pseudo-header.
pub fn tcp_checksum(src_ip: Ipv4Addr, dst_ip: Ipv4Addr, tcp_data: &[u8]) -> u16 {
    let tcp_len = tcp_data.len() as u16;
    let mut pseudo = Vec::with_capacity(12 + tcp_data.len());
    pseudo.extend_from_slice(&src_ip);
    pseudo.extend_from_slice(&dst_ip);
    pseudo.push(0); // reserved
    pseudo.push(6); // protocol TCP
    pseudo.extend_from_slice(&tcp_len.to_be_bytes());
    pseudo.extend_from_slice(tcp_data);
    ipv4::checksum(&pseudo)
}

/// Parse a TCP segment.
pub fn parse(data: &[u8]) -> Option<(TcpHeader, &[u8])> {
    if data.len() < 20 {
        return None;
    }

    let data_offset = data[12] >> 4;
    if data_offset < 5 {
        return None;
    }
    let header_len = (data_offset as usize) * 4;
    if data.len() < header_len {
        return None;
    }

    let header = TcpHeader {
        src_port: u16::from_be_bytes([data[0], data[1]]),
        dst_port: u16::from_be_bytes([data[2], data[3]]),
        seq: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        ack: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
        data_offset,
        flags: data[13],
        window: u16::from_be_bytes([data[14], data[15]]),
        checksum: u16::from_be_bytes([data[16], data[17]]),
        urgent: u16::from_be_bytes([data[18], data[19]]),
    };

    Some((header, &data[header_len..]))
}

/// Parameters for building a TCP segment.
pub struct TcpBuildParams {
    pub src_ip: Ipv4Addr,
    pub dst_ip: Ipv4Addr,
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
}

/// Build a TCP segment with auto-computed checksum.
pub fn build(p: &TcpBuildParams, payload: &[u8]) -> Vec<u8> {
    let max_payload = MAX_TCP_SEGMENT - 20;
    let payload = if payload.len() > max_payload {
        &payload[..max_payload]
    } else {
        payload
    };
    let data_offset: u8 = 5;
    let total_len = 20 + payload.len();
    let mut pkt = Vec::with_capacity(total_len);

    pkt.extend_from_slice(&p.src_port.to_be_bytes());
    pkt.extend_from_slice(&p.dst_port.to_be_bytes());
    pkt.extend_from_slice(&p.seq.to_be_bytes());
    pkt.extend_from_slice(&p.ack.to_be_bytes());
    pkt.push(data_offset << 4);
    pkt.push(p.flags);
    pkt.extend_from_slice(&p.window.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    pkt.extend_from_slice(&0u16.to_be_bytes()); // urgent pointer
    pkt.extend_from_slice(payload);

    let cksum = tcp_checksum(p.src_ip, p.dst_ip, &pkt);
    pkt[16] = (cksum >> 8) as u8;
    pkt[17] = cksum as u8;

    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params() -> TcpBuildParams {
        TcpBuildParams {
            src_ip: [10, 0, 0, 1],
            dst_ip: [10, 0, 0, 2],
            src_port: 12345,
            dst_port: 80,
            seq: 1000,
            ack: 2000,
            flags: TCP_ACK,
            window: 8192,
        }
    }

    #[test]
    fn parse_valid() {
        let p = sample_params();
        let seg = build(&p, b"GET / HTTP/1.0");
        let (header, payload) = parse(&seg).unwrap();
        assert_eq!(header.src_port, 12345);
        assert_eq!(header.dst_port, 80);
        assert_eq!(header.seq, 1000);
        assert_eq!(header.ack, 2000);
        assert_eq!(header.flags, TCP_ACK);
        assert_eq!(payload, b"GET / HTTP/1.0");
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 19]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn build_round_trip() {
        let p = sample_params();
        let payload = b"hello tcp";
        let seg = build(&p, payload);
        let (header, data) = parse(&seg).unwrap();
        assert_eq!(header.src_port, p.src_port);
        assert_eq!(header.dst_port, p.dst_port);
        assert_eq!(header.seq, p.seq);
        assert_eq!(header.ack, p.ack);
        assert_eq!(data, payload);
    }

    #[test]
    fn tcp_checksum_verification() {
        let p = sample_params();
        let seg = build(&p, b"data");
        // Verify checksum: recomputing should yield 0
        let verify = tcp_checksum(p.src_ip, p.dst_ip, &seg);
        assert_eq!(verify, 0);
    }

    #[test]
    fn flag_constants() {
        assert_eq!(TCP_FIN, 0x01);
        assert_eq!(TCP_SYN, 0x02);
        assert_eq!(TCP_RST, 0x04);
        assert_eq!(TCP_PSH, 0x08);
        assert_eq!(TCP_ACK, 0x10);
        // Flags are combinable
        let combined = TCP_SYN | TCP_ACK;
        assert_eq!(combined, 0x12);
    }
}
