//! ICMP echo request/reply (P16-T031 through P16-T033).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::ipv4::{self, Ipv4Header};

// ===========================================================================
// ICMP types
// ===========================================================================

const ICMP_ECHO_REPLY: u8 = 0;
const ICMP_ECHO_REQUEST: u8 = 8;

// ===========================================================================
// ICMP header (P16-T031)
// ===========================================================================

/// Parsed ICMP header.
#[derive(Debug, Clone, Copy)]
pub struct IcmpHeader {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub rest: [u8; 4], // identifier (2) + sequence (2) for echo
}

/// Parse an ICMP packet.
fn parse(data: &[u8]) -> Option<(IcmpHeader, &[u8])> {
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
fn build(icmp_type: u8, code: u8, rest: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(8 + payload.len());
    pkt.push(icmp_type);
    pkt.push(code);
    // Checksum placeholder
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&rest);
    pkt.extend_from_slice(payload);

    // Compute checksum over entire ICMP message.
    let cksum = ipv4::checksum(&pkt);
    pkt[2] = (cksum >> 8) as u8;
    pkt[3] = cksum as u8;

    pkt
}

// ===========================================================================
// ICMP echo reply (P16-T032)
// ===========================================================================

/// Handle an incoming ICMP packet.
pub fn handle_icmp(ip_header: &Ipv4Header, payload: &[u8]) {
    let (icmp_hdr, icmp_data) = match parse(payload) {
        Some(h) => h,
        None => return,
    };

    match icmp_hdr.icmp_type {
        ICMP_ECHO_REQUEST => {
            // Send echo reply with the same identifier, sequence, and data.
            let reply = build(ICMP_ECHO_REPLY, 0, icmp_hdr.rest, icmp_data);
            ipv4::send(ip_header.src, ipv4::PROTO_ICMP, &reply);
            log::debug!(
                "[icmp] echo reply sent to {}.{}.{}.{}",
                ip_header.src[0],
                ip_header.src[1],
                ip_header.src[2],
                ip_header.src[3],
            );
        }
        ICMP_ECHO_REPLY => {
            // Record the reply for the ping function.
            let seq = u16::from_be_bytes([icmp_hdr.rest[2], icmp_hdr.rest[3]]);
            let tick = crate::arch::x86_64::interrupts::tick_count();
            PING_REPLY_TICK.store(tick, Ordering::Release);
            PING_REPLY_RECEIVED.store(true, Ordering::Release);
            log::info!(
                "[icmp] echo reply from {}.{}.{}.{} seq={}",
                ip_header.src[0],
                ip_header.src[1],
                ip_header.src[2],
                ip_header.src[3],
                seq,
            );
        }
        _ => {}
    }
}

// ===========================================================================
// Ping (P16-T033)
// ===========================================================================

/// Tracks whether a ping reply has been received.
pub static PING_REPLY_RECEIVED: AtomicBool = AtomicBool::new(false);
/// Tick count when the ping reply was received.
pub static PING_REPLY_TICK: AtomicU64 = AtomicU64::new(0);

/// Send an ICMP echo request to the given IP address.
///
/// Returns the tick count at which the request was sent, for RTT calculation.
pub fn ping(target_ip: super::arp::Ipv4Addr, seq: u16) -> u64 {
    PING_REPLY_RECEIVED.store(false, Ordering::Release);

    let rest = [
        0x00,
        0x01, // identifier
        (seq >> 8) as u8,
        seq as u8, // sequence number
    ];
    // 32 bytes of payload data.
    let payload = [0xABu8; 32];

    let icmp_pkt = build(ICMP_ECHO_REQUEST, 0, rest, &payload);
    let send_tick = crate::arch::x86_64::interrupts::tick_count();
    ipv4::send(target_ip, ipv4::PROTO_ICMP, &icmp_pkt);

    log::debug!(
        "[icmp] echo request sent to {}.{}.{}.{} seq={}",
        target_ip[0],
        target_ip[1],
        target_ip[2],
        target_ip[3],
        seq,
    );

    send_tick
}
