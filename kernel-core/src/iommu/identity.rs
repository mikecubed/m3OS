//! Identity-mapping IOMMU fallback — Phase 55a Track E.3.
//!
//! When boot discovers no DMAR or IVRS table, or the vendor bring-up fails,
//! the kernel installs [`IdentityUnit`] as a pass-through `IommuUnit`
//! implementation. The unit performs no hardware setup and no MMIO; every
//! `map` call is a no-op and every claimed device sees its DMA physical
//! addresses as-is. This matches the Phase 55 behavior exactly, preserving
//! the unprotected-but-working baseline without bypassing the `IommuUnit`
//! trait seam.
//!
//! # Why a named unit instead of a branch
//!
//! The alternative — sprinkling `if iommu::active() { ... } else { ... }`
//! branches through every DMA path — was the original Phase 55 shape, and
//! it leaks "is IOMMU on?" into every driver. Routing the fallback through
//! an `IommuUnit` impl keeps the caller surface uniform: drivers always
//! allocate through a domain, and whether the domain is translated or
//! identity-mapped is invisible at the call site. [`IdentityUnit`] is the
//! seam that makes that possible.
//!
//! # Pure-logic scope
//!
//! This module contains no MMIO and no hardware dependencies. It is
//! host-testable via `cargo test -p kernel-core` like every other module
//! under `kernel_core::iommu::`. Kernel-side code wraps it in the registry
//! declared in `kernel/src/iommu/registry.rs`.
//!
//! # Domain identity
//!
//! The unit hands out monotonic `DomainId`s so that a caller holding two
//! `DmaDomain`s can distinguish them. Internally there is no page table to
//! maintain; the "domain" is just a handle bookkept for lifetime purposes.

use alloc::vec::Vec;

use super::contract::{
    DmaDomain, DomainError, DomainId, FaultHandlerFn, IommuCapabilities, IommuError, IommuUnit,
    Iova, MapFlags, PhysAddr,
};

/// A no-op `IommuUnit` implementation used when no real IOMMU is available.
///
/// `map` and `unmap` succeed without any hardware effect. `bus_address()`
/// on a [`crate::iommu::contract::DmaDomain`] returned by this unit is
/// equivalent to the physical address. Capabilities report a generous
/// address width so driver allocations are never refused for size reasons.
#[derive(Debug)]
pub struct IdentityUnit {
    /// Index assigned to this unit in the registry. Stamped onto every
    /// returned [`DmaDomain`] so `destroy_domain` knows which unit owns it.
    unit_index: usize,
    /// Next `DomainId` to hand out. Incremented on each `create_domain`.
    next_domain_id: u32,
    /// Domain IDs currently alive (awaiting `destroy_domain`).
    live_domains: Vec<DomainId>,
    /// `true` after [`IommuUnit::bring_up`] has been called at least once.
    /// The [`IommuUnit`] trait contract requires this to be true before any
    /// `create_domain` / `map` / `flush` call; [`IdentityUnit`] honors that
    /// gate so swapping a real VT-d / AMD-Vi unit for the identity fallback
    /// does not change caller-observable ordering.
    brought_up: bool,
}

impl IdentityUnit {
    /// Create a new identity-map unit. The caller must invoke
    /// [`IommuUnit::bring_up`] before any other method — matching the
    /// `IommuUnit` trait contract. Bring-up is infallible and idempotent.
    pub const fn new(unit_index: usize) -> Self {
        Self {
            unit_index,
            next_domain_id: 1,
            live_domains: Vec::new(),
            brought_up: false,
        }
    }

    /// The identity unit's stable unit-index, for registry lookup.
    pub const fn unit_index(&self) -> usize {
        self.unit_index
    }

    /// Count of live domains. Useful for tests and leak detection.
    pub fn live_domain_count(&self) -> usize {
        self.live_domains.len()
    }
}

impl IommuUnit for IdentityUnit {
    fn bring_up(&mut self) -> Result<(), IommuError> {
        self.brought_up = true;
        Ok(())
    }

    fn create_domain(&mut self) -> Result<DmaDomain, IommuError> {
        if !self.brought_up {
            return Err(IommuError::NotAvailable);
        }
        let id = DomainId(self.next_domain_id);
        self.next_domain_id = self.next_domain_id.saturating_add(1);
        self.live_domains.push(id);
        Ok(DmaDomain::new(id, self.unit_index))
    }

    fn destroy_domain(&mut self, domain: DmaDomain) -> Result<(), IommuError> {
        if !self.brought_up {
            domain.release();
            return Err(IommuError::NotAvailable);
        }
        if domain.unit_index() != self.unit_index {
            // The caller passed us a handle belonging to another unit. We
            // consume it here (release suppresses the Drop leak-guard) and
            // report the mismatch; the caller cannot re-use the handle after
            // a failed destroy.
            domain.release();
            return Err(IommuError::Invalid);
        }
        let id = domain.id();
        let Some(pos) = self.live_domains.iter().position(|d| *d == id) else {
            domain.release();
            return Err(IommuError::Invalid);
        };
        self.live_domains.swap_remove(pos);
        domain.release();
        Ok(())
    }

