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
//! # Design
//!
//! Addresses and IDs are plain scalar values (`u64`, `u16`, `u8`). This
//! module intentionally avoids any dependency on `x86_64` so it stays
//! host-testable. The acceptance criterion names `lookup(segment, bus,
//! device, function)` returning `Option<usize>` where the returned index is
//! into the caller's descriptor vector; that is exactly the shape exposed
//! here.

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
    /// `true` if the BDF falls within this range (segment matches and bus
    /// number is inside the inclusive range). Device and function fields
    /// are ignored because DRHD / IVHD scopes operate at bus granularity;
    /// filtering by specific `(device, function)` pairs is left to future
    /// phases that need it.
    pub fn contains(&self, segment: u16, bus: u8) -> bool {
        self.segment == segment && bus >= self.bus_start && bus <= self.bus_end
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
struct Entry {
    segment: u16,
    bus_start: u8,
    bus_end: u8,
    unit_index: usize,
}

impl DeviceToUnitMap {
    /// Build a map from a slice of unit descriptors.
    ///
    /// Each descriptor contributes one entry per scope in its `scopes`
    /// vector. Descriptors with `include_all == true` contribute a single
    /// `bus_start=0, bus_end=255` entry, whose segment is inferred from
    /// the descriptor's first scope (or 0 when the scope vector is empty).
    ///
    /// Callers must pass descriptors whose `unit_index` equals the slice
    /// position; that invariant is preserved by `iommu_units_from_acpi`.
    pub fn build(descriptors: &[IommuUnitDescriptor]) -> Self {
        let mut entries = Vec::new();
        for desc in descriptors {
            if desc.include_all {
                // Derive the segment from the descriptor's first scope;
                // fall back to 0 if the scope list is empty.
                let segment = desc.scopes.first().map(|s| s.segment).unwrap_or(0);
                entries.push(Entry {
                    segment,
                    bus_start: 0,
                    bus_end: 255,
                    unit_index: desc.unit_index,
                });
            } else {
                for scope in &desc.scopes {
                    entries.push(Entry {
                        segment: scope.segment,
                        bus_start: scope.bus_start,
                        bus_end: scope.bus_end,
                        unit_index: desc.unit_index,
                    });
                }
            }
        }
        // Sort by (segment, bus_start) so binary search can find the
        // rightmost candidate in O(log N).
        entries.sort_by_key(|e| (e.segment, e.bus_start));
        Self { entries }
    }

    /// Look up the unit index that owns `(segment, bus, device, function)`.
    ///
    /// Returns `None` if no unit claims the BDF. `device` and `function`
    /// are accepted for forward compatibility but currently ignored —
    /// matching is at bus granularity because that is the level at which
    /// DRHD / IVHD scopes declare their claims.
    pub fn lookup(&self, segment: u16, bus: u8, _device: u8, _function: u8) -> Option<usize> {
        // Binary-search for the rightmost entry with `(segment, bus_start)
        // <= (segment, bus)`; confirm `bus_end >= bus`.
        let target = (segment, bus);
        let idx = match self
            .entries
            .binary_search_by_key(&target, |e| (e.segment, e.bus_start))
        {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let entry = self.entries.get(idx)?;
        if entry.segment == segment && bus >= entry.bus_start && bus <= entry.bus_end {
            Some(entry.unit_index)
        } else {
            None
        }
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
        let map = DeviceToUnitMap::build(&[desc(0, IommuVendor::Vtd, 0xfed9_0000, vec![], true)]);
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
            vec![
                scope(0, 0x00, 0x0f),
                scope(0, 0x40, 0x4f),
                scope(0, 0xa0, 0xaf),
            ],
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
