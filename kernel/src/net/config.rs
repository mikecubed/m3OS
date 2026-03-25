//! Static network configuration (P16-T029).
//!
//! QEMU user-mode networking defaults:
//! - Guest IP: 10.0.2.15/24
//! - Gateway:  10.0.2.2
//! - DNS:      10.0.2.3

use super::arp::Ipv4Addr;

/// Our static IPv4 address.
pub fn our_ip() -> Ipv4Addr {
    [10, 0, 2, 15]
}

/// Subnet mask (/24).
pub fn subnet_mask() -> Ipv4Addr {
    [255, 255, 255, 0]
}

/// Default gateway.
pub fn gateway_ip() -> Ipv4Addr {
    [10, 0, 2, 2]
}

/// Check if `ip` is on the local subnet.
pub fn is_local(ip: Ipv4Addr) -> bool {
    let mask = subnet_mask();
    let our = our_ip();
    for i in 0..4 {
        if (ip[i] & mask[i]) != (our[i] & mask[i]) {
            return false;
        }
    }
    true
}
