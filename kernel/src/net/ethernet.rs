//! Ethernet frame parsing and construction — re-exported from kernel-core.

#[allow(unused_imports)]
pub use kernel_core::net::ethernet::{
    build, parse, EthernetFrame, ETHERTYPE_ARP, ETHERTYPE_IPV4, MAC_BROADCAST,
};
