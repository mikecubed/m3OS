//! Shared-memory regions backed by a small refcounted registry.
//!
//! Each region owns a contiguous run of physical frames allocated from
//! the buddy allocator on creation. Multiple processes can map the same
//! region; the per-region refcount tracks how many live mappings exist.
//! When the refcount drops to zero, the underlying frames are returned
//! to the buddy.
//!
//! # Why this exists
//!
//! Phase 56's display protocol shipped a chunked-pixel transport
//! (`LABEL_PIXELS_CHUNK`) that copies surface bytes from the client's
//! address space into kernel bulk buffers and then into
//! `display_server`'s heap, ~252 IPC roundtrips per 1 MiB frame. The
//! Phase 57d follow-up replaces that with a shared-memory region: the
//! client allocates the buffer once, both processes map the same
//! physical frames, and per-frame updates degenerate to small protocol
//! verbs (`DamageSurface`, `CommitSurface`) with no pixel transport.
//!
//! # Lifecycle
//!
//! 1. `create(byte_len)` allocates `ceil(byte_len / 4096)` contiguous
//!    pre-zeroed pages from the buddy, inserts a registry entry with
//!    `refcount = 1`, and returns a small numeric `ShmId`. The initial
//!    `1` is the *creator handle*: the process that called `create`
//!    is responsible for one matching `decref` (via `sys_shm_destroy`,
//!    or as part of `sys_shm_unmap` if it later maps and unmaps the
//!    same region itself; `sys_shm_destroy` is the path that always
//!    works, including on the failure path before any map happens).
//! 2. `incref(id)` raises the refcount; the caller is now responsible
//!    for one matching `decref` (typically via `sys_shm_unmap`).
//! 3. `decref(id)` lowers the refcount; the frames are returned to the
//!    buddy when the count reaches zero.
//! 4. `frames(id)` returns the (start_phys, page_count) of an existing
//!    region for callers that need to install page-table mappings.
//!
//! Each `create` therefore needs one matching `destroy`, and each
//! successful `map` needs one matching `unmap`. A typical
//! create -> map -> unmap -> destroy cycle in the creator's process
//! returns the underlying frames to the buddy at the final step.
//!
//! # Page allocation note
//!
//! The buddy allocator only produces power-of-two contiguous runs, so
//! `byte_len` is rounded up to the next power-of-two page count rather
//! than just to the next page boundary. Callers that care about exact
//! pad bytes should size their request to a power-of-two pages.
//!
//! # Page-table integration
//!
//! When a process maps an SHM region, the leaf PTEs are tagged with
//! `PageTableFlags::BIT_11` (the same "device/hardware frame" marker
//! the framebuffer mapping uses). `mm::free_process_page_table`'s
//! teardown filter skips BIT_11 leaves, so an SHM region survives a
//! single mapper's process exit without being doubled-freed by both
//! the buddy (via teardown) and the registry (via `decref`). The
//! refcount path stays the single source of truth.
//!
//! # Security note
//!
//! Phase 56's single-client shape lets `ShmId` travel as a plain
//! integer through IPC: any process holding the id can `sys_shm_map`
//! against it. Production multi-tenant builds will tighten this to a
//! capability-secured grant via `Capability::Grant` transfer; the
//! registry's API is shaped to make that swap straightforward (the
//! `ShmId` becomes the identity carried inside the cap).

extern crate alloc;

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use crate::mm::frame_allocator;

/// Opaque identity of a shared-memory region. Globally unique within a
/// boot, allocated sequentially by [`create`]. Wraps `u32` rather than
/// being a raw alias so the IPC ABI can pin its width independently
/// from any cap handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShmId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmError {
    /// Caller asked for zero bytes.
    InvalidLength,
    /// Caller's `byte_len` rounds up to more than 2^15 pages
    /// (128 MiB) — the largest power-of-two page count that fits in
    /// the `u16` page count without the `(1 << order) as u16` cast
    /// wrapping. The buddy allocator's effective ceiling is currently
    /// `MAX_ORDER = 13` (8192 pages = 32 MiB), so requests between
    /// 32 MiB and 128 MiB will fail earlier with
    /// [`OutOfContiguousFrames`]; this guard keeps the cast sound if
    /// `MAX_ORDER` is ever raised again.
    LengthTooLarge,
    /// Buddy allocator could not produce a contiguous block of the
    /// requested order. Phase 73 surfaces (1920 × 1080 × 4 ≈ 7.91 MiB)
    /// fit inside the order-13 (32 MiB) max with comfortable headroom;
    /// this fires on adversarial requests or very fragmented memory.
    OutOfContiguousFrames,
    /// Lookup against an `ShmId` that does not name a live region.
    NotFound,
    /// Granting the requested `byte_len` would push the calling
    /// process over [`PER_PROCESS_SHM_CAP_BYTES`]. A clean
    /// back-pressure error the client can surface as "out of
    /// memory" — strictly preferable to letting one runaway process
    /// fragment the buddy allocator and OOM-panic the whole kernel
    /// next time anything tries to allocate a contiguous order-4
    /// run (new kernel stacks, large `Vec`s, etc).
    ProcessCapExceeded,
}

