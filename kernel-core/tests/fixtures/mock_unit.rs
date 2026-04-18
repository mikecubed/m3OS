//! `MockUnit` — pure-logic reference implementation of the `IommuUnit` trait.
//!
//! Phase 55a Track A.1 acceptance artifact: this is the authoritative
//! reference against which the documented trait behavior is tested. The
//! contract suite (F.4) will parameterize over this type; vendor
//! implementations (Intel VT-d, AMD-Vi) must satisfy the same observable
//! behavior.
//!
//! Scope is deliberately narrow — the mock tracks mappings in a
//! `BTreeMap` keyed by `(DomainId, Iova)` and enforces the two
//! behavioral rules the contract suite relies on:
//!
//! - A `map` call that overlaps an existing mapping at the same IOVA
//!   returns `DomainError::AlreadyMapped`.
//! - An `unmap` of an IOVA that was never mapped (or was already
//!   unmapped) returns `DomainError::NotMapped`.
//!
//! No `unsafe`, no allocation beyond the map itself, and no hidden state
//! beyond the counters named below.

extern crate alloc;

use alloc::collections::BTreeMap;

use kernel_core::iommu::contract::{
    DmaDomain, DomainError, DomainId, FaultHandlerFn, IommuCapabilities, IommuError, IommuUnit,
    Iova, MapFlags, PhysAddr,
};

/// Canned capability profile returned by [`MockUnit::capabilities`].
///
/// Values are chosen to cover the fields the contract suite will assert
/// on. They do not match any real silicon.
pub const MOCK_CAPABILITIES: IommuCapabilities = IommuCapabilities {
    // 4 KiB and 2 MiB supported; 1 GiB intentionally off to exercise
    // the "capability reports absent feature" branch.
    supported_page_sizes: (1 << 12) | (1 << 21),
    address_width_bits: 48,
    interrupt_remapping: false,
    queued_invalidation: true,
    scalable_mode: false,
};

/// Pure-logic reference `IommuUnit`.
///
/// Identified by `unit_index`, which is stamped into every
/// [`DmaDomain`] it hands out so `destroy_domain` can reject foreign
/// domains.
pub struct MockUnit {
    unit_index: usize,
    next_domain_id: u32,
    brought_up: bool,
    domains: BTreeMap<DomainId, DomainState>,
    mappings: BTreeMap<(DomainId, Iova), Mapping>,
    flush_count: u64,
    fault_handler: Option<FaultHandlerFn>,
}

/// Per-domain bookkeeping tracked by the mock.
#[derive(Debug)]
struct DomainState {
    /// Set to `true` once [`IommuUnit::destroy_domain`] returns `Ok`.
    destroyed: bool,
}

/// A single live mapping recorded by the mock.
///
/// `len` and `flags` are recorded but not consulted by the current
/// smoke test; the Track F.4 contract suite will assert on them
/// directly, so they are kept on the struct rather than dropped.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct Mapping {
    phys: PhysAddr,
    len: usize,
    flags: MapFlags,
}

impl MockUnit {
    /// Construct a fresh mock with the given `unit_index`.
    pub const fn new(unit_index: usize) -> Self {
        Self {
            unit_index,
            next_domain_id: 0,
            brought_up: false,
            domains: BTreeMap::new(),
            mappings: BTreeMap::new(),
            flush_count: 0,
            fault_handler: None,
        }
    }

    /// Count of live mappings across every domain the mock owns.
    ///
    /// Reserved for use by the Track F.4 contract suite; the smoke
    /// test in this worktree asserts on `lookup_phys` instead.
    #[allow(dead_code)]
    pub fn live_mapping_count(&self) -> usize {
        self.mappings.len()
    }

    /// Count of `flush` invocations since construction.
    pub fn flush_count(&self) -> u64 {
        self.flush_count
    }

    /// `true` once any fault handler has been installed.
    pub fn has_fault_handler(&self) -> bool {
        self.fault_handler.is_some()
    }

    /// Lookup the recorded physical backing for a given IOVA. `None`
    /// when no mapping is present. Used by tests to assert observable
    /// state after `map` / `unmap` sequences.
    pub fn lookup_phys(&self, domain: DomainId, iova: Iova) -> Option<PhysAddr> {
        self.mappings.get(&(domain, iova)).map(|m| m.phys)
    }
}

impl IommuUnit for MockUnit {
    fn bring_up(&mut self) -> Result<(), IommuError> {
        if self.brought_up {
            // Idempotent: bring_up twice is not a contract error.
            return Ok(());
        }
        self.brought_up = true;
        Ok(())
    }

    fn create_domain(&mut self) -> Result<DmaDomain, IommuError> {
        if !self.brought_up {
            return Err(IommuError::NotAvailable);
        }
        let id = DomainId(self.next_domain_id);
        self.next_domain_id = self
            .next_domain_id
            .checked_add(1)
            .ok_or(IommuError::Invalid)?;
        self.domains.insert(id, DomainState { destroyed: false });
        Ok(DmaDomain::new(id, self.unit_index))
    }

    fn destroy_domain(&mut self, domain: DmaDomain) -> Result<(), IommuError> {
        if domain.unit_index() != self.unit_index {
            return Err(IommuError::Invalid);
        }
        let id = domain.id();
        let state = self.domains.get_mut(&id).ok_or(IommuError::Invalid)?;
        if state.destroyed {
            return Err(IommuError::Invalid);
        }
        state.destroyed = true;
        // Drop every mapping belonging to this domain so observable
        // state matches hardware-side device-table-entry teardown.
        self.mappings.retain(|(d, _), _| *d != id);
        // Consume the DmaDomain through the destroy path, marking it
        // as released so Drop does not fire the leak assertion.
        domain.release();
        Ok(())
    }

    fn map(
        &mut self,
        domain: DomainId,
        iova: Iova,
        phys: PhysAddr,
        len: usize,
        flags: MapFlags,
    ) -> Result<(), DomainError> {
        if len == 0 {
            return Err(DomainError::InvalidRange);
        }
        match self.domains.get(&domain) {
            Some(state) if !state.destroyed => {}
            _ => return Err(DomainError::InvalidRange),
        }
        if self.mappings.contains_key(&(domain, iova)) {
            return Err(DomainError::AlreadyMapped);
        }
        self.mappings
            .insert((domain, iova), Mapping { phys, len, flags });
        Ok(())
    }

    fn unmap(&mut self, domain: DomainId, iova: Iova, _len: usize) -> Result<(), DomainError> {
        match self.domains.get(&domain) {
            Some(state) if !state.destroyed => {}
            _ => return Err(DomainError::InvalidRange),
        }
        self.mappings
            .remove(&(domain, iova))
            .ok_or(DomainError::NotMapped)?;
        Ok(())
    }

    fn flush(&mut self, domain: DomainId) -> Result<(), IommuError> {
        if !self.domains.contains_key(&domain) {
            return Err(IommuError::Invalid);
        }
        self.flush_count = self.flush_count.wrapping_add(1);
        Ok(())
    }

    fn install_fault_handler(&mut self, handler: FaultHandlerFn) -> Result<(), IommuError> {
        self.fault_handler = Some(handler);
        Ok(())
    }

    fn capabilities(&self) -> IommuCapabilities {
        MOCK_CAPABILITIES
    }
}