    fn map(
        &mut self,
        domain: DomainId,
        _iova: Iova,
        _phys: PhysAddr,
        len: usize,
        _flags: MapFlags,
    ) -> Result<(), DomainError> {
        if !self.brought_up {
            return Err(DomainError::InvalidRange);
        }
        if len == 0 {
            return Err(DomainError::InvalidRange);
        }
        if !self.live_domains.contains(&domain) {
            return Err(DomainError::InvalidRange);
        }
        // Identity map: IOVA == phys by convention, no page-table mutation.
        Ok(())
    }

    fn unmap(&mut self, domain: DomainId, _iova: Iova, len: usize) -> Result<(), DomainError> {
        if !self.brought_up {
            return Err(DomainError::InvalidRange);
        }
        if len == 0 {
            return Err(DomainError::InvalidRange);
        }
        if !self.live_domains.contains(&domain) {
            return Err(DomainError::InvalidRange);
        }
        // Nothing to undo; identity mapping has no state.
        Ok(())
    }

    fn flush(&mut self, domain: DomainId) -> Result<(), IommuError> {
        if !self.brought_up {
            return Err(IommuError::NotAvailable);
        }
        if !self.live_domains.contains(&domain) {
            return Err(IommuError::Invalid);
        }
        Ok(())
    }

    fn install_fault_handler(&mut self, _handler: FaultHandlerFn) -> Result<(), IommuError> {
        // No faults can originate from an identity mapping: every DMA is
        // forwarded to the physical address the caller chose. The handler
        // is recorded as a no-op for API compatibility.
        Ok(())
    }

