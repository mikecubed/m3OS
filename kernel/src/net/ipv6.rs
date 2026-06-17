//! IPv6 L3 send + receive (RFC 8200) — the structural sibling of `ipv4.rs`.
//! Pure parse/build live in `kernel_core::net::ipv6`; next-hop selection,
//! NDP-driven neighbor resolution, RX dispatch, and the SLAAC/DHCPv6 periodic
//! tick live here.

use kernel_core::net::ipv6 as ipv6_core;
use kernel_core::net::{ethernet as eth_core, udp as udp_core};
use kernel_core::types::{Ipv6Addr, MacAddr, ipv6_is_multicast};

use super::{config, ethernet, ndp};

pub use kernel_core::net::ipv6::{
    Ipv6Header, PROTO_ICMPV6, PROTO_TCP, PROTO_UDP, parse, walk_ext_headers,
};

/// Map an IPv6 multicast destination to its Ethernet MAC (RFC 2464 §7).
fn multicast_mac(addr: &Ipv6Addr) -> MacAddr {
    [0x33, 0x33, addr[12], addr[13], addr[14], addr[15]]
}

/// Resolve the next-hop MAC for `dst`, or `None` (queuing an NDP solicitation on
/// a unicast cache miss, mirroring `ipv4::send`'s ARP behaviour).
fn next_hop_mac(dst: &Ipv6Addr) -> Option<MacAddr> {
    if ipv6_is_multicast(dst) {
        return Some(multicast_mac(dst));
    }
    let next_hop = if config::is_local_v6(dst) {
        *dst
    } else {
        config::gateway_ip_v6()? // None → no default route yet
    };
    match ndp::resolve(&next_hop) {
        Some(mac) => Some(mac),
        None => {
            ndp::send_solicitation(&next_hop);
            None
        }
    }
}

/// Send an IPv6 packet, choosing our configured source address.
pub fn send(dst: Ipv6Addr, next_header: u8, payload: &[u8]) {
    let src = match config::our_ip_v6() {
        Some(s) => s,
        None => return,
    };
    send_from(src, dst, next_header, payload);
}

/// True if `dst` is `::1` or one of the addresses this host assigned to itself.
fn is_self_address(dst: &Ipv6Addr) -> bool {
    kernel_core::types::ipv6_is_loopback(dst)
        || config::our_ip_v6().as_ref() == Some(dst)
        || config::link_local_v6().as_ref() == Some(dst)
}

/// Send an IPv6 packet from an explicit source address (e.g. an ICMPv6 echo
/// reply sourced from the pinged address).
pub fn send_from(src: Ipv6Addr, dst: Ipv6Addr, next_header: u8, payload: &[u8]) {
    // Internal loopback: m3OS has no routed `lo`, so a packet addressed to `::1`
    // or one of our own addresses is fed straight back into the RX path rather
    // than put on the wire (B.1). This makes `ping6 ::1` and echo-to-self travel
    // the real `handle_icmpv6` request→reply path. A small depth guard prevents
    // any pathological self-send recursion.
    if is_self_address(&dst) {
        use core::sync::atomic::{AtomicU8, Ordering};
        static LOOPBACK_DEPTH: AtomicU8 = AtomicU8::new(0);
        if LOOPBACK_DEPTH.fetch_add(1, Ordering::Relaxed) < 4 {
            let our_mac = super::mac_address().unwrap_or([0; 6]);
            let ip_pkt = ipv6_core::build(src, dst, next_header, payload);
            handle_ipv6(&ip_pkt, our_mac);
        }
        LOOPBACK_DEPTH.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    let our_mac = match super::mac_address() {
        Some(m) => m,
        None => return,
    };
    let dst_mac = match next_hop_mac(&dst) {
        Some(m) => m,
        None => return, // NDP miss — solicitation queued, packet dropped
    };
    let ip_pkt = ipv6_core::build(src, dst, next_header, payload);
    let frame = ethernet::build(dst_mac, our_mac, eth_core::ETHERTYPE_IPV6, &ip_pkt);
    super::send_frame(&frame);
}

/// Dispatch a received IPv6 frame payload. `src_mac` is the Ethernet source,
/// used for passive NDP learning.
pub fn handle_ipv6(frame_payload: &[u8], src_mac: MacAddr) {
    let (header, payload) = match parse(frame_payload) {
        Some(v) => v,
        None => return,
    };

    // Passive neighbor learning (mirror of `arp::learn` on inbound IPv4).
    ndp::learn(header.src, src_mac);

    let (upper_proto, offset) = walk_ext_headers(header.next_header, payload);
    if offset > payload.len() {
        return;
    }
    let upper = &payload[offset..];

    match upper_proto {
        PROTO_ICMPV6 => {
            super::icmpv6::handle_icmpv6(&header, upper, src_mac);
        }
        PROTO_UDP => {
            handle_udp_v6(&header, upper);
        }
        PROTO_TCP => {
            super::tcp::handle_tcp_v6(&header, upper);
        }
        _ => {}
    }
}

/// UDP-over-IPv6 receive. The DHCPv6 client port (546) is routed to the kernel
/// DHCPv6 driver; everything else is delivered to bound AF_INET6 UDP sockets.
fn handle_udp_v6(header: &Ipv6Header, payload: &[u8]) {
    let (udp_hdr, udp_data) = match udp_core::parse(payload) {
        Some(v) => v,
        None => return,
    };
    if udp_hdr.dst_port == kernel_core::net::dhcpv6::DHCPV6_CLIENT_PORT {
        super::dhcpv6::on_udp(header.src, udp_data);
        return;
    }
    super::udp::handle_udp_v6(header.src, udp_hdr.src_port, udp_hdr.dst_port, udp_data);
}

/// Periodic IPv6 maintenance, driven once per `net_task` pass (alongside
/// `tcp_tick`). Forms the link-local address on first run, solicits a router,
/// and steps the DHCPv6 client.
pub fn v6_tick() {
    use core::sync::atomic::{AtomicBool, Ordering};
    static INITED: AtomicBool = AtomicBool::new(false);

    // Form the link-local address + kick off discovery once the MAC is known.
    if !INITED.load(Ordering::Relaxed)
        && let Some(_ll) = ndp::init_link_local()
    {
        INITED.store(true, Ordering::Relaxed);
        ndp::send_router_solicitation();
        log::info!("[ipv6] link-local configured; router solicitation sent");
    }

    super::dhcpv6::tick();
}
