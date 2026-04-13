//! Kernel slab allocator with per-CPU magazine caching (Phase 53a, Track B.3).
//!
//! Each CPU owns a [`PerCpuMagazines`] with two magazines (loaded + previous)
//! per size class.  Allocation and free fast paths operate lock-free under
//! `without_interrupts`, falling back to the shared [`MagazineDepot`] and then
//! to the underlying [`SlabCache`] when the local magazines are exhausted or
//! full.
//!
//! The named slab caches (`task_cache`, `fd_cache`, …) are preserved for
//! compatibility with existing callers and the stats surface.

use kernel_core::magazine::{Magazine, MagazineDepot};
#[allow(unused_imports)]
use kernel_core::size_class::size_to_class;
use kernel_core::size_class::{NUM_SIZE_CLASSES, SIZE_CLASSES};
use kernel_core::slab::SlabCache;
use spin::Mutex;

// ---------------------------------------------------------------------------
// Per-CPU magazine layer (B.3)
// ---------------------------------------------------------------------------

/// Per-CPU magazine pair for a single size class.
///
/// `loaded` is tried first on allocate; `previous` is tried first on free.
/// Swapping the two avoids depot round-trips when alloc/free rates are balanced.
pub struct MagazinePair {
    loaded: Magazine,
    previous: Magazine,
}

impl MagazinePair {
    pub const fn new() -> Self {
        Self {
            loaded: Magazine::new(),
            previous: Magazine::new(),
        }
    }
}

/// Per-CPU magazine state for all 13 size classes.
///
/// One instance lives in each core's [`PerCoreData`] as an `UnsafeCell`.
/// Only accessed by the owning core with interrupts masked.
pub struct PerCpuMagazines {
    pairs: [MagazinePair; NUM_SIZE_CLASSES],
}

impl PerCpuMagazines {
    pub const fn new() -> Self {
        // const-compatible: manually build the array since MagazinePair::new()
        // is const but [MagazinePair::new(); N] requires Copy.
        Self {
            pairs: [
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
                MagazinePair::new(),
            ],
        }
    }
}

// Safety: PerCpuMagazines is only accessed by its owning core with interrupts
// masked.  The raw pointers inside Magazine are opaque handles.
unsafe impl Send for PerCpuMagazines {}

// ---------------------------------------------------------------------------
// Global depot + backing slab caches (one per size class)
// ---------------------------------------------------------------------------

/// Per-size-class global state: a magazine depot and a backing slab cache.
#[allow(dead_code)]
struct SizeClassState {
    depot: MagazineDepot,
    slab: Mutex<SlabCache>,
}

/// Global array of per-size-class depots and slab caches.
static SIZE_CLASS_STATE: spin::Once<[SizeClassState; NUM_SIZE_CLASSES]> = spin::Once::new();

/// Helper to get the global size-class state array.
#[allow(dead_code)]
fn sc_state() -> &'static [SizeClassState; NUM_SIZE_CLASSES] {
    SIZE_CLASS_STATE
        .get()
        .expect("slab size-class state not initialized")
}

// ---------------------------------------------------------------------------
// Named slab caches (compatibility layer)
// ---------------------------------------------------------------------------

/// Pre-configured slab caches for common kernel object sizes.
///
/// These caches are infrastructure for future migration of kernel object
/// allocations (Phase 33, Track C.4 — deferred).
#[allow(dead_code)]
pub struct KernelSlabCaches {
    /// 512-byte objects (e.g. task control blocks).
    pub task_cache: Mutex<SlabCache>,
    /// 64-byte objects (e.g. file descriptors).
    pub fd_cache: Mutex<SlabCache>,
    /// 128-byte objects (e.g. IPC endpoints).
    pub endpoint_cache: Mutex<SlabCache>,
    /// 4096-byte objects (e.g. pipe buffers).
    pub pipe_cache: Mutex<SlabCache>,
    /// 256-byte objects (e.g. socket structures).
    pub socket_cache: Mutex<SlabCache>,
}

static SLAB_CACHES: spin::Once<KernelSlabCaches> = spin::Once::new();

/// Page allocator callback for slab caches: obtains a virtual-address page
/// backed by a physical frame.
#[allow(dead_code)]
fn slab_page_alloc() -> Option<usize> {
    let frame = crate::mm::frame_allocator::allocate_frame()?;
    Some((crate::mm::phys_offset() + frame.start_address().as_u64()) as usize)
}

