//! Neighbor Discovery Protocol (RFC 4861) — IPv6's ARP, carried over ICMPv6.
//!
//! These are ICMPv6 messages (type 133–136): the 4-byte ICMPv6 `rest` word
//! carries the message's reserved/flags field and the ICMPv6 `body` carries the
//! target address (NS/NA) followed by the option area. Options use the RFC 4861
//! §4.6 TLV format: `type(1) length(1) value(..)`, where `length` is in units of
//! **8 octets and INCLUDES the type+length bytes** (so an option spans
//! `length * 8` bytes; a `length` of 0 is invalid).

use alloc::vec::Vec;

use crate::types::{Ipv6Addr, MacAddr};

use super::icmpv6::{
    self, ICMPV6_NEIGHBOR_ADVERTISEMENT, ICMPV6_NEIGHBOR_SOLICITATION, ICMPV6_ROUTER_ADVERTISEMENT,
    ICMPV6_ROUTER_SOLICITATION,
};

// NDP option type codes (RFC 4861 §4.6 + RFC 8106 §5.1).
const OPT_SOURCE_LLADDR: u8 = 1;
const OPT_TARGET_LLADDR: u8 = 2;
const OPT_PREFIX_INFO: u8 = 3;
const OPT_RDNSS: u8 = 25;

/// Upper bound on the number of options we will walk in one message. NDP
/// messages carry only a handful of options; a cap keeps a malformed/looping
/// buffer from being walked unboundedly.
const MAX_OPTIONS: usize = 16;

/// Neighbor Advertisement Router flag (R) bit in the `rest` flags byte.
const NA_FLAG_ROUTER: u8 = 0x80;
/// Neighbor Advertisement Solicited flag (S) bit.
const NA_FLAG_SOLICITED: u8 = 0x40;
/// Neighbor Advertisement Override flag (O) bit.
const NA_FLAG_OVERRIDE: u8 = 0x20;

/// Router Advertisement Managed-address-configuration flag (M).
const RA_FLAG_MANAGED: u8 = 0x80;
/// Router Advertisement Other-configuration flag (O).
const RA_FLAG_OTHER: u8 = 0x40;

/// Prefix Information On-Link flag (L).
const PIO_FLAG_ON_LINK: u8 = 0x80;
/// Prefix Information Autonomous-address-configuration flag (A).
const PIO_FLAG_AUTONOMOUS: u8 = 0x40;

/// Parsed Neighbor Solicitation (RFC 4861 §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighborSolicitation {
    pub target: Ipv6Addr,
    pub src_lladdr: Option<MacAddr>,
}

/// Parsed Neighbor Advertisement (RFC 4861 §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighborAdvertisement {
    pub target: Ipv6Addr,
    pub router: bool,
    pub solicited: bool,
    pub override_flag: bool,
    pub target_lladdr: Option<MacAddr>,
}

/// Parsed Prefix Information option (RFC 4861 §4.6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixInfo {
    pub prefix_length: u8,
    pub on_link: bool,
    pub autonomous: bool,
    pub valid_lifetime: u32,
    pub preferred_lifetime: u32,
    pub prefix: Ipv6Addr,
}

/// Parsed Router Advertisement (RFC 4861 §4.2 + RFC 8106 RDNSS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterAdvertisement {
    pub cur_hop_limit: u8,
    /// M flag — addresses come from DHCPv6 (managed configuration).
    pub managed: bool,
    /// O flag — other config (e.g. DNS) comes from DHCPv6.
    pub other: bool,
    /// Router lifetime in seconds; 0 means "not a default router".
    pub router_lifetime: u16,
    pub prefix: Option<PrefixInfo>,
    /// RFC 8106 recursive DNS server addresses.
    pub rdnss: Vec<Ipv6Addr>,
    pub src_lladdr: Option<MacAddr>,
}

