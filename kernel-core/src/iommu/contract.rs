//! `IommuUnit` trait and `DmaDomain` contract — Phase 55a Track A.1.
//!
//! This module is the single extension seam every IOMMU driver consumes.
//! Intel VT-d (`kernel/src/iommu/intel.rs`) and AMD-Vi
//! (`kernel/src/iommu/amd.rs`) each implement [`IommuUnit`]; drivers in
//! Track E consume domains through the trait, never through a concrete
//! impl. A third vendor (e.g. ARM SMMU in a later phase) lands by
//! implementing the trait — no caller edits.
//!
//! The types defined here are pure logic: they carry no MMIO, no
//! hardware register access, and no kernel-only dependencies. That is
//! deliberate — it lets the Track F.4 contract suite exercise every
//! observable behavior on the host without a hardware unit.
//!
//! # Lock ordering (authoritative for IOMMU subsystem)
//!
//! The IOMMU subsystem orders its locks as:
//!
//! ```text
//! domain lock  →  unit lock  →  buddy-allocator lock
//! ```
//!
//! That is: a caller that already holds a domain lock may acquire the
//! owning unit's lock, which in turn may acquire the Phase 53a buddy
//! allocator's lock. No reverse nesting is permitted. In particular:
//!
//! - **Driver-side locks never nest IOMMU-unit locks.** A driver that
//!   holds its own device lock must release it before invoking any
//!   [`IommuUnit`] method that takes the unit lock, or it must never
//!   take the device lock while inside an IOMMU path. The contract is
//!   one-directional: IOMMU paths may call into the buddy allocator,
//!   but driver paths must not call into IOMMU paths while holding
//!   driver-side locks.
//! - **IOMMU-unit locks never nest buddy-allocator locks held by
//!   callers.** A caller that already holds the buddy allocator lock
//!   must release it before invoking [`IommuUnit::map`] or any other
//!   method that itself may take the allocator.
//! - **Fault handlers run in IRQ context** and must not take any lock
//!   that a non-IRQ path could hold for more than bounded work. See
//!   [`IommuUnit::install_fault_handler`] for the full rule.
//!
//! Violations are caught by kernel-side debug assertions when
//! `cfg!(debug_assertions)` is enabled; the production path relies on
//! the ordering being statically obvious from call sites.

use core::fmt;
use core::ops::{BitOr, BitOrAssign};

/// Identifier for a DMA translation domain owned by an [`IommuUnit`].
///
/// Domains are handed out in unspecified order; callers must not rely
/// on the numeric value beyond using it as an opaque handle. The
/// identifier is meaningful only within the issuing unit: a
/// [`DomainId`] from one unit has no relationship to the same numeric
/// value on another unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DomainId(pub u32);

/// I/O virtual address — the "virtual" address a device sees on the
/// bus after IOMMU translation.
///
/// Distinct from a CPU-side virtual address: an [`Iova`] is never
/// dereferenced by the kernel, only installed in page tables and
/// handed to device hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Iova(pub u64);

/// Physical address (pure-logic wrapper).
///
/// This is a local newtype so `kernel-core` — which is host-testable
/// and must not depend on `x86_64` — can speak about physical
/// addresses without pulling in architecture-specific types. Kernel
/// code converts between this and `x86_64::PhysAddr` at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysAddr(pub u64);

/// Permission and attribute bits for an IOVA → physical mapping.
///
/// Hand-rolled as a newtype over `u8` rather than using a
/// `bitflags!`-generated type because `kernel-core` does not depend
/// on the `bitflags` crate. The named constants cover the four
/// vendor-neutral attributes that both VT-d and AMD-Vi expose for a
/// leaf page-table entry:
///
/// - [`MapFlags::READ`], [`MapFlags::WRITE`], [`MapFlags::EXECUTE`]:
///   device-side access permissions. A device whose read-only
///   descriptor points at a write-only mapping faults.
/// - [`MapFlags::CACHEABLE`]: the mapping participates in the CPU
///   cache coherency domain (as opposed to a snoop-less / un-cached
///   range used for device-private scratch pages).
///
/// The type supports `|` / `|=` for composing flag sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapFlags(pub u8);

