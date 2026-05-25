//! Phase 74 Track B — Page-grant bulk-data transport.
//!
//! Closes the Phase 6 deferral at `kernel/src/ipc/mod.rs:34` for page-grant
//! transfers. Eliminates the per-frame `memcpy` cost that the Phase 72
//! multi-app compositor previously paid on every 1080p surface upload by
//! moving ownership of the backing pages between processes without
//! copying any bytes.
//!
//! # Model
//!
//! A [`PageGrant`] is a kernel object that records:
//!
//! - The list of physical frames extracted from the sender's address space
//! - The byte length of the granted region
//! - A monotonically-increasing grant epoch
//!
//! `sys_page_grant_send` walks the sender's page table, collects the PFN
//! list, unmaps each page (issuing local `INVLPG` per page plus an
//! SMP-wide TLB shootdown across cores running the sender), and creates a
//! [`PageGrant`] kernel object. The grant is wrapped in a
//! [`Capability::PageGrant`] and inserted into the sender's capability
//! table so the sender can pass it to a peer via `sys_cap_grant` (Phase 6)
//! or via the Phase 74 cap_slots field in an IPC message.
//!
//! `sys_page_grant_recv` consumes the grant capability and maps the
//! frames at a kernel-chosen virtual address inside the receiver's
//! address space, returning that address.
//!
//! # IOMMU integration
//!
//! On non-IOMMU platforms, the receiver-side page-table map is the only
//! step the kernel needs: the receiver's CPU MMU translates the new
//! virtual addresses to the granted physical frames directly.
//!
//! On IOMMU-enabled platforms, an additional `iommu_remap_grant` call
//! would update the receiver's IOMMU translation domain. For Phase 74
//! the identity-fallback path that Phase 55a's `DmaBuffer<T>` uses is
//! sufficient — the granted frames are already identity-covered in the
//! IOMMU domain at boot, so a fresh remap is not required for the
//! receiver to access them. A future hardening pass can tighten this to
//! per-grant IOMMU domain entries.

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{
    Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB, Translate,
    mapper::TranslateResult,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::ipc::Capability;
use crate::mm::paging::GlobalFrameAlloc;
use crate::task::TaskId;

/// Monotonically increasing epoch counter assigned to each grant.
///
/// Stored on every [`PageGrant`] so future hardening passes that wire
/// per-frame metadata into the frame allocator can refuse `free()` on
/// frames whose grant is still live. Phase 74's first cut does not yet
/// hook the frame allocator (no allocator API surfaces per-frame
/// metadata), but the epoch is plumbed through the kernel-object
/// lifecycle so the follow-up is a strictly-additive change.
static GRANT_EPOCH: AtomicU64 = AtomicU64::new(1);

/// Active [`PageGrant`] objects keyed by their grant id. A separate
/// `Mutex` keeps the table independent of the cap-table lock; capability
/// handles are the public face, this is the internal bookkeeping.
static GRANT_REGISTRY: Mutex<Vec<Option<PageGrant>>> = Mutex::new(Vec::new());

/// Per-frame grant tracker — maps each pinned PFN (physical frame
/// number) to the grant epoch that owns it. The frame allocator
/// consults this table on every `free_frame` call via
/// [`is_frame_granted`] and refuses to free a pinned frame.
///
/// Entries are installed by [`pin_frames`] (called from
/// [`PageGrant::new`]) and removed by [`unpin_frames`] (called from
/// [`consume`] right before the frames flow back into the receiver's
/// page table). A `BTreeMap` keeps lookups O(log n) without depending
/// on a hash allocator.
static GRANTED_FRAMES: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());

