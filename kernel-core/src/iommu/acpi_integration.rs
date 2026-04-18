//! Glue between decoded ACPI tables and the IOMMU subsystem types.
//!
//! Phase 55a Track B.1 / B.2.
//!
//! Two things live here:
//!
//! - [`iommu_units_from_dmar`] / [`iommu_units_from_ivrs`] convert decoded
//!   [`tables::DmarTables`] / [`tables::IvrsTables`] into a flat
//!   [`device_map::IommuUnitDescriptor`] list. The kernel-side wrapper picks
//!   one ACPI table and builds the map from this list; if both tables are
//!   present it prefers the first found and logs a warning (B.1 acceptance).
//!
//! - [`reserved_regions_from_dmar`] extracts RMRR regions into a
//!   [`regions::ReservedRegionSet`] for the shared reserved-region pre-map
//!   helper (E.4 / B.2 acceptance).
//!
//! # Test-first stub
//!
//! The tests in this file arrive in the first 55a-B commit; the
//! implementation bodies currently return empty / placeholder values so the
//! tests fail at runtime. Commit 2 replaces the stubs with the real logic.

use alloc::vec::Vec;

use super::device_map::IommuUnitDescriptor;
use super::regions::ReservedRegionSet;
use super::tables::{DmarTables, IvrsTables};

// ---------------------------------------------------------------------------
// Public types — reserved-region summaries
// ---------------------------------------------------------------------------

/// Summary of one reserved region for boot-time logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedRegionSummary {
    pub source_table: ReservedRegionSource,
    pub source_index: usize,
    pub start: u64,
    pub len: u64,
}

/// Source table for a reserved region — matches the vendor discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservedRegionSource {
    /// Intel DMAR RMRR sub-table.
    DmarRmrr,
    /// AMD IVRS IVMD sub-table (future; currently unused).
    IvrsIvmd,
}

// ---------------------------------------------------------------------------
// Unit descriptor extraction (stubs)
// ---------------------------------------------------------------------------

/// Build an [`IommuUnitDescriptor`] list from a decoded DMAR table.
///
/// Stub: returns empty. Real impl in commit 2.
pub fn iommu_units_from_dmar(_tables: &DmarTables) -> Vec<IommuUnitDescriptor> {
    Vec::new()
}

/// Build an [`IommuUnitDescriptor`] list from a decoded IVRS table.
///
/// Stub: returns empty. Real impl in commit 2.
pub fn iommu_units_from_ivrs(_tables: &IvrsTables) -> Vec<IommuUnitDescriptor> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// Reserved-region extraction (stubs)
// ---------------------------------------------------------------------------

/// Extract RMRR regions from a decoded DMAR table.
///
/// Stub: returns empty. Real impl in commit 2.
pub fn reserved_regions_from_dmar(
    _tables: &DmarTables,
) -> (ReservedRegionSet, Vec<ReservedRegionSummary>) {
    (ReservedRegionSet::new(), Vec::new())
}

/// Extract reserved regions from a decoded IVRS table.
///
/// AMD-Vi unity maps live in IVMD sub-tables which the Track A decoder does
/// not yet decode. This stub is a documented no-op; Track D will extend it
/// when IVMD support lands.
pub fn reserved_regions_from_ivrs(
    _tables: &IvrsTables,
) -> (ReservedRegionSet, Vec<ReservedRegionSummary>) {
    (ReservedRegionSet::new(), Vec::new())
}