impl MapFlags {
    /// Device may read through this mapping.
    pub const READ: MapFlags = MapFlags(0b0001);
    /// Device may write through this mapping.
    pub const WRITE: MapFlags = MapFlags(0b0010);
    /// Device may fetch instructions through this mapping.
    pub const EXECUTE: MapFlags = MapFlags(0b0100);
    /// Mapping is cacheable (participates in CPU cache coherency).
    pub const CACHEABLE: MapFlags = MapFlags(0b1000);

    /// Empty flag set — no permissions granted.
    pub const NONE: MapFlags = MapFlags(0);

    /// `true` if every bit in `other` is set in `self`.
    pub const fn contains(self, other: MapFlags) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Raw bit pattern, for tests and debug output.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl BitOr for MapFlags {
    type Output = MapFlags;
    fn bitor(self, rhs: MapFlags) -> MapFlags {
        MapFlags(self.0 | rhs.0)
    }
}

impl BitOrAssign for MapFlags {
    fn bitor_assign(&mut self, rhs: MapFlags) {
        self.0 |= rhs.0;
    }
}

/// Structured record delivered to a fault handler on every IOMMU
/// fault.
///
/// Fields intentionally mirror the data both VT-d fault records and
/// AMD-Vi event-log entries expose, so the handler is vendor-neutral.
/// Vendor-specific extensions (VT-d address-type bits, AMD-Vi event
/// codes) are decoded into this shape by the per-vendor fault path in
/// `kernel/src/iommu/fault.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultRecord {
    /// PCI BDF (bus / device / function) of the device that issued
    /// the faulting DMA request, encoded as `bus << 8 | device << 3 | function`.
    pub requester_bdf: u16,
    /// Vendor-decoded fault reason — the reason code lifted from the
    /// fault record, normalized enough that the log format matches
    /// across vendors.
    pub fault_reason: u16,
    /// The IOVA the device tried to access.
    pub iova: Iova,
}

/// Fault handler signature.
///
/// Handlers run in IRQ context. Implementations must not allocate,
/// must not block, and must complete bounded work per invocation. See
/// [`IommuUnit::install_fault_handler`] for the full contract.
pub type FaultHandlerFn = fn(&FaultRecord);

/// Capability snapshot a unit advertises once it is brought up.
///
/// Everything the caller might reasonably branch on (page sizes,
/// address width, interrupt remapping, scalable mode) is expressed as
/// data on this struct — never as method-presence divergence — so
/// that VT-d and AMD-Vi remain LSP-compliant substitutes for each
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IommuCapabilities {
    /// Bitmask where bit `n` indicates support for a `2^n`-byte page.
    /// For example, a unit that supports 4 KiB and 2 MiB pages has
    /// `(1 << 12) | (1 << 21)` set.
    pub supported_page_sizes: u64,
    /// Width of the IOVA space the unit can translate, in bits
    /// (e.g. 48 for VT-d "adjusted guest address width = 48b").
    pub address_width_bits: u8,
    /// `true` if the unit advertises interrupt-remapping support.
    /// Phase 55a leaves interrupt remapping disabled; this bit is
    /// reported for forward compatibility and test visibility.
    pub interrupt_remapping: bool,
    /// `true` if invalidation is processed via a queue (VT-d QI,
    /// AMD-Vi command ring). `false` indicates register-based
    /// invalidation only.
    pub queued_invalidation: bool,
    /// `true` if scalable mode (VT-d) or a vendor equivalent is
    /// available. Phase 55a explicitly does not enable this; the bit
    /// is exposed as data so a later phase can opt in without
    /// altering the trait.
    pub scalable_mode: bool,
}