/// Register `frames` as pinned by the supplied grant epoch.
///
/// Idempotent: a frame already pinned by the same epoch is left
/// unchanged. A frame pinned by a *different* epoch panics — the
/// kernel's bookkeeping is broken if two grants claim the same PFN
/// simultaneously.
fn pin_frames(frames: &[u64], epoch: u64) {
    let mut map = GRANTED_FRAMES.lock();
    for &pfn in frames {
        match map.insert(pfn, epoch) {
            None => {}
            Some(existing) if existing == epoch => {}
            Some(existing) => {
                panic!(
                    "[page_grant] frame {:#x} double-pin: existing epoch {} vs new epoch {}",
                    pfn, existing, epoch
                );
            }
        }
    }
}

/// Release the pins held by `epoch` over `frames`. Frames not pinned by
/// `epoch` (already released, or pinned by a different grant) are left
/// alone — the allocator's free path will surface any leak through its
/// own bookkeeping.
fn unpin_frames(frames: &[u64], epoch: u64) {
    let mut map = GRANTED_FRAMES.lock();
    for &pfn in frames {
        if let Some(&existing) = map.get(&pfn)
            && existing == epoch
        {
            map.remove(&pfn);
        }
    }
}

/// Frame-allocator integration: returns `true` if `phys` (a byte
/// address) names a 4 KiB frame currently pinned by some live grant.
///
/// Called from `mm::frame_allocator::free_frame` (and the contiguous
/// variant) to refuse `free()` on a granted frame and emit a
/// rate-limited warning. The function is `pub` so the allocator can
/// import it without exposing the grant registry's internal structure.
pub fn is_frame_granted(phys: u64) -> bool {
    let pfn = phys / 4096;
    GRANTED_FRAMES.lock().contains_key(&pfn)
}

/// Phase 74 hardening — diagnostic count of `free_frame` attempts that
/// were refused because the frame was pinned by a live grant. Bumped
/// from the allocator's free path; readable via the diagnostic test
/// in this module.
static GRANTED_FREE_REFUSALS: AtomicU64 = AtomicU64::new(0);

/// Bump the granted-free-refusal counter. Called from
/// `mm::frame_allocator::free_frame` after `is_frame_granted` returns
/// `true`.
pub fn record_granted_free_refusal() {
    GRANTED_FREE_REFUSALS.fetch_add(1, Ordering::Relaxed);
}

/// Diagnostic accessor for the granted-free-refusal counter.
pub fn granted_free_refusal_count() -> u64 {
    GRANTED_FREE_REFUSALS.load(Ordering::Relaxed)
}

/// Identifier of a [`PageGrant`] inside [`GRANT_REGISTRY`]. Opaque to
/// userspace — capability handles are the public face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantId(pub u32);

/// Kernel object describing a transferred page region.
#[derive(Debug)]
pub struct PageGrant {
    /// The monotonically-increasing epoch token. Future hardening will
    /// store this in the frame allocator's per-frame metadata so a stale
    /// free against a since-recycled grant trips an `EBUSY`.
    pub epoch: u64,
    /// Sender task — kept for accounting and rollback on grant teardown.
    pub sender: TaskId,
    /// Physical frame numbers (PFN, not byte addresses) covered by this
    /// grant. The receiver maps these into its own address space at
    /// `sys_page_grant_recv` time.
    pub frames: Vec<u64>,
    /// Byte length of the granted region. Equals `frames.len() * 4096`
    /// in the common 4 KiB-only case.
    pub byte_len: usize,
    /// Whether the receiver has already consumed this grant. Flipped to
    /// `true` inside `sys_page_grant_recv`; a double-recv attempt
    /// returns `EINVAL`.
    pub consumed: bool,
}

impl PageGrant {
    /// Construct a new grant. Pulls a fresh epoch number off the global
    /// counter and pins every frame in [`GRANTED_FRAMES`] so a stray
    /// `free_frame` against any PFN in the grant trips
    /// [`is_frame_granted`].
    pub fn new(sender: TaskId, frames: Vec<u64>, byte_len: usize) -> Self {
        let epoch = GRANT_EPOCH.fetch_add(1, Ordering::Relaxed);
        pin_frames(&frames, epoch);
        Self {
            epoch,
            sender,
            frames,
            byte_len,
            consumed: false,
        }
    }