/// One decoded NDP option, yielded to the visitor by [`parse_ndp_options`].
enum NdpOption<'a> {
    SourceLlAddr(MacAddr),
    TargetLlAddr(MacAddr),
    PrefixInfo(PrefixInfo),
    Rdnss(&'a [u8]),
}

/// Walk the NDP option area, invoking `visitor` for each option we understand.
///
/// Each option is `type(1) length(1) value(..)`, `length` counting 8-octet
/// units (including the 2 type/length bytes). Walking stops on `length == 0`,
/// on an option that runs past `opts`, or after [`MAX_OPTIONS`] options. Other
/// (unrecognized) option types are skipped using their `length * 8` size rather
/// than failing — an NDP receiver must ignore options it does not understand.
fn parse_ndp_options<F: FnMut(NdpOption<'_>)>(opts: &[u8], mut visitor: F) {
    let mut off = 0usize;
    let mut seen = 0usize;
    while off + 2 <= opts.len() && seen < MAX_OPTIONS {
        let opt_type = opts[off];
        let len_units = opts[off + 1] as usize;
        if len_units == 0 {
            break; // invalid per RFC 4861 §4.6 — stop, do not loop forever.
        }
        let opt_len = len_units * 8;
        if off + opt_len > opts.len() {
            break; // option runs past the buffer.
        }
        let value = &opts[off + 2..off + opt_len];

        match opt_type {
            OPT_SOURCE_LLADDR if len_units == 1 => {
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&value[..6]);
                visitor(NdpOption::SourceLlAddr(mac));
            }
            OPT_TARGET_LLADDR if len_units == 1 => {
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&value[..6]);
                visitor(NdpOption::TargetLlAddr(mac));
            }
            OPT_PREFIX_INFO if len_units == 4 => {
                // value: prefix_length(1) flags(1) valid(4) preferred(4)
                //        reserved(4) prefix(16) — 30 bytes after the 2-byte hdr.
                let flags = value[1];
                let valid_lifetime = u32::from_be_bytes([value[2], value[3], value[4], value[5]]);
                let preferred_lifetime =
                    u32::from_be_bytes([value[6], value[7], value[8], value[9]]);
                let mut prefix = [0u8; 16];
                prefix.copy_from_slice(&value[14..30]);
                visitor(NdpOption::PrefixInfo(PrefixInfo {
                    prefix_length: value[0],
                    on_link: flags & PIO_FLAG_ON_LINK != 0,
                    autonomous: flags & PIO_FLAG_AUTONOMOUS != 0,
                    valid_lifetime,
                    preferred_lifetime,
                    prefix,
                }));
            }
            OPT_RDNSS => {
                // value: reserved(2) lifetime(4) then N*16-byte addresses.
                // num_servers = (len_units - 1) / 2; pass the address region.
                visitor(NdpOption::Rdnss(&value[6..]));
            }
            _ => {} // unrecognized — skip via opt_len, do not fail.
        }

        off += opt_len;
        seen += 1;
    }
}

/// Parse a Neighbor Solicitation from a full ICMPv6 message.
pub fn parse_neighbor_solicitation(icmpv6_msg: &[u8]) -> Option<NeighborSolicitation> {
    let (header, body) = icmpv6::parse(icmpv6_msg)?;
    if header.msg_type != ICMPV6_NEIGHBOR_SOLICITATION {
        return None;
    }
    if body.len() < 16 {
        return None;
    }
    let mut target = [0u8; 16];
    target.copy_from_slice(&body[..16]);

    let mut src_lladdr = None;
    parse_ndp_options(&body[16..], |opt| {
        if let NdpOption::SourceLlAddr(mac) = opt {
            src_lladdr = Some(mac);
        }
    });

    Some(NeighborSolicitation { target, src_lladdr })
}

