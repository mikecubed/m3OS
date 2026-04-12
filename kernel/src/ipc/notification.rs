//! Asynchronous notification objects.
//!
//! A [`Notification`] is a single machine-word bitfield.  Each bit is an
//! independent signal channel.  [`signal_irq`] sets bits atomically from
//! interrupt handlers (ISR-safe, lock-free); [`signal`] may only be called
//! from task context (acquires a mutex to wake the waiter).  The receiver
//! blocks until at least one bit is set, then atomically clears and returns
//! the pending bits.
//!
//! # Pool capacity and ISR-safety constraint
//!
//! The notification pool is **fixed-size** (`MAX_NOTIFS = 64`).  This is a
//! deliberate design choice, not a missing feature:
//!
//! - `PENDING` and `ISR_WAITERS` must be accessible from ISR context using
//!   only lock-free atomics.  A growable `Vec` requires allocation or
//!   reallocation, neither of which is safe in an interrupt handler.
//! - A lock-free growable structure (e.g. a concurrent hash map or RCU-
//!   protected indirection) would add significant complexity for a pool
//!   that currently needs fewer than 10 active slots.
//! - 64 slots comfortably cover the foreseeable notification demand (one
//!   per IRQ line, one per keyboard/timer/network device, plus userspace
//!   IPC notifications).  Exhaustion is diagnosed at runtime.
//!
//! If a future phase requires more than 64 concurrent notification objects,
//! the recommended path is a two-level indirection: a fixed-size ISR-visible
//! fast table backed by a growable overflow pool accessed only from task
//! context.  This is explicitly deferred.
//!
//! # ISR-safety design
//!
//! [`signal_irq`] is called from the keyboard interrupt handler and must not
//! take any spin lock — on a single-CPU kernel a spinlock in an ISR will
//! deadlock if the preempted task happens to hold the same lock.
//!
//! To achieve this, the module separates its state into two layers:
//!
//! - **Lock-free layer** (`PENDING`, `IRQ_MAP`, `ISR_WAITERS`): plain
//!   `AtomicU64`/`AtomicU8`/`AtomicI32` arrays indexed by `NotifId`.
//!   Safe to read/write from interrupt handlers.
//! - **Mutex-protected layer** (`WAITERS`, `ALLOCATED`): holds waiter and
//!   allocation state.  Only accessed from task context, never from
//!   interrupt handlers.
//!
//! [`signal_irq`] exclusively uses the lock-free layer.  It sets bits in
//! `PENDING` and pushes the waiter to the per-core `IsrWakeQueue` (if
//! registered in `ISR_WAITERS`), then calls [`signal_reschedule`] to ensure
//! the waiting task is eventually rescheduled.
//!
//! [`signal`] (used from the `notify_signal` syscall in task context) follows
//! the same lock-free bit-set, then additionally attempts to wake the waiter
//! via the mutex-protected `WAITERS` layer.  Because it runs in task context
//! (with no scheduler lock held), the scheduler-lock acquisition inside
//! [`wake_task`] is safe.
//!
//! # Exhaustion behavior
//!
//! - [`try_create`] returns `None` when all 64 slots are occupied.  A
//!   diagnostic `log::warn` is emitted on exhaustion.
//! - [`create`] panics on exhaustion (used for kernel-internal allocations
//!   where failure is not recoverable).
//! - Userspace-facing syscalls use `try_create` and return an error code.
//!
//! # Typical use: IRQ delivery
//!
//! ```text
//! kbd_server startup:
//!   handle = create_notification()
//!   register_irq(IRQ1, handle)   // kernel: on IRQ1, set bit 1 in PENDING
//!   loop:
//!     bits = notify_wait(handle) // blocks until PENDING[handle] != 0
//!     scancode = in(0x60)
//!     ... process key event ...
//! ```
// Wired: notifications allocated/registered in main.rs; keyboard ISR calls signal_irq(1).
// Keep dead-code allowance for unused APIs.
#![allow(dead_code)]

use core::sync::atomic::{AtomicI32, AtomicU8, AtomicU64, Ordering};
use spin::Mutex;

use crate::task::{TaskId, scheduler};

pub use kernel_core::types::NotifId;

// ---------------------------------------------------------------------------
// Lock-free state (safe for ISR access)
// ---------------------------------------------------------------------------

/// Maximum number of notification objects.
///
/// Fixed at 64 because `PENDING` and `ISR_WAITERS` must be accessible from
/// ISR context using only lock-free atomics.  A growable pool would require
/// allocation or lock-based indirection, neither of which is ISR-safe.
/// See the module-level doc for the full rationale and deferred design.
pub(super) const MAX_NOTIFS: usize = 64;

