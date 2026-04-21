//! Ethernet frame dispatch (P16-T016).
//!
//! Receives frames from the selected NIC driver and dispatches them based on
//! EtherType to the appropriate protocol handler.
//!
//! Phase 55b E.5: the in-kernel e1000 driver has been removed. The ring-3
//! e1000 driver (`userspace/drivers/e1000`) delivers RX frames via
//! `RemoteNic::inject_rx_frame` which calls [`process_rx_frames`] directly.
//! `process_rx` now only drains virtio-net; RemoteNic frames arrive
//! out-of-band through the IPC endpoint and are already dispatched before
//! `process_rx` is called.

use super::arp;
use super::ethernet;
use super::ipv4;
use super::virtio_net;

/// Dispatch a single raw Ethernet frame into the protocol stack.
///
/// This is the shared entry point for all NIC paths — the in-kernel virtio-net
/// driver and the ring-3 e1000 driver via `RemoteNic::inject_rx_frame`. The
/// frame must be a complete Ethernet frame starting at the 6-byte destination
/// MAC.
///
/// Returns `true` when the frame parsed as a valid Ethernet frame and was
/// routed to an EtherType arm (including the unknown-EtherType drop path);
/// returns `false` when the Ethernet header itself was malformed and the
/// frame was rejected before any dispatch. Callers that surface an
/// "injected" count (e.g. `RemoteNic::inject_rx_frame`) use this to avoid
/// reporting rejected frames as successfully delivered.
pub fn process_rx_frames(raw: &[u8]) -> bool {
    let frame = match ethernet::parse(raw) {
        Some(f) => f,
        None => return false,
    };
    match frame.ethertype {
        ethernet::ETHERTYPE_ARP => {
            if let Some(pkt) = arp::parse(&frame.payload) {
                arp::handle_arp(&pkt);
            }
        }
        ethernet::ETHERTYPE_IPV4 => {
            if let Some((header, payload)) = ipv4::parse(&frame.payload) {
                // Passive ARP learning: populate the cache with
                // (sender_ip, sender_mac) before dispatching. Without this,
                // the first inbound TCP SYN dropped the reply-side SYN-ACK
                // because arp::resolve(gateway) missed the cache; see
                // arp::learn for the full rationale.
                arp::learn(header.src, frame.src);
                ipv4::handle_ipv4(&header, payload);
            }
        }
        _ => {
            // Unknown EtherType — drop silently.
        }
    }
    true
}

/// Process all pending received frames from the in-kernel VirtIO-net driver.
///
/// Called from the network processing task whenever a driver IRQ fires or on
/// a periodic poll. RX frames from the ring-3 e1000 driver
/// (`userspace/drivers/e1000`) are injected directly via
/// `RemoteNic::inject_rx_frame` → `process_rx_frames` and do not go through
/// this function.
pub fn process_rx() {
    for raw in &virtio_net::recv_frames() {
        // virtio-net callers do not track per-frame dispatch outcome; the
        // return value is used by `RemoteNic::inject_rx_frame` only.
        let _ = process_rx_frames(raw);
    }
}
