/// Unique task identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId(pub u64);

/// Index into the global endpoint registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointId(pub u8);

/// Index into the global notification registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifId(pub u8);

/// MAC address as 6 bytes.
pub type MacAddr = [u8; 6];

/// IPv4 address as 4 bytes.
pub type Ipv4Addr = [u8; 4];

/// IPv6 address as 16 bytes (Phase 91). Stored in network byte order, the same
/// big-endian convention `Ipv4Addr` uses, so on-wire framing is a direct copy.
pub type Ipv6Addr = [u8; 16];

/// The all-zero unspecified address `::`.
pub const IPV6_UNSPECIFIED: Ipv6Addr = [0u8; 16];

/// The loopback address `::1`.
pub const IPV6_LOOPBACK: Ipv6Addr = {
    let mut a = [0u8; 16];
    a[15] = 1;
    a
};

/// `true` if `addr` is the unspecified address `::`.
pub fn ipv6_is_unspecified(addr: &Ipv6Addr) -> bool {
    *addr == IPV6_UNSPECIFIED
}

/// `true` if `addr` is the loopback address `::1`.
pub fn ipv6_is_loopback(addr: &Ipv6Addr) -> bool {
    *addr == IPV6_LOOPBACK
}

/// `true` if `addr` is a link-local unicast address (`fe80::/10`, RFC 4291 §2.5.6).
pub fn ipv6_is_link_local(addr: &Ipv6Addr) -> bool {
    addr[0] == 0xfe && (addr[1] & 0xc0) == 0x80
}

/// `true` if `addr` is a multicast address (`ff00::/8`, RFC 4291 §2.7).
pub fn ipv6_is_multicast(addr: &Ipv6Addr) -> bool {
    addr[0] == 0xff
}

/// Derive a 64-bit Modified EUI-64 interface identifier from a 48-bit MAC
/// (RFC 4291 Appendix A): insert `ff:fe` in the middle and flip the
/// universal/local bit (bit 1 of the first octet).
pub fn eui64_from_mac(mac: MacAddr) -> [u8; 8] {
    [
        mac[0] ^ 0x02,
        mac[1],
        mac[2],
        0xff,
        0xfe,
        mac[3],
        mac[4],
        mac[5],
    ]
}

/// Compute the solicited-node multicast address `ff02::1:ffXX:XXXX` for `addr`
/// (RFC 4291 §2.7.1): the `ff02::1:ff00:0/104` prefix concatenated with the low
/// 24 bits of `addr`.
pub fn solicited_node_multicast(addr: &Ipv6Addr) -> Ipv6Addr {
    let mut out = [0u8; 16];
    out[0] = 0xff;
    out[1] = 0x02;
    out[11] = 0x01;
    out[12] = 0xff;
    out[13] = addr[13];
    out[14] = addr[14];
    out[15] = addr[15];
    out
}

/// Compose a link-local unicast address `fe80::/64 ++ EUI-64(mac)` from a MAC
/// (RFC 4291). Used at NIC init to form the address NDP itself sources from.
pub fn link_local_from_mac(mac: MacAddr) -> Ipv6Addr {
    let iid = eui64_from_mac(mac);
    let mut out = [0u8; 16];
    out[0] = 0xfe;
    out[1] = 0x80;
    out[8..16].copy_from_slice(&iid);
    out
}

/// Compose a global/unicast address from a 64-bit prefix and a MAC-derived
/// EUI-64 interface identifier (SLAAC, RFC 4862). Only the first 8 bytes of
/// `prefix` are used.
pub fn slaac_address(prefix: &Ipv6Addr, mac: MacAddr) -> Ipv6Addr {
    let iid = eui64_from_mac(mac);
    let mut out = [0u8; 16];
    out[0..8].copy_from_slice(&prefix[0..8]);
    out[8..16].copy_from_slice(&iid);
    out
}