/// Per-notification pending bitfields.
///
/// `PENDING[i]` holds the accumulated unread bits for notification `i`.
/// Written by [`signal`] / [`signal_irq`] (lock-free); drained by [`wait`].
///
/// Must remain a fixed-size array — ISR context cannot acquire locks or
/// allocate, so dynamic `Vec` is not an option.
#[allow(clippy::declare_interior_mutable_const)]
static PENDING: [AtomicU64; MAX_NOTIFS] = {
    const ZERO: AtomicU64 = AtomicU64::new(0);
    [ZERO; MAX_NOTIFS]
};

/// Lock-free mapping from hardware IRQ line (0–15) to `NotifId`.
///
/// `0xff` means the IRQ line is not registered.  Written once at boot (before
/// the IRQ line is enabled) by [`register_irq`]; read from the keyboard ISR.
static IRQ_MAP: [AtomicU8; 16] = [
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
    AtomicU8::new(0xff),
];

// ---------------------------------------------------------------------------
// Lock-free ISR waiter mirror (Phase 52: ISR-direct wakeup)
// ---------------------------------------------------------------------------

/// Lock-free mirror of WAITERS holding only the task *index* (into the
/// scheduler's task vec) for each notification slot.
///
/// -1 means no waiter is registered.  Written from task context when `wait()`
/// registers a waiter; read from ISR context by `signal_irq()` to push the
/// waiter into the per-core `IsrWakeQueue` without touching any mutex.
///
/// The task index (not `TaskId`) is stored because the scheduler needs it for
/// direct state manipulation without a linear search.
#[allow(clippy::declare_interior_mutable_const)]
static ISR_WAITERS: [AtomicI32; MAX_NOTIFS] = {
    const NO_WAITER: AtomicI32 = AtomicI32::new(-1);
    [NO_WAITER; MAX_NOTIFS]
};

// ---------------------------------------------------------------------------
// Mutex-protected waiter state (task context only)
// ---------------------------------------------------------------------------

/// Per-notification waiter.
///
/// `WAITERS[i]` is `Some(task_id)` when a task is blocked in [`wait`] on
/// notification `i`.  Protected by a `Mutex` — only accessed in task context,
/// never from an interrupt handler.
static WAITERS: Mutex<[Option<TaskId>; MAX_NOTIFS]> = Mutex::new([None; MAX_NOTIFS]);

// ---------------------------------------------------------------------------
// Allocation registry
// ---------------------------------------------------------------------------

/// Tracks which notification slots are allocated.
static ALLOCATED: Mutex<[bool; MAX_NOTIFS]> = Mutex::new([false; MAX_NOTIFS]);

/// Return the notification pool capacity.
pub fn capacity() -> usize {
    MAX_NOTIFS
}

/// Return the number of currently allocated notification slots.
pub fn allocated_count() -> usize {
    ALLOCATED.lock().iter().filter(|&&b| b).count()
}

// ---------------------------------------------------------------------------
// Public API (used from kernel and ipc/mod.rs dispatch)
// ---------------------------------------------------------------------------

/// Allocate a new notification object and return its [`NotifId`].
///
/// # Panics
///
/// Panics if all 64 slots are occupied (in both debug and release builds).
pub fn create() -> NotifId {
    try_create().expect("notification registry full")
}

/// Fallible version of [`create`] — returns `None` when all slots are
/// occupied instead of panicking.  Used by userspace-facing syscalls to
/// avoid kernel DoS via notification exhaustion.
pub fn try_create() -> Option<NotifId> {
    let mut alloc = ALLOCATED.lock();
    for (i, slot) in alloc.iter_mut().enumerate() {
        if !*slot {
            *slot = true;
            // Warn once when pool usage crosses the 75% threshold.
            let in_use = alloc.iter().filter(|&&b| b).count();
            if in_use == MAX_NOTIFS * 3 / 4 {
                log::warn!(
                    "[ipc::notification] pool at {}/{} (75% threshold crossed)",
                    in_use,
                    MAX_NOTIFS,
                );
            }
            return Some(NotifId(i as u8));
        }
    }
    log::warn!(
        "[ipc::notification] pool exhausted ({}/{} slots in use)",
        MAX_NOTIFS,
        MAX_NOTIFS
    );
    None
}

/// Free a notification slot so it can be reused.
///
/// Used to roll back a `try_create` when the subsequent capability insert
/// fails, preventing permanent slot leaks from userspace syscalls.
pub fn free(id: NotifId) {
    let mut alloc = ALLOCATED.lock();
    if let Some(slot) = alloc.get_mut(id.0 as usize) {
        *slot = false;
    }
}

