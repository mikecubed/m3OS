//! Lock-free per-task CPU-time and rusage counters.
//!
//! ## Why this exists
//!
//! Phase 61 Tracks E.2 (per-tick CS sampling) and E.4 (per-CoW-fault
//! accounting) originally mutated four `u64` fields directly on
//! [`crate::task::Task`] from the timer-IRQ and page-fault handlers. Even
//! after the `7785bb5` fix routed those handlers through `try_scheduler_lock`
//! (so they no longer deadlock), every successful try-lock briefly held
//! `SCHEDULER_INNER` from interrupt context. With four cores doing this on
//! every tick (BSP at 1 kHz, APs at 100 Hz) the contention competed with the
//! IPC wake/dispatch path and surfaced as `vfs_server: slow req` warnings of
//! 50–130 ms. Diagnostic A/B (`diag/61-disable-per-tick-accounting`)
//! confirmed: disabling the per-tick + per-fault helpers eliminated 226 of
//! 242 slow_req warnings under the same Doom-load workload.
//!
//! This module replaces the in-Task counters with a dedicated static array
//! of atomics, indexed by the task's scheduler-vec slot. The four mutators
//! that used to take a scheduler lock are now lock-free `fetch_add` calls;
//! the read-side aggregators in [`crate::task::scheduler`] still run under
//! the scheduler lock (because they iterate `tasks` to match by pid) but no
//! longer race against the IRQ writers.
//!
//! ## Sizing
//!
//! 256 slots × 32 bytes (4 × `AtomicU64`) = 8 KiB in `.bss`. Sized to
//! [`crate::task::MAX_TASKS`] to match every other per-task table in the
//! kernel (`TCB_BOUND_NOTIF`, the kstack pool, etc.).

use core::sync::atomic::{AtomicU64, Ordering};

/// Per-task CPU-time and rusage counters. Each field is incremented from
/// statistical samplers (timer IRQ for ticks, page-fault handler for faults)
/// without any locking.
#[repr(C)]
pub struct TaskRusage {
    /// Wall-clock milliseconds the task has spent in ring 3.
    pub user_ticks: AtomicU64,
    /// Wall-clock milliseconds the task has spent in ring 0.
    pub system_ticks: AtomicU64,
    /// CoW-resolved (no-I/O) page faults charged to this task.
    pub minor_faults: AtomicU64,
    /// Disk-backed page faults. In Phase 61 the disk-backed mmap path is
    /// incomplete, so this stays at 0 in practice; the field is present so
    /// the API surface is stable for `getrusage(2)` consumers.
    pub major_faults: AtomicU64,
}

impl TaskRusage {
    pub const fn zero() -> Self {
        Self {
            user_ticks: AtomicU64::new(0),
            system_ticks: AtomicU64::new(0),
            minor_faults: AtomicU64::new(0),
            major_faults: AtomicU64::new(0),
        }
    }

    /// Reset every counter to zero. Called from the scheduler when a slot in
    /// the tasks vec is reused via the free list (`alloc_task_slot`) so the
    /// new task starts with fresh counters and doesn't inherit the
    /// terminated task's totals.
    pub fn reset(&self) {
        self.user_ticks.store(0, Ordering::Relaxed);
        self.system_ticks.store(0, Ordering::Relaxed);
        self.minor_faults.store(0, Ordering::Relaxed);
        self.major_faults.store(0, Ordering::Relaxed);
    }
}

/// Static counter table — one entry per scheduler task slot.
#[allow(clippy::declare_interior_mutable_const)]
pub static RUSAGE: [TaskRusage; crate::task::MAX_TASKS] = {
    const Z: TaskRusage = TaskRusage::zero();
    [Z; crate::task::MAX_TASKS]
};

/// Borrow the rusage entry for slot `idx`, or `None` if `idx` is out of
/// range. `idx` is the scheduler's tasks-vec position, which is also what
/// `get_current_task_idx()` returns.
#[inline]
pub fn get(idx: usize) -> Option<&'static TaskRusage> {
    RUSAGE.get(idx)
}
