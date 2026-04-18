//! Device-to-IOMMU-unit mapping — Phase 55a Track B.1.
//!
//! Pure-logic translation of "which IOMMU unit owns this PCI BDF?" Callers
//! supply a list of [`IommuUnitDescriptor`] values (one per DRHD on an Intel
//! platform or one per IVHD block on an AMD platform). Each descriptor names
//! the vendor, register base, and the device scopes the unit claims.
//!
//! Lookup runs in `O(log N)` over a vector sorted by `(segment, bus_start)`.
//! That keeps hot-path DMA claim / release cheap even on platforms with many
//! units.
//!
//! # Test-first stub
//!
//! The tests in this file arrive in the first 55a-B commit; the
//! implementation arrives in the second commit. `build` and `lookup`
//! currently return placeholder values so the tests below fail, preserving
//! the TDD red-phase evidence in `git log`.

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Vendor discriminator
// ---------------------------------------------------------------------------

/// Vendor identifier for an [`IommuUnitDescriptor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IommuVendor {
    /// Intel VT-d (ACPI table "DMAR").
    Vtd,
    /// AMD-Vi / AMD IOMMU (ACPI table "IVRS").
    AmdVi,
}

// ---------------------------------------------------------------------------
// Device-scope range owned by one unit
// ---------------------------------------------------------------------------

/// Bus range (inclusive) a unit claims within a PCI segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeRange {
    /// PCI segment group.
    pub segment: u16,
    /// First bus number covered (inclusive).
    pub bus_start: u8,
    /// Last bus number covered (inclusive).
    pub bus_end: u8,
}

impl ScopeRange {
    /// `true` if the BDF falls within this range. Implementation lands in
    /// commit 2; stub returns `false` so the red-phase test fails.
    pub fn contains(&self, _segment: u16, _bus: u8) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Unit descriptor consumed by the map builder
// ---------------------------------------------------------------------------

/// A single IOMMU unit's public metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IommuUnitDescriptor {
    /// Zero-based index into the descriptor vector the caller built.
    pub unit_index: usize,
    /// Which vendor's hardware this unit is.
    pub vendor: IommuVendor,
    /// Physical MMIO register base.
    pub register_base: u64,
    /// Bus ranges this unit owns.
    pub scopes: Vec<ScopeRange>,
    /// `true` when the unit claims every BDF in its segment.
    pub include_all: bool,
}

// ---------------------------------------------------------------------------
// The map itself
// ---------------------------------------------------------------------------

/// Sorted lookup table from `(segment, bus)` to a unit descriptor index.
#[derive(Debug, Clone, Default)]
pub struct DeviceToUnitMap {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
struct Entry {
    segment: u16,
    bus_start: u8,
    bus_end: u8,
    unit_index: usize,
}

impl DeviceToUnitMap {
    /// Build a map from a slice of unit descriptors.
    ///
    /// Stub: returns an empty map. Replaced by the real impl in commit 2.
    pub fn build(_descriptors: &[IommuUnitDescriptor]) -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Look up the unit index that owns `(segment, bus, device, function)`.
    ///
    /// Stub: always returns `None`. Replaced in commit 2.
    pub fn lookup(
        &self,
        _segment: u16,
        _bus: u8,
        _device: u8,
        _function: u8,
    ) -> Option<usize> {
        None
    }

    /// Number of flattened scope entries (not unit count).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no unit has any scope.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn scope(segment: u16, bus_start: u8, bus_end: u8) -> ScopeRange {
        ScopeRange {
            segment,
            bus_start,
            bus_end,
        }
    }

    fn desc(
        unit_index: usize,
        vendor: IommuVendor,
        register_base: u64,
        scopes: Vec<ScopeRange>,
        include_all: bool,
    ) -> IommuUnitDescriptor {
        IommuUnitDescriptor {
            unit_index,
            vendor,
            register_base,
            scopes,
            include_all,
        }
    }

    #[test]
    fn empty_map_never_matches() {
        let map = DeviceToUnitMap::build(&[]);
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
        assert_eq!(map.lookup(0, 0, 0, 0), None);
        assert_eq!(map.lookup(0, 255, 0, 0), None);
    }

    #[test]
    fn single_scope_matches_within_bus_range() {
        let map = DeviceToUnitMap::build(&[desc(
            0,
            IommuVendor::Vtd,
            0xfed9_0000,
            vec![scope(0, 0x10, 0x1f)],
            false,
        )]);
        assert_eq!(map.lookup(0, 0x10, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x1f, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x15, 0x5, 0), Some(0));
    }

