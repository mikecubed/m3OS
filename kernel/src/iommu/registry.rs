//! Kernel-side IOMMU unit registry — Phase 55a Track E.
//!
//! The registry holds every live [`IommuUnit`] impl behind a single
//! [`Mutex`], indexed by `unit_index`. `PciDeviceHandle` uses it to create
//! and destroy per-device domains via the trait, without needing to know
//! whether the backing unit is a VT-d, AMD-Vi, or identity-map instance.
//!
//! # Why a wrapping enum instead of `Box<dyn IommuUnit>`
//!
//! Vendor impls are distinct, concrete types with distinct sizes; stuffing
//! them behind a trait object would force a heap allocation per unit and
//! turn every method call into a vtable dispatch. The vendor set is small
//! and fixed (VT-d, AMD-Vi, Identity) so a wrapping enum delegates cleanly
//! and keeps the dispatch inlineable. It also sidesteps any future concern
//! about `dyn IommuUnit + Send` object-safety when a new method is added.
//!
//! # Reserved-region pre-mapping
//!
//! After `create_domain` returns, the high-level `claim_pci_device` path
//! calls [`pre_map_reserved`] to install every firmware-owned region as an
//! identity mapping in the new domain. This is deliberately a registry
//! helper (and not a trait method) so vendor code does not grow a hard
//! dependency on the kernel-side `reserved_regions()` accessor.

use alloc::vec::Vec;
use spin::Mutex;

use kernel_core::iommu::contract::{
    DmaDomain, DomainError, DomainId, IommuError, IommuUnit, Iova, MapFlags, PhysAddr,
};
use kernel_core::iommu::identity::IdentityUnit;

use super::amd::AmdViUnit;
use super::intel::VtdUnit;

// ---------------------------------------------------------------------------
// RegisteredUnit — sum type wrapping every concrete `IommuUnit` impl.
// ---------------------------------------------------------------------------

/// Vendor-tagged wrapper over the three concrete `IommuUnit` impls the
/// kernel knows about. The enum implements [`IommuUnit`] itself by
/// delegating to the active variant. A third vendor (e.g. ARM SMMU) lands
/// by adding a new variant here; no other caller changes.
///
/// `VtdUnit` carries a 4 KiB cache of context-table physical addresses
/// which makes the enum variant size disparity large. Accepted as a
/// documented one-time allocation — the alternative (`Box<dyn IommuUnit>`)
/// would add a heap allocation per unit and a vtable dispatch on every
/// method call. The vendor set is small and fixed.
#[allow(clippy::large_enum_variant, dead_code)]
pub enum RegisteredUnit {
    /// Real Intel VT-d hardware unit.
    Vtd(VtdUnit),
    /// Real AMD-Vi hardware unit.
    AmdVi(AmdViUnit),
    /// Pure-logic identity-mapping fallback (no hardware effect).
    Identity(IdentityUnit),
}

impl RegisteredUnit {
    /// Short vendor tag for log output.
    #[allow(dead_code)]
    pub fn vendor_tag(&self) -> &'static str {
        match self {
            RegisteredUnit::Vtd(_) => "vtd",
            RegisteredUnit::AmdVi(_) => "amdvi",
            RegisteredUnit::Identity(_) => "identity",
        }
    }
}

impl IommuUnit for RegisteredUnit {
    fn bring_up(&mut self) -> Result<(), IommuError> {
        match self {
            RegisteredUnit::Vtd(u) => u.bring_up(),
            RegisteredUnit::AmdVi(u) => u.bring_up(),
            RegisteredUnit::Identity(u) => u.bring_up(),
        }
    }

    fn create_domain(&mut self) -> Result<DmaDomain, IommuError> {
        match self {
            RegisteredUnit::Vtd(u) => u.create_domain(),
            RegisteredUnit::AmdVi(u) => u.create_domain(),
            RegisteredUnit::Identity(u) => u.create_domain(),
        }
    }

    fn destroy_domain(&mut self, domain: DmaDomain) -> Result<(), IommuError> {
        match self {
            RegisteredUnit::Vtd(u) => u.destroy_domain(domain),
            RegisteredUnit::AmdVi(u) => u.destroy_domain(domain),
            RegisteredUnit::Identity(u) => u.destroy_domain(domain),
        }
    }

    fn map(
        &mut self,
        domain: DomainId,
        iova: Iova,
        phys: PhysAddr,
        len: usize,
        flags: MapFlags,
    ) -> Result<(), DomainError> {
        match self {
            RegisteredUnit::Vtd(u) => u.map(domain, iova, phys, len, flags),
            RegisteredUnit::AmdVi(u) => u.map(domain, iova, phys, len, flags),
            RegisteredUnit::Identity(u) => u.map(domain, iova, phys, len, flags),
        }
    }

