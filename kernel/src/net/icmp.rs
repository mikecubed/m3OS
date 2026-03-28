//! ICMP echo request/reply — pure logic re-exported from kernel-core.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

use super::ipv4::{self, Ipv4Header};

use kernel_core::net::icmp::{build, parse};
#[allow(unused_imports)]
pub use kernel_core::net::icmp::{IcmpHeader, ICMP_ECHO_REPLY, ICMP_ECHO_REQUEST};

/// Handle an incoming ICMP packet.
pub fn handle_icmp(ip_header: &Ipv4Header, payload: &[u8]) {
    let (icmp_hdr, icmp_data) = match parse(payload) {
        Some(h) => h,
        None => return,
    };

    match icmp_hdr.icmp_type {
        ICMP_ECHO_REQUEST => {
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
            let id = u16::from_be_bytes([icmp_hdr.rest[0], icmp_hdr.rest[1]]);
            let seq = u16::from_be_bytes([icmp_hdr.rest[2], icmp_hdr.rest[3]]);

            let expected_id = PING_EXPECTED_ID.load(Ordering::Acquire);
            let expected_seq = PING_EXPECTED_SEQ.load(Ordering::Acquire);
            if id != expected_id || seq != expected_seq {
                log::debug!(
                    "[icmp] ignoring echo reply id={} seq={} (expected id={} seq={})",
                    id,
                    seq,
                    expected_id,
                    expected_seq
                );
                return;
            }

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

pub static PING_REPLY_RECEIVED: AtomicBool = AtomicBool::new(false);
pub static PING_REPLY_TICK: AtomicU64 = AtomicU64::new(0);
pub static PING_EXPECTED_ID: AtomicU16 = AtomicU16::new(0);
pub static PING_EXPECTED_SEQ: AtomicU16 = AtomicU16::new(0);

/// Send an ICMP echo request to the given IP address.
pub fn ping(target_ip: super::arp::Ipv4Addr, seq: u16) -> u64 {
    PING_REPLY_RECEIVED.store(false, Ordering::Release);
    PING_EXPECTED_ID.store(1, Ordering::Release);
    PING_EXPECTED_SEQ.store(seq, Ordering::Release);

    let rest = [0x00, 0x01, (seq >> 8) as u8, seq as u8];
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
