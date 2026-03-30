//! Ethernet frame parsing and construction — re-exported from kernel-core.

#[allow(unused_imports)]
pub use kernel_core::net::ethernet::{
    ETHERTYPE_ARP, ETHERTYPE_IPV4, EthernetFrame, MAC_BROADCAST, build, parse,
};
