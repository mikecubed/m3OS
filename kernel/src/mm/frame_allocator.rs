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
//!
//! # Minimal allocation-context contract (F.1)
//!
//! Phase 53a documents a two-tier contract even though full GFP-style flags are
//! deferred:
//!
//! * [`AllocationContext::IrqSensitive`] — hard IRQ, IPI, or page-fault-adjacent
//!   callers. Per-CPU page-cache mutations run inside `without_interrupts` and a
//!   same-core non-reentrancy guard. If the guard is already held, the call
//!   bypasses the local cache and uses the cold path instead of corrupting
//!   CPU-local state.
//! * [`AllocationContext::Sleepable`] — task/syscall context. Callers may
//!   tolerate cold-path lock spinning, perform higher-level reclaim, or retry
//!   after `None`.
//!
//! The frame allocator still uses spin locks only, so "sleepable" here means
//! "allowed to take the contended cold path / retry", not that the allocator
//! literally sleeps today.

extern crate alloc;

use alloc::vec::Vec;
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};
use kernel_core::buddy::BuddyAllocator;
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::structures::paging::{PhysFrame, Size4KiB};

const PAGE_SIZE: u64 = 4096;

/// Minimal allocation-context contract for Phase 53a Track F.1.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationContext {
    /// Task or syscall context that may tolerate cold-path contention, reclaim,
    /// or retry when an allocation attempt returns `None`.
    Sleepable,
    /// Hard-IRQ, IPI, or page-fault-adjacent context that must avoid re-entering
    /// CPU-local allocator state while it is already being mutated on this core.
    IrqSensitive,
}

// ---------------------------------------------------------------------------
// Per-CPU page cache (A.1)
// ---------------------------------------------------------------------------

/// Maximum number of cached frames per CPU.
pub const PER_CPU_PAGE_CACHE_CAP: usize = 64;

/// Number of frames to transfer in a single batch refill or drain.
const BATCH_SIZE: usize = 32;

/// When the cache fill level exceeds this, a batch drain is triggered on free.
const HIGH_WATERMARK: usize = 48;

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
    #[expect(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns `true` if the cache is at capacity.
    #[inline]
    #[expect(dead_code)]
    pub fn is_full(&self) -> bool {
        self.count as usize >= PER_CPU_PAGE_CACHE_CAP
    }

    /// Pop one frame from the cache.  Returns `None` if empty.
    #[inline]
    pub fn pop(&mut self) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        self.count -= 1;
        Some(self.frames[self.count as usize])
    }

    /// Push one frame onto the cache.
    ///
    /// # Panics
    ///
    /// Panics if the cache is full.
    #[inline]
    pub fn push(&mut self, phys: u64) {
        debug_assert!(
            (self.count as usize) < PER_CPU_PAGE_CACHE_CAP,
            "PerCpuPageCache::push: cache is full"
        );
        self.frames[self.count as usize] = phys;
        self.count += 1;
    }

    /// Drain up to `n` frames from the cache into a callback.
    ///
    /// Pops from the top of the stack and invokes `f` for each frame.
    /// Returns the number of frames actually drained.
    #[inline]
    pub fn drain_to(&mut self, n: usize, mut f: impl FnMut(u64)) -> usize {
        let to_drain = n.min(self.count as usize);
        for _ in 0..to_drain {
            self.count -= 1;
            f(self.frames[self.count as usize]);
        }
        to_drain
    }
}

/// Same-core non-reentrancy guard for per-CPU page-cache mutations.
static PAGE_CACHE_GUARD: AtomicU64 = AtomicU64::new(0);

struct LocalPageCacheGuard {
    mask: u64,
}

impl LocalPageCacheGuard {
    #[inline]
    fn try_enter() -> Option<Self> {
        let mask = 1u64 << (crate::smp::per_core().core_id as u64);
        let prev = PAGE_CACHE_GUARD.fetch_or(mask, Ordering::AcqRel);
        if prev & mask != 0 {
            return None;
        }
        Some(Self { mask })
    }
}

impl Drop for LocalPageCacheGuard {
    fn drop(&mut self) {
        PAGE_CACHE_GUARD.fetch_and(!self.mask, Ordering::Release);
    }
}

