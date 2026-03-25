//! Ethernet frame dispatch (P16-T016).
//!
//! Receives frames from the virtio-net driver and dispatches them based on
//! EtherType to the appropriate protocol handler.

use super::arp;
use super::ethernet;
use super::ipv4;
use super::virtio_net;

/// Process all pending received frames.
///
/// Called from the network processing task whenever the virtio-net IRQ fires
/// or on a periodic poll.
pub fn process_rx() {
    let frames = virtio_net::recv_frames();

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
