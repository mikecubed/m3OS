//! ARP (Address Resolution Protocol) for IPv4 over Ethernet (P16-T017 through P16-T023).

use alloc::vec::Vec;
use spin::Mutex;

use super::ethernet::{self, MAC_BROADCAST};
use super::virtio_net::{self, MacAddr};

// ===========================================================================
// Types
// ===========================================================================

/// IPv4 address as 4 bytes.
pub type Ipv4Addr = [u8; 4];

// ===========================================================================
// ARP packet structure (P16-T017)
// ===========================================================================

/// ARP hardware type: Ethernet = 1.
const ARP_HW_ETHERNET: u16 = 1;
/// ARP protocol type: IPv4 = 0x0800.
const ARP_PROTO_IPV4: u16 = 0x0800;

const ARP_OP_REQUEST: u16 = 1;
const ARP_OP_REPLY: u16 = 2;

/// Parsed ARP packet (Ethernet/IPv4 only).
#[derive(Debug, Clone)]
pub struct ArpPacket {
    pub operation: u16,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

// ===========================================================================
// Parse / Build (P16-T018)
// ===========================================================================

/// Parse an ARP packet from raw payload bytes.
pub fn parse(payload: &[u8]) -> Option<ArpPacket> {
    if payload.len() < 28 {
        return None;
    }

    let hw_type = u16::from_be_bytes([payload[0], payload[1]]);
    let proto_type = u16::from_be_bytes([payload[2], payload[3]]);
    let hw_len = payload[4];
    let proto_len = payload[5];

    if hw_type != ARP_HW_ETHERNET || proto_type != ARP_PROTO_IPV4 || hw_len != 6 || proto_len != 4 {
        return None;
    }

    let operation = u16::from_be_bytes([payload[6], payload[7]]);

    let mut sender_mac = [0u8; 6];
    sender_mac.copy_from_slice(&payload[8..14]);
    let mut sender_ip = [0u8; 4];
    sender_ip.copy_from_slice(&payload[14..18]);
    let mut target_mac = [0u8; 6];
    target_mac.copy_from_slice(&payload[18..24]);
    let mut target_ip = [0u8; 4];
    target_ip.copy_from_slice(&payload[24..28]);

    Some(ArpPacket {
        operation,
        sender_mac,
        sender_ip,
        target_mac,
        target_ip,
    })
}

/// Build an ARP packet as raw bytes.
pub fn build(
    operation: u16,
    sender_mac: MacAddr,
    sender_ip: Ipv4Addr,
    target_mac: MacAddr,
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(28);
    pkt.extend_from_slice(&ARP_HW_ETHERNET.to_be_bytes());
    pkt.extend_from_slice(&ARP_PROTO_IPV4.to_be_bytes());
    pkt.push(6); // hw addr len
    pkt.push(4); // proto addr len
    pkt.extend_from_slice(&operation.to_be_bytes());
    pkt.extend_from_slice(&sender_mac);
    pkt.extend_from_slice(&sender_ip);
    pkt.extend_from_slice(&target_mac);
    pkt.extend_from_slice(&target_ip);
    pkt
}

// ===========================================================================
// ARP cache (P16-T019)
// ===========================================================================

const ARP_CACHE_SIZE: usize = 16;

struct ArpEntry {
    ip: Ipv4Addr,
    mac: MacAddr,
    tick: u64,
}

struct ArpCache {
    entries: [Option<ArpEntry>; ARP_CACHE_SIZE],
}

impl ArpCache {
    const fn new() -> Self {
        // We cannot use `[None; N]` because ArpEntry is not Copy.
        Self {
            entries: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None,
            ],
        }
    }

    /// Look up a MAC address by IPv4 address.
    fn lookup(&self, ip: Ipv4Addr) -> Option<MacAddr> {
        for e in self.entries.iter().flatten() {
            if e.ip == ip {
                return Some(e.mac);
            }
        }
        None
    }