/// `true` if the first `len` bits of `a` and `b` are equal — the on-link test
/// for an IPv6 prefix of arbitrary (not necessarily byte-aligned) length. A
/// `len` of 0 matches everything; `len >= 128` compares the full address.
pub fn ipv6_prefix_matches(a: &Ipv6Addr, b: &Ipv6Addr, len: u8) -> bool {
    let len = len.min(128) as usize;
    let full_bytes = len / 8;
    if a[..full_bytes] != b[..full_bytes] {
        return false;
    }
    let rem = (len % 8) as u8;
    if rem != 0 {
        let mask = 0xffu8 << (8 - rem);
        if (a[full_bytes] & mask) != (b[full_bytes] & mask) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod ipv6_tests {
    use super::*;

    #[test]
    fn classification() {
        assert!(ipv6_is_unspecified(&IPV6_UNSPECIFIED));
        assert!(!ipv6_is_loopback(&IPV6_UNSPECIFIED));
        assert!(ipv6_is_loopback(&IPV6_LOOPBACK));
        assert!(!ipv6_is_unspecified(&IPV6_LOOPBACK));

        let ll: Ipv6Addr = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert!(ipv6_is_link_local(&ll));
        assert!(!ipv6_is_multicast(&ll));
        assert!(!ipv6_is_loopback(&ll));

        // fec0:: is site-local, NOT link-local (top 10 bits differ).
        let site: Ipv6Addr = [0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        assert!(!ipv6_is_link_local(&site));

        let mc: Ipv6Addr = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert!(ipv6_is_multicast(&mc));
        assert!(!ipv6_is_link_local(&mc));
    }

    #[test]
    fn eui64_derivation() {
        // RFC 4291 worked example: MAC 52:54:00:12:34:56 -> IID 5054:00ff:fe12:3456.
        let mac: MacAddr = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let iid = eui64_from_mac(mac);
        assert_eq!(iid, [0x50, 0x54, 0x00, 0xff, 0xfe, 0x12, 0x34, 0x56]);
    }

    #[test]
    fn link_local_composition() {
        let mac: MacAddr = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let ll = link_local_from_mac(mac);
        assert_eq!(
            ll,
            [
                0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x50, 0x54, 0x00, 0xff, 0xfe, 0x12, 0x34, 0x56
            ]
        );
        assert!(ipv6_is_link_local(&ll));
    }

    #[test]
    fn slaac_composition() {
        let mac: MacAddr = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        // 2001:db8::/64 prefix.
        let prefix: Ipv6Addr = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let g = slaac_address(&prefix, mac);
        assert_eq!(
            g,
            [
                0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0x50, 0x54, 0x00, 0xff, 0xfe, 0x12, 0x34, 0x56
            ]
        );
    }

    #[test]
    fn prefix_matches_on_link_and_off_link() {
        // 2001:db8::/64 — same /64 is on-link, a differing /64 is off-link.
        let a: Ipv6Addr = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        let same64: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0xaa, 0xbb, 0, 0, 0, 0, 0, 2,
        ];
        let diff64: Ipv6Addr = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2];
        assert!(ipv6_prefix_matches(&a, &same64, 64));
        assert!(!ipv6_prefix_matches(&a, &diff64, 64));
        // /0 matches anything; the full /128 only matches itself.
        assert!(ipv6_prefix_matches(&a, &diff64, 0));
        assert!(ipv6_prefix_matches(&a, &a, 128));
        assert!(!ipv6_prefix_matches(&a, &same64, 128));
    }

    #[test]
    fn prefix_matches_non_byte_aligned() {
        // A /60 boundary splits byte 7 (4 prefix bits + 4 host bits): equal in
        // the high nibble matches, differing in it does not.
        let a: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0xab, 0xcd, 0xef, 0x10, 0, 0, 0, 0, 0, 0, 0, 1,
        ];
        let hi_eq: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0xab, 0xcd, 0xef, 0x1f, 0, 0, 0, 0, 0, 0, 0, 2,
        ];
        let hi_ne: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0xab, 0xcd, 0xef, 0x20, 0, 0, 0, 0, 0, 0, 0, 2,
        ];
        assert!(ipv6_prefix_matches(&a, &hi_eq, 60));
        assert!(!ipv6_prefix_matches(&a, &hi_ne, 60));
    }

    #[test]
    fn solicited_node() {
        // RFC 4291 §2.7.1 example: low 24 bits 0x12ec:eafd -> ff02::1:ffec:eafd.
        let addr: Ipv6Addr = [
            0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x12, 0xec, 0xea, 0xfd,
        ];
        let snm = solicited_node_multicast(&addr);
        assert_eq!(
            snm,
            [
                0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0xff, 0xec, 0xea, 0xfd
            ]
        );
        assert!(ipv6_is_multicast(&snm));
    }
}