    fn unmap(&mut self, domain: DomainId, iova: Iova, len: usize) -> Result<(), DomainError> {
        match self {
            RegisteredUnit::Vtd(u) => u.unmap(domain, iova, len),
            RegisteredUnit::AmdVi(u) => u.unmap(domain, iova, len),
            RegisteredUnit::Identity(u) => u.unmap(domain, iova, len),
        }
    }

    fn flush(&mut self, domain: DomainId) -> Result<(), IommuError> {
        match self {
            RegisteredUnit::Vtd(u) => u.flush(domain),
            RegisteredUnit::AmdVi(u) => u.flush(domain),
            RegisteredUnit::Identity(u) => u.flush(domain),
        }
    }

    fn install_fault_handler(
        &mut self,
        handler: kernel_core::iommu::contract::FaultHandlerFn,
    ) -> Result<(), IommuError> {
        match self {
            RegisteredUnit::Vtd(u) => u.install_fault_handler(handler),
            RegisteredUnit::AmdVi(u) => u.install_fault_handler(handler),
            RegisteredUnit::Identity(u) => u.install_fault_handler(handler),
        }
    }

    fn capabilities(&self) -> kernel_core::iommu::contract::IommuCapabilities {
        match self {
            RegisteredUnit::Vtd(u) => u.capabilities(),
            RegisteredUnit::AmdVi(u) => u.capabilities(),
            RegisteredUnit::Identity(u) => u.capabilities(),
        }
    }
}

// ---------------------------------------------------------------------------
// Global registry
// ---------------------------------------------------------------------------

/// Live unit list indexed by `unit_index`. Populated during boot by
/// [`install_identity_fallback`] or [`install_units`]; read every time a
/// PCI device is claimed so its domain can be created on the right unit.
static REGISTRY: Mutex<Vec<RegisteredUnit>> = Mutex::new(Vec::new());

/// `true` once any unit has been installed. Used by [`active`] to answer
/// the "is IOMMU translation really running?" question without taking
/// the registry lock in hot paths.
///
/// An [`IdentityUnit`] registration sets this to `true` too, because a
/// registered unit exists — callers check [`translating`] when they need
/// the stricter "real IOMMU hardware is on" query.
static REGISTERED: spin::RwLock<bool> = spin::RwLock::new(false);

/// `true` when the installed units are real hardware (VT-d or AMD-Vi);
/// `false` when only an [`IdentityUnit`] was installed as fallback.
static TRANSLATING: spin::RwLock<bool> = spin::RwLock::new(false);

/// Install the full vendor-unit set the kernel discovered at boot.
///
/// Called with a fully-initialized `Vec<RegisteredUnit>` whose indices
/// match the ACPI-enumerated `unit_index`. The registry takes ownership
/// of the list and subsequent lookups use the passed-in index as the
/// registry slot index.
pub fn install_units(units: Vec<RegisteredUnit>) {
    let translating = units
        .iter()
        .any(|u| !matches!(u, RegisteredUnit::Identity(_)));
    let have_any = !units.is_empty();
    *REGISTRY.lock() = units;
    *REGISTERED.write() = have_any;
    *TRANSLATING.write() = translating;
}

/// Install a single [`IdentityUnit`] as the sole registered unit. Used
/// by the Track E.3 fallback path when ACPI reports no IOMMU or vendor
/// bring-up fails.
pub fn install_identity_fallback() {
    let unit = RegisteredUnit::Identity(IdentityUnit::new(0));
    install_units(alloc::vec![unit]);
}

/// `true` when at least one `IommuUnit` (real or identity-fallback) has
/// been registered. Used by `PciDeviceHandle` to know whether to call
/// into the registry at all.
#[allow(dead_code)]
pub fn registered() -> bool {
    *REGISTERED.read()
}

/// `true` when the active set is real hardware (VT-d or AMD-Vi);
/// `false` when only [`IdentityUnit`] is installed. Mirrors the
/// `iommu.active` boolean surfaced to diagnostic output.
#[allow(dead_code)]
pub fn translating() -> bool {
    *TRANSLATING.read()
}

/// Count of registered units. Diagnostic only.
#[allow(dead_code)]
pub fn len() -> usize {
    REGISTRY.lock().len()
}

// ---------------------------------------------------------------------------
// Domain lifecycle entry points
// ---------------------------------------------------------------------------

/// Request a new [`DmaDomain`] from the unit at `unit_index`.
///
/// Returns `Err(IommuError::NotAvailable)` if no unit occupies the slot.
/// The caller owns the returned handle and **must** release it via
/// [`destroy_domain`] (on the same unit index).
#[allow(dead_code)]
pub fn create_domain(unit_index: usize) -> Result<DmaDomain, IommuError> {
    let mut reg = REGISTRY.lock();
    let unit = reg.get_mut(unit_index).ok_or(IommuError::NotAvailable)?;
    unit.create_domain()
}

