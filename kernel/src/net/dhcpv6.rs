//! DHCPv6 client driver (Phase 91 C.2) — kernel glue around the pure
//! [`kernel_core::net::dhcpv6`] state machine, structurally mirroring the
//! DHCPv4 driver from PR #237.
//!
//! Unlike DHCPv4 (which is RemoteNic-gated because SLIRP serves IPv4 statically),
//! DHCPv6 runs on the ordinary virtio/SLIRP path: IPv6 has no static-address
//! fallback, and the CI-deterministic gate boots QEMU with `ipv6=on`, which
//! answers a **stateless** Information-Request with a DNS server option. The
//! driver issues that Information-Request once a link-local address exists and
//! installs the learned DNS server via [`config::add_dns_server`].
//!
//! The full **stateful** four-message Solicit/Advertise/Request/Reply address
//! lease lives in the host-tested `kernel_core::net::dhcpv6` state machine and
//! is exercised by the opt-in `M3OS_IPV6_NET` arm; auto-driving it from a
//! managed (M-flag) RA is a tracked follow-up.

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

/// Derive a 3-byte transaction id from the NIC MAC (no RNG here; it only needs
/// to be stable within a transaction).
fn derive_xid(mac: MacAddr, salt: u8) -> [u8; 3] {
    [mac[3] ^ salt, mac[4], mac[5]]
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
        let msg = d.client.start_information_request(mac, derive_xid(mac, 0));
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
        let msg = d.client.start_information_request(mac, derive_xid(mac, 1));
        drop(d);
        send_to_servers(src, &msg);
        log::info!("[dhcpv6] retransmit Information-Request");
    }
}

/// Feed an inbound UDP datagram received on the client port (546) to the state
/// machine. Routed here from `ipv6::handle_ipv6`.
pub fn on_udp(src: Ipv6Addr, data: &[u8]) {
    let reply = match dhcpv6::parse_reply(data) {
        Some(r) => r,
        None => return,
    };
    let mut d = DRIVER.lock();
    match d.client.on_reply(reply) {
        Dhcpv6Action::SendRequest(bytes) => {
            drop(d);
            send_to_servers(src, &bytes);
            log::info!("[dhcpv6] Advertise received; Request sent");
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
