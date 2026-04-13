//! Physical frame allocator.
//!
//! # Zero-before-exposure invariant (D.4)
//!
//! The kernel guarantees that **no frame reaches user-visible address space
//! containing stale data from a prior allocation**.  Enforcement strategy:
//!
//! * [`allocate_frame_zeroed`] / [`allocate_contiguous_zeroed`] — allocate and
//!   zero in one step.  **Standard path for any frame that will become
//!   user-visible** (data pages, stack pages, demand-paged pages, brk growth,
//!   mmap pages, ELF segment pages).  All audited user-facing callsites in
//!   `demand_map_user_page_locked`, `map_user_stack`, `map_load_segment`,
//!   `sys_linux_brk`, `sys_mmap_file_backed`, and `map_user_pages` use this
//!   path.
//!
//! * [`allocate_frame`] / [`allocate_contiguous`] — raw allocation, **no
//!   zeroing**.  Acceptable when the caller **fully overwrites** the frame
//!   before user exposure (e.g., `copy_nonoverlapping` in `resolve_cow_fault`)
//!   or when the frame is kernel-internal (heap backing, DMA buffers,
//!   page-table frames that the caller zeroes explicitly).
//!
//! `free_frame` and `free_contiguous` do **not** zero on free.  The stale
//! content is harmless because every user-facing path goes through a zeroed
//! allocation or an explicit full-page copy (CoW).

extern crate alloc;

use alloc::vec::Vec;
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use kernel_core::buddy::BuddyAllocator;
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::structures::paging::{PhysFrame, Size4KiB};

const PAGE_SIZE: u64 = 4096;

// ---------------------------------------------------------------------------
// Per-CPU page cache (A.1)
// ---------------------------------------------------------------------------

/// Maximum number of cached frames per CPU.
pub const PER_CPU_PAGE_CACHE_CAP: usize = 64;

/// Per-CPU cache of recently freed physical frames.
///
/// Each core maintains a small stack of physical page addresses so that the
/// hot-path `allocate_frame` / `free_frame` can operate without acquiring
/// the global buddy-allocator lock.  The cache is only ever accessed by its
/// owning core (with interrupts masked or from a non-reentrant context), so
/// no internal locking is required.
///
/// Aligned to 64 bytes (one cache line) to prevent false sharing when
/// per-core data structs are adjacent in memory.
#[repr(C, align(64))]
pub struct PerCpuPageCache {
    /// Stack of cached physical addresses.  Only entries `[0..count)` are valid.
    frames: [u64; PER_CPU_PAGE_CACHE_CAP],
    /// Number of valid entries in `frames`.
    count: u32,
}

#[allow(dead_code)]
impl PerCpuPageCache {
    /// Create an empty cache.
    pub const fn new() -> Self {
        Self {
            frames: [0; PER_CPU_PAGE_CACHE_CAP],
            count: 0,
        }
    }

    /// Number of frames currently cached.
    #[inline]
    pub fn len(&self) -> u32 {
        self.count
    }

    /// Returns `true` if the cache contains no frames.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns `true` if the cache is at capacity.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.count as usize >= PER_CPU_PAGE_CACHE_CAP
    }
}

/// Frames below 1 MiB are skipped even when the region is marked Usable.
/// Some UEFI/QEMU memory maps mark conventional low memory as Usable, but
/// those frames may hold BIOS data area remnants or be used by UEFI firmware
/// code paths that run before ExitBootServices completes.
pub(crate) const ALLOC_MIN_ADDR: u64 = 0x0010_0000; // 1 MiB

/// Magic value written to bytes 8..16 of each free frame for double-free detection.
const FREE_MAGIC: u64 = 0xDEAD_F4EE_F4EE_DEAD;