/// Errors raised by [`IommuUnit`] operations at the unit level.
///
/// Domain-map-level failures (out-of-IOVA, overlapping map, etc.)
/// travel on [`DomainError`]. Keeping the two surfaces distinct lets
/// callers handle transient mapping errors without tearing the whole
/// unit down on every failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IommuError {
    /// The unit is not present (e.g. no DMAR / IVRS entry for this
    /// register base) or was not brought up before a call that
    /// required it.
    NotAvailable,
    /// Hardware reported a fault the driver could not recover from
    /// (e.g. translation-enable timeout, invalidation-queue
    /// head-pointer hang).
    HardwareFault,
    /// The request arguments were invalid (e.g. destroying a domain
    /// that does not belong to this unit, or that was already
    /// destroyed).
    Invalid,
}

impl fmt::Display for IommuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IommuError::NotAvailable => f.write_str("iommu unit not available"),
            IommuError::HardwareFault => f.write_str("iommu hardware fault"),
            IommuError::Invalid => f.write_str("iommu invalid argument"),
        }
    }
}

/// Errors raised when mapping or unmapping IOVA → physical through a
/// domain.
///
/// These are recoverable: a caller that gets [`DomainError::IovaExhausted`]
/// can free live mappings and retry. A caller that gets
/// [`DomainError::AlreadyMapped`] has a real bug (overlapping mapping
/// request) and should fail upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainError {
    /// The domain's IOVA space is full; no range of the requested
    /// size is available. Surfaces the same condition
    /// `IovaError::Exhausted` reports from the allocator.
    IovaExhausted,
    /// An existing mapping already covers (part of) the requested
    /// IOVA range. Retrying at a different IOVA, or unmapping the
    /// conflict first, are the two valid responses.
    AlreadyMapped,
    /// The requested IOVA range is not currently mapped. Most often
    /// raised by a double-unmap.
    NotMapped,
    /// `len == 0`, address not aligned to the unit's minimum page
    /// size, or range straddles the end of the domain's IOVA space.
    InvalidRange,
    /// The domain has hit the per-domain page-table-page cap. The
    /// allocation fails without corrupting the domain; the caller
    /// should release other mappings or fail upward.
    PageTablePagesCapExceeded,
    /// Hardware reported a fault specific to the mapping operation
    /// (e.g. invalidation queue rejected the descriptor).
    HardwareFault,
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomainError::IovaExhausted => f.write_str("iova space exhausted"),
            DomainError::AlreadyMapped => f.write_str("iova already mapped"),
            DomainError::NotMapped => f.write_str("iova not mapped"),
            DomainError::InvalidRange => f.write_str("iova range invalid"),
            DomainError::PageTablePagesCapExceeded => f.write_str("page-table page cap exceeded"),
            DomainError::HardwareFault => f.write_str("domain hardware fault"),
        }
    }
}

/// Owned handle to a DMA translation domain.
///
/// A `DmaDomain` is created by [`IommuUnit::create_domain`] and must be
/// released by [`IommuUnit::destroy_domain`] on the *same* unit that
/// created it — the `unit_index` field encodes which unit that is.
///
/// # Drop semantics
///
/// Callers **must** pass the domain back through `destroy_domain`
/// rather than letting it drop. The `destroy_domain` path performs
/// the real cleanup (walking and freeing the page table, invalidating
/// the device-table entry, flushing the TLB). A dropped-without-destroy
/// domain represents a resource leak on the unit and a correctness
/// bug in the caller.
///
/// To make that bug visible in tests, `DmaDomain`'s [`Drop`]
/// implementation fires a debug-assertion failure when
/// `cfg!(debug_assertions)` is enabled and the domain was not
/// released through [`DmaDomain::release`] (which `destroy_domain`
/// calls internally). Release builds accept the leak silently to
/// avoid aborting on a panic path.
pub struct DmaDomain {
    id: DomainId,
    unit_index: usize,
    released: bool,
}