    fn capabilities(&self) -> IommuCapabilities {
        IommuCapabilities {
            // Advertise 4 KiB, 2 MiB, and 1 GiB page sizes so drivers that
            // key on these bits see a reasonable answer. Because the unit
            // is a no-op, these values do not affect correctness.
            supported_page_sizes: (1u64 << 12) | (1u64 << 21) | (1u64 << 30),
            // 64-bit address width matches the physical window on x86_64.
            address_width_bits: 64,
            interrupt_remapping: false,
            queued_invalidation: false,
            scalable_mode: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bring_up_is_idempotent() {
        let mut unit = IdentityUnit::new(0);
        assert!(unit.bring_up().is_ok());
        assert!(unit.bring_up().is_ok());
    }

    #[test]
    fn create_destroy_domain_cycle() {
        let mut unit = IdentityUnit::new(3);
        unit.bring_up().unwrap();
        let domain = unit.create_domain().expect("create_domain should succeed");
        assert_eq!(domain.unit_index(), 3);
        assert_eq!(unit.live_domain_count(), 1);
        unit.destroy_domain(domain)
            .expect("destroy_domain should succeed");
        assert_eq!(unit.live_domain_count(), 0);
    }

    #[test]
    fn destroy_domain_with_wrong_unit_fails() {
        let mut unit_a = IdentityUnit::new(1);
        let mut unit_b = IdentityUnit::new(2);
        unit_a.bring_up().unwrap();
        unit_b.bring_up().unwrap();
        let domain = unit_a.create_domain().expect("create ok");
        // Fabricate a handle stamped with unit_a's index so unit_b must
        // reject it. `destroy_domain` consumes the handle regardless of
        // outcome (release is called on every path), so the Drop guard
        // stays quiet.
        let phantom = DmaDomain::new(domain.id(), 1);
        let err = unit_b
            .destroy_domain(phantom)
            .expect_err("cross-unit destroy must fail");
        assert_eq!(err, IommuError::Invalid);
        // unit_a still has the real domain live; release it cleanly.
        unit_a.destroy_domain(domain).expect("owner destroy ok");
    }

    #[test]
    fn create_domain_returns_unique_ids() {
        let mut unit = IdentityUnit::new(0);
        unit.bring_up().unwrap();
        let d1 = unit.create_domain().unwrap();
        let d2 = unit.create_domain().unwrap();
        let d3 = unit.create_domain().unwrap();
        assert_ne!(d1.id(), d2.id());
        assert_ne!(d2.id(), d3.id());
        assert_ne!(d1.id(), d3.id());
        unit.destroy_domain(d1).unwrap();
        unit.destroy_domain(d2).unwrap();
        unit.destroy_domain(d3).unwrap();
    }

    #[test]
    fn map_unmap_succeed_as_noop() {
        let mut unit = IdentityUnit::new(0);
        unit.bring_up().unwrap();
        let domain = unit.create_domain().unwrap();
        let id = domain.id();
        // Identity mapping: IOVA == phys.
        let iova = Iova(0x1000);
        let phys = PhysAddr(0x1000);
        assert!(
            unit.map(id, iova, phys, 4096, MapFlags::READ | MapFlags::WRITE)
                .is_ok()
        );
        assert!(unit.unmap(id, iova, 4096).is_ok());
        unit.destroy_domain(domain).unwrap();
    }

    #[test]
    fn zero_length_map_is_rejected() {
        let mut unit = IdentityUnit::new(0);
        unit.bring_up().unwrap();
        let domain = unit.create_domain().unwrap();
        let id = domain.id();
        let err = unit
            .map(id, Iova(0x1000), PhysAddr(0x1000), 0, MapFlags::READ)
            .expect_err("zero-length map must fail");
        assert_eq!(err, DomainError::InvalidRange);
        unit.destroy_domain(domain).unwrap();
    }

    #[test]
    fn map_to_unknown_domain_fails() {
        let mut unit = IdentityUnit::new(0);
        unit.bring_up().unwrap();
        let stale = DomainId(42);
        let err = unit
            .map(stale, Iova(0x1000), PhysAddr(0x1000), 4096, MapFlags::READ)
            .expect_err("unknown domain must fail");
        assert_eq!(err, DomainError::InvalidRange);
    }

    #[test]
    fn flush_unknown_domain_fails() {
        let mut unit = IdentityUnit::new(0);
        unit.bring_up().unwrap();
        let err = unit
            .flush(DomainId(99))
            .expect_err("flush on unknown domain must fail");
        assert_eq!(err, IommuError::Invalid);
    }

    #[test]
    fn install_fault_handler_succeeds_as_noop() {
        let mut unit = IdentityUnit::new(0);
        unit.bring_up().unwrap();
        fn handler(_rec: &super::super::contract::FaultRecord) {}
        assert!(unit.install_fault_handler(handler).is_ok());
    }

    #[test]
    fn capabilities_advertise_wide_support() {
        let unit = IdentityUnit::new(0);
        let caps = unit.capabilities();
        // 4 KiB bit set.
        assert!(caps.supported_page_sizes & (1 << 12) != 0);
        // 64-bit address width — identity mapping covers the whole window.
        assert_eq!(caps.address_width_bits, 64);
        assert!(!caps.interrupt_remapping);
        assert!(!caps.scalable_mode);
    }

    #[test]
    fn double_destroy_fails() {
        let mut unit = IdentityUnit::new(5);
        unit.bring_up().unwrap();
        let domain = unit.create_domain().unwrap();
        let id = domain.id();
        // Build a second `DmaDomain` handle with the same id/unit — simulate
        // the accidental double-destroy scenario a buggy caller could cause.
        let phantom = DmaDomain::new(id, 5);
        unit.destroy_domain(domain).expect("first destroy ok");
        let err = unit
            .destroy_domain(phantom)
            .expect_err("double-destroy must fail");
        assert_eq!(err, IommuError::Invalid);
    }

    #[test]
    fn create_domain_before_bring_up_returns_not_available() {
        // The IommuUnit trait contract requires bring_up to succeed before
        // create_domain. IdentityUnit honors that gate so identity-fallback
        // slots cannot diverge from real vendor units on method ordering.
        let mut unit = IdentityUnit::new(0);
        let err = unit
            .create_domain()
            .expect_err("create_domain before bring_up must fail");
        assert_eq!(err, IommuError::NotAvailable);
    }

    #[test]
    fn map_before_bring_up_returns_invalid_range() {
        let mut unit = IdentityUnit::new(0);
        let err = unit
            .map(
                DomainId(1),
                Iova(0x1000),
                PhysAddr(0x1000),
                4096,
                MapFlags::READ,
            )
            .expect_err("map before bring_up must fail");
        assert_eq!(err, DomainError::InvalidRange);
    }

    #[test]
    fn flush_before_bring_up_returns_not_available() {
        let mut unit = IdentityUnit::new(0);
        let err = unit
            .flush(DomainId(1))
            .expect_err("flush before bring_up must fail");
        assert_eq!(err, IommuError::NotAvailable);
    }

    #[test]
    fn destroy_domain_before_bring_up_releases_handle() {
        // The pre-bring-up error path still consumes the passed handle so
        // the caller's DmaDomain Drop leak-guard stays quiet in debug
        // builds. Failing to release here would panic on drop.
        let mut unit = IdentityUnit::new(0);
        let phantom = DmaDomain::new(DomainId(1), 0);
        let err = unit
            .destroy_domain(phantom)
            .expect_err("destroy_domain before bring_up must fail");
        assert_eq!(err, IommuError::NotAvailable);
    }
}
