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
//! # AMD-Vi unity-map extraction — scope
//!
//! Per the AMD I/O Virtualization spec, unity-map address regions live in
//! **IVMD** (I/O Virtualization Memory Definition) sub-tables of IVRS, not
//! inside IVHD device entries. The Track A decoder currently decodes IVHD
//! blocks (10h / 11h / 40h) but not IVMD; IVMD handling is planned for the
//! AMD-Vi implementation track (D). [`reserved_regions_from_ivrs`] therefore
//! returns an empty set, documented inline, and logs zero regions on AMD
//! platforms. When Track D lands an IVMD decoder this function will be
//! extended to populate the same set. This is explicitly recorded as a B.2
//! deviation in the commit trail.

use alloc::vec::Vec;

use super::device_map::{IommuUnitDescriptor, IommuVendor, ScopeRange};
use super::regions::{RegionFlags, ReservedRegion, ReservedRegionSet};
use super::tables::{DeviceScope, DmarTables, IvhdDeviceEntry, IvrsTables};

// ---------------------------------------------------------------------------
// Public types — reserved-region summaries
// ---------------------------------------------------------------------------

/// Summary of one reserved region for boot-time logging.
///
/// The kernel-side wrapper uses this to emit a single structured log line
/// per region without re-walking the RMRR list; keeping the summary in
/// `kernel-core` lets the host test that the boot-time log would cover
/// every region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedRegionSummary {
    /// Which ACPI table the region came from.
    pub source_table: ReservedRegionSource,
    /// Zero-based index within that table's list of reserved entries
    /// (e.g. RMRR index for DMAR).
    pub source_index: usize,
    /// Physical start address.
    pub start: u64,
    /// Length in bytes. Guaranteed >= 1 for every summarized region.
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
// Unit descriptor extraction
// ---------------------------------------------------------------------------

/// Build an [`IommuUnitDescriptor`] list from a decoded DMAR table.
///
/// One descriptor is produced per DRHD. A DRHD whose `flags` bit 0
/// (`INCLUDE_PCI_ALL`) is set expands to a single `0..=255` scope within
/// its segment; other DRHDs produce one [`ScopeRange`] per device-scope
/// entry covering `[start_bus, start_bus]` — bus-start granularity is
/// sufficient for Phase 55a routing.
pub fn iommu_units_from_dmar(tables: &DmarTables) -> Vec<IommuUnitDescriptor> {
    let mut out = Vec::with_capacity(tables.drhds.len());
    for (idx, drhd) in tables.drhds.iter().enumerate() {
        let include_all = (drhd.flags & 0x01) != 0;
        let scopes = if include_all {
            // Synthesize a full-bus scope so the map builder knows which
            // segment the `INCLUDE_PCI_ALL` unit lives in.
            alloc::vec![ScopeRange {
                segment: drhd.segment,
                bus_start: 0,
                bus_end: 255,
            }]
        } else {
            drhd_scopes_to_ranges(drhd.segment, &drhd.device_scopes)
        };
        out.push(IommuUnitDescriptor {
            unit_index: idx,
            vendor: IommuVendor::Vtd,
            register_base: drhd.register_base_address,
            scopes,
            include_all,
        });
    }
    out
}

/// Build an [`IommuUnitDescriptor`] list from a decoded IVRS table.
///
/// One descriptor is produced per IVHD block. A block with no decoded
/// device entries is treated as "claim the whole segment" (include-all).
pub fn iommu_units_from_ivrs(tables: &IvrsTables) -> Vec<IommuUnitDescriptor> {
    let mut out = Vec::with_capacity(tables.ivhd_blocks.len());
    for (idx, ivhd) in tables.ivhd_blocks.iter().enumerate() {
        let scopes = ivhd_entries_to_ranges(ivhd.pci_segment, &ivhd.device_entries);
        let include_all = scopes.is_empty();
        let scopes = if include_all {
            alloc::vec![ScopeRange {
                segment: ivhd.pci_segment,
                bus_start: 0,
                bus_end: 255,
            }]
        } else {
            scopes
        };
        out.push(IommuUnitDescriptor {
            unit_index: idx,
            vendor: IommuVendor::AmdVi,
            register_base: ivhd.iommu_base_address,
            scopes,
            include_all,
        });
    }
    out
}

