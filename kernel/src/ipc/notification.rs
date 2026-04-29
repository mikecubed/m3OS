//! Asynchronous notification objects.
//!
//! A [`Notification`] is a single machine-word bitfield.  Each bit is an
//! independent signal channel.  [`signal_irq`] sets bits atomically from
//! interrupt handlers (ISR-safe, lock-free); [`signal`] may only be called
//! from task context (acquires a mutex to wake the waiter).  The receiver
//! blocks until at least one bit is set, then atomically clears and returns
//! the pending bits.
//!
//! # Phase 57 — audio reuses the same path
//!
//! `audio_server` (the AC'97 ring-3 driver, Phase 57 Track D) consumes
//! IRQs through this module's [`signal_irq_bit`] entry point exactly the
//! way the existing `e1000_driver` and `nvme_driver` do — bind a
//! `Notification` to the device IRQ via `sys_device_irq_subscribe`
//! (Phase 55b Track B.4), then `recv_msg_with_notif` returns
//! `WakeKind::Notification(bits)` when the audio controller fires its
//! buffer-empty IRQ. The kernel does not learn AC'97 specifics; the path
//! described here covers every ring-3 driver. See Phase 57 Track C.3
//! acceptance and `docs/appendix/phase-57-audio-abi.md` for the
//! pure-userspace IPC choice that makes this collapse possible.
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
//! - **Lock-free layer** (`PENDING`, `IRQ_MAP`, `ISR_WAITERS`, `BOUND_TCB`,
//!   `TCB_BOUND_NOTIF`): plain `AtomicU64`/`AtomicU8`/`AtomicI32` arrays
//!   indexed by `NotifId` or scheduler task index. Safe to read/write from
//!   interrupt handlers.
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

/// Maximum number of task scheduler-Vec entries tracked by [`TCB_BOUND_NOTIF`].
///
/// Must cover the maximum scheduler-task-Vec index. Entries beyond this bound
/// are silently treated as "no binding" at fast-path lookup time.
pub(crate) const MAX_TASKS: usize = crate::task::MAX_TASKS;

/// Sentinel: no task is bound to this notification slot.
const TCB_NONE: i32 = -1;

/// Sentinel: no notification is bound to this task slot.
const NOTIF_NONE: u8 = 0xff;

// ---------------------------------------------------------------------------
// Bound-notification lookup tables (Track B — ISR-safe, lock-free)
// ---------------------------------------------------------------------------

/// Persistent TCB binding table.
///
/// `BOUND_TCB[notif_idx]` = scheduler task-Vec index of the task that called
/// `sys_notif_bind` for this notification, or `TCB_NONE` (-1) if unbound.
///
/// **ISR-safety**: reads via `load(Acquire)` in `signal_irq` /
/// `signal_irq_bit` without acquiring any lock — safe from interrupt context.
///
/// **Lock order**: BOUND_TCB is lock-free and imposes no ordering constraint.
/// When both the endpoint lock and the notification WAITERS lock must be held,
/// endpoint must be acquired first (endpoint → WAITERS).
///
/// Parallel to `ISR_WAITERS` but **persistent**: set at bind time by
/// `sys_notif_bind` and cleared only at task exit or explicit unbind.
#[allow(clippy::declare_interior_mutable_const)]
static BOUND_TCB: [AtomicI32; MAX_NOTIFS] = {
    const NONE: AtomicI32 = AtomicI32::new(TCB_NONE);
    [NONE; MAX_NOTIFS]
};

