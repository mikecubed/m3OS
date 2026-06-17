//! DHCP client driver (Phase 96 R4) — kernel glue around the pure
//! [`kernel_core::net::dhcp`] protocol/state-machine.
//!
//! The protocol module produces request bytes and consumes parsed replies; this
//! module supplies the transport: broadcast UDP (`0.0.0.0:68` → `255.255.255.255:67`,
//! Ethernet broadcast) for sending, and `udp::recv(68)` for receiving. It is
//! driven a step at a time by the `net_task` ([`tick`]) so it never blocks the
//! single network task while waiting for an OFFER/ACK.
//!
//! On a bound ACK it installs the lease via [`config::set_config`], so the
//! in-kernel stack switches from the default static `10.0.2.15` to the
//! DHCP-assigned address — which is what makes real networking work over a
//! passthrough NIC on a physical LAN (e.g. the Phase 96 `ure` dongle).
//!
//! Gated on [`remote::RemoteNic::is_registered`]: a pure virtio/SLIRP boot keeps
//! its exact static-IP behaviour (no DHCP), so existing network gates are
//! unaffected; only a RemoteNic boot (ure / e1000 / r8169) runs DHCP, and those
//! converge to the correct address (SLIRP serves `10.0.2.15`; a real LAN serves
//! its own).

use kernel_core::net::dhcp::{self, DHCP_CLIENT_PORT, DHCP_SERVER_PORT, DhcpAction, DhcpClient};
use kernel_core::net::ethernet::MAC_BROADCAST;
use kernel_core::types::MacAddr;

use crate::task::scheduler::IrqSafeMutex;

use super::arp::Ipv4Addr;
use super::{config, ethernet, ipv4, remote, udp};

/// `255.255.255.255` — the limited-broadcast destination for DHCP.
const BROADCAST_IP: Ipv4Addr = [255, 255, 255, 255];
/// `0.0.0.0` — the source address a client uses before it has a lease.
const UNSPEC_IP: Ipv4Addr = [0, 0, 0, 0];
/// `net_task` ticks between retransmits of an unanswered DISCOVER/REQUEST. The
/// task wakes on a ~200 ms idle deadline (and sooner under traffic), so ~10
/// ticks is roughly a 2 s retransmit floor.
const RETRANSMIT_TICKS: u32 = 10;

/// `net_task` ticks between network-state heartbeats (~3 s at the ~200 ms idle
/// wake cadence). Bare metal has no serial console or keyboard, so this pins the
/// DHCP state + RX-path counters to the bottom of the framebuffer scroll.
/// Minimum wall-clock gap (ms) between framebuffer heartbeats. Wall-clock, not
/// iteration count: `net_task` wakes once per inbound/outbound frame, so an
/// iteration-gated heartbeat would flood the (uncached, slow) bare-metal
/// framebuffer under load — each line costs ~hundreds of ms there, throttling
/// the very traffic it reports.
const HB_INTERVAL_MS: u64 = 3000;

struct DhcpDriver {
    client: DhcpClient,
    started: bool,
    bound: bool,
    ticks: u32,
    /// DHCP replies seen on UDP:68 (OFFER/ACK/NAK datagrams), for the heartbeat.
    offers: u32,
    /// Tick (ms) of the last framebuffer heartbeat — wall-clock throttle.
    hb_last_ms: u64,
}

static DRIVER: IrqSafeMutex<DhcpDriver> = IrqSafeMutex::new(DhcpDriver {
    client: DhcpClient::new(),
    started: false,
    bound: false,
    ticks: 0,
    offers: 0,
    hb_last_ms: 0,
});

/// Build + broadcast a DHCP message (UDP `0.0.0.0:68` → `255.255.255.255:67`,
/// Ethernet broadcast). DHCP permits a zero UDP checksum (which `udp::build`
/// emits), so no IP pseudo-header is needed.
fn send_broadcast(mac: MacAddr, msg: &[u8]) {
    let udp_pkt = kernel_core::net::udp::build(DHCP_CLIENT_PORT, DHCP_SERVER_PORT, msg);
    let ip_pkt = kernel_core::net::ipv4::build(UNSPEC_IP, BROADCAST_IP, ipv4::PROTO_UDP, &udp_pkt);
    let frame = ethernet::build(MAC_BROADCAST, mac, ethernet::ETHERTYPE_IPV4, &ip_pkt);
    super::send_frame(&frame);
}

/// Derive a transaction id from the NIC MAC (no RNG available here; the xid only
/// needs to be stable within a transaction and distinct across restarts).
fn derive_xid(mac: MacAddr, salt: u32) -> u32 {
    u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]) ^ 0x9e37_79b9 ^ salt
}

