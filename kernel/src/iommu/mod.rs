//! Kernel-side IOMMU subsystem — Phase 55a Track B.
//!
//! This module is the boot-time glue that takes decoded ACPI DMAR / IVRS
//! tables (produced by [`crate::acpi`] via the pure-logic decoders in
//! `kernel-core::iommu::tables`), hands them to the kernel-core builders
//! for unit descriptors and reserved-region sets, and caches the results
//! behind [`spin::Once`] for later tracks to consume.
//!
//! # What lives where
//!
//! - **Pure logic** — `kernel-core::iommu::{contract, tables, device_map,
//!   acpi_integration, iova, regions}`. Host-testable; no MMIO, no
//!   hardware dependencies. That is where the structural decoders, the
//!   `IommuUnit` trait, the IOVA allocator, the reserved-region algebra,
//!   and the device-to-unit map live.
//! - **Kernel-side glue** (this module) — locates ACPI tables at boot,
//!   calls the pure-logic builders, stashes the result. Future tracks
//!   (C for VT-d, D for AMD-Vi, E for the `DmaBuffer` rewrite) extend
//!   this module with hardware bring-up, fault handling, and per-device
//!   domain lifetime management.
//!
//! # Lock ordering
//!
//! The IOMMU subsystem orders its locks as:
//!
//! ```text
//! domain lock  →  unit lock  →  buddy-allocator lock
//! ```
//!
//! No reverse nesting is permitted. Driver-side locks never nest IOMMU-unit
//! locks; IOMMU-unit locks never nest buddy-allocator locks held by callers;
//! fault handlers run in IRQ context and must not take any lock a non-IRQ
//! path could hold for more than bounded work. The authoritative write-up is
//! `kernel-core::iommu::contract` module docs; this comment mirrors the rule
//! so grep from the kernel side finds it.

// Vendor-specific IOMMU implementations. Each vendor module owns its own
// hardware state and exposes an [`kernel_core::iommu::contract::IommuUnit`]
// impl. Track C lands VT-d (Intel) and the shared fault logger; Track D
// lands AMD-Vi. Both implement the same trait and pass the same contract
// suite (Track F.4), so a driver consuming `IommuUnit` is provably correct
// across vendors.
pub mod amd;
pub mod fault;
pub mod intel;

use alloc::vec::Vec;
use spin::Once;

use kernel_core::iommu::acpi_integration::{
    ReservedRegionSource, ReservedRegionSummary, iommu_units_from_dmar, iommu_units_from_ivrs,
    reserved_regions_from_tables,
};
use kernel_core::iommu::device_map::{DeviceToUnitMap, IommuUnitDescriptor, IommuVendor};
use kernel_core::iommu::regions::ReservedRegionSet;

// ---------------------------------------------------------------------------
// Cached results produced by `init` — populated once at boot, read-only
// thereafter.
// ---------------------------------------------------------------------------

static IOMMU_UNITS: Once<Vec<IommuUnitDescriptor>> = Once::new();
static DEVICE_MAP: Once<DeviceToUnitMap> = Once::new();
static RESERVED_REGIONS: Once<ReservedRegionSet> = Once::new();

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the IOMMU unit descriptor list from decoded ACPI tables.
///
/// - If DMAR is present (Intel VT-d), the DRHD entries become the unit
///   list and IVRS is logged as warning-and-ignored per B.1 acceptance.
/// - If only IVRS is present, the IVHD blocks become the unit list.
/// - If neither is present, an empty list is returned and the kernel
///   proceeds to E.3's identity-map fallback later in the boot sequence.
///
/// Results are cached: a second call returns the same slice.
pub fn iommu_units_from_acpi() -> &'static [IommuUnitDescriptor] {
    IOMMU_UNITS.call_once(|| {
        let dmar = crate::acpi::dmar_tables();
        let ivrs = crate::acpi::ivrs_tables();
        match (dmar, ivrs) {
            (Some(d), Some(_i)) => {
                log::warn!(
                    "[iommu] Both DMAR and IVRS present — preferring DMAR, ignoring IVRS \
                     (multi-vendor platform is unexpected)"
                );
                let descs = iommu_units_from_dmar(d);
                log_units(&descs);
                descs
            }
            (Some(d), None) => {
                let descs = iommu_units_from_dmar(d);
                log_units(&descs);
                descs
            }
            (None, Some(i)) => {
                let descs = iommu_units_from_ivrs(i);
                log_units(&descs);
                descs
            }
            (None, None) => {
                log::info!(
                    "[iommu] No DMAR or IVRS table present — IOMMU absent, identity fallback \
                     will engage"
                );
                Vec::new()
            }
        }
    })
}