#[inline]
fn with_local_page_cache<T>(f: impl FnOnce(&mut PerCpuPageCache) -> T) -> Result<T, ()> {
    let Some(_guard) = LocalPageCacheGuard::try_enter() else {
        return Err(());
    };
    let per_core = crate::smp::per_core();
    let cache = unsafe { &mut *per_core.page_cache.get() };
    let result = f(cache);
    // Sync atomic shadow counter so remote cores can read it without UB.
    per_core
        .page_cache_count
        .store(cache.len() as usize, Ordering::Release);
    Ok(result)
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum FrameHotPathProbe {
    None = 0,
    AllocateHit = 1,
    FreePush = 2,
}

#[cfg(test)]
static FRAME_HOT_PATH_PROBE: AtomicU8 = AtomicU8::new(FrameHotPathProbe::None as u8);
#[cfg(test)]
static FRAME_HOT_PATH_PROBE_PHYS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn arm_frame_hot_path_probe(probe: FrameHotPathProbe, phys: u64) {
    FRAME_HOT_PATH_PROBE_PHYS.store(phys, Ordering::Release);
    FRAME_HOT_PATH_PROBE.store(probe as u8, Ordering::Release);
}

#[cfg(test)]
fn run_frame_hot_path_probe(expected: FrameHotPathProbe) {
    if FRAME_HOT_PATH_PROBE
        .compare_exchange(
            expected as u8,
            FrameHotPathProbe::None as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        let phys = FRAME_HOT_PATH_PROBE_PHYS.swap(0, Ordering::AcqRel);
        if phys != 0 {
            free_frame(phys);
        }
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

/// Acquire the global frame allocator lock with interrupts masked.
///
/// Masking interrupts prevents the IPI cache-drain handler from deadlocking
/// against a lock holder on the same core.  All post-init lock acquisitions
/// outside an existing `without_interrupts` scope must go through this helper.
#[inline]
fn with_frame_alloc_irq_safe<T>(f: impl FnOnce(&mut FrameAllocator) -> T) -> T {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut alloc = FRAME_ALLOCATOR.0.lock();
        f(&mut alloc)
    })
}

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

/// Allocate a single 4 KiB frame.
///
/// # Minimal allocation-context contract (F.1)
///
/// - [`AllocationContext::IrqSensitive`] callers get a guarded fast path:
///   cache-hit mutations run with interrupts masked and a same-core guard. A
///   re-entrant entry bypasses the page cache and goes straight to the global
///   buddy/free-list path instead of touching CPU-local state twice.
/// - [`AllocationContext::Sleepable`] callers may tolerate a cache miss, higher
///   level reclaim, or retry after `None`.
///
/// # Slow-path behavior
///
/// The refill path batch-allocates from `FRAME_ALLOCATOR`. It does **not**
/// sleep, but it may spin on the global allocator lock and it performs exactly
/// one refill attempt before returning `None`.
pub fn allocate_frame() -> Option<PhysFrame<Size4KiB>> {
    if crate::smp::is_per_core_ready() {
        let result = x86_64::instructions::interrupts::without_interrupts(|| {
            match with_local_page_cache(|cache| {
                if let Some(phys) = cache.pop() {
                    #[cfg(test)]
                    run_frame_hot_path_probe(FrameHotPathProbe::AllocateHit);
                    return Some(phys);
                }
                None
            }) {
                Ok(Some(phys)) => return Some(phys),
                Err(()) => {
                    let frame = FRAME_ALLOCATOR.0.lock().allocate()?;
                    return Some(frame.start_address().as_u64());
                }
                Ok(None) => {}
            }

            let mut refill = [0u64; BATCH_SIZE];
            let mut refill_count = 0usize;
            {
                let mut alloc = FRAME_ALLOCATOR.0.lock();
                for slot in &mut refill {
                    if let Some(frame) = alloc.allocate() {
                        *slot = frame.start_address().as_u64();
                        refill_count += 1;
                    } else {
                        break;
                    }
                }
            }

            if refill_count == 0 {
                return None;
            }

            if refill_count > 1 {
                match with_local_page_cache(|cache| {
                    for phys in &refill[1..refill_count] {
                        cache.push(*phys);
                    }
                }) {
                    Ok(()) => {}
                    Err(()) => {
                        let mut alloc = FRAME_ALLOCATOR.0.lock();
                        for phys in &refill[1..refill_count] {
                            alloc.free_to_pool(*phys);
                        }
                    }
                }
            }

            Some(refill[0])
        });
        if let Some(phys) = result {
            if REFCOUNT_INIT.load(Ordering::Acquire) {
                refcount_inc(phys);
            }
            let addr = PhysAddr::new(phys);
            return Some(PhysFrame::containing_address(addr));
        }
        return None;
    }

    // Pre-SMP / BSP fallback — go straight to the buddy.
    let frame = FRAME_ALLOCATOR.0.lock().allocate()?;
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
///
/// On failure, drains per-CPU page caches (A.4) and retries once, since
/// hoarded single-page frames may be coalesced by the buddy into the
/// requested high-order block. The retry path escalates to the full
/// allocator-local reclaim sequence so slab magazines / depots can also return
/// hidden pages before the allocation gives up.
#[allow(dead_code)]
pub fn allocate_contiguous(order: usize) -> Option<PhysFrame<Size4KiB>> {
    if let Some(frame) = with_frame_alloc_irq_safe(|alloc| alloc.allocate_contiguous(order)) {
        if REFCOUNT_INIT.load(Ordering::Acquire) {
            refcount_inc(frame.start_address().as_u64());
        }
        return Some(frame);
    }

    // Retry after allocator-local reclaim so order-0 hoarding can coalesce.
    super::heap::reclaim_allocator_local_caches("high-order frame allocation");

    let frame = with_frame_alloc_irq_safe(|alloc| alloc.allocate_contiguous(order))?;
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
/// The frame is only reclaimed when the count reaches zero. Frames allocated
/// before refcounting was enabled (refcount == 0) are freed directly.
///
/// # Minimal allocation-context contract (F.1)
///
/// - [`AllocationContext::IrqSensitive`] callers mutate the local page cache
///   with interrupts masked and a same-core guard. A re-entrant free bypasses
///   the cache and returns directly to the buddy/free-list pool.
/// - [`AllocationContext::Sleepable`] callers may tolerate cache-overflow
///   drains that briefly spin on the global allocator lock.
///
/// # Slow-path behavior
///
/// When the cache exceeds the high watermark, the allocator drains a batch of
/// 32 frames back to the buddy. This path does **not** sleep and does not
/// retry; it may briefly spin on `FRAME_ALLOCATOR`.
///
/// Does **not** zero the frame — the zero-before-exposure invariant is
/// enforced on the allocation side via [`allocate_frame_zeroed`]. Panics on
/// double-free (frame already on the free list).
pub fn free_frame(phys: u64) {
    if !release_last_reference(phys) {
        return;
    }

    if crate::smp::is_per_core_ready() {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut drained = [0u64; BATCH_SIZE];
            match with_local_page_cache(|cache| {
                cache.push(phys);
                #[cfg(test)]
                run_frame_hot_path_probe(FrameHotPathProbe::FreePush);

                let mut drained_count = 0usize;
                if cache.len() as usize > HIGH_WATERMARK {
                    cache.drain_to(BATCH_SIZE, |frame_phys| {
                        drained[drained_count] = frame_phys;
                        drained_count += 1;
                    });
                }
                drained_count
            }) {
                Ok(drain_count) => {
                    if drain_count > 0 {
                        let mut alloc = FRAME_ALLOCATOR.0.lock();
                        for frame_phys in &drained[..drain_count] {
                            alloc.free_to_pool(*frame_phys);
                        }
                    }
                }
                Err(()) => {
                    FRAME_ALLOCATOR.0.lock().free_to_pool(phys);
                }
            }
        });
        return;
    }

    // Pre-SMP fallback.
    FRAME_ALLOCATOR.0.lock().free_to_pool(phys);
}

/// Return a frame directly to the buddy/free-list pool, bypassing the per-CPU
/// page cache while still honoring the refcount contract.
///
/// Used by allocator-local reclaim when a hidden slab page has already been
/// selected for immediate surfacing back to the global pool.
pub(crate) fn free_frame_direct(phys: u64) {
    if !release_last_reference(phys) {
        return;
    }
    with_frame_alloc_irq_safe(|alloc| alloc.free_to_pool(phys));
}

/// Free a contiguous block of `1 << order` pages.
///
/// Decrements refcount for the base frame. Frees the entire block when the
/// count reaches zero.  Does **not** zero — see [`allocate_contiguous_zeroed`].
#[allow(dead_code)]
pub fn free_contiguous(phys: u64, order: usize) {
    if !release_last_reference(phys) {
        return;
    }
    with_frame_alloc_irq_safe(|alloc| alloc.free_contiguous(phys, order));
}

// ---------------------------------------------------------------------------
// Per-CPU cache drain (A.4)
// ---------------------------------------------------------------------------

/// Atomic counter used by `drain_per_cpu_caches` to wait for IPI-driven
/// remote drains to complete.  Each remote core decrements this after
/// flushing its local cache.
static DRAIN_PENDING: AtomicU8 = AtomicU8::new(0);
/// Serializes initiators so concurrent memory-pressure drains cannot stomp the
/// shared pending counter or IPI handshake.
static CACHE_DRAIN_LOCK: Mutex<()> = Mutex::new(());
/// Whether the current `IPI_CACHE_DRAIN` round should also service page-cache
/// drains on the remote cores.
static CACHE_DRAIN_ACTIVE: AtomicBool = AtomicBool::new(false);

fn drain_local_page_cache_to_pool() -> usize {
    if !crate::smp::is_per_core_ready() {
        return 0;
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let per_core = crate::smp::per_core();
        let cache = unsafe { &mut *per_core.page_cache.get() };
        let n = cache.len() as usize;
        if n > 0 {
            let mut alloc = FRAME_ALLOCATOR.0.lock();
            cache.drain_to(n, |frame_phys| {
                alloc.free_to_pool(frame_phys);
            });
            per_core.page_cache_count.store(0, Ordering::Release);
        }
        n
    })
}