/// Page allocator callback for size-class slab caches.
///
/// Unlike the generic named-cache helper above, this tags the backing page in
/// the heap allocator's dense metadata table so `GlobalAlloc::dealloc` can
/// recover the owning size class from the object address.
fn size_class_page_alloc(class_idx: usize) -> Option<usize> {
    let frame = crate::mm::frame_allocator::allocate_frame()?;
    let phys = frame.start_address().as_u64();
    crate::mm::heap::register_slab_page(phys, class_idx);
    Some((crate::mm::phys_offset() + phys) as usize)
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the kernel slab caches and per-size-class depot/slab state.
///
/// Must be called after the heap is ready (post `frame_allocator::init_buddy`).
pub fn init() {
    // Initialize per-size-class depots and backing slab caches.
    SIZE_CLASS_STATE.call_once(|| {
        // Build each element; core::array::from_fn isn't const but works here.
        core::array::from_fn(|i| SizeClassState {
            depot: MagazineDepot::new(),
            slab: Mutex::new(SlabCache::new(SIZE_CLASSES[i], 4096)),
        })
    });

    // Named caches (backward compatibility).
    SLAB_CACHES.call_once(|| KernelSlabCaches {
        task_cache: Mutex::new(SlabCache::new(512, 4096)),
        fd_cache: Mutex::new(SlabCache::new(64, 4096)),
        endpoint_cache: Mutex::new(SlabCache::new(128, 4096)),
        pipe_cache: Mutex::new(SlabCache::new(4096, 4096)),
        socket_cache: Mutex::new(SlabCache::new(256, 4096)),
    });
    log::info!("[mm] slab caches initialized (13 size classes + depots)");
}

/// Get a reference to the kernel slab caches.
#[allow(dead_code)]
pub fn caches() -> &'static KernelSlabCaches {
    SLAB_CACHES.get().expect("slab caches not initialized")
}

// ---------------------------------------------------------------------------
// Per-CPU magazine allocation fast path (B.3)
// ---------------------------------------------------------------------------

/// Allocate an object of exactly `size_class_bytes` via the per-CPU magazine
/// layer.
///
/// This is the per-CPU magazine-cached fast path.  The call hierarchy is:
///
/// 1. Pop from `loaded` magazine (no lock, no atomic).
/// 2. If loaded empty, swap loaded ↔ previous and retry.
/// 3. If both empty, exchange empty magazine for full from depot (depot lock).
/// 4. If depot empty, batch-fill a magazine from the backing slab (slab lock).
///
/// All steps run with interrupts masked to prevent ISR reentrancy.
///
/// Returns the virtual address of the allocated object, or `None` on OOM.
///
/// # Safety contract
///
/// The caller must ensure `class_idx` is a valid index (0..NUM_SIZE_CLASSES).
/// This is guaranteed when `class_idx` comes from `size_to_class()`.
#[allow(dead_code)]
pub fn magazine_alloc(class_idx: usize) -> Option<*mut u8> {
    debug_assert!(class_idx < NUM_SIZE_CLASSES);

    if !crate::smp::is_per_core_ready() {
        // Pre-SMP fallback: go straight to the backing slab.
        return slab_alloc_fallback(class_idx);
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mags = unsafe { &mut *crate::smp::per_core().slab_magazines.get() };
        let pair = &mut mags.pairs[class_idx];

        // 1. Try loaded magazine.
        if let Some(ptr) = pair.loaded.pop() {
            return Some(ptr);
        }

        // 2. Swap loaded ↔ previous, retry.
        core::mem::swap(&mut pair.loaded, &mut pair.previous);
        if let Some(ptr) = pair.loaded.pop() {
            return Some(ptr);
        }

        // 3. Exchange empty magazine for full from depot.
        let state = &sc_state()[class_idx];
        let empty_mag = core::mem::replace(&mut pair.loaded, Magazine::new());
        match state.depot.exchange_empty_for_full(empty_mag) {
            Ok(full_mag) => {
                pair.loaded = full_mag;
                return pair.loaded.pop(); // guaranteed Some
            }
            Err(returned_empty) => {
                pair.loaded = returned_empty;
            }
        }

        // 4. Depot empty — batch-fill from the slab layer.
        let mut slab = state.slab.lock();
        // Fill the loaded magazine from the slab.
        while !pair.loaded.is_full() {
            if let Some(addr) = slab.allocate(&mut || size_class_page_alloc(class_idx)) {
                let _ = pair.loaded.push(addr as *mut u8);
            } else {
                break;
            }
        }
        pair.loaded.pop()
    })
}

