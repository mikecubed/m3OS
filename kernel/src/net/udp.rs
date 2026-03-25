//! UDP datagram send/receive with port multiplexing (P16-T034 through P16-T039).

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

use super::arp::Ipv4Addr;
use super::ipv4::{self, Ipv4Header};

// ===========================================================================
// UDP Header (P16-T034)
// ===========================================================================

/// Parsed UDP header.
#[derive(Debug, Clone, Copy)]
pub struct UdpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub length: u16,
    pub checksum: u16,
}

// ===========================================================================
// Parse (P16-T035)
// ===========================================================================

/// Parse a UDP datagram from raw payload bytes.
pub fn parse(data: &[u8]) -> Option<(UdpHeader, &[u8])> {
    if data.len() < 8 {
        return None;
    }

    let header = UdpHeader {
        src_port: u16::from_be_bytes([data[0], data[1]]),
        dst_port: u16::from_be_bytes([data[2], data[3]]),
        length: u16::from_be_bytes([data[4], data[5]]),
        checksum: u16::from_be_bytes([data[6], data[7]]),
    };

    let payload_len = (header.length as usize)
        .saturating_sub(8)
        .min(data.len() - 8);
    Some((header, &data[8..8 + payload_len]))
}

// ===========================================================================
// Build (P16-T036)
// ===========================================================================

/// Build a UDP datagram. Checksum is set to 0 (optional for UDP over IPv4).
pub fn build(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let length = 8 + payload.len() as u16;
    let mut pkt = Vec::with_capacity(length as usize);
    pkt.extend_from_slice(&src_port.to_be_bytes());
    pkt.extend_from_slice(&dst_port.to_be_bytes());
    pkt.extend_from_slice(&length.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum = 0
    pkt.extend_from_slice(payload);
    pkt
}

// ===========================================================================
// Port binding table (P16-T037)
// ===========================================================================

/// A received UDP datagram queued for a bound port.
pub struct UdpDatagram {
    pub src_ip: Ipv4Addr,
    pub src_port: u16,
    pub data: Vec<u8>,
}

struct PortBinding {
    port: u16,
    queue: VecDeque<UdpDatagram>,
}

const MAX_BINDINGS: usize = 16;
const MAX_QUEUE_LEN: usize = 32;

struct UdpBindings {
    bindings: [Option<PortBinding>; MAX_BINDINGS],
}

impl UdpBindings {
    const fn new() -> Self {
        Self {
            bindings: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None,
            ],
        }
    }

    fn bind(&mut self, port: u16) -> bool {
        // Check if already bound.
        for b in self.bindings.iter().flatten() {
            if b.port == port {
                return false;
            }
        }
        // Find empty slot.
        for slot in &mut self.bindings {
            if slot.is_none() {
                *slot = Some(PortBinding {
                    port,
                    queue: VecDeque::new(),
                });
                return true;
            }
        }
        false
    }

    fn enqueue(&mut self, port: u16, dgram: UdpDatagram) {
        for b in self.bindings.iter_mut().flatten() {
            if b.port == port && b.queue.len() < MAX_QUEUE_LEN {
                b.queue.push_back(dgram);
                return;
            }
        }
    }

    fn dequeue(&mut self, port: u16) -> Option<UdpDatagram> {
        for b in self.bindings.iter_mut().flatten() {
            if b.port == port {
                return b.queue.pop_front();
            }
        }
        None
    }
}

static UDP_BINDINGS: Mutex<UdpBindings> = Mutex::new(UdpBindings::new());

/// Bind a local UDP port for receiving datagrams.
pub fn bind(port: u16) -> bool {
    UDP_BINDINGS.lock().bind(port)
}

// ===========================================================================
// Send (P16-T038)
// ===========================================================================

/// Send a UDP datagram.
pub fn send(dst_ip: Ipv4Addr, dst_port: u16, src_port: u16, data: &[u8]) {
    let udp_pkt = build(src_port, dst_port, data);
    ipv4::send(dst_ip, ipv4::PROTO_UDP, &udp_pkt);
}

// ===========================================================================
// Receive (P16-T039)
// ===========================================================================

/// Try to dequeue a received UDP datagram from a bound port.
///
/// Returns `None` if no datagram is available (non-blocking).
pub fn recv(port: u16) -> Option<UdpDatagram> {
    UDP_BINDINGS.lock().dequeue(port)
}

// ===========================================================================
// Incoming packet handler
// ===========================================================================

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
