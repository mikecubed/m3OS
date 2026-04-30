//! Kernel slab allocator with per-CPU magazine caching (Phase 53a, Tracks B.3 + E).
//!
//! Each CPU owns a [`PerCpuMagazines`] with two magazines (loaded + previous)
//! per size class.  Allocation and free fast paths operate lock-free under
//! `without_interrupts`, falling back to the shared [`MagazineDepot`] and then
//! to the underlying [`SlabCache`] when the local magazines are exhausted or
//! full.
//!
//! ## Cross-CPU free routing (Track E.1 / E.2)
//!
//! Every slab page is tagged in the dense `PageMeta` side table with its
//! owning CPU and size class (stored out-of-line so 4096-byte objects remain
//! usable).  When `slab_free` detects that the freed object belongs to a
//! different CPU, it CAS-pushes the pointer onto the victim CPU's lock-free
//! MPSC [`CrossCpuFreeList`] (one per size class per CPU).  The owning CPU
//! batch-collects via `take_all` on its next magazine refill.
//!
//! The named slab caches (`task_cache`, `fd_cache`, …) are preserved for
//! compatibility with existing callers and the stats surface.
//!
//! ## Minimal allocation-context contract (Track F.1)
//!
//! The slab magazine layer follows the same two-tier contract as
//! [`crate::mm::frame_allocator::AllocationContext`]:
//!
//! * **IRQ-sensitive / page-fault-adjacent callers** rely on the guarded local
//!   magazine fast path (`without_interrupts` + same-core guard). Re-entrant
//!   allocs bypass magazines and try the direct slab path; re-entrant frees
//!   fall back to the owner CPU's lock-free queue instead of touching magazine
//!   state twice.
//! * **Sleepable callers** may tolerate the depot/slab cold path and retry after
//!   `None`.
//!
//! Cold paths still use spin locks only, so "sleepable" here means "allowed to
//! take the contended cold path", not that the allocator literally sleeps.
//!
//! ## Allocator-local reclaim ordering (Track F.2)
//!
//! Memory-pressure reclaim must not mutate another CPU's magazines or
//! cross-CPU free list in place. The reclaim sequence is therefore:
//!
//! 1. [`crate::mm::frame_allocator::drain_per_cpu_caches`] returns order-0 page
//!    hoards to the buddy via owner-CPU self-drain.
//! 2. [`collect_remote_frees`] asks each CPU (local critical section or IPI
//!    self-drain) to flush its cross-CPU free lists and local magazines back to
//!    the backing slab caches, then drains depot magazines on the initiator.
//! 3. [`reclaim_empty_slabs`] runs only after step 2 so `inuse_count == 0`
//!    really means "no object is still hidden in a magazine/depot/remote queue".
//!
//! The cold reclaim path may hold interrupts disabled while it walks the local
//! magazines and takes the per-size-class slab lock, but it never holds a slab
//! lock while requesting another CPU to mutate its local caches.

use crate::task::scheduler::IrqSafeMutex;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use kernel_core::cross_cpu_free::CrossCpuFreeList;
use kernel_core::magazine::{Magazine, MagazineDepot};
#[allow(unused_imports)]
use kernel_core::size_class::size_to_class;
use kernel_core::size_class::{NUM_SIZE_CLASSES, SIZE_CLASSES};
use kernel_core::slab::SlabCache;

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

/// Same-core non-reentrancy guard for per-CPU magazine mutations.
static MAGAZINE_GUARD: AtomicU64 = AtomicU64::new(0);

struct LocalMagazineGuard {
    mask: u64,
}

impl LocalMagazineGuard {
    #[inline]
    fn try_enter() -> Option<Self> {
        let mask = 1u64 << (crate::smp::per_core().core_id as u64);
        let prev = MAGAZINE_GUARD.fetch_or(mask, Ordering::AcqRel);
        if prev & mask != 0 {
            return None;
        }
        Some(Self { mask })
    }
}

