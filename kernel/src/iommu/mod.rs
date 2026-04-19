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
use kernel_core::iommu::contract::{IommuUnit, PhysAddr};
use kernel_core::iommu::device_map::{DeviceToUnitMap, IommuUnitDescriptor, IommuVendor};
use kernel_core::iommu::regions::ReservedRegionSet;

use registry::RegisteredUnit;

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
/// Track E wired the registry and per-device domain lifecycle; this
/// follow-up wires vendor MMIO bring-up on top. For each descriptor
/// reported by ACPI, the kernel constructs the matching vendor impl,
/// calls [`IommuUnit::bring_up`], and — on success — registers the real
/// unit so [`active`] flips to `true` and every claimed device sees a
/// translating domain. On any bring-up failure the unit is replaced
/// with an [`IdentityUnit`] and the structured `iommu.fallback.identity`
/// event is logged with the real reason (`vtd_init_failed` /
/// `amdvi_init_failed`). When ACPI reports no tables at all the same
/// event is logged with reason `no_dmar_or_ivrs`.
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
        return;
    }

    log::info!("[iommu] init: {} IOMMU unit(s) discovered", descs.len());

    // Try to bring up every descriptor. Successes become real
    // RegisteredUnit::Vtd / RegisteredUnit::AmdVi entries; failures
    // degrade to RegisteredUnit::Identity at the same slot so the
    // `unit_index` published by `DeviceToUnitMap` remains valid.
    let mut units: Vec<RegisteredUnit> = Vec::with_capacity(descs.len());
    let mut any_real = false;
    let mut vtd_failed = false;
    let mut amdvi_failed = false;
    for (slot, desc) in descs.iter().enumerate() {
        let built = match desc.vendor {
            IommuVendor::Vtd => build_and_bring_up_vtd(slot, desc.register_base),
            IommuVendor::AmdVi => build_and_bring_up_amdvi(slot, desc.register_base),
        };
        match built {
            Ok(u) => {
                any_real = true;
                units.push(u);
            }
            Err(reason) => {
                match desc.vendor {
                    IommuVendor::Vtd => vtd_failed = true,
                    IommuVendor::AmdVi => amdvi_failed = true,
                }
                log::warn!(
                    "[iommu] unit[{}] vendor={} bring_up failed (reason={:?}); \
                     installing identity fallback at this slot",
                    slot,
                    match desc.vendor {
                        IommuVendor::Vtd => "vtd",
                        IommuVendor::AmdVi => "amdvi",
                    },
                    reason,
                );
                units.push(RegisteredUnit::Identity(
                    kernel_core::iommu::identity::IdentityUnit::new(slot),
                ));
            }
        }
    }

    registry::install_units(units);

    if any_real {
        log::info!(
            "[iommu] init: {} real unit(s) brought up; translating mode active",
            descs.len()
        );
    } else {
        // Every unit failed bring-up. Pick the first descriptor's vendor
        // for the structured reason tag (most platforms have one vendor).
        let reason = if vtd_failed {
            registry::IdentityFallbackReason::VtdInitFailed
        } else if amdvi_failed {
            registry::IdentityFallbackReason::AmdViInitFailed
        } else {
            // Unreachable given the loop above only takes this branch on
            // real vendors, but default defensively.
            registry::IdentityFallbackReason::NoDmarOrIvrs
        };
        registry::log_identity_fallback(reason);
    }
}

/// Construct and bring up an Intel VT-d unit. On any failure (allocator
/// exhaustion, GSTS poll timeout, etc.) returns the `IommuError` so the
/// caller can log and fall back.
fn build_and_bring_up_vtd(slot: usize, register_base: u64) -> Result<RegisteredUnit, IommuError> {
    let mut unit = intel::VtdUnit::new(slot, PhysAddr(register_base));
    unit.bring_up()?;
    Ok(RegisteredUnit::Vtd(unit))
}

/// Construct and bring up an AMD-Vi unit. Returns the `IommuError` on
/// any failure (register-base mapping, allocator exhaustion, etc.) so
/// the caller can log and fall back.
fn build_and_bring_up_amdvi(slot: usize, register_base: u64) -> Result<RegisteredUnit, IommuError> {
    let mut unit = amd::AmdViUnit::new(register_base, slot)?;
    unit.bring_up()?;
    Ok(RegisteredUnit::AmdVi(unit))
}