/// Maximum total bytes of live SHM regions a single process may
/// hold open at once, as measured by the sum of page-rounded
/// allocations on `sys_shm_create`. 256 MiB comfortably exceeds
/// the working set of any expected client (term: ~16 MiB
/// double-buffer at typical tile sizes; bar / wallpaper / launcher
/// similar order of magnitude) while bounding the worst-case
/// damage a runaway or misbehaving process can do.
pub const PER_PROCESS_SHM_CAP_BYTES: u64 = 256 * 1024 * 1024;

/// One row of the registry. Holds the contiguous frame run plus the
/// shared refcount plus the creator pid so the per-process byte
/// accounting can be reclaimed on final decref.
struct ShmEntry {
    start_phys: u64,
    page_count: u16,
    refcount: AtomicU32,
    /// PID of the process that called [`create`]. Stored so the
    /// last [`decref`] (the one that returns frames to the buddy)
    /// can subtract this region's byte count from the right
    /// per-process budget — even when the destroyer is not the
    /// creator (e.g. the SHM outlives the creator while a mapper
    /// still holds it).
    creator_pid: u32,
}

/// Module-level state. A `Mutex<BTreeMap>` is overkill for the
/// expected contention (one create + a handful of map / unmap per
/// surface lifecycle), but the BTreeMap gives us O(log n) lookup
/// without constraining `ShmId` to be a slot index.
static REGISTRY: Mutex<BTreeMap<ShmId, ShmEntry>> = Mutex::new(BTreeMap::new());

/// Per-process accounting of live SHM bytes (sum of
/// `page_count * 4096` over every region this pid has created and
/// that has not yet been fully released). [`create`] increments the
/// matching entry under the cap check; the final [`decref`]
/// subtracts. Stale entries — a process exited without destroying
/// its SHMs and another mapper is keeping them alive — are pruned
/// on the next refcount-zero event, since the entry's `creator_pid`
/// still points at the (now-dead) pid; `saturating_sub` handles the
/// case where the bookkeeping has already been cleared by
/// [`release_creator`].
static PROCESS_BYTES: Mutex<BTreeMap<u32, u64>> = Mutex::new(BTreeMap::new());

