//! DHCPv6 client driver (Phase 91 C.2) — kernel glue around the pure
//! [`kernel_core::net::dhcpv6`] state machine, structurally mirroring the
//! DHCPv4 driver from PR #237.
//!
//! Unlike DHCPv4 (which is RemoteNic-gated because SLIRP serves IPv4 statically),
//! DHCPv6 runs on the ordinary virtio/SLIRP path: IPv6 has no static-address
//! fallback. Once a link-local address exists, the driver issues a **stateless**
//! Information-Request and installs any learned DNS server via
//! [`config::add_dns_server`].
//!
//! **Not CI-deterministic under SLIRP.** Packet capture showed QEMU 8.2.2's
//! libslirp (`ipv6=on`) answers NDP NS/NA but runs **no DHCPv6 server** — the
//! Information-Request goes out correctly formed but gets no reply. So the
//! DHCPv6 DNS path is validated by the host-tested `kernel_core::net::dhcpv6`
//! state machine and live-validated only behind the opt-in `M3OS_IPV6_LIVE` arm
//! (a TAP bridged to a real router; `M3OS_IPV6_DHCPV6=1` additionally requires
//! the router to run a DHCPv6 server).
//!
//! The full **stateful** four-message Solicit/Advertise/Request/Reply address
//! lease also lives in that host-tested state machine and is exercised by the
//! same opt-in `M3OS_IPV6_LIVE` arm; auto-driving it from a managed (M-flag) RA
//! is a tracked follow-up.

use kernel_core::net::dhcpv6::{
    self, ALL_DHCP_SERVERS, DHCPV6_SERVER_PORT, Dhcpv6Action, Dhcpv6Client,
};
use kernel_core::types::{Ipv6Addr, MacAddr};

use crate::task::scheduler::IrqSafeMutex;

use super::config::{self, DnsServer};
use super::udp;

/// `net_task` ticks between Information-Request retransmits (~2 s at the ~200 ms
/// idle wake cadence).
const RETRANSMIT_TICKS: u32 = 10;

struct Dhcpv6Driver {
    client: Dhcpv6Client,
    started: bool,
    bound: bool,
    ticks: u32,
}

static DRIVER: IrqSafeMutex<Dhcpv6Driver> = IrqSafeMutex::new(Dhcpv6Driver {
    client: Dhcpv6Client::new(),
    started: false,
    bound: false,
    ticks: 0,
});

/// Derive a 3-byte DHCPv6 transaction id from the NIC MAC. Per RFC 8415 §15 the
/// transaction id MUST stay unchanged across retransmissions of the same
/// message, so the initial Information-Request and every retransmit reuse this
/// single value (matching `Dhcpv6Client`'s "replayed in all subsequent
/// messages" contract). This is a transaction identifier (RFC 8415 §8), not a
/// cryptographic value — no RNG is needed; it only has to be stable within a
/// single transaction.
fn derive_xid(mac: MacAddr) -> [u8; 3] {
    [mac[3], mac[4], mac[5]]
}

/// Send a DHCPv6 client message to `[ff02::1:2]:547` (UDP 546 → 547).
fn send_to_servers(src: Ipv6Addr, msg: &[u8]) {
    udp::send_v6(
        src,
        ALL_DHCP_SERVERS,
        DHCPV6_SERVER_PORT,
        dhcpv6::DHCPV6_CLIENT_PORT,
        msg,
    );
}

/// Advance the DHCPv6 client one step. Called from `ipv6::v6_tick`. No-op until a
/// link-local address exists or once bound. Issues a stateless
/// Information-Request, drains nothing here (replies arrive via [`on_udp`]), and
/// retransmits on timeout.
pub fn tick() {
    let src = match config::link_local_v6() {
        Some(a) => a,
        None => return, // wait until link-local is configured
    };
    let mac = match super::mac_address() {
        Some(m) => m,
        None => return,
    };
    let mut d = DRIVER.lock();
    if d.bound {
        return;
    }
    if !d.started {
        let msg = d.client.start_information_request(mac, derive_xid(mac));
        d.started = true;
        d.ticks = 0;
        drop(d);
        send_to_servers(src, &msg);
        log::info!("[dhcpv6] Information-Request sent");
        return;
    }
    d.ticks = d.ticks.wrapping_add(1);
    if d.ticks >= RETRANSMIT_TICKS {
        d.ticks = 0;
        // RFC 8415 §15: a retransmission keeps the SAME transaction id as the
        // initial Information-Request — `derive_xid` is deterministic per MAC.
        let msg = d.client.start_information_request(mac, derive_xid(mac));
        drop(d);
        send_to_servers(src, &msg);
        log::info!("[dhcpv6] retransmit Information-Request");
    }
}

/// Feed an inbound UDP datagram received on the client port (546) to the state
/// machine. Routed here from `ipv6::handle_ipv6`. The datagram's source address
/// is the server's; the client's own outgoing messages are sourced from our
/// link-local (see [`config::link_local_v6`]), never from the inbound source.
pub fn on_udp(data: &[u8]) {
    let reply = match dhcpv6::parse_reply(data) {
        Some(r) => r,
        None => return,
    };
    let mut d = DRIVER.lock();
    match d.client.on_reply(reply) {
        Dhcpv6Action::SendRequest(bytes) => {
            drop(d);
            // Source the REQUEST from our own link-local address, not the
            // server's (the inbound datagram source). The DHCPv6 flow only
            // starts once a link-local exists, so this is normally present.
            match config::link_local_v6() {
                Some(local) => {
                    send_to_servers(local, &bytes);
                    log::info!("[dhcpv6] Advertise received; Request sent");
                }
                None => {
                    log::warn!("[dhcpv6] Advertise received but no link-local; Request not sent");
                }
            }
        }
        Dhcpv6Action::Bound(cfg) => {
            d.bound = true;
            drop(d);
            // Install the learned address (stateful) + DNS servers.
            if let Some(addr) = cfg.address {
                // Stateful lease: install as the global address (prefix/gateway
                // come from the RA). Use a /64 assumption for the on-link prefix.
                let mut prefix = addr;
                prefix[8..].fill(0);
                config::set_config_v6(addr, prefix, 64, None);
            }
            for dns in &cfg.dns_servers {
                config::add_dns_server(DnsServer::V6(*dns));
            }
            log::info!(
                "[dhcpv6] bound: {} dns server(s){}",
                cfg.dns_servers.len(),
                if cfg.address.is_some() {
                    " + address"
                } else {
                    " (stateless)"
                },
            );
        }
        Dhcpv6Action::Ignore => {}
    }
}