/// Frame allocator with two phases:
///
/// **Phase 1 (before heap):** A simple intrusive free-list allocator.  Each free
/// 4 KiB frame stores a next-pointer and magic sentinel in its first 16 bytes.
///
/// **Phase 2 (after heap):** A buddy allocator (`kernel_core::buddy::BuddyAllocator`)
/// that supports O(log n) allocation and free (bounded by MAX_ORDER + 1 levels),
/// with buddy-merging on free and multi-page contiguous
/// allocations.  The free-list frames are drained into the buddy allocator during
/// `init_buddy()`.
struct FrameAllocator {
    /// Physical address of the first free frame, or 0 if the list is empty.
    head: u64,
    /// Number of frames currently free (free-list or buddy).
    free_count: usize,
    /// Total number of usable frames discovered at init (>= 1 MiB).
    total_frames: usize,
    /// Virtual base of the physical-memory offset mapping.
    phys_offset: u64,
    /// Highest physical frame number from the memory map.
    max_frame_number: u64,
    /// Buddy allocator, initialized after heap is available.
    buddy: Option<BuddyAllocator>,
}

impl FrameAllocator {
    const fn new() -> Self {
        Self {
            head: 0,
            free_count: 0,
            total_frames: 0,
            phys_offset: 0,
            max_frame_number: 0,
            buddy: None,
        }
    }

    /// Build the free list from bootloader memory regions.
    ///
    /// Pushes every usable frame (>= 1 MiB) onto the intrusive linked list.
    fn init(&mut self, regions: &'static [MemoryRegion], phys_offset: u64) {
        self.phys_offset = phys_offset;
        self.head = 0;
        self.free_count = 0;
        self.total_frames = 0;

        // Determine highest physical frame number from usable regions.
        let max_phys_addr = regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| r.end)
            .max()
            .unwrap_or(0);
        self.max_frame_number = if max_phys_addr > 0 {
            (max_phys_addr - 1) / PAGE_SIZE
        } else {
            0
        };

        for region in regions {
            if region.kind != MemoryRegionKind::Usable {
                continue;
            }

            let start = align_up(region.start.max(ALLOC_MIN_ADDR), PAGE_SIZE);
            let end = align_down(region.end, PAGE_SIZE);
            if end <= start {
                continue;
            }

            let mut addr = start;
            while addr + PAGE_SIZE <= end {
                self.push_frame(addr);
                self.total_frames += 1;
                addr += PAGE_SIZE;
            }
        }

        log::info!(
            "[mm] frame allocator: {} usable 4KiB frames on free list (>= 1 MiB), max frame #{}",
            self.total_frames,
            self.max_frame_number
        );
    }

    /// Push a frame onto the head of the free list.
    fn push_frame(&mut self, phys: u64) {
        let virt = (self.phys_offset + phys) as *mut u64;
        // SAFETY: `phys` is a valid, page-aligned physical address within a
        // Usable memory region.  The physical-memory offset mapping guarantees
        // `virt` is a valid kernel virtual address.
        unsafe {
            virt.write(self.head);
            virt.add(1).write(FREE_MAGIC);
        }
        self.head = phys;
        self.free_count += 1;
    }

    /// Pop a frame from the head of the free list.
    fn pop_frame(&mut self) -> Option<u64> {
        if self.head == 0 {
            return None;
        }

        let phys = self.head;
        let virt = (self.phys_offset + phys) as *mut u64;
        // SAFETY: `phys` is a frame on our free list; the physical-memory offset
        // mapping makes `virt` a valid kernel virtual address.
        unsafe {
            self.head = virt.read();
            virt.add(1).write(0);
        }
        self.free_count -= 1;
        Some(phys)
    }

    /// Allocate a single 4 KiB frame.
    fn allocate(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let phys = if let Some(ref mut buddy) = self.buddy {
            let pfn = buddy.allocate(0)?;
            (pfn as u64) * PAGE_SIZE
        } else {
            self.pop_frame()?
        };
        let addr = PhysAddr::new(phys);
        Some(PhysFrame::containing_address(addr))
    }

    /// Allocate a contiguous block of `1 << order` pages.
    ///
    /// Returns the base physical frame, or `None` if no block is available.
    /// Only works after buddy allocator is initialized.
    #[allow(dead_code)]
    fn allocate_contiguous(&mut self, order: usize) -> Option<PhysFrame<Size4KiB>> {
        let buddy = self.buddy.as_mut()?;
        let pfn = buddy.allocate(order)?;
        let phys = (pfn as u64) * PAGE_SIZE;
        Some(PhysFrame::containing_address(PhysAddr::new(phys)))
    }

    /// Return a frame to the allocator.
    ///
    /// Panics on double-free when using the free-list path.
    fn free_to_pool(&mut self, phys: u64) {
        debug_assert!(
            phys >= ALLOC_MIN_ADDR,
            "free_frame: address {:#x} is below ALLOC_MIN_ADDR",
            phys
        );
        debug_assert!(
            phys.is_multiple_of(PAGE_SIZE),
            "free_frame: address {:#x} is not page-aligned",
            phys
        );

        if let Some(ref mut buddy) = self.buddy {
            let pfn = (phys / PAGE_SIZE) as usize;
            buddy.free(pfn, 0);
        } else {
            // Free-list path (before buddy init).
            // Double-free detection via magic sentinel.
            let virt = (self.phys_offset + phys) as *const u64;
            let magic = unsafe { virt.add(1).read() };
            if magic == FREE_MAGIC {
                panic!(
                    "double-free detected: frame {:#x} is already on the free list",
                    phys
                );
            }

            self.push_frame(phys);
        }
    }

    /// Free a contiguous block of `1 << order` pages.
    ///
    /// Only works after buddy allocator is initialized.
    #[allow(dead_code)]
    fn free_contiguous(&mut self, phys: u64, order: usize) {
        if let Some(ref mut buddy) = self.buddy {
            let pfn = (phys / PAGE_SIZE) as usize;
            buddy.free(pfn, order);
        } else {
            // Before buddy init, free each page individually.
            let count = 1u64 << order;
            for i in 0..count {
                self.free_to_pool(phys + i * PAGE_SIZE);
            }
        }
    }

    /// Current free page count.
    fn current_free_count(&self) -> usize {
        if let Some(ref buddy) = self.buddy {
            buddy.free_count()
        } else {
            self.free_count
        }
    }
}