/// Remove an IRQ→notification mapping, resetting the IRQ line to unregistered.
///
/// Used to roll back a `register_irq` when the subsequent capability insert
/// fails, preventing IRQ misrouting.
pub fn unregister_irq(irq: u8) {
    if (irq as usize) < IRQ_MAP.len() {
        IRQ_MAP[irq as usize].store(0xff, Ordering::Release);
    }
}

/// Check whether an IRQ line already has a notification registered.
///
/// Returns `true` if the IRQ line has a registered notification (i.e. not
/// `0xff`).  Used by `create_irq_notification` to enforce exclusive
/// registration — only one notification per IRQ line.
pub fn is_irq_registered(irq: u8) -> bool {
    IRQ_MAP
        .get(irq as usize)
        .is_some_and(|a| a.load(Ordering::Acquire) != 0xff)
}

/// Register an IRQ number to signal a notification on each delivery.
///
/// `irq` is the hardware IRQ line (0 = timer, 1 = keyboard, …).
/// Must be called with interrupts **disabled** or before the IRQ line is
/// unmasked, to avoid a race where the ISR fires before `IRQ_MAP` is updated.
pub fn register_irq(irq: u8, notif_id: NotifId) {
    if (irq as usize) < IRQ_MAP.len() {
        IRQ_MAP[irq as usize].store(notif_id.0, Ordering::Release);
    }
}

