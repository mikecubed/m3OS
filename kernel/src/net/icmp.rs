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

// Kernel ping builtin removed in Phase 23 — ping is now a userspace binary.
// See userspace/ping/ for the socket-based implementation.