/// Per-task notification binding index.
///
/// `TCB_BOUND_NOTIF[task_sched_idx]` = `NotifId.0` of the notification bound
/// to that task, or `NOTIF_NONE` (0xff) if the task has no bound notification.
///
/// **ISR-safety**: reads via `load(Acquire)` — safe from interrupt context.
/// Written by `bind_task` / `clear_bound_task` only from task context.
#[allow(clippy::declare_interior_mutable_const)]
pub(crate) static TCB_BOUND_NOTIF: [AtomicU8; MAX_TASKS] = {
    const NONE: AtomicU8 = AtomicU8::new(NOTIF_NONE);
    [NONE; MAX_TASKS]
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

/// Fully release a notification object held by a dying task.
///
/// This clears any waiter state, drops IRQ mappings that still point at this
/// notification, resets pending bits, and returns the slot to the allocator.
pub fn release(id: NotifId) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let idx = id.0 as usize;
        if idx >= MAX_NOTIFS {
            return;
        }

        let bound = BOUND_TCB[idx].swap(TCB_NONE, Ordering::AcqRel);
        if bound >= 0 {
            let task_idx = bound as usize;
            if task_idx < MAX_TASKS {
                let _ = TCB_BOUND_NOTIF[task_idx].compare_exchange(
                    id.0,
                    NOTIF_NONE,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                );
            }
        }

        for slot in &IRQ_MAP {
            let _ = slot.compare_exchange(id.0, 0xff, Ordering::AcqRel, Ordering::Acquire);
        }

        {
            let mut waiters = WAITERS.lock();
            waiters[idx] = None;
        }
        ISR_WAITERS[idx].store(-1, Ordering::Release);
        PENDING[idx].store(0, Ordering::Release);

        free(id);
    });
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
    let mut pushed_waiter_idx = -1;
    if let Some(isr_waiter) = ISR_WAITERS.get(idx as usize) {
        let waiter_idx = isr_waiter.load(Ordering::Acquire);
        if waiter_idx >= 0
            && isr_waiter
                .compare_exchange(waiter_idx, -1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            && let Some(data) = crate::smp::try_per_core()
            && data.isr_wake_queue.push(waiter_idx as usize)
        {
            pushed_waiter_idx = waiter_idx;
        }
    }

    // Also wake any task parked in recv_msg with this notification bound.
    // BOUND_TCB is persistent (not cleared by the push), so we just push
    // without CAS-clearing — the ISR drain handles non-BlockedOnNotif tasks
    // gracefully (no-op).
    let bound = BOUND_TCB[idx as usize].load(Ordering::Acquire);
    if bound >= 0
        && bound != pushed_waiter_idx
        && let Some(data) = crate::smp::try_per_core()
    {
        let _ = data.isr_wake_queue.push(bound as usize);
    }

    // Trigger a reschedule so the blocked task runs on the next tick and
    // drains the pending bits from its wait() loop.
    scheduler::signal_reschedule();
    // Do NOT call wake_task() — that acquires SCHEDULER.lock() which is not
    // safe from ISR context on a single-CPU kernel.
}

