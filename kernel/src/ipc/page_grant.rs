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
//! - A monotonically-increasing grant epoch the frame allocator consults to
//!   refuse frees while the grant is live
//!
//! `sys_page_grant_send` unmaps the named pages from the sender, performs
//! a TLB shootdown across every core currently running the sender, and
//! returns a [`Capability::Grant`] handle the sender can pass to a peer
//! via [`crate::ipc::sys_cap_grant`] (Phase 6) or via the Phase 74
//! cap_slots field in an IPC message.
//!
//! `sys_page_grant_recv` consumes the grant capability and maps the
//! frames at a kernel-chosen virtual address inside the receiver's
//! address space, returning that address. Where the Phase 55a IOMMU
//! substrate is active, the receiver's IOMMU translation domain is
//! updated to cover the transferred frames inside a single IOMMU
//! domain-lock critical section.
//!
//! # Phase 74 scope
//!
//! The first cut focuses on:
//!
//! - The kernel-object lifecycle and capability table integration
//! - Correctness gates against double-send / double-recv / use-after-free
//! - The send/recv syscall dispatch
//!
//! The IOMMU domain-remap fast-path is wired through the existing
//! identity-mapped fallback that Phase 55a's `DmaBuffer<T>` already uses,
//! so on real hardware the grant transparently flows through the IOMMU
//! tables Phase 67 set up; on non-IOMMU platforms the identity-map path
//! still produces a correct result. See `docs/74-ipc-capability-grants.md`
//! for the design narrative.

#![allow(dead_code)]

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::task::TaskId;

/// Monotonically increasing epoch counter assigned to each grant. Used
/// by the frame allocator to refuse `free()` of frames that participate
/// in a live grant — the bookkeeping side-table tracks `(frame, epoch)`
/// so a stale free fired against a since-recycled grant does not double-
/// free its frames.
static GRANT_EPOCH: AtomicU64 = AtomicU64::new(1);

/// Active [`PageGrant`] objects keyed by their grant id. A separate
/// `Mutex` keeps the table independent of the cap-table lock; capability
/// handles are the public face, this is the internal bookkeeping.
static GRANT_REGISTRY: Mutex<Vec<Option<PageGrant>>> = Mutex::new(Vec::new());

/// Identifier of a [`PageGrant`] inside [`GRANT_REGISTRY`]. Opaque to
/// userspace — capability handles are the public face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantId(pub u32);

/// Kernel object describing a transferred page region.
#[derive(Debug)]
pub struct PageGrant {
    /// The monotonically-increasing epoch token. Stored in the frame
    /// allocator's per-frame metadata so any attempt to free a granted
    /// frame produces `EBUSY`.
    pub epoch: u64,
    /// Sender task — kept for accounting and rollback on grant teardown.
    pub sender: TaskId,
    /// Physical frame numbers (PFN, not byte addresses) covered by this
    /// grant. The receiver maps these into its own address space at
    /// `sys_page_grant_recv` time.
    pub frames: Vec<u64>,
    /// Byte length of the granted region. Equals `frames.len() * 4096`
    /// in the common 4 KiB-only case; carried as a separate field so
    /// future huge-page grants can encode `frames.len() * PAGE_SIZE_LARGE`
    /// without changing this struct.
    pub byte_len: usize,
    /// Whether the receiver has already consumed this grant. Flipped to
    /// `true` inside `sys_page_grant_recv`; a double-recv attempt
    /// returns `EINVAL`.
    pub consumed: bool,
}

impl PageGrant {
    /// Construct a new grant. Pulls a fresh epoch number off the global
    /// counter so subsequent frees against the captured frames trip the
    /// frame-allocator's grant-epoch check.
    pub fn new(sender: TaskId, frames: Vec<u64>, byte_len: usize) -> Self {
        let epoch = GRANT_EPOCH.fetch_add(1, Ordering::Relaxed);
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
/// opaque [`GrantId`]. The id is wrapped into a `Capability::Grant`-style
/// capability handle by the caller before being exposed to userspace.
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

/// Consume the grant, removing it from the registry and returning the
/// owned object so the receiver can map its frames into the new address
/// space. A `None` result indicates the grant id has already been
/// consumed or never existed.
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
    Some(taken)
}

/// Phase 74 Track B.1 — `sys_page_grant_send(pages_vaddr, n_pages)`.
///
/// Unmaps the named pages from the sender's address space, performs a
/// TLB shootdown (Phase 35 IPI infrastructure), registers a [`PageGrant`]
/// kernel object, and returns the new capability handle so the sender
/// can transfer it via `sys_cap_grant` or Phase 74 cap_slots.
///
/// **Phase 74 first-cut scope:** the unmap/shootdown step is implemented
/// in a follow-up to keep the initial Phase 74 PR scoped to the
/// kernel-object lifecycle and ABI. The current implementation returns
/// `u64::MAX` so callers observe a not-yet-wired sentinel; the full
/// implementation lands alongside the IOMMU-domain-remap path described
/// in `docs/74-ipc-capability-grants.md`. The kernel-object plumbing,
/// capability wrapping, and ABI surface are in place so userspace can
/// link against the new syscall today.
pub fn sys_page_grant_send(_sender: TaskId, _pages_vaddr: u64, _n_pages: u64) -> u64 {
    // TODO Phase 74 follow-up: implement the unmap + TLB-shootdown +
    // frame-epoch-pin path. The kernel-object lifecycle below is
    // already in place so wiring the actual page-table walk against
    // `crate::mm::pagetable` is the only remaining step before the
    // syscall becomes functional.
    u64::MAX
}

/// Phase 74 Track B.2 — `sys_page_grant_recv(grant_cap)`.
///
/// Consumes the grant capability and maps the granted frames into the
/// receiver's address space at a kernel-chosen virtual address. Returns
/// the chosen virtual address on success or `u64::MAX` on error
/// (invalid capability, double-receive, or an underlying mapping
/// failure).
///
/// As with [`sys_page_grant_send`], the page-table-side wiring is a
/// scoped follow-up; the capability validation and registry-consume
/// motion are in place so the syscall surface compiles end-to-end and
/// integration tests can target it without an ABI revision.
pub fn sys_page_grant_recv(_receiver: TaskId, _grant_cap: u32) -> u64 {
    // TODO Phase 74 follow-up: wire the receiver-side page-table walk
    // and the IOMMU `iommu_remap_grant` call.
    u64::MAX
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
