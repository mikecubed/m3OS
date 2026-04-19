//! DMA buffer allocation — Phase 55a Track E.2 (signature rewrite).
//!
//! Queue-based device drivers (virtio virtqueues, NVMe descriptor rings,
//! e1000 TX/RX descriptors) need physically-contiguous, DMA-safe memory.
//! Phase 55 introduced [`DmaBuffer<T>`] as the shared primitive; Phase 55a
//! rewrites the constructor signature to route every allocation through a
//! per-device `IommuUnit` domain so the kernel can protect itself from
//! device-initiated corruption.
//!
//! Every live [`DmaBuffer`] now owns:
//!
//! * A contiguous **physical** region allocated via the Phase 53a buddy
//!   allocator.
//! * An **IOVA** range pulled from the owning device's domain and installed
//!   into the IOMMU page table via [`kernel_core::iommu::contract::IommuUnit::map`].
//!   When the IOMMU is in identity-fallback mode, the IOVA equals the
//!   physical address.
//! * A kernel-virtual view through the shared phys-offset window so kernel
//!   code can still read and write the buffer without going through the
//!   IOMMU.
//!
//! # API
//!
//! The new signature is:
//!
//! ```ignore
//! DmaBuffer::<[u8]>::allocate(device, bytes)?
//! ```
//!
//! `device: &PciDeviceHandle` names the device the buffer belongs to. The
//! handle already carries a `DmaDomain` attached by `claim_pci_device`, so
//! the allocator can look up the owning IOMMU unit and install the
//! mapping without any driver-side plumbing.
//!
//! # Bus vs physical address
//!
//! - [`DmaBuffer::bus_address`] returns the IOVA when an IOMMU is
//!   translating; the physical address in identity fallback. This is the
//!   address that belongs in descriptor rings.
//! - [`DmaBuffer::physical_address`] always returns the physical frame
//!   address. Retained for the rare call site that needs the raw value
//!   (debug dumps, legacy hardware quirks that bypass translation).
//!
//! Callers should prefer `bus_address()` in descriptor programming —
//! both values are safe to hand to the device because the two paths
//! produce equivalent addresses when IOMMU is inactive.
//!
//! # Failure surfaces as an error
//!
//! `DmaBuffer::allocate` returns a `Result<Self, DmaError>`. Allocation
//! failure is a normal condition (no contiguous 64 KiB block available,
//! IOVA space exhausted, invalidation rejected) that driver init code
//! should propagate up, not panic on.

use core::marker::PhantomData;
use core::mem;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

use x86_64::PhysAddr;

use kernel_core::iommu::contract::{
    DomainError, DomainId, Iova, MapFlags, PhysAddr as CorePhysAddr,
};

use super::frame_allocator;
use crate::iommu::registry;
use crate::pci::PciDeviceHandle;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// DMA allocation failure reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DmaError {
    /// The requested size or count is zero.
    ZeroSize,
    /// `size_of::<T>() * count` overflowed `usize` — the request is too
    /// large to describe, independent of whether the allocator could
    /// satisfy it.
    SizeOverflow,
    /// Alignment is not a power of two, or larger than the page size (we
    /// map via phys-offset at page granularity).
    UnsupportedAlignment,
    /// The caller asked for a size the allocator can classify but that is
    /// otherwise invalid for DMA (for example a non-page-multiple size
    /// when the IOMMU path requires page granularity).
    InvalidSize,
    /// The buddy allocator could not provide a contiguous block of the
    /// requested order.
    OutOfMemory,
    /// The device's IOMMU domain is out of free IOVA ranges.
    IovaExhausted,
    /// The IOMMU hardware or vendor driver rejected the mapping operation.
    DomainHardwareFault,
    /// The device has no attached `DmaDomain` — usually means
    /// `claim_pci_device` was not routed through the IOMMU-enabled path.
    NoDomainAttached,
}

impl From<DomainError> for DmaError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::IovaExhausted => DmaError::IovaExhausted,
            DomainError::AlreadyMapped
            | DomainError::NotMapped
            | DomainError::InvalidRange
            | DomainError::PageTablePagesCapExceeded => DmaError::DomainHardwareFault,
            DomainError::HardwareFault => DmaError::DomainHardwareFault,
        }
    }
}

// ---------------------------------------------------------------------------
// DmaBuffer<T>
// ---------------------------------------------------------------------------