struct LockedFrameAllocator(Mutex<FrameAllocator>);

static FRAME_ALLOCATOR: LockedFrameAllocator =
    LockedFrameAllocator(Mutex::new(FrameAllocator::new()));

static FRAME_ALLOC_INIT: AtomicBool = AtomicBool::new(false);

/// Cached physical-memory offset — constant after `init()`, read without the
/// frame allocator lock to avoid double-lock in `free_frame` / `free_contiguous`.
static PHYS_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Read the cached physical-memory offset (set once during `init`).
#[inline]
fn cached_phys_offset() -> u64 {
    PHYS_OFFSET.load(Ordering::Acquire)
}

pub fn init(regions: &'static [MemoryRegion], phys_offset: u64) {
    assert!(
        !FRAME_ALLOC_INIT.swap(true, Ordering::AcqRel),
        "frame_allocator::init called more than once"
    );
    PHYS_OFFSET.store(phys_offset, Ordering::Release);
    FRAME_ALLOCATOR.0.lock().init(regions, phys_offset);
}

/// Upgrade from the bootstrap free-list to the buddy allocator.
///
/// Must be called **after** `heap::init_heap` (since `BuddyAllocator` uses `Vec`).
/// Drains all frames from the free list into the buddy allocator, which
/// coalesces them into the largest possible blocks.
pub fn init_buddy() {
    let mut alloc = FRAME_ALLOCATOR.0.lock();

    // Drain all frames from the free list.
    let mut frames = Vec::new();
    while let Some(phys) = alloc.pop_frame() {
        frames.push(phys);
    }

    let total_pfns = (alloc.max_frame_number as usize) + 1;
    let mut buddy = BuddyAllocator::new(total_pfns);

    // Sort frames so contiguous runs are added in order for better coalescing.
    frames.sort_unstable();

    // Feed each frame into the buddy allocator (it will coalesce buddies).
    for phys in &frames {
        let pfn = (*phys / PAGE_SIZE) as usize;
        buddy.free(pfn, 0);
    }

    let buddy_free = buddy.free_count();
    alloc.buddy = Some(buddy);

    log::info!(
        "[mm] buddy allocator: {} free pages across {} orders",
        buddy_free,
        kernel_core::buddy::MAX_ORDER + 1
    );
}