/// Drain per-CPU page caches across all cores and return frames to the buddy.
///
/// Each CPU's cache is only mutated by its owning core — remote CPUs are
/// notified via a dedicated IPI (`IPI_CACHE_DRAIN`, vector 0xFC) and drain
/// their own cache in the handler.  The calling core drains its own cache
/// directly.
///
/// # Interrupt safety
///
/// The caller must not hold the `FRAME_ALLOCATOR` lock.  The local drain
/// runs with interrupts masked.  Remote cores drain under the IPI handler
/// which also masks interrupts implicitly.
///
/// # Ordering
///
/// 1. Serialize initiators with `CACHE_DRAIN_LOCK`.
/// 2. Drain the local core's cache under `without_interrupts`.
/// 3. Publish the remote-core count in `DRAIN_PENDING`, set
///    `CACHE_DRAIN_ACTIVE = true`, then send `IPI_CACHE_DRAIN`.
/// 4. Each remote core self-drains inside the handler and decrements
///    `DRAIN_PENDING`.
/// 5. Spin-wait until `DRAIN_PENDING` reaches 0, then clear
///    `CACHE_DRAIN_ACTIVE`.
pub fn drain_per_cpu_caches() {
    if !crate::smp::is_per_core_ready() {
        return;
    }

    let _drain_guard = CACHE_DRAIN_LOCK.lock();
    let core_count = crate::smp::core_count();

    // Drain local cache.
    let _ = drain_local_page_cache_to_pool();

    if core_count <= 1 {
        return;
    }

    // Count online remote cores.
    let my_core = crate::smp::per_core().core_id;
    let mut remote_count: u8 = 0;
    for cid in 0..core_count {
        if cid == my_core {
            continue;
        }
        if let Some(data) = crate::smp::get_core_data(cid)
            && data.is_online.load(Ordering::Acquire)
        {
            remote_count += 1;
        }
    }

    if remote_count == 0 {
        return;
    }

    DRAIN_PENDING.store(remote_count, Ordering::Release);
    CACHE_DRAIN_ACTIVE.store(true, Ordering::Release);
    crate::smp::ipi::send_ipi_all_excluding_self(crate::smp::ipi::IPI_CACHE_DRAIN);

    // Spin-wait for all remote cores to complete their drain.
    while DRAIN_PENDING.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }
    CACHE_DRAIN_ACTIVE.store(false, Ordering::Release);

    log::debug!(
        "[mm] drained per-CPU page caches on {} remote core(s)",
        remote_count
    );
}

