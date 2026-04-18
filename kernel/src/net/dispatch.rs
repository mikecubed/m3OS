//! Ethernet frame dispatch (P16-T016).
//!
//! Receives frames from the selected NIC driver and dispatches them based on
//! EtherType to the appropriate protocol handler.  Phase 55 E.4 added the
//! e1000 path alongside the original virtio-net one — both funnel through
//! the same ARP / IPv4 handlers so protocol state is driver-agnostic.

use super::arp;
use super::e1000;
use super::ethernet;
use super::ipv4;
use super::virtio_net;

/// Process all pending received frames.
///
/// Called from the network processing task whenever a driver IRQ fires or on
/// a periodic poll.  Drains every initialized NIC in turn; on typical QEMU
/// configurations only one of `e1000` / `virtio-net` is present.
pub fn process_rx() {
    // Drain e1000 first when present; the driver's own IRQ path woke the
    // task.  Virtio-net's `recv_frames` is cheap when idle (returns an empty
    // Vec) so calling both in sequence is fine.
    let mut frames = e1000::e1000_receive_packets();
    frames.extend(virtio_net::recv_frames());

    for raw in &frames {
        let frame = match ethernet::parse(raw) {
            Some(f) => f,
            None => continue,
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
}