pub fn allocate_frame() -> Option<PhysFrame<Size4KiB>> {
    let frame = FRAME_ALLOCATOR.0.lock().allocate()?;
    // Set refcount to 1 for freshly allocated frames.
    if REFCOUNT_INIT.load(Ordering::Acquire) {
        refcount_inc(frame.start_address().as_u64());
    }
    Some(frame)
}

/// Allocate a single 4 KiB frame and zero its contents.
///
/// This is the **standard** path for any frame that will become visible to
/// user-space (data pages, stack pages, demand-paged pages, brk growth, mmap
/// pages, ELF segment pages).  Callers that fully overwrite the frame before
/// user exposure (CoW `copy_nonoverlapping`) or need a kernel-internal frame
/// (heap, DMA, page tables) may use [`allocate_frame`] instead.
///
/// See module-level docs for the full zero-before-exposure invariant.
pub fn allocate_frame_zeroed() -> Option<PhysFrame<Size4KiB>> {
    let frame = allocate_frame()?;
    let phys_offset = cached_phys_offset();
    let virt_ptr = (phys_offset + frame.start_address().as_u64()) as *mut u8;
    // SAFETY: frame is freshly allocated with exclusive ownership; the
    // physical-memory offset guarantees virt_ptr is a valid kernel address.
    unsafe {
        core::ptr::write_bytes(virt_ptr, 0, PAGE_SIZE as usize);
    }
    Some(frame)
}

/// Allocate a contiguous block of `1 << order` pages.
///
/// Returns the base frame. Sets refcount for the base frame only.
/// Only available after buddy allocator initialization.
#[allow(dead_code)]
pub fn allocate_contiguous(order: usize) -> Option<PhysFrame<Size4KiB>> {
    let frame = FRAME_ALLOCATOR.0.lock().allocate_contiguous(order)?;
    if REFCOUNT_INIT.load(Ordering::Acquire) {
        refcount_inc(frame.start_address().as_u64());
    }
    Some(frame)
}

/// Allocate a contiguous block of `1 << order` pages and zero all of them.
///
/// Zeroed-allocation variant of [`allocate_contiguous`] for user-visible
/// contiguous mappings.
#[allow(dead_code)]
pub fn allocate_contiguous_zeroed(order: usize) -> Option<PhysFrame<Size4KiB>> {
    let frame = allocate_contiguous(order)?;
    let page_count = 1u64 << order;
    let phys_offset = cached_phys_offset();
    let base = frame.start_address().as_u64();
    for i in 0..page_count {
        let virt_ptr = (phys_offset + base + i * PAGE_SIZE) as *mut u8;
        unsafe {
            core::ptr::write_bytes(virt_ptr, 0, PAGE_SIZE as usize);
        }
    }
    Some(frame)
}

/// Return a frame to the allocator.
///
/// If refcounting is initialized, decrements the reference count first.
/// The frame is only pushed onto the free list when the count reaches zero.
/// Frames allocated before refcounting was enabled (refcount == 0) are freed
/// directly without decrementing.
///
/// Does **not** zero the frame — the zero-before-exposure invariant is
/// enforced on the allocation side via [`allocate_frame_zeroed`].
/// Panics on double-free (frame already on the free list).
pub fn free_frame(phys: u64) {
    // Use refcounting when available.
    if REFCOUNT_INIT.load(Ordering::Acquire) {
        let current = refcount_get(phys);
        if current == 0 {
            // Frame was allocated before refcounting was enabled — free directly.
        } else {
            let new_count = refcount_dec(phys);
            if new_count > 0 {
                // Frame is still shared — do not reclaim.
                return;
            }
        }
    }
    FRAME_ALLOCATOR.0.lock().free_to_pool(phys);
}

/// Free a contiguous block of `1 << order` pages.
///
/// Decrements refcount for the base frame. Frees the entire block when the
/// count reaches zero.  Does **not** zero — see [`allocate_contiguous_zeroed`].
#[allow(dead_code)]
pub fn free_contiguous(phys: u64, order: usize) {
    if REFCOUNT_INIT.load(Ordering::Acquire) {
        let current = refcount_get(phys);
        if current == 0 {
            // Pre-refcount frame — free directly.
        } else {
            let new_count = refcount_dec(phys);
            if new_count > 0 {
                return;
            }
        }
    }
    FRAME_ALLOCATOR.0.lock().free_contiguous(phys, order);
}

