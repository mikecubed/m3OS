//! DMA buffer allocation — Phase 55 C.2.
//!
//! Queue-based device drivers (virtio virtqueues, NVMe descriptor rings, e1000
//! TX/RX descriptors) need physically-contiguous, DMA-safe memory. Before
//! this module each driver open-coded `frame_allocator::allocate_contiguous`
//! and manually tracked the (phys, virt) pair.
//!
//! A [`DmaBuffer<T>`] owns a contiguous physical region plus its kernel-virtual
//! mapping, exposes the physical address via [`DmaBuffer::physical_address`]
//! for programming into descriptor rings, and returns the frames to the buddy
//! allocator on drop. The kernel-virtual mapping uses the shared phys-offset
//! window; the buddy allocator hands us pages that are already writable from
//! the kernel, and x86-64 defaults to strong-uncacheable MMIO outside the RAM
//! range so normal RAM DMA buffers see the standard writeback caching — which
//! is the correct behaviour for host↔device memory rings (the device coherently
//! snoops the CPU cache).
//!
//! The type parameter `T` on [`DmaBuffer<T>`] gives ergonomic typed access:
//! a descriptor ring driver can do `dma.as_mut()[i] = desc;` instead of
//! wrangling raw pointers everywhere.
//!
//! # Failure surfaces as an error
//!
//! `DmaBuffer::new*` returns a `Result` — allocation failure is a normal
//! condition (e.g. no contiguous 64 KiB block available at init) that driver
//! init code should propagate up, not panic on.

use core::marker::PhantomData;
use core::mem;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

use x86_64::PhysAddr;

use super::frame_allocator;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// DMA allocation failure reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// The requested size or count is zero.
    ZeroSize,
    /// Alignment is not a power of two, or larger than the page size (we map
    /// via phys-offset at page granularity).
    UnsupportedAlignment,
    /// The buddy allocator could not provide a contiguous block of the
    /// requested order.
    OutOfMemory,
}

// ---------------------------------------------------------------------------
// DmaBuffer<T>
// ---------------------------------------------------------------------------

/// A DMA-safe, physically-contiguous buffer usable by both the CPU (via its
/// kernel-virtual mapping) and by the device (via its bus-visible physical
/// address).
///
/// `T` is the logical element type. Most callers will use slice types for
/// rings (`DmaBuffer::<[MyDesc]>::new_array(N)`) or a small `#[repr(C)]` for
/// single-object DMA (`DmaBuffer::<MyCmd>::new()`).
pub struct DmaBuffer<T: ?Sized> {
    /// Kernel-virtual pointer to the start of the allocation.
    virt: NonNull<u8>,
    /// Physical (bus) address of `virt`.
    phys: PhysAddr,
    /// Buddy order used for the allocation.
    order: usize,
    /// Total byte size of the allocation (may be >= mem::size_of::<T>()
    /// because the buddy allocator hands out whole pages at the requested
    /// order).
    bytes: usize,
    /// Element count for array-typed buffers (1 for single-object).
    len: usize,
    _marker: PhantomData<T>,
}

// SAFETY: DmaBuffer owns its memory exclusively; sending it between threads
// moves ownership of the contiguous region, which is safe because the buddy
// allocator is itself Send. Drivers that share a DmaBuffer between an ISR
// and a task must wrap it in a lock or use volatile access through raw
// pointers.
unsafe impl<T: ?Sized + Send> Send for DmaBuffer<T> {}

impl<T> DmaBuffer<T> {
    /// Allocate a DMA buffer sized to hold a single `T`.
    ///
    /// The buffer is zero-initialized.
    ///
    /// # Alignment
    ///
    /// The returned buffer is always 4 KiB-aligned (because the buddy
    /// allocator hands out whole pages). Alignment stricter than 4 KiB is not
    /// supported here — if a device needs 64 KiB alignment, ask for enough
    /// pages that the start of the allocation naturally satisfies the
    /// requirement. (This is the same pattern the existing virtio code uses.)
    #[allow(dead_code)]
    pub fn new() -> Result<Self, DmaError> {
        let size = mem::size_of::<T>();
        if size == 0 {
            return Err(DmaError::ZeroSize);
        }
        // `T` must not require alignment stricter than a page.
        if mem::align_of::<T>() > PAGE_SIZE {
            return Err(DmaError::UnsupportedAlignment);
        }
        let order = order_for_bytes(size);
        let (phys, virt, bytes) = alloc_and_zero(order)?;
        Ok(DmaBuffer {
            virt,
            phys,
            order,
            bytes,
            len: 1,
            _marker: PhantomData,
        })
    }

