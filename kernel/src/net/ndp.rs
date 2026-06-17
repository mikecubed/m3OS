//! Neighbor Discovery Protocol (RFC 4861) — IPv6's ARP. Pure NS/NA/RS/RA
//! parse/build live in `kernel_core::net::ndp`; the stateful neighbor cache and
//! the send/handle glue (plus RA-driven SLAAC, C.1) live here, structurally
//! mirroring `arp.rs`.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::task::scheduler::IrqSafeMutex;

use kernel_core::net::ndp as ndp_core;
use kernel_core::net::{ethernet as eth_core, icmpv6 as icmpv6_core, ipv6 as ipv6_core};
use kernel_core::types::{Ipv6Addr, MacAddr, slaac_address, solicited_node_multicast};

use super::ethernet;

// ===========================================================================
// Neighbor cache (mirror of the ARP cache)
// ===========================================================================

const NDP_CACHE_SIZE: usize = 16;

struct NdpEntry {
    ip: Ipv6Addr,
    mac: MacAddr,
    tick: u64,
}

struct NdpCache {
    entries: [Option<NdpEntry>; NDP_CACHE_SIZE],
}

impl NdpCache {
    const fn new() -> Self {
        Self {
            entries: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None,
            ],
        }
    }

    fn lookup(&self, ip: &Ipv6Addr) -> Option<MacAddr> {
        for e in self.entries.iter().flatten() {
            if e.ip == *ip {
                return Some(e.mac);
            }
        }
        None
    }

    fn insert(&mut self, ip: Ipv6Addr, mac: MacAddr) {
        let tick = crate::arch::x86_64::interrupts::tick_count();
        for e in self.entries.iter_mut().flatten() {
            if e.ip == ip {
                e.mac = mac;
                e.tick = tick;
                return;
            }
        }
        for entry in &mut self.entries {
            if entry.is_none() {
                *entry = Some(NdpEntry { ip, mac, tick });
                return;
            }
        }
        let mut oldest_idx = 0;
        let mut oldest_tick = u64::MAX;
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(e) = entry
                && e.tick < oldest_tick
            {
                oldest_tick = e.tick;
                oldest_idx = i;
            }
        }
        self.entries[oldest_idx] = Some(NdpEntry { ip, mac, tick });
    }
}

static NDP_CACHE: IrqSafeMutex<NdpCache> = IrqSafeMutex::new(NdpCache::new());

/// NS messages received that targeted one of our addresses (heartbeat/diag).
static NDP_REQ_FOR_US: AtomicU32 = AtomicU32::new(0);
/// NA replies we have sent in response.
static NDP_REPLIES: AtomicU32 = AtomicU32::new(0);

/// Snapshot `(neighbor solicitations for us, advertisements sent)`.
#[allow(dead_code)]
pub fn responder_counts() -> (u32, u32) {
    (
        NDP_REQ_FOR_US.load(Ordering::Relaxed),
        NDP_REPLIES.load(Ordering::Relaxed),
    )
}

/// Look up a neighbor's MAC in the cache (non-blocking).
pub fn resolve(target: &Ipv6Addr) -> Option<MacAddr> {
    NDP_CACHE.lock().lookup(target)
}

/// Passively learn `(ip, mac)` from inbound traffic — mirrors `arp::learn` to
/// avoid the first-reply drop. Skips null/multicast MACs.
pub fn learn(ip: Ipv6Addr, mac: MacAddr) {
    use kernel_core::types::{ipv6_is_multicast, ipv6_is_unspecified};
    if mac == [0; 6] || ipv6_is_unspecified(&ip) || ipv6_is_multicast(&ip) {
        return;
    }
    NDP_CACHE.lock().insert(ip, mac);
}

// ===========================================================================
// Frame helpers
// ===========================================================================

/// Map an IPv6 multicast address to its Ethernet destination MAC (RFC 2464 §7):
/// `33:33:` followed by the low 32 bits of the address.
fn multicast_mac(addr: &Ipv6Addr) -> MacAddr {
    [0x33, 0x33, addr[12], addr[13], addr[14], addr[15]]
}

/// Build + transmit an ICMPv6 message as an Ethernet frame to a known dst MAC.
fn emit(src: Ipv6Addr, dst_ip: Ipv6Addr, dst_mac: MacAddr, our_mac: MacAddr, icmpv6_msg: &[u8]) {
    let ip_pkt = ipv6_core::build(src, dst_ip, ipv6_core::PROTO_ICMPV6, icmpv6_msg);
    let frame = ethernet::build(dst_mac, our_mac, eth_core::ETHERTYPE_IPV6, &ip_pkt);
    super::send_frame(&frame);
}

/// True if `addr` is one of the addresses this host has assigned to itself.
fn is_our_address(addr: &Ipv6Addr) -> bool {
    super::config::link_local_v6().as_ref() == Some(addr)
        || super::config::global_ip_v6().as_ref() == Some(addr)
}

// ===========================================================================
// Send paths
// ===========================================================================