/// Signal a single bit on a `NotifId` from interrupt context.
///
/// **ISR-safe** — uses only `AtomicU64::fetch_or` and a per-core `push` on
/// the lock-free `IsrWakeQueue`. No allocation, no mutex, no IPC. This is
/// the lower-level sibling of [`signal_irq`] used by the Phase 55b device
/// IRQ subscription path (`sys_device_irq_subscribe` → MSI / MSI-X / INTx
/// → this function) where the kernel needs to deliver to an arbitrary
/// `NotifId` rather than the legacy IRQ-line indirection in `IRQ_MAP`.
///
/// Callers (existing): `nvme_driver`, `e1000_driver`, and (Phase 57)
/// `audio_server` — every ring-3 driver consumes its IRQ vector through
/// this entry point. The function is device-class agnostic; the audio
/// path adds no new code, only a new caller. The IRQ handler installed by
/// `sys_device_irq_subscribe` does the minimum (read status, signal the
/// bit, EOI) — no allocation, no IPC, no blocking. See the module-level
/// docs for the Phase 57 audio reuse note.
///
/// `bit` must be < 64 — callers validate at the syscall boundary so the
/// ISR path is branchless past the allocation. An out-of-range bit is
/// silently dropped here (preserving ISR-safety: we cannot `panic!` from
/// interrupt context).
pub fn signal_irq_bit(notif_id: NotifId, bit: u8) {
    let idx = notif_id.0 as usize;
    if idx >= MAX_NOTIFS || bit >= 64 {
        return;
    }
    // Set the requested bit atomically. `fetch_or` is commutative, so
    // concurrent ISRs targeting the same notification accumulate bits
    // without loss.
    PENDING[idx].fetch_or(1u64 << (bit as u32), Ordering::Release);

    // Push the waiter (if any) to the per-core ISR wake queue — mirrors
    // the logic in `signal_irq` but without the `IRQ_MAP` indirection.
    let mut pushed_waiter_idx = -1;
    if let Some(isr_waiter) = ISR_WAITERS.get(idx) {
        let waiter_idx = isr_waiter.load(Ordering::Acquire);
        if waiter_idx >= 0
            && isr_waiter
                .compare_exchange(waiter_idx, -1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            && let Some(data) = crate::smp::try_per_core()
            && data.isr_wake_queue.push(waiter_idx as usize)
        {
            pushed_waiter_idx = waiter_idx;
        }
    }

    // Also wake any task parked in recv_msg_with_notif with this notification bound.
    let bound = BOUND_TCB[idx].load(Ordering::Acquire);
    if bound >= 0
        && bound != pushed_waiter_idx
        && let Some(data) = crate::smp::try_per_core()
    {
        let _ = data.isr_wake_queue.push(bound as usize);
    }

    scheduler::signal_reschedule();
}

/// Test-only accessor for the pending-bit word on a notification.
///
/// Returns the current `PENDING[idx]` value without draining it. Used by
/// the Phase 55b Track B.4 synthetic-IRQ test to inspect what the ISR
/// shim delivered without disturbing the waiter-wake state.
#[cfg(test)]
pub fn test_peek_pending(idx: u8) -> u64 {
    let i = idx as usize;
    if i >= MAX_NOTIFS {
        return 0;
    }
    PENDING[i].load(Ordering::Acquire)
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
        // D.3: route through wake_task_v2 under sched-v2 so all wake paths
        // use the CAS primitive and no v1 deferred-enqueue flag is set.
        {
            let _ = scheduler::wake_task_v2(task);
        }
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

/// Bind `notif_id` to the task at `task_sched_idx`.
///
/// Updates both `BOUND_TCB[notif_idx]` and `TCB_BOUND_NOTIF[task_sched_idx]`.
///
/// - Returns `Ok(())` if newly bound or already bound to the same task
///   (idempotent).
/// - Returns `Err(())` if the notification is already bound to a different
///   task or the task is already bound to a different notification.
///
/// Task-context only (may not be called from ISR).
pub(super) fn bind_task(notif_id: NotifId, task_sched_idx: usize) -> Result<(), ()> {
    let notif_idx = notif_id.0 as usize;
    if notif_idx >= MAX_NOTIFS || task_sched_idx >= MAX_TASKS {
        return Err(());
    }
    let new_val = task_sched_idx as i32;

    match TCB_BOUND_NOTIF[task_sched_idx].compare_exchange(
        NOTIF_NONE,
        notif_id.0,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {}
        Err(existing) if existing == notif_id.0 => {}
        Err(_) => {
            return Err(());
        }
    }

    match BOUND_TCB[notif_idx].compare_exchange(
        TCB_NONE,
        new_val,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {}
        Err(existing) if existing == new_val => {}
        Err(_) => {
            let _ = TCB_BOUND_NOTIF[task_sched_idx].compare_exchange(
                notif_id.0,
                NOTIF_NONE,
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
            return Err(());
        }
    }

    Ok(())
}

/// Clear the binding for a dying task.
///
/// Called during task cleanup (`cleanup_task_ipc`). ISR-safe to call because
/// all writes are lock-free atomics; the caller need not hold any lock.
pub(crate) fn clear_bound_task(task_sched_idx: usize) {
    if task_sched_idx >= MAX_TASKS {
        return;
    }
    let notif_idx_byte = TCB_BOUND_NOTIF[task_sched_idx].swap(NOTIF_NONE, Ordering::AcqRel);
    if notif_idx_byte == NOTIF_NONE {
        return;
    }
    let notif_idx = notif_idx_byte as usize;
    if notif_idx < MAX_NOTIFS {
        let _ = BOUND_TCB[notif_idx].compare_exchange(
            task_sched_idx as i32,
            TCB_NONE,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }
}

/// Look up the notification bound to a task.
///
/// Returns `Some(NotifId)` if the task at `task_sched_idx` has a bound
/// notification, or `None` otherwise. ISR-safe (lock-free read).
pub(super) fn lookup_bound_notif(task_sched_idx: usize) -> Option<NotifId> {
    if task_sched_idx >= MAX_TASKS {
        return None;
    }
    let val = TCB_BOUND_NOTIF[task_sched_idx].load(Ordering::Acquire);
    if val == NOTIF_NONE {
        None
    } else {
        Some(NotifId(val))
    }
}

/// Atomically drain pending bits from a notification.
///
/// Returns the bits that were pending (0 if none). ISR-safe.
pub(super) fn drain_bits(notif_id: NotifId) -> u64 {
    let idx = notif_id.0 as usize;
    if idx >= MAX_NOTIFS {
        return 0;
    }
    PENDING[idx].swap(0, Ordering::AcqRel)
}

/// Register a task as the current recv waiter for a bound notification.
///
/// Returns `Some(bits)` if a notification arrived in the registration window,
/// in which case the caller must not block. Returns `None` once registration
/// completed and the task may safely park.
pub(super) fn register_recv_waiter(
    notif_id: NotifId,
    receiver: TaskId,
    task_sched_idx: usize,
) -> Option<u64> {
    let idx = notif_id.0 as usize;
    if idx >= MAX_NOTIFS {
        return Some(0);
    }

    let mut waiters = WAITERS.lock();
    let bits = PENDING[idx].swap(0, Ordering::AcqRel);
    if bits != 0 {
        return Some(bits);
    }

    debug_assert!(
        waiters[idx].is_none(),
        "[ipc] recv_msg_with_notif: two tasks waiting on same notification {idx}"
    );
    waiters[idx] = Some(receiver);
    if task_sched_idx < MAX_TASKS {
        ISR_WAITERS[idx].store(task_sched_idx as i32, Ordering::Release);
    }
    None
}

/// Unregister a task from the recv waiter slot for a bound notification.
pub(super) fn unregister_recv_waiter(notif_id: NotifId, receiver: TaskId) {
    let idx = notif_id.0 as usize;
    if idx >= MAX_NOTIFS {
        return;
    }

    let mut waiters = WAITERS.lock();
    if waiters[idx] == Some(receiver) {
        waiters[idx] = None;
        ISR_WAITERS[idx].store(-1, Ordering::Release);
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
            // D.3: route through wake_task_v2 under sched-v2 so all wake
            // paths use the CAS primitive and no v1 deferred-enqueue flag
            // is set.  Under the default (v1) build, use the existing path.
            {
                let _ = scheduler::wake_task_v2(task);
            }
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

        // F.2: Block using the v2 or v1 path depending on the `sched-v2` feature.
        //
        // Under `sched-v2`: use `block_current_until` with a dummy local
        // `AtomicBool`.  The flag is never explicitly set by the wake side;
        // instead, `wake_task_v2` (called from `signal`/`drain_pending_waiters`)
        // transitions the task to Ready via CAS.  On resume the outer loop
        // re-drains `PENDING[idx]`, so the woken-flag value is irrelevant.
        // No deadline is passed (notifications have no built-in timeout).
        //
        // Under v1 the original `block_current_on_notif()` path is retained.
        {
            use core::sync::atomic::AtomicBool;
            let woken = AtomicBool::new(false);
            let _ = scheduler::block_current_until(&woken, None);
        }
        // On wake, loop back to drain pending bits.
    }
}

// ---------------------------------------------------------------------------
// Kernel unit tests (run in the main kernel test binary via #[test_case])
// ---------------------------------------------------------------------------

/// These tests run inline inside the kernel after `mm::init` but before the
/// scheduler and task system are started.  They have direct access to the
/// real global arrays (`PENDING`, `BOUND_TCB`, `TCB_BOUND_NOTIF`) and the
/// actual `bind_task` / `clear_bound_task` / `drain_bits` functions — so
/// any regression in their logic is caught here, not by a separate copy.
///
/// Use indices ≥ 50 to avoid colliding with kernel boot-time allocations.
#[cfg(test)]
mod tests {
    use super::*;
    use kernel_core::ipc::wake_kind;

    // ------------------------------------------------------------------
    // B.5 — Process exit clears the bound-notification TCB entry
    //
    // The real `cleanup_task_ipc` path calls `clear_bound_task`.
    // This test exercises the actual function (not a copy) with the real
    // global atomic arrays so any regression in the clear logic fails here.
    // ------------------------------------------------------------------

    #[test_case]
    fn clear_bound_task_wires_both_sides_of_binding() {
        const NOTIF: usize = 50;
        const TASK: usize = 50;
        let notif_id = NotifId(NOTIF as u8);

        // Reset global state to a known baseline for this slot.
        PENDING[NOTIF].store(0, Ordering::SeqCst);
        BOUND_TCB[NOTIF].store(TCB_NONE, Ordering::SeqCst);
        TCB_BOUND_NOTIF[TASK].store(NOTIF_NONE, Ordering::SeqCst);

        // Bind via the real bind_task (same path as sys_notif_bind).
        bind_task(notif_id, TASK).expect("bind_task must succeed on a free slot");

        // Verify both sides of the binding are live.
        assert_eq!(
            BOUND_TCB[NOTIF].load(Ordering::Acquire),
            TASK as i32,
            "BOUND_TCB must record the bound task after bind_task",
        );
        assert_eq!(
            TCB_BOUND_NOTIF[TASK].load(Ordering::Acquire),
            NOTIF as u8,
            "TCB_BOUND_NOTIF must record the notification after bind_task",
        );

        // Simulate process exit: call the REAL cleanup path.
        // cleanup_task_ipc calls clear_bound_task(task_sched_idx).
        clear_bound_task(TASK);

        // Both sides must be reset to their unbound sentinels.
        assert_eq!(
            BOUND_TCB[NOTIF].load(Ordering::Acquire),
            TCB_NONE,
            "BOUND_TCB must be TCB_NONE after clear_bound_task",
        );
        assert_eq!(
            TCB_BOUND_NOTIF[TASK].load(Ordering::Acquire),
            NOTIF_NONE,
            "TCB_BOUND_NOTIF must be NOTIF_NONE after clear_bound_task",
        );

        // The slot must be available for a new bind — no dangling reference.
        bind_task(notif_id, TASK).expect("re-bind must succeed after clear");
        // Leave the slot clean for subsequent tests.
        clear_bound_task(TASK);
    }

    #[test_case]
    fn bind_task_rejects_task_slots_outside_reverse_table() {
        const NOTIF: usize = 52;
        let notif_id = NotifId(NOTIF as u8);
        let out_of_range_task = MAX_TASKS;

        BOUND_TCB[NOTIF].store(TCB_NONE, Ordering::SeqCst);

        assert_eq!(
            bind_task(notif_id, out_of_range_task),
            Err(()),
            "bind_task must reject task indices that cannot be recorded in TCB_BOUND_NOTIF",
        );
        assert_eq!(
            BOUND_TCB[NOTIF].load(Ordering::Acquire),
            TCB_NONE,
            "bind_task must not publish a one-sided BOUND_TCB entry on failure",
        );
    }

    #[test_case]
    fn bind_task_does_not_publish_forward_binding_when_task_already_bound() {
        const FIRST_NOTIF: usize = 53;
        const SECOND_NOTIF: usize = 54;
        const TASK: usize = 53;
        let first = NotifId(FIRST_NOTIF as u8);
        let second = NotifId(SECOND_NOTIF as u8);

        BOUND_TCB[FIRST_NOTIF].store(TCB_NONE, Ordering::SeqCst);
        BOUND_TCB[SECOND_NOTIF].store(TCB_NONE, Ordering::SeqCst);
        TCB_BOUND_NOTIF[TASK].store(NOTIF_NONE, Ordering::SeqCst);

        bind_task(first, TASK).expect("initial bind must succeed");
        assert_eq!(
            bind_task(second, TASK),
            Err(()),
            "second bind must fail when the task is already bound elsewhere",
        );
        assert_eq!(
            BOUND_TCB[SECOND_NOTIF].load(Ordering::Acquire),
            TCB_NONE,
            "failed bind must not publish a forward mapping visible to IRQ wakeups",
        );
        assert_eq!(
            TCB_BOUND_NOTIF[TASK].load(Ordering::Acquire),
            first.0,
            "failed bind must preserve the original reverse binding",
        );

        clear_bound_task(TASK);
    }

    // ------------------------------------------------------------------
    // B.4 supplement — drain_bits exercises the real PENDING array
    //
    // The fast-path in `recv_msg_with_notif` is:
    //   let bits = notification::drain_bits(notif_id);
    //   if classify_recv(bits) == RECV_KIND_NOTIFICATION { return ... }
    //
    // This test verifies drain_bits on the actual global PENDING array so
    // that regressions in the drain logic (wrong index, missing swap, …)
    // fail loudly in the kernel test binary.
    // ------------------------------------------------------------------

    #[test_case]
    fn drain_bits_returns_pending_and_clears_atomically() {
        const NOTIF: usize = 51;
        let notif_id = NotifId(NOTIF as u8);
        const BITS: u64 = 0b1010_0101;

        // Reset state.
        PENDING[NOTIF].store(0, Ordering::SeqCst);

        // Set bits (mirrors what signal_irq does in ISR context).
        PENDING[NOTIF].fetch_or(BITS, Ordering::Release);

        // drain_bits must return all bits and clear PENDING atomically.
        let got = drain_bits(notif_id);
        assert_eq!(got, BITS, "drain_bits must return the exact pending bits");
        assert_eq!(
            PENDING[NOTIF].load(Ordering::Acquire),
            0,
            "drain_bits must clear PENDING to zero",
        );

        // A second drain must return 0 (nothing left).
        assert_eq!(drain_bits(notif_id), 0, "second drain must return 0");

        // classify_recv (the shared seam used by recv_msg_with_notif) must
        // select NOTIFICATION for the first drain result…
        assert_eq!(
            wake_kind::classify_recv(got),
            wake_kind::RECV_KIND_NOTIFICATION,
            "non-zero drained bits must classify as NOTIFICATION",
        );
        // …and MESSAGE after the bits are exhausted.
        assert_eq!(
            wake_kind::classify_recv(0),
            wake_kind::RECV_KIND_MESSAGE,
            "zero drained bits must classify as MESSAGE",
        );
    }
}