/// Returns the number of frames currently free.
pub fn free_count() -> usize {
    FRAME_ALLOCATOR.0.lock().current_free_count()
}

/// Returns the total number of usable frames discovered at boot.
pub fn total_frames() -> usize {
    FRAME_ALLOCATOR.0.lock().total_frames
}

/// Frame allocator statistics snapshot.
pub struct FrameStats {
    pub total_frames: usize,
    pub free_frames: usize,
    pub allocated_frames: usize,
    pub free_by_order: [usize; kernel_core::buddy::MAX_ORDER + 1],
}

/// Returns a snapshot of frame allocator statistics.
pub fn frame_stats() -> FrameStats {
    let alloc = FRAME_ALLOCATOR.0.lock();
    let total = alloc.total_frames;
    let (free, by_order) = if let Some(ref buddy) = alloc.buddy {
        (buddy.free_count(), buddy.free_count_by_order())
    } else {
        (alloc.free_count, [0; kernel_core::buddy::MAX_ORDER + 1])
    };
    FrameStats {
        total_frames: total,
        free_frames: free,
        allocated_frames: total.saturating_sub(free),
        free_by_order: by_order,
    }
}

// ---------------------------------------------------------------------------
// Per-frame reference counting (P17-T009 through P17-T015)
// ---------------------------------------------------------------------------

/// True once `init_refcounts()` has completed.
static REFCOUNT_INIT: AtomicBool = AtomicBool::new(false);

/// The refcount table. Each entry corresponds to a physical frame number
/// (phys_addr / 4096). Allocated on the heap after `heap::init_heap`.
static REFCOUNT_TABLE: spin::Once<Vec<AtomicU16>> = spin::Once::new();

/// Initialize the per-frame refcount table.
///
/// Must be called **after** `heap::init_heap` (since it allocates a `Vec`) and
/// **after** `frame_allocator::init` (since it reads `max_frame_number`).
pub fn init_refcounts() {
    let max_frame = FRAME_ALLOCATOR.0.lock().max_frame_number;
    let count = (max_frame + 1) as usize;

    REFCOUNT_TABLE.call_once(|| {
        let mut table = Vec::with_capacity(count);
        for _ in 0..count {
            table.push(AtomicU16::new(0));
        }
        table
    });

    REFCOUNT_INIT.store(true, Ordering::Release);

    log::info!(
        "[mm] refcount table: {} entries for frames 0..={}",
        count,
        max_frame
    );
}

/// Atomically increment the reference count for the frame at `phys`.
///
/// Panics on overflow (count would exceed `u16::MAX`).
pub fn refcount_inc(phys: u64) {
    let idx = (phys / PAGE_SIZE) as usize;
    let table = REFCOUNT_TABLE
        .get()
        .expect("refcount table not initialized");
    assert!(idx < table.len(), "refcount_inc: frame index out of range");
    let prev = table[idx].fetch_add(1, Ordering::Relaxed);
    assert!(
        prev < u16::MAX,
        "refcount_inc: overflow for frame {:#x}",
        phys
    );
}

/// Atomically decrement the reference count for the frame at `phys`.
///
/// Returns the **new** count. Panics on underflow (decrement below 0).
pub fn refcount_dec(phys: u64) -> u16 {
    let idx = (phys / PAGE_SIZE) as usize;
    let table = REFCOUNT_TABLE
        .get()
        .expect("refcount table not initialized");
    assert!(idx < table.len(), "refcount_dec: frame index out of range");
    let prev = table[idx].fetch_sub(1, Ordering::AcqRel);
    assert!(prev > 0, "refcount_dec: underflow for frame {:#x}", phys);
    prev - 1
}

/// Read the current reference count for the frame at `phys`.
#[allow(dead_code)]
pub fn refcount_get(phys: u64) -> u16 {
    let idx = (phys / PAGE_SIZE) as usize;
    let table = REFCOUNT_TABLE
        .get()
        .expect("refcount table not initialized");
    assert!(idx < table.len(), "refcount_get: frame index out of range");
    table[idx].load(Ordering::Acquire)
}

#[inline]
fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}