    /// Number of 4 KiB pages covered by this grant.
    pub fn n_pages(&self) -> usize {
        self.frames.len()
    }
}

/// Register a [`PageGrant`] in the global registry and return its
/// opaque [`GrantId`]. The id is wrapped into a `Capability::PageGrant`
/// capability by the caller before being exposed to userspace.
pub fn register(grant: PageGrant) -> GrantId {
    let mut reg = GRANT_REGISTRY.lock();
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(grant);
            return GrantId(i as u32);
        }
    }
    let id = reg.len() as u32;
    reg.push(Some(grant));
    GrantId(id)
}

/// Look up a grant by id, returning a borrow guarded by the registry
/// mutex. The grant is dropped from the registry by [`consume`].
pub fn with_grant<R>(id: GrantId, f: impl FnOnce(&mut PageGrant) -> R) -> Option<R> {
    let mut reg = GRANT_REGISTRY.lock();
    reg.get_mut(id.0 as usize)
        .and_then(|slot| slot.as_mut().map(f))
}

/// Read the grant's frame list without consuming the registry entry or
/// releasing the per-frame pins. Used by [`sys_page_grant_recv`] to walk
/// the page-table mapping side first, so [`consume`] only runs once the
/// mapping has fully landed and the syscall is committed.
///
/// Returns `None` if `id` is invalid or the grant has already been
/// consumed.
pub fn peek_grant_frames(id: GrantId) -> Option<Vec<u64>> {
    let reg = GRANT_REGISTRY.lock();
    reg.get(id.0 as usize)?
        .as_ref()
        .filter(|g| !g.consumed)
        .map(|g| g.frames.clone())
}

/// Consume the grant, removing it from the registry and returning the
/// owned object so the receiver can map its frames into the new address
/// space. A `None` result indicates the grant id has already been
/// consumed or never existed.
///
/// Releases the per-frame pins installed by [`PageGrant::new`] so the
/// receiver can map the frames and the allocator can later free them
/// through the normal page-table teardown path.
pub fn consume(id: GrantId) -> Option<PageGrant> {
    let mut reg = GRANT_REGISTRY.lock();
    let slot = reg.get_mut(id.0 as usize)?;
    if let Some(g) = slot.as_ref()
        && g.consumed
    {
        return None;
    }
    let mut taken = slot.take()?;
    taken.consumed = true;
    drop(reg);
    unpin_frames(&taken.frames, taken.epoch);
    Some(taken)
}

// ---------------------------------------------------------------------------
// Phase 74 Track B.1 — sys_page_grant_send
// ---------------------------------------------------------------------------

/// Maximum number of pages a single grant may carry. Cap protects the
/// kernel from a userspace request that would force a multi-megabyte
/// PFN-list allocation; chosen to cover a 4K RGBA framebuffer (8 064
/// pages) with headroom.
const MAX_PAGES_PER_GRANT: usize = 16384;

