//! UDP datagram send/receive — pure logic re-exported from kernel-core.

use spin::Mutex;

use super::arp::Ipv4Addr;
use super::ipv4::{self, Ipv4Header};

use kernel_core::net::udp::{UdpBindings, build, parse};
#[allow(unused_imports)]
pub use kernel_core::net::udp::{UdpDatagram, UdpHeader};

static UDP_BINDINGS: Mutex<UdpBindings> = Mutex::new(UdpBindings::new());

/// Bind a local UDP port for receiving datagrams.
pub fn bind(port: u16) -> bool {
    UDP_BINDINGS.lock().bind(port)
}

/// Unbind a UDP port.
pub fn unbind(port: u16) {
    UDP_BINDINGS.lock().unbind(port);
}

/// Send a UDP datagram.
pub fn send(dst_ip: Ipv4Addr, dst_port: u16, src_port: u16, data: &[u8]) {
    let udp_pkt = build(src_port, dst_port, data);
    ipv4::send(dst_ip, ipv4::PROTO_UDP, &udp_pkt);
}

/// Try to dequeue a received UDP datagram from a bound port.
pub fn recv(port: u16) -> Option<UdpDatagram> {
    UDP_BINDINGS.lock().dequeue(port)
}

/// Check if a bound UDP port has pending datagrams.
pub fn has_data(port: u16) -> bool {
    UDP_BINDINGS.lock().has_data(port)
}

/// Handle an incoming UDP packet from the IPv4 layer.
pub fn handle_udp(ip_header: &Ipv4Header, payload: &[u8]) {
    let (udp_hdr, udp_data) = match parse(payload) {
        Some(h) => h,
        None => return,
    };

    log::debug!(
        "[udp] {}.{}.{}.{}:{} → port {}  len={}",
        ip_header.src[0],
        ip_header.src[1],
        ip_header.src[2],
        ip_header.src[3],
        udp_hdr.src_port,
        udp_hdr.dst_port,
        udp_data.len(),
    );

    UDP_BINDINGS.lock().enqueue(
        udp_hdr.dst_port,
        UdpDatagram {
            src_ip: ip_header.src,
            src_port: udp_hdr.src_port,
            data: udp_data.to_vec(),
        },
    );
}
