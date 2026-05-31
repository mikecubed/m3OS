//! NIC PCI vendor/device-ID tables and family matching — Phase 79.
//!
//! Single source of truth for the vendor:device IDs each ring-3 NIC driver
//! family claims. The drivers themselves are `no_std` binaries; placing the ID
//! sets and the `is_<family>` predicates here lets the same logic be exercised
//! from host tests (`cargo test -p kernel-core`) — including the cross-family
//! **exclusion** invariants the Phase 79 acceptance criteria require (e.g. igb
//! must claim no igc ID, r8125 must not bind the 1GbE `0x8168`).
//!
//! ## Driver-routing rule (mirrors Linux `drivers/net/ethernet/intel`)
//!
//! Among the Intel families the ID sets are **disjoint**: a given device ID is
//! claimed by exactly one of e1000 / e1000e / igb / igc. igb claims the
//! 82575/82576/I210/I211/I350/I354 parts; igc claims **only** I225/I226. The
//! Realtek families are likewise disjoint: r8169 owns the 1GbE PCIe parts,
//! r8125 owns the 2.5GbE `0x8125`/`0x8126`.
//!
//! ## Device-ID accuracy
//!
//! IDs are cross-verified against the upstream Linux driver headers and
//! `pci.ids`. Three errors carried by the original Phase 79 draft are
//! corrected: RTL8125 is `0x8125` (not the 1GbE `0x8161`); `0x8168` is the
//! RTL8111/8168 **Gigabit** family (not the parallel-PCI RTL8169 `0x8169`);
//! and the e1000e set is expanded to include the common I218/I219 IDs.

/// Intel PCI vendor ID (`0x8086`).
pub const VENDOR_INTEL: u16 = 0x8086;

/// Realtek PCI vendor ID (`0x10EC`).
pub const VENDOR_REALTEK: u16 = 0x10EC;

/// PCI class/subclass/prog_if triple identifying an Ethernet controller.
/// Class `0x02` = Network Controller, subclass `0x00` = Ethernet, prog_if `0x00`.
pub const ETHERNET_CLASS: u8 = 0x02;
pub const ETHERNET_SUBCLASS: u8 = 0x00;
pub const ETHERNET_PROG_IF: u8 = 0x00;

/// Classic e1000 (82540EM) — the single ID the existing ring-3 e1000 driver
/// binds. QEMU's `-device e1000` / `-device e1000-82540em` is `0x100E`.
pub const E1000_IDS: &[u16] = &[0x100E];

/// e1000e family: 82574, 82583, 82579, I217, I218, I219.
/// QEMU's `-device e1000e` models the 82574L (`0x10D3`).
pub const E1000E_IDS: &[u16] = &[
    0x10D3, // 82574L
    0x10F6, // 82574LA
    0x150C, // 82583V
    0x1502, // 82579LM
    0x1503, // 82579V
    0x153A, // I217-LM
    0x153B, // I217-V
    // I218
    0x155A, 0x1559, 0x15A0, 0x15A1, 0x15A2, 0x15A3, // I219 (representative span)
    0x156F, 0x1570, 0x15B7, 0x15B8, 0x15B9, 0x15BA, 0x15BB, 0x15BC, 0x15BD, 0x15BE,
];

/// igb family: 82575, 82576, I350, I210, I211, I354.
/// QEMU's `-device igb` models the 82576 (`0x10C9`).
pub const IGB_IDS: &[u16] = &[
    // 82575
    0x10A7, 0x10A9, 0x10D6, // 82576
    0x10C9, 0x10E6, 0x10E7, 0x10E8, 0x1526, 0x150A, 0x1518, 0x150D, // I350
    0x1521, 0x1522, 0x1523, 0x1524, // I210
    0x1533, 0x1536, 0x1537, 0x1538, 0x157B, 0x157C, // I211
    0x1539, // I354
    0x1F40, 0x1F41, 0x1F45,
];

/// igc family: discrete Foxville 2.5GbE PCIe controllers — I225 / I226 only.
pub const IGC_IDS: &[u16] = &[
    // I225
    0x15F2, 0x15F3, 0x15F8, 0x0D9F, 0x3100, 0x3101, 0x5502, // I226
    0x125B, 0x125C, 0x125D, 0x3102, 0x5503,
];

/// Realtek r8169 1GbE family: RTL8111/8168 PCIe Gigabit (`0x8168`, the common
/// modern part), the original parallel-PCI RTL8169 (`0x8169`), 8168 variants
/// (`0x8161`/`0x8167`), and the RTL810xE Fast Ethernet (`0x8136`).
pub const R8169_IDS: &[u16] = &[0x8168, 0x8169, 0x8161, 0x8167, 0x8136];