/// Send a Neighbor Solicitation to resolve `target` (mirrors `arp::send_request`).
/// The destination is `target`'s solicited-node multicast address.
pub fn send_solicitation(target: &Ipv6Addr) {
    let our_mac = match super::mac_address() {
        Some(m) => m,
        None => return,
    };
    let src = match super::config::link_local_v6() {
        Some(a) => a,
        None => return, // no source address yet
    };
    let snm = solicited_node_multicast(target);
    let msg = ndp_core::build_neighbor_solicitation(*target, our_mac, src, snm);
    emit(src, snm, multicast_mac(&snm), our_mac, &msg);
}

/// Send a Router Solicitation to the all-routers multicast `ff02::2` so a router
/// answers with an RA immediately rather than waiting for the periodic one.
pub fn send_router_solicitation() {
    let our_mac = match super::mac_address() {
        Some(m) => m,
        None => return,
    };
    let src = match super::config::link_local_v6() {
        Some(a) => a,
        None => return,
    };
    const ALL_ROUTERS: Ipv6Addr = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02];
    let msg = ndp_core::build_router_solicitation(our_mac, src, ALL_ROUTERS);
    emit(src, ALL_ROUTERS, multicast_mac(&ALL_ROUTERS), our_mac, &msg);
}

// ===========================================================================
// Receive handlers (dispatched from icmpv6::handle_icmpv6)
// ===========================================================================

/// Handle an inbound Neighbor Solicitation: if it targets one of our addresses,
/// reply with a solicited+override Neighbor Advertisement.
pub fn handle_neighbor_solicitation(src_ip: &Ipv6Addr, icmpv6_msg: &[u8]) {
    let ns = match ndp_core::parse_neighbor_solicitation(icmpv6_msg) {
        Some(n) => n,
        None => return,
    };
    if let Some(mac) = ns.src_lladdr {
        learn(*src_ip, mac);
    }
    if !is_our_address(&ns.target) {
        return;
    }
    NDP_REQ_FOR_US.fetch_add(1, Ordering::Relaxed);
    let our_mac = match super::mac_address() {
        Some(m) => m,
        None => return,
    };
    // Reply from the solicited target address straight back to the soliciter.
    let dst_mac = ns
        .src_lladdr
        .or_else(|| resolve(src_ip))
        .unwrap_or([0xff; 6]);
    let na =
        ndp_core::build_neighbor_advertisement(ns.target, our_mac, true, true, ns.target, *src_ip);
    emit(ns.target, *src_ip, dst_mac, our_mac, &na);
    NDP_REPLIES.fetch_add(1, Ordering::Relaxed);
}

/// Handle an inbound Neighbor Advertisement: learn `(target, target_lladdr)` so
/// a pending `ipv6::send` to that neighbor can resolve.
pub fn handle_neighbor_advertisement(icmpv6_msg: &[u8]) {
    let na = match ndp_core::parse_neighbor_advertisement(icmpv6_msg) {
        Some(n) => n,
        None => return,
    };
    if let Some(mac) = na.target_lladdr {
        learn(na.target, mac);
    }
}

/// Handle an inbound Router Advertisement (B.3 + C.1 SLAAC): install the on-link
/// prefix + default gateway, form a global SLAAC address, and record any RDNSS
/// DNS servers. A zero router-lifetime RA installs no default route.
pub fn handle_router_advertisement(src_ip: &Ipv6Addr, src_mac: MacAddr, icmpv6_msg: &[u8]) {
    let ra = match ndp_core::parse_router_advertisement(icmpv6_msg) {
        Some(r) => r,
        None => return,
    };
    // Learn the router's link-local -> MAC so we can route through it.
    learn(*src_ip, src_mac);

    // The RA source is the default gateway, unless router_lifetime == 0.
    let gateway = if ra.router_lifetime > 0 {
        Some(*src_ip)
    } else {
        None
    };

    if let Some(pi) = ra.prefix
        && pi.on_link
    {
        let our_mac = super::mac_address().unwrap_or([0; 6]);
        if pi.autonomous && !ra.managed {
            // SLAAC: form a global address from the prefix + our EUI-64.
            let global = slaac_address(&pi.prefix, our_mac);
            super::config::set_config_v6(global, pi.prefix, pi.prefix_length, gateway);
            log::info!(
                "[ndp] SLAAC global {:x}:{:x}::/{} gw={}",
                u16::from_be_bytes([global[0], global[1]]),
                u16::from_be_bytes([global[2], global[3]]),
                pi.prefix_length,
                gateway.is_some(),
            );
        } else {
            // M flag set (managed) — DHCPv6 supplies the address; keep the route.
            super::config::set_route_v6(pi.prefix, pi.prefix_length, gateway);
        }
    }

    // RDNSS (RFC 8106): record any advertised DNS servers.
    for dns in ra.rdnss {
        super::config::add_dns_server(super::config::DnsServer::V6(dns));
    }
}

/// Build the link-local address from the NIC MAC. Idempotent — re-derives the
/// same address. Returns the address if a MAC is available.
pub fn init_link_local() -> Option<Ipv6Addr> {
    let mac = super::mac_address()?;
    let ll = kernel_core::types::link_local_from_mac(mac);
    super::config::set_link_local_v6(ll);
    Some(ll)
}

// Re-export for callers that build/parse ICMPv6 messages alongside NDP.
#[allow(unused_imports)]
pub use icmpv6_core::{ICMPV6_NEIGHBOR_ADVERTISEMENT, ICMPV6_NEIGHBOR_SOLICITATION};