/// Destroy a previously-created domain on the given unit index.
#[allow(dead_code)]
pub fn destroy_domain(unit_index: usize, domain: DmaDomain) -> Result<(), IommuError> {
    let mut reg = REGISTRY.lock();
    let unit = reg.get_mut(unit_index).ok_or(IommuError::NotAvailable)?;
    unit.destroy_domain(domain)
}

/// Install a mapping in the domain. Thin delegator through the registry
/// lock so callers hold the lock only for the duration of the trait call.
#[allow(dead_code)]
pub fn map(
    unit_index: usize,
    domain: DomainId,
    iova: Iova,
    phys: PhysAddr,
    len: usize,
    flags: MapFlags,
) -> Result<(), DomainError> {
    let mut reg = REGISTRY.lock();
    let unit = reg.get_mut(unit_index).ok_or(DomainError::InvalidRange)?;
    unit.map(domain, iova, phys, len, flags)
}

/// Remove a mapping. Mirrors [`map`].
#[allow(dead_code)]
pub fn unmap(
    unit_index: usize,
    domain: DomainId,
    iova: Iova,
    len: usize,
) -> Result<(), DomainError> {
    let mut reg = REGISTRY.lock();
    let unit = reg.get_mut(unit_index).ok_or(DomainError::InvalidRange)?;
    unit.unmap(domain, iova, len)
}

// ---------------------------------------------------------------------------
// Reserved-region pre-mapping helper (E.4)
// ---------------------------------------------------------------------------

/// Pre-map every firmware-declared reserved region in the newly-created
/// domain as an identity mapping (IOVA == phys). Also reserves the same
/// ranges in the domain-side IOVA allocator (if any) so subsequent
/// driver allocations cannot collide.
///
/// Called from `claim_pci_device` after `create_domain` returns; keeps
/// the reserved-region knowledge in the kernel layer and off the
/// pure-logic vendor page-table walkers. Idempotent at the registry
/// level: repeated calls install the same mappings, which the underlying
/// vendor impl either treats as already-mapped (returns
/// `DomainError::AlreadyMapped`) or silently accepts. Errors are logged
/// but not propagated — a missing RMRR mapping on a rare device is a
/// degraded-but-working state, not a boot abort.
#[allow(dead_code)]
pub fn pre_map_reserved(unit_index: usize, domain: DomainId) {
    let regions = super::reserved_regions();
    if regions.is_empty() {
        return;
    }
    let flags = MapFlags::READ | MapFlags::WRITE;
    for region in regions.iter() {
        if region.len == 0 {
            continue;
        }
        // Identity map: IOVA == phys.
        match map(
            unit_index,
            domain,
            Iova(region.start),
            PhysAddr(region.start),
            region.len,
            flags,
        ) {
            Ok(()) => {
                log::debug!(
                    "[iommu] pre_map_reserved: unit={} domain={:?} start={:#x} len={:#x}",
                    unit_index,
                    domain,
                    region.start,
                    region.len,
                );
            }
            Err(DomainError::AlreadyMapped) => {
                // Overlap with an existing reserved-region entry. Accept silently.
            }
            Err(e) => {
                log::warn!(
                    "[iommu] pre_map_reserved: unit={} start={:#x} len={:#x} failed: {}",
                    unit_index,
                    region.start,
                    region.len,
                    e,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Fallback-reason logging
// ---------------------------------------------------------------------------

/// Reason identity fallback engaged at boot. Logged once from
/// [`log_identity_fallback`].
#[derive(Debug, Clone, Copy)]
pub enum IdentityFallbackReason {
    /// ACPI reported no DMAR / IVRS — no IOMMU on this platform.
    NoDmarOrIvrs,
    /// DMAR was present but VT-d bring-up failed.
    VtdInitFailed,
    /// IVRS was present but AMD-Vi bring-up failed.
    AmdViInitFailed,
}

impl IdentityFallbackReason {
    fn tag(self) -> &'static str {
        match self {
            IdentityFallbackReason::NoDmarOrIvrs => "no_dmar_or_ivrs",
            IdentityFallbackReason::VtdInitFailed => "vtd_init_failed",
            IdentityFallbackReason::AmdViInitFailed => "amdvi_init_failed",
        }
    }
}

/// Emit the one-per-boot `iommu.fallback.identity` structured event.
/// Callers must invoke this exactly once when identity fallback engages.
pub fn log_identity_fallback(reason: IdentityFallbackReason) {
    log::info!(
        "[iommu] iommu.fallback.identity event reason={} — no translation active; \
         drivers see raw physical addresses",
        reason.tag()
    );
}