/// Free an object back via the per-CPU magazine layer.
///
/// The call hierarchy mirrors allocation:
///
/// 1. Push to `previous` magazine (no lock, no atomic).
/// 2. If previous full, swap loaded ↔ previous and retry.
/// 3. If both full, exchange full magazine for empty from depot (depot lock).
/// 4. If depot has no empties, drain full magazine back to slab (slab lock)
///    and deposit the now-empty magazine back so it can be reused.
///
/// # Cross-CPU free safety
///
/// Track E (cross-CPU atomic free lists) is not yet implemented.  For now,
/// frees always go to the *calling* CPU's magazines.  This means an object
/// allocated on CPU A but freed on CPU B ends up in CPU B's magazine for that
/// size class — functionally correct (the slab layer does not assume per-CPU
/// ownership of individual objects) but suboptimal for cache locality.  Track E
/// will add MPSC atomic free lists to route cross-CPU frees to the home CPU.
#[allow(dead_code)]
pub fn magazine_free(class_idx: usize, ptr: *mut u8) {
    debug_assert!(class_idx < NUM_SIZE_CLASSES);

    if !crate::smp::is_per_core_ready() {
        slab_free_fallback(class_idx, ptr);
        return;
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let mags = unsafe { &mut *crate::smp::per_core().slab_magazines.get() };
        let pair = &mut mags.pairs[class_idx];

        // 1. Try previous magazine.
        if pair.previous.push(ptr).is_ok() {
            return;
        }

        // 2. Swap loaded ↔ previous, retry.
        core::mem::swap(&mut pair.loaded, &mut pair.previous);
        if pair.previous.push(ptr).is_ok() {
            return;
        }

        // 3. Both full — exchange full magazine for empty from depot.
        let state = &sc_state()[class_idx];
        let full_mag = core::mem::replace(&mut pair.previous, Magazine::new());
        match state.depot.exchange_full_for_empty(full_mag) {
            Ok(empty_mag) => {
                pair.previous = empty_mag;
                let _ = pair.previous.push(ptr);
                return;
            }
            Err(returned_full) => {
                pair.previous = returned_full;
            }
        }

        // 4. Depot has no empties — drain previous magazine back to slab,
        //    then push the freed pointer into the now-empty magazine.
        let mut slab = state.slab.lock();
        while let Some(obj) = pair.previous.pop() {
            slab.free(obj as usize);
        }
        // previous is now empty — push the freed object directly.
        let _ = pair.previous.push(ptr);
    });
}

// ---------------------------------------------------------------------------
// Fallback paths (pre-SMP or direct slab access)
// ---------------------------------------------------------------------------

/// Direct slab allocation (pre-SMP fallback).
#[allow(dead_code)]
fn slab_alloc_fallback(class_idx: usize) -> Option<*mut u8> {
    let state = &sc_state()[class_idx];
    let mut slab = state.slab.lock();
    slab.allocate(&mut || size_class_page_alloc(class_idx))
        .map(|a| a as *mut u8)
}

/// Direct slab free (pre-SMP fallback).
#[allow(dead_code)]
fn slab_free_fallback(class_idx: usize, ptr: *mut u8) {
    let state = &sc_state()[class_idx];
    let mut slab = state.slab.lock();
    slab.free(ptr as usize);
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Summary of all slab cache statistics.
pub struct AllSlabStats {
    pub task: kernel_core::slab::SlabStats,
    pub fd: kernel_core::slab::SlabStats,
    pub endpoint: kernel_core::slab::SlabStats,
    pub pipe: kernel_core::slab::SlabStats,
    pub socket: kernel_core::slab::SlabStats,
}

/// Returns a snapshot of all named slab cache statistics.
pub fn all_slab_stats() -> AllSlabStats {
    let c = caches();
    AllSlabStats {
        task: c.task_cache.lock().stats(),
        fd: c.fd_cache.lock().stats(),
        endpoint: c.endpoint_cache.lock().stats(),
        pipe: c.pipe_cache.lock().stats(),
        socket: c.socket_cache.lock().stats(),
    }
}

/// Returns per-size-class slab statistics from the backing slab caches
/// (does not include objects sitting in per-CPU magazines or depots).
#[allow(dead_code)]
pub fn size_class_slab_stats() -> [kernel_core::slab::SlabStats; NUM_SIZE_CLASSES] {
    let state = sc_state();
    core::array::from_fn(|i| state[i].slab.lock().stats())
}
