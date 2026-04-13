//! Kernel heap allocator — size-class `GlobalAlloc` with bootstrap fallback.
//!
//! ## Boot phases
//!
//! 1. **Bootstrap** (`BootstrapAllocator`): active from
//!    `init_heap()` until `activate_size_class_allocator()`.  Serves buddy
//!    init, refcount-table setup, slab metadata bootstrap, and per-core
//!    bring-up allocations.  Lives at `HEAP_START..HEAP_START+HEAP_MAX_SIZE`.
//!
//! 2. **Size-class** (`SizeClassAllocator`): active after `activate_size_class_allocator()`.
//!    Small allocations (size ≤ 4096 with natural alignment ≤ size-class
//!    guarantee) route through `magazine_alloc`.  Large or high-alignment
//!    allocations get page-backed buddy frames in the physmap region.
//!
//! ## Allocation metadata (dense side table)
//!
//! A page-number-keyed `Vec<PageMeta>` covers the physmap region.  Each entry
//! records either the owning size class for slab-backed pages or the
//! allocation order for a page-backed large allocation so `dealloc` can route
//! back to the correct backend without guessing from `Layout`.  Pages in the
//! bootstrap heap region are identified by address range and handled directly
//! by the bootstrap allocator.

extern crate alloc;

use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering};
use kernel_core::size_class::{NUM_SIZE_CLASSES, SIZE_CLASSES, size_to_class};
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB};

pub const HEAP_START: usize = 0xFFFF_8000_0000_0000;
/// Initial heap size mapped at boot (8 MiB).
pub const HEAP_INITIAL_SIZE: usize = 8 * 1024 * 1024; // 8 MiB
/// Maximum heap size the kernel may grow to (64 MiB).
pub const HEAP_MAX_SIZE: usize = 64 * 1024 * 1024; // 64 MiB

/// Tracks total successful allocations (for Track F diagnostics).
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
/// Tracks total deallocations (for Track F diagnostics).
static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Page-backed allocation metadata (dense side table, C.1)
// ---------------------------------------------------------------------------

const PAGE_META_NONE: u16 = 0;
const PAGE_META_LARGE_TAG: u16 = 0x8000;

/// Per-page metadata for post-cutover allocator ownership in the physmap.
///
/// Encoding (u16):
/// - `0x0000`: untracked / bootstrap / not owned by the cutover allocator
/// - Slab page: `(owning_cpu << 8) | (class_idx + 1)` — bits 0–7 hold the
///   size-class tag (1..=NUM_SIZE_CLASSES), bits 8–11 hold the owning CPU ID
///   (0..15, fits MAX_CORES=16).
/// - Large page: `0x8000 | (order + 1)` — bit 15 set, bits 0–14 hold the
///   buddy order.
///
/// Storing owning CPU out-of-line keeps the full 4096-byte page available
/// for slab objects (E.2 acceptance criteria).
#[repr(transparent)]
struct PageMeta(AtomicU16);