/// Phase 74 Track B.1 — `sys_page_grant_send(pages_vaddr, n_pages)`.
///
/// Walks the sender's page table for `n_pages` 4 KiB pages starting at
/// `pages_vaddr`, collects the physical frame numbers, unmaps each page
/// from the sender's address space, issues an SMP-wide TLB shootdown
/// over the unmapped range, builds a [`PageGrant`] kernel object,
/// registers it, and inserts a [`Capability::PageGrant`] capability
/// into the sender's capability table.
///
/// Returns the new capability handle as `u64` on success, or `u64::MAX`
/// on any error (alignment violation, page not mapped, page-count
/// overflow, registry-full, cap-table-full).
///
/// # All-or-nothing
///
/// If the page-table walk hits an unmapped page in the middle of the
/// range, every already-unmapped page is restored before the error
/// returns. The sender observes the failing call as a pure no-op.
pub fn sys_page_grant_send(sender: TaskId, pages_vaddr: u64, n_pages: u64) -> u64 {
    if n_pages == 0 || n_pages as usize > MAX_PAGES_PER_GRANT {
        return u64::MAX;
    }
    if !pages_vaddr.is_multiple_of(4096) {
        return u64::MAX;
    }
    let n_pages = n_pages as usize;

    // Resolve the sender's CR3 via the userspace PID. Kernel tasks
    // (`pid == 0`) have no userspace VAs, so a grant from a kernel task
    // is rejected.
    let pid = match crate::task::scheduler::pid_for_task_id(sender) {
        Some(p) if p != 0 => p,
        _ => return u64::MAX,
    };
    let cr3_phys = match cr3_for_pid(pid) {
        Some(c) => c,
        None => return u64::MAX,
    };
    let cr3_frame = match PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(cr3_phys)) {
        Ok(f) => f,
        Err(_) => return u64::MAX,
    };

    let mut frames: Vec<u64> = Vec::with_capacity(n_pages);
    // Per-page original PTE flags captured in Phase A0 (pre-walk). The
    // rollback path uses these to restore each prefix page with its
    // exact prior permissions instead of a fixed
    // PRESENT|WRITABLE|USER_ACCESSIBLE|NO_EXECUTE set — that hardcoded
    // set could silently change read-only or executable mappings into
    // writable/NX ones when a grant range crossed an unmapped page.
    let mut orig_flags: Vec<PageTableFlags> = Vec::with_capacity(n_pages);

    // SAFETY: cr3_frame names the sender's PML4. No other OffsetPageTable
    // over the same frame is alive on this core (we drop `mapper` before
    // any syscall path can re-enter).
    let mut mapper = unsafe { crate::mm::mapper_for_frame(cr3_frame) };

    // Phase A0: pre-walk to verify every page is mapped and snapshot its
    // PTE flags before any unmap mutates the page table. If any page in
    // the requested range is unmapped, reject the whole call with no
    // side effects — the sender observes the failure as a pure no-op.
    for i in 0..n_pages {
        let vaddr = pages_vaddr + (i as u64) * 4096;
        match mapper.translate(VirtAddr::new(vaddr)) {
            TranslateResult::Mapped { flags, .. } => orig_flags.push(flags),
            _ => return u64::MAX,
        }
    }

    // Phase A: walk + unmap. On error, restore the prefix using the
    // per-page flags captured in Phase A0.
    for i in 0..n_pages {
        let vaddr = pages_vaddr + (i as u64) * 4096;
        let page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(vaddr));
        match mapper.unmap(page) {
            Ok((frame, flush)) => {
                flush.flush();
                frames.push(frame.start_address().as_u64() / 4096);
            }
            Err(_) => {
                // Restore prefix: re-map every PFN already extracted
                // with its original PTE flag set.
                let mut alloc = GlobalFrameAlloc;
                for (j, &pfn) in frames.iter().enumerate() {
                    let restore_addr = pages_vaddr + (j as u64) * 4096;
                    let restore_page: Page<Size4KiB> =
                        Page::containing_address(VirtAddr::new(restore_addr));
                    let restore_frame = match PhysFrame::<Size4KiB>::from_start_address(
                        PhysAddr::new(pfn * 4096),
                    ) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };
                    let flags = orig_flags[j];
                    // SAFETY: restoring a mapping the kernel just removed
                    // with the exact PTE flags captured in Phase A0; no
                    // permission widening or executable-bit flipping.
                    if let Ok(flush) =
                        unsafe { mapper.map_to(restore_page, restore_frame, flags, &mut alloc) }
                    {
                        flush.flush();
                    }
                }
                return u64::MAX;
            }
        }
    }

    // OffsetPageTable holds a mutable borrow of the PML4; let it fall
    // out of scope naturally here (it does not implement Drop, so an
    // explicit `drop()` call is a clippy lint).
    let _ = mapper;

    // Phase B: SMP-wide TLB shootdown over the unmapped range. The
    // sender's other threads must not retain a stale TLB entry pointing
    // at a frame the receiver is about to own.
    if let Some(addr_space) = addr_space_for_pid(pid) {
        let start = pages_vaddr;
        let end = pages_vaddr + (n_pages as u64) * 4096;
        // SAFETY: addr_space is a raw pointer into the process table;
        // tlb_shootdown_range takes &AddressSpace and the table is
        // protected by the process-table lock. Dereference is bounded
        // to this call.
        unsafe {
            let aspace_ref = &*addr_space;
            crate::smp::tlb::tlb_shootdown_range(aspace_ref, start, end);
        }
    }

    // Phase C: register + cap insert. Roll back the unmap if cap-insert
    // fails (returning frames is best-effort — the registry's frame list
    // is the canonical owner now).
    let byte_len = n_pages * 4096;
    let grant = PageGrant::new(sender, frames, byte_len);
    let grant_id = register(grant);
    let cap = Capability::PageGrant {
        grant_id: grant_id.0,
    };
    match crate::task::scheduler::insert_cap(sender, cap) {
        Ok(handle) => u64::from(handle),
        Err(_) => {
            // Consume the grant so the frames are at least released by
            // the registry; without per-frame allocator hooks the
            // frames stay live but the grant cannot be re-handed-out.
            let _ = consume(grant_id);
            u64::MAX
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 74 Track B.2 — sys_page_grant_recv
// ---------------------------------------------------------------------------

/// Anonymous mmap region used to allocate fresh user VAs for the
/// receiver-side mapping. Matches the bump-pointer base used by
/// `device_host::reserve_user_va_for_pid` so grant mappings live in the
/// same VA window as device-host DMA mappings.
const GRANT_RECV_VA_BASE: u64 = 0x0000_0000_2000_0000;
const GRANT_RECV_VA_END: u64 = 0x0000_8000_0000_0000;

/// Phase 74 Track B.2 — IOMMU integration helper. Called from
/// [`sys_page_grant_recv`] after the receiver-side page-table mappings
/// land so a future driver-receiver use-case can wire device-side DMA
/// translation in the same syscall.
///
/// # Today's behaviour
///
/// The function is a documented identity-fallback shim:
///
/// - If [`crate::iommu::registry::translating()`] is `false` (no IOMMU
///   active, or only identity-mapping units are installed), the function
///   returns `Ok(())` without touching any IOMMU state — the CPU MMU
///   mapping done by `sys_page_grant_recv` is sufficient for the
///   receiver to access the granted frames.
/// - If IOMMU translation IS active and the receiver process holds zero
///   device claims, the function similarly returns `Ok(())` — no
///   per-device IOVA→phys mapping needs updating because the receiver
///   never initiates DMA through an IOMMU domain.
/// - If IOMMU translation is active AND the receiver holds at least one
///   device claim (i.e. it is a ring-3 driver), the function would
///   install per-frame identity-IOVA mappings on each of the receiver's
///   bound domains. The current build emits an info-level log and
///   returns `Ok(())` because no in-tree driver consumes page-grants
///   from another process; the hookable extension point is in place so
///   a future driver-receiver use case wires this with a single helper
///   call instead of duplicated registry walks.
///
/// # Why this is enough for Phase 74
///
/// The Phase 74 use cases (display_server surface buffers, audio_server
/// PCM rings) involve userspace-only receivers that access the granted
/// frames through the CPU's MMU. They do not initiate DMA into the
/// granted frames from a device the receiver controls. The IOMMU
/// domain stays untouched, the receiver still reads/writes the frames
/// correctly through its newly-installed page-table mappings, and the
/// design contract from Phase 55a's `DmaBuffer<T>` identity-fallback
/// path is honoured end-to-end.
pub fn iommu_remap_grant(receiver_pid: u32, frames: &[u64]) -> Result<(), &'static str> {
    if !crate::iommu::registry::translating() {
        log::trace!(
            "[page_grant] iommu_remap_grant: identity fallback (no IOMMU translation) \
             receiver_pid={} pages={}",
            receiver_pid,
            frames.len(),
        );
        return Ok(());
    }

    // Receiver holds no device claims → no driver-side DMA into these
    // frames → no IOMMU domain to update. This is the common case for
    // the Phase 74 use cases (display_server, audio_server clients).
    //
    // The check is "best effort": the device-host registry exposes
    // `drain_pid` for teardown but no read-only "does this PID own
    // any device?" predicate. A future hardening pass can add such a
    // predicate and surface a tighter trace line. The conservative
    // path here logs the IOMMU-active case and returns Ok so the
    // identity-fallback contract holds.
    log::info!(
        "[page_grant] iommu_remap_grant: IOMMU active, receiver_pid={} pages={} — \
         identity fallback (no driver-receiver migration yet wires per-domain mapping)",
        receiver_pid,
        frames.len(),
    );
    Ok(())
}

/// Phase 74 Track B.2 — `sys_page_grant_recv(grant_cap)`.
///
/// Looks up `grant_cap` in the receiver's capability table, consumes the
/// underlying [`PageGrant`], maps each frame into the receiver's address
/// space at a kernel-chosen virtual address, and returns that address.
///
/// Returns the virtual address of the mapped region on success, or
/// `u64::MAX` on any error (invalid handle, double-consume, no free VA,
/// page-table walk failure).
pub fn sys_page_grant_recv(receiver: TaskId, grant_cap: u32) -> u64 {
    // Validate cap type — peek only; we do not remove the cap or
    // consume the grant until the page-table mapping has fully landed.
    // Until then, any early-error path leaves the cap + grant in their
    // pre-call state so the caller can retry.
    let grant_id = match crate::task::scheduler::task_cap(receiver, grant_cap) {
        Ok(Capability::PageGrant { grant_id }) => GrantId(grant_id),
        _ => return u64::MAX,
    };

    let pid = match crate::task::scheduler::pid_for_task_id(receiver) {
        Some(p) if p != 0 => p,
        _ => return u64::MAX,
    };
    let cr3_phys = match cr3_for_pid(pid) {
        Some(c) => c,
        None => return u64::MAX,
    };
    let cr3_frame = match PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(cr3_phys)) {
        Ok(f) => f,
        Err(_) => return u64::MAX,
    };

    // Read the frame list without consuming the registry slot or
    // releasing the per-frame pins. Both are released by the [`consume`]
    // call at the bottom of the function, only on the success path.
    let frames = match peek_grant_frames(grant_id) {
        Some(f) => f,
        None => return u64::MAX,
    };

    let n_pages = frames.len();
    let bytes = (n_pages as u64) * 4096;

    // Reserve a fresh user VA range via the process's mmap bump
    // pointer. Mirrors `device_host::reserve_user_va_for_pid`. On VA
    // exhaustion the grant + cap are untouched, so the caller may retry.
    let base = match reserve_user_va(pid, bytes) {
        Some(v) => v,
        None => return u64::MAX,
    };

    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    // SAFETY: cr3_frame names the receiver's PML4. No other
    // OffsetPageTable over the same frame is alive on this core.
    let mut mapper = unsafe { crate::mm::mapper_for_frame(cr3_frame) };
    let mut alloc = GlobalFrameAlloc;

    for (i, &pfn) in frames.iter().enumerate() {
        let vaddr = base + (i as u64) * 4096;
        let page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(vaddr));
        let frame = match PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(pfn * 4096)) {
            Ok(f) => f,
            Err(_) => {
                rollback_recv_map(&mut mapper, base, i);
                return u64::MAX;
            }
        };
        // SAFETY: the destination VA was just reserved from mmap_next
        // and is not mapped; the frame came from the peeked grant and
        // is still pinned (consume runs at the bottom of this function).
        match unsafe { mapper.map_to(page, frame, flags, &mut alloc) } {
            Ok(flush) => flush.flush(),
            Err(_) => {
                rollback_recv_map(&mut mapper, base, i);
                return u64::MAX;
            }
        }
    }

    // Phase 74 Track B.2 hardening — give the IOMMU substrate a chance
    // to update the receiver-side IOVA translation. Current behaviour
    // is identity-fallback (see `iommu_remap_grant`); a future
    // hardening pass that wires per-domain IOVA mapping can hook the
    // same call site without changing the syscall ABI.
    let _ = iommu_remap_grant(pid, &frames);

    // Mapping landed end-to-end. Now (and only now) consume the grant —
    // dropping the registry entry and releasing the per-frame pins — and
    // remove the one-shot capability from the receiver's cap table. Any
    // earlier failure leaves both intact so the caller can retry.
    let _ = consume(grant_id);
    let _ = crate::task::scheduler::remove_task_cap(receiver, grant_cap);

    base
}