/// Counter for monotonic id allocation. Wraps after 2^32 ids — for
/// reference, allocating one id per CPU cycle that's still ~50s of
/// continuous churn before wrap, well past Phase 56's expected usage.
/// On wrap, [`create`] retries until an unused id is found.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Allocate a new shared-memory region of `byte_len` bytes (rounded up
/// to a 4 KiB page boundary). Returns the assigned `ShmId` plus the
/// `(start_phys, page_count)` of the underlying frame run; the frames
/// have already been pre-zeroed by the allocator. Refcount starts at
/// `1` — the caller is responsible for one matching [`decref`] (via
/// the unmap syscall, or via `decref` directly if the cap is dropped
/// before any mapping is installed).
pub fn create(byte_len: usize, creator_pid: u32) -> Result<(ShmId, u64, u16), ShmError> {
    if byte_len == 0 {
        return Err(ShmError::InvalidLength);
    }
    let pages = byte_len.div_ceil(4096);
    // Bound: `pages.next_power_of_two()` must fit in u16 so the
    // `(1 << order) as u16` cast below cannot wrap to 0.
    if pages > (1usize << 15) {
        return Err(ShmError::LengthTooLarge);
    }
    let order = pages.next_power_of_two().trailing_zeros() as usize;
    let page_count_u64 = 1u64 << order;
    let allocated_bytes = page_count_u64 * 4096;
    // Per-process cap check, performed *before* we touch the buddy
    // allocator so a denied request leaves no side effect. The
    // accounting tracks page-rounded bytes (what the kernel actually
    // commits), not the user-requested `byte_len`, so the cap reflects
    // real memory cost.
    {
        let mut pb = PROCESS_BYTES.lock();
        let current = pb.get(&creator_pid).copied().unwrap_or(0);
        let new_total = current.saturating_add(allocated_bytes);
        if new_total > PER_PROCESS_SHM_CAP_BYTES {
            return Err(ShmError::ProcessCapExceeded);
        }
        pb.insert(creator_pid, new_total);
    }
    // Reserve the frames. On failure we have to undo the budget
    // increment, otherwise a denied buddy allocation would leak the
    // cap counter.
    let frame = match frame_allocator::allocate_contiguous_zeroed(order) {
        Some(f) => f,
        None => {
            release_bytes(creator_pid, allocated_bytes);
            return Err(ShmError::OutOfContiguousFrames);
        }
    };
    let start_phys = frame.start_address().as_u64();
    let page_count = (1usize << order) as u16;

    let mut reg = REGISTRY.lock();
    let id = loop {
        let candidate = ShmId(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        // Skip id 0 so callers can use it as an "unset" sentinel.
        if candidate.0 == 0 {
            continue;
        }
        if !reg.contains_key(&candidate) {
            break candidate;
        }
    };
    reg.insert(
        id,
        ShmEntry {
            start_phys,
            page_count,
            refcount: AtomicU32::new(1),
            creator_pid,
        },
    );
    Ok((id, start_phys, page_count))
}

/// Subtract `bytes` from the byte budget tracked against `pid`.
/// Removes the map entry when the count reaches zero so dead pids
/// don't accumulate. Saturating to handle the case where
/// [`release_creator`] cleared the budget mid-flight (a creator
/// process exit while a mapper is still holding the SHM alive).
fn release_bytes(pid: u32, bytes: u64) {
    let mut pb = PROCESS_BYTES.lock();
    if let Some(current) = pb.get_mut(&pid) {
        *current = current.saturating_sub(bytes);
        if *current == 0 {
            pb.remove(&pid);
        }
    }
}

/// Clear all per-process SHM byte accounting for `pid` — called by
/// the process-exit path so a fresh PID assignment after teardown
/// starts with a clean budget even when the exiting process left
/// SHM regions alive via other mappers. Subsequent
/// refcount-zero decrefs against stale `creator_pid` entries are
/// no-ops via [`release_bytes`]'s saturating subtract.
pub fn release_creator(pid: u32) {
    let mut pb = PROCESS_BYTES.lock();
    pb.remove(&pid);
}

/// Increment the refcount for an existing region and return its
/// `(start_phys, page_count)` for installing a page-table mapping.
/// Errors if the id does not name a live region.
pub fn incref(id: ShmId) -> Result<(u64, u16), ShmError> {
    let reg = REGISTRY.lock();
    let entry = reg.get(&id).ok_or(ShmError::NotFound)?;
    entry.refcount.fetch_add(1, Ordering::Relaxed);
    Ok((entry.start_phys, entry.page_count))
}

/// Decrement the refcount for `id`. Returns `true` if this was the
/// last reference and the underlying frames have been returned to the
/// buddy allocator. Returns `false` if the region still has live
/// references. Returns [`ShmError::NotFound`] if the id is unknown.
pub fn decref(id: ShmId) -> Result<bool, ShmError> {
    let mut reg = REGISTRY.lock();
    let entry = reg.get(&id).ok_or(ShmError::NotFound)?;
    let prev = entry.refcount.fetch_sub(1, Ordering::AcqRel);
    if prev > 1 {
        return Ok(false);
    }
    // Last reference — extract the entry, drop the registry lock, and
    // return frames. Holding the lock across the buddy free path would
    // serialise SHM activity behind the global frame-allocator lock.
    let entry = reg.remove(&id).expect("entry vanished after refcount-zero");
    drop(reg);
    // Return the byte count to the creator's per-process budget
    // before freeing the frames; the order matters only if another
    // task is concurrently waiting on cap headroom (and even then,
    // either ordering is correct since we never temporarily
    // double-count).
    release_bytes(entry.creator_pid, (entry.page_count as u64) * 4096);
    let order = (entry.page_count as usize).trailing_zeros() as usize;
    frame_allocator::free_contiguous(entry.start_phys, order);
    Ok(true)
}

/// Return the `(start_phys, page_count)` of an existing region without
/// touching the refcount. Used by the syscall layer when validating a
/// caller-supplied `ShmId` before deciding to allocate a virtual
/// range; the matching `incref` lands inside the install-mapping path.
#[allow(dead_code)]
pub fn frames(id: ShmId) -> Result<(u64, u16), ShmError> {
    let reg = REGISTRY.lock();
    let entry = reg.get(&id).ok_or(ShmError::NotFound)?;
    Ok((entry.start_phys, entry.page_count))
}
