//! UDP datagram send/receive — pure logic re-exported from kernel-core.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::task::scheduler::IrqSafeMutex;

use super::arp::Ipv4Addr;
use super::ipv4::{self, Ipv4Header};

use kernel_core::net::udp::{UdpBindings, build, parse};
#[allow(unused_imports)]
pub use kernel_core::net::udp::{UdpDatagram, UdpHeader};
use kernel_core::types::Ipv6Addr;

// Phase 57b G.2.a — IrqSafeMutex inherits Track F.1's preempt-discipline.
// UDP_BINDINGS is touched only from the net polling task and from socket
// syscalls (task context); no ISR ever holds it.  Pure type change.
static UDP_BINDINGS: IrqSafeMutex<UdpBindings> = IrqSafeMutex::new(UdpBindings::new());

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
    // Wake any socket polling this UDP port.
    super::wake_sockets_for_udp_port(udp_hdr.dst_port);
}

// ===========================================================================
// Phase 91 — UDP over IPv6 (A.7)
// ===========================================================================

/// A received UDP datagram with a 128-bit source, queued for an AF_INET6 socket.
pub struct UdpV6Datagram {
    pub src_ip: Ipv6Addr,
    pub src_port: u16,
    pub data: Vec<u8>,
}

const MAX_V6_QUEUES: usize = 8;
const MAX_V6_QUEUE_LEN: usize = 16;

struct UdpV6Queue {
    port: u16,
    items: VecDeque<UdpV6Datagram>,
}

struct UdpV6Recv {
    queues: [Option<UdpV6Queue>; MAX_V6_QUEUES],
}

impl UdpV6Recv {
    const fn new() -> Self {
        const NONE: Option<UdpV6Queue> = None;
        Self {
            queues: [NONE; MAX_V6_QUEUES],
        }
    }

    fn bind(&mut self, port: u16) -> bool {
        for q in self.queues.iter().flatten() {
            if q.port == port {
                return true; // already bound (idempotent)
            }
        }
        for slot in self.queues.iter_mut() {
            if slot.is_none() {
                *slot = Some(UdpV6Queue {
                    port,
                    items: VecDeque::new(),
                });
                return true;
            }
        }
        false
    }

    fn enqueue(&mut self, port: u16, dgram: UdpV6Datagram) {
        for q in self.queues.iter_mut().flatten() {
            if q.port == port {
                if q.items.len() < MAX_V6_QUEUE_LEN {
                    q.items.push_back(dgram);
                }
                return;
            }
        }
    }

    fn dequeue(&mut self, port: u16) -> Option<UdpV6Datagram> {
        for q in self.queues.iter_mut().flatten() {
            if q.port == port {
                return q.items.pop_front();
            }
        }
        None
    }

    fn unbind(&mut self, port: u16) {
        for slot in self.queues.iter_mut() {
            if matches!(slot, Some(q) if q.port == port) {
                *slot = None;
            }
        }
    }
}

static UDP_V6_RECV: IrqSafeMutex<UdpV6Recv> = IrqSafeMutex::new(UdpV6Recv::new());

/// Bind a local UDP port for receiving AF_INET6 datagrams.
pub fn bind_v6(port: u16) -> bool {
    UDP_V6_RECV.lock().bind(port)
}

/// Unbind an AF_INET6 UDP port.
pub fn unbind_v6(port: u16) {
    UDP_V6_RECV.lock().unbind(port);
}

/// Dequeue a received AF_INET6 UDP datagram.
pub fn recv_v6(port: u16) -> Option<UdpV6Datagram> {
    UDP_V6_RECV.lock().dequeue(port)
}

/// Build a UDP datagram with a correct IPv6 pseudo-header checksum (mandatory
/// over IPv6, unlike IPv4 where it is optional). A computed checksum of 0 is
/// transmitted as `0xffff` per RFC 768.
pub fn build_v6(
    src: Ipv6Addr,
    dst: Ipv6Addr,
    src_port: u16,
    dst_port: u16,
    data: &[u8],
) -> Vec<u8> {
    use kernel_core::net::ipv6;
    let mut pkt = build(src_port, dst_port, data);
    let mut ck = ipv6::pseudo_header_checksum(src, dst, ipv6::PROTO_UDP, &pkt);
    if ck == 0 {
        ck = 0xffff;
    }
    pkt[6..8].copy_from_slice(&ck.to_be_bytes());
    pkt
}

/// Send a UDP datagram over IPv6 from an explicit source address.
pub fn send_v6(src: Ipv6Addr, dst: Ipv6Addr, dst_port: u16, src_port: u16, data: &[u8]) {
    let pkt = build_v6(src, dst, src_port, dst_port, data);
    super::ipv6::send_from(src, dst, kernel_core::net::ipv6::PROTO_UDP, &pkt);
}

/// Handle an inbound UDP-over-IPv6 datagram destined for an AF_INET6 socket
/// (the DHCPv6 client port is intercepted earlier in `ipv6::handle_ipv6`).
pub fn handle_udp_v6(src_ip: Ipv6Addr, src_port: u16, dst_port: u16, data: &[u8]) {
    UDP_V6_RECV.lock().enqueue(
        dst_port,
        UdpV6Datagram {
            src_ip,
            src_port,
            data: data.to_vec(),
        },
    );
    super::wake_sockets_for_udp_port(dst_port);
}