/// Combined extraction path for both tables.
///
/// Stub: returns empty. Real impl in commit 2.
pub fn reserved_regions_from_tables(
    _dmar: Option<&DmarTables>,
    _ivrs: Option<&IvrsTables>,
) -> (ReservedRegionSet, Vec<ReservedRegionSummary>) {
    (ReservedRegionSet::new(), Vec::new())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::device_map::IommuVendor;
    use super::super::regions::RegionFlags;
    use super::super::tables::{
        DeviceScope, DmaRemappingUnit, DmarTables, IvhdBlock, IvhdDeviceEntry, IvrsTables,
        ReservedMemoryRegion,
    };
    use super::*;
    use alloc::vec;

    fn make_drhd(
        flags: u8,
        segment: u16,
        base: u64,
        scopes: Vec<DeviceScope>,
    ) -> DmaRemappingUnit {
        DmaRemappingUnit {
            flags,
            segment,
            register_base_address: base,
            device_scopes: scopes,
        }
    }

    fn make_rmrr(segment: u16, base: u64, limit: u64) -> ReservedMemoryRegion {
        ReservedMemoryRegion {
            segment,
            base_addr: base,
            limit_addr: limit,
            device_scopes: vec![],
        }
    }

    fn make_ivhd(
        block_type: u8,
        base: u64,
        segment: u16,
        entries: Vec<IvhdDeviceEntry>,
    ) -> IvhdBlock {
        IvhdBlock {
            block_type,
            flags: 0,
            length: 0,
            device_id: 0,
            capability_offset: 0,
            iommu_base_address: base,
            pci_segment: segment,
            iommu_info: 0,
            iommu_feature_info: 0,
            device_entries: entries,
        }
    }

    fn scope(bus: u8) -> DeviceScope {
        DeviceScope {
            scope_type: 1,
            length: 6,
            enumeration_id: 0,
            start_bus: bus,
            path: vec![(0, 0)],
        }
    }

    // ---- unit descriptor extraction ----

    #[test]
    fn dmar_with_no_drhds_produces_empty_descriptors() {
        let tables = DmarTables::default();
        let descs = iommu_units_from_dmar(&tables);
        assert!(descs.is_empty());
    }

    #[test]
    fn dmar_single_drhd_include_all_produces_full_bus_scope() {
        let mut tables = DmarTables::default();
        tables.drhds.push(make_drhd(0x01, 0, 0xfed9_0000, vec![]));
        let descs = iommu_units_from_dmar(&tables);
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].vendor, IommuVendor::Vtd);
        assert_eq!(descs[0].register_base, 0xfed9_0000);
        assert!(descs[0].include_all);
        assert_eq!(descs[0].scopes.len(), 1);
        assert_eq!(descs[0].scopes[0].bus_start, 0);
        assert_eq!(descs[0].scopes[0].bus_end, 255);
    }

    #[test]
    fn dmar_drhd_with_scopes_preserves_bus_starts() {
        let mut tables = DmarTables::default();
        tables.drhds.push(make_drhd(
            0x00,
            0,
            0xfed9_0000,
            vec![scope(0x10), scope(0x20)],
        ));
        let descs = iommu_units_from_dmar(&tables);
        assert!(!descs[0].include_all);
        let buses: Vec<u8> = descs[0].scopes.iter().map(|s| s.bus_start).collect();
        assert_eq!(buses, vec![0x10, 0x20]);
    }

    #[test]
    fn dmar_unit_index_tracks_position() {
        let mut tables = DmarTables::default();
        tables.drhds.push(make_drhd(0x01, 0, 0xfed9_0000, vec![]));
        tables.drhds.push(make_drhd(0x01, 0, 0xfed9_1000, vec![]));
        let descs = iommu_units_from_dmar(&tables);
        assert_eq!(descs[0].unit_index, 0);
        assert_eq!(descs[1].unit_index, 1);
    }

    #[test]
    fn ivrs_with_no_blocks_produces_empty_descriptors() {
        let tables = IvrsTables::default();
        let descs = iommu_units_from_ivrs(&tables);
        assert!(descs.is_empty());
    }

    #[test]
    fn ivrs_select_entry_maps_to_single_bus_scope() {
        let mut tables = IvrsTables::default();
        tables.ivhd_blocks.push(make_ivhd(
            0x10,
            0xf000_0000,
            0,
            vec![IvhdDeviceEntry::Select {
                device_id: 0x1050,
                data_setting: 0,
            }],
        ));
        let descs = iommu_units_from_ivrs(&tables);
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].vendor, IommuVendor::AmdVi);
        assert!(!descs[0].include_all);
        assert_eq!(descs[0].scopes.len(), 1);
        assert_eq!(descs[0].scopes[0].bus_start, 0x10);
        assert_eq!(descs[0].scopes[0].bus_end, 0x10);
    }

    #[test]
    fn ivrs_range_pair_maps_to_bus_range_scope() {
        let mut tables = IvrsTables::default();
        tables.ivhd_blocks.push(make_ivhd(
            0x11,
            0xf000_0000,
            0,
            vec![
                IvhdDeviceEntry::StartRange {
                    device_id: 0x2000,
                    data_setting: 0,
                },
                IvhdDeviceEntry::EndRange {
                    device_id: 0x3f00,
                    data_setting: 0,
                },
            ],
        ));
        let descs = iommu_units_from_ivrs(&tables);
        assert_eq!(descs[0].scopes.len(), 1);
        assert_eq!(descs[0].scopes[0].bus_start, 0x20);
        assert_eq!(descs[0].scopes[0].bus_end, 0x3f);
    }

    #[test]
    fn ivrs_empty_device_list_is_include_all() {
        let mut tables = IvrsTables::default();
        tables.ivhd_blocks.push(make_ivhd(0x40, 0xf000_0000, 0, vec![]));
        let descs = iommu_units_from_ivrs(&tables);
        assert!(descs[0].include_all);
        assert_eq!(descs[0].scopes[0].bus_start, 0);
        assert_eq!(descs[0].scopes[0].bus_end, 255);
    }

    // ---- reserved-region extraction ----

    #[test]
    fn dmar_with_no_rmrr_produces_empty_set() {
        let tables = DmarTables::default();
        let (set, summaries) = reserved_regions_from_dmar(&tables);
        assert!(set.is_empty());
        assert!(summaries.is_empty());
    }

    #[test]
    fn single_rmrr_becomes_reserved_region() {
        let mut tables = DmarTables::default();
        tables.rmrrs.push(make_rmrr(0, 0x1000, 0x1fff));
        let (set, summaries) = reserved_regions_from_dmar(&tables);
        assert_eq!(set.len(), 1);
        assert_eq!(summaries.len(), 1);
        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0x1000);
        // Inclusive limit: length = 0x1fff - 0x1000 + 1 = 0x1000
        assert_eq!(r.len, 0x1000);
        // Firmware-owned flag must be set.
        assert!(r.flags.bits() & RegionFlags::FIRMWARE_OWNED.bits() != 0);
    }

    #[test]
    fn two_overlapping_rmrrs_merge_into_one_region() {
        let mut tables = DmarTables::default();
        tables.rmrrs.push(make_rmrr(0, 0x1000, 0x2fff));
        tables.rmrrs.push(make_rmrr(0, 0x2000, 0x3fff));
        let (set, _summaries) = reserved_regions_from_dmar(&tables);
        assert_eq!(set.len(), 1);
        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0x1000);
        assert_eq!(r.len, 0x3000);
    }

    #[test]
    fn rmrr_summary_fields_match_input() {
        let mut tables = DmarTables::default();
        tables.rmrrs.push(make_rmrr(0, 0x8000, 0x8fff));
        tables.rmrrs.push(make_rmrr(0, 0xa000_0000, 0xa000_ffff));
        let (_set, summaries) = reserved_regions_from_dmar(&tables);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].source_table, ReservedRegionSource::DmarRmrr);
        assert_eq!(summaries[0].source_index, 0);
        assert_eq!(summaries[0].start, 0x8000);
        assert_eq!(summaries[0].len, 0x1000);
        assert_eq!(summaries[1].source_index, 1);
        assert_eq!(summaries[1].start, 0xa000_0000);
        assert_eq!(summaries[1].len, 0x1_0000);
    }

    #[test]
    fn rmrr_with_base_greater_than_limit_is_skipped() {
        let mut tables = DmarTables::default();
        tables.rmrrs.push(make_rmrr(0, 0x2000, 0x1000));
        let (set, summaries) = reserved_regions_from_dmar(&tables);
        assert!(set.is_empty());
        assert!(summaries.is_empty());
    }

    #[test]
    fn reserved_regions_from_tables_handles_none() {
        let (set, summaries) = reserved_regions_from_tables(None, None);
        assert!(set.is_empty());
        assert!(summaries.is_empty());
    }

    #[test]
    fn reserved_regions_from_tables_handles_dmar_only() {
        let mut dmar = DmarTables::default();
        dmar.rmrrs.push(make_rmrr(0, 0x1000, 0x1fff));
        let (set, summaries) = reserved_regions_from_tables(Some(&dmar), None);
        assert_eq!(set.len(), 1);
        assert_eq!(summaries.len(), 1);
    }

    #[test]
    fn reserved_regions_from_ivrs_is_noop_stub() {
        let mut ivrs = IvrsTables::default();
        ivrs.ivhd_blocks.push(make_ivhd(0x10, 0xf000_0000, 0, vec![]));
        let (set, summaries) = reserved_regions_from_ivrs(&ivrs);
        assert!(set.is_empty());
        assert!(summaries.is_empty());
    }
}