/// Atomically register an IRQ line if it is currently unregistered.
///
/// Returns `true` on success, `false` if the IRQ line already has a
/// notification mapped.  Uses `compare_exchange` to prevent cross-core
/// races where two concurrent callers both pass the `is_irq_registered`
/// check and overwrite each other.
pub fn try_register_irq(irq: u8, notif_id: NotifId) -> bool {
    match IRQ_MAP.get(irq as usize) {
        Some(slot) => slot
            .compare_exchange(0xff, notif_id.0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok(),
        None => false,
    }
}

/// Deliver a hardware IRQ to its registered notification object.
///
/// **ISR-safe** — uses only lock-free atomics and does not call `wake_task`.
/// If a waiter is registered in `ISR_WAITERS`, its task index is pushed to
/// the current core's `IsrWakeQueue` so the scheduler can wake it on the
/// next loop iteration (sub-ms latency).  If the ISR wake queue is full or
/// the waiter was not yet registered, the fallback `drain_pending_waiters()`
/// on the BSP will catch it on the next tick (~10 ms).
pub fn signal_irq(irq: u8) {
    let idx = IRQ_MAP
        .get(irq as usize)
        .map(|a| a.load(Ordering::Acquire))
        .unwrap_or(0xff);
    if idx == 0xff {
        return;
    }
    // Set the bit for this IRQ line atomically (lock-free).
    if let Some(pending) = PENDING.get(idx as usize) {
        pending.fetch_or(1u64 << (irq as u32), Ordering::Release);
    }

    // Phase 52: push waiter to per-core ISR wakeup queue (lock-free).
    // Use compare_exchange to atomically claim the waiter, preventing
    // duplicate pushes when multiple IRQs arrive before the task wakes.
    if let Some(isr_waiter) = ISR_WAITERS.get(idx as usize) {
        let waiter_idx = isr_waiter.load(Ordering::Acquire);
        if waiter_idx >= 0
            && isr_waiter
                .compare_exchange(waiter_idx, -1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            && let Some(data) = crate::smp::try_per_core()
        {
            let _ = data.isr_wake_queue.push(waiter_idx as usize);
        }
    }

    // Trigger a reschedule so the blocked task runs on the next tick and
    // drains the pending bits from its wait() loop.
    scheduler::signal_reschedule();
    // Do NOT call wake_task() — that acquires SCHEDULER.lock() which is not
    // safe from ISR context on a single-CPU kernel.
}

/// Signal one or more bits on a notification object.
///
/// **Task-context safe** (not ISR-safe — may call [`wake_task`]).
/// If called from an interrupt handler, use [`signal_irq`] instead.
pub fn signal(notif_id: NotifId, bits: u64) {
    let idx = notif_id.0 as usize;
    if idx >= MAX_NOTIFS {
        return;
    }
    // Set pending bits (lock-free).
    PENDING[idx].fetch_or(bits, Ordering::Release);

    // Wake the waiter (if any) via the mutex-protected layer.
    // Safe to call here because signal() only runs in task context
    // (syscall 8 — notify_signal), where the scheduler lock is never
    // already held by the calling task.
    let waiter = {
        let mut waiters = WAITERS.lock();
        let w = waiters[idx].take();
        // Clear the ISR mirror when taking the waiter.
        if w.is_some() {
            ISR_WAITERS[idx].store(-1, Ordering::Release);
        }
        w
    };
    if let Some(task) = waiter {
        let _ = scheduler::wake_task(task);
    }
    // Also trigger reschedule in case the waiter wasn't in WAITERS yet
    // (it may be between the swap(0) check and the waiter registration).
    scheduler::signal_reschedule();
}

/// Clear a dying task from all notification waiter slots.
///
/// Called during task cleanup to ensure the dying task is not left as a
/// waiter on any notification object.  Without this, a subsequent
/// `signal()` would attempt to wake a dead task.
pub fn clear_waiter(task_id: TaskId) {
    let mut waiters = WAITERS.lock();
    for (i, slot) in waiters.iter_mut().enumerate() {
        if *slot == Some(task_id) {
            *slot = None;
            // Also clear the ISR mirror so signal_irq doesn't push a dead task.
            ISR_WAITERS[i].store(-1, Ordering::Release);
        }
    }
}

/// Scan all notifications and wake any task whose PENDING bits are non-zero.
///
/// Called from the scheduler loop in task context (after interrupts are
/// re-enabled, before `pick_next`).  `signal_irq` sets `PENDING` bits
/// and calls `signal_reschedule`, but it cannot call `wake_task` (not
/// ISR-safe).  This function closes the gap: on each scheduler tick it
/// transitions any `BlockedOnNotif` waiter with pending bits to `Ready`.
///
/// Safe to call here because `wake_task` is task-context-only and the
/// scheduler lock is not held when this runs.
pub fn drain_pending_waiters() {
    for idx in 0..MAX_NOTIFS {
        // Fast path: no pending bits → skip without acquiring any lock.
        if PENDING[idx].load(Ordering::Acquire) == 0 {
            continue;
        }
        // Pending bits exist — check for a blocked waiter.
        let waiter = {
            let mut waiters = WAITERS.lock();
            // Re-check under the lock to close the TOCTOU window where
            // wait() may have drained PENDING between our load and here.
            if PENDING[idx].load(Ordering::Acquire) == 0 {
                None
            } else {
                let w = waiters[idx].take();
                // Clear the ISR mirror when taking the waiter.
                if w.is_some() {
                    ISR_WAITERS[idx].store(-1, Ordering::Release);
                }
                w
            }
        };
        if let Some(task) = waiter {
            let _ = scheduler::wake_task(task);
        }
    }
}

/// Wait for any bit to be set on a notification object.
///
/// If bits are already pending, clears and returns them immediately (no
/// blocking).  Otherwise registers as the waiter and blocks via
/// `block_current_on_notif` until [`signal`] or [`signal_irq`] fires.
///
/// Returns the bits that were pending (non-zero on success), or 0 on error.
pub fn wait(waiter: TaskId, notif_id: NotifId) -> u64 {
    // Verify the caller is blocking itself, not a foreign task.  Passing a
    // mismatched TaskId would record the wrong ID in WAITERS while the
    // actual current task blocks forever on block_current_on_notif().
    debug_assert_eq!(
        Some(waiter),
        scheduler::current_task_id(),
        "[ipc] notification::wait: waiter TaskId does not match current task"
    );

    let idx = notif_id.0 as usize;
    if idx >= MAX_NOTIFS {
        return 0;
    }

    loop {
        // Fast path: drain any pending bits.
        let bits = PENDING[idx].swap(0, Ordering::Acquire);
        if bits != 0 {
            return bits;
        }

        // No bits pending — register as waiter, then double-check before
        // blocking to close the TOCTOU window: a signal might have arrived
        // between the swap(0) above and acquiring WAITERS.
        {
            let mut waiters = WAITERS.lock();
            let bits2 = PENDING[idx].swap(0, Ordering::Acquire);
            if bits2 != 0 {
                // Signal arrived in the window — return without blocking.
                return bits2;
            }
            // Single-waiter design: assert no other task is already waiting.
            debug_assert!(
                waiters[idx].is_none(),
                "[ipc] notify wait: two tasks waiting on the same notification (notif_id={:?})",
                notif_id
            );
            waiters[idx] = Some(waiter);

            // Phase 52: populate ISR mirror with the task's scheduler index
            // so signal_irq() can push it to the per-core wakeup queue
            // without acquiring any lock.
            if let Some(task_idx) = scheduler::get_current_task_idx() {
                ISR_WAITERS[idx].store(task_idx as i32, Ordering::Release);
            }
        }
        // Release WAITERS lock before blocking; signal() can now wake us.

        // Block using the dedicated notification state.
        scheduler::block_current_on_notif();
        // On wake, loop back to drain pending bits.
    }
}