/// Lookup `(segment, bus, device, function) -> unit_index` via the
/// cached device-to-unit map. Returns `None` when no IOMMU unit claims
/// the BDF (or when no IOMMU is present).
///
/// Kept accessible for Track E (`claim_pci_device`) to consume; the
/// `allow(dead_code)` attribute suppresses the Track B "no caller yet"
/// warning without hiding the public API.
#[allow(dead_code)]
pub fn device_to_unit(segment: u16, bus: u8, device: u8, function: u8) -> Option<usize> {
    let map = DEVICE_MAP.get()?;
    map.lookup(segment, bus, device, function)
}

/// Return the cached `ReservedRegionSet` built from the active IOMMU
/// table. Empty when no IOMMU is present or no reserved regions are
/// declared.
pub fn reserved_regions() -> &'static ReservedRegionSet {
    RESERVED_REGIONS.call_once(|| {
        let dmar = crate::acpi::dmar_tables();
        let ivrs = crate::acpi::ivrs_tables();
        let (set, summaries) = reserved_regions_from_tables(dmar, ivrs);
        log_reserved(&summaries);
        set
    })
}

/// Build the `ReservedRegionSet` for the active IOMMU tables.
///
/// Exposed as a named free function so later tracks (E.4's domain
/// pre-map helper) can call it with an explicit DmarTables reference
/// when they need the regions as part of domain creation rather than
/// through the cached accessor. Suppressed from the dead-code lint
/// because Track B is where the symbol is introduced; Track E will
/// add the call site.
#[allow(dead_code)]
pub fn reserved_regions_from_units() -> &'static ReservedRegionSet {
    reserved_regions()
}

/// Initialize the kernel-side IOMMU subsystem. Call once after
/// `acpi::init` has returned so that DMAR / IVRS tables are available.
///
/// After this call:
/// - [`iommu_units_from_acpi`] returns a stable slice.
/// - [`device_to_unit`] routes BDFs to unit indices.
/// - [`reserved_regions`] returns a stable set of firmware-reserved
///   ranges.
///
/// Later tracks will extend this with hardware bring-up and per-device
/// domain creation; Track B stops at the discovery + map construction
/// boundary.
pub fn init() {
    // Force the Once cells; ignore the returned slice / set — callers use
    // the public accessors above.
    let descs = iommu_units_from_acpi();
    // Build the device map from the descriptors and cache it.
    DEVICE_MAP.call_once(|| DeviceToUnitMap::build(descs));
    // Prime the reserved-region cache and log summaries.
    let _ = reserved_regions();

    if descs.is_empty() {
        log::info!("[iommu] init: no IOMMU units discovered");
    } else {
        log::info!("[iommu] init: {} IOMMU unit(s) discovered", descs.len());
    }
}

// ---------------------------------------------------------------------------
// Logging helpers
// ---------------------------------------------------------------------------

fn log_units(descs: &[IommuUnitDescriptor]) {
    for desc in descs {
        let vendor = match desc.vendor {
            IommuVendor::Vtd => "vtd",
            IommuVendor::AmdVi => "amdvi",
        };
        log::info!(
            "[iommu] unit[{}]: vendor={} register_base={:#x} include_all={} scopes={}",
            desc.unit_index,
            vendor,
            desc.register_base,
            desc.include_all,
            desc.scopes.len(),
        );
        for (i, scope) in desc.scopes.iter().enumerate() {
            log::info!(
                "[iommu]   scope[{}]: segment={} bus=[{:#04x}..={:#04x}]",
                i,
                scope.segment,
                scope.bus_start,
                scope.bus_end,
            );
        }
    }
}

fn log_reserved(summaries: &[ReservedRegionSummary]) {
    for s in summaries {
        let source = match s.source_table {
            ReservedRegionSource::DmarRmrr => "dmar_rmrr",
            ReservedRegionSource::IvrsIvmd => "ivrs_ivmd",
        };
        log::info!(
            "[iommu] reserved region: source={} index={} start={:#x} len={:#x}",
            source,
            s.source_index,
            s.start,
            s.len,
        );
    }
}