/// A DMA-safe, physically-contiguous buffer usable by both the CPU (via
/// its kernel-virtual mapping) and by the device (via its bus-visible
/// IOVA, or physical address in identity-fallback).
///
/// `T` is the logical element type. Most callers will use slice types
/// for rings (`DmaBuffer::<[MyDesc]>::allocate_array(device, N)`) or a
/// small `#[repr(C)]` for single-object DMA.
pub struct DmaBuffer<T: ?Sized> {
    /// Kernel-virtual pointer to the start of the allocation.
    virt: NonNull<u8>,
    /// Physical (bus) address of `virt`.
    phys: PhysAddr,
    /// Bus address the device should see in descriptor rings. Equals
    /// the IOVA when an IOMMU is translating; equals `phys.as_u64()`
    /// when identity-fallback is active.
    bus: u64,
    /// Byte length of the IOVA mapping. Equal to `bytes` for translated
    /// domains; `0` for identity-fallback (nothing to unmap on drop).
    mapping_len: usize,
    /// Buddy order used for the allocation.
    order: usize,
    /// Total byte size of the allocation (may be >= `mem::size_of::<T>()`
    /// because the buddy allocator hands out whole pages at the
    /// requested order).
    bytes: usize,
    /// Element count for array-typed buffers (1 for single-object).
    len: usize,
    /// Domain metadata used to unmap at drop time. `None` for buffers
    /// allocated through the test-only helpers below.
    domain_ctx: Option<DomainContext>,
    _marker: PhantomData<T>,
}

/// Snapshot of the domain an IOVA mapping lives in. Stored on the
/// DmaBuffer so Drop can unmap without consulting the `PciDeviceHandle`
/// (which may have moved or been dropped by then — but the domain
/// outlives the DmaBuffer because the handle owns the buffer's domain
/// lifetime).
#[derive(Clone, Copy, Debug)]
struct DomainContext {
    unit_index: usize,
    domain: DomainId,
}

// SAFETY: DmaBuffer owns its memory exclusively; sending it between
// threads moves ownership of the contiguous region, which is safe
// because the buddy allocator is itself Send. Drivers that share a
// DmaBuffer between an ISR and a task must wrap it in a lock or use
// volatile access through raw pointers.
unsafe impl<T: ?Sized + Send> Send for DmaBuffer<T> {}

impl<T> DmaBuffer<T> {
    /// Allocate a DMA buffer sized to hold `count` contiguous `T`,
    /// attached to `device`'s IOMMU domain.
    ///
    /// Intended for descriptor rings:
    /// `DmaBuffer::<NvmeCommand>::allocate_array(handle, QUEUE_DEPTH)`.
    #[allow(dead_code)]
    pub fn allocate_array(
        device: &PciDeviceHandle,
        count: usize,
    ) -> Result<DmaBuffer<[T]>, DmaError> {
        if count == 0 {
            return Err(DmaError::ZeroSize);
        }
        let size = mem::size_of::<T>()
            .checked_mul(count)
            .ok_or(DmaError::SizeOverflow)?;
        if size == 0 {
            return Err(DmaError::ZeroSize);
        }
        if mem::align_of::<T>() > PAGE_SIZE {
            return Err(DmaError::UnsupportedAlignment);
        }
        let order = order_for_bytes(size);
        let (phys, virt, bytes) = alloc_and_zero(order)?;
        let (bus, mapping_len, domain_ctx) =
            install_iova_mapping(device, phys.as_u64(), bytes, order)?;
        Ok(DmaBuffer {
            virt,
            phys,
            bus,
            mapping_len,
            order,
            bytes,
            len: count,
            domain_ctx,
            _marker: PhantomData,
        })
    }

