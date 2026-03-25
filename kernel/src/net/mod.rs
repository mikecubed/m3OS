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
pub mod virtio_net;