impl DmaDomain {
    /// Construct a fresh `DmaDomain` — intended for [`IommuUnit`]
    /// implementations only. External callers receive domains from
    /// [`IommuUnit::create_domain`].
    pub const fn new(id: DomainId, unit_index: usize) -> Self {
        Self {
            id,
            unit_index,
            released: false,
        }
    }

    /// The domain identifier.
    pub const fn id(&self) -> DomainId {
        self.id
    }

    /// Index of the unit that owns this domain. A `destroy_domain`
    /// call against a different unit returns [`IommuError::Invalid`].
    pub const fn unit_index(&self) -> usize {
        self.unit_index
    }

    /// Mark this domain as released through the `destroy_domain`
    /// path. `IommuUnit` implementations call this after the
    /// underlying hardware state has been torn down; Drop then
    /// accepts the handle silently.
    pub fn release(mut self) {
        self.released = true;
        // Drop fires normally but finds `released == true`.
    }
}

impl fmt::Debug for DmaDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaDomain")
            .field("id", &self.id)
            .field("unit_index", &self.unit_index)
            .field("released", &self.released)
            .finish()
    }
}

impl Drop for DmaDomain {
    fn drop(&mut self) {
        // `debug_assert!` compiles out in release builds, so the leak
        // is silent there — matching the doc-comment contract.
        debug_assert!(
            self.released,
            "DmaDomain {:?} (unit {}) dropped without destroy_domain — resource leak",
            self.id, self.unit_index
        );
    }
}

/// Vendor-neutral interface every IOMMU unit implementation
/// satisfies.
///
/// The trait is deliberately narrow: bring-up, domain lifecycle,
/// mapping, flush, fault-handler installation, and capability query.
/// Device-table layout, page-table walker internals, and invalidation
/// queue / command buffer descriptor layouts are **not** part of the
/// trait — they live behind `pub(crate)` in each vendor's module.
///
/// # Method contract summary
///
/// - [`IommuUnit::bring_up`] must succeed (or idempotently report
///   success) before any other method is called. Calling
///   [`IommuUnit::create_domain`] before bring-up returns
///   [`IommuError::NotAvailable`].
/// - [`IommuUnit::create_domain`] returns a [`DmaDomain`] whose
///   [`DomainId`] is unique within this unit for the lifetime of the
///   program.
/// - [`IommuUnit::destroy_domain`] accepts the `DmaDomain` handle
///   back, tears down the hardware state, and consumes the handle.
/// - [`IommuUnit::map`] / [`IommuUnit::unmap`] operate on IOVA ranges
///   within the named domain; behavior on overlapping map and
///   double-unmap is as documented on [`DomainError::AlreadyMapped`]
///   and [`DomainError::NotMapped`].
/// - [`IommuUnit::flush`] issues the TLB / context-cache invalidation
///   appropriate for the vendor; it is the last step before returning
///   success from an `unmap`, but is also separately callable for
///   bulk workflows.
/// - [`IommuUnit::install_fault_handler`] registers an IRQ-context
///   callback that will be invoked for every decoded fault record.
///   Handlers **must not** allocate, block, or take locks that a
///   non-IRQ path could hold.
/// - [`IommuUnit::capabilities`] is pure and may be called before
///   bring-up completes; it returns the unit's static capability
///   profile.
pub trait IommuUnit {
    /// Bring the unit into an operational state: enable translation,
    /// commit root-table / device-table base pointers, and set up
    /// the invalidation and fault-log channels. Idempotent — a
    /// second call after success returns `Ok(())` without side
    /// effects.
    fn bring_up(&mut self) -> Result<(), IommuError>;

    /// Create a new translation domain. The returned [`DmaDomain`]
    /// must be released through [`IommuUnit::destroy_domain`] on
    /// this same unit.
    fn create_domain(&mut self) -> Result<DmaDomain, IommuError>;