/// Handle a cache-drain IPI on the receiving core.
///
/// Called from the IDT handler for `IPI_CACHE_DRAIN`.  Drains the local
/// per-CPU page cache into the buddy allocator, then decrements
/// `DRAIN_PENDING` to signal completion to the initiator. The same vector is
/// also reused by slab reclaim, so page-cache work is gated by
/// `CACHE_DRAIN_ACTIVE`.
pub fn handle_cache_drain_ipi() {
    if CACHE_DRAIN_ACTIVE.load(Ordering::Acquire) {
        let _ = drain_local_page_cache_to_pool();
        DRAIN_PENDING.fetch_sub(1, Ordering::AcqRel);
    }
    crate::mm::slab::handle_reclaim_ipi();
}

/// Returns the number of frames available for allocation, including reclaimable
/// per-CPU cached pages.
///
/// This matches the Linux-like `MemAvailable` view. For buddy-managed frames
/// immediately allocatable without draining per-CPU caches, use
/// `frame_stats().free_frames`.
pub fn available_count() -> usize {
    with_frame_alloc_irq_safe(|alloc| alloc.current_free_count()) + per_cpu_cached_total()
}

/// Compatibility accessor for older callers that interpreted "free" as
/// reclaimable/available rather than buddy-only `MemFree`.
#[allow(dead_code)]
pub fn free_count() -> usize {
    available_count()
}

