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
pub fn process_rx_frames(raw: &[u8]) {
    let frame = match ethernet::parse(raw) {
        Some(f) => f,
        None => return,
    };
    match frame.ethertype {
        ethernet::ETHERTYPE_ARP => {
            if let Some(pkt) = arp::parse(&frame.payload) {
                arp::handle_arp(&pkt);
            }
        }
        ethernet::ETHERTYPE_IPV4 => {
            if let Some((header, payload)) = ipv4::parse(&frame.payload) {
                ipv4::handle_ipv4(&header, payload);
            }
        }
        _ => {
            // Unknown EtherType — drop silently.
        }
    }
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
        process_rx_frames(raw);
    }
}
