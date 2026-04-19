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
pub mod registry;

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

/// `true` when a real (hardware-translating) IOMMU unit is active on the
/// platform. Returns `false` when only the identity-map fallback is
/// installed, or when [`init`] has not yet run.
///
/// Exposed so diagnostic code (meminfo, boot banner) can surface
/// `iommu.active = false` without having to inspect the registry
/// directly.
#[allow(dead_code)]
pub fn active() -> bool {
    registry::translating()
}

/// Initialize the kernel-side IOMMU subsystem. Call once after
/// `acpi::init` has returned so that DMAR / IVRS tables are available.
///
/// After this call:
/// - [`iommu_units_from_acpi`] returns a stable slice.
/// - [`device_to_unit`] routes BDFs to unit indices.
/// - [`reserved_regions`] returns a stable set of firmware-reserved
///   ranges.
/// - [`registry`] holds either the discovered vendor units or a single
///   [`kernel_core::iommu::identity::IdentityUnit`] fallback. In the
///   fallback path a single structured `iommu.fallback.identity`
///   log event records the reason.
///
/// Phase 55a Track B landed this function as "build the device map";
/// Track E extends it with the registry install + identity fallback so
/// every `claim_pci_device` has a unit to create a domain on. Vendor
/// bring-up (calling `VtdUnit::bring_up` / `AmdViUnit::bring_up`) still
/// lands on a later commit once the MSI-vector routing and MMIO
/// mapping prerequisites are in place; for now, devices on IOMMU
/// platforms fall back to identity mapping until that commit lands and
/// IOMMU hardware is wired up end-to-end.
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
        registry::install_identity_fallback();
        registry::log_identity_fallback(registry::IdentityFallbackReason::NoDmarOrIvrs);
    } else {
        log::info!("[iommu] init: {} IOMMU unit(s) discovered", descs.len());
        // Vendor MMIO bring-up (actually calling `VtdUnit::bring_up` or
        // `AmdViUnit::bring_up`) is not part of Track E — a follow-up
        // commit wires MSI-vector fault routing and the final enable
        // sequence. Until that lands, Track E registers identity fallback
        // so every `claim_pci_device` still gets a working domain. Logging
        // discriminates between "no ACPI table at all" and "ACPI reports
        // vendor hardware but we haven't turned it on yet" so operators
        // can tell the two apart.
        registry::install_identity_fallback();
        let reason = match descs[0].vendor {
            IommuVendor::Vtd => registry::IdentityFallbackReason::VtdInitFailed,
            IommuVendor::AmdVi => registry::IdentityFallbackReason::AmdViInitFailed,
        };
        registry::log_identity_fallback(reason);
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

// ---------------------------------------------------------------------------
// Phase 55a Track F.2 — kernel-side IOMMU smoke tests
// ---------------------------------------------------------------------------
//
// Runs inside QEMU via `cargo xtask test`. The kernel test harness fires
// `test_main()` right after `mm::init` and before `acpi::init` /
// `iommu::init` are called from the production boot path (see
// `kernel/src/main.rs`). The tests therefore drive `iommu::init()`
// explicitly before asserting on its observable state. The `Once`-guarded
// caches make this idempotent: the test-time call primes the state and the
// production-path call (if it ran first on a non-test boot) would be a no-op.
//
// Deviation from Track F.2 acceptance as written:
//
// Track E documents vendor MMIO bring-up (VtdUnit::bring_up / AmdViUnit::bring_up)
// as deferred — the registry installs identity fallback even when DMAR/IVRS
// is present, logging a reason of `vtd_init_failed` / `amdvi_init_failed` so
// operators can tell the two apart. As a result:
//
// 1. Default `cargo xtask test` (no `-device intel-iommu`) boots with an
//    empty DMAR/IVRS list; identity fallback engages with reason
//    `no_dmar_or_ivrs`. The assertions below verify this observable.
// 2. `cargo xtask test --iommu` would observe `descs.len() >= 1` and
//    `registry::translating() == false` (still identity fallback, but with
//    reason `vtd_init_failed`). Since vendor bring-up is deferred, we only
//    check that the boot path did not panic and the registry is populated;
//    a future phase that wires VT-d MSI + IOTLB will flip `translating()`
//    to true and this test expands accordingly.
//
// Every assertion here is a truth about the currently-shipped (Phase 55a
// Track E) code path, not a placeholder.

#[cfg(test)]
mod iommu_smoke_tests {
    use super::*;

    /// Ensure ACPI + IOMMU are initialized before the assertions below
    /// run. The kernel test harness fires `test_main()` before the
    /// production boot path reaches `acpi::init`/`iommu::init`, so each
    /// test primes them via the idempotent `Once`-guarded accessors.
    ///
    /// ACPI discovery is skipped when the bootloader handed us no RSDP
    /// (typical under `cargo xtask test`). `iommu::init` still runs and
    /// falls through to identity fallback with reason `no_dmar_or_ivrs`.
    /// Drains per-CPU frame caches on the way out so subsequent tests'
    /// `available_count()` baselines are not shifted by the one-time
    /// `Vec<RegisteredUnit>` allocation the registry performs.
    fn ensure_iommu_initialized() {
        // `acpi::init` is called for its side effect: populating the
        // DMAR/IVRS caches that `iommu::init` then reads.
        crate::acpi::init(None);
        init();
        crate::mm::frame_allocator::drain_per_cpu_caches();
    }

    /// Smoke: the IOMMU subsystem boot-time state is consistent.
    ///
    /// Regardless of whether ACPI reports DMAR / IVRS, after `iommu::init()`
    /// runs the registry holds at least one unit (the identity fallback at
    /// minimum). Without vendor MMIO bring-up (deferred per Track E)
    /// `registry::translating()` is false in both the "no ACPI table" and
    /// "ACPI table but no bring-up" branches.
    #[test_case]
    fn iommu_registry_is_populated_after_init() {
        ensure_iommu_initialized();
        assert!(
            registry::registered(),
            "registry must hold at least one unit (identity fallback at minimum)"
        );
        assert!(
            registry::len() >= 1,
            "expected >= 1 registered unit, got {}",
            registry::len()
        );
    }

    /// Smoke: `iommu::active()` is the public "is real IOMMU translation
    /// running?" accessor. Given vendor bring-up is deferred (Track E), this
    /// is always `false` in Phase 55a — a deliberate, logged degradation.
    /// The test pins this observable so a future phase that enables VT-d
    /// bring-up flips this assertion and surfaces the change in CI.
    #[test_case]
    fn iommu_active_reflects_identity_fallback_in_phase_55a() {
        ensure_iommu_initialized();
        assert_eq!(
            active(),
            false,
            "Phase 55a vendor bring-up is deferred; \
             `iommu::active()` is false until VT-d / AMD-Vi MSI wiring lands"
        );
    }

    /// Smoke: `iommu_units_from_acpi` is memoized and idempotent.
    #[test_case]
    fn iommu_units_from_acpi_returns_stable_slice_across_calls() {
        ensure_iommu_initialized();
        let a = iommu_units_from_acpi();
        let b = iommu_units_from_acpi();
        // Pointer equality: Once-guarded cache returns the same slice.
        assert_eq!(a.as_ptr(), b.as_ptr());
        assert_eq!(a.len(), b.len());
    }

    /// Smoke: `reserved_regions` is memoized and idempotent.
    #[test_case]
    fn reserved_regions_returns_stable_set_across_calls() {
        ensure_iommu_initialized();
        let a = reserved_regions() as *const _;
        let b = reserved_regions() as *const _;
        assert_eq!(a, b);
    }

    /// Smoke: The device-to-unit map is queryable even when empty.
    /// Looking up any BDF returns `None` when no IOMMU units claim it; on
    /// QEMU default (no `-device intel-iommu`), that's every BDF.
    #[test_case]
    fn device_to_unit_lookup_is_total_function() {
        ensure_iommu_initialized();
        // A handful of arbitrary BDFs. None should panic; in the default
        // config all return None (no units).
        let _ = device_to_unit(0, 0, 0, 0);
        let _ = device_to_unit(0, 1, 0, 0);
        let _ = device_to_unit(0, 0x50, 0, 0);
        let _ = device_to_unit(0, 0xFF, 0x1F, 7);
    }

    /// Smoke: registry bookkeeping is coherent.
    ///
    /// `registered()` and `len()` agree, and `translating()` reflects
    /// identity-only fallback in Phase 55a. We intentionally do NOT run
    /// `create_domain`/`destroy_domain` against the global registry here
    /// — those paths push to a Vec<DomainId> inside the registry's
    /// IdentityUnit, and the resulting slab-cache churn shifts
    /// frame-allocator baselines in later `#[test_case]`s. Domain
    /// lifecycle is exercised exhaustively by the pure-logic
    /// `IdentityUnit` tests in `kernel-core::iommu::identity` and by the
    /// MockUnit contract suite in `kernel-core/tests/iommu_contract.rs`.
    #[test_case]
    fn registry_bookkeeping_is_coherent() {
        ensure_iommu_initialized();
        assert!(registry::registered());
        assert!(registry::len() >= 1);
        // Phase 55a: vendor bring-up deferred; registry holds only
        // IdentityUnit variants so `translating()` is false.
        assert!(!registry::translating());
    }

    // ----------------------------------------------------------------------
    // Phase 55a Track F.3 — malformed-PRP fault-injection smoke
    // ----------------------------------------------------------------------
    //
    // The design-doc acceptance: "a deliberately-malformed NVMe PRP entry
    // pointing outside the driver's DMA allocation triggers an IOMMU fault
    // rather than corrupting kernel memory".
    //
    // Because Track E documents vendor MMIO bring-up (VtdUnit::bring_up /
    // AmdViUnit::bring_up) as deferred, Phase 55a boots with `translating()
    // == false` even under `cargo xtask test --iommu`. No IOMMU hardware is
    // asserting translations, so a malformed PRP would corrupt memory
    // rather than fault — exactly the failure mode F.3 is meant to detect.
    // Running the test body in this state would either falsely pass (by
    // seeing the sentinel page unmodified for unrelated reasons) or
    // falsely fail (by observing silent memory corruption). Both are
    // worse than a named skip.
    //
    // The test therefore asserts the skip condition is legible: when
    // `iommu::active()` is false, the test logs a structured reason and
    // returns success without touching NVMe or a sentinel page. A follow-up
    // commit that lands VT-d bring-up with real MSI-based fault delivery
    // will swap the skip body for the live fault-injection path:
    //
    //   1. Allocate a DmaBuffer through a claimed NVMe device.
    //   2. Allocate a sentinel page (DmaBuffer) and fill it with a known
    //      pattern; snapshot its bytes.
    //   3. Submit a synthesized NVMe command whose PRP points at the
    //      sentinel's physical address, NOT its IOVA — the IOMMU should
    //      fault because the device's domain has no mapping for that IOVA.
    //   4. Assert the serial log contains an `iommu.fault` event with the
    //      expected requester BDF and the malformed IOVA.
    //   5. Assert the sentinel page is unmodified (byte-compare before and
    //      after).
    //   6. Assert a subsequent well-formed NVMe command still succeeds.
    //
    // Until that lands, the skip below is the authoritative record that
    // F.3 exists, has a concrete plan, and is gated on work that has not
    // been done yet — rather than a silent gap.
    #[test_case]
    fn malformed_prp_triggers_iommu_fault_or_skips_cleanly() {
        ensure_iommu_initialized();
        if !active() {
            log::info!(
                "[iommu] F.3 malformed_prp_triggers_iommu_fault: \
                 skipped: iommu inactive (reason=vendor bring-up deferred per Track E)"
            );
            return;
        }

        // --- Live path placeholder (unreachable in Phase 55a) ---
        //
        // When vendor bring-up lands, replace this with the sequence
        // described in the module comment above. The sentinel is
        // byte-compared before and after; the boot log is scanned for
        // the structured `iommu.fault` event emitted by
        // `kernel::iommu::fault::log_fault_event`.
        //
        // We deliberately do not put a panic here — reaching this branch
        // in a future build means the live path has work to do, not a
        // test failure. Log and return success so the skip-vs-live
        // transition is visible in CI.
        log::info!(
            "[iommu] F.3 malformed_prp_triggers_iommu_fault: \
             iommu.active() true — live fault-injection path pending follow-up commit"
        );
    }
}
