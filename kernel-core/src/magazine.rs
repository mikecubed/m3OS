//! Per-CPU magazine caching layer for the slab allocator.
//!
//! A [`Magazine`] is a fixed-capacity stack of freed object pointers that lives
//! on a single CPU—push and pop are O(1) with **no** synchronization.
//!
//! A [`MagazineDepot`] is the shared pool from which CPUs exchange empty
//! magazines for full ones (and vice-versa), protected by a spinlock.
//!
//! ## Phase 57b G.9 — preempt-discipline classification
//!
//! Per the Track A.1 spinlock callsite audit
//! (`docs/handoffs/57b-spinlock-callsite-audit.md`, row for
//! `kernel-core/src/magazine.rs`), the two `spin::Mutex` fields inside
//! [`MagazineDepot`] (`full` and `empty`) are classified **host-test-only**:
//!
//! - **Host build (`cargo test -p kernel-core`)** — the locks are exercised
//!   directly by the in-module unit tests below. Host tests are
//!   single-threaded and have no scheduler, so `spin::Mutex` provides exactly
//!   the contract the tests need; preempt-discipline is a no-op concept.
//!
//! - **Kernel build (`cargo xtask run`/`test`)** — the depot is a member of
//!   `kernel::mm::slab::SizeClassState`. The depot's lock acquisitions are
//!   reached from the slab fast path (`magazine_alloc` / `magazine_free`)
//!   and the cold reclaim path (`drain_depot_magazines`). The audit's
//!   classification is "host-test-only on host build / convert-to-irqsafe
//!   via wrapper on kernel build" — the **wrapper** referenced is the
//!   kernel-side caller, not a kernel-core type. `kernel-core` cannot
//!   reach `kernel::task::scheduler::preempt_disable` /
//!   `preempt_enable` from a `no_std` cdep without an extern hook, so
//!   the discipline is enforced **at the kernel-side consumer site** —
//!   either by holding the depot inside an `IrqSafeMutex` (Track F) or by
//!   bracketing each depot operation in an explicit `preempt_disable` /
//!   `preempt_enable` pair at the slab callsite.
//!
//! Today the kernel callers (`kernel/src/mm/slab.rs`) wrap depot operations
//! in `interrupts::without_interrupts(|| ...)` so IF=0 across the critical
//! section. That alone is sufficient to keep the depot's `spin::Mutex` from
//! being re-entered by an ISR on the same core. Phase 57b's `preempt_count`
//! discipline (Track F) adds a parallel guarantee that voluntary preemption
//! cannot fire inside the locked region; if/when Phase 57d wires voluntary
//! preemption points into the slab fast path, the kernel-side caller — not
//! `kernel-core` — owns the migration to `preempt_disable` / `preempt_enable`
//! (or to wrapping the depot in an `IrqSafeMutex<MagazineDepot>` field).
//!
//! Track G.9 deliberately keeps `MagazineDepot::full` / `::empty` as plain
//! `spin::Mutex` so the host test suite continues to exercise the pure-logic
//! invariants of the magazine state machine without pulling in any kernel-
//! side `IrqSafeMutex` machinery. The audit row's "host-test-only" tag
//! pins this contract.

use alloc::vec::Vec;
use spin::Mutex;

/// Number of object pointers a single magazine can hold.
pub const MAGAZINE_CAPACITY: usize = 32;

// ---------------------------------------------------------------------------
// Magazine
// ---------------------------------------------------------------------------

/// A fixed-capacity LIFO stack of freed object pointers.
///
/// Designed for single-CPU use—no internal locking.
pub struct Magazine {
    slots: [*mut u8; MAGAZINE_CAPACITY],
    count: usize,
}

impl core::fmt::Debug for Magazine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Magazine")
            .field("count", &self.count)
            .finish()
    }
}

// Safety: Magazine is moved between CPUs only through the depot, which uses a
// lock.  The raw pointers inside are opaque handles; the magazine never
// dereferences them.
unsafe impl Send for Magazine {}

impl Default for Magazine {
    fn default() -> Self {
        Self::new()
    }
}

impl Magazine {
    /// Create an empty magazine.
    pub const fn new() -> Self {
        Self {
            slots: [core::ptr::null_mut(); MAGAZINE_CAPACITY],
            count: 0,
        }
    }

    /// Returns `true` when the magazine has no stored pointers.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns `true` when the magazine is at capacity.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.count == MAGAZINE_CAPACITY
    }

    /// Number of pointers currently stored.
    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Push a pointer onto the magazine.  Returns `Err(ptr)` if full.
    #[inline]
    pub fn push(&mut self, ptr: *mut u8) -> Result<(), *mut u8> {
        if self.count >= MAGAZINE_CAPACITY {
            return Err(ptr);
        }
        self.slots[self.count] = ptr;
        self.count += 1;
        Ok(())
    }

    /// Pop a pointer from the magazine.  Returns `None` if empty.
    #[inline]
    pub fn pop(&mut self) -> Option<*mut u8> {
        if self.count == 0 {
            return None;
        }
        self.count -= 1;
        Some(self.slots[self.count])
    }
}