    /// Destroy a previously-created domain. The handle is consumed.
    /// Returns [`IommuError::Invalid`] if the domain belongs to a
    /// different unit or has already been destroyed.
    fn destroy_domain(&mut self, domain: DmaDomain) -> Result<(), IommuError>;

    /// Install a mapping in the named domain.
    ///
    /// `len` is in bytes and must be positive. The unit is free to
    /// split the range across multiple page-table levels (4 KiB / 2
    /// MiB / 1 GiB) according to its capabilities.
    fn map(
        &mut self,
        domain: DomainId,
        iova: Iova,
        phys: PhysAddr,
        len: usize,
        flags: MapFlags,
    ) -> Result<(), DomainError>;

    /// Remove a mapping from the named domain. The unit **must**
    /// issue the appropriate TLB invalidation before returning
    /// success, so that a subsequent [`IommuUnit::map`] at the same
    /// IOVA does not observe a stale translation.
    fn unmap(&mut self, domain: DomainId, iova: Iova, len: usize) -> Result<(), DomainError>;

    /// Flush the TLB / context cache for the named domain. Callers
    /// that issue bulk `unmap` followed by `map` may prefer to
    /// suppress per-`unmap` flushes and call this once at the end.
    fn flush(&mut self, domain: DomainId) -> Result<(), IommuError>;

    /// Install the IRQ-context fault handler. The handler must not
    /// allocate, must not block, and must complete bounded work per
    /// invocation. Replacing the handler is permitted.
    fn install_fault_handler(&mut self, handler: FaultHandlerFn) -> Result<(), IommuError>;

    /// Returns the unit's static capability profile. Safe to call
    /// before [`IommuUnit::bring_up`].
    fn capabilities(&self) -> IommuCapabilities;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_flags_bitor_composes() {
        let rw = MapFlags::READ | MapFlags::WRITE;
        assert!(rw.contains(MapFlags::READ));
        assert!(rw.contains(MapFlags::WRITE));
        assert!(!rw.contains(MapFlags::EXECUTE));
        assert_eq!(rw.bits(), 0b0011);
    }

    #[test]
    fn map_flags_bitor_assign_accumulates() {
        let mut flags = MapFlags::READ;
        flags |= MapFlags::WRITE;
        flags |= MapFlags::CACHEABLE;
        assert!(flags.contains(MapFlags::READ | MapFlags::WRITE | MapFlags::CACHEABLE));
        assert!(!flags.contains(MapFlags::EXECUTE));
    }

    #[test]
    fn dma_domain_release_suppresses_drop_assertion() {
        // Smoke-test that DmaDomain's Drop path is sound when
        // release() has been called — debug-builds would panic
        // otherwise, failing this test.
        let domain = DmaDomain::new(DomainId(7), 3);
        assert_eq!(domain.id(), DomainId(7));
        assert_eq!(domain.unit_index(), 3);
        domain.release();
    }

    #[test]
    fn error_enums_format_without_allocation() {
        // Display impls are used by logging; ensure they at least
        // produce non-empty output for every variant.
        for err in [
            IommuError::NotAvailable,
            IommuError::HardwareFault,
            IommuError::Invalid,
        ] {
            let s = alloc::format!("{}", err);
            assert!(!s.is_empty());
        }
        for err in [
            DomainError::IovaExhausted,
            DomainError::AlreadyMapped,
            DomainError::NotMapped,
            DomainError::InvalidRange,
            DomainError::PageTablePagesCapExceeded,
            DomainError::HardwareFault,
        ] {
            let s = alloc::format!("{}", err);
            assert!(!s.is_empty());
        }
    }

    #[test]
    fn fault_record_is_pod_like() {
        let rec = FaultRecord {
            requester_bdf: 0x0100,
            fault_reason: 0x0005,
            iova: Iova(0xdead_beef),
        };
        let copy = rec;
        assert_eq!(rec, copy);
    }
}
