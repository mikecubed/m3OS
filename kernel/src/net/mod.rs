//! Network stack — Phase 16.
//!
//! Layers: virtio-net driver → Ethernet → ARP → IPv4 → ICMP / UDP / TCP.

#[allow(dead_code)]
pub mod arp;
#[allow(dead_code)]
pub mod config;
#[allow(dead_code)]
pub mod dispatch;
#[allow(dead_code)]
pub mod ethernet;
#[allow(dead_code)]
pub mod icmp;
#[allow(dead_code)]
pub mod ipv4;
#[allow(dead_code)]
pub mod tcp;
#[allow(dead_code)]
pub mod udp;
pub mod virtio_net;
