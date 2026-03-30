//! IPv4 packet parsing, construction, and sending — pure logic re-exported from kernel-core.

use super::arp::{self, Ipv4Addr};
use super::config;
use super::ethernet;
use super::virtio_net;

#[allow(unused_imports)]
pub use kernel_core::net::ipv4::{
    Ipv4Header, PROTO_ICMP, PROTO_TCP, PROTO_UDP, build, checksum, parse,
};

/// Send an IPv4 packet to the given destination.
pub fn send(dst_ip: Ipv4Addr, protocol: u8, payload: &[u8]) {
    let our_mac = match virtio_net::mac_address() {
        Some(m) => m,
        None => return,
    };
    let our_ip = config::our_ip();

    let next_hop = if config::is_local(dst_ip) {
        dst_ip
    } else {
        config::gateway_ip()
    };

    let dst_mac = match arp::resolve(next_hop) {
        Some(mac) => mac,
        None => {
            arp::send_request(next_hop);
            return;
        }
    };

    let ip_pkt = build(our_ip, dst_ip, protocol, payload);
    let frame = ethernet::build(dst_mac, our_mac, ethernet::ETHERTYPE_IPV4, &ip_pkt);
    virtio_net::send_frame(&frame);
}

/// Dispatch a received IPv4 packet to the appropriate protocol handler.
pub fn handle_ipv4(header: &Ipv4Header, payload: &[u8]) {
    match header.protocol {
        PROTO_ICMP => {
            super::icmp::handle_icmp(header, payload);
        }
        PROTO_UDP => {
            super::udp::handle_udp(header, payload);
        }
        PROTO_TCP => {
            super::tcp::handle_tcp(header, payload);
        }
        _ => {}
    }
}
