use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::types::Ipv4Addr;

/// Parsed UDP header.
#[derive(Debug, Clone, Copy)]
pub struct UdpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub length: u16,
    pub checksum: u16,
}

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

/// Build a UDP datagram. Checksum is set to 0 (optional for UDP over IPv4).
pub fn build(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let max_payload = (u16::MAX as usize).saturating_sub(8);
    let effective = if payload.len() > max_payload {
        &payload[..max_payload]
    } else {
        payload
    };
    let length = 8 + effective.len() as u16;
    let mut pkt = Vec::with_capacity(length as usize);
    pkt.extend_from_slice(&src_port.to_be_bytes());
    pkt.extend_from_slice(&dst_port.to_be_bytes());
    pkt.extend_from_slice(&length.to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum = 0
    pkt.extend_from_slice(effective);
    pkt
}

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

/// Must match `userspace/net_server/src/main.rs::MAX_BINDINGS` so every bind
/// the service approves can actually be installed in the mechanism layer.
/// With a smaller cap here the kernel's `udp::bind()` silently returned
/// `false` in the service-approved branch and the kernel call site ignored
/// it (the service is the policy authority), so ingress datagrams on those
/// ports never queued.
const MAX_BINDINGS: usize = 32;
const MAX_QUEUE_LEN: usize = 32;

/// UDP port binding table — testable pure-logic struct.
pub struct UdpBindings {
    bindings: [Option<PortBinding>; MAX_BINDINGS],
}

impl UdpBindings {
    /// Create a new empty binding table.
    pub const fn new() -> Self {
        Self {
            bindings: [const { None }; MAX_BINDINGS],
        }
    }

    /// Bind a local UDP port. Returns false if already bound or table full.
    pub fn bind(&mut self, port: u16) -> bool {
        for b in self.bindings.iter().flatten() {
            if b.port == port {
                return false;
            }
        }
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

    /// Enqueue a datagram for a bound port.
    pub fn enqueue(&mut self, port: u16, dgram: UdpDatagram) {
        for b in self.bindings.iter_mut().flatten() {
            if b.port == port && b.queue.len() < MAX_QUEUE_LEN {
                b.queue.push_back(dgram);
                return;
            }
        }
    }

    /// Dequeue a datagram from a bound port.
    pub fn dequeue(&mut self, port: u16) -> Option<UdpDatagram> {
        for b in self.bindings.iter_mut().flatten() {
            if b.port == port {
                return b.queue.pop_front();
            }
        }
        None
    }

    /// Unbind a UDP port, releasing it for future use.
    pub fn unbind(&mut self, port: u16) {
        for slot in self.bindings.iter_mut() {
            if let Some(b) = slot
                && b.port == port
            {
                *slot = None;
                return;
            }
        }
    }

    /// Check if a bound port has pending datagrams without dequeuing.
    pub fn has_data(&self, port: u16) -> bool {
        for b in self.bindings.iter().flatten() {
            if b.port == port {
                return !b.queue.is_empty();
            }
        }
        false
    }
}

impl Default for UdpBindings {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        let pkt = build(1234, 5678, b"hello");
        let (header, payload) = parse(&pkt).unwrap();
        assert_eq!(header.src_port, 1234);
        assert_eq!(header.dst_port, 5678);
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn parse_too_short() {
        assert!(parse(&[0u8; 7]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn build_round_trip() {
        let payload = b"test data";
        let raw = build(80, 443, payload);
        let (header, data) = parse(&raw).unwrap();
        assert_eq!(header.src_port, 80);
        assert_eq!(header.dst_port, 443);
        assert_eq!(data, payload);
        assert_eq!(header.length, 8 + payload.len() as u16);
    }

    #[test]
    fn payload_truncation() {
        // length field says more data than available — should clamp
        let mut raw = build(1, 2, b"abc");
        // Truncate the raw packet to remove the last byte of payload
        raw.truncate(raw.len() - 1);
        let (_, data) = parse(&raw).unwrap();
        assert_eq!(data, b"ab"); // clamped to available bytes
    }

    #[test]
    fn bindings_bind_and_dequeue() {
        let mut bindings = UdpBindings::new();
        assert!(bindings.bind(53));
        bindings.enqueue(
            53,
            UdpDatagram {
                src_ip: [10, 0, 0, 1],
                src_port: 1234,
                data: b"dns query".to_vec(),
            },
        );
        let dgram = bindings.dequeue(53).unwrap();
        assert_eq!(dgram.data, b"dns query");
        assert!(bindings.dequeue(53).is_none());
    }

    #[test]
    fn bindings_duplicate_bind() {
        let mut bindings = UdpBindings::new();
        assert!(bindings.bind(80));
        assert!(!bindings.bind(80)); // duplicate
    }

    #[test]
    fn has_data_peek() {
        let mut bindings = UdpBindings::new();
        assert!(bindings.bind(9000));
        assert!(!bindings.has_data(9000));
        bindings.enqueue(
            9000,
            UdpDatagram {
                src_ip: [10, 0, 0, 1],
                src_port: 1234,
                data: vec![1, 2, 3],
            },
        );
        assert!(bindings.has_data(9000));
        // Dequeue and verify has_data returns false
        let _ = bindings.dequeue(9000);
        assert!(!bindings.has_data(9000));
    }

    #[test]
    fn bindings_full_queue() {
        let mut bindings = UdpBindings::new();
        bindings.bind(100);
        for i in 0..32 {
            bindings.enqueue(
                100,
                UdpDatagram {
                    src_ip: [0; 4],
                    src_port: i,
                    data: Vec::new(),
                },
            );
        }
        // 33rd should be silently dropped
        bindings.enqueue(
            100,
            UdpDatagram {
                src_ip: [0; 4],
                src_port: 9999,
                data: Vec::new(),
            },
        );
        // Verify first dequeued is the first enqueued
        let first = bindings.dequeue(100).unwrap();
        assert_eq!(first.src_port, 0);
    }
}