    /// Insert or update an entry. Evicts the oldest entry if full.
    fn insert(&mut self, ip: Ipv4Addr, mac: MacAddr) {
        let tick = crate::arch::x86_64::interrupts::tick_count();

        // Update existing entry.
        for e in self.entries.iter_mut().flatten() {
            if e.ip == ip {
                e.mac = mac;
                e.tick = tick;
                return;
            }
        }

        // Find an empty slot.
        for entry in &mut self.entries {
            if entry.is_none() {
                *entry = Some(ArpEntry { ip, mac, tick });
                return;
            }
        }

        // LRU eviction: replace the oldest entry.
        let mut oldest_idx = 0;
        let mut oldest_tick = u64::MAX;
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(e) = entry {
                if e.tick < oldest_tick {
                    oldest_tick = e.tick;
                    oldest_idx = i;
                }
            }
        }
        self.entries[oldest_idx] = Some(ArpEntry { ip, mac, tick });
    }
}

static ARP_CACHE: Mutex<ArpCache> = Mutex::new(ArpCache::new());

// ===========================================================================
// ARP resolve (P16-T020)
// ===========================================================================

/// Look up a MAC address in the ARP cache.
pub fn resolve(target_ip: Ipv4Addr) -> Option<MacAddr> {
    ARP_CACHE.lock().lookup(target_ip)
}

// ===========================================================================
// ARP request (P16-T021)
// ===========================================================================

/// Send an ARP request to resolve `target_ip`.
pub fn send_request(target_ip: Ipv4Addr) {
    let our_mac = match virtio_net::mac_address() {
        Some(m) => m,
        None => return,
    };
    let our_ip = super::config::our_ip();

    let arp_pkt = build(
        ARP_OP_REQUEST,
        our_mac,
        our_ip,
        [0; 6], // target MAC unknown
        target_ip,
    );

    let frame = ethernet::build(MAC_BROADCAST, our_mac, ethernet::ETHERTYPE_ARP, &arp_pkt);

    virtio_net::send_frame(&frame);
    log::debug!(
        "[arp] sent request: who has {}.{}.{}.{}?",
        target_ip[0],
        target_ip[1],
        target_ip[2],
        target_ip[3]
    );
}

// ===========================================================================
// ARP reply handler (P16-T022)
// ===========================================================================

/// Process an incoming ARP packet.
///
/// - If it's a reply, update the cache.
/// - If it's a request for our IP, send a reply (P16-T023).
pub fn handle_arp(pkt: &ArpPacket) {
    // Always update the cache with the sender's info if it's a valid mapping.
    if pkt.sender_mac != [0; 6] && pkt.sender_ip != [0; 4] {
        ARP_CACHE.lock().insert(pkt.sender_ip, pkt.sender_mac);
    }

    match pkt.operation {
        ARP_OP_REPLY => {
            log::debug!(
                "[arp] reply: {}.{}.{}.{} is {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                pkt.sender_ip[0],
                pkt.sender_ip[1],
                pkt.sender_ip[2],
                pkt.sender_ip[3],
                pkt.sender_mac[0],
                pkt.sender_mac[1],
                pkt.sender_mac[2],
                pkt.sender_mac[3],
                pkt.sender_mac[4],
                pkt.sender_mac[5],
            );
        }
        ARP_OP_REQUEST => {
            // P16-T023: Respond to ARP requests for our IP.
            let our_ip = super::config::our_ip();
            if pkt.target_ip == our_ip {
                let our_mac = match virtio_net::mac_address() {
                    Some(m) => m,
                    None => return,
                };

                let reply = build(ARP_OP_REPLY, our_mac, our_ip, pkt.sender_mac, pkt.sender_ip);
                let frame =
                    ethernet::build(pkt.sender_mac, our_mac, ethernet::ETHERTYPE_ARP, &reply);
                virtio_net::send_frame(&frame);

                log::debug!(
                    "[arp] sent reply to {}.{}.{}.{}",
                    pkt.sender_ip[0],
                    pkt.sender_ip[1],
                    pkt.sender_ip[2],
                    pkt.sender_ip[3],
                );
            }
        }
        _ => {}
    }
}
