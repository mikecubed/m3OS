//! ARP (Address Resolution Protocol) — pure logic re-exported from kernel-core,
//! kernel-specific cache and send/handle functions remain here.

use spin::Mutex;

use super::ethernet::{self, MAC_BROADCAST};
use super::virtio_net::MacAddr;

pub use kernel_core::net::arp::{ARP_OP_REPLY, ARP_OP_REQUEST, ArpPacket, build, parse};
pub use kernel_core::types::Ipv4Addr;

// ===========================================================================
// ARP cache
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
        Self {
            entries: [
                None, None, None, None, None, None, None, None, None, None, None, None, None, None,
                None, None,
            ],
        }
    }

    fn lookup(&self, ip: Ipv4Addr) -> Option<MacAddr> {
        for e in self.entries.iter().flatten() {
            if e.ip == ip {
                return Some(e.mac);
            }
        }
        None
    }

    fn insert(&mut self, ip: Ipv4Addr, mac: MacAddr) {
        let tick = crate::arch::x86_64::interrupts::tick_count();

        for e in self.entries.iter_mut().flatten() {
            if e.ip == ip {
                e.mac = mac;
                e.tick = tick;
                return;
            }
        }

        for entry in &mut self.entries {
            if entry.is_none() {
                *entry = Some(ArpEntry { ip, mac, tick });
                return;
            }
        }

        let mut oldest_idx = 0;
        let mut oldest_tick = u64::MAX;
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(e) = entry
                && e.tick < oldest_tick
            {
                oldest_tick = e.tick;
                oldest_idx = i;
            }
        }
        self.entries[oldest_idx] = Some(ArpEntry { ip, mac, tick });
    }
}

static ARP_CACHE: Mutex<ArpCache> = Mutex::new(ArpCache::new());

/// Look up a MAC address in the ARP cache.
pub fn resolve(target_ip: Ipv4Addr) -> Option<MacAddr> {
    ARP_CACHE.lock().lookup(target_ip)
}

/// Send an ARP request to resolve `target_ip`.
pub fn send_request(target_ip: Ipv4Addr) {
    let our_mac = match super::mac_address() {
        Some(m) => m,
        None => return,
    };
    let our_ip = super::config::our_ip();

    let arp_pkt = build(ARP_OP_REQUEST, our_mac, our_ip, [0; 6], target_ip);

    let frame = ethernet::build(MAC_BROADCAST, our_mac, ethernet::ETHERTYPE_ARP, &arp_pkt);

    super::send_frame(&frame);
    log::debug!(
        "[arp] sent request: who has {}.{}.{}.{}?",
        target_ip[0],
        target_ip[1],
        target_ip[2],
        target_ip[3]
    );
}

/// Process an incoming ARP packet.
pub fn handle_arp(pkt: &ArpPacket) {
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
            let our_ip = super::config::our_ip();
            if pkt.target_ip == our_ip {
                let our_mac = match super::mac_address() {
                    Some(m) => m,
                    None => return,
                };

                let reply = build(ARP_OP_REPLY, our_mac, our_ip, pkt.sender_mac, pkt.sender_ip);
                let frame =
                    ethernet::build(pkt.sender_mac, our_mac, ethernet::ETHERTYPE_ARP, &reply);
                super::send_frame(&frame);

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