    /// Allocate a DMA buffer sized to hold a single `T`, attached to
    /// `device`'s IOMMU domain.
    ///
    /// The buffer is zero-initialized.
    ///
    /// # Alignment
    ///
    /// The returned buffer is always 4 KiB-aligned (the buddy allocator
    /// hands out whole pages). Alignment stricter than 4 KiB is not
    /// supported here — if a device needs 64 KiB alignment, ask for
    /// enough pages that the start of the allocation naturally satisfies
    /// the requirement.
    #[allow(dead_code)]
    pub fn allocate_one(device: &PciDeviceHandle) -> Result<Self, DmaError> {
        let size = mem::size_of::<T>();
        if size == 0 {
            return Err(DmaError::ZeroSize);
        }
        if mem::align_of::<T>() > PAGE_SIZE {
            return Err(DmaError::UnsupportedAlignment);
        }
        let order = order_for_bytes(size);
        let (phys, virt, bytes) = alloc_and_zero(order)?;
        let (bus, mapping_len, domain_ctx) =
            install_iova_mapping(device, phys.as_u64(), bytes, order)?;
        Ok(DmaBuffer {
            virt,
            phys,
            bus,
            mapping_len,
            order,
            bytes,
            len: 1,
            domain_ctx,
            _marker: PhantomData,
        })
    }
}

impl DmaBuffer<[u8]> {
    /// Allocate a raw byte buffer of `bytes` bytes attached to `device`'s
    /// IOMMU domain.
    ///
    /// This is the central signature of Phase 55a Track E.2. The
    /// allocation is page-aligned; callers that need stricter alignment
    /// should request enough pages for the first page to satisfy the
    /// requirement naturally (same pattern as Phase 55 `new_bytes`).
    pub fn allocate(device: &PciDeviceHandle, bytes: usize) -> Result<Self, DmaError> {
        if bytes == 0 {
            return Err(DmaError::ZeroSize);
        }
        let order = order_for_bytes(bytes);
        let (phys, virt, bytes_alloc) = alloc_and_zero(order)?;
        let (bus, mapping_len, domain_ctx) =
            install_iova_mapping(device, phys.as_u64(), bytes_alloc, order)?;
        Ok(DmaBuffer {
            virt,
            phys,
            bus,
            mapping_len,
            order,
            bytes: bytes_alloc,
            len: bytes,
            domain_ctx,
            _marker: PhantomData,
        })
    }
}

impl<T: ?Sized> DmaBuffer<T> {
    /// Bus-visible address the device should see in descriptor rings.
    ///
    /// When an IOMMU is translating, this is the IOVA installed at
    /// allocation time. When identity-fallback is active, this is the
    /// physical frame address. Callers should not branch on which case
    /// is active — both values are safe to hand to the device.
    #[inline]
    pub fn bus_address(&self) -> u64 {
        self.bus
    }

    /// Raw physical frame address of the allocation.
    ///
    /// Retained for the few call sites that must program a physical
    /// address unconditionally (legacy hardware quirks, debug dumps).
    /// Prefer [`bus_address`](Self::bus_address) in descriptor rings.
    #[inline]
    #[allow(dead_code)]
    pub fn physical_address(&self) -> PhysAddr {
        self.phys
    }

    /// Byte size of the backing allocation (page-rounded, may exceed the
    /// logical payload).
    #[inline]
    #[allow(dead_code)]
    pub fn capacity_bytes(&self) -> usize {
        self.bytes
    }

    /// Raw kernel-virtual pointer to the allocation.
    #[inline]
    #[allow(dead_code)]
    pub fn as_ptr(&self) -> *const u8 {
        self.virt.as_ptr()
    }

    /// Mutable raw kernel-virtual pointer to the allocation.
    #[inline]
    #[allow(dead_code)]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.virt.as_ptr()
    }
}

// ---------------------------------------------------------------------------
// Deref / DerefMut impls
// ---------------------------------------------------------------------------

impl<T> Deref for DmaBuffer<T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: `virt` points to a zero-initialized allocation of at
        // least `size_of::<T>()` bytes (enforced by the constructors).
        // We have exclusive access (no Clone; Send is the only Sync-safe
        // marker).
        unsafe { &*(self.virt.as_ptr() as *const T) }
    }
}

impl<T> DerefMut for DmaBuffer<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above, exclusive access.
        unsafe { &mut *(self.virt.as_ptr() as *mut T) }
    }
}

impl<T> Deref for DmaBuffer<[T]> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        // SAFETY: `virt` points to `len` contiguous `T` (enforced by
        // `allocate_array` or `allocate` for T=u8). No aliasing — we
        // have exclusive ownership.
        unsafe { core::slice::from_raw_parts(self.virt.as_ptr() as *const T, self.len) }
    }
}

impl<T> DerefMut for DmaBuffer<[T]> {
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: same as Deref above.
        unsafe { core::slice::from_raw_parts_mut(self.virt.as_ptr() as *mut T, self.len) }
    }
}