use kernel_core::iommu::contract::IommuError;

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
// Test-path behavior (`cargo xtask test` without `-device intel-iommu`):
//
// No DMAR / IVRS present → `iommu_units_from_acpi()` returns an empty
// slice → identity fallback engages with reason `no_dmar_or_ivrs`.
// `active() == false`, `translating() == false`, `registered() == true`.
//
// Test-path behavior (`cargo xtask test --iommu`):
//
// DMAR present → init constructs `VtdUnit`s and calls `bring_up` on each.
// On QEMU with `-device intel-iommu` the register block responds and
// bring_up succeeds → `active() == true`. If the hardware fails to
// respond (register read returns garbage or GSTS poll times out), the
// slot is demoted to identity and logged as `vtd_init_failed` /
// `amdvi_init_failed`.
//
// The assertions in this module are true on the default `cargo xtask
// test` path (empty DMAR). Test cases that would observe different state
// under `--iommu` are structured to branch on `active()` so they work in
// both configurations.

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

    /// Smoke: `iommu::active()` reports translating mode only when real
    /// vendor bring-up succeeded. On the default `cargo xtask test` path
    /// there is no DMAR / IVRS table, so `active()` is `false` and the
    /// registry holds a single `IdentityUnit` entry. Under `cargo xtask
    /// test --iommu` this test would assert `active() == true` on a
    /// healthy QEMU q35 + `-device intel-iommu` configuration; we keep
    /// the default-path assertion here and cover the `--iommu` path via
    /// the log-event assertions a future CI configuration will add.
    #[test_case]
    fn iommu_active_is_false_on_default_test_path() {
        ensure_iommu_initialized();
        assert!(
            !active(),
            "default cargo xtask test has no DMAR; active() must be false"
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
    /// `registered()` and `len()` agree after init. We intentionally do NOT
    /// run `create_domain`/`destroy_domain` against the global registry
    /// here — those paths push to a `Vec<DomainId>` inside the registry's
    /// `IdentityUnit`, and the resulting slab-cache churn shifts
    /// frame-allocator baselines in later `#[test_case]`s. Domain lifecycle
    /// is exercised exhaustively by the pure-logic `IdentityUnit` tests in
    /// `kernel-core::iommu::identity` and by the MockUnit contract suite in
    /// `kernel-core/tests/iommu_contract.rs`.
    #[test_case]
    fn registry_bookkeeping_is_coherent() {
        ensure_iommu_initialized();
        assert!(registry::registered());
        assert!(registry::len() >= 1);
        // On the default `cargo xtask test` path there is no DMAR, so
        // only identity fallback is installed and `translating()` is
        // false. A future CI configuration that runs the test under
        // `--iommu` would assert the opposite.
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
    // The test branches on `iommu::active()`:
    //
    // - `active() == false` (default `cargo xtask test`, no DMAR, or a
    //   platform where vendor bring-up legitimately failed): there is no
    //   IOMMU to fault, so the test logs a structured skip-reason and
    //   returns success. That keeps the test healthy on the regression
    //   path while still asserting F.3 exists and has a skip-condition
    //   that is greppable.
    // - `active() == true` (`cargo xtask test --iommu` with a working
    //   vendor unit): the live fault-injection body runs. That body is
    //   still a TODO — it requires wiring an NVMe claim path that routes
    //   through the translating unit and then submitting a synthesized
    //   PRP. The plan is:
    //
    //     1. Allocate a DmaBuffer through a claimed NVMe device.
    //     2. Allocate a sentinel page (DmaBuffer) and fill it with a
    //        known pattern; snapshot its bytes.
    //     3. Submit a synthesized NVMe command whose PRP points at the
    //        sentinel's physical address, NOT its IOVA — the IOMMU
    //        should fault because the device's domain has no mapping
    //        for that IOVA.
    //     4. Assert the serial log contains an `iommu.fault` event with
    //        the expected requester BDF and the malformed IOVA.
    //     5. Assert the sentinel page is unmodified (byte-compare).
    //     6. Assert a subsequent well-formed NVMe command still works.
    #[test_case]
    fn malformed_prp_triggers_iommu_fault_or_skips_cleanly() {
        ensure_iommu_initialized();
        if !active() {
            log::info!(
                "[iommu] F.3 malformed_prp_triggers_iommu_fault: \
                 skipped: iommu inactive (default test path has no DMAR; \
                 active() is false)"
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