impl Drop for LocalMagazineGuard {
    fn drop(&mut self) {
        MAGAZINE_GUARD.fetch_and(!self.mask, Ordering::Release);
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum MagazineHotPathProbe {
    None = 0,
    AllocateHit = 1,
    FreePush = 2,
}

#[cfg(test)]
static MAGAZINE_HOT_PATH_PROBE: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(MagazineHotPathProbe::None as u8);
#[cfg(test)]
static MAGAZINE_HOT_PATH_PTR: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn arm_magazine_hot_path_probe(probe: MagazineHotPathProbe, ptr: *mut u8) {
    MAGAZINE_HOT_PATH_PTR.store(ptr as usize, Ordering::Release);
    MAGAZINE_HOT_PATH_PROBE.store(probe as u8, Ordering::Release);
}

#[cfg(test)]
fn run_magazine_hot_path_probe(expected: MagazineHotPathProbe, class_idx: usize) {
    if MAGAZINE_HOT_PATH_PROBE
        .compare_exchange(
            expected as u8,
            MagazineHotPathProbe::None as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        let ptr = MAGAZINE_HOT_PATH_PTR.swap(0, Ordering::AcqRel) as *mut u8;
        if !ptr.is_null() {
            magazine_free(class_idx, ptr, crate::smp::per_core().core_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Per-CPU cross-CPU free lists (E.1)
// ---------------------------------------------------------------------------

/// Per-CPU atomic free list heads for cross-CPU slab frees.
///
/// When CPU B frees an object owned by CPU A, it CAS-pushes to
/// `lists[class_idx]` on CPU A's `CrossCpuFreeLists`.  CPU A collects
/// the entire queue with `take_all` on its next allocation refill.
///
/// All operations are lock-free and allocation-free (safe from ISR-disabled
/// magazine paths and any CPU context).
pub struct CrossCpuFreeLists {
    pub lists: [CrossCpuFreeList; NUM_SIZE_CLASSES],
}

impl CrossCpuFreeLists {
    pub const fn new() -> Self {
        Self {
            lists: [
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
                CrossCpuFreeList::new(),
            ],
        }
    }
}

// Safety: CrossCpuFreeLists contains only AtomicPtr fields and is designed
// for concurrent access from any CPU.
unsafe impl Send for CrossCpuFreeLists {}
unsafe impl Sync for CrossCpuFreeLists {}

// ---------------------------------------------------------------------------
// Global depot + backing slab caches (one per size class)
// ---------------------------------------------------------------------------

/// Per-size-class global state: a magazine depot and a backing slab cache.
///
/// Phase 57b G.4 — `slab` is an [`IrqSafeMutex`] so the per-size-class slab
/// cache inherits Track F.1's preempt-discipline.  Acquired only from task
/// context; the magazine layer wraps callsites in `without_interrupts`
/// already, so the additional IRQ-mask in `IrqSafeMutex::lock` is a no-op
/// inside those scopes.
#[allow(dead_code)]
struct SizeClassState {
    depot: MagazineDepot,
    slab: IrqSafeMutex<SlabCache>,
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

#[inline]
fn size_class_state_ready() -> bool {
    SIZE_CLASS_STATE.get().is_some()
}

// ---------------------------------------------------------------------------
// Named slab caches (compatibility layer)
// ---------------------------------------------------------------------------

/// Pre-configured slab caches for common kernel object sizes.
///
/// These caches are infrastructure for future migration of kernel object
/// allocations (Phase 33, Track C.4 — deferred).
///
/// Phase 57b G.4 — each cache is wrapped in [`IrqSafeMutex`] so it inherits
/// Track F.1's preempt-discipline.  Task-context only.
#[allow(dead_code)]
pub struct KernelSlabCaches {
    /// 512-byte objects (e.g. task control blocks).
    pub task_cache: IrqSafeMutex<SlabCache>,
    /// 64-byte objects (e.g. file descriptors).
    pub fd_cache: IrqSafeMutex<SlabCache>,
    /// 128-byte objects (e.g. IPC endpoints).
    pub endpoint_cache: IrqSafeMutex<SlabCache>,
    /// 4096-byte objects (e.g. pipe buffers).
    pub pipe_cache: IrqSafeMutex<SlabCache>,
    /// 256-byte objects (e.g. socket structures).
    pub socket_cache: IrqSafeMutex<SlabCache>,
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
/// recover the owning size class and owning CPU from the object address.
fn size_class_page_alloc(class_idx: usize) -> Option<usize> {
    let frame = crate::mm::frame_allocator::allocate_frame()?;
    let phys = frame.start_address().as_u64();
    let cpu_id = if crate::smp::is_per_core_ready() {
        crate::smp::per_core().core_id
    } else {
        0
    };
    crate::mm::heap::register_slab_page(phys, class_idx, cpu_id);
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
            slab: IrqSafeMutex::new(SlabCache::new(SIZE_CLASSES[i], 4096)),
        })
    });

    // Named caches (backward compatibility).
    SLAB_CACHES.call_once(|| KernelSlabCaches {
        task_cache: IrqSafeMutex::new(SlabCache::new(512, 4096)),
        fd_cache: IrqSafeMutex::new(SlabCache::new(64, 4096)),
        endpoint_cache: IrqSafeMutex::new(SlabCache::new(128, 4096)),
        pipe_cache: IrqSafeMutex::new(SlabCache::new(4096, 4096)),
        socket_cache: IrqSafeMutex::new(SlabCache::new(256, 4096)),
    });
    log::info!("[mm] slab caches initialized (13 size classes + depots)");
}

/// Get a reference to the kernel slab caches.
#[allow(dead_code)]
pub fn caches() -> &'static KernelSlabCaches {
    SLAB_CACHES.get().expect("slab caches not initialized")
}

// ---------------------------------------------------------------------------
// Allocator-local reclaim (F.2)
// ---------------------------------------------------------------------------

/// Remote-drain completion counter for `collect_remote_frees`.
static SLAB_RECLAIM_PENDING: AtomicU8 = AtomicU8::new(0);
/// Whether the current `IPI_CACHE_DRAIN` round should also flush slab-local
/// magazines / cross-CPU free lists on the receiving cores.
static SLAB_RECLAIM_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Serializes initiators so the reclaim IPI handshake cannot race.
///
/// Phase 57b — **preempt-only** migration shape.  The lock holder broadcasts
/// `IPI_CACHE_DRAIN` to every other online core and spins on
/// `SLAB_RECLAIM_PENDING` until each acks via [`handle_reclaim_ipi`].  IF
/// MUST stay enabled across the lock-held region: a contender that takes
/// this lock with IF=0 cannot service the holder's reclaim IPI, deadlocking
/// both cores.  `handle_reclaim_ipi` does not touch this lock, so re-entry
/// of an IRQ handler on the holder's core during the spin-wait is safe.
static SLAB_RECLAIM_LOCK: spin::Mutex<()> = spin::Mutex::new(());

fn drain_local_reclaimable_objects() -> super::heap::AllocatorLocalReclaimStats {
    let mut stats = super::heap::AllocatorLocalReclaimStats::default();
    if !crate::smp::is_per_core_ready() || !size_class_state_ready() {
        return stats;
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let Some(_guard) = LocalMagazineGuard::try_enter() else {
            log::warn!("[mm] slab reclaim skipped local drain due to held magazine guard");
            return;
        };
        let core = crate::smp::per_core();
        let mags = unsafe { &mut *core.slab_magazines.get() };

        for class_idx in 0..NUM_SIZE_CLASSES {
            let state = &sc_state()[class_idx];
            let pair = &mut mags.pairs[class_idx];
            let mut slab = state.slab.lock();

            let mut node = core.cross_cpu_free.lists[class_idx].take_all();
            while !node.is_null() {
                let next = unsafe { (node as *const *mut u8).read() };
                slab.free(node as usize);
                stats.remote_free_objects += 1;
                node = next;
            }

            while let Some(obj) = pair.loaded.pop() {
                slab.free(obj as usize);
                stats.magazine_objects += 1;
            }
            while let Some(obj) = pair.previous.pop() {
                slab.free(obj as usize);
                stats.magazine_objects += 1;
            }
        }
    });

    stats
}

fn drain_depot_magazines(stats: &mut super::heap::AllocatorLocalReclaimStats) {
    if !size_class_state_ready() {
        return;
    }

    for class_idx in 0..NUM_SIZE_CLASSES {
        let state = &sc_state()[class_idx];
        state.depot.drain_full(|mag| {
            let mut slab = state.slab.lock();
            while let Some(obj) = mag.pop() {
                slab.free(obj as usize);
                stats.depot_objects += 1;
            }
        });
    }
}

/// Flush pending remote frees and hidden magazine objects back into the slab
/// metadata so empty pages become reclaimable.
///
/// The initiator drains its own CPU-local state directly, requests the same
/// owner-CPU self-drain from remotes via `IPI_CACHE_DRAIN`, waits for those
/// remotes to acknowledge, then drains depot-held full magazines.
pub fn collect_remote_frees() -> super::heap::AllocatorLocalReclaimStats {
    if !size_class_state_ready() {
        return super::heap::AllocatorLocalReclaimStats::default();
    }

    // Phase 57b — preempt-only.  IF stays enabled because the holder
    // broadcasts `IPI_CACHE_DRAIN` and spins on remote acks; a contender
    // that masked IF would block both cores.
    crate::task::scheduler::preempt_disable();
    let stats = {
        let _reclaim_guard = SLAB_RECLAIM_LOCK.lock();
        let mut stats = drain_local_reclaimable_objects();

        if crate::smp::is_per_core_ready() {
            let my_core = crate::smp::per_core().core_id;
            let mut remote_count: u8 = 0;
            for cid in 0..crate::smp::core_count() {
                if cid == my_core {
                    continue;
                }
                if let Some(data) = crate::smp::get_core_data(cid)
                    && data.is_online.load(Ordering::Acquire)
                {
                    remote_count += 1;
                }
            }

            if remote_count != 0 {
                SLAB_RECLAIM_PENDING.store(remote_count, Ordering::Release);
                SLAB_RECLAIM_ACTIVE.store(true, Ordering::Release);
                crate::smp::ipi::send_ipi_all_excluding_self(crate::smp::ipi::IPI_CACHE_DRAIN);
                while SLAB_RECLAIM_PENDING.load(Ordering::Acquire) != 0 {
                    core::hint::spin_loop();
                }
                SLAB_RECLAIM_ACTIVE.store(false, Ordering::Release);
            }
        }

        drain_depot_magazines(&mut stats);
        stats
    };
    crate::task::scheduler::preempt_enable();
    stats
}

/// IPI-side companion to [`collect_remote_frees`].
///
/// Runs only on the owner CPU. If reclaim is active, flushes that CPU's
/// cross-CPU free lists and magazines back into the slab caches, then signals
/// the initiator via `SLAB_RECLAIM_PENDING`.
pub fn handle_reclaim_ipi() {
    if SLAB_RECLAIM_ACTIVE.load(Ordering::Acquire) {
        let _ = drain_local_reclaimable_objects();
        SLAB_RECLAIM_PENDING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Reclaim every completely-empty size-class slab page.
///
/// Callers should run [`collect_remote_frees`] first; otherwise hidden frees in
/// magazines / depots will keep `inuse_count` non-zero and the slab will remain
/// intentionally unreclaimable.
pub fn reclaim_empty_slabs() -> usize {
    if !size_class_state_ready() {
        return 0;
    }

    let phys_offset = crate::mm::phys_offset();
    let mut reclaimed = 0usize;
    for class_idx in 0..NUM_SIZE_CLASSES {
        let state = &sc_state()[class_idx];
        let mut slab = state.slab.lock();
        reclaimed += slab.reclaim_empty(|base| {
            let phys = (base as u64)
                .checked_sub(phys_offset)
                .expect("slab reclaim page not in physmap");
            super::heap::unregister_slab_page(phys, class_idx);
            super::frame_allocator::free_frame_direct(phys);
        });
    }
    reclaimed
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
/// 3. Drain this CPU's cross-CPU free list for this size class (E.1).
/// 4. Exchange empty magazine for full from depot (depot lock).
/// 5. If depot empty, batch-fill a magazine from the backing slab (slab lock).
///
/// All steps run with interrupts masked to prevent ISR reentrancy.
///
/// Returns the virtual address of the allocated object, or `None` on OOM.
///
/// # Minimal allocation-context contract (F.1)
///
/// - [`crate::mm::frame_allocator::AllocationContext::IrqSensitive`] callers
///   get a guarded local-magazine fast path. If a same-core re-entrant entry
///   is detected, allocation bypasses magazines and attempts a direct slab
///   allocation instead.
/// - [`crate::mm::frame_allocator::AllocationContext::Sleepable`] callers may
///   tolerate depot/slab lock spinning and retry after `None`.
///
/// # Slow-path behavior
///
/// The depot exchange and slab refill paths do **not** sleep, but they may
/// spin on the depot or slab lock. The allocator performs one depot exchange
/// and one slab-fill attempt before returning `None`.
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
        let Some(_local_guard) = LocalMagazineGuard::try_enter() else {
            return slab_alloc_reentrant_fallback(class_idx);
        };
        let core = crate::smp::per_core();
        let mags = unsafe { &mut *core.slab_magazines.get() };
        let pair = &mut mags.pairs[class_idx];

        // 1. Try loaded magazine.
        if let Some(ptr) = pair.loaded.pop() {
            #[cfg(test)]
            run_magazine_hot_path_probe(MagazineHotPathProbe::AllocateHit, class_idx);
            return Some(ptr);
        }

        // 2. Swap loaded ↔ previous, retry.
        core::mem::swap(&mut pair.loaded, &mut pair.previous);
        if let Some(ptr) = pair.loaded.pop() {
            #[cfg(test)]
            run_magazine_hot_path_probe(MagazineHotPathProbe::AllocateHit, class_idx);
            return Some(ptr);
        }

        // 3. Drain cross-CPU free list (E.1 batch collect).
        let chain = core.cross_cpu_free.lists[class_idx].take_all();
        if !chain.is_null() {
            let mut node = chain;
            while !node.is_null() {
                // Read next-pointer before pushing (magazine doesn't modify object).
                let next = unsafe { (node as *const *mut u8).read() };
                if pair.loaded.push(node).is_err() {
                    // Magazine full — push excess back to cross-CPU list.
                    // push() overwrites node's intrusive ptr, so read next first.
                    unsafe { core.cross_cpu_free.lists[class_idx].push(node) };
                    let mut rest = next;
                    while !rest.is_null() {
                        let n = unsafe { (rest as *const *mut u8).read() };
                        unsafe { core.cross_cpu_free.lists[class_idx].push(rest) };
                        rest = n;
                    }
                    break;
                }
                node = next;
            }
            if let Some(ptr) = pair.loaded.pop() {
                #[cfg(test)]
                run_magazine_hot_path_probe(MagazineHotPathProbe::AllocateHit, class_idx);
                return Some(ptr);
            }
        }

        // 4. Exchange empty magazine for full from depot.
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

        // 5. Depot empty — batch-fill from the slab layer.
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
/// `owning_cpu` is the CPU that owns the slab page containing `ptr` (read
/// from the dense `PageMeta` side table by the caller).
///
/// ## Routing (Track E.2)
///
/// - **Same CPU** (`owning_cpu == current`): push to local magazine (fast
///   path, no atomics beyond interrupt masking).
/// - **Different CPU**: CAS-push to the victim CPU's per-size-class
///   [`CrossCpuFreeList`] — lock-free and allocation-free.
///
/// The local-magazine hierarchy mirrors allocation:
///
/// 1. Push to `previous` magazine (no lock, no atomic).
/// 2. If previous full, swap loaded ↔ previous and retry.
/// 3. If both full, exchange full magazine for empty from depot (depot lock).
/// 4. If depot has no empties, drain full magazine back to slab (slab lock)
///    and deposit the now-empty magazine back so it can be reused.
///
/// # Minimal allocation-context contract (F.1)
///
/// - [`crate::mm::frame_allocator::AllocationContext::IrqSensitive`] callers
///   mutate the local magazine pair under `without_interrupts` plus the
///   same-core guard. A same-core re-entrant free bypasses magazines and falls
///   back to direct slab free or the owner CPU's lock-free queue.
/// - [`crate::mm::frame_allocator::AllocationContext::Sleepable`] callers may
///   tolerate depot/slab lock spinning when a full magazine must be drained.
///
/// # Slow-path behavior
///
/// The depot exchange and slab drain paths do **not** sleep, but they may spin
/// on the depot/slab lock. `magazine_free` does not retry; once the object is
/// queued or returned to the backing slab, the call is finished.
#[allow(dead_code)]
pub fn magazine_free(class_idx: usize, ptr: *mut u8, owning_cpu: u8) {
    debug_assert!(class_idx < NUM_SIZE_CLASSES);

    if !crate::smp::is_per_core_ready() {
        slab_free_fallback(class_idx, ptr);
        return;
    }

    x86_64::instructions::interrupts::without_interrupts(|| {
        let Some(_local_guard) = LocalMagazineGuard::try_enter() else {
            slab_free_reentrant_fallback(class_idx, ptr, owning_cpu);
            return;
        };
        let core = crate::smp::per_core();

        // E.2: route based on page ownership.
        if core.core_id != owning_cpu {
            // Cross-CPU free: CAS push to victim CPU's atomic free list.
            if let Some(victim) = crate::smp::get_core_data(owning_cpu) {
                // Safety: ptr is a freed slab object with at least size_of::<*mut u8>()
                // bytes (guaranteed by slab minimum object size).
                unsafe { victim.cross_cpu_free.lists[class_idx].push(ptr) };
                return;
            }
            // Victim CPU not initialized — fall through to local magazine.
        }

        // Same-CPU free: push to local magazine (fast path).
        let mags = unsafe { &mut *core.slab_magazines.get() };
        let pair = &mut mags.pairs[class_idx];

        // 1. Try previous magazine.
        if pair.previous.push(ptr).is_ok() {
            #[cfg(test)]
            run_magazine_hot_path_probe(MagazineHotPathProbe::FreePush, class_idx);
            return;
        }

        // 2. Swap loaded ↔ previous, retry.
        core::mem::swap(&mut pair.loaded, &mut pair.previous);
        if pair.previous.push(ptr).is_ok() {
            #[cfg(test)]
            run_magazine_hot_path_probe(MagazineHotPathProbe::FreePush, class_idx);
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

fn slab_alloc_reentrant_fallback(class_idx: usize) -> Option<*mut u8> {
    let state = &sc_state()[class_idx];
    let mut slab = state.slab.try_lock()?;
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

fn slab_free_reentrant_fallback(class_idx: usize, ptr: *mut u8, owning_cpu: u8) {
    let state = &sc_state()[class_idx];
    let current_cpu = crate::smp::per_core().core_id;

    if owning_cpu == current_cpu
        && let Some(mut slab) = state.slab.try_lock()
    {
        slab.free(ptr as usize);
        return;
    }

    if let Some(victim) = crate::smp::get_core_data(owning_cpu) {
        unsafe { victim.cross_cpu_free.lists[class_idx].push(ptr) };
        return;
    }

    if let Some(mut slab) = state.slab.try_lock() {
        slab.free(ptr as usize);
        return;
    }

    panic!("reentrant slab free fallback could not make progress");
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

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    fn ensure_test_per_core() {
        if !crate::smp::is_per_core_ready() {
            crate::smp::init_bsp_per_core();
        }
    }

    fn drain_local_magazines_for_test(class_idx: usize) {
        ensure_test_per_core();
        x86_64::instructions::interrupts::without_interrupts(|| {
            let Some(_guard) = LocalMagazineGuard::try_enter() else {
                panic!("test drain re-entered local magazine guard");
            };
            let core = crate::smp::per_core();
            let state = &sc_state()[class_idx];
            let mut slab = state.slab.lock();

            let mut node = core.cross_cpu_free.lists[class_idx].take_all();
            while !node.is_null() {
                let next = unsafe { (node as *const *mut u8).read() };
                slab.free(node as usize);
                node = next;
            }

            let mags = unsafe { &mut *core.slab_magazines.get() };
            let pair = &mut mags.pairs[class_idx];
            while let Some(obj) = pair.loaded.pop() {
                slab.free(obj as usize);
            }
            while let Some(obj) = pair.previous.pop() {
                slab.free(obj as usize);
            }
        });
    }

    #[test_case]
    fn magazine_alloc_hot_path_tolerates_reentrant_free() {
        ensure_test_per_core();
        let class_idx = size_to_class(64).expect("64-byte class");
        drain_local_magazines_for_test(class_idx);

        let before = sc_state()[class_idx].slab.lock().stats().active_objects;
        let current_cpu = crate::smp::per_core().core_id;

        let nested = magazine_alloc(class_idx).expect("nested object");
        arm_magazine_hot_path_probe(MagazineHotPathProbe::AllocateHit, nested);

        let outer = magazine_alloc(class_idx).expect("outer object");
        magazine_free(class_idx, outer, current_cpu);

        drain_local_magazines_for_test(class_idx);
        let after = sc_state()[class_idx].slab.lock().stats().active_objects;
        assert_eq!(
            after, before,
            "reentrant free during magazine_alloc hot path leaked slab objects"
        );
    }

    #[test_case]
    fn magazine_free_hot_path_tolerates_reentrant_free() {
        ensure_test_per_core();
        let class_idx = size_to_class(64).expect("64-byte class");
        drain_local_magazines_for_test(class_idx);

        let before = sc_state()[class_idx].slab.lock().stats().active_objects;
        let current_cpu = crate::smp::per_core().core_id;

        let outer = magazine_alloc(class_idx).expect("outer object");
        let nested = magazine_alloc(class_idx).expect("nested object");
        arm_magazine_hot_path_probe(MagazineHotPathProbe::FreePush, nested);

        magazine_free(class_idx, outer, current_cpu);

        drain_local_magazines_for_test(class_idx);
        let after = sc_state()[class_idx].slab.lock().stats().active_objects;
        assert_eq!(
            after, before,
            "reentrant free during magazine_free hot path leaked slab objects"
        );
    }

    #[test_case]
    fn global_alloc_slab_reclaim_does_not_leak_bootstrap_heap() {
        ensure_test_per_core();
        let class_idx = size_to_class(64).expect("64-byte class");
        drain_local_magazines_for_test(class_idx);
        let _ = collect_remote_frees();
        let _ = reclaim_empty_slabs();

        drop(Box::new([0u8; 64]));
        let _ = collect_remote_frees();
        let _ = reclaim_empty_slabs();

        let baseline = crate::mm::heap::heap_stats();
        for cycle in 0..4 {
            drop(Box::new([0u8; 64]));
            let _ = collect_remote_frees();
            let _ = reclaim_empty_slabs();

            let after = crate::mm::heap::heap_stats();
            assert_eq!(
                after.slab_pages, baseline.slab_pages,
                "slab reclaim left extra pages allocated on cycle {cycle}"
            );
            assert_eq!(
                after.used_bytes, baseline.used_bytes,
                "slab reclaim leaked bootstrap heap bytes on cycle {cycle}"
            );
        }
    }
}
