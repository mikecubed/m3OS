//! Static network configuration (P16-T029).
//!
//! QEMU user-mode networking defaults:
//! - Guest IP: 10.0.2.15/24
//! - Gateway:  10.0.2.2
//! - DNS:      10.0.2.3
//!
//! Phase 91 adds dual-stack IPv6 state: a link-local address derived from the
//! NIC MAC at init, plus a SLAAC/DHCPv6-learned global address, prefix, and
//! default gateway (all runtime-mutable, written once per RA/lease), and a
//! runtime DNS-server list (IPv4 + IPv6) the resolver path consults.

use super::arp::Ipv4Addr;
use crate::task::scheduler::IrqSafeMutex;
use kernel_core::types::Ipv6Addr;

/// Our static IPv4 address.
pub fn our_ip() -> Ipv4Addr {
    [10, 0, 2, 15]
}

/// Subnet mask (/24).
pub fn subnet_mask() -> Ipv4Addr {
    [255, 255, 255, 0]
}

/// Default gateway.
pub fn gateway_ip() -> Ipv4Addr {
    [10, 0, 2, 2]
}

/// Check if `ip` is on the local subnet.
pub fn is_local(ip: Ipv4Addr) -> bool {
    let mask = subnet_mask();
    let our = our_ip();
    for i in 0..4 {
        if (ip[i] & mask[i]) != (our[i] & mask[i]) {
            return false;
        }
    }
    true
}

// ===========================================================================
// Phase 91 — IPv6 dual-stack state (C.1) + runtime DNS storage (D.1)
// ===========================================================================

/// Maximum DNS servers tracked at once (IPv4 + IPv6 mixed).
const MAX_DNS: usize = 4;

/// A DNS server learned at runtime (DHCPv4/DHCPv6/RDNSS) or the static default.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DnsServer {
    V4(Ipv4Addr),
    V6(Ipv6Addr),
}

struct V6State {
    /// `fe80::`-prefix link-local address derived from the NIC MAC at init.
    /// `None` until the MAC is known.
    link_local: Option<Ipv6Addr>,
    /// SLAAC/DHCPv6-assigned global unicast address, if any.
    global: Option<Ipv6Addr>,
    /// On-link prefix learned from a Router Advertisement (`/prefix_len`).
    prefix: Option<Ipv6Addr>,
    prefix_len: u8,
    /// IPv6 default gateway (the RA source link-local), if any.
    gateway: Option<Ipv6Addr>,
    /// Runtime DNS servers (the static IPv4 default is seeded at boot).
    dns: [Option<DnsServer>; MAX_DNS],
}

static V6: IrqSafeMutex<V6State> = IrqSafeMutex::new(V6State {
    link_local: None,
    global: None,
    prefix: None,
    prefix_len: 0,
    gateway: None,
    // Seed the static QEMU/SLIRP IPv4 nameserver so the resolver always has a
    // default; DHCPv6/RDNSS overwrites/extends this when a v6 server arrives.
    dns: [Some(DnsServer::V4([10, 0, 2, 3])), None, None, None],
});

/// Install the link-local address (called once the NIC MAC is known).
pub fn set_link_local_v6(addr: Ipv6Addr) {
    V6.lock().link_local = Some(addr);
}

/// The link-local `fe80::` address, or `None` before NIC init.
pub fn link_local_v6() -> Option<Ipv6Addr> {
    V6.lock().link_local
}

/// The preferred IPv6 source address: the global SLAAC/DHCPv6 address if
/// configured, else the link-local address.
pub fn our_ip_v6() -> Option<Ipv6Addr> {
    let s = V6.lock();
    s.global.or(s.link_local)
}

/// The configured global IPv6 address (SLAAC/DHCPv6), if any. Distinct from
/// [`our_ip_v6`] — RFC 6724 source selection needs to know whether a *global*
/// source exists, not just a link-local one.
pub fn global_ip_v6() -> Option<Ipv6Addr> {
    V6.lock().global
}

/// The IPv6 default gateway (RA source), if a default route is installed.
pub fn gateway_ip_v6() -> Option<Ipv6Addr> {
    V6.lock().gateway
}

/// Install a SLAAC/DHCPv6 global address + on-link prefix + default gateway.
/// A `gateway` of `None` (e.g. a zero-router-lifetime RA) installs no default
/// route. Phase 91 C.1.
pub fn set_config_v6(
    global: Ipv6Addr,
    prefix: Ipv6Addr,
    prefix_len: u8,
    gateway: Option<Ipv6Addr>,
) {
    let mut s = V6.lock();
    s.global = Some(global);
    s.prefix = Some(prefix);
    s.prefix_len = prefix_len;
    if gateway.is_some() {
        s.gateway = gateway;
    }
}

/// Record only the on-link prefix + default gateway from an RA (used when the M
/// flag requests DHCPv6 for the address but the route still comes from the RA).
pub fn set_route_v6(prefix: Ipv6Addr, prefix_len: u8, gateway: Option<Ipv6Addr>) {
    let mut s = V6.lock();
    s.prefix = Some(prefix);
    s.prefix_len = prefix_len;
    if gateway.is_some() {
        s.gateway = gateway;
    }
}

/// Is `addr` on-link for the learned prefix? Link-local and multicast are always
/// treated as on-link; otherwise compare against the learned `/prefix_len`.
pub fn is_local_v6(addr: &Ipv6Addr) -> bool {
    use kernel_core::types::{ipv6_is_link_local, ipv6_is_multicast};
    if ipv6_is_link_local(addr) || ipv6_is_multicast(addr) {
        return true;
    }
    let s = V6.lock();
    match (s.prefix, s.prefix_len) {
        (Some(prefix), len) if len > 0 => prefix_matches(&prefix, addr, len),
        _ => false,
    }
}

/// Compare the first `len` bits of two addresses.
fn prefix_matches(a: &Ipv6Addr, b: &Ipv6Addr, len: u8) -> bool {
    let full_bytes = (len / 8) as usize;
    if a[..full_bytes] != b[..full_bytes] {
        return false;
    }
    let rem = len % 8;
    if rem != 0 {
        let mask = 0xffu8 << (8 - rem);
        if (a[full_bytes] & mask) != (b[full_bytes] & mask) {
            return false;
        }
    }
    true
}

/// Add/replace a learned DNS server (DHCPv4/DHCPv6/RDNSS). v6 servers are added
/// without evicting the static v4 default; duplicates are ignored.
pub fn add_dns_server(server: DnsServer) {
    let mut s = V6.lock();
    for slot in s.dns.iter().flatten() {
        if *slot == server {
            return; // already present
        }
    }
    for slot in s.dns.iter_mut() {
        if slot.is_none() {
            *slot = Some(server);
            return;
        }
    }
    // Full — replace the last slot (LRU-ish; learned servers are few).
    s.dns[MAX_DNS - 1] = Some(server);
}

/// Snapshot the current DNS server list (D.1). Callers copy out.
pub fn dns_servers() -> [Option<DnsServer>; MAX_DNS] {
    V6.lock().dns
}