/// Returns the total number of usable frames discovered at boot.
pub fn total_frames() -> usize {
    with_frame_alloc_irq_safe(|alloc| alloc.total_frames)
}

/// Returns the highest physical frame number from the memory map.
///
/// Used by the heap allocator to size the dense page-metadata side table.
pub fn max_frame_number() -> usize {
    with_frame_alloc_irq_safe(|alloc| alloc.max_frame_number as usize)
}

/// Sum of frames currently held in all per-CPU page caches.
///
/// Reads each core's atomic shadow counter rather than the `UnsafeCell`
/// page-cache contents, avoiding a data race on remote non-atomic fields.
fn per_cpu_cached_total() -> usize {
    if !crate::smp::is_per_core_ready() {
        return 0;
    }
    let mut total: usize = 0;
    for cid in 0..crate::smp::core_count() {
        if let Some(data) = crate::smp::get_core_data(cid)
            && data.is_online.load(Ordering::Acquire)
        {
            total += data.page_cache_count.load(Ordering::Acquire);
        }
    }
    total
}

/// Frame allocator statistics snapshot.
///
/// # Accounting policy (Linux-like)
///
/// * **`free_frames`** — buddy-managed frames immediately allocatable without
///   draining any per-CPU cache.  Corresponds to Linux `MemFree`.
/// * **`available_frames`** — `free_frames` plus reclaimable per-CPU cached
///   pages.  Corresponds to Linux `MemAvailable`.
/// * **`allocated_frames`** — `total_frames − available_frames`.  Frames
///   actively backing kernel or user mappings.
/// * **`per_cpu_cached`** — frames held in per-CPU page caches.  Excluded from
///   `free_frames`, included in `available_frames`.
pub struct FrameStats {
    pub total_frames: usize,
    /// Buddy-managed free frames (immediately allocatable, excludes per-CPU caches).
    pub free_frames: usize,
    /// Frames available for allocation (free + reclaimable per-CPU caches).
    pub available_frames: usize,
    /// Frames in use by kernel/userspace (`total − available`).
    pub allocated_frames: usize,
    pub free_by_order: [usize; kernel_core::buddy::MAX_ORDER + 1],
    /// Frames held in per-CPU page caches (reclaimable, not in buddy free lists).
    pub per_cpu_cached: usize,
}

