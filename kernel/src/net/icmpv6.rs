//! ICMPv6 (RFC 4443) echo + NDP dispatch — the structural mirror of `icmp.rs`.
//! Pure parse/build live in `kernel_core::net::icmpv6`; the inbound handler,
//! echo counters, and ping6 reply-tracking live here. The ICMPv6 checksum is
//! verified over the IPv6 pseudo-header (ICMPv4 has none).

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering};

use kernel_core::net::icmpv6::{
    self, ICMPV6_ECHO_REPLY, ICMPV6_ECHO_REQUEST, ICMPV6_NEIGHBOR_ADVERTISEMENT,
    ICMPV6_NEIGHBOR_SOLICITATION, ICMPV6_ROUTER_ADVERTISEMENT,
};
use kernel_core::net::ipv6::{self, Ipv6Header};
use kernel_core::types::MacAddr;

use super::ndp;

/// ICMPv6 echo requests received (for one of our addresses).
static ECHO_RX_V6: AtomicU32 = AtomicU32::new(0);
/// ICMPv6 echo replies we have transmitted.
static ECHO_TX_V6: AtomicU32 = AtomicU32::new(0);

/// Snapshot `(echo requests received, replies sent)`.
#[allow(dead_code)]
pub fn echo_counts() -> (u32, u32) {
    (
        ECHO_RX_V6.load(Ordering::Relaxed),
        ECHO_TX_V6.load(Ordering::Relaxed),
    )
}

// ping6 reply tracking — mirrors `icmp::PING_*` (used by the AF_INET6 ICMPv6
// socket recv path).
pub static PING6_REPLY_RECEIVED: AtomicBool = AtomicBool::new(false);
pub static PING6_REPLY_TICK: AtomicU64 = AtomicU64::new(0);
pub static PING6_EXPECTED_ID: AtomicU16 = AtomicU16::new(0);
pub static PING6_EXPECTED_SEQ: AtomicU16 = AtomicU16::new(0);

/// Arm the ping6 reply tracker for an outgoing echo request (id+seq). The reply
/// — whether it returns over the wire or via the `ipv6::send_from` internal
/// loopback for a self/`::1` destination — is matched in the ECHO_REPLY arm.
pub fn arm_ping6(id: u16, seq: u16) {
    PING6_REPLY_RECEIVED.store(false, Ordering::Release);
    PING6_EXPECTED_ID.store(id, Ordering::Release);
    PING6_EXPECTED_SEQ.store(seq, Ordering::Release);
}

/// Handle an inbound ICMPv6 message (dispatched from `ipv6::handle_ipv6`).
/// `src_mac` is the Ethernet source, used by NDP for passive learning.
pub fn handle_icmpv6(ip_header: &Ipv6Header, payload: &[u8], src_mac: MacAddr) {
    // Verify the pseudo-header checksum; a wrong-checksum packet is dropped.
    if ipv6::pseudo_header_checksum(ip_header.src, ip_header.dst, ipv6::PROTO_ICMPV6, payload) != 0
    {
        return;
    }

    let (hdr, body) = match icmpv6::parse(payload) {
        Some(h) => h,
        None => return,
    };

    match hdr.msg_type {
        ICMPV6_ECHO_REQUEST => {
            ECHO_RX_V6.fetch_add(1, Ordering::Relaxed);
            // Reply from the pinged address (ip_header.dst) back to the sender.
            let reply = icmpv6::build(
                ICMPV6_ECHO_REPLY,
                0,
                hdr.rest,
                body,
                ip_header.dst,
                ip_header.src,
            );
            super::ipv6::send_from(ip_header.dst, ip_header.src, ipv6::PROTO_ICMPV6, &reply);
            ECHO_TX_V6.fetch_add(1, Ordering::Relaxed);
        }
        ICMPV6_ECHO_REPLY => {
            let id = u16::from_be_bytes([hdr.rest[0], hdr.rest[1]]);
            let seq = u16::from_be_bytes([hdr.rest[2], hdr.rest[3]]);
            if id == PING6_EXPECTED_ID.load(Ordering::Acquire)
                && seq == PING6_EXPECTED_SEQ.load(Ordering::Acquire)
            {
                let tick = crate::arch::x86_64::interrupts::tick_count();
                PING6_REPLY_TICK.store(tick, Ordering::Release);
                PING6_REPLY_RECEIVED.store(true, Ordering::Release);
            }
        }
        ICMPV6_NEIGHBOR_SOLICITATION => {
            ndp::handle_neighbor_solicitation(&ip_header.src, payload);
        }
        ICMPV6_NEIGHBOR_ADVERTISEMENT => {
            ndp::handle_neighbor_advertisement(payload);
        }
        ICMPV6_ROUTER_ADVERTISEMENT => {
            ndp::handle_router_advertisement(&ip_header.src, src_mac, payload);
        }
        // Router Solicitation, errors, etc. — no host action.
        _ => {}
    }
}
