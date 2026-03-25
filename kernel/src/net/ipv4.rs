//! IPv4 packet parsing, construction, and sending (P16-T024 through P16-T030).

use alloc::vec::Vec;

use super::arp::{self, Ipv4Addr};
use super::config;
use super::ethernet;
use super::virtio_net;

// ===========================================================================
// IPv4 Header (P16-T024)
// ===========================================================================

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

// ===========================================================================
// Parse (P16-T025)
// ===========================================================================

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
    let checksum = u16::from_be_bytes([data[10], data[11]]);

    let mut src = [0u8; 4];
    let mut dst = [0u8; 4];
    src.copy_from_slice(&data[12..16]);
    dst.copy_from_slice(&data[16..20]);

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
            checksum,
            src,
            dst,
        },
        payload,
    ))
}

// ===========================================================================
// Checksum (P16-T026)
// ===========================================================================

/// Compute the IPv4 header checksum (RFC 1071).
///
/// Works on any slice of bytes; the caller passes the header with the
/// checksum field set to zero.
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

// ===========================================================================
// Build (P16-T027)
// ===========================================================================

/// Build a raw IPv4 packet with the given protocol and payload.
///
/// Uses TTL=64, no fragmentation, auto-computed checksum.
/// Payloads larger than 65515 bytes are truncated to fit the 16-bit total length.
pub fn build(src: Ipv4Addr, dst: Ipv4Addr, protocol: u8, payload: &[u8]) -> Vec<u8> {
    // IPv4 total_length is 16 bits and includes the 20-byte header.
    let max_payload = (u16::MAX as usize) - 20;
    let payload = if payload.len() > max_payload {
        &payload[..max_payload]
    } else {
        payload
    };
    let total_length = 20 + payload.len() as u16;

    let mut pkt = Vec::with_capacity(total_length as usize);

    // Version (4) + IHL (5) = 0x45
    pkt.push(0x45);
    // DSCP + ECN
    pkt.push(0x00);
    // Total length
    pkt.extend_from_slice(&total_length.to_be_bytes());
    // Identification (use a simple counter would be better, but 0 is fine for now)
    pkt.extend_from_slice(&0u16.to_be_bytes());
    // Flags (DF=1) + Fragment offset (0)
    pkt.extend_from_slice(&0x4000u16.to_be_bytes());
    // TTL
    pkt.push(64);
    // Protocol
    pkt.push(protocol);
    // Checksum placeholder (will be computed)
    pkt.extend_from_slice(&0u16.to_be_bytes());
    // Source IP
    pkt.extend_from_slice(&src);
    // Destination IP
    pkt.extend_from_slice(&dst);

    // Compute and fill in the checksum.
    let cksum = checksum(&pkt[..20]);
    pkt[10] = (cksum >> 8) as u8;
    pkt[11] = cksum as u8;

    // Payload
    pkt.extend_from_slice(payload);

    pkt
}

// ===========================================================================
// Send (P16-T028)
// ===========================================================================

/// Send an IPv4 packet to the given destination.
///
/// Resolves the next-hop MAC via ARP (gateway if off-subnet), wraps in an
/// Ethernet frame, and sends via virtio-net.
pub fn send(dst_ip: Ipv4Addr, protocol: u8, payload: &[u8]) {
    let our_mac = match virtio_net::mac_address() {
        Some(m) => m,
        None => return,
    };
    let our_ip = config::our_ip();

    // Determine next-hop IP for ARP resolution.
    let next_hop = if config::is_local(dst_ip) {
        dst_ip
    } else {
        config::gateway_ip()
    };

    // Try ARP cache first; if miss, send request and use broadcast as fallback.
    let dst_mac = match arp::resolve(next_hop) {
        Some(mac) => mac,
        None => {
            // Send ARP request and use broadcast for this first packet.
            // The reply will populate the cache for subsequent packets.
            arp::send_request(next_hop);
            ethernet::MAC_BROADCAST
        }
    };

    let ip_pkt = build(our_ip, dst_ip, protocol, payload);
    let frame = ethernet::build(dst_mac, our_mac, ethernet::ETHERTYPE_IPV4, &ip_pkt);
    virtio_net::send_frame(&frame);
}

// ===========================================================================
// Protocol dispatch (P16-T030)
// ===========================================================================

/// Dispatch a received IPv4 packet to the appropriate protocol handler.
pub fn handle_ipv4(header: &Ipv4Header, payload: &[u8]) {
    match header.protocol {
        PROTO_ICMP => {
            super::icmp::handle_icmp(header, payload);
        }
        PROTO_UDP => {
            super::udp::handle_udp(header, payload);
        }
        PROTO_TCP => {
            super::tcp::handle_tcp(header, payload);
        }
        _ => {}
    }
}