    /// Allocate a DMA buffer sized to hold `count` contiguous `T`.
    ///
    /// `count` must be at least 1. Intended for descriptor rings:
    /// `DmaBuffer::<NvmeCommand>::new_array(QUEUE_DEPTH)`.
    #[allow(dead_code)]
    pub fn new_array(count: usize) -> Result<DmaBuffer<[T]>, DmaError> {
        if count == 0 {
            return Err(DmaError::ZeroSize);
        }
        let size = mem::size_of::<T>()
            .checked_mul(count)
            .ok_or(DmaError::ZeroSize)?;
        if size == 0 {
            return Err(DmaError::ZeroSize);
        }
        if mem::align_of::<T>() > PAGE_SIZE {
            return Err(DmaError::UnsupportedAlignment);
        }
        let order = order_for_bytes(size);
        let (phys, virt, bytes) = alloc_and_zero(order)?;
        Ok(DmaBuffer {
            virt,
            phys,
            order,
            bytes,
            len: count,
            _marker: PhantomData,
        })
    }
}

impl DmaBuffer<[u8]> {
    /// Allocate a raw byte buffer of `size` bytes with at least `alignment`
    /// alignment. `alignment` must be a power of two, 1..=4096 — the buddy
    /// allocator's minimum unit is 4 KiB so every returned buffer is already
    /// page-aligned and the `alignment` parameter is validated as an upper
    /// bound.
    #[allow(dead_code)]
    pub fn new_bytes(size: usize, alignment: usize) -> Result<Self, DmaError> {
        if size == 0 {
            return Err(DmaError::ZeroSize);
        }
        if !alignment.is_power_of_two() || alignment > PAGE_SIZE {
            return Err(DmaError::UnsupportedAlignment);
        }
        let order = order_for_bytes(size);
        let (phys, virt, bytes) = alloc_and_zero(order)?;
        Ok(DmaBuffer {
            virt,
            phys,
            order,
            bytes,
            len: size,
            _marker: PhantomData,
        })
    }
}

impl<T: ?Sized> DmaBuffer<T> {
    /// Bus-visible physical address of the allocation.
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
        // SAFETY: `virt` points to a zero-initialized allocation of at least
        // `size_of::<T>()` bytes (enforced by `DmaBuffer::new`). We have
        // exclusive access (no Clone; Send is the only Sync-safe marker).
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
        // `new_array` or `new_bytes` for T=u8). No aliasing — we have
        // exclusive ownership.
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
// Drop — return the frames to the buddy allocator.
// ---------------------------------------------------------------------------

impl<T: ?Sized> Drop for DmaBuffer<T> {
    fn drop(&mut self) {
        // `allocate_contiguous(order)` in the frame allocator is paired with
        // `free_contiguous(phys, order)`; the virtual mapping lives in the
        // phys-offset window and does not require explicit unmapping
        // (unmapping one slice of the phys-offset window would break other
        // kernel code that expects the window intact).
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

/// Allocate a contiguous block at `order` and zero it. Returns (phys_start,
/// virt_nonnull, byte_size).
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

// ---------------------------------------------------------------------------
// Tests — acceptance C.2 bullet 7 (allocation, deref access, drop/reclaim).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn dma_buffer_allocation_succeeds_and_reclaims() {
        let before = frame_allocator::available_count();
        {
            let buf = DmaBuffer::<[u8]>::new_bytes(4096, 4096)
                .expect("single-page DMA allocation should succeed");
            assert_eq!(buf.capacity_bytes(), 4096);
            assert_ne!(buf.physical_address().as_u64(), 0);
        }
        // Buffer dropped — frames returned to buddy allocator.
        // Drain per-CPU caches so refcount-free frames actually show up as
        // available again.
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
        // Array of u32: simulate a descriptor ring.
        let mut buf = DmaBuffer::<u32>::new_array(16).expect("ring alloc");
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = i as u32;
        }
        for (i, slot) in buf.iter().enumerate() {
            assert_eq!(*slot, i as u32);
        }
        // Physical address must be page-aligned.
        assert_eq!(buf.physical_address().as_u64() & 0xFFF, 0);
    }

    #[test_case]
    fn dma_buffer_drop_reclaim_multipage() {
        let before = frame_allocator::available_count();
        {
            // 3 pages → buddy order 2 (4 pages).
            let buf = DmaBuffer::<[u8]>::new_bytes(3 * 4096, 4096).expect("multi-page DMA alloc");
            assert!(buf.capacity_bytes() >= 3 * 4096);
            // Touch every byte to make sure the mapping is live and writable.
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
    fn dma_buffer_rejects_zero_size_and_bad_alignment() {
        assert_eq!(
            DmaBuffer::<[u8]>::new_bytes(0, 4096).err(),
            Some(DmaError::ZeroSize)
        );
        assert_eq!(
            DmaBuffer::<[u8]>::new_bytes(4096, 3).err(),
            Some(DmaError::UnsupportedAlignment)
        );
        assert_eq!(
            DmaBuffer::<[u8]>::new_bytes(4096, 8192).err(),
            Some(DmaError::UnsupportedAlignment)
        );
    }
}