    #[test]
    fn single_scope_rejects_outside_bus_range() {
        let map = DeviceToUnitMap::build(&[desc(
            0,
            IommuVendor::Vtd,
            0xfed9_0000,
            vec![scope(0, 0x10, 0x1f)],
            false,
        )]);
        assert_eq!(map.lookup(0, 0x0f, 0, 0), None);
        assert_eq!(map.lookup(0, 0x20, 0, 0), None);
    }

    #[test]
    fn segment_mismatch_misses() {
        let map = DeviceToUnitMap::build(&[desc(
            0,
            IommuVendor::Vtd,
            0xfed9_0000,
            vec![scope(0, 0x00, 0xff)],
            false,
        )]);
        assert_eq!(map.lookup(1, 0x00, 0, 0), None);
        assert_eq!(map.lookup(1, 0x50, 0, 0), None);
    }

    #[test]
    fn include_all_covers_every_bus() {
        let map = DeviceToUnitMap::build(&[desc(
            0,
            IommuVendor::Vtd,
            0xfed9_0000,
            vec![],
            true,
        )]);
        for bus in [0u8, 1, 7, 127, 200, 255] {
            assert_eq!(map.lookup(0, bus, 0, 0), Some(0));
        }
    }

    #[test]
    fn include_all_uses_first_scope_segment_when_present() {
        let map = DeviceToUnitMap::build(&[desc(
            0,
            IommuVendor::AmdVi,
            0xf000_0000,
            vec![scope(2, 0x00, 0xff)],
            true,
        )]);
        assert_eq!(map.lookup(2, 0x50, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x50, 0, 0), None);
    }

    #[test]
    fn two_units_non_overlapping_route_correctly() {
        let map = DeviceToUnitMap::build(&[
            desc(
                0,
                IommuVendor::Vtd,
                0xfed9_0000,
                vec![scope(0, 0x00, 0x7f)],
                false,
            ),
            desc(
                1,
                IommuVendor::Vtd,
                0xfed9_1000,
                vec![scope(0, 0x80, 0xff)],
                false,
            ),
        ]);
        assert_eq!(map.lookup(0, 0x00, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x7f, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x80, 0, 0), Some(1));
        assert_eq!(map.lookup(0, 0xff, 0, 0), Some(1));
    }

    #[test]
    fn multiple_scopes_on_one_unit_all_match() {
        let map = DeviceToUnitMap::build(&[desc(
            0,
            IommuVendor::AmdVi,
            0xf000_0000,
            vec![scope(0, 0x00, 0x0f), scope(0, 0x40, 0x4f), scope(0, 0xa0, 0xaf)],
            false,
        )]);
        assert_eq!(map.lookup(0, 0x00, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x0f, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x10, 0, 0), None);
        assert_eq!(map.lookup(0, 0x40, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0x4f, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0xa5, 0, 0), Some(0));
        assert_eq!(map.lookup(0, 0xb0, 0, 0), None);
    }

    #[test]
    fn multi_segment_routing() {
        let map = DeviceToUnitMap::build(&[
            desc(
                0,
                IommuVendor::Vtd,
                0xfed9_0000,
                vec![scope(0, 0x00, 0xff)],
                false,
            ),
            desc(
                1,
                IommuVendor::Vtd,
                0xfed9_1000,
                vec![scope(1, 0x00, 0xff)],
                false,
            ),
        ]);
        assert_eq!(map.lookup(0, 0x50, 0, 0), Some(0));
        assert_eq!(map.lookup(1, 0x50, 0, 0), Some(1));
        assert_eq!(map.lookup(2, 0x50, 0, 0), None);
    }

    #[test]
    fn device_and_function_do_not_influence_lookup() {
        let map = DeviceToUnitMap::build(&[desc(
            0,
            IommuVendor::Vtd,
            0xfed9_0000,
            vec![scope(0, 0x10, 0x1f)],
            false,
        )]);
        for device in 0u8..32 {
            for function in 0u8..8 {
                assert_eq!(map.lookup(0, 0x10, device, function), Some(0));
            }
        }
    }

    #[test]
    fn scope_range_contains_matches_spec() {
        let s = scope(0, 0x10, 0x1f);
        assert!(s.contains(0, 0x10));
        assert!(s.contains(0, 0x1f));
        assert!(s.contains(0, 0x15));
        assert!(!s.contains(0, 0x0f));
        assert!(!s.contains(0, 0x20));
        assert!(!s.contains(1, 0x10));
    }
}