/// Parse a Neighbor Advertisement from a full ICMPv6 message.
pub fn parse_neighbor_advertisement(icmpv6_msg: &[u8]) -> Option<NeighborAdvertisement> {
    let (header, body) = icmpv6::parse(icmpv6_msg)?;
    if header.msg_type != ICMPV6_NEIGHBOR_ADVERTISEMENT {
        return None;
    }
    if body.len() < 16 {
        return None;
    }
    let flags = header.rest[0];
    let mut target = [0u8; 16];
    target.copy_from_slice(&body[..16]);

    let mut target_lladdr = None;
    parse_ndp_options(&body[16..], |opt| {
        if let NdpOption::TargetLlAddr(mac) = opt {
            target_lladdr = Some(mac);
        }
    });

    Some(NeighborAdvertisement {
        target,
        router: flags & NA_FLAG_ROUTER != 0,
        solicited: flags & NA_FLAG_SOLICITED != 0,
        override_flag: flags & NA_FLAG_OVERRIDE != 0,
        target_lladdr,
    })
}

/// Parse a Router Advertisement from a full ICMPv6 message.
pub fn parse_router_advertisement(icmpv6_msg: &[u8]) -> Option<RouterAdvertisement> {
    let (header, body) = icmpv6::parse(icmpv6_msg)?;
    if header.msg_type != ICMPV6_ROUTER_ADVERTISEMENT {
        return None;
    }
    // body: reachable_time(4) retrans_timer(4) then options.
    if body.len() < 8 {
        return None;
    }
    let cur_hop_limit = header.rest[0];
    let flags = header.rest[1];
    let router_lifetime = u16::from_be_bytes([header.rest[2], header.rest[3]]);

    let mut prefix = None;
    let mut rdnss = Vec::new();
    let mut src_lladdr = None;
    parse_ndp_options(&body[8..], |opt| match opt {
        NdpOption::PrefixInfo(pi) => prefix = Some(pi),
        NdpOption::Rdnss(addrs) => {
            for chunk in addrs.chunks_exact(16) {
                let mut a = [0u8; 16];
                a.copy_from_slice(chunk);
                rdnss.push(a);
            }
        }
        NdpOption::SourceLlAddr(mac) => src_lladdr = Some(mac),
        NdpOption::TargetLlAddr(_) => {}
    });

    Some(RouterAdvertisement {
        cur_hop_limit,
        managed: flags & RA_FLAG_MANAGED != 0,
        other: flags & RA_FLAG_OTHER != 0,
        router_lifetime,
        prefix,
        rdnss,
        src_lladdr,
    })
}

/// Encode an 8-byte link-layer-address option (`type, len=1, mac[0..6]`).
fn lladdr_option(opt_type: u8, mac: MacAddr) -> [u8; 8] {
    [opt_type, 1, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]
}

/// Build a Neighbor Solicitation. The `dst` is normally
/// `solicited_node_multicast(target)` and `src` our link-local address.
pub fn build_neighbor_solicitation(
    target: Ipv6Addr,
    src_lladdr: MacAddr,
    src: Ipv6Addr,
    dst: Ipv6Addr,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(16 + 8);
    body.extend_from_slice(&target);
    body.extend_from_slice(&lladdr_option(OPT_SOURCE_LLADDR, src_lladdr));
    icmpv6::build(
        ICMPV6_NEIGHBOR_SOLICITATION,
        0,
        [0, 0, 0, 0],
        &body,
        src,
        dst,
    )
}

/// Build a Neighbor Advertisement. `router` is false here (a host), with the
/// Solicited/Override flags taken from the arguments and a Target Link-Layer
/// Address option appended.
pub fn build_neighbor_advertisement(
    target: Ipv6Addr,
    target_lladdr: MacAddr,
    solicited: bool,
    override_flag: bool,
    src: Ipv6Addr,
    dst: Ipv6Addr,
) -> Vec<u8> {
    let mut flags = 0u8;
    if solicited {
        flags |= NA_FLAG_SOLICITED;
    }
    if override_flag {
        flags |= NA_FLAG_OVERRIDE;
    }
    let mut body = Vec::with_capacity(16 + 8);
    body.extend_from_slice(&target);
    body.extend_from_slice(&lladdr_option(OPT_TARGET_LLADDR, target_lladdr));
    icmpv6::build(
        ICMPV6_NEIGHBOR_ADVERTISEMENT,
        0,
        [flags, 0, 0, 0],
        &body,
        src,
        dst,
    )
}