impl PageMeta {
    const fn new() -> Self {
        Self(AtomicU16::new(PAGE_META_NONE))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageMetaKind {
    Slab { class_idx: usize, owning_cpu: u8 },
    Large { order: usize },
}

#[inline]
fn encode_slab_meta(class_idx: usize, owning_cpu: u8) -> u16 {
    debug_assert!(class_idx < NUM_SIZE_CLASSES);
    debug_assert!((owning_cpu as usize) < crate::smp::MAX_CORES);
    ((owning_cpu as u16) << 8) | (class_idx as u16 + 1)
}

#[inline]
fn encode_large_meta(order: usize) -> u16 {
    PAGE_META_LARGE_TAG | ((order as u16) + 1)
}

#[inline]
fn decode_page_meta(raw: u16) -> Option<PageMetaKind> {
    if raw == PAGE_META_NONE {
        return None;
    }
    if raw & PAGE_META_LARGE_TAG != 0 {
        Some(PageMetaKind::Large {
            order: ((raw & !PAGE_META_LARGE_TAG) - 1) as usize,
        })
    } else {
        let class_idx = ((raw & 0xFF) - 1) as usize;
        let owning_cpu = (raw >> 8) as u8;
        Some(PageMetaKind::Slab {
            class_idx,
            owning_cpu,
        })
    }
}

/// Dense side table: one `PageMeta` per physical page frame.
/// Allocated after slab init so it can use `Vec`.
static PAGE_META_TABLE: spin::Once<Vec<PageMeta>> = spin::Once::new();

/// Whether the size-class allocator is active (post-cutover).
static SIZE_CLASS_ACTIVE: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Per-core recursion guard (prevents deadlock from slab-internal Vec growth)
// ---------------------------------------------------------------------------

/// Bitmask tracking which cores are currently inside the slab allocation path.
/// Bit N set means core N is inside a slab alloc/free operation.  Any recursive
/// GlobalAlloc call from that core (e.g. Vec growth inside SlabCache::allocate)
/// falls back to the bootstrap allocator instead of re-entering the slab.
static SLAB_RECURSE: AtomicU64 = AtomicU64::new(0);

#[inline]
fn core_bit() -> u64 {
    if !crate::smp::is_per_core_ready() {
        1 // bit 0 for the sole boot CPU
    } else {
        1u64 << (crate::smp::current_core_id() & 63)
    }
}

/// Returns true if the current core is already inside the slab alloc path.
#[inline]
fn in_slab_recursion() -> bool {
    SLAB_RECURSE.load(Ordering::Acquire) & core_bit() != 0
}

/// Mark the current core as entering the slab alloc path.
#[inline]
fn enter_slab() {
    SLAB_RECURSE.fetch_or(core_bit(), Ordering::Release);
}

/// Mark the current core as leaving the slab alloc path.
#[inline]
fn leave_slab() {
    SLAB_RECURSE.fetch_and(!core_bit(), Ordering::Release);
}

#[inline]
const fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}

// ---------------------------------------------------------------------------
// Bootstrap allocator (C.2)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default)]
struct BootstrapStats {
    mapped_bytes: usize,
    used_bytes: usize,
    free_bytes: usize,
}

#[derive(Debug)]
struct BootstrapState {
    start: usize,
    mapped_end: usize,
    next: usize,
}

impl BootstrapState {
    const fn new() -> Self {
        Self {
            start: 0,
            mapped_end: 0,
            next: 0,
        }
    }

    fn init(&mut self, start: usize, bytes: usize) {
        self.start = start;
        self.mapped_end = start + bytes;
        self.next = start;
    }

    fn extend(&mut self, bytes: usize) {
        self.mapped_end += bytes;
    }

    fn stats(&self) -> BootstrapStats {
        let mapped_bytes = self.mapped_end.saturating_sub(self.start);
        let used_bytes = self.next.saturating_sub(self.start);
        BootstrapStats {
            mapped_bytes,
            used_bytes,
            free_bytes: mapped_bytes.saturating_sub(used_bytes),
        }
    }

    unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let start = align_up(self.next, layout.align());
        let end = match start.checked_add(layout.size().max(1)) {
            Some(end) => end,
            None => return core::ptr::null_mut(),
        };
        if end > self.mapped_end {
            return core::ptr::null_mut();
        }
        self.next = end;
        start as *mut u8
    }
}

/// Monotonic bootstrap allocator for pre-cutover metadata.
///
/// Early allocations are long-lived kernel structures, so deallocation is a
/// deliberate no-op. Bootstrap pointers are still recognized by range so they
/// remain safe to "free" after the cutover without corrupting size-class state.
struct BootstrapAllocator {
    state: Mutex<BootstrapState>,
}

impl BootstrapAllocator {
    const fn new() -> Self {
        Self {
            state: Mutex::new(BootstrapState::new()),
        }
    }

    fn init(&self, start: usize, bytes: usize) {
        self.state.lock().init(start, bytes);
    }

    fn extend(&self, bytes: usize) {
        self.state.lock().extend(bytes);
    }