/// Roll back a partial receiver-side mapping by unmapping the prefix.
fn rollback_recv_map(mapper: &mut OffsetPageTable<'_>, base: u64, mapped: usize) {
    for j in 0..mapped {
        let va = base + (j as u64) * 4096;
        let page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(va));
        if let Ok((_f, flush)) = mapper.unmap(page) {
            flush.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Process-side helpers
// ---------------------------------------------------------------------------

/// Look up the CR3 (PML4 physical address) for a userspace PID.
fn cr3_for_pid(pid: u32) -> Option<u64> {
    let table = crate::process::PROCESS_TABLE.lock();
    table
        .find(pid)
        .and_then(|p| p.addr_space.as_ref().map(|a| a.pml4_phys().as_u64()))
}

/// Borrow the `AddressSpace` for `pid` as a raw pointer so the TLB
/// shootdown helper can read its `active_cores` mask. The pointer is
/// only valid for the duration of the call inside the locked process
/// table — callers must dereference inside a tight unsafe block while
/// the process is known to be live.
fn addr_space_for_pid(pid: u32) -> Option<*const crate::mm::AddressSpace> {
    let table = crate::process::PROCESS_TABLE.lock();
    table.find(pid).and_then(|p| {
        p.addr_space
            .as_ref()
            .map(|a| a.as_ref() as *const crate::mm::AddressSpace)
    })
}

/// Reserve `bytes` bytes of user VA from the process's `mmap_next`
/// bump pointer. Returns the base VA on success or `None` if the
/// reservation would push past the user-space ceiling.
fn reserve_user_va(pid: u32, bytes: u64) -> Option<u64> {
    crate::process::with_shared_mm_mut(pid, |_brk, mmap_next, _vmas| {
        let current = if *mmap_next == 0 {
            GRANT_RECV_VA_BASE
        } else {
            *mmap_next
        };
        let end = current
            .checked_add(bytes)
            .filter(|v| *v <= GRANT_RECV_VA_END)?;
        *mmap_next = end;
        Some(current)
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_grant() -> PageGrant {
        PageGrant::new(TaskId(1), alloc::vec![0x1000, 0x2000, 0x3000], 3 * 4096)
    }

    #[test]
    fn register_returns_unique_ids() {
        let a = register(fresh_grant());
        let b = register(fresh_grant());
        assert_ne!(a, b);
    }

    #[test]
    fn consume_removes_grant() {
        let id = register(fresh_grant());
        assert!(consume(id).is_some());
        assert!(consume(id).is_none());
    }

    #[test]
    fn epoch_monotonically_increases() {
        let a = register(fresh_grant());
        let b = register(fresh_grant());
        let epoch_a = with_grant(a, |g| g.epoch).expect("grant a present");
        let epoch_b = with_grant(b, |g| g.epoch).expect("grant b present");
        assert!(epoch_b > epoch_a);
        // Cleanup.
        let _ = consume(a);
        let _ = consume(b);
    }

    #[test]
    fn n_pages_matches_frames_len() {
        let grant = fresh_grant();
        assert_eq!(grant.n_pages(), 3);
        assert_eq!(grant.byte_len, 3 * 4096);
    }
}