/// Returns a snapshot of frame allocator statistics.
///
/// Uses Linux-like accounting: `free_frames` reflects only buddy-managed pages
/// (immediately allocatable).  Per-CPU cached pages are excluded from free but
/// included in `available_frames` because they are reclaimable.
pub fn frame_stats() -> FrameStats {
    let (total, buddy_free, by_order) = with_frame_alloc_irq_safe(|alloc| {
        let total = alloc.total_frames;
        if let Some(ref buddy) = alloc.buddy {
            (total, buddy.free_count(), buddy.free_count_by_order())
        } else {
            (
                total,
                alloc.free_count,
                [0; kernel_core::buddy::MAX_ORDER + 1],
            )
        }
    });
    let cached = per_cpu_cached_total();
    let available = buddy_free + cached;
    FrameStats {
        total_frames: total,
        free_frames: buddy_free,
        available_frames: available,
        allocated_frames: total.saturating_sub(available),
        free_by_order: by_order,
        per_cpu_cached: cached,
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
fn release_last_reference(phys: u64) -> bool {
    if !REFCOUNT_INIT.load(Ordering::Acquire) {
        return true;
    }
    let current = refcount_get(phys);
    if current == 0 {
        return true;
    }
    refcount_dec(phys) == 0
}

#[inline]
fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_test_per_core() {
        if !crate::smp::is_per_core_ready() {
            crate::smp::init_bsp_per_core();
        }
        drain_per_cpu_caches();
    }

    #[test_case]
    fn allocate_frame_hot_path_tolerates_reentrant_free() {
        ensure_test_per_core();
        let before = available_count();

        let cached_a = allocate_frame().expect("cached_a");
        let cached_b = allocate_frame().expect("cached_b");
        free_frame(cached_a.start_address().as_u64());
        free_frame(cached_b.start_address().as_u64());

        let nested = allocate_frame().expect("nested frame");
        let nested_phys = nested.start_address().as_u64();
        arm_frame_hot_path_probe(FrameHotPathProbe::AllocateHit, nested_phys);

        let outer = allocate_frame().expect("outer frame");
        free_frame(outer.start_address().as_u64());

        assert_eq!(
            available_count(),
            before,
            "reentrant free during allocate_frame cache hit leaked frames"
        );
        drain_per_cpu_caches();
    }

    #[test_case]
    fn free_frame_hot_path_tolerates_reentrant_free() {
        ensure_test_per_core();
        let before = available_count();

        let outer = allocate_frame().expect("outer frame");
        let nested = allocate_frame().expect("nested frame");
        let nested_phys = nested.start_address().as_u64();
        arm_frame_hot_path_probe(FrameHotPathProbe::FreePush, nested_phys);

        free_frame(outer.start_address().as_u64());

        assert_eq!(
            available_count(),
            before,
            "reentrant free during free_frame cache push leaked frames"
        );
        drain_per_cpu_caches();
    }

    #[test_case]
    fn contiguous_alloc_recovers_order0_hoarding() {
        ensure_test_per_core();

        // Pre-allocate the bookkeeping Vec so any slab pages it needs are
        // accounted for before we snapshot frame availability.
        let mut held = alloc::vec::Vec::with_capacity(available_count());
        let before = available_count();

        let hoarded = allocate_contiguous(2).expect("seed contiguous block");
        let base = hoarded.start_address().as_u64();

        while let Some(frame) = allocate_frame() {
            held.push(frame.start_address().as_u64());
        }

        // Return the seed block one page at a time so it sits in the local
        // per-CPU cache as order-0 pages instead of buddy-visible order-2 state.
        for page in 0..(1u64 << 2) {
            free_frame(base + page * PAGE_SIZE);
        }

        let recovered =
            allocate_contiguous(2).expect("allocator-local reclaim should recover hoarded pages");
        assert_eq!(
            recovered.start_address().as_u64(),
            base,
            "high-order retry should recover the hoarded block after draining caches"
        );

        free_contiguous(recovered.start_address().as_u64(), 2);
        for &phys in &held {
            free_frame(phys);
        }
        drain_per_cpu_caches();
        let after = available_count();
        assert_eq!(
            after, before,
            "contiguous reclaim test leaked frames: before={} after={}",
            before, after
        );
        drop(held);
    }

    #[test_case]
    fn irq_safe_lock_masks_interrupts() {
        ensure_test_per_core();
        // Verify the helper runs the closure with interrupts disabled.
        let irq_was_off =
            with_frame_alloc_irq_safe(|_alloc| !x86_64::instructions::interrupts::are_enabled());
        assert!(
            irq_was_off,
            "with_frame_alloc_irq_safe must mask interrupts while holding the lock"
        );
    }

    #[test_case]
    fn page_cache_shadow_count_tracks_ops() {
        ensure_test_per_core();
        let per_core = crate::smp::per_core();

        // After drain, both real count and shadow should be 0.
        drain_per_cpu_caches();
        assert_eq!(
            per_core.page_cache_count.load(Ordering::Acquire),
            0,
            "shadow should be 0 after drain"
        );

        // Allocate + free to push frames into the local cache.
        let a = allocate_frame().expect("a");
        let b = allocate_frame().expect("b");
        free_frame(a.start_address().as_u64());
        free_frame(b.start_address().as_u64());

        let shadow = per_core.page_cache_count.load(Ordering::Acquire);
        assert!(
            shadow >= 2,
            "shadow should reflect cached frames after free, got {}",
            shadow
        );

        // Drain again — shadow must return to 0.
        drain_per_cpu_caches();
        assert_eq!(
            per_core.page_cache_count.load(Ordering::Acquire),
            0,
            "shadow should be 0 after second drain"
        );
    }
}