/// Build a Router Solicitation. The body is just the Source Link-Layer Address
/// option (no target). `dst` is normally the all-routers multicast `ff02::2`.
pub fn build_router_solicitation(src_lladdr: MacAddr, src: Ipv6Addr, dst: Ipv6Addr) -> Vec<u8> {
    let body = lladdr_option(OPT_SOURCE_LLADDR, src_lladdr);
    icmpv6::build(ICMPV6_ROUTER_SOLICITATION, 0, [0, 0, 0, 0], &body, src, dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::solicited_node_multicast;

    const LINK_LOCAL: Ipv6Addr = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01];
    const FEC0_3: Ipv6Addr = [0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03];
    // fec0::/64 prefix bytes (low 64 bits zero in a Prefix Information option).
    const FEC0_PREFIX: Ipv6Addr = [0xfe, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const TARGET: Ipv6Addr = [
        0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0x02, 0x11, 0x22, 0xff, 0xfe, 0x33, 0x44, 0x55,
    ];
    const MAC: MacAddr = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

    #[test]
    fn ns_round_trip() {
        let dst = solicited_node_multicast(&TARGET);
        let msg = build_neighbor_solicitation(TARGET, MAC, LINK_LOCAL, dst);

        let parsed = parse_neighbor_solicitation(&msg).unwrap();
        assert_eq!(parsed.target, TARGET);
        assert_eq!(parsed.src_lladdr, Some(MAC));
    }

    #[test]
    fn na_round_trip() {
        let msg = build_neighbor_advertisement(TARGET, MAC, true, true, LINK_LOCAL, LINK_LOCAL);

        let parsed = parse_neighbor_advertisement(&msg).unwrap();
        assert_eq!(parsed.target, TARGET);
        assert!(parsed.solicited);
        assert!(parsed.override_flag);
        assert!(!parsed.router); // a host never sets R.
        assert_eq!(parsed.target_lladdr, Some(MAC));
    }

    /// Hand-build a `radvd`-style Router Advertisement carrying a Prefix
    /// Information option, an RDNSS option, and a Source LLA option.
    fn build_sample_ra(managed: bool, router_lifetime: u16) -> Vec<u8> {
        // body: reachable_time(4) retrans_timer(4) then options.
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // reachable_time
        body.extend_from_slice(&0u32.to_be_bytes()); // retrans_timer

        // Prefix Information option (type 3, len=4 → 32 bytes).
        body.push(OPT_PREFIX_INFO);
        body.push(4);
        body.push(64); // prefix_length
        body.push(PIO_FLAG_ON_LINK | PIO_FLAG_AUTONOMOUS); // L + A
        body.extend_from_slice(&2_592_000u32.to_be_bytes()); // valid_lifetime
        body.extend_from_slice(&604_800u32.to_be_bytes()); // preferred_lifetime
        body.extend_from_slice(&0u32.to_be_bytes()); // reserved
        body.extend_from_slice(&FEC0_PREFIX); // prefix (16)

        // RDNSS option (type 25, one server → len = 1 + 2*1 = 3 → 24 bytes).
        body.push(OPT_RDNSS);
        body.push(3);
        body.extend_from_slice(&0u16.to_be_bytes()); // reserved
        body.extend_from_slice(&86_400u32.to_be_bytes()); // lifetime
        body.extend_from_slice(&FEC0_3); // server address (16)

        // Source Link-Layer Address option (type 1, len=1 → 8 bytes).
        body.extend_from_slice(&lladdr_option(OPT_SOURCE_LLADDR, MAC));

        let mut flags = RA_FLAG_OTHER & 0; // start clear
        if managed {
            flags |= RA_FLAG_MANAGED;
        }
        let rest = [
            64,
            flags,
            (router_lifetime >> 8) as u8,
            (router_lifetime & 0xff) as u8,
        ];
        icmpv6::build(
            ICMPV6_ROUTER_ADVERTISEMENT,
            0,
            rest,
            &body,
            LINK_LOCAL,
            [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01],
        )
    }

    #[test]
    fn ra_parse_full() {
        let msg = build_sample_ra(false, 1800);
        let ra = parse_router_advertisement(&msg).unwrap();

        assert_eq!(ra.cur_hop_limit, 64);
        assert!(!ra.managed);
        assert!(!ra.other);
        assert_eq!(ra.router_lifetime, 1800);

        let pi = ra.prefix.expect("prefix information present");
        assert_eq!(pi.prefix_length, 64);
        assert!(pi.on_link);
        assert!(pi.autonomous);
        assert_eq!(pi.valid_lifetime, 2_592_000);
        assert_eq!(pi.preferred_lifetime, 604_800);
        assert_eq!(pi.prefix, FEC0_PREFIX);

        assert_eq!(ra.rdnss, [FEC0_3]);
        assert_eq!(ra.src_lladdr, Some(MAC));
    }

    #[test]
    fn ra_zero_router_lifetime() {
        let msg = build_sample_ra(false, 0);
        let ra = parse_router_advertisement(&msg).unwrap();
        // The kernel uses lifetime==0 to NOT install a default route.
        assert_eq!(ra.router_lifetime, 0);
    }

    #[test]
    fn ra_managed_flag_surfaced() {
        let msg = build_sample_ra(true, 1800);
        let ra = parse_router_advertisement(&msg).unwrap();
        assert!(ra.managed);
    }

    #[test]
    fn parse_rejects_malformed() {
        // Empty buffer.
        assert!(parse_neighbor_solicitation(&[]).is_none());
        assert!(parse_neighbor_advertisement(&[]).is_none());
        assert!(parse_router_advertisement(&[]).is_none());

        // Wrong msg_type: build an NS, ask for an NA.
        let ns = build_neighbor_solicitation(TARGET, MAC, LINK_LOCAL, LINK_LOCAL);
        assert!(parse_neighbor_advertisement(&ns).is_none());

        // Valid ICMPv6 header but target truncated (only 8 of 16 target bytes).
        let short = icmpv6::build(
            ICMPV6_NEIGHBOR_SOLICITATION,
            0,
            [0, 0, 0, 0],
            &[0u8; 8],
            LINK_LOCAL,
            LINK_LOCAL,
        );
        assert!(parse_neighbor_solicitation(&short).is_none());

        // Truncated option (length claims 8 bytes but only 4 remain) must not
        // panic — the option walk stops and the message still parses.
        let mut bad_opt = Vec::new();
        bad_opt.extend_from_slice(&TARGET);
        bad_opt.extend_from_slice(&[OPT_SOURCE_LLADDR, 1, 0xAA, 0xBB]); // truncated
        let msg = icmpv6::build(
            ICMPV6_NEIGHBOR_SOLICITATION,
            0,
            [0, 0, 0, 0],
            &bad_opt,
            LINK_LOCAL,
            LINK_LOCAL,
        );
        let parsed = parse_neighbor_solicitation(&msg).unwrap();
        assert_eq!(parsed.target, TARGET);
        assert_eq!(parsed.src_lladdr, None); // truncated option ignored.

        // A zero-length option must stop the walk (no infinite loop).
        let mut zero_opt = Vec::new();
        zero_opt.extend_from_slice(&TARGET);
        zero_opt.extend_from_slice(&[OPT_SOURCE_LLADDR, 0]); // invalid len 0
        let msg = icmpv6::build(
            ICMPV6_NEIGHBOR_SOLICITATION,
            0,
            [0, 0, 0, 0],
            &zero_opt,
            LINK_LOCAL,
            LINK_LOCAL,
        );
        let parsed = parse_neighbor_solicitation(&msg).unwrap();
        assert_eq!(parsed.src_lladdr, None);
    }
}