/// Realtek RTL8125 2.5GbE family: `0x8125` (RTL8125/8125B); `0x8126` is the
/// opportunistically-matched RTL8126 5GbE part.
pub const R8125_IDS: &[u16] = &[0x8125, 0x8126];

/// Return true when `device_id` is in `ids`.
#[inline]
pub fn matches(ids: &[u16], device_id: u16) -> bool {
    ids.contains(&device_id)
}

/// True for the classic 82540EM e1000.
#[inline]
pub fn is_e1000(device_id: u16) -> bool {
    matches(E1000_IDS, device_id)
}

/// True for an e1000e-family device.
#[inline]
pub fn is_e1000e(device_id: u16) -> bool {
    matches(E1000E_IDS, device_id)
}

/// True for an igb-family device.
#[inline]
pub fn is_igb(device_id: u16) -> bool {
    matches(IGB_IDS, device_id)
}

/// True for an igc-family device (I225/I226 only).
#[inline]
pub fn is_igc(device_id: u16) -> bool {
    matches(IGC_IDS, device_id)
}

/// True for an r8169 1GbE-family device.
#[inline]
pub fn is_r8169(device_id: u16) -> bool {
    matches(R8169_IDS, device_id)
}

/// True for an RTL8125/8126 2.5G+-family device.
#[inline]
pub fn is_r8125(device_id: u16) -> bool {
    matches(R8125_IDS, device_id)
}

// ---------------------------------------------------------------------------
// Multi-NIC registry routing — Phase 79 Track E.1.
// ---------------------------------------------------------------------------
//
// The kernel `RemoteNic` registry was a single `Option<NicEntry>` slot; Phase
// 79 lifts it to a bounded `Vec<NicEntry>`. These pure helpers carry the
// default-route + RX-routing policy so they are host-testable independent of
// the kernel locking/IPC machinery. Multi-NIC *routing tables* are out of
// scope for 1.0 — the policy here is simply "first registered is the default
// interface", with RX steered to the NIC whose MAC matches the frame's
// destination (falling back to the default for broadcast/multicast).

/// Maximum number of ring-3 NIC drivers the kernel registry holds at once.
pub const MAX_NICS: usize = 8;

/// Index of the default-route NIC given `nic_count` registered entries.
///
/// "First registered wins": the default interface is index 0 when any NIC is
/// present, else `None`.
#[inline]
pub fn default_route_index(nic_count: usize) -> Option<usize> {
    if nic_count == 0 { None } else { Some(0) }
}