/// Convert a DMAR DRHD's device-scope list into a flat [`ScopeRange`] vector.
///
/// Each device-scope entry's `start_bus` is the primary bus the scope
/// claims. Phase 55a uses bus-start granularity everywhere; a future phase
/// could look up secondary bus numbers via PCI config space to widen the
/// range.
fn drhd_scopes_to_ranges(segment: u16, scopes: &[DeviceScope]) -> Vec<ScopeRange> {
    scopes
        .iter()
        .map(|s| ScopeRange {
            segment,
            bus_start: s.start_bus,
            bus_end: s.start_bus,
        })
        .collect()
}

/// Convert an IVHD block's device-entry list into a [`ScopeRange`] vector.
///
/// Entries are processed left to right:
///
/// - `Select` produces a single-bus claim at `device_id >> 8`.
/// - A pending `StartRange` followed by `EndRange` produces a range
///   `[start_bus, end_bus]` using each entry's `device_id >> 8`.
/// - `AliasSelect` / `AliasStartRange` are treated like their non-alias
///   counterparts for bus-range purposes; the alias relationship is not
///   part of bus-granularity routing.
fn ivhd_entries_to_ranges(segment: u16, entries: &[IvhdDeviceEntry]) -> Vec<ScopeRange> {
    let mut out = Vec::new();
    let mut pending_start: Option<u8> = None;
    for entry in entries {
        match entry {
            IvhdDeviceEntry::Select { device_id, .. }
            | IvhdDeviceEntry::AliasSelect { device_id, .. } => {
                let bus = (device_id >> 8) as u8;
                out.push(ScopeRange {
                    segment,
                    bus_start: bus,
                    bus_end: bus,
                });
            }
            IvhdDeviceEntry::StartRange { device_id, .. }
            | IvhdDeviceEntry::AliasStartRange { device_id, .. } => {
                pending_start = Some((device_id >> 8) as u8);
            }
            IvhdDeviceEntry::EndRange { device_id, .. } => {
                if let Some(start) = pending_start.take() {
                    let end = (device_id >> 8) as u8;
                    out.push(ScopeRange {
                        segment,
                        bus_start: core::cmp::min(start, end),
                        bus_end: core::cmp::max(start, end),
                    });
                }
                // A stray EndRange with no preceding StartRange is ignored.
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reserved-region extraction — DMAR RMRRs
// ---------------------------------------------------------------------------

/// Extract RMRR regions from a decoded DMAR table into a [`ReservedRegionSet`].
///
/// Every RMRR becomes a [`ReservedRegion`] covering `[base_addr, limit_addr + 1)`
/// with the `FIRMWARE_OWNED`, `WRITABLE`, and `CACHEABLE` flags set (firmware
/// regions must remain identity-mapped writable to whoever owns them).
/// Overlapping or touching RMRRs merge via the set's insert path.
///
/// RMRRs whose `base > limit` (malformed firmware) are skipped without
/// panicking; no summary is emitted for skipped entries.
pub fn reserved_regions_from_dmar(
    tables: &DmarTables,
) -> (ReservedRegionSet, Vec<ReservedRegionSummary>) {
    let mut set = ReservedRegionSet::new();
    let mut summaries = Vec::with_capacity(tables.rmrrs.len());
    for (idx, rmrr) in tables.rmrrs.iter().enumerate() {
        if rmrr.base_addr > rmrr.limit_addr {
            continue;
        }
        // RMRR limit_addr is inclusive, so length = limit - base + 1.
        let len_u64 = rmrr
            .limit_addr
            .saturating_sub(rmrr.base_addr)
            .saturating_add(1);
        let len = if len_u64 > usize::MAX as u64 {
            usize::MAX
        } else {
            len_u64 as usize
        };
        set.insert(ReservedRegion {
            start: rmrr.base_addr,
            len,
            flags: RegionFlags::FIRMWARE_OWNED
                .union(RegionFlags::WRITABLE)
                .union(RegionFlags::CACHEABLE),
        });
        summaries.push(ReservedRegionSummary {
            source_table: ReservedRegionSource::DmarRmrr,
            source_index: idx,
            start: rmrr.base_addr,
            len: len_u64,
        });
    }
    (set, summaries)
}

/// Extract reserved regions from a decoded IVRS table.
///
/// AMD-Vi unity maps live in IVMD sub-tables which the Track A decoder does
/// not yet decode. This stub returns an empty set so the B.2 caller shape
/// stays consistent across vendors. Track D will extend this when IVMD
/// support lands.
pub fn reserved_regions_from_ivrs(
    _tables: &IvrsTables,
) -> (ReservedRegionSet, Vec<ReservedRegionSummary>) {
    (ReservedRegionSet::new(), Vec::new())
}

/// Combined extraction path for both tables.
///
/// Merges DMAR RMRR regions (currently the only source) with any future
/// IVRS IVMD regions. Returns the merged set together with the combined
/// summary list so the kernel wrapper can emit one log line per summary.
pub fn reserved_regions_from_tables(
    dmar: Option<&DmarTables>,
    ivrs: Option<&IvrsTables>,
) -> (ReservedRegionSet, Vec<ReservedRegionSummary>) {
    let mut set = ReservedRegionSet::new();
    let mut summaries = Vec::new();
    if let Some(d) = dmar {
        let (s, sm) = reserved_regions_from_dmar(d);
        set.union(&s);
        summaries.extend(sm);
    }
    if let Some(i) = ivrs {
        let (s, sm) = reserved_regions_from_ivrs(i);
        set.union(&s);
        summaries.extend(sm);
    }
    (set, summaries)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::device_map::IommuVendor;
    use super::super::regions::RegionFlags;
    use super::super::tables::{
        DeviceScope, DmaRemappingUnit, DmarTables, IvhdBlock, IvhdDeviceEntry, IvmdKind,
        IvrsMemDefinition, IvrsTables, ReservedMemoryRegion,
    };
    use super::*;
    use alloc::vec;

    fn make_drhd(flags: u8, segment: u16, base: u64, scopes: Vec<DeviceScope>) -> DmaRemappingUnit {
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
        tables
            .ivhd_blocks
            .push(make_ivhd(0x40, 0xf000_0000, 0, vec![]));
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

    fn make_ivmd(kind: IvmdKind, flags: u8, start: u64, length: u64) -> IvrsMemDefinition {
        IvrsMemDefinition {
            kind,
            flags,
            start_addr: start,
            length,
        }
    }

    #[test]
    fn reserved_regions_from_ivrs_with_no_ivmds_is_empty() {
        let mut ivrs = IvrsTables::default();
        ivrs.ivhd_blocks
            .push(make_ivhd(0x10, 0xf000_0000, 0, vec![]));
        let (set, summaries) = reserved_regions_from_ivrs(&ivrs);
        assert!(set.is_empty());
        assert!(summaries.is_empty());
    }

    #[test]
    fn single_ivmd_becomes_reserved_region() {
        let mut ivrs = IvrsTables::default();
        ivrs.ivmds
            .push(make_ivmd(IvmdKind::All, 0x01, 0xC000_0000, 0x0010_0000));
        let (set, summaries) = reserved_regions_from_ivrs(&ivrs);
        assert_eq!(set.len(), 1);
        assert_eq!(summaries.len(), 1);
        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0xC000_0000);
        assert_eq!(r.len, 0x0010_0000);
        // IVMDs must be flagged as firmware-owned and writable.
        assert!(r.flags.bits() & RegionFlags::FIRMWARE_OWNED.bits() != 0);
        assert!(r.flags.bits() & RegionFlags::WRITABLE.bits() != 0);

        assert_eq!(summaries[0].source_table, ReservedRegionSource::IvrsIvmd);
        assert_eq!(summaries[0].source_index, 0);
        assert_eq!(summaries[0].start, 0xC000_0000);
        assert_eq!(summaries[0].len, 0x0010_0000);
    }

    #[test]
    fn two_overlapping_ivmds_merge_into_one_region() {
        let mut ivrs = IvrsTables::default();
        // [0x1000..0x3000) and [0x2000..0x4000) overlap → merge to [0x1000..0x4000).
        ivrs.ivmds
            .push(make_ivmd(IvmdKind::All, 0x01, 0x1000, 0x2000));
        ivrs.ivmds.push(make_ivmd(
            IvmdKind::Select { device_id: 0x0018 },
            0x01,
            0x2000,
            0x2000,
        ));
        let (set, summaries) = reserved_regions_from_ivrs(&ivrs);
        assert_eq!(set.len(), 1);
        // Both summaries must still be emitted so logging sees every IVMD.
        assert_eq!(summaries.len(), 2);
        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0x1000);
        assert_eq!(r.len, 0x3000);
    }

    #[test]
    fn zero_length_ivmd_is_skipped() {
        let mut ivrs = IvrsTables::default();
        ivrs.ivmds
            .push(make_ivmd(IvmdKind::All, 0x01, 0x1000, 0));
        let (set, summaries) = reserved_regions_from_ivrs(&ivrs);
        assert!(set.is_empty());
        assert!(summaries.is_empty());
    }

    #[test]
    fn ivmd_summary_carries_source_index_for_each_entry() {
        let mut ivrs = IvrsTables::default();
        ivrs.ivmds
            .push(make_ivmd(IvmdKind::All, 0x01, 0x1000, 0x1000));
        ivrs.ivmds.push(make_ivmd(
            IvmdKind::Range {
                start_device_id: 0x0100,
                end_device_id: 0x01FF,
            },
            0x01,
            0x3000,
            0x1000,
        ));
        let (_set, summaries) = reserved_regions_from_ivrs(&ivrs);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].source_index, 0);
        assert_eq!(summaries[1].source_index, 1);
        assert!(matches!(
            summaries[0].source_table,
            ReservedRegionSource::IvrsIvmd
        ));
        assert!(matches!(
            summaries[1].source_table,
            ReservedRegionSource::IvrsIvmd
        ));
    }

    #[test]
    fn reserved_regions_from_tables_handles_ivrs_only() {
        let mut ivrs = IvrsTables::default();
        ivrs.ivmds
            .push(make_ivmd(IvmdKind::All, 0x01, 0xD000_0000, 0x4000));
        let (set, summaries) = reserved_regions_from_tables(None, Some(&ivrs));
        assert_eq!(set.len(), 1);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].source_table, ReservedRegionSource::IvrsIvmd);
    }

    #[test]
    fn reserved_regions_from_tables_merges_dmar_and_ivrs() {
        let mut dmar = DmarTables::default();
        dmar.rmrrs.push(make_rmrr(0, 0x1000, 0x1FFF));
        let mut ivrs = IvrsTables::default();
        ivrs.ivmds
            .push(make_ivmd(IvmdKind::All, 0x01, 0x1_0000, 0x1000));
        let (set, summaries) = reserved_regions_from_tables(Some(&dmar), Some(&ivrs));
        // Two disjoint regions, one from each table.
        assert_eq!(set.len(), 2);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].source_table, ReservedRegionSource::DmarRmrr);
        assert_eq!(summaries[1].source_table, ReservedRegionSource::IvrsIvmd);
    }
}
