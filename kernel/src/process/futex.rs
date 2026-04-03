//! Futex wait/wake queue infrastructure (Phase 40, Track D).
//!
//! Provides a global futex wait-queue table keyed by `(page_table_root, vaddr)`.
//! Threads block via `FUTEX_WAIT` and are woken by `FUTEX_WAKE`.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Lazy;
use spin::Mutex;

use crate::task::TaskId;

// ---------------------------------------------------------------------------
// Waiter entry
// ---------------------------------------------------------------------------

/// A thread waiting on a futex word.
pub struct FutexWaiter {
    /// Task (thread) ID of the blocked thread.
    pub tid: TaskId,
    /// Bitset used for `FUTEX_WAIT_BITSET` / `FUTEX_WAKE_BITSET` filtering.
    pub bitset: u32,
}

// ---------------------------------------------------------------------------
// Global futex table
// ---------------------------------------------------------------------------

/// Key: `(page_table_root, virtual_address)`.
///
/// For `FUTEX_PRIVATE_FLAG` futexes the page_table_root component is 0,
/// since private futexes are scoped to a single address space and do not
/// require cross-process identity.
type FutexKey = (u64, u64);

/// Global table of futex wait queues, keyed by `(page_table_root, vaddr)`.
pub static FUTEX_TABLE: Lazy<Mutex<BTreeMap<FutexKey, Vec<FutexWaiter>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Bitset that matches any waiter — used implicitly by plain FUTEX_WAIT/WAKE.
pub const FUTEX_BITSET_MATCH_ANY: u32 = 0xFFFF_FFFF;