/// Advance the DHCP client one step. Called from `net_task`. No-op once bound or
/// until a RemoteNic is registered. Sends DISCOVER on the first call, drains
/// UDP:68 for OFFER/ACK, retransmits on timeout, and installs the lease on ACK.
pub fn tick() {
    // Only drive DHCP on a RemoteNic boot — pure virtio/SLIRP keeps its static IP.
    if !remote::RemoteNic::is_registered() {
        return;
    }
    let mut d = DRIVER.lock();

    // Network-state heartbeat (bare-metal localization; survives the scroll).
    // Logs even once bound so the leased IP stays visible. One line answers the
    // whole packet path: where inbound frames stop and whether we got a lease.
    let now_ms = crate::arch::x86_64::interrupts::tick_count();
    if now_ms.wrapping_sub(d.hb_last_ms) >= HB_INTERVAL_MS {
        d.hb_last_ms = now_ms;
        let ip = config::our_ip();
        let mac = super::mac_address().unwrap_or([0; 6]);
        let (rx_total, rx_arp, rx_ipv4) = super::dispatch::rx_counts();
        let (arp_for_us, arp_replies) = super::arp::responder_counts();
        let (echo_rx, echo_tx) = super::icmp::echo_counts();
        // Sink to BOTH serial (QEMU/log) and the framebuffer console — the
        // kernel `log::*` macros are serial-only, which is invisible on bare
        // metal, so this heartbeat would otherwise never reach the screen. The
        // `mac=` field is the MAC we present to DHCP/ARP; compare it to the
        // dongle's real (host-seen) MAC to explain a non-reserved lease.
        log::info!(
            "[net] HB ip={}.{}.{}.{} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} bound={} offers={} rx={} arp={} ipv4={} arp4us={} arprep={} echorx={} echotx={}",
            ip[0],
            ip[1],
            ip[2],
            ip[3],
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            d.bound,
            d.offers,
            rx_total,
            rx_arp,
            rx_ipv4,
            arp_for_us,
            arp_replies,
            echo_rx,
            echo_tx,
        );
        // Two SHORT framebuffer lines (the single long line wrapped to unreadable
        // tiny text on a real screen). Line 1 = identity, line 2 = packet-path
        // counters. `a4u`/`rep` = ARP requests for our IP / replies sent;
        // `erx`/`etx` = ICMP echo requests received / replies sent.
        crate::fb::write_fmt(format_args!(
            "[net] ip={}.{}.{}.{} bnd={} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n",
            ip[0], ip[1], ip[2], ip[3], d.bound, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        ));
        crate::fb::write_fmt(format_args!(
            "[net] rx={} arp={} ip4={} off={} a4u={} rep={} erx={} etx={}\n",
            rx_total, rx_arp, rx_ipv4, d.offers, arp_for_us, arp_replies, echo_rx, echo_tx,
        ));
        // Line 3 = kernel TX-drop attribution (bare-metal visible; the matching
        // log::warn lines are serial-only). `rf` = dropped, TX queue full;
        // `rs` = dropped, restart-suspected latched; `si` = interrupted sends
        // recovered (re-queued); `susp` = restart latch currently set. A wedged
        // link shows here exactly where outbound frames are being lost.
        let (tx_rf, tx_rs, tx_si, tx_susp) = super::remote::tx_drop_counts();
        crate::fb::write_fmt(format_args!(
            "[net] txdrop rf={} rs={} si={} susp={}\n",
            tx_rf, tx_rs, tx_si, tx_susp as u8,
        ));
    }

    if d.bound {
        return;
    }
    let mac = match super::mac_address() {
        Some(m) => m,
        None => return,
    };

    if !d.started {
        udp::bind(DHCP_CLIENT_PORT);
        let discover = d.client.start(mac, derive_xid(mac, 0));
        send_broadcast(mac, &discover);
        d.started = true;
        d.ticks = 0;
        log::info!("[dhcp] DISCOVER sent");
        return;
    }

    // Drain any replies queued on the client port.
    while let Some(dg) = udp::recv(DHCP_CLIENT_PORT) {
        d.offers = d.offers.wrapping_add(1);
        let Some(reply) = dhcp::parse_reply(&dg.data) else {
            continue;
        };
        match d.client.on_reply(reply) {
            DhcpAction::SendRequest(bytes) => {
                send_broadcast(mac, &bytes);
                d.ticks = 0;
                log::info!("[dhcp] OFFER received; REQUEST sent");
            }
            DhcpAction::Bound(cfg) => {
                config::set_config(cfg.ip, cfg.mask, cfg.gateway);
                d.bound = true;
                // Gratuitous ARP: announce our IP→MAC to the LAN so peers cache
                // it without first having to ARP us. A request whose target IS
                // our own address is the standard announcement form. Helps a
                // host reach us immediately after the lease (and refreshes a
                // stale entry from a prior boot / the dongle's other MAC).
                super::arp::send_request(cfg.ip);
                log::info!(
                    "[dhcp] bound ip={}.{}.{}.{}/{}.{}.{}.{} gw={}.{}.{}.{}",
                    cfg.ip[0],
                    cfg.ip[1],
                    cfg.ip[2],
                    cfg.ip[3],
                    cfg.mask[0],
                    cfg.mask[1],
                    cfg.mask[2],
                    cfg.mask[3],
                    cfg.gateway[0],
                    cfg.gateway[1],
                    cfg.gateway[2],
                    cfg.gateway[3],
                );
                return;
            }
            DhcpAction::Nak => {
                log::info!("[dhcp] NAK; restarting");
                d.client.reset();
                d.started = false;
                return;
            }
            DhcpAction::Ignore => {}
        }
    }

    // Retransmit on timeout — restart the transaction from DISCOVER with a fresh
    // xid (a valid recovery and simpler than tracking per-state resend bytes).
    d.ticks = d.ticks.wrapping_add(1);
    if d.ticks >= RETRANSMIT_TICKS {
        d.ticks = 0;
        let discover = d.client.start(mac, derive_xid(mac, 1));
        send_broadcast(mac, &discover);
        log::info!("[dhcp] retransmit DISCOVER");
    }
}