// ---------------------------------------------------------------------------
// MagazineDepot
// ---------------------------------------------------------------------------

/// Shared depot of full and empty magazines, one per size-class.
///
/// CPUs exchange their exhausted/filled magazines here under the depot lock.
///
/// ## Lock-discipline contract (Phase 57b G.9, host-test-only)
///
/// `full` and `empty` are `spin::Mutex` on every build target.  See the
/// module-level "Phase 57b G.9 — preempt-discipline classification" section
/// for the full rationale.  In short:
///
/// - **Host build** — `cargo test -p kernel-core` uses these locks directly.
/// - **Kernel build** — the kernel-side caller (`kernel/src/mm/slab.rs`) is
///   responsible for wrapping every depot operation in an interrupt-masked
///   region (`without_interrupts`, IrqSafeMutex, or an explicit
///   `preempt_disable` / `preempt_enable` pair).  `MagazineDepot` itself does
///   not call `preempt_disable` because `kernel-core` is `no_std` and cannot
///   reach `kernel::task::scheduler` from the host build.
///
/// Source ref: `docs/handoffs/57b-spinlock-callsite-audit.md` row
/// `kernel-core/src/magazine.rs:?` (classification: `host-test-only`).
pub struct MagazineDepot {
    full: Mutex<Vec<Magazine>>,
    empty: Mutex<Vec<Magazine>>,
}

impl Default for MagazineDepot {
    fn default() -> Self {
        Self::new()
    }
}

impl MagazineDepot {
    /// Create an empty depot.
    pub fn new() -> Self {
        Self {
            full: Mutex::new(Vec::new()),
            empty: Mutex::new(Vec::new()),
        }
    }

    /// Exchange an empty magazine for a full one from the depot.
    ///
    /// On success the caller receives a full magazine and the depot keeps the
    /// empty one for later reuse.  Returns `Err(empty_mag)` if no full
    /// magazines are available.
    #[allow(clippy::result_large_err)]
    pub fn exchange_empty_for_full(&self, empty_mag: Magazine) -> Result<Magazine, Magazine> {
        debug_assert!(empty_mag.is_empty());
        let mut fulls = self.full.lock();
        if let Some(full_mag) = fulls.pop() {
            drop(fulls);
            self.empty.lock().push(empty_mag);
            Ok(full_mag)
        } else {
            Err(empty_mag)
        }
    }

    /// Exchange a full magazine for an empty one from the depot.
    ///
    /// On success the caller receives an empty magazine and the depot keeps
    /// the full one for later distribution.  Returns `Err(full_mag)` if no
    /// empty magazines are available.
    #[allow(clippy::result_large_err)]
    pub fn exchange_full_for_empty(&self, full_mag: Magazine) -> Result<Magazine, Magazine> {
        debug_assert!(full_mag.is_full());
        let mut empties = self.empty.lock();
        if let Some(empty_mag) = empties.pop() {
            drop(empties);
            self.full.lock().push(full_mag);
            Ok(empty_mag)
        } else {
            Err(full_mag)
        }
    }

    /// Drain every full magazine through `f`, returning each emptied magazine to
    /// the depot's empty stack for reuse.
    ///
    /// The callback must consume every object from the magazine so the returned
    /// magazine is truly empty before it is parked for future exchanges.
    pub fn drain_full(&self, mut f: impl FnMut(&mut Magazine)) -> usize {
        let mut drained = 0usize;
        loop {
            let mut mag = {
                let mut fulls = self.full.lock();
                match fulls.pop() {
                    Some(mag) => mag,
                    None => break,
                }
            };
            f(&mut mag);
            debug_assert!(mag.is_empty(), "drain_full callback must empty magazine");
            self.empty.lock().push(mag);
            drained += 1;
        }
        drained
    }

    /// Number of full magazines currently in the depot.
    pub fn full_count(&self) -> usize {
        self.full.lock().len()
    }