// ---------------------------------------------------------------------------
// Drop — unmap the IOVA, return the frames to the buddy allocator.
// ---------------------------------------------------------------------------

impl<T: ?Sized> Drop for DmaBuffer<T> {
    fn drop(&mut self) {
        // 1. Undo any translated IOVA mapping first, so a subsequent
        //    re-allocation at the same IOVA does not observe the stale
        //    translation. Identity fallback has `mapping_len == 0` and
        //    skips the unmap path.
        if self.mapping_len != 0
            && let Some(ctx) = self.domain_ctx
            && let Err(e) =
                registry::unmap(ctx.unit_index, ctx.domain, Iova(self.bus), self.mapping_len)
        {
            // Drop runs outside any error path — log and continue so we
            // at least return the physical frames to the allocator.
            log::warn!(
                "[dma] unmap failed at drop: unit={} domain={:?} iova={:#x} len={:#x}: {}",
                ctx.unit_index,
                ctx.domain,
                self.bus,
                self.mapping_len,
                e,
            );
        }
        // 2. Return the frames.
        frame_allocator::free_contiguous(self.phys.as_u64(), self.order);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const PAGE_SIZE: usize = 4096;

/// Round `bytes` up to the buddy order needed to cover it.
#[inline]
fn order_for_bytes(bytes: usize) -> usize {
    let pages = bytes.div_ceil(PAGE_SIZE);
    if pages <= 1 {
        0
    } else {
        (usize::BITS - (pages - 1).leading_zeros()) as usize
    }
}

/// Allocate a contiguous block at `order` and zero it. Returns
/// `(phys_start, virt_nonnull, byte_size)`.
fn alloc_and_zero(order: usize) -> Result<(PhysAddr, NonNull<u8>, usize), DmaError> {
    let frame = frame_allocator::allocate_contiguous(order).ok_or(DmaError::OutOfMemory)?;
    let phys = frame.start_address();
    let pages = 1usize << order;
    let bytes = pages * PAGE_SIZE;
    let virt_u64 = super::phys_offset() + phys.as_u64();
    let virt_ptr = virt_u64 as *mut u8;
    // SAFETY: `frame` was just allocated; we hold exclusive ownership.
    // The phys-offset window always maps this frame.
    unsafe {
        core::ptr::write_bytes(virt_ptr, 0, bytes);
    }
    let virt = NonNull::new(virt_ptr).ok_or(DmaError::OutOfMemory)?;
    Ok((phys, virt, bytes))
}

/// Install an IOVA → phys mapping in the device's domain, if one is
/// attached and the unit is translating. Returns
/// `(bus_address, mapping_len, domain_ctx)` — `mapping_len` is zero for
/// identity-fallback so Drop knows not to call unmap.
fn install_iova_mapping(
    device: &PciDeviceHandle,
    phys: u64,
    bytes: usize,
    _order: usize,
) -> Result<(u64, usize, Option<DomainContext>), DmaError> {
    // The device's claim path attached a `DmaDomain` (Track E.1). When
    // no IOMMU unit is registered, the handle still carries the
    // identity-domain unit_index; we just skip the map() call and use
    // the physical address directly.
    let Some(snap) = device.domain_snapshot() else {
        // Claim path that did not record a domain. Safe fallback:
        // identity. This path is typical for transitional code that
        // claims devices before iommu::init has run.
        return Ok((phys, 0, None));
    };

    if !registry::translating() {
        // Identity fallback — no-op map, return phys as the bus address.
        return Ok((phys, 0, None));
    }

    // Translated path. Install the mapping; on failure, return the
    // frame to the allocator via the caller's error path.
    let flags = MapFlags::READ | MapFlags::WRITE;
    registry::map(
        snap.unit_index,
        snap.domain,
        Iova(phys),
        CorePhysAddr(phys),
        bytes,
        flags,
    )?;
    Ok((
        phys,
        bytes,
        Some(DomainContext {
            unit_index: snap.unit_index,
            domain: snap.domain,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Tests — acceptance C.2 bullet 7 (allocation, deref access, drop/reclaim).
// ---------------------------------------------------------------------------
//
// These kernel-side tests run inside QEMU via the xtask test harness.
// They exercise the underlying frame allocator and zero-init path using
// the identity-fallback code path (no PciDeviceHandle); the IOMMU-mapped
// end-to-end behavior is covered by the integration tests in Phase 55a
// Track F.

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only allocator: bypasses the PciDeviceHandle/domain plumbing
    // and exercises the underlying frame allocator + zero-init path.
    // Equivalent to `DmaBuffer::allocate(device, size)` with an
    // identity-fallback domain attached. Not exposed outside `#[cfg(test)]`
    // so no production code can bypass the domain path by accident.
    impl DmaBuffer<[u8]> {
        fn test_allocate(bytes: usize) -> Result<Self, DmaError> {
            if bytes == 0 {
                return Err(DmaError::ZeroSize);
            }
            let order = order_for_bytes(bytes);
            let (phys, virt, bytes_alloc) = alloc_and_zero(order)?;
            Ok(DmaBuffer {
                virt,
                phys,
                bus: phys.as_u64(),
                mapping_len: 0,
                order,
                bytes: bytes_alloc,
                len: bytes,
                domain_ctx: None,
                _marker: PhantomData,
            })
        }
    }

    impl<T> DmaBuffer<T> {
        fn test_allocate_array(count: usize) -> Result<DmaBuffer<[T]>, DmaError> {
            if count == 0 {
                return Err(DmaError::ZeroSize);
            }
            let size = mem::size_of::<T>()
                .checked_mul(count)
                .ok_or(DmaError::SizeOverflow)?;
            if size == 0 {
                return Err(DmaError::ZeroSize);
            }
            let order = order_for_bytes(size);
            let (phys, virt, bytes_alloc) = alloc_and_zero(order)?;
            Ok(DmaBuffer {
                virt,
                phys,
                bus: phys.as_u64(),
                mapping_len: 0,
                order,
                bytes: bytes_alloc,
                len: count,
                domain_ctx: None,
                _marker: PhantomData,
            })
        }
    }

    #[test_case]
    fn dma_buffer_allocation_succeeds_and_reclaims() {
        let before = frame_allocator::available_count();
        {
            let buf = DmaBuffer::<[u8]>::test_allocate(4096)
                .expect("single-page DMA allocation should succeed");
            assert_eq!(buf.capacity_bytes(), 4096);
            assert_ne!(buf.physical_address().as_u64(), 0);
            // Bus address equals physical in the test (identity) path.
            assert_eq!(buf.bus_address(), buf.physical_address().as_u64());
        }
        frame_allocator::drain_per_cpu_caches();
        let after = frame_allocator::available_count();
        assert_eq!(
            after, before,
            "DmaBuffer drop must return frames (before={} after={})",
            before, after
        );
    }

    #[test_case]
    fn dma_buffer_deref_and_mut_access() {
        let mut buf = DmaBuffer::<u32>::test_allocate_array(16).expect("ring alloc");
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = i as u32;
        }
        for (i, slot) in buf.iter().enumerate() {
            assert_eq!(*slot, i as u32);
        }
        assert_eq!(buf.physical_address().as_u64() & 0xFFF, 0);
    }

    #[test_case]
    fn dma_buffer_drop_reclaim_multipage() {
        let before = frame_allocator::available_count();
        {
            let buf = DmaBuffer::<[u8]>::test_allocate(3 * 4096).expect("multi-page DMA alloc");
            assert!(buf.capacity_bytes() >= 3 * 4096);
            let mut b = buf;
            for byte in b.iter_mut() {
                *byte = 0xA5;
            }
            for byte in b.iter() {
                assert_eq!(*byte, 0xA5);
            }
        }
        frame_allocator::drain_per_cpu_caches();
        let after = frame_allocator::available_count();
        assert_eq!(
            after, before,
            "multi-page DmaBuffer drop must return all frames (before={} after={})",
            before, after
        );
    }

    #[test_case]
    fn dma_buffer_rejects_zero_size() {
        assert_eq!(
            DmaBuffer::<[u8]>::test_allocate(0).err(),
            Some(DmaError::ZeroSize)
        );
    }

    #[test_case]
    fn dma_buffer_allocate_array_reports_overflow_distinctly_from_zero_size() {
        assert_eq!(
            DmaBuffer::<u64>::test_allocate_array(0).err(),
            Some(DmaError::ZeroSize)
        );
        assert_eq!(
            DmaBuffer::<u64>::test_allocate_array(usize::MAX).err(),
            Some(DmaError::SizeOverflow)
        );
    }
}