/// Index of the NIC a received frame addressed to `dest_mac` should be routed
/// to, given the registered NICs' MAC addresses in registration order.
///
/// Exact-MAC match wins; a frame whose destination matches no NIC (broadcast,
/// multicast, or a foreign unicast accepted in promiscuous mode) routes to the
/// default interface (index 0) when any NIC is present.
pub fn rx_route_index(macs: &[[u8; 6]], dest_mac: &[u8; 6]) -> Option<usize> {
    if let Some(i) = macs.iter().position(|m| m == dest_mac) {
        return Some(i);
    }
    default_route_index(macs.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_ids_match_pci_ids_database() {
        assert_eq!(VENDOR_INTEL, 0x8086);
        assert_eq!(VENDOR_REALTEK, 0x10EC);
    }

    #[test]
    fn ethernet_class_triple_is_correct() {
        assert_eq!(
            (ETHERNET_CLASS, ETHERNET_SUBCLASS, ETHERNET_PROG_IF),
            (0x02, 0x00, 0x00)
        );
    }

    // --- A.1: e1000e ID set ---

    #[test]
    fn e1000e_matches_representative_set() {
        for &id in &[
            0x10D3u16, 0x10F6, 0x150C, 0x1502, 0x1503, 0x153A, 0x153B, 0x155A, 0x1559, 0x15A0,
            0x15A1, 0x15A2, 0x15A3, 0x156F, 0x1570, 0x15B7, 0x15BE,
        ] {
            assert!(is_e1000e(id), "e1000e must match {id:#06x}");
        }
        // QEMU's -device e1000e is the 82574L.
        assert!(is_e1000e(0x10D3));
    }

    #[test]
    fn e1000e_does_not_claim_classic_e1000() {
        assert!(!is_e1000e(0x100E));
        assert!(is_e1000(0x100E));
    }

    // --- B.1: igb ID set + exclusion ---

    #[test]
    fn igb_matches_required_ids() {
        for &id in &[
            0x10A7u16, 0x10A9, 0x10D6, 0x10C9, 0x1521, 0x1522, 0x1523, 0x1524, 0x1533, 0x1536,
            0x1537, 0x1538, 0x157B, 0x157C, 0x1539, 0x1F40, 0x1F41, 0x1F45,
        ] {
            assert!(is_igb(id), "igb must match {id:#06x}");
        }
        // QEMU's -device igb models the 82576.
        assert!(is_igb(0x10C9));
    }

    #[test]
    fn igb_claims_no_e1000e_or_igc_id() {
        for &id in E1000E_IDS {
            assert!(!is_igb(id), "igb must not claim e1000e id {id:#06x}");
        }
        for &id in IGC_IDS {
            assert!(!is_igb(id), "igb must not claim igc id {id:#06x}");
        }
    }

    // --- B.2: igc ID set + exclusion ---

    #[test]
    fn igc_matches_only_i225_i226() {
        for &id in &[
            0x15F2u16, 0x15F3, 0x15F8, 0x0D9F, 0x3100, 0x3101, 0x5502, 0x125B, 0x125C, 0x125D,
            0x3102, 0x5503,
        ] {
            assert!(is_igc(id), "igc must match {id:#06x}");
        }
    }

    #[test]
    fn igc_claims_no_igb_id() {
        for &id in IGB_IDS {
            assert!(!is_igc(id), "igc must not claim igb id {id:#06x}");
        }
        // i210/i211 → igb, not igc.
        assert!(is_igb(0x1533) && !is_igc(0x1533));
        assert!(is_igb(0x1539) && !is_igc(0x1539));
    }

    // --- C.1: Realtek GbE set ---

    #[test]
    fn r8169_matches_realtek_gbe_set() {
        for &id in &[0x8168u16, 0x8169, 0x8161, 0x8167, 0x8136] {
            assert!(is_r8169(id), "r8169 must match {id:#06x}");
        }
    }

    // --- D.1: RTL8125 corrected ID + exclusion ---

    #[test]
    fn r8125_binds_0x8125_not_0x8161() {
        assert!(is_r8125(0x8125));
        assert!(!is_r8125(0x8161), "0x8161 is a 1GbE part, not 2.5G");
        assert!(is_r8169(0x8161));
    }

    #[test]
    fn r8125_and_r8169_are_disjoint() {
        for &id in R8125_IDS {
            assert!(!is_r8169(id), "r8169 must not claim 2.5G id {id:#06x}");
        }
        for &id in R8169_IDS {
            assert!(!is_r8125(id), "r8125 must not claim 1GbE id {id:#06x}");
        }
    }

    #[test]
    fn all_intel_families_pairwise_disjoint() {
        let sets: [(&str, &[u16]); 4] = [
            ("e1000", E1000_IDS),
            ("e1000e", E1000E_IDS),
            ("igb", IGB_IDS),
            ("igc", IGC_IDS),
        ];
        for (i, (na, a)) in sets.iter().enumerate() {
            for (nb, b) in sets.iter().skip(i + 1) {
                for &id in *a {
                    assert!(!b.contains(&id), "{na} id {id:#06x} also in {nb}");
                }
            }
        }
    }

    // --- E.1: multi-NIC registry routing ---

    #[test]
    fn default_route_is_first_registered() {
        assert_eq!(default_route_index(0), None);
        assert_eq!(default_route_index(1), Some(0));
        assert_eq!(default_route_index(2), Some(0));
        assert_eq!(default_route_index(MAX_NICS), Some(0));
    }

    #[test]
    fn rx_routes_to_matching_nic_index_else_default() {
        let a = [0x52, 0x54, 0x00, 0xAA, 0xAA, 0xAA];
        let b = [0x52, 0x54, 0x00, 0xBB, 0xBB, 0xBB];
        let macs = [a, b];
        // Exact match → that NIC's index.
        assert_eq!(rx_route_index(&macs, &a), Some(0));
        assert_eq!(rx_route_index(&macs, &b), Some(1));
        // Broadcast (no MAC match) → default interface (index 0).
        assert_eq!(rx_route_index(&macs, &[0xFF; 6]), Some(0));
        // Empty registry → no route.
        assert_eq!(rx_route_index(&[], &a), None);
    }

    #[test]
    fn no_duplicate_ids_within_a_family() {
        for set in [E1000E_IDS, IGB_IDS, IGC_IDS, R8169_IDS, R8125_IDS] {
            for (i, &a) in set.iter().enumerate() {
                for &b in &set[i + 1..] {
                    assert_ne!(a, b, "duplicate id {a:#06x} within a family set");
                }
            }
        }
    }
}