    fn stats(&self) -> BootstrapStats {
        self.state.lock().stats()
    }

    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { self.state.lock().alloc(layout) }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

// ---------------------------------------------------------------------------
// Size-class GlobalAlloc (C.1)
// ---------------------------------------------------------------------------

/// Kernel allocator that dispatches to slab caches (small) or page-backed
/// buddy allocations (large), with a bootstrap fallback before cutover.
struct SizeClassAllocator {
    bootstrap: BootstrapAllocator,
}

unsafe impl Sync for SizeClassAllocator {}

/// Returns true if `layout` can be satisfied by the slab path.
///
/// Criteria: size fits a size class AND the requested alignment is at most
/// the size-class bucket size (slab objects are naturally aligned to their
/// bucket size, which is always a power-of-two-aligned for power-of-two
/// classes and at least 32-byte aligned for all classes).
#[inline]
fn is_slab_eligible(layout: &Layout) -> bool {
    let size = layout.size();
    let align = layout.align();
    if size == 0 || size > SIZE_CLASSES[NUM_SIZE_CLASSES - 1] {
        return false;
    }
    // Every size class ≥ 32 and the class size is always ≥ size, so the
    // minimum natural alignment from the slab is 32.  For classes that are
    // exact powers of two (32, 64, 128, 256, 512, 1024, 2048, 4096) the
    // alignment guarantee equals the class size.  For non-power-of-two
    // classes (48, 96, 192, 384, 768) the alignment guarantee is the
    // largest power of two that divides the class size (e.g. 48 → 16,
    // 96 → 32, 192 → 64, 384 → 128, 768 → 256).
    //
    // Conservative: we guarantee alignment up to the largest power-of-two
    // factor of the class size.  For the common case (align ≤ 16 or align
    // = size) this is always satisfied.
    if let Some(idx) = size_to_class(size) {
        let class_size = SIZE_CLASSES[idx];
        // Alignment guarantee: largest power-of-two divisor of class_size.
        let class_align = class_size & class_size.wrapping_neg();
        align <= class_align
    } else {
        false
    }
}

/// Number of 4 KiB pages required for a layout, plus the buddy order.
#[inline]
fn pages_for_layout(layout: &Layout) -> (usize, usize) {
    let size = layout.size().max(layout.align());
    let pages = size.div_ceil(4096);
    // Round up to next power of two for buddy order.
    let order = if pages <= 1 {
        0
    } else {
        (usize::BITS - (pages - 1).leading_zeros()) as usize
    };
    (1 << order, order)
}

#[inline]
fn page_meta_entry(pfn: usize) -> Option<&'static PageMeta> {
    PAGE_META_TABLE.get().and_then(|table| table.get(pfn))
}

#[inline]
fn phys_from_direct_map_ptr(ptr: *mut u8) -> Option<u64> {
    let addr = ptr as u64;
    let phys_off = super::phys_offset();
    (addr >= phys_off).then_some(addr - phys_off)
}

#[inline]
fn page_meta_for_ptr(ptr: *mut u8) -> Option<PageMetaKind> {
    let phys = phys_from_direct_map_ptr(ptr)?;
    let pfn = (phys / 4096) as usize;
    let raw = page_meta_entry(pfn)?.0.load(Ordering::Acquire);
    decode_page_meta(raw)
}