    /// Number of empty magazines currently in the depot.
    pub fn empty_count(&self) -> usize {
        self.empty.lock().len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Magazine push / pop basics ------------------------------------------

    #[test]
    fn push_pop_lifo_order() {
        let mut mag = Magazine::new();
        let ptrs: Vec<*mut u8> = (1..=5).map(|i| i as *mut u8).collect();

        for &p in &ptrs {
            mag.push(p).unwrap();
        }
        assert_eq!(mag.len(), 5);

        // LIFO: last pushed is first popped.
        for &p in ptrs.iter().rev() {
            assert_eq!(mag.pop(), Some(p));
        }
        assert!(mag.is_empty());
    }

    #[test]
    fn empty_magazine_pop_returns_none() {
        let mut mag = Magazine::new();
        assert!(mag.is_empty());
        assert_eq!(mag.pop(), None);
    }

    #[test]
    fn full_magazine_push_returns_err() {
        let mut mag = Magazine::new();
        for i in 0..MAGAZINE_CAPACITY {
            mag.push((i + 1) as *mut u8).unwrap();
        }
        assert!(mag.is_full());
        let extra = 0xFF as *mut u8;
        assert_eq!(mag.push(extra), Err(extra));
    }

    #[test]
    fn full_and_empty_detection() {
        let mut mag = Magazine::new();
        assert!(mag.is_empty());
        assert!(!mag.is_full());

        for i in 0..MAGAZINE_CAPACITY {
            mag.push(i as *mut u8).unwrap();
        }
        assert!(mag.is_full());
        assert!(!mag.is_empty());

        mag.pop();
        assert!(!mag.is_full());
        assert!(!mag.is_empty());
    }

    // -- MagazineDepot exchange ----------------------------------------------

    fn make_full_magazine() -> Magazine {
        let mut mag = Magazine::new();
        for i in 0..MAGAZINE_CAPACITY {
            mag.push((i + 1) as *mut u8).unwrap();
        }
        mag
    }

    #[test]
    fn depot_exchange_empty_for_full() {
        let depot = MagazineDepot::new();

        // Deposit a full magazine directly so there is one available.
        depot.full.lock().push(make_full_magazine());
        assert_eq!(depot.full_count(), 1);

        let empty = Magazine::new();
        let result = depot.exchange_empty_for_full(empty);
        assert!(result.is_ok());

        let full = result.unwrap();
        assert!(full.is_full());
        // The empty magazine should now be in the empty stack.
        assert_eq!(depot.empty_count(), 1);
        assert_eq!(depot.full_count(), 0);
    }

    #[test]
    fn depot_exchange_full_for_empty() {
        let depot = MagazineDepot::new();

        // Deposit an empty magazine so there is one available.
        depot.empty.lock().push(Magazine::new());
        assert_eq!(depot.empty_count(), 1);

        let full = make_full_magazine();
        let result = depot.exchange_full_for_empty(full);
        assert!(result.is_ok());

        let empty = result.unwrap();
        assert!(empty.is_empty());
        assert_eq!(depot.full_count(), 1);
        assert_eq!(depot.empty_count(), 0);
    }

    #[test]
    fn depot_exchange_fails_when_none_available() {
        let depot = MagazineDepot::new();

        // No full magazines → exchange_empty_for_full fails.
        let empty = Magazine::new();
        let result = depot.exchange_empty_for_full(empty);
        assert!(result.is_err());

        // No empty magazines → exchange_full_for_empty fails.
        let full = make_full_magazine();
        let result = depot.exchange_full_for_empty(full);
        assert!(result.is_err());
    }

    #[test]
    fn depot_multiple_exchanges_round_trip() {
        let depot = MagazineDepot::new();

        // Seed depot with 3 full and 2 empty magazines.
        for _ in 0..3 {
            depot.full.lock().push(make_full_magazine());
        }
        for _ in 0..2 {
            depot.empty.lock().push(Magazine::new());
        }
        assert_eq!(depot.full_count(), 3);
        assert_eq!(depot.empty_count(), 2);

        // Exchange empty-for-full twice.
        for _ in 0..2 {
            let e = Magazine::new();
            let f = depot.exchange_empty_for_full(e).unwrap();
            assert!(f.is_full());
        }
        // full: 1 remaining, empty: 2 + 2 = 4.
        assert_eq!(depot.full_count(), 1);
        assert_eq!(depot.empty_count(), 4);

        // Exchange full-for-empty.
        let f = make_full_magazine();
        let e = depot.exchange_full_for_empty(f).unwrap();
        assert!(e.is_empty());
        assert_eq!(depot.full_count(), 2);
        assert_eq!(depot.empty_count(), 3);
    }

    #[test]
    fn depot_drain_full_magazines_returns_empties() {
        let depot = MagazineDepot::new();
        depot.full.lock().push(make_full_magazine());
        depot.full.lock().push(make_full_magazine());

        let mut drained_objects = 0usize;
        let drained = depot.drain_full(|mag| {
            while mag.pop().is_some() {
                drained_objects += 1;
            }
        });

        assert_eq!(drained, 2);
        assert_eq!(drained_objects, 2 * MAGAZINE_CAPACITY);
        assert_eq!(depot.full_count(), 0);
        assert_eq!(depot.empty_count(), 2);
    }
}
