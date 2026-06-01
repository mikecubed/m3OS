//! HDA PCI matching — host-testable pure logic (Phase 80b, Track B.1).
//!
//! HDA is identified primarily by PCI class/subclass **0x04/0x03**
//! (Multimedia Controller — High Definition Audio) as specified in the
//! PCI Local Bus specification and the Intel HD Audio spec rev 1.0a §2.
//!
//! The AC'97 controller (`0x8086:0x2415`) is class 0x04 / subclass **0x01**
//! (Audio Device) — its subclass distinguishes it from HDA and it MUST NOT be
//! accepted by this driver.  Gating on a single vendor ID (the AC'97 mistake)
//! would silently miss every non-Intel HDA controller (AMD, nVidia, VIA, …),
//! so the primary match is vendor-agnostic class/subclass; the vendor:device
//! table covers controllers that may enumerate with non-standard class codes.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Class / subclass / prog_if — HDA spec §2, PCI SIG class code database
// ---------------------------------------------------------------------------

/// PCI base class: Multimedia Controller.
pub const HDA_CLASS: u8 = 0x04;
/// PCI subclass: High Definition Audio controller (not AC'97 = 0x01).
pub const HDA_SUBCLASS: u8 = 0x03;
/// PCI prog-if: 0x00 (the only defined value for HDA).
pub const HDA_PROG_IF: u8 = 0x00;

// ---------------------------------------------------------------------------
// Vendor:device table — controllers worth recognising by ID even if their
// class code differs from the standard triple.
// ---------------------------------------------------------------------------

/// A (vendor, device) pair that identifies a specific HDA controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdaDeviceId {
    pub vendor: u16,
    pub device: u16,
}

/// Known HDA device IDs.  The primary match path uses the class/subclass
/// triple; this table supplements it for controllers that may enumerate with
/// incorrect or vendor-specific class codes.
pub const HDA_DEVICE_IDS: &[HdaDeviceId] = &[
    // AMD Raven Ridge / Renoir HDA (dev-laptop controller)
    HdaDeviceId {
        vendor: 0x1022,
        device: 0x15e3,
    },
    // Intel ICH6 HDA (original HD Audio silicon, QEMU default)
    HdaDeviceId {
        vendor: 0x8086,
        device: 0x2668,
    },
    // Intel ICH9 / ICH10 HDA (Ibex Peak)
    HdaDeviceId {
        vendor: 0x8086,
        device: 0x293e,
    },
    // Intel Sunrise Point HDA (Skylake/Kaby Lake PCH)
    HdaDeviceId {
        vendor: 0x8086,
        device: 0xa170,
    },
    // Intel Cannon Lake HDA
    HdaDeviceId {
        vendor: 0x8086,
        device: 0xa348,
    },
];

// ---------------------------------------------------------------------------
// Matching predicate
// ---------------------------------------------------------------------------

/// Returns `true` when the PCI function described by the arguments is an
/// HDA controller that the m3OS HDA driver should bind.
///
/// Match logic (in priority order):
///
/// 1. `class == HDA_CLASS && subclass == HDA_SUBCLASS` — vendor-agnostic
///    class match.  `prog_if` is accepted regardless of its value because
///    some firmware enumerates HDA with a non-zero prog_if.  This **excludes**
///    the AC'97 class 0x04/0x01 controller because its subclass ≠ 0x03.
///
/// 2. `(vendor, device)` is in `HDA_DEVICE_IDS` — catch controllers whose
///    class code is wrong or vendor-specific.
#[inline]
pub fn hda_pci_match(class: u8, subclass: u8, _prog_if: u8, vendor: u16, device: u16) -> bool {
    // Primary: vendor-agnostic class/subclass check (prog_if intentionally ignored).
    if class == HDA_CLASS && subclass == HDA_SUBCLASS {
        return true;
    }
    // Secondary: explicit vendor:device table.
    HDA_DEVICE_IDS
        .iter()
        .any(|id| id.vendor == vendor && id.device == device)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Class 0x04 / subclass 0x03 / prog_if 0x00 must match for any vendor.
    #[test]
    fn matches_class_040300_and_amd() {
        // Vendor-agnostic: an nVidia HDA with correct class/subclass
        assert!(
            hda_pci_match(0x04, 0x03, 0x00, 0x10de, 0xbeef),
            "class/subclass match must be vendor-agnostic"
        );
        // AMD dev-laptop controller — also matches via its entry in the table
        // (regardless of what class/subclass it was passed with)
        assert!(
            hda_pci_match(0x04, 0x03, 0x00, 0x1022, 0x15e3),
            "AMD 0x1022:0x15e3 must match via class"
        );
        // AMD via device table even if called with wrong class
        assert!(
            hda_pci_match(0xff, 0xff, 0x00, 0x1022, 0x15e3),
            "AMD 0x1022:0x15e3 must match via device table regardless of class"
        );
        // prog_if != 0x00 must still be accepted when class/subclass match
        assert!(
            hda_pci_match(0x04, 0x03, 0x01, 0x10de, 0x0040),
            "non-zero prog_if with correct class/subclass must still match"
        );
    }

    /// AC'97 controller (class 0x04 / subclass 0x01) MUST be rejected.
    #[test]
    fn rejects_ac97() {
        assert!(
            !hda_pci_match(0x04, 0x01, 0x00, 0x8086, 0x2415),
            "AC'97 0x8086:0x2415 (subclass 0x01) must not match"
        );
        // Generic AC'97 with any vendor must also be rejected by class check
        assert!(
            !hda_pci_match(0x04, 0x01, 0x00, 0x1234, 0x5678),
            "generic AC'97 class 0x04/0x01 must not match"
        );
    }

    /// The Intel ICH6 HDA device is in the table and must match.
    #[test]
    fn matches_ich6_via_table() {
        // Via class (normal enumeration)
        assert!(hda_pci_match(0x04, 0x03, 0x00, 0x8086, 0x2668));
        // Via table (abnormal / missing class)
        assert!(hda_pci_match(0x00, 0x00, 0x00, 0x8086, 0x2668));
    }

    /// HDA_CLASS / HDA_SUBCLASS constants must be correct per the PCI spec.
    #[test]
    fn class_constants_correct() {
        assert_eq!(HDA_CLASS, 0x04);
        assert_eq!(HDA_SUBCLASS, 0x03);
        assert_eq!(HDA_PROG_IF, 0x00);
    }

    /// No duplicate entries in the device-ID table.
    #[test]
    fn no_duplicate_device_ids() {
        for (i, a) in HDA_DEVICE_IDS.iter().enumerate() {
            for b in HDA_DEVICE_IDS.iter().skip(i + 1) {
                assert!(
                    !(a.vendor == b.vendor && a.device == b.device),
                    "duplicate HDA device id {:04x}:{:04x}",
                    a.vendor,
                    a.device
                );
            }
        }
    }
}