pub(crate) fn register_slab_page(phys: u64, class_idx: usize, owning_cpu: u8) {
    let pfn = (phys / 4096) as usize;
    if let Some(entry) = page_meta_entry(pfn) {
        entry
            .0
            .compare_exchange(
                PAGE_META_NONE,
                encode_slab_meta(class_idx, owning_cpu),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("heap: slab page metadata already populated");
    }
}

fn register_large_page(phys: u64, order: usize) {
    let pfn = (phys / 4096) as usize;
    if let Some(entry) = page_meta_entry(pfn) {
        entry
            .0
            .compare_exchange(
                PAGE_META_NONE,
                encode_large_meta(order),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("heap: large allocation metadata already populated");
    }
}

/// Allocate via the page-backed large-allocation path.
///
/// Returns a pointer into the physmap region backed by `2^order` contiguous
/// buddy frames.
fn large_alloc(layout: &Layout) -> *mut u8 {
    let (_page_count, order) = pages_for_layout(layout);
    let frame = if order == 0 {
        super::frame_allocator::allocate_frame_zeroed()
    } else {
        super::frame_allocator::allocate_contiguous_zeroed(order)
    };
    let frame = match frame {
        Some(f) => f,
        None => {
            // Drain per-CPU caches and retry once.
            super::frame_allocator::drain_per_cpu_caches();
            let retry = if order == 0 {
                super::frame_allocator::allocate_frame_zeroed()
            } else {
                super::frame_allocator::allocate_contiguous_zeroed(order)
            };
            match retry {
                Some(f) => f,
                None => return core::ptr::null_mut(),
            }
        }
    };

    let phys = frame.start_address().as_u64();
    let virt = (super::phys_offset() + phys) as usize;
    register_large_page(phys, order);

    // For high-alignment requests, the buddy allocation is already
    // page-aligned (4096).  If alignment > 4096, we rely on the fact
    // that buddy allocations of order N are naturally 2^N-page-aligned.
    // Verify the alignment contract is met.
    debug_assert!(
        virt.is_multiple_of(layout.align()),
        "large_alloc: alignment {:#x} not met at {:#x}",
        layout.align(),
        virt
    );

    virt as *mut u8
}

/// Free a page-backed large allocation.
fn large_dealloc(ptr: *mut u8) {
    let phys = match phys_from_direct_map_ptr(ptr) {
        Some(phys) => phys,
        None => {
            log::error!(
                "[heap] large_dealloc: ptr {:#x} below physmap",
                ptr as usize
            );
            return;
        }
    };
    let pfn = (phys / 4096) as usize;
    let raw = match page_meta_entry(pfn) {
        Some(entry) => entry.0.swap(PAGE_META_NONE, Ordering::AcqRel),
        None => PAGE_META_NONE,
    };
    let order = match decode_page_meta(raw) {
        Some(PageMetaKind::Large { order }) => order,
        Some(PageMetaKind::Slab { class_idx, .. }) => {
            log::error!(
                "[heap] large_dealloc: slab metadata for class {} found at ptr {:#x}",
                class_idx,
                ptr as usize
            );
            return;
        }
        None => {
            log::error!(
                "[heap] large_dealloc: no metadata for pfn {} (ptr {:#x})",
                pfn,
                ptr as usize
            );
            return;
        }
    };

    if order == 0 {
        super::frame_allocator::free_frame(phys);
    } else {
        super::frame_allocator::free_contiguous(phys, order);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllocationBackend {
    Bootstrap,
    Slab { class_idx: usize, owning_cpu: u8 },
    Large,
}

fn classify_allocation(ptr: *mut u8) -> Option<AllocationBackend> {
    let addr = ptr as usize;
    if (HEAP_START..HEAP_START + HEAP_MAX_SIZE).contains(&addr) {
        return Some(AllocationBackend::Bootstrap);
    }
    match page_meta_for_ptr(ptr)? {
        PageMetaKind::Slab {
            class_idx,
            owning_cpu,
        } => Some(AllocationBackend::Slab {
            class_idx,
            owning_cpu,
        }),
        PageMetaKind::Large { .. } => Some(AllocationBackend::Large),
    }
}

unsafe impl GlobalAlloc for SizeClassAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() == 0 {
            return layout.align() as *mut u8;
        }

        // --- Bootstrap path (C.2) ---
        // Also used as recursion fallback: if we're inside a slab operation
        // on this core (e.g. SlabCache::allocate triggering Vec growth),
        // route to the bootstrap allocator to avoid deadlock.
        if !SIZE_CLASS_ACTIVE.load(Ordering::Acquire) || in_slab_recursion() {
            let ptr = unsafe { self.bootstrap.alloc(layout) };
            if !ptr.is_null() {
                ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
                return ptr;
            }
            if try_grow_on_oom_for_layout(layout) {
                let ptr = unsafe { self.bootstrap.alloc(layout) };
                if !ptr.is_null() {
                    ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
                }
                return ptr;
            }
            return core::ptr::null_mut();
        }

        // --- Size-class path (C.1) ---
        enter_slab();
        let ptr = if is_slab_eligible(&layout) {
            let idx = size_to_class(layout.size()).unwrap();
            super::slab::magazine_alloc(idx).unwrap_or_default()
        } else {
            large_alloc(&layout)
        };
        leave_slab();

        if !ptr.is_null() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.size() == 0 {
            return;
        }

        match classify_allocation(ptr) {
            Some(AllocationBackend::Bootstrap) => unsafe {
                self.bootstrap.dealloc(ptr, layout);
            },
            Some(AllocationBackend::Slab {
                class_idx,
                owning_cpu,
            }) => {
                enter_slab();
                super::slab::magazine_free(class_idx, ptr, owning_cpu);
                leave_slab();
            }
            Some(AllocationBackend::Large) => {
                large_dealloc(ptr);
            }
            None => {
                log::error!(
                    "[heap] dealloc: no allocation metadata for ptr {:#x} size={} align={}",
                    ptr as usize,
                    layout.size(),
                    layout.align()
                );
                return;
            }
        }
        DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: SizeClassAllocator = SizeClassAllocator {
    bootstrap: BootstrapAllocator::new(),
};

static HEAP_INIT: AtomicBool = AtomicBool::new(false);

/// Tracks the current number of bytes mapped into the bootstrap heap region.
static HEAP_MAPPED: AtomicUsize = AtomicUsize::new(0);

/// Serializes bootstrap heap growth.
///
/// The bootstrap allocator can still be used post-cutover as the recursion
/// fallback when slab-internal metadata needs temporary space. Serializing
/// `grow_heap` avoids leaving unmapped holes in the bootstrap heap range if two
/// cores hit the fallback concurrently and one growth attempt only partially
/// succeeds.
static GROW_HEAP_LOCK: Mutex<()> = Mutex::new(());

/// Map the kernel heap region and initialise the bootstrap allocator.
///
/// This sets up the monotonic bootstrap allocator at `HEAP_START` which serves
/// all allocations until `activate_size_class_allocator()` is called after
/// slab init.
///
/// Panics if called more than once.
pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) {
    assert!(
        !HEAP_INIT.swap(true, Ordering::AcqRel),
        "heap::init_heap called more than once"
    );

    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_INITIAL_SIZE as u64 - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .expect("heap: out of physical frames");
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe {
            mapper
                .map_to(page, frame, flags, frame_allocator)
                .expect("heap: page mapping failed")
                .flush();
        }
    }

    ALLOCATOR.bootstrap.init(HEAP_START, HEAP_INITIAL_SIZE);

    HEAP_MAPPED.store(HEAP_INITIAL_SIZE, Ordering::Release);

    log::info!(
        "[mm] bootstrap heap initialized at {:#x}, size={}KiB, max={}MiB",
        HEAP_START,
        HEAP_INITIAL_SIZE / 1024,
        HEAP_MAX_SIZE / (1024 * 1024),
    );
}

/// Activate the size-class allocator after slab caches are initialized.
///
/// This is the explicit cutover point (C.2): all subsequent eligible small
/// allocations route through `magazine_alloc`, and large allocations use
/// page-backed buddy frames.  Allocations made before this point (bootstrap)
/// are tracked by address range and continue to be handled by the bootstrap
/// allocator's no-op free path.
///
/// Must be called exactly once, after `slab::init()`.
pub fn activate_size_class_allocator() {
    assert!(
        !SIZE_CLASS_ACTIVE.load(Ordering::Acquire),
        "heap::activate_size_class_allocator called more than once"
    );
    let table_size = super::frame_allocator::max_frame_number() + 1;

    PAGE_META_TABLE.call_once(|| {
        let mut table = Vec::with_capacity(table_size);
        for _ in 0..table_size {
            table.push(PageMeta::new());
        }
        table
    });

    SIZE_CLASS_ACTIVE.store(true, Ordering::Release);

    log::info!(
        "[mm] size-class allocator active (side table: {} entries, {} KiB)",
        table_size,
        table_size / 1024,
    );
}

/// Grow the bootstrap heap by up to `additional_bytes`, mapping new pages and
/// extending the bootstrap allocator.
///
/// Only used during the bootstrap phase and as a fallback for bootstrap-region
/// deallocations.  After size-class activation, new allocations go through
/// slab or page-backed paths instead of growing this region.
pub fn grow_heap(additional_bytes: usize) -> Result<(), ()> {
    use super::paging::{GlobalFrameAlloc, get_mapper};
    let _growth_guard = GROW_HEAP_LOCK.lock();

    // Round up to page boundary.
    let page_size: usize = 4096;
    let additional_bytes = (additional_bytes + page_size - 1) & !(page_size - 1);

    let current_mapped = HEAP_MAPPED.load(Ordering::Acquire);
    let new_mapped = current_mapped.checked_add(additional_bytes).ok_or(())?;
    if new_mapped > HEAP_MAX_SIZE {
        log::error!(
            "[mm] heap growth refused: requested total {}KiB exceeds max {}MiB",
            new_mapped / 1024,
            HEAP_MAX_SIZE / (1024 * 1024),
        );
        return Err(());
    }
    let new_mapped = current_mapped + additional_bytes;

    let mut mapper = unsafe { get_mapper() };
    let mut frame_alloc = GlobalFrameAlloc;

    let start_addr = VirtAddr::new((HEAP_START + current_mapped) as u64);
    let end_addr = VirtAddr::new((HEAP_START + new_mapped - 1) as u64);
    let start_page: Page<Size4KiB> = Page::containing_address(start_addr);
    let end_page: Page<Size4KiB> = Page::containing_address(end_addr);

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

    let mut pages_mapped: usize = 0;
    for page in Page::range_inclusive(start_page, end_page) {
        let frame = match super::frame_allocator::allocate_frame() {
            Some(f) => f,
            None => {
                log::error!(
                    "[mm] heap growth failed: frame allocation failed after {} pages",
                    pages_mapped,
                );
                break;
            }
        };

        let map_result = unsafe { mapper.map_to(page, frame, flags, &mut frame_alloc) };
        match map_result {
            Ok(flush) => flush.flush(),
            Err(e) => {
                log::error!("[mm] heap growth failed: map_to error: {:?}", e);
                super::frame_allocator::free_frame(frame.start_address().as_u64());
                break;
            }
        }
        pages_mapped += 1;
    }

    if pages_mapped == 0 {
        return Err(());
    }

    let bytes_mapped = pages_mapped * 4096;
    let mapped_end = current_mapped + bytes_mapped;
    ALLOCATOR.bootstrap.extend(bytes_mapped);
    HEAP_MAPPED.store(mapped_end, Ordering::Release);

    log::info!(
        "[mm] bootstrap heap grown by {}KiB → total {}KiB",
        bytes_mapped / 1024,
        mapped_end / 1024,
    );

    Ok(())
}

/// Attempt to grow the bootstrap heap enough to satisfy `layout`.
fn try_grow_on_oom_for_layout(layout: Layout) -> bool {
    let requested = (layout.size() + 4095) & !4095;
    let requested = requested.max(4096);

    for &size in &[requested, 1 << 20, 2 << 20, 4 << 20] {
        if grow_heap(size).is_ok() {
            return true;
        }
    }

    super::frame_allocator::drain_per_cpu_caches();
    grow_heap(requested).is_ok()
}

/// Attempt to grow the heap on OOM (simple 1 MiB attempt).
#[expect(dead_code)]
pub fn try_grow_on_oom() -> bool {
    grow_heap(1024 * 1024).is_ok()
}

/// Returns whether the size-class allocator is active (post-cutover).
#[inline]
#[expect(dead_code)]
pub fn is_size_class_active() -> bool {
    SIZE_CLASS_ACTIVE.load(Ordering::Acquire)
}

// ---------------------------------------------------------------------------
// Statistics (C.1 / F.3)
// ---------------------------------------------------------------------------

/// Kernel heap statistics snapshot.
pub struct HeapStats {
    /// Bootstrap heap: total bytes mapped at HEAP_START.
    pub total_size: usize,
    /// Bootstrap heap: bytes in use.
    pub used_bytes: usize,
    /// Bootstrap heap: bytes free.
    pub free_bytes: usize,
    /// Total successful allocations (all paths).
    pub alloc_count: u64,
    /// Total deallocations (all paths).
    pub dealloc_count: u64,
    /// Whether the size-class allocator is active.
    pub size_class_active: bool,
    /// Number of pages currently owned by size-class slab caches.
    pub slab_pages: usize,
    /// Number of pages currently owned by page-backed large allocations.
    pub page_backed_pages: usize,
}

/// Returns a snapshot of kernel heap statistics.
pub fn heap_stats() -> HeapStats {
    let bootstrap = ALLOCATOR.bootstrap.stats();
    let (slab_pages, page_backed_pages) = if let Some(table) = PAGE_META_TABLE.get() {
        let mut slab_pages = 0usize;
        let mut page_backed_pages = 0usize;
        for entry in table {
            match decode_page_meta(entry.0.load(Ordering::Acquire)) {
                Some(PageMetaKind::Slab { .. }) => slab_pages += 1,
                Some(PageMetaKind::Large { order }) => page_backed_pages += 1usize << order,
                None => {}
            }
        }
        (slab_pages, page_backed_pages)
    } else {
        (0, 0)
    };
    HeapStats {
        total_size: bootstrap.mapped_bytes,
        used_bytes: bootstrap.used_bytes,
        free_bytes: bootstrap.free_bytes,
        alloc_count: ALLOC_COUNT.load(Ordering::Relaxed),
        dealloc_count: DEALLOC_COUNT.load(Ordering::Relaxed),
        size_class_active: SIZE_CLASS_ACTIVE.load(Ordering::Relaxed),
        slab_pages,
        page_backed_pages,
    }
}
