use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use linked_list_allocator::LockedHeap;
use x86_64::VirtAddr;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB};

pub const HEAP_START: usize = 0xFFFF_8000_0000_0000;
/// Initial heap size mapped at boot (4 MiB).
pub const HEAP_INITIAL_SIZE: usize = 4 * 1024 * 1024; // 4 MiB
/// Maximum heap size the kernel may grow to (64 MiB).
pub const HEAP_MAX_SIZE: usize = 64 * 1024 * 1024; // 64 MiB

/// Tracks total successful allocations (for Track F diagnostics).
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
/// Tracks total deallocations (for Track F diagnostics).
static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// OOM-retrying allocator wrapper around `LockedHeap`.
///
/// On allocation failure, attempts to grow the kernel heap with escalating
/// sizes before retrying. This avoids the limitations of `alloc_error_handler`
/// which cannot retry the failed allocation.
struct RetryAllocator {
    inner: LockedHeap,
}

unsafe impl Sync for RetryAllocator {}

unsafe impl GlobalAlloc for RetryAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            return ptr;
        }
        // Try growing the heap and retry the allocation.
        if try_grow_on_oom_for_layout(layout) {
            let ptr = unsafe { self.inner.alloc(layout) };
            if !ptr.is_null() {
                ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            }
            ptr
        } else {
            core::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.inner.dealloc(ptr, layout) };
        DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: RetryAllocator = RetryAllocator {
    inner: LockedHeap::empty(),
};

static HEAP_INIT: AtomicBool = AtomicBool::new(false);

/// Tracks the current number of bytes mapped into the heap region.
static HEAP_MAPPED: AtomicUsize = AtomicUsize::new(0);

/// Map the kernel heap region and initialise the global allocator.
///
/// Panics if called more than once — re-initialising `LockedHeap` after
/// allocations have been made corrupts allocator state.
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

    unsafe {
        ALLOCATOR
            .inner
            .lock()
            .init(HEAP_START as *mut u8, HEAP_INITIAL_SIZE);
    }

    HEAP_MAPPED.store(HEAP_INITIAL_SIZE, Ordering::Release);

    log::info!(
        "[mm] heap initialized at {:#x}, size={}KiB, max={}MiB",
        HEAP_START,
        HEAP_INITIAL_SIZE / 1024,
        HEAP_MAX_SIZE / (1024 * 1024),
    );
}

/// Grow the kernel heap by up to `additional_bytes`, mapping new pages and
/// extending the allocator.
///
/// Growth may be partial if frame allocation fails mid-way. Returns `Ok(())`
/// if at least one new page was mapped; returns `Err(())` if growth would
/// exceed `HEAP_MAX_SIZE` or no pages could be mapped at all.
pub fn grow_heap(additional_bytes: usize) -> Result<(), ()> {
    use super::paging::{GlobalFrameAlloc, get_mapper};

    // Round up to page boundary.
    let page_size: usize = 4096;
    let additional_bytes = (additional_bytes + page_size - 1) & !(page_size - 1);

    // Atomically reserve the heap range to prevent SMP races: two CPUs
    // hitting OOM at the same time must not map the same address range.
    let current_mapped = loop {
        let current = HEAP_MAPPED.load(Ordering::Acquire);
        let new_mapped = current.checked_add(additional_bytes).ok_or(())?;

        // P17-T026: safety cap — refuse to exceed HEAP_MAX_SIZE.
        if new_mapped > HEAP_MAX_SIZE {
            log::error!(
                "[mm] heap growth refused: requested total {}KiB exceeds max {}MiB",
                new_mapped / 1024,
                HEAP_MAX_SIZE / (1024 * 1024),
            );
            return Err(());
        }

        // CAS to reserve [current..new_mapped). Loser retries with the
        // updated value (which may now be large enough to skip growth).
        match HEAP_MAPPED.compare_exchange_weak(
            current,
            new_mapped,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break current,
            Err(_) => continue,
        }
    };
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
                // Return the just-allocated frame to avoid leaking it.
                super::frame_allocator::free_frame(frame.start_address().as_u64());
                break;
            }
        }
        pages_mapped += 1;
    }

    if pages_mapped == 0 {
        return Err(());
    }

    // Extend the allocator for however many pages were actually mapped.
    let bytes_mapped = pages_mapped * 4096;
    unsafe {
        ALLOCATOR.inner.lock().extend(bytes_mapped);
    }

    // If we mapped fewer pages than reserved, adjust HEAP_MAPPED back down.
    let wasted = additional_bytes - bytes_mapped;
    if wasted > 0 {
        HEAP_MAPPED.fetch_sub(wasted, Ordering::Release);
    }

    log::info!(
        "[mm] heap grown by {}KiB → total {}KiB",
        bytes_mapped / 1024,
        (current_mapped + bytes_mapped) / 1024,
    );

    Ok(())
}

/// Attempt to grow the heap enough to satisfy `layout`, trying escalating sizes.
///
/// Returns `true` if at least one growth succeeded and the caller should retry
/// the allocation.
fn try_grow_on_oom_for_layout(layout: Layout) -> bool {
    // Round the requested size up to at least one page.
    let requested = (layout.size() + 4095) & !4095;
    let requested = requested.max(4096);

    // Try escalating growth sizes.
    for &size in &[requested, 1 << 20, 2 << 20, 4 << 20] {
        if grow_heap(size).is_ok() {
            return true;
        }
    }
    false
}

/// Attempt to grow the heap on OOM (simple 1 MiB attempt).
///
/// Kept for backward compatibility; the `RetryAllocator` uses the more
/// sophisticated `try_grow_on_oom_for_layout` instead.
#[expect(dead_code)]
pub fn try_grow_on_oom() -> bool {
    grow_heap(1024 * 1024).is_ok()
}

/// Kernel heap statistics snapshot.
pub struct HeapStats {
    pub total_size: usize,
    pub used_bytes: usize,
    pub free_bytes: usize,
    pub alloc_count: u64,
    pub dealloc_count: u64,
}

/// Returns a snapshot of kernel heap statistics.
pub fn heap_stats() -> HeapStats {
    let total_size = HEAP_MAPPED.load(Ordering::Relaxed);
    let free_bytes = ALLOCATOR.inner.lock().free();
    let used_bytes = total_size.saturating_sub(free_bytes);
    HeapStats {
        total_size,
        used_bytes,
        free_bytes,
        alloc_count: ALLOC_COUNT.load(Ordering::Relaxed),
        dealloc_count: DEALLOC_COUNT.load(Ordering::Relaxed),
    }
}
