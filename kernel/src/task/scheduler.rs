//! SMP-aware scheduler with per-core run queues and work-stealing.
//!
//! # Design (Phase 25, refined Phase 52c, audited Phase 52d)
//!
//! ## Global lock and per-core queues
//!
//! All task state lives in a global `SCHEDULER: Mutex<Scheduler>`. The
//! global lock is acquired on every dispatch iteration for:
//!
//! - **Task selection** (`pick_next`): reads per-core run queues (which are
//!   per-core `Mutex<VecDeque>` inside `PerCoreData`) and validates task
//!   state/saved_rsp from the global `tasks` vec.
//! - **State transitions**: marking the selected task `Running`, marking
//!   yielded/blocked tasks.
//! - **ISR wake drain**: waking tasks pushed to the per-core `IsrWakeQueue`
//!   by `signal_irq()`.
//! - **Post-switch bookkeeping**: saving the outgoing task's RSP and
//!   re-enqueueing yielded tasks.
//! - **Lifecycle**: spawn, exit, drain-dead, capability/IPC operations.
//!
//! The per-core infrastructure (run queues, `IsrWakeQueue`, reschedule
//! flags, `current_task_idx`) avoids *some* cross-core contention — each
//! core selects from its own queue and ISR wakeups are pushed lock-free —
//! but the global `SCHEDULER` lock is still acquired in the dispatch hot
//! path for every task state read/write.
//!
//! **True per-core scheduling** (where the dispatch hot path never acquires
//! a global lock) is deferred to a future phase. It requires splitting the
//! `tasks` vec into per-core task ownership or a lock-free task registry,
//! which is a larger architectural change than Phase 52c/52d scope.
//!
//! ## Work-stealing (Phase 52c A.2)
//!
//! When a core's local run queue is empty, `pick_next` calls `try_steal`
//! to take one ready task from the longest other-core queue, provided the
//! task's affinity mask permits running on the stealing core. The stolen
//! task's `assigned_core` and `last_migrated_tick` are updated, and recently
//! assigned, woken, or yielded tasks are skipped until the cooldown expires.
//!
//! ## Load balancing (Phase 52c A.4)
//!
//! The BSP runs `maybe_load_balance()` every 50 scheduler ticks (~500 ms
//! at 100 Hz). If the longest run queue exceeds the shortest by more than
//! 2 entries, one task is migrated — skipping any task whose
//! `last_migrated_tick` is within `MIGRATE_COOLDOWN` (100 ticks / ~1 s).
//!
//! ## Dead-slot recycling (Phase 52c A.3)
//!
//! Dead task slots are recycled via a free list to bound memory growth.
//! `drain_dead` runs on the BSP each scheduler iteration.
//!
//! ## Voluntary yield and ISR-triggered reschedule
//!
//! A task voluntarily returns control by calling [`yield_now`], which uses
//! `switch_context` to the calling core's scheduler RSP.
//!
//! The timer ISR calls [`signal_reschedule`], which sets the calling core's
//! reschedule flag. The scheduler loop checks this flag before halting.
//!
//! # Lock hierarchy (Phase 57a v2 protocol)
//!
//! `pi_lock` is *outer*, `SCHEDULER.lock` is *inner* (Linux's `p->pi_lock` →
//! `rq->lock` pattern).  A code path may hold `pi_lock` while acquiring
//! `SCHEDULER.lock`; the reverse is forbidden.
//!
//! **State ownership (SOLID Single-Responsibility split):**
//! - `pi_lock` guards canonical block state: `TaskBlockState.state`,
//!   `wake_deadline`.
//! - `SCHEDULER.lock` guards scheduler-visible state: run-queue membership,
//!   `Task::on_cpu` (introduced in E.1).
//! - Scheduler-side iterations (`pick_next`, dispatch, `scan_expired`) read
//!   scheduler-visible state — never `pi_lock`-protected fields.
//!
//! The lock-ordering invariant is enforced in debug builds by a per-CPU
//! `holds_scheduler_lock: AtomicBool` (set/cleared in [`scheduler_lock`] /
//! [`SchedulerLockSentinel::drop`]) that [`Task::with_block_state`] reads
//! before acquiring `pi_lock`.
//!
//! # Phase 57a v2 Block/Wake Protocol
//!
//! The v2 protocol (Phase 57a) eliminates the lost-wake bug class that arose
//! from v1's intermediate-state flags. `Task::state` under `pi_lock` is the
//! sole source of truth for block state. See
//! `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md` for the full
//! state-transition spec; `docs/04-tasking.md` for the narrative description.
//!
//! ## `block_current_until` — four-step Linux recipe
//!
//! 1. **State write under `pi_lock`.** Write `task.state ← Blocked*` and
//!    `task.wake_deadline`; release `pi_lock`. (Linux `set_current_state` /
//!    `smp_store_mb` pattern.)
//! 2. **Release `pi_lock`** so a concurrent waker can CAS without deadlock.
//! 3. **Condition recheck.** If condition already satisfied, self-revert:
//!    `pi_lock` → CAS `Blocked* → Running` → clear deadline → return.
//! 4. **Yield via `SCHEDULER.lock`.** Remove task from run queue;
//!    `switch_context`. On resume, recheck; re-enter step 1 on spurious wake.
//!
//! ## `wake_task` CAS rewrite
//!
//! 1. Acquire `pi_lock`; CAS any `Blocked*` → `Ready`; clear `wake_deadline`;
//!    release `pi_lock`. If CAS fails: return `AlreadyAwake`.
//! 2. Acquire `SCHEDULER.lock`; idempotency guard (already enqueued?).
//! 3. Spin-wait if `task.on_cpu == true` until switch-out epilogue publishes
//!    `saved_rsp` (Linux `p->on_cpu` `smp_cond_load_acquire` pattern).
//! 4. Enqueue task; send reschedule IPI if cross-core.
//!
//! ## `Task::on_cpu` RSP-publication marker
//!
//! Set to `true` on dispatch; cleared in the arch-level switch-out epilogue
//! once `saved_rsp` is committed. Replaces v1's `PENDING_SWITCH_OUT[core]`
//! deferred-enqueue hand-off (deleted in Phase 57a Track E).
//!
//! ## v1 fields deleted
//!
//! `switching_out`, `wake_after_switch`, and `PENDING_SWITCH_OUT` are absent
//! from this codebase (removed in Phase 57a Tracks E–F). Any reference to
//! these names is a bug.
#![allow(dead_code)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::{cell::UnsafeCell, sync::atomic::Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;

use super::{Task, TaskId, TaskState, switch_context};
use crate::ipc::{CapError, CapHandle, Capability, EndpointId, Message, NotifId};

type TaskDebugSnapshot = (u32, &'static str, TaskState, u8, u64, u64, u64);
type CurrentTaskDebugSnapshot = (TaskId, u32, &'static str, TaskState, u8, u64, u64, u64);

// ---------------------------------------------------------------------------
// IRQ-safe scheduler lock wrapper
// ---------------------------------------------------------------------------
//
// A same-core interrupt that calls `wake_task` can deadlock a task-context
// holder of `scheduler_lock()`. [`IrqSafeMutex`] prevents that by masking
// interrupts for the duration of the critical section — the same pattern
// used by `virtio_net::DRIVER` (wrapped in `without_interrupts`) and the
// frame allocator's `with_frame_alloc_irq_safe` helper. Root cause analysis
// is in `docs/appendix/scheduler-fairness-regression.md` (early-wedge).
//
// The guard's fields are dropped in declaration order: the inner spin
// guard releases the lock *before* [`InterruptRestore`] re-enables IF,
// so an ISR cannot fire during the unlock window and reach a just-freed
// lock with stale `was_enabled` state.
pub struct IrqSafeMutex<T: ?Sized> {
    inner: Mutex<T>,
}

pub struct IrqSafeGuard<'a, T: ?Sized + 'a> {
    guard: spin::MutexGuard<'a, T>,
    _restore: InterruptRestore,
}

struct InterruptRestore {
    was_enabled: bool,
}

impl<T> IrqSafeMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }
}

impl<T: ?Sized> IrqSafeMutex<T> {
    pub fn lock(&self) -> IrqSafeGuard<'_, T> {
        let was_enabled = interrupts::are_enabled();
        if was_enabled {
            interrupts::disable();
        }
        let guard = self.inner.lock();
        IrqSafeGuard {
            guard,
            _restore: InterruptRestore { was_enabled },
        }
    }

    /// Non-blocking lock attempt — returns `None` if the inner spinlock is
    /// already held. Used by the panic-diagnostic path so it can inspect
    /// task state without deadlocking if the scheduler was holding the lock
    /// at the moment the panic fired.
    pub fn try_lock(&self) -> Option<IrqSafeGuard<'_, T>> {
        let was_enabled = interrupts::are_enabled();
        if was_enabled {
            interrupts::disable();
        }
        match self.inner.try_lock() {
            Some(guard) => Some(IrqSafeGuard {
                guard,
                _restore: InterruptRestore { was_enabled },
            }),
            None => {
                if was_enabled {
                    interrupts::enable();
                }
                None
            }
        }
    }
}

impl<T: ?Sized> core::ops::Deref for IrqSafeGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: ?Sized> core::ops::DerefMut for IrqSafeGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl Drop for InterruptRestore {
    fn drop(&mut self) {
        if self.was_enabled {
            interrupts::enable();
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 57a B.3 — SCHEDULER lock sentinel
// ---------------------------------------------------------------------------

/// RAII guard that wraps [`IrqSafeGuard`] and sets/clears the per-CPU
/// `holds_scheduler_lock` flag for the lock-ordering assertion in
/// [`Task::with_block_state`].
///
/// Drop order: inner `IrqSafeGuard` releases the spin lock, then
/// [`SchedulerLockSentinel`] clears the flag — so the flag is cleared after
/// the lock is released, which is the safe ordering.
pub(crate) struct SchedulerGuard<'a> {
    inner: IrqSafeGuard<'a, Scheduler>,
    _sentinel: SchedulerLockSentinel,
}

struct SchedulerLockSentinel;

impl Drop for SchedulerLockSentinel {
    fn drop(&mut self) {
        // Clear the flag when the scheduler lock is released.
        if let Some(core) = crate::smp::try_per_core() {
            core.holds_scheduler_lock
                .store(false, core::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl core::ops::Deref for SchedulerGuard<'_> {
    type Target = Scheduler;
    fn deref(&self) -> &Scheduler {
        &self.inner
    }
}

impl core::ops::DerefMut for SchedulerGuard<'_> {
    fn deref_mut(&mut self) -> &mut Scheduler {
        &mut self.inner
    }
}

// ---------------------------------------------------------------------------
// Statics
// ---------------------------------------------------------------------------

static SCHEDULER_INNER: IrqSafeMutex<Scheduler> = IrqSafeMutex::new(Scheduler::new());

/// Acquire the global scheduler lock, setting the per-CPU `holds_scheduler_lock`
/// flag for the Phase 57a B.3 lock-ordering assertion.
///
/// Use this in place of `SCHEDULER_INNER.lock()` everywhere inside this module.
#[inline]
pub(super) fn scheduler_lock() -> SchedulerGuard<'static> {
    let guard = SCHEDULER_INNER.lock();
    // Set the flag *after* acquiring the lock so that any contention spin
    // before acquisition does not falsely assert "holding" the lock.
    if let Some(core) = crate::smp::try_per_core() {
        core.holds_scheduler_lock
            .store(true, core::sync::atomic::Ordering::Relaxed);
    }
    SchedulerGuard {
        inner: guard,
        _sentinel: SchedulerLockSentinel,
    }
}

/// Non-blocking scheduler lock attempt.  Returns `None` if already held.
/// Sets `holds_scheduler_lock` on success; does NOT set it when returning
/// `None` (the caller does not hold the lock in that case).
#[inline]
pub(super) fn try_scheduler_lock() -> Option<SchedulerGuard<'static>> {
    let guard = SCHEDULER_INNER.try_lock()?;
    if let Some(core) = crate::smp::try_per_core() {
        core.holds_scheduler_lock
            .store(true, core::sync::atomic::Ordering::Relaxed);
    }
    Some(SchedulerGuard {
        inner: guard,
        _sentinel: SchedulerLockSentinel,
    })
}

// ---------------------------------------------------------------------------
// Per-core helpers
// ---------------------------------------------------------------------------

/// Get the current core's reschedule flag.
fn per_core_reschedule() -> &'static core::sync::atomic::AtomicBool {
    &crate::smp::per_core().reschedule
}

/// Get a mutable pointer to the current core's scheduler RSP.
fn per_core_scheduler_rsp_ptr() -> *mut u64 {
    let data = crate::smp::per_core();
    data.scheduler_rsp.get()
}

/// Get the current core's scheduler RSP value.
fn per_core_scheduler_rsp() -> u64 {
    let data = crate::smp::per_core();
    unsafe { *data.scheduler_rsp.get() }
}

/// Get/set the current task index on this core.
pub fn get_current_task_idx() -> Option<usize> {
    let val = crate::smp::per_core()
        .current_task_idx
        .load(Ordering::Relaxed);
    if val < 0 { None } else { Some(val as usize) }
}

fn set_current_task_idx(idx: Option<usize>) {
    let val = match idx {
        Some(i) => i as i32,
        None => -1,
    };
    crate::smp::per_core()
        .current_task_idx
        .store(val, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Scheduler struct
// ---------------------------------------------------------------------------

pub(crate) struct Scheduler {
    /// Addresses of `Task` instances are stable for the task's lifetime. Per-CPU dispatch state (`current_preempt_count_ptr`) caches raw pointers into `Task::preempt_count` and relies on this stability. The outer `Vec` may reallocate when growing; the inner `Box` keeps each `Task` at a fixed heap address regardless.
    #[allow(clippy::vec_box)]
    tasks: Vec<Box<Task>>,
    /// Index of the last non-idle task that was dispatched (for round-robin).
    last_run: usize,
    /// Indices of per-core idle tasks. Index by core_id.
    idle_tasks: [Option<usize>; crate::smp::MAX_CORES],
    /// Free list of dead task indices available for reuse (Phase 52c A.3).
    /// When a task exits, its index is pushed here. `spawn` pops from this
    /// list before growing the `tasks` vec.
    free_list: Vec<usize>,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            tasks: Vec::new(),
            last_run: 0,
            idle_tasks: [const { None }; crate::smp::MAX_CORES],
            free_list: Vec::new(),
        }
    }

    /// Return a reference to the task at the given index, if in range.
    ///
    /// Used by `panic_diag` to inspect the current task without panicking.
    pub(crate) fn get_task(&self, idx: usize) -> Option<&Task> {
        self.tasks.get(idx).map(|b| &**b)
    }

    fn find_by_pid(&self, pid: u32) -> Option<usize> {
        self.tasks.iter().position(|t| t.pid == pid)
    }

    /// Return the index of the task with the given [`TaskId`], if present.
    fn find(&self, id: TaskId) -> Option<usize> {
        self.tasks.iter().position(|t| t.id == id)
    }

    /// Look up a capability in the given task's cap table.
    pub fn cap(&self, id: TaskId, handle: CapHandle) -> Result<Capability, CapError> {
        let idx = self.find(id).ok_or(CapError::InvalidHandle)?;
        self.tasks[idx].caps.get(handle)
    }

    /// Remove a capability from the given task's cap table (consumes one-shot caps).
    pub fn remove_cap(&mut self, id: TaskId, handle: CapHandle) -> Result<Capability, CapError> {
        let idx = self.find(id).ok_or(CapError::InvalidHandle)?;
        self.tasks[idx].caps.remove(handle)
    }

    /// Atomically transfer a capability between two live tasks while holding
    /// the scheduler lock so concurrent cleanup cannot observe a holderless gap.
    pub fn grant_cap(
        &mut self,
        source_id: TaskId,
        source_handle: CapHandle,
        target_id: TaskId,
    ) -> Result<CapHandle, CapError> {
        let source_idx = self.find(source_id).ok_or(CapError::InvalidHandle)?;
        let target_idx = self.find(target_id).ok_or(CapError::InvalidHandle)?;

        if source_idx == target_idx {
            let cap = self.tasks[source_idx].caps.remove(source_handle)?;
            match self.tasks[source_idx].caps.insert(cap) {
                Ok(handle) => Ok(handle),
                Err(err) => {
                    let _ = self.tasks[source_idx].caps.insert_at(source_handle, cap);
                    Err(err)
                }
            }
        } else if source_idx < target_idx {
            let (before_target, from_target) = self.tasks.split_at_mut(target_idx);
            before_target[source_idx]
                .caps
                .grant(source_handle, &mut from_target[0].caps)
        } else {
            let (before_source, from_source) = self.tasks.split_at_mut(source_idx);
            from_source[0]
                .caps
                .grant(source_handle, &mut before_source[target_idx].caps)
        }
    }

    /// Return the server endpoint registered for this task.
    pub fn server_endpoint(&self, id: TaskId) -> Option<EndpointId> {
        let idx = self.find(id)?;
        self.tasks[idx].server_endpoint
    }

    /// Return whether any live task other than `excluding` still holds a cap
    /// to `ep_id`.
    pub fn other_task_holds_endpoint_cap(&self, excluding: TaskId, ep_id: EndpointId) -> bool {
        self.tasks.iter().any(|task| {
            task.id != excluding
                && task.state != TaskState::Dead
                && task.caps.contains_endpoint(ep_id)
        })
    }

    /// Return the callers currently waiting on reply capabilities held by
    /// `id`.
    pub fn reply_waiters(&self, id: TaskId) -> alloc::vec::Vec<TaskId> {
        self.find(id)
            .map(|idx| self.tasks[idx].caps.reply_targets())
            .unwrap_or_default()
    }

    /// Return the notification capabilities currently held by `id`.
    pub fn notification_caps(&self, id: TaskId) -> alloc::vec::Vec<NotifId> {
        self.find(id)
            .map(|idx| self.tasks[idx].caps.notification_ids())
            .unwrap_or_default()
    }

    fn task_current_on_any_core(&self, task_idx: usize) -> bool {
        for core_id in 0..crate::smp::core_count() {
            if let Some(data) = crate::smp::get_core_data(core_id)
                && data.current_task_idx.load(Ordering::Acquire) == task_idx as i32
            {
                return true;
            }
        }
        false
    }

    /// Return dead tasks that still need per-task IPC teardown, but only once
    /// they are no longer running on any core.
    pub fn pending_dead_ipc_cleanup(&self) -> alloc::vec::Vec<TaskId> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(idx, task)| {
                task.state == TaskState::Dead
                    && !task.ipc_cleaned
                    && !task.on_cpu.load(Ordering::Acquire)
                    && task.saved_rsp != 0
                    && !self.task_current_on_any_core(*idx)
            })
            .map(|(_, task)| task.id)
            .collect()
    }

    /// Reclaim dead tasks: free their stacks and add their indices to the
    /// free list for reuse by future spawns (Phase 52c A.3).
    fn drain_dead(&mut self) {
        // With SMP, removing tasks from the vec would invalidate indices held
        // by per-core run queues, current_task_idx, and PENDING_REENQUEUE.
        // Instead, dead tasks remain in the vec and are skipped by pick_next.
        // Their stack memory is released here to avoid leaks.
        for i in 0..self.tasks.len() {
            let task_current = self.task_current_on_any_core(i);
            let task = &mut self.tasks[i];
            if task.state == TaskState::Dead
                && task.ipc_cleaned
                && !task.on_cpu.load(Ordering::Acquire)
                && task.saved_rsp != 0
                && !task_current
            {
                let _ = task._stack.take();
                // Mark as drained so we don't try to free again.
                task.saved_rsp = 0;
                // Add to free list for index reuse, unless already there.
                if !self.free_list.contains(&i) {
                    self.free_list.push(i);
                }
            }
        }
    }

    /// Pick the next task to run on the given core.
    ///
    /// Selects from the per-core run queue (highest priority first), then
    /// attempts work-stealing from other cores, then falls back to the
    /// idle task.
    ///
    /// NOTE: this method is called while the caller holds `scheduler_lock()`,
    /// so it has access to the full `tasks` vec for state validation. True
    /// lock-free per-core dispatch is deferred (see module-level doc).
    fn pick_next(&mut self, core_id: u8) -> Option<(u64, usize)> {
        let core_bit = 1u64 << core_id;

        // Phase 1: Scan local run queue — highest-priority (lowest numeric)
        // Ready task in a single pass.
        if let Some(idx) = self.dequeue_local(core_id, core_bit) {
            self.last_run = idx;
            return Some((self.tasks[idx].saved_rsp, idx));
        }

        // Phase 2: Work-stealing — try to steal one task from another core
        // (Phase 52c A.2).
        if let Some(idx) = self.try_steal(core_id, core_bit) {
            self.tasks[idx].assigned_core = core_id;
            self.tasks[idx].last_migrated_tick = crate::arch::x86_64::interrupts::tick_count();
            self.last_run = idx;
            return Some((self.tasks[idx].saved_rsp, idx));
        }

        // Phase 3: Fall back to this core's idle task.
        if let Some(idle_idx) = self.idle_tasks[core_id as usize]
            && self.tasks[idle_idx].state == TaskState::Ready
        {
            debug_assert!(
                self.tasks[idle_idx].saved_rsp != 0,
                "pick_next: idle task idx={} has zero saved_rsp on core {}",
                idle_idx,
                core_id
            );
            return Some((self.tasks[idle_idx].saved_rsp, idle_idx));
        }

        None
    }

    /// Dequeue the highest-priority Ready task from this core's local run queue.
    /// Removes stale/ineligible entries as it scans.
    fn dequeue_local(&mut self, core_id: u8, core_bit: u64) -> Option<usize> {
        let data = crate::smp::get_core_data(core_id)?;
        let mut q = data.run_queue.lock();
        let mut best_pos: Option<usize> = None;
        let mut best_prio: u8 = u8::MAX;

        let mut i = 0;
        while i < q.len() {
            let idx = q[i];
            // Phase 57a follow-up DEBUG: log the SPECIFIC filter reason when
            // dropping a queue entry — without this, a task that's queued
            // but never dispatched is invisible (the silent `continue` was
            // hiding a per-core dispatch regression).  Budgeted in caller
            // via DEQUEUE_FILTER_LOG_BUDGET to avoid drowning the log.
            if idx >= self.tasks.len() {
                log_dequeue_filter_drop(core_id, idx, "idx-out-of-bounds", 0, 0);
                q.remove(i);
                continue;
            }
            if self.tasks[idx].state != TaskState::Ready {
                log_dequeue_filter_drop(
                    core_id,
                    idx,
                    "state-not-ready",
                    self.tasks[idx].pid,
                    self.tasks[idx].state as u64,
                );
                q.remove(i);
                continue;
            }
            if self.idle_tasks.contains(&Some(idx)) {
                log_dequeue_filter_drop(core_id, idx, "is-idle", self.tasks[idx].pid, 0);
                q.remove(i);
                continue;
            }
            if self.tasks[idx].affinity_mask & core_bit == 0 {
                log_dequeue_filter_drop(
                    core_id,
                    idx,
                    "affinity-mismatch",
                    self.tasks[idx].pid,
                    self.tasks[idx].affinity_mask,
                );
                q.remove(i);
                continue;
            }
            if self.tasks[idx].saved_rsp == 0 {
                log::error!(
                    "[sched] dropping ready task idx={} pid={} name={} with zero saved_rsp",
                    idx,
                    self.tasks[idx].pid,
                    self.tasks[idx].name
                );
                // TODO(57a-C/D): route through pi_lock + with_block_state
                self.tasks[idx].state = TaskState::Dead;
                q.remove(i);
                continue;
            }
            if self.tasks[idx].priority < best_prio {
                best_prio = self.tasks[idx].priority;
                best_pos = Some(i);
            }
            i += 1;
        }

        if let Some(pos) = best_pos {
            let idx = q.remove(pos).unwrap();
            debug_assert!(
                self.tasks[idx].state == TaskState::Ready,
                "pick_next: local queue task idx={} not Ready (state={:?})",
                idx,
                self.tasks[idx].state
            );
            return Some(idx);
        }
        None
    }

    /// Try to steal one task from another core's run queue (Phase 52c A.2).
    ///
    /// Iterates over all other cores, preferring the one with the longest
    /// queue. Steals at most one task, checking affinity before stealing.
    fn try_steal(&mut self, my_core: u8, my_core_bit: u64) -> Option<usize> {
        let n = crate::smp::core_count();
        if n <= 1 {
            return None;
        }

        // Find the core with the longest run queue (excluding ourselves).
        let mut best_core: Option<u8> = None;
        let mut best_len: usize = 0;
        for id in 0..n {
            if id == my_core {
                continue;
            }
            if let Some(data) = crate::smp::get_core_data(id) {
                let len = data.run_queue.lock().len();
                if len > best_len {
                    best_len = len;
                    best_core = Some(id);
                }
            }
        }

        let victim_core = best_core?;
        if best_len == 0 {
            return None;
        }

        let data = crate::smp::get_core_data(victim_core)?;
        let mut q = data.run_queue.lock();

        // Find a stealable task: Ready, affinity-compatible with our core,
        // not an idle task.
        for i in 0..q.len() {
            let idx = q[i];
            if idx >= self.tasks.len() {
                continue;
            }
            let task = &self.tasks[idx];
            if task.state != TaskState::Ready {
                continue;
            }
            // Fresh fork/clone children carry a task-local fork_ctx until their
            // first dispatch through fork_child_trampoline. Keep them on the
            // spawning core so the parent's immediate wait/read-yield loop
            // can't starve them behind background work on another CPU.
            if task.fork_ctx.is_some() {
                continue;
            }
            if crate::arch::x86_64::interrupts::tick_count().saturating_sub(task.last_migrated_tick)
                < MIGRATE_COOLDOWN
            {
                continue;
            }
            if self.idle_tasks.contains(&Some(idx)) {
                continue;
            }
            if task.affinity_mask & my_core_bit == 0 {
                continue;
            }
            if task.saved_rsp == 0 {
                continue;
            }
            // Steal this task.
            q.remove(i);
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::RunQueueEnqueue {
                task_idx: idx as u32,
                core: my_core,
            });
            return Some(idx);
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Signal the scheduler to run on the next opportunity (current core).
///
/// This is the only scheduler function called from an interrupt handler and
/// must be async-signal-safe: it performs only an atomic store.
pub fn signal_reschedule() {
    // During early boot (before SMP init), gs_base is 0. Fall back to
    // a no-op — the BSP will pick up the reschedule when it enters run().
    if crate::smp::is_per_core_ready() {
        per_core_reschedule().store(true, Ordering::Relaxed);
    }
}

/// Signal all online cores to reschedule (used when a task becomes Ready
/// and any idle core could pick it up).
fn signal_reschedule_all() {
    if !crate::smp::is_per_core_ready() {
        return;
    }
    let n = crate::smp::core_count();
    for id in 0..n {
        if let Some(data) = crate::smp::get_core_data(id) {
            data.reschedule.store(true, Ordering::Relaxed);
        }
    }
}

fn core_load(sched: &Scheduler, core_id: u8) -> usize {
    let Some(data) = crate::smp::get_core_data(core_id) else {
        return usize::MAX;
    };

    // Phase 57 fix: an AP whose `INIT-SIPI-SIPI` boot timed out still
    // has its `PerCoreData` allocated (`init_ap_per_core` runs before
    // we wait for `is_online`), so `get_core_data` returns `Some` for
    // it. Its run queue is empty, which previously made it look like
    // the "least loaded" core to `least_loaded_core` — fork-children
    // got queued onto a core whose scheduler never runs and were lost
    // forever. Treat an offline core as fully saturated so it is
    // never selected by the load balancer.
    if !data.is_online.load(Ordering::Acquire) {
        return usize::MAX;
    }

    let mut load = data.run_queue.lock().len();
    let current = data.current_task_idx.load(Ordering::Relaxed);
    if current >= 0 {
        let idx = current as usize;
        if idx < sched.tasks.len()
            && sched.idle_tasks[core_id as usize] != Some(idx)
            && sched.tasks[idx].state != TaskState::Dead
        {
            load += 1;
        }
    }
    load
}

/// Find the core with the shortest run queue for task assignment.
fn least_loaded_core(sched: &Scheduler) -> u8 {
    let n = crate::smp::core_count();
    if n <= 1 {
        return 0;
    }
    let mut best_core = 0u8;
    let mut best_len = usize::MAX;
    for id in 0..n {
        let len = core_load(sched, id);
        if len < best_len {
            best_len = len;
            best_core = id;
        }
    }
    best_core
}

/// Enqueue a task index into a specific core's run queue and signal it.
///
/// The body runs with interrupts masked on the current CPU so that a
/// same-core ISR cannot re-enter and deadlock on `run_queue.lock()` —
/// the ISR-callable wake path (virtio-net, virtio-blk) funnels through
/// `wake_task` → `enqueue_to_core`, and the task-context holder must not
/// be preempted while the per-core run queue is locked. See the root-cause
/// note at [`IrqSafeMutex`].
fn enqueue_to_core(core_id: u8, idx: usize) {
    debug_assert!(
        (core_id as usize) < crate::smp::MAX_CORES,
        "enqueue_to_core: core_id={} exceeds MAX_CORES={}",
        core_id,
        crate::smp::MAX_CORES
    );
    interrupts::without_interrupts(|| {
        if let Some(data) = crate::smp::get_core_data(core_id) {
            data.run_queue.lock().push_back(idx);
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::RunQueueEnqueue {
                task_idx: idx as u32,
                core: core_id,
            });
            data.reschedule.store(true, Ordering::Relaxed);
            // Phase 54: when we enqueue onto a DIFFERENT core than the caller's,
            // setting the reschedule flag alone won't wake a halted target core
            // — `hlt` only wakes on an interrupt. Send a reschedule IPI so the
            // target picks up the new task immediately instead of waiting for
            // its next local timer tick (~10 ms on APs).
            //
            // Without this, the high-volume cross-core IPC introduced by the
            // broadened VFS routing in commit 3944b9b causes deterministic
            // login hangs on `/etc/services.d` STAT and intermittent hangs
            // during subsequent interactive commands — wake_task returns true
            // (state flips Blocked→Ready) but the idle target core never
            // notices the new run-queue entry.
            if crate::smp::is_per_core_ready() {
                let current = crate::smp::per_core().core_id;
                if current != core_id {
                    crate::smp::ipi::send_ipi_to_core(core_id, crate::smp::ipi::IPI_RESCHEDULE);
                }
            }
        }
    });
}

/// Allocate a slot for a new task, reusing a dead slot from the free list
/// if available, otherwise appending to the task vec.
fn alloc_task_slot(sched: &mut Scheduler, task: Task) -> usize {
    let boxed = Box::new(task);
    if let Some(idx) = sched.free_list.pop() {
        // Reuse a dead slot. Overwriting the slot drops the prior `Box<Task>`
        // and installs a fresh stable heap address for the new task.
        crate::ipc::notification::clear_bound_task(idx);
        sched.tasks[idx] = boxed;
        idx
    } else {
        let idx = sched.tasks.len();
        sched.tasks.push(boxed);
        idx
    }
}

/// Spawn a new kernel task. The task is assigned to the least-loaded core
/// and enqueued to that core's run queue.
pub fn spawn(entry: fn() -> !, name: &'static str) {
    let mut task = Task::new(entry, name);
    let mut sched = scheduler_lock();
    let target = least_loaded_core(&sched);
    let now = crate::arch::x86_64::interrupts::tick_count();
    task.assigned_core = target;
    task.last_migrated_tick = now;
    task.last_ready_tick = now;
    let idx = alloc_task_slot(&mut sched, task);
    drop(sched);
    enqueue_to_core(target, idx);
}

/// Spawn a new kernel task on the calling core.
///
/// Used for short-lived local kernel work that should stay with the caller.
pub fn spawn_on_current_core(entry: fn() -> !, name: &'static str) {
    let mut task = Task::new(entry, name);
    let core = crate::smp::per_core().core_id;
    let now = crate::arch::x86_64::interrupts::tick_count();
    task.assigned_core = core;
    task.last_migrated_tick = now;
    task.last_ready_tick = now;
    let mut sched = scheduler_lock();
    let idx = alloc_task_slot(&mut sched, task);
    drop(sched);
    enqueue_to_core(core, idx);
}

/// Spawn a fork/clone child task with its userspace entry context attached
/// directly to the task instead of a global queue.
pub fn spawn_fork_task(ctx: crate::process::ForkChildCtx, name: &'static str) -> u8 {
    let current_core = crate::smp::per_core().core_id;
    let fork_pid = ctx.pid;
    let fork_rip = ctx.user_rip;
    let fork_rsp = ctx.user_rsp;
    let mut task = Task::new(crate::process::fork_child_trampoline, name);
    let mut sched = scheduler_lock();
    let now = crate::arch::x86_64::interrupts::tick_count();
    let target_core = if fork_pid == 1 {
        current_core
    } else {
        least_loaded_core(&sched)
    };
    task.assigned_core = target_core;
    task.last_migrated_tick = now;
    task.last_ready_tick = now;
    // Fresh fork children need one prompt first dispatch so they can consume
    // `fork_ctx` in `fork_child_trampoline` and enter their normal userspace
    // wait/exec path. Restore the default priority as soon as the trampoline
    // takes the context.
    task.priority = 19;
    // Publish the child PID before the first dispatch so pid-based lifecycle
    // operations (for example exit_group teardown) can target the task even if
    // it has not reached fork_child_trampoline yet.
    task.pid = fork_pid;
    task.fork_ctx = Some(ctx);
    debug_assert!(
        task.fork_ctx.is_some(),
        "spawn_fork_task: fork_ctx missing after set"
    );
    let idx = alloc_task_slot(&mut sched, task);
    drop(sched);

    crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::ForkCtxPublish {
        pid: fork_pid,
        rip: fork_rip,
        rsp: fork_rsp,
    });
    crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::ForkTaskSpawned {
        pid: fork_pid,
        task_idx: idx as u32,
        core: target_core,
    });
    // PHASE 57 DEBUG: log every fork-child task spawn at INFO so the
    // boot transcript shows the (pid, task_idx, target_core) tuple
    // for every fork. Two pids (kbd at 6, fat at 10) never reach
    // their userspace child path; this trace tells us whether they
    // even get enqueued, and to which core.
    log::info!(
        "[sched] fork-task-spawn pid={} task_idx={} target_core={} rip={:#x} rsp={:#x}",
        fork_pid,
        idx,
        target_core,
        fork_rip,
        fork_rsp,
    );
    enqueue_to_core(target_core, idx);

    target_core
}

/// Register an idle task for a specific core.
///
/// Each core should have its own idle task that runs when no other task
/// is ready on that core.
pub fn spawn_idle_for_core(entry: fn() -> !, core_id: u8) {
    assert!((core_id as usize) < crate::smp::MAX_CORES);
    let mut task = Task::new(entry, "idle");
    task.assigned_core = core_id;
    task.priority = 30; // Idle priority
    let mut sched = scheduler_lock();
    let idx = alloc_task_slot(&mut sched, task);
    sched.idle_tasks[core_id as usize] = Some(idx);
}

/// Register the idle task (legacy single-core API — registers for core 0).
pub fn spawn_idle(entry: fn() -> !) {
    spawn_idle_for_core(entry, 0);
}

/// Accumulate elapsed ticks for the current task.
///
/// Currently all ticks are attributed to `user_ticks`. Splitting ticks into
/// user vs system (ring 3 vs ring 0) requires tracking the syscall-entry
/// boundary and is deferred to a future phase.
fn accumulate_ticks(sched: &mut Scheduler, idx: usize) {
    let now = crate::arch::x86_64::interrupts::tick_count();
    let elapsed = now.saturating_sub(sched.tasks[idx].start_tick);
    sched.tasks[idx].user_ticks += elapsed;
}

fn current_user_return_addr_space_snapshot(pid: u32) -> (u64, u64) {
    if pid == 0 {
        return (0, 0);
    }
    let table = crate::process::PROCESS_TABLE.lock();
    match table.find(pid) {
        Some(p) => {
            let cr3 = p
                .addr_space
                .as_ref()
                .map(|a| a.pml4_phys().as_u64())
                .unwrap_or(0);
            let as_gen = p.addr_space.as_ref().map(|a| a.generation()).unwrap_or(0);
            (cr3, as_gen)
        }
        None => (0, 0),
    }
}

/// Snapshot per-core user state into the task's `UserReturnState`.
///
/// Phase 52d: this is now a **secondary** save path.  The authoritative
/// snapshot is taken at syscall entry (see `snapshot_user_return_state` in
/// `syscall/mod.rs`).  Block/yield sites call this only as a safety net
/// for kernel-originated yields that bypass `syscall_handler` (e.g.
/// `signal_reschedule` during IRQ-driven preemption).  For normal syscall
/// paths the snapshot is already populated and this call merely refreshes
/// the FS.base which may have been modified by `ARCH_SET_FS`.
///
/// The caller passes the address-space metadata in so this helper never
/// takes `PROCESS_TABLE` while `SCHEDULER` is already locked.
fn save_user_return_state(task: &mut Task, cr3_phys: u64, addr_space_gen: u64) {
    if task.pid != 0 {
        let pc = crate::smp::per_core();
        let fs = x86_64::registers::model_specific::FsBase::read().as_u64();
        task.user_return = Some(crate::task::UserReturnState {
            user_rsp: pc.syscall_user_rsp,
            kernel_stack_top: pc.syscall_stack_top,
            fs_base: fs,
            cr3_phys,
            addr_space_gen,
        });
    }
}

/// Per-core pending re-enqueue slot. When a task yields, its index is stored
/// here instead of being immediately enqueued. The scheduler loop re-enqueues
/// it AFTER `switch_context` has saved the task's RSP, preventing a race where
/// another core picks up the task with a stale RSP.
/// -1 = no pending task. Indexed by core_id.
#[allow(clippy::declare_interior_mutable_const)]
static PENDING_REENQUEUE: [core::sync::atomic::AtomicI32; crate::smp::MAX_CORES] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(-1);
    [INIT; crate::smp::MAX_CORES]
};

struct SavedRspCell(UnsafeCell<u64>);

unsafe impl Sync for SavedRspCell {}

impl SavedRspCell {
    const fn new(value: u64) -> Self {
        Self(UnsafeCell::new(value))
    }

    fn get(&self) -> *mut u64 {
        self.0.get()
    }
}

/// Per-core scratch slot used while a task is switching out. `switch_context`
/// stores the updated RSP here, and the scheduler copies it back into the task
/// record once control returns to the scheduler.
static PENDING_SAVED_RSP: [SavedRspCell; crate::smp::MAX_CORES] =
    [const { SavedRspCell::new(0) }; crate::smp::MAX_CORES];

fn per_core_switch_save_rsp_ptr() -> *mut u64 {
    PENDING_SAVED_RSP[crate::smp::per_core().core_id as usize].get()
}

fn take_per_core_switch_save_rsp(core_id: usize) -> u64 {
    unsafe { *PENDING_SAVED_RSP[core_id].get() }
}

// ---------------------------------------------------------------------------
// Phase 57b C.2 — `current_preempt_count_ptr` switch-out retarget
// ---------------------------------------------------------------------------
//
// Every `IrqSafeMutex` guard (Phase 57b F.1, future wave) must decrement the
// **same pointee** it incremented.  The scheduler dispatches between two
// disjoint contexts:
//
//   1. **Scheduler context** (this `run` loop's stack) — between
//      `switch_context` returning and the next `switch_context` call.  Here
//      `IrqSafeMutex` acquire/release pairs (e.g. inside `pick_next` or
//      `drain_dead`) must charge a single per-core pointee — the
//      [`crate::smp::SCHED_PREEMPT_COUNT_DUMMY`] slot.
//   2. **Task context** — anywhere the chosen task executes after
//      `switch_context` jumps in.  Acquire/release pairs there must charge
//      that task's `Task::preempt_count` (wired in C.3).
//
// C.2 covers the switch-out retarget: immediately after `switch_context`
// returns onto the scheduler stack, and **before** any new lock is acquired
// on that stack, the pointer is restored to the per-core dummy.

/// Phase 57b C.2 — switch-out retarget helper.
///
/// Stores `&SCHED_PREEMPT_COUNT_DUMMY[core_id]` into
/// [`crate::smp::PerCoreData::current_preempt_count_ptr`] with `Release`
/// ordering, inside an interrupt-masked window.  Called from the dispatch
/// path immediately after `switch_context` returns onto the scheduler stack
/// and before any new `IrqSafeMutex` is acquired.
///
/// `cli`s itself rather than relying on the surrounding state: `switch_context`
/// `popf`s the scheduler's saved RFLAGS on resume (typically IF=1), so the
/// switch-out path cannot assume IRQs are masked.  IF is restored on exit if
/// it was enabled on entry.
///
/// Lock-free by mandate: no `IrqSafeMutex::lock`, no `scheduler_lock()`.
/// Phase 57b F.1's `IrqSafeMutex::lock` will call `preempt_disable()`
/// (which reads this pointer); if this helper acquired a lock, the wiring
/// would recurse.
#[inline]
fn retarget_preempt_count_to_dummy(core_id: u8) {
    let saved_if = interrupts::are_enabled();
    interrupts::disable();
    let dummy_ptr = &crate::smp::SCHED_PREEMPT_COUNT_DUMMY[core_id as usize]
        as *const core::sync::atomic::AtomicI32
        as *mut core::sync::atomic::AtomicI32;
    let pc = crate::smp::per_core();
    pc.current_preempt_count_ptr
        .store(dummy_ptr, core::sync::atomic::Ordering::Release);
    if saved_if {
        interrupts::enable();
    }
}

/// Phase 57 DEBUG: per-core countdown for yield_now log markers.
/// Atomic so the IPI-context observer doesn't trip race detection
/// in case a yield races with another path.
// Phase 57a follow-up: bumped from 4 to 1024 so the per-core dispatch
// regression (tasks queued on core 1 silently never dispatching after the
// first wave) is visible in the boot transcript instead of being budgeted
// out after the first 4 yields.
static YIELD_LOG_BUDGET: [core::sync::atomic::AtomicI32; crate::smp::MAX_CORES] =
    [const { core::sync::atomic::AtomicI32::new(1024) }; crate::smp::MAX_CORES];

// Phase 57a follow-up DEBUG: per-core budget for the dequeue filter-drop
// trace.  Each filter rejection in `dequeue_local` consumes one slot.
// Bounded so a permanently-stuck task that the queue scanner repeatedly
// rejects doesn't drown the log; large enough to surface the *first*
// time a previously-running task gets filtered out.
static DEQUEUE_FILTER_LOG_BUDGET: [core::sync::atomic::AtomicI32; crate::smp::MAX_CORES] =
    [const { core::sync::atomic::AtomicI32::new(64) }; crate::smp::MAX_CORES];

#[cold]
fn log_dequeue_filter_drop(core_id: u8, idx: usize, reason: &str, pid: u32, extra: u64) {
    let n = DEQUEUE_FILTER_LOG_BUDGET[core_id as usize]
        .fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    if n > 0 {
        log::warn!(
            "[sched] dequeue-drop core={} idx={} pid={} reason={} extra={:#x}",
            core_id,
            idx,
            pid,
            reason,
            extra,
        );
    }
}

/// Yield the current task back to the scheduler.
pub fn yield_now() {
    // Phase 57 DEBUG: log entry and exit of yield_now per core. If
    // we see "yield-enter core=3" but no "yield-handoff core=3" or
    // a missing "resume core=3" pair, we'll know the yield itself is
    // hanging vs the switch_context call site is hanging.
    {
        let core_id = crate::smp::per_core().core_id;
        let n =
            YIELD_LOG_BUDGET[core_id as usize].fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
        if n > 0 {
            log::info!("[sched] yield-enter core={}", core_id);
        }
    }
    let addr_space_snapshot =
        current_user_return_addr_space_snapshot(crate::process::current_pid());
    let idx = {
        let mut sched = scheduler_lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        debug_assert!(
            idx < sched.tasks.len(),
            "yield_now: task idx={} out of bounds (len={})",
            idx,
            sched.tasks.len()
        );
        accumulate_ticks(&mut sched, idx);
        // Keep state as Running — the scheduler will set Ready + enqueue AFTER
        // switch_context saves the RSP.
        // E.1: mark RSP-publication window (cleared by dispatch epilogue).
        sched.tasks[idx].on_cpu.store(true, Ordering::Release);
        save_user_return_state(
            &mut sched.tasks[idx],
            addr_space_snapshot.0,
            addr_space_snapshot.1,
        );
        set_current_task_idx(None);
        idx
    };
    // Store idx so the dispatch handler can save RSP and re-enqueue after switch_context.
    let my_core = crate::smp::per_core().core_id as usize;
    PENDING_REENQUEUE[my_core].store(idx as i32, Ordering::Release);
    let sched_rsp = per_core_scheduler_rsp();
    debug_assert!(
        sched_rsp != 0,
        "yield_now: scheduler RSP is zero on core {}",
        my_core
    );
    crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::YieldNow {
        task_idx: idx as u32,
        core: my_core as u8,
    });
    // Phase 57 DEBUG: log right before the switch_context handoff so
    // we can spot a yield that reaches here but doesn't hand off.
    {
        let n = YIELD_LOG_BUDGET[my_core].load(core::sync::atomic::Ordering::Relaxed);
        if n >= 0 {
            log::info!(
                "[sched] yield-handoff core={} sched_rsp={:#x} idx={}",
                my_core,
                sched_rsp,
                idx
            );
        }
    }
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };
}

// ---------------------------------------------------------------------------
// IPC scheduler primitives
// ---------------------------------------------------------------------------

/// Store a PID in the current task so the scheduler can restore per-core
/// process context on re-dispatch.
pub fn set_current_task_pid(pid: u32) {
    if let Some(idx) = get_current_task_idx() {
        scheduler_lock().tasks[idx].pid = pid;
    }
}

/// Phase 52d B.1: set the current task's `UserReturnState` from the
/// syscall entry snapshot.  Called by `snapshot_user_return_state` in
/// `arch/x86_64/syscall/mod.rs`.
pub fn set_current_user_return(urs: crate::task::UserReturnState) {
    if let Some(idx) = get_current_task_idx() {
        scheduler_lock().tasks[idx].user_return = Some(urs);
    }
}

pub fn take_current_task_fork_ctx() -> Option<crate::process::ForkChildCtx> {
    let idx = get_current_task_idx()?;
    let mut sched = scheduler_lock();
    let task = &mut sched.tasks[idx];
    let ctx = task.fork_ctx.take()?;
    task.priority = 20;
    Some(ctx)
}

/// Return the PID associated with the given task index.
fn task_pid(idx: usize) -> u32 {
    scheduler_lock().tasks[idx].pid
}

/// Resolve a [`TaskId`] to its owning process PID, if the task exists.
///
/// Used by kernel-side facades that need to validate the provenance of a
/// service registration (e.g. `kernel::blk::remote::is_registered` must
/// check that whoever registered `nvme.block` is a supervised driver
/// process, not an arbitrary ring-3 task that grabbed the name first).
/// Returns `None` if no task with the given id exists.
pub fn pid_for_task_id(task_id: TaskId) -> Option<u32> {
    let sched = scheduler_lock();
    sched.tasks.iter().find(|t| t.id == task_id).map(|t| t.pid)
}

/// Return the user and system tick counts for the current task.
pub fn current_task_times() -> Option<(u64, u64)> {
    let idx = get_current_task_idx()?;
    let sched = scheduler_lock();
    Some((sched.tasks[idx].user_ticks, sched.tasks[idx].system_ticks))
}

/// Return the [`TaskId`] of the task currently running on this core.
pub fn current_task_id() -> Option<TaskId> {
    let idx = get_current_task_idx()?;
    let sched = scheduler_lock();
    Some(sched.tasks[idx].id)
}

/// Best-effort debug snapshot for the task currently running on this core.
pub fn current_task_debug_snapshot() -> Option<CurrentTaskDebugSnapshot> {
    let idx = get_current_task_idx()?;
    let sched = scheduler_lock();
    let task = sched.tasks.get(idx)?;
    Some((
        task.id,
        task.pid,
        task.name,
        task.state,
        task.assigned_core,
        task.affinity_mask,
        task.last_ready_tick,
        task.last_migrated_tick,
    ))
}

/// Best-effort debug snapshot for an arbitrary task id.
pub fn task_debug_snapshot(id: TaskId) -> Option<TaskDebugSnapshot> {
    let sched = scheduler_lock();
    let idx = sched.find(id)?;
    let task = sched.tasks.get(idx)?;
    Some((
        task.pid,
        task.name,
        task.state,
        task.assigned_core,
        task.affinity_mask,
        task.last_ready_tick,
        task.last_migrated_tick,
    ))
}

/// Return dead tasks whose IPC state still needs deferred cleanup.
pub fn dead_tasks_needing_ipc_cleanup() -> alloc::vec::Vec<TaskId> {
    let sched = scheduler_lock();
    sched.pending_dead_ipc_cleanup()
}

// ---------------------------------------------------------------------------
// v2 block primitive (Phase 57a Track C) — feature-gated behind `sched-v2`
// ---------------------------------------------------------------------------

/// Outcome returned by [`block_current_until`].
///
/// Callers use this to distinguish why the block ended: a successful wake,
/// deadline expiry, or an early return because the condition was already
/// satisfied when the function was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockOutcome {
    /// A waker wrote `true` to the `woken` flag; the task resumed normally.
    Woken,
    /// The absolute-tick `deadline_ticks` elapsed before a wake arrived.
    DeadlineExpired,
    /// The `woken` flag was already `true` when checked at entry (step 2
    /// condition recheck); the task self-reverted without yielding.
    AlreadyTrue,
}

/// v2 block primitive following Linux's `do_nanosleep` four-step pattern.
///
/// This is the canonical replacement for the v1 `block_current_unless_woken`
/// family. It eliminates the lost-wake bug class by making `TaskBlockState`
/// under `pi_lock` the sole source of truth, mirroring Linux's
/// `set_current_state` + `schedule()` + recheck pattern.
///
/// # Four-step protocol (Linux `do_nanosleep`, `kernel/time/hrtimer.c`)
///
/// 1. **State write under `pi_lock`.** Acquire `pi_lock`; write
///    `state ← BlockedOnRecv`; set `wake_deadline ← deadline_ticks`; release
///    `pi_lock`. This pairs with the Acquire barrier on the wake side's CAS,
///    closing the lost-wake window (`smp_store_mb` / `set_current_state`).
///
/// 2. **Release `pi_lock`.** The lock is dropped before the condition recheck
///    so a concurrent waker can acquire `pi_lock` and CAS without deadlock.
///
/// 3. **Condition recheck.** If `woken.load(Acquire) == true` or
///    `tick_count() >= deadline_ticks` **before** yielding: acquire `pi_lock`;
///    CAS `BlockedOnRecv → Running`; clear `wake_deadline`; release `pi_lock`;
///    return [`BlockOutcome::AlreadyTrue`] without yielding. This closes the
///    race window between the state write and the yield (Linux: `t->task` /
///    `task->__state` recheck before `schedule()`).
///
/// 4. **Yield via `SCHEDULER.lock`.** Acquire `SCHEDULER.lock`; remove task
///    from run queue; call `switch_context` to the scheduler RSP. On resume,
///    recheck; spurious wakes loop back to step 1 (not expected on this
///    microkernel but correct by construction).
///
/// # Deadline semantics
///
/// `deadline_ticks` is an **absolute** tick count (`TICKS_PER_SEC = 1000`, so
/// 1 tick = 1 ms). Callers convert from `Duration` / `timespec` / TSC at the
/// syscall boundary — no nanoseconds inside this primitive.
/// Pass `None` for an indefinite timeout.
///
/// # v1 compatibility (transition window)
///
/// During the Track C–F migration window, this function also writes the v1
/// The v1 shadow-lock dual-write pattern is no longer needed after Track E.
///
/// # Wake-side `on_cpu` spin-wait
///
/// The E.1 `Task::on_cpu` RSP-publication marker is being landed in a
/// parallel worktree. The BLOCK side (this function) does not need it; the
/// spin-wait lives on the WAKE side (`wake_task`, Track D). No reference to
/// `on_cpu` is made here.
///
/// # References
///
/// - Linux `do_nanosleep` (`kernel/time/hrtimer.c`) — four-step block recipe.
/// - Linux `try_to_wake_up` (`kernel/sched/core.c`) — CAS wake side.
/// - m3OS handoff 2026-04-25: `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md`.
pub fn block_current_until(
    kind: TaskState,
    woken: &core::sync::atomic::AtomicBool,
    deadline_ticks: Option<u64>,
) -> BlockOutcome {
    use core::sync::atomic::Ordering;

    debug_assert!(
        matches!(
            kind,
            TaskState::BlockedOnRecv
                | TaskState::BlockedOnSend
                | TaskState::BlockedOnReply
                | TaskState::BlockedOnNotif
                | TaskState::BlockedOnFutex
        ),
        "block_current_until kind must be a Blocked* variant; got {:?}",
        kind
    );

    // Early bail: condition already true before any state write (fast path).
    if woken.load(Ordering::Acquire) {
        return BlockOutcome::AlreadyTrue;
    }
    if deadline_ticks
        .map(|d| crate::arch::x86_64::interrupts::tick_count() >= d)
        .unwrap_or(false)
    {
        return BlockOutcome::DeadlineExpired;
    }

    let addr_space_snapshot =
        current_user_return_addr_space_snapshot(crate::process::current_pid());

    // ── Lock-order discipline (Linux p->pi_lock → rq->lock) ───────────────────
    //
    // pi_lock is OUTER, SCHEDULER.lock is INNER.  Both writes (canonical
    // `TaskBlockState.state` and scheduler-visible `Task::state`) MUST happen
    // under pi_lock to be atomic with respect to `wake_task_v2`, which holds
    // pi_lock during its CAS.  Pattern:
    //
    //   1. Brief scheduler_lock to capture idx + pi_lock_ptr (raw).
    //   2. Drop scheduler_lock.
    //   3. Acquire pi_lock (OUTER).
    //   4. Inside pi_lock, acquire scheduler_lock (INNER) and write BOTH
    //      `TaskBlockState.state` and `Task::state` atomically (waker is
    //      blocked on pi_lock for the duration).  Mirror bookkeeping
    //      (accumulate_ticks, save_user_return_state, set_current_task_idx).
    //   5. Drop scheduler_lock; drop pi_lock.
    //
    // SAFETY of the raw pointer: the Task at `tasks[idx]` is stable in memory
    // for its lifetime (the Vec only grows; dead-slot recycling runs under
    // SCHEDULER.lock and we re-check the slot is still ours).
    let (idx, core, pi_lock_ptr) = {
        let sched = scheduler_lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return BlockOutcome::AlreadyTrue, // no current task — bail
        };
        let pi_lock_ptr: *const IrqSafeMutex<super::TaskBlockState> =
            &raw const sched.tasks[idx].pi_lock;
        let core = crate::smp::per_core().core_id;
        (idx, core, pi_lock_ptr)
        // scheduler_lock dropped here.
    };

    // ── Step 1: Atomic state write under pi_lock (OUTER) → scheduler_lock (INNER)
    //
    // SAFETY: pi_lock_ptr points into the stable Task struct captured above.
    // SCHEDULER.lock is NOT held when we acquire pi_lock here, satisfying the
    // lock-ordering invariant.  The waker (`wake_task_v2`) will spin on pi_lock
    // until we release, so its CAS cannot interleave with our writes.
    {
        let pi_lock_ref = unsafe { &*pi_lock_ptr };
        let mut bs = pi_lock_ref.lock();

        // Inner scheduler_lock — both writes happen with both locks held.
        {
            let mut sched = scheduler_lock();
            if idx >= sched.tasks.len() {
                return BlockOutcome::AlreadyTrue;
            }
            // Canonical (pi_lock-protected) write.
            bs.state = kind;
            if deadline_ticks.is_some() && bs.wake_deadline.is_none() {
                ACTIVE_WAKE_DEADLINES.fetch_add(1, Ordering::Relaxed);
            } else if deadline_ticks.is_none() && bs.wake_deadline.is_some() {
                ACTIVE_WAKE_DEADLINES.fetch_sub(1, Ordering::Relaxed);
            }
            bs.wake_deadline = deadline_ticks;

            // Scheduler-visible mirror — same critical section, no race window.
            accumulate_ticks(&mut sched, idx);
            sched.tasks[idx].state = kind;
            sched.tasks[idx].blocked_since_tick = crate::arch::x86_64::interrupts::tick_count();
            sched.tasks[idx].wake_deadline = deadline_ticks;
            // E.1: mark RSP-publication window for cross-core wakers.
            // `wake_task_v2` only spin-waits on this when the waker's core is
            // DIFFERENT from `assigned_core` (see same-core escape in
            // wake_task_v2's step 4 spin) — a same-core IRQ that wakes the
            // very task it interrupted MUST NOT spin on `on_cpu==false`
            // because `on_cpu` clears only at the dispatch epilogue, which
            // can't run until the ISR returns; that would deadlock.
            sched.tasks[idx].on_cpu.store(true, Ordering::Release);
            save_user_return_state(
                &mut sched.tasks[idx],
                addr_space_snapshot.0,
                addr_space_snapshot.1,
            );
            set_current_task_idx(None);
            // scheduler_lock released.
        }
        // pi_lock released.
    }

    // ── Step 2: pi_lock + SCHEDULER.lock both released ───────────────────────

    // ── Step 3: Condition recheck before yielding ─────────────────────────────
    //
    // Two cases when `woken || expired` is observed:
    //   (a) Waker is racing — woken set, but `wake_task_v2`'s pi_lock CAS has
    //       not run yet.  Canonical state is still Blocked*.
    //   (b) Waker already won — CAS'd to Ready, mirrored Task::state = Ready,
    //       and enqueued the task.
    //
    // The self-revert below handles BOTH cases atomically by overwriting
    // canonical and scheduler-visible state to Running under pi_lock + sched.
    //   - Case (a): waker is still spinning on pi_lock; when it finally CAS's,
    //     it sees Running and returns AlreadyAwake.  No enqueue happens.
    //   - Case (b): waker already enqueued.  After we restore current_task_idx
    //     and return, the queue has a stale entry pointing at our idx; pick_next
    //     sees `state != Ready` (Running) and silently removes it (`dequeue_local`
    //     filter at scheduler.rs:577).  No double-dispatch.
    let already_woken = woken.load(Ordering::Acquire);
    let already_expired = deadline_ticks
        .map(|d| crate::arch::x86_64::interrupts::tick_count() >= d)
        .unwrap_or(false);

    if already_woken || already_expired {
        // Self-revert under pi_lock OUTER + scheduler_lock INNER, atomic with
        // respect to wake_task_v2.
        //
        // Also clear on_cpu (set by the block-side write above): the task is
        // staying on this CPU and will NOT reach the dispatch epilogue that
        // normally clears it.  Leaving on_cpu=true would stall every
        // subsequent CROSS-CORE waker until the task's next real
        // switch_context (could be milliseconds away).
        {
            let pi_lock_ref = unsafe { &*pi_lock_ptr };
            let mut bs = pi_lock_ref.lock();
            {
                let mut sched = scheduler_lock();
                if idx < sched.tasks.len() {
                    bs.state = TaskState::Running;
                    if bs.wake_deadline.take().is_some() {
                        ACTIVE_WAKE_DEADLINES.fetch_sub(1, Ordering::Relaxed);
                    }
                    sched.tasks[idx].state = TaskState::Running;
                    sched.tasks[idx].wake_deadline = None;
                    sched.tasks[idx].on_cpu.store(false, Ordering::Release);
                }
                // scheduler_lock released.
            }
            // pi_lock released.
        }
        // Restore current_task_idx so callers see us as Running again.
        set_current_task_idx(Some(idx));
        return if already_expired {
            BlockOutcome::DeadlineExpired
        } else {
            BlockOutcome::AlreadyTrue
        };
    }

    // ── Step 4: Yield via SCHEDULER.lock → switch_context ────────────────────
    //
    // Store task idx so the dispatch handler can save RSP after switch_context.
    PENDING_REENQUEUE[core as usize].store(idx as i32, Ordering::Release);
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    #[cfg(feature = "sched-trace")]
    {
        let pid_for_trace = {
            let s = scheduler_lock();
            if idx < s.tasks.len() {
                s.tasks[idx].pid
            } else {
                0
            }
        };
        crate::task::sched_trace::record(pid_for_trace, TaskState::Running as u8, kind as u8);
    }
    // SAFETY: same invariants as block_current_unless_woken_inner — we are
    // the running task on this core, scheduler RSP is valid.
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };

    // On resume: the waker called wake_task which transitioned us to Ready and
    // re-enqueued us. The dispatch loop re-set current_task_idx. Now check why
    // we were woken.
    if woken.load(Ordering::Acquire) {
        BlockOutcome::Woken
    } else {
        // Woken by the deadline scanner (scan_expired_wake_deadlines).
        BlockOutcome::DeadlineExpired
    }
}

/// v2 helper: block the current task (as `BlockedOnReply`) until a message is
/// delivered into its pending slot.
///
/// This is the v2 replacement for `block_current_on_reply_unless_message`.
/// It wraps [`block_current_until`] using a **local** `AtomicBool` as the
/// `woken` flag. The flag starts `false`; the caller is responsible for the
/// condition being rechecked via `take_message` after this returns.
///
/// **Why a local `AtomicBool`?** During the Track C migration window the wake
/// side (`wake_task`) still uses the v1 path and does not set any per-call
/// flag.  The self-revert path in [`block_current_until`] checks
/// `pending_msg.is_some()` via the `woken` flag — but because the wake side
/// has not been migrated to the v2 protocol (Track D), the flag will always
/// be `false` at the step-3 recheck.  The function therefore always goes to
/// step 4 (yield) unless `pending_msg` is already set at entry (the early
/// `AlreadyTrue` return).  Once Track D migrates `wake_task`, wakers will set
/// the flag, enabling the no-yield fast path.
///
/// Returns `true` if the task was woken with a message (`Woken` or
/// `AlreadyTrue`), `false` on a spurious or deadline wake (the latter is not
/// possible on this path since no deadline is set, but is included for
/// type-safety).
///
/// # Call-site migration contract
///
/// Under `cfg(feature = "sched-v2")`, `call_msg` in `endpoint.rs` calls this
/// function instead of `block_current_on_reply_unless_message`.  The semantic
/// outcome is identical: the caller resumes when a reply is delivered.
pub fn block_current_on_reply_v2(caller: TaskId) -> bool {
    use core::sync::atomic::AtomicBool;

    // Check if a message was already delivered before we even try to block.
    {
        let sched = scheduler_lock();
        if sched
            .find(caller)
            .map(|idx| sched.tasks[idx].pending_msg.is_some())
            .unwrap_or(false)
        {
            return true;
        }
    }

    // Use a stack-allocated AtomicBool as the v2 woken flag.
    // Track D will set this from wake_task; for now it stays false until
    // the block side self-reverts (which cannot happen without Track D waking it).
    let woken = AtomicBool::new(false);

    let outcome = block_current_until(TaskState::BlockedOnReply, &woken, None);

    // Regardless of the outcome, the caller (call_msg) will call take_message()
    // to confirm message delivery. We report true for Woken/AlreadyTrue, false
    // for DeadlineExpired (no deadline set, so this branch is dead code for now).
    match outcome {
        BlockOutcome::Woken | BlockOutcome::AlreadyTrue => true,
        BlockOutcome::DeadlineExpired => false,
    }
}

/// Check whether a task has a pending message (for use by v2 wakers and
/// condition checks without holding the scheduler lock long-term).
///
/// Acquires `SCHEDULER.lock` momentarily.
pub fn has_pending_message(id: TaskId) -> bool {
    let sched = scheduler_lock();
    sched
        .find(id)
        .map(|idx| sched.tasks[idx].pending_msg.is_some())
        .unwrap_or(false)
}

/// v2 helper: block the current task (as `BlockedOnRecv`) until a message is
/// delivered into its pending slot.
///
/// This is the v2 replacement for `block_current_on_recv_unless_message` used
/// in `recv_msg`. It wraps [`block_current_until`] using a stack-allocated
/// `AtomicBool` as the `woken` flag, mirroring the pattern established by
/// [`block_current_on_reply_v2`] (Track C.4).
///
/// **Condition recheck (approach c):** The pending_msg pre-check is performed
/// in the IPC layer (endpoint.rs) before calling this function, and
/// `take_message` is called after return — the IPC layer owns the condition
/// logic. This helper only owns the block/yield/resume protocol.
///
/// Returns `true` if woken (`Woken` or `AlreadyTrue`), `false` on deadline
/// (no deadline is set for IPC recv, so this is dead code for now).
pub fn block_current_on_recv_v2(receiver: TaskId) -> bool {
    use core::sync::atomic::AtomicBool;

    // Fast path: message already delivered before we even try to block.
    {
        let sched = scheduler_lock();
        if sched
            .find(receiver)
            .map(|idx| sched.tasks[idx].pending_msg.is_some())
            .unwrap_or(false)
        {
            return true;
        }
    }

    let woken = AtomicBool::new(false);
    let outcome = block_current_until(TaskState::BlockedOnRecv, &woken, None);
    match outcome {
        BlockOutcome::Woken | BlockOutcome::AlreadyTrue => true,
        BlockOutcome::DeadlineExpired => false,
    }
}

/// v2 helper: block the current task (as `BlockedOnNotif`) until a message or
/// notification is delivered.
///
/// This is the v2 replacement for `block_current_on_notif_unless_message` used
/// in `recv_msg_with_notif`. Follows the same pattern as
/// [`block_current_on_reply_v2`] (Track C.4) and [`block_current_on_recv_v2`].
///
/// **Condition recheck (approach c):** The IPC layer (endpoint.rs) owns the
/// condition check (pending_msg / notification bits); this helper owns only the
/// block/yield/resume protocol under `block_current_until`.
///
/// Returns `true` if woken, `false` on deadline (no deadline set → dead code).
pub fn block_current_on_notif_v2(receiver: TaskId) -> bool {
    use core::sync::atomic::AtomicBool;

    // Fast path: message already delivered.
    {
        let sched = scheduler_lock();
        if sched
            .find(receiver)
            .map(|idx| sched.tasks[idx].pending_msg.is_some())
            .unwrap_or(false)
        {
            return true;
        }
    }

    let woken = AtomicBool::new(false);
    let outcome = block_current_until(TaskState::BlockedOnNotif, &woken, None);
    match outcome {
        BlockOutcome::Woken | BlockOutcome::AlreadyTrue => true,
        BlockOutcome::DeadlineExpired => false,
    }
}

/// v2 helper: block the current task (as `BlockedOnSend`) until the send
/// operation is accepted by a receiver.
///
/// This is the v2 replacement for `block_current_on_send_unless_completed` used
/// in `send` and `send_with_cap`. Follows the same pattern as
/// [`block_current_on_reply_v2`] (Track C.4).
///
/// **Condition recheck (approach c):** The IPC layer owns the
/// `pending_msg.is_some() || send_completed` pre-check; this helper owns
/// only the block/yield/resume protocol.
///
/// Returns `true` if woken, `false` on deadline (no deadline → dead code).
pub fn block_current_on_send_v2(sender: TaskId) -> bool {
    use core::sync::atomic::AtomicBool;

    // Fast path: send already completed (receiver picked us up) or error
    // message delivered.
    {
        let mut sched = scheduler_lock();
        if let Some(idx) = sched.find(sender)
            && (sched.tasks[idx].pending_msg.is_some() || sched.tasks[idx].send_completed)
        {
            sched.tasks[idx].send_completed = false;
            return true;
        }
    }

    let woken = AtomicBool::new(false);
    let outcome = block_current_until(TaskState::BlockedOnSend, &woken, None);

    // Clear send_completed flag after waking (mirrors v1 block_current_on_send_unless_completed).
    {
        let mut sched = scheduler_lock();
        if let Some(idx) = sched.find(sender) {
            sched.tasks[idx].send_completed = false;
        }
    }

    match outcome {
        BlockOutcome::Woken | BlockOutcome::AlreadyTrue => true,
        BlockOutcome::DeadlineExpired => false,
    }
}

/// Permanently mark the current task as dead and switch back to the scheduler.
pub fn mark_current_dead() -> ! {
    let idx = {
        let mut sched = scheduler_lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => loop {
                x86_64::instructions::hlt();
            },
        };
        sched.tasks[idx].state = TaskState::Dead;
        // E.1: mark RSP-publication window.
        sched.tasks[idx].on_cpu.store(true, Ordering::Release);
        set_current_task_idx(None);
        idx
    };
    PENDING_REENQUEUE[crate::smp::per_core().core_id as usize].store(idx as i32, Ordering::Release);
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };
    loop {
        x86_64::instructions::hlt();
    }
}

/// Mark a task as [`TaskState::Dead`] by its process/thread PID.
///
/// Used by `exit_group()` to kill sibling threads.  Returns `true` if the
/// task was found and marked dead, `false` otherwise.
pub fn mark_task_dead_by_pid(pid: u32) -> bool {
    // Lock-order: pi_lock OUTER, scheduler_lock INNER, BOTH HELD ATOMICALLY
    // for the canonical + mirror writes. Splitting them allows the
    // scheduler-visible identity check to fail after canonical state is
    // already Dead, leaving the task dead in pi_lock but live in Task::state.
    let captured = {
        let sched = scheduler_lock();
        sched
            .tasks
            .iter()
            .position(|t| t.pid == pid)
            .map(|idx| (idx, &raw const sched.tasks[idx].pi_lock))
    };
    let (idx, pi_lock_ptr) = match captured {
        Some(x) => x,
        None => return false,
    };
    // Acquire pi_lock OUTER + scheduler_lock INNER for atomic write.
    // SAFETY: tasks[idx] is stable while idx remains valid (the Vec only
    // grows; dead-slot recycling runs under SCHEDULER.lock, which we hold).
    let pi_lock_ref = unsafe { &*pi_lock_ptr };
    let mut bs = pi_lock_ref.lock();
    let mut sched = scheduler_lock();
    if idx >= sched.tasks.len() || sched.tasks[idx].pid != pid {
        // Task identity changed between collection and lock — refuse, leaving
        // both canonical and scheduler-visible state untouched.
        return false;
    }
    bs.state = TaskState::Dead;
    if bs.wake_deadline.take().is_some() {
        ACTIVE_WAKE_DEADLINES.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
    sched.tasks[idx].group_exit_pending = false;
    sched.tasks[idx].state = TaskState::Dead;
    sched.tasks[idx].wake_deadline = None;
    true
    // Both locks released as the guards drop on return.
}

/// Request that the task with `pid` stop itself on its own core.
pub fn request_group_exit_by_pid(pid: u32) -> bool {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find_by_pid(pid) {
        sched.tasks[idx].group_exit_pending = true;
        true
    } else {
        false
    }
}

/// Consume the current task's pending `exit_group()` stop request.
pub fn take_current_group_exit_request() -> bool {
    let idx = match get_current_task_idx() {
        Some(idx) => idx,
        None => return false,
    };
    let mut sched = scheduler_lock();
    if idx >= sched.tasks.len() {
        return false;
    }
    let pending = sched.tasks[idx].group_exit_pending;
    sched.tasks[idx].group_exit_pending = false;
    pending
}

/// Atomically confirm that a sibling is off-core and mark it dead so it can
/// be reaped by another thread in the same group.
pub fn quiesce_task_for_remote_reap_by_pid(pid: u32) -> bool {
    // Lock-order: pi_lock OUTER, scheduler_lock INNER, BOTH HELD ATOMICALLY
    // for the quiescence check + canonical/mirror write.  Splitting them
    // means the task could become non-quiescent (current/on-CPU again)
    // between the canonical Dead write and the scheduler_lock check —
    // we'd then return false while pi_lock state is already Dead.
    let captured = {
        let sched = scheduler_lock();
        sched
            .find_by_pid(pid)
            .map(|idx| (idx, &raw const sched.tasks[idx].pi_lock))
    };
    let (idx, pi_lock_ptr) = match captured {
        Some(x) => x,
        None => return false,
    };
    // Acquire both locks before any mutation; verify quiescence with both held.
    // SAFETY: tasks[idx] is stable while idx remains valid (scheduler_lock
    // gates dead-slot recycling).
    let pi_lock_ref = unsafe { &*pi_lock_ptr };
    let mut bs = pi_lock_ref.lock();
    let mut sched = scheduler_lock();
    let Some(check_idx) = sched.find_by_pid(pid) else {
        return false;
    };
    if check_idx != idx
        || sched.task_current_on_any_core(idx)
        || sched.tasks[idx].on_cpu.load(Ordering::Acquire)
    {
        return false;
    }
    // Atomic Dead write under both locks.
    bs.state = TaskState::Dead;
    if bs.wake_deadline.take().is_some() {
        ACTIVE_WAKE_DEADLINES.fetch_sub(1, Ordering::Relaxed);
    }
    sched.tasks[idx].state = TaskState::Dead;
    sched.tasks[idx].wake_deadline = None;
    sched.tasks[idx].group_exit_pending = false;
    true
}

// ---------------------------------------------------------------------------
// D.1 + D.2 — v2 wake primitive (sched-v2 feature gate)
// ---------------------------------------------------------------------------
//
// `wake_task_v2` is the CAS-style wake primitive for the v2 scheduler
// protocol.  It replaces the v1 `wake_task` incrementally, gated behind
// `cfg(feature = "sched-v2")`.
//
// During the C–F migration window, `wake_task_v2` performs a **dual write**:
// it mutates `TaskBlockState` (under `pi_lock`) AND the legacy `Task::state`
// / `Task::wake_deadline` fields (under `SCHEDULER.lock`).  Track E removes
// the legacy fields once all call sites are migrated.
//
// # Linux citation
//
// The implementation mirrors Linux's `try_to_wake_up` in
// `kernel/sched/core.c`:
// - CAS `p->__state` from TASK_INTERRUPTIBLE/UNINTERRUPTIBLE to TASK_RUNNING
//   under `p->pi_lock`.
// - `smp_cond_load_acquire`-style spin-wait on `p->on_cpu == 0` before
//   `ttwu_queue` (enqueue to the task's home run queue).
// - `ttwu_queue` sends a reschedule IPI when the task is on a different CPU.
//
// See also: m3OS handoff 2026-04-25,
// `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md` (wake side steps
// 1–5).

/// Outcome of a [`wake_task_v2`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOutcome {
    /// CAS `Blocked* → Ready` succeeded; the task has been enqueued on its
    /// assigned core's run queue.  A reschedule IPI was sent if the target
    /// core differs from the caller's core.
    Woken,
    /// CAS failed because the task's canonical state was not `Blocked*`
    /// (it was `Ready`, `Running`, or `Dead`).  The caller may treat this
    /// as a silent no-op — the task will recheck its condition on resume.
    AlreadyAwake,
}

/// v2 wake primitive — CAS `Blocked* → Ready` under `pi_lock`.
///
/// # Protocol (five steps, mirroring Linux `try_to_wake_up`)
///
/// 1. **Find the task index** from the `TaskId` via `SCHEDULER.lock`; then
///    release `SCHEDULER.lock` to preserve lock-ordering (pi_lock OUTER).
/// 2. **Acquire `pi_lock`**; CAS `state` from any `Blocked*` to `Ready`;
///    clear `wake_deadline` (`ACTIVE_WAKE_DEADLINES--` if `Some`); release
///    `pi_lock`.  Returns `AlreadyAwake` if the CAS fails.
/// 3. **Mirror to v1 fields** under `SCHEDULER.lock` (shadow-lock dual write,
///    required until Track E.3 removes `Task::state` / `Task::wake_deadline`).
/// 4. **Spin-wait** on `Task::on_cpu == false` before enqueuing (Linux
///    `p->on_cpu` `smp_cond_load_acquire` pattern, `kernel/sched/core.c`,
///    `try_to_wake_up`).  This replaces v1's `PENDING_SWITCH_OUT[core]`
///    RSP-publication guard.
/// 5. **Enqueue** to `assigned_core` run queue via [`enqueue_to_core`].
///    If the assigned core differs from the caller's core, [`enqueue_to_core`]
///    already sends a reschedule IPI (`IPI_RESCHEDULE` vector, `smp::ipi`),
///    satisfying the D.2 cross-core IPI requirement.
///
/// # Lock ordering
///
/// `pi_lock` is OUTER; `SCHEDULER.lock` is INNER (Linux's `p->pi_lock` →
/// `rq->lock` pattern).  Step 1 captures `idx` + `pi_lock_ptr` under a
/// brief `SCHEDULER.lock` and drops it.  Steps 2+3 acquire `pi_lock` OUTER
/// and then `SCHEDULER.lock` INNER **simultaneously** so the canonical CAS
/// and the scheduler-visible mirror are atomic with respect to a racing
/// `block_current_until` self-revert.  Step 4's `on_cpu` spin-wait and
/// step 5's enqueue run with both locks dropped (Linux pattern); a
/// self-revert that interleaves there is harmless — `pick_next`'s
/// `state != Ready` filter silently drops any stale queue entry.
///
/// # Identity revalidation
///
/// `idx` and the captured raw pointers persist across the SCHEDULER.lock
/// drop in step 1; if the slot was recycled (Dead → free list →
/// allocate_task at e.g. scheduler.rs:821) before we re-acquire
/// SCHEDULER.lock in step 3, the slot now holds a DIFFERENT task at the
/// same memory address.  Step 3 therefore revalidates
/// `sched.tasks[idx].id == id` BEFORE writing — on mismatch we return
/// `AlreadyAwake` and never mutate the (recycled) task.  Briefly locking
/// the recycled task's pi_lock for the validation read is benign: we read
/// state, find identity mismatch, release without writing.
///
/// `assigned_core` and `on_cpu_ptr` are also re-read inside the validated
/// critical section so step 4's spin-wait and step 5's enqueue use values
/// fresh from the validated slot, not stale values from step 1's snapshot.
///
/// # Constraints
///
/// - Does NOT touch `wake_after_switch` or read `switching_out` (v1 fields).
/// - Safe to call from interrupt context (all locks are `IrqSafeMutex` or
///   `spin::Mutex`).
///
/// # References
///
/// - Linux `try_to_wake_up`, `kernel/sched/core.c` — `p->on_cpu`
///   `smp_cond_load_acquire` spin-wait before `ttwu_queue`; cross-core
///   IPI via `smp_send_reschedule` inside `ttwu_queue`.
/// - m3OS handoff 2026-04-25:
///   `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md` (wake side
///   steps 1–5).
pub fn wake_task_v2(id: TaskId) -> WakeOutcome {
    // ── Step 1: Find the task index + capture pi_lock pointer ────────────────
    //
    // The Vec never shrinks; slots are Dead-recycled but the memory at
    // `tasks[idx]` is stable.  We capture only `idx` and `pi_lock_ptr` here
    // because everything else (`assigned_core`, `on_cpu_ptr`) MUST be re-read
    // inside the validated critical section after revalidating the slot's
    // identity — otherwise a recycle between step 1 and step 3 would let
    // us spin / enqueue using stale values from the previous task in the
    // slot.
    let (idx, pi_lock_ptr) = {
        let sched = scheduler_lock();
        let idx = match sched.find(id) {
            Some(i) => i,
            None => return WakeOutcome::AlreadyAwake,
        };
        let pi_lock_ptr: *const IrqSafeMutex<super::TaskBlockState> =
            &raw const sched.tasks[idx].pi_lock;
        (idx, pi_lock_ptr)
        // SCHEDULER.lock dropped.
    };

    // ── Step 2+3: Atomic CAS + scheduler-visible mirror under both locks ────
    //
    // Acquire pi_lock OUTER, then SCHEDULER.lock INNER.  Both writes
    // (canonical TaskBlockState.state and Task::state) happen with both
    // locks held — atomic with respect to `block_current_until`'s
    // self-revert path.
    //
    // Identity revalidation: between step 1's SCHEDULER.lock drop and our
    // re-acquisition here, the slot may have been Dead-recycled into a
    // different task (allocate_task pushes into the same `tasks[idx]`
    // memory; pi_lock_ptr now references the NEW task's pi_lock).  We
    // briefly lock that pi_lock (harmless), then under SCHEDULER.lock
    // verify `tasks[idx].id == id`.  On mismatch we return AlreadyAwake
    // and never mutate the recycled task.
    //
    // SAFETY: `pi_lock_ptr` points into the stable `tasks[idx]` memory.
    // SCHEDULER.lock is NOT held when we acquire pi_lock.
    let now = crate::arch::x86_64::interrupts::tick_count();
    let post_lock = {
        let pi_lock_ref = unsafe { &*pi_lock_ptr };
        let mut guard = pi_lock_ref.lock();
        let mut sched = scheduler_lock();

        // Identity revalidation — slot may have been recycled.
        if idx >= sched.tasks.len() || sched.tasks[idx].id != id {
            return WakeOutcome::AlreadyAwake;
        }

        // CAS check (canonical state).
        let prev_state_u8 = guard.state as u8;
        if !matches!(
            guard.state,
            TaskState::BlockedOnRecv
                | TaskState::BlockedOnSend
                | TaskState::BlockedOnReply
                | TaskState::BlockedOnNotif
                | TaskState::BlockedOnFutex
        ) {
            return WakeOutcome::AlreadyAwake;
        }

        // Atomic canonical + scheduler-visible writes.
        guard.state = TaskState::Ready;
        if guard.wake_deadline.take().is_some() {
            ACTIVE_WAKE_DEADLINES.fetch_sub(1, Ordering::Relaxed);
        }
        sched.tasks[idx].state = TaskState::Ready;
        sched.tasks[idx].last_ready_tick = now;
        sched.tasks[idx].blocked_since_tick = 0;
        sched.tasks[idx].wake_deadline = None;
        #[cfg(feature = "sched-trace")]
        crate::task::sched_trace::record(
            sched.tasks[idx].pid,
            prev_state_u8,
            TaskState::Ready as u8,
        );
        crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::WakeTask {
            task_idx: idx as u32,
            state_before: prev_state_u8,
            core: sched.tasks[idx].assigned_core,
        });

        // Re-read `assigned_core` and `on_cpu_ptr` from the VALIDATED slot,
        // for use after both locks drop.
        let assigned: u8 = sched.tasks[idx].assigned_core;
        let on_cpu_ptr: *const core::sync::atomic::AtomicBool = &raw const sched.tasks[idx].on_cpu;
        (assigned, on_cpu_ptr)
        // SCHEDULER.lock released, then pi_lock released.
    };
    let (assigned_core, on_cpu_ptr) = post_lock;

    // ── Step 4: Spin-wait on Task::on_cpu == false (cross-core only) ─────────
    //
    // The arch-level switch-out epilogue clears `on_cpu` only after
    // `saved_rsp` is durably written to the task struct (with Release
    // ordering, Track E.1).  Spinning here with Acquire ordering guarantees
    // that our subsequent `enqueue_to_core` observes the published
    // `saved_rsp` so the dispatch path does not jump to a stale RSP.
    //
    // Linux analog: `smp_cond_load_acquire(&p->on_cpu, !VAL)` in
    // `try_to_wake_up` (`kernel/sched/core.c`).
    //
    // # Same-core escape
    //
    // If the waker is running on the task's `assigned_core`, we skip the
    // spin entirely.  Same-core wake is dispatch-safe by construction:
    //   1. `pick_next` on this core consumes this core's local queue, so
    //      our enqueue cannot be picked up until WE return.
    //   2. The interrupted (or blocking) task can't reach the dispatch
    //      epilogue — and therefore can't clear `on_cpu` — until WE
    //      return.  Spinning would be a guaranteed deadlock for
    //      same-core IRQ wakes (e.g. COM1 RX → wake_feeder_task →
    //      wake_task_v2 for the very task whose `block_current_until`
    //      the IRQ interrupted).
    //   3. After we return, the task either self-reverts (state=Running,
    //      our queue entry is silently filtered by dequeue_local's
    //      state==Ready check) or proceeds to switch_context (saved_rsp
    //      committed before pick_next can run on this core).
    //
    // SAFETY: `on_cpu_ptr` is a valid pointer to an `AtomicBool` in the
    // same stable Task struct as `pi_lock_ptr` (step 1 invariant).
    let waker_core = crate::smp::per_core().core_id;
    if assigned_core != waker_core {
        let on_cpu_ref = unsafe { &*on_cpu_ptr };
        while on_cpu_ref.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }

    // ── Step 5: Enqueue to assigned_core run queue + reschedule IPI ──────────
    //
    // SLOT-RECYCLE GUARD: between dropping pi_lock+SCHEDULER.lock at end of
    // step 3 and reaching here, the task we just CAS'd to Ready could have
    // been:
    //   1. Marked Dead by another path (e.g. `mark_task_dead_by_pid`).
    //   2. Drained to the free list by BSP cleanup (scheduler.rs:~496),
    //      which only requires `state == Dead`, `ipc_cleaned`, `!on_cpu`,
    //      `saved_rsp != 0`, `!task_current` — all of which become true
    //      shortly after Dead.
    //   3. Have its slot reused by `alloc_task_slot` (scheduler.rs:~821)
    //      for a brand-new task with a different `TaskId`.
    //
    // If we naively `enqueue_to_core(assigned_core, idx)` after that
    // sequence, we'd push the NEW task's idx onto a queue using the
    // OLD task's `assigned_core` — duplicate-enqueueing the new task
    // and possibly placing it on the wrong core's run queue.
    //
    // Take SCHEDULER.lock for the enqueue and revalidate `tasks[idx].id`.
    // SCHEDULER.lock is the gate for both Dead-state mirror writes and
    // alloc_task_slot, so holding it serialises us against recycle.
    // Re-read `assigned_core` under the lock too, since load-balance
    // could have migrated the (still-our) task between step 3 and now.
    //
    // D.2: `enqueue_to_core` already sends a reschedule IPI when the
    // target differs from the caller's core; `wait_icr_idle()` is
    // bounded so spinning on it under SCHEDULER.lock is safe.
    //
    // Linux analog: `ttwu_queue` runs under `rq->lock`; `try_to_wake_up`
    // re-checks `p->state` after the on_cpu spin before the actual
    // enqueue.
    {
        let sched = scheduler_lock();
        if idx >= sched.tasks.len() || sched.tasks[idx].id != id {
            // Slot recycled or task vanished — abandon the enqueue.  The
            // new task occupying this slot (if any) has its own enqueue
            // path via `spawn_fork_task`.
            return WakeOutcome::AlreadyAwake;
        }
        let live_assigned_core = sched.tasks[idx].assigned_core;
        // Drop the lock before enqueue_to_core (which takes the per-core
        // run_queue lock and may send an IPI — both safe outside
        // SCHEDULER.lock).  We've now confirmed the slot identity and
        // captured the live `assigned_core`.
        drop(sched);
        enqueue_to_core(live_assigned_core, idx);
    }

    WakeOutcome::Woken
}

/// Store a [`Message`] in a task's pending slot.
pub fn deliver_message(id: TaskId, msg: Message) {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_msg = Some(msg);
    }
}

/// Store a [`Message`] only if the task's pending slot is empty.
///
/// Returns `true` if the message was installed. Used by signal delivery so
/// a racing legitimate server reply already parked in `pending_msg` is not
/// clobbered by the EINTR sentinel — the signal simply remains pending and
/// fires on the next syscall boundary.
pub fn try_deliver_message(id: TaskId, msg: Message) -> bool {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id)
        && sched.tasks[idx].pending_msg.is_none()
    {
        sched.tasks[idx].pending_msg = Some(msg);
        return true;
    }
    false
}

/// Remove every `Capability::Reply(target)` from every task's capability
/// table.
///
/// Called when signal delivery pulls `target` out of an IPC wait: any server
/// still holding a reply cap for that caller would otherwise be able to drop
/// a late reply into the caller's pending slot, which a subsequent
/// `ipc_call` would consume as its own reply. Dropping the reply caps makes
/// the stale `ipc_reply` fail fast (with `u64::MAX`) instead.
pub fn revoke_reply_caps_for(target: TaskId) {
    let mut sched = scheduler_lock();
    for task in sched.tasks.iter_mut() {
        task.caps
            .revoke_matching(|cap| matches!(cap, Capability::Reply(t) if *t == target));
    }
}

/// Remove and return the pending message for a task.
pub fn take_message(id: TaskId) -> Option<Message> {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_msg.take()
    } else {
        None
    }
}

/// Store bulk data alongside a pending message (Phase 52).
pub fn deliver_bulk(id: TaskId, data: alloc::vec::Vec<u8>) {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_bulk = Some(data);
    }
}

/// Mark that a blocked or soon-to-block sender has had its message consumed.
pub fn complete_send(id: TaskId) {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].send_completed = true;
    }
}

/// Remove and return the pending bulk data for a task (Phase 52).
pub fn take_bulk_data(id: TaskId) -> Option<alloc::vec::Vec<u8>> {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_bulk.take()
    } else {
        None
    }
}

/// Insert a capability into a task's capability table.
pub fn insert_cap(id: TaskId, cap: Capability) -> Result<CapHandle, CapError> {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].caps.insert(cap)
    } else {
        Err(CapError::InvalidHandle)
    }
}

/// Insert a capability into a task's capability table at a specific slot.
pub fn insert_cap_at(id: TaskId, handle: CapHandle, cap: Capability) -> Result<(), CapError> {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].caps.insert_at(handle, cap)
    } else {
        Err(CapError::InvalidHandle)
    }
}

/// Look up a capability in a task's capability table.
pub fn task_cap(id: TaskId, handle: CapHandle) -> Result<Capability, CapError> {
    let sched = scheduler_lock();
    sched.cap(id, handle)
}

/// Remove a capability from a task's capability table.
pub fn remove_task_cap(id: TaskId, handle: CapHandle) -> Result<Capability, CapError> {
    let mut sched = scheduler_lock();
    sched.remove_cap(id, handle)
}

/// Atomically transfer a capability between two tasks.
pub fn grant_task_cap(
    source_id: TaskId,
    source_handle: CapHandle,
    target_id: TaskId,
) -> Result<CapHandle, CapError> {
    let mut sched = scheduler_lock();
    sched.grant_cap(source_id, source_handle, target_id)
}

/// Register the endpoint this task acts as server for.
pub fn set_server_endpoint(id: TaskId, ep_id: EndpointId) {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].server_endpoint = Some(ep_id);
    }
}

/// Return the server endpoint for a task.
pub fn server_endpoint(id: TaskId) -> Option<EndpointId> {
    let sched = scheduler_lock();
    sched.server_endpoint(id)
}

/// Return the notification capabilities currently held by `id`.
pub fn task_notification_caps(id: TaskId) -> alloc::vec::Vec<NotifId> {
    let sched = scheduler_lock();
    sched.notification_caps(id)
}

/// Return the scheduler task-vec index for `id`, if it is still live.
pub fn task_idx_for_task_id(id: TaskId) -> Option<usize> {
    let sched = scheduler_lock();
    sched.find(id)
}

/// Mark that per-task IPC teardown has completed for `id`.
pub fn mark_ipc_cleaned(id: TaskId) {
    let mut sched = scheduler_lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].ipc_cleaned = true;
    }
}

#[cfg(test)]
fn test_task_entry() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
pub(crate) fn install_test_task_idx(task_id: TaskId, idx: usize) {
    let mut sched = scheduler_lock();
    while sched.tasks.len() <= idx {
        let mut filler = Task::new(test_task_entry, "test-filler");
        // TODO(57a-C/D): route through pi_lock + with_block_state
        filler.state = TaskState::Dead;
        sched.tasks.push(Box::new(filler));
    }

    let mut task = Task::new(test_task_entry, "test-cleanup");
    task.id = task_id;
    // TODO(57a-C/D): route through pi_lock + with_block_state
    task.state = TaskState::Ready;
    sched.tasks[idx] = Box::new(task);
}

/// Return whether any live task other than `excluding` still holds a cap to
/// `ep_id`.
pub fn other_task_holds_endpoint_cap(excluding: TaskId, ep_id: EndpointId) -> bool {
    let sched = scheduler_lock();
    sched.other_task_holds_endpoint_cap(excluding, ep_id)
}

/// Return the callers currently waiting on reply capabilities held by `id`.
pub fn reply_waiters(id: TaskId) -> alloc::vec::Vec<TaskId> {
    let sched = scheduler_lock();
    sched.reply_waiters(id)
}

/// Return blocked task ids that belong to `pid` and are currently sleeping in
/// IPC wait states.
pub fn blocked_ipc_task_ids_for_pid(pid: u32) -> alloc::vec::Vec<TaskId> {
    let sched = scheduler_lock();
    sched
        .tasks
        .iter()
        .filter(|task| {
            task.pid == pid
                && matches!(
                    task.state,
                    TaskState::BlockedOnRecv | TaskState::BlockedOnSend | TaskState::BlockedOnReply
                )
        })
        .map(|task| task.id)
        .collect()
}

/// The main scheduler loop. Called once per core. Never returns.
///
/// Each core runs its own instance. The per-core reschedule flag gates
/// iteration; per-core run queues provide task selection locality. However,
/// the global `SCHEDULER` lock is acquired on each iteration for task state
/// reads, state transitions, and post-switch bookkeeping (see module doc).
pub fn run() -> ! {
    let core_id = crate::smp::per_core().core_id;
    // Phase 57 DEBUG: log scheduler-loop iterations per core so we can
    // tell whether a "stuck" core is actually executing the loop at all.
    // Bumped from 4 to 1024 (Phase 57a follow-up) so a per-core dispatch
    // failure that emerges AFTER the initial 4 events is still visible
    // in the boot transcript.  Once the per-core dispatch regression is
    // root-caused, restore the smaller budget.
    let mut wake_log_budget: u32 = 1024;
    let mut dispatch_log_budget: u32 = 1024;
    let mut resume_log_budget: u32 = 1024;

    loop {
        let reschedule = per_core_reschedule();

        interrupts::disable();
        if !reschedule.swap(false, Ordering::AcqRel) {
            interrupts::enable_and_hlt();
            continue;
        }
        interrupts::enable();

        if wake_log_budget > 0 {
            log::info!("[sched] run-loop wake core={}", core_id);
            wake_log_budget -= 1;
        }

        debug_assert!(
            per_core_scheduler_rsp() != 0,
            "core {}: scheduler RSP is zero",
            core_id
        );

        // Phase 52: drain per-core ISR wakeup queue (lock-free fast path).
        // ISRs push task indices here via signal_irq(); we wake them directly
        // without waiting for the tick-driven drain_pending_waiters().
        if let Some(data) = crate::smp::get_core_data(core_id) {
            for task_idx in data.isr_wake_queue.drain() {
                // Look up the TaskId for this idx (briefly under scheduler_lock).
                let task_id = {
                    let sched = scheduler_lock();
                    if task_idx < sched.tasks.len() {
                        Some(sched.tasks[task_idx].id)
                    } else {
                        None
                    }
                };
                // Route through wake_task_v2 — it handles the pi_lock CAS,
                // on_cpu spin-wait, scheduler_lock mirror, and enqueue
                // atomically. The CAS only succeeds for Blocked* states, so
                // tasks that are already Ready/Running are silently dropped
                // (idempotent wake) — equivalent to the previous BlockedOnNotif
                // gate but now also covers BlockedOnRecv/Send/Reply/Futex.
                if let Some(id) = task_id {
                    let _ = wake_task_v2(id);
                }
            }
        }

        // Drain notification waiters (only BSP does this to avoid contention).
        // Kept as a safety net: if the ISR wakeup queue was full or the ISR
        // fired before the waiter registered in ISR_WAITERS, this fallback
        // will still catch the pending notification.
        if core_id == 0 {
            crate::ipc::notification::drain_pending_waiters();
        }

        // Remove dead tasks (BSP only to avoid contention).
        if core_id == 0 {
            for task_id in dead_tasks_needing_ipc_cleanup() {
                crate::ipc::cleanup::cleanup_task_ipc(task_id);
            }
            let mut sched = scheduler_lock();
            sched.drain_dead();
        }

        // Periodic load balancing with per-task cooldown (Phase 52c A.4).
        if core_id == 0 {
            maybe_load_balance();
        }

        // G.1: Periodic stuck-task watchdog scan (BSP only, same convention as
        // drain_dead / maybe_load_balance). Every WATCHDOG_SCAN_INTERVAL_TICKS
        // ticks, logs WARN for any Blocked* task with no pending waker or with
        // an expired deadline. See kernel/src/task/watchdog.rs.
        if core_id == 0 {
            crate::task::watchdog::watchdog_scan();
        }

        // Before picking next, wake any tasks whose `wake_deadline` has
        // elapsed. This is the task-context replacement for a timer-ISR
        // force-wake — it runs under `SCHEDULER.lock` already and cannot
        // deadlock on same-core ISR re-entrance.
        //
        // Enqueue is deferred until after the lock is released because
        // `enqueue_to_core` sends a cross-core reschedule IPI that must
        // not run with `SCHEDULER.lock` held.
        // Phase 57a follow-up: collect expired-deadline candidates under
        // SCHEDULER.lock, then drive each through `wake_task_v2` with
        // SCHEDULER.lock dropped.  Restores the pi_lock-outer / SCHEDULER.lock-
        // inner invariant that the previous in-place scan violated.
        drive_expired_wake_deadlines();

        // Pick the next ready task and atomically mark it Running.
        // `stale_info` carries (pid, name, last_ready_tick, ticks_stale) when
        // ready-to-running latency exceeds the diagnostic threshold; logged
        // after the lock is dropped (Phase 54 diagnostic).
        let mut stale_info: Option<(u32, &'static str, u64, u64)> = None;
        let next = {
            let mut sched = scheduler_lock();
            if let Some((rsp, idx)) = sched.pick_next(core_id) {
                let now = crate::arch::x86_64::interrupts::tick_count();
                let is_idle = sched.idle_tasks.contains(&Some(idx));
                let task = &mut sched.tasks[idx];
                let stale_ticks = now.saturating_sub(task.last_ready_tick);
                // 50 ticks ≈ 500 ms at 100 Hz — catches the 1-second hang
                // pattern without spamming on normal brief waits.
                if stale_ticks >= 50 && !is_idle {
                    stale_info = Some((task.pid, task.name, task.last_ready_tick, stale_ticks));
                }
                // TODO(57a-C/D): route through pi_lock + with_block_state
                task.state = TaskState::Running;
                task.start_tick = now;
                debug_assert!(
                    task.state == TaskState::Running,
                    "dispatch: task idx={} not Running after mark on core {}",
                    idx,
                    core_id
                );
                set_current_task_idx(Some(idx));
                crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::Dispatch {
                    task_idx: idx as u32,
                    core: core_id,
                    rsp,
                });
                Some((rsp, idx))
            } else {
                None
            }
        };

        if let Some((pid, name, last_ready, stale_ticks)) = stale_info {
            // stale_ticks is already in ms: TICKS_PER_SEC = 1000, so 1 tick = 1 ms.
            // The old `stale_ticks * 10` assumed a 100 Hz timer (10 ms/tick) — G.3 fix.
            log::warn!(
                "[sched] stale-ready: pid={} name={} core={} stale~{} ms (ready_at_tick={})",
                pid,
                name,
                core_id,
                stale_ticks,
                last_ready
            );
        }

        let (task_rsp, _task_idx) = match next {
            Some(t) => t,
            None => continue,
        };
        debug_assert!(
            task_rsp != 0,
            "dispatch: task {} has zero saved_rsp on core {}",
            _task_idx,
            core_id
        );

        // Restore per-core process context for the dispatched task.
        //
        // Phase 52d B.2: all thread-local return-state fields (user_rsp,
        // kernel_stack_top, fs_base, CR3) are now restored from the task's
        // `UserReturnState` — the single authoritative source of truth set
        // at syscall entry (B.1).  The `Process` table is only consulted
        // for the address-space pointer needed for TLB tracking.
        {
            let pid = task_pid(_task_idx);
            crate::process::set_current_pid(pid);
            let old_as_ptr = if crate::smp::is_per_core_ready() {
                crate::smp::per_core().current_addrspace
            } else {
                core::ptr::null()
            };
            let mut new_as_ptr: *const crate::mm::AddressSpace = core::ptr::null();
            // Keep a live Arc guard so the AddressSpace is not freed
            // between the PROCESS_TABLE lock drop and the later
            // activate/deactivate calls.
            let mut new_as_guard: Option<alloc::sync::Arc<crate::mm::AddressSpace>> = None;
            if pid != 0 {
                // Read the task's UserReturnState (authoritative source).
                let urs = {
                    let sched = scheduler_lock();
                    sched.get_task(_task_idx).and_then(|t| t.user_return)
                };
                // Read the address-space pointer for TLB tracking — still
                // derived from Process because the raw pointer management
                // is a per-core concern, not part of the resume contract.
                new_as_guard = {
                    let table = crate::process::PROCESS_TABLE.lock();
                    table.find(pid).and_then(|p| p.addr_space.clone())
                };
                new_as_ptr = new_as_guard
                    .as_deref()
                    .map(|a| a as *const crate::mm::AddressSpace)
                    .unwrap_or_default();

                if let Some(urs) = urs {
                    // Restore CR3 from task-owned state.
                    if urs.cr3_phys != 0 {
                        unsafe {
                            use x86_64::{
                                PhysAddr,
                                registers::control::{Cr3, Cr3Flags},
                                structures::paging::{PhysFrame, Size4KiB},
                            };
                            let frame: PhysFrame<Size4KiB> =
                                PhysFrame::containing_address(PhysAddr::new(urs.cr3_phys));
                            Cr3::write(frame, Cr3Flags::empty());
                        }
                        #[cfg(debug_assertions)]
                        {
                            let (loaded_frame, _) = x86_64::registers::control::Cr3::read();
                            debug_assert_eq!(
                                loaded_frame.start_address().as_u64(),
                                urs.cr3_phys,
                                "CR3 mismatch after load on core {}",
                                core_id
                            );
                        }
                    }
                    // Restore kernel stack top (TSS.RSP0 + per-core SYSCALL_STACK_TOP).
                    if urs.kernel_stack_top != 0 {
                        crate::smp::set_current_core_kernel_stack(urs.kernel_stack_top);
                        unsafe {
                            crate::arch::x86_64::syscall::set_per_core_syscall_stack_top(
                                urs.kernel_stack_top,
                            );
                        }
                    }
                    // Restore FS.base (TLS pointer).
                    x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(
                        urs.fs_base,
                    ));
                    // Restore per-core syscall_user_rsp.
                    let data = crate::smp::per_core() as *const crate::smp::PerCoreData
                        as *mut crate::smp::PerCoreData;
                    unsafe {
                        (*data).syscall_user_rsp = urs.user_rsp;
                    }
                } else {
                    // Fallback for tasks that have not yet entered syscall_handler
                    // (e.g. freshly forked children before first dispatch).
                    // Read from PROCESS_TABLE as the legacy path.
                    let (cr3_phys, kstack, fs) = {
                        let table = crate::process::PROCESS_TABLE.lock();
                        match table.find(pid) {
                            Some(p) => (
                                p.addr_space.as_ref().map(|a| a.pml4_phys()),
                                p.kernel_stack_top,
                                p.fs_base,
                            ),
                            None => (None, 0, 0),
                        }
                    };
                    if let Some(cr3) = cr3_phys {
                        unsafe {
                            use x86_64::{
                                PhysAddr,
                                registers::control::{Cr3, Cr3Flags},
                                structures::paging::{PhysFrame, Size4KiB},
                            };
                            let frame: PhysFrame<Size4KiB> =
                                PhysFrame::containing_address(PhysAddr::new(cr3.as_u64()));
                            Cr3::write(frame, Cr3Flags::empty());
                        }
                    }
                    if kstack != 0 {
                        crate::smp::set_current_core_kernel_stack(kstack);
                        unsafe {
                            crate::arch::x86_64::syscall::set_per_core_syscall_stack_top(kstack);
                        }
                    }
                    x86_64::registers::model_specific::FsBase::write(x86_64::VirtAddr::new(fs));
                    // Log a diagnostic — this path should only be hit during
                    // first dispatch of a new task.
                    log::trace!(
                        "[sched] dispatch task {} pid={} via PROCESS_TABLE fallback on core {}",
                        _task_idx,
                        pid,
                        core_id
                    );
                }
            }
            // Update active-core tracking only after the new CR3 is actually
            // loaded on this core. Otherwise targeted TLB shootdowns can skip
            // the still-active old address space.
            if crate::smp::is_per_core_ready() {
                if pid == 0 && !old_as_ptr.is_null() {
                    crate::mm::restore_kernel_cr3();
                }
                let pc = crate::smp::per_core();
                if !old_as_ptr.is_null() && old_as_ptr != new_as_ptr {
                    unsafe { &*old_as_ptr }.deactivate_on_core(core_id);
                }
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                if let Some(new_as) = new_as_guard.as_deref()
                    && old_as_ptr != new_as_ptr
                {
                    new_as.activate_on_core(core_id);
                }
                let pc_mut = pc as *const crate::smp::PerCoreData as *mut crate::smp::PerCoreData;
                unsafe { (*pc_mut).current_addrspace = new_as_ptr };
            }
        }

        // F.1: Validate saved_rsp falls within the task's kernel stack.
        {
            let sched = scheduler_lock();
            if let Some(task) = sched.get_task(_task_idx)
                && let Some((base, top)) = task.stack_bounds()
            {
                debug_assert!(
                    task_rsp >= base && task_rsp < top,
                    "dispatch: task {} saved_rsp={:#x} outside stack [{:#x}..{:#x}] on core {}",
                    _task_idx,
                    task_rsp,
                    base,
                    top,
                    core_id
                );
            }
        }

        // Phase 57 DEBUG: log per-core dispatches just before
        // switch_context. Pairs with `resume` log below to detect a
        // dispatch that hangs / never returns to the scheduler.
        if dispatch_log_budget > 0 {
            log::info!(
                "[sched] dispatch core={} task_idx={} task_rsp={:#x}",
                core_id,
                _task_idx,
                task_rsp
            );
            dispatch_log_budget -= 1;
        }

        // Switch to the task.
        unsafe {
            switch_context(per_core_scheduler_rsp_ptr(), task_rsp);
        }

        // Phase 57b C.2 — switch-out retarget.
        //
        // `switch_context` has just returned onto this scheduler stack.  It
        // `popf`d the scheduler's saved RFLAGS, typically restoring IF=1 — so
        // we cannot assume IRQs are masked here.  Retarget back to the
        // per-core dummy *before* any scheduler-context `IrqSafeMutex::lock`
        // call (the next one is inside the `pending >= 0` block below) so
        // that scheduler-context lock acquire/release pairs charge the same
        // pointee.
        //
        // C.3 (next commit) will wire the matching switch-in retarget that
        // pivots the pointer to the chosen task's `Task::preempt_count`.
        retarget_preempt_count_to_dummy(core_id);

        // --- Scheduler resumes here after the task yields back ---
        if resume_log_budget > 0 {
            log::info!("[sched] resume core={}", core_id);
            resume_log_budget -= 1;
        }
        // The task's RSP has now been saved by switch_context. Commit bookkeeping.
        let pending = PENDING_REENQUEUE[core_id as usize].swap(-1, Ordering::Acquire);
        if pending >= 0 {
            let sidx = pending as usize;
            let saved_rsp = take_per_core_switch_save_rsp(core_id as usize);
            // `hog_info` carries (pid, name, ran_ticks, final_state) when the
            // task held the CPU for longer than the diagnostic threshold;
            // logged after the SCHEDULER lock is dropped.
            let mut hog_info: Option<(
                u32,
                &'static str,
                u64,
                TaskState,
                Option<alloc::string::String>,
            )> = None;
            let enqueue = {
                let mut sched = scheduler_lock();
                debug_assert!(
                    sidx < sched.tasks.len(),
                    "dispatch: switched task sidx={} out of bounds (len={}) on core {}",
                    sidx,
                    sched.tasks.len(),
                    core_id
                );
                if sidx < sched.tasks.len() {
                    let task = &mut sched.tasks[sidx];
                    task.saved_rsp = saved_rsp;
                    // E.1 epilogue: clear on_cpu AFTER saved_rsp is durably written.
                    // The Release ordering ensures that a concurrent waker observing
                    // on_cpu == false (via Acquire load in wake_task_v2's spin-wait)
                    // is guaranteed to see the published saved_rsp
                    // (Linux p->on_cpu smp_cond_load_acquire pattern, try_to_wake_up).
                    task.on_cpu.store(false, Ordering::Release);
                    // F.2: Validate saved_rsp after yield/block save.
                    if let Some((base, top)) = task.stack_bounds() {
                        debug_assert!(
                            saved_rsp >= base && saved_rsp < top,
                            "dispatch: saved task sidx={} rsp={:#x} outside stack [{:#x}..{:#x}] on core {}",
                            sidx,
                            saved_rsp,
                            base,
                            top,
                            core_id
                        );
                    }

                    // Phase 54 diagnostic: detect CPU-hog patterns. If a task
                    // held the CPU for >= 20 ticks (~200 ms) before yielding,
                    // log it. Suggests a syscall that spin-waits (e.g.
                    // virtio_blk poll under contention) rather than a normal
                    // interactive task.
                    let now = crate::arch::x86_64::interrupts::tick_count();
                    let ran_ticks = now.saturating_sub(task.start_tick);
                    if ran_ticks >= 20 {
                        let exec_path = if task.pid != 0 {
                            let table = crate::process::PROCESS_TABLE.lock();
                            table.find(task.pid).map(|proc| proc.exec_path.clone())
                        } else {
                            None
                        };
                        hog_info = Some((task.pid, task.name, ran_ticks, task.state, exec_path));
                    }
                    // Re-enqueue if the task yielded (still Running); blocked/dead
                    // tasks will be re-enqueued by wake_task_v2 after their waker fires.
                    if task.state == TaskState::Running {
                        task.state = TaskState::Ready;
                        task.last_ready_tick = now;
                        task.last_migrated_tick = now;
                        Some((task.assigned_core, sidx))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some((pid, name, ran_ticks, final_state, exec_path)) = hog_info {
                // ran_ticks is already in ms: TICKS_PER_SEC = 1000, so 1 tick = 1 ms.
                // The old `ran_ticks * 10` assumed a 100 Hz timer (10 ms/tick) — G.3 fix.
                log::warn!(
                    "[sched] cpu-hog: pid={} name={} exec_path={} core={} ran~{} ms final_state={:?}",
                    pid,
                    name,
                    exec_path.as_deref().unwrap_or("-"),
                    core_id,
                    ran_ticks,
                    final_state
                );
            }
            crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::SwitchOut {
                task_idx: sidx as u32,
                core: core_id,
                saved_rsp,
            });
            if let Some((target_core, idx)) = enqueue {
                enqueue_to_core(target_core, idx);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Load balancing (Phase 35 Track E, refined Phase 52c A.4)
// ---------------------------------------------------------------------------

/// Minimum ticks between migrations for a single task (Phase 52c A.4).
/// At 100 Hz timer, 100 ticks = 1 second cooldown.
const MIGRATE_COOLDOWN: u64 = 100;

/// Periodic load balancer tick counter. BSP calls `maybe_load_balance()`
/// from the scheduler loop; actual migration happens every 50 ticks
/// Counts tasks currently holding a non-`None` `wake_deadline`. Acts as a
/// fast-path gate for `scan_expired_wake_deadlines` — if zero, the full
/// `O(n_tasks)` scan is skipped on every dispatch. Incremented when
/// `block_current_unless_woken_until` sets a deadline; decremented when
/// `wake_task` clears one, when the scan expires one, or when a Blocked
/// task is found with a stale deadline on a non-Blocked state.
pub(crate) static ACTIVE_WAKE_DEADLINES: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// Collect TaskIds whose `wake_deadline` has expired.
///
/// **Lock-ordering invariant (Phase 57a B.3): pi_lock is OUTER, SCHEDULER.lock
/// is INNER.** This function runs under SCHEDULER.lock and therefore MUST NOT
/// touch any `pi_lock`.  The caller drops SCHEDULER.lock and then calls
/// `wake_task_v2` for each candidate — `wake_task_v2` does the proper
/// pi_lock-outer / scheduler_lock-inner CAS dance.
///
/// Why scheduler-lock-only is sufficient for collection:
///   - We read `Task::state` (the scheduler-visible mirror).  If a concurrent
///     `wake_task_v2` is in flight and racing with us, it has already CAS'd
///     `TaskBlockState.state` to Ready under pi_lock and is about to mirror
///     to `Task::state` under scheduler_lock.  At collection time we are
///     holding scheduler_lock so the waker is blocked on it — which means
///     either (a) the waker hasn't started its INNER scheduler_lock yet
///     (Task::state is still Blocked*) and we collect, then call
///     wake_task_v2 ourselves — its CAS sees Ready (set by the other waker
///     before our outer lookup) or Blocked* (we win) and behaves
///     idempotently; or (b) the waker already ran (Task::state == Ready)
///     and we skip.
///   - Spurious wakes (collecting a task whose deadline was just cleared) are
///     harmless: `wake_task_v2`'s pi_lock CAS only succeeds for Blocked*; if
///     the task already woke, we get `AlreadyAwake` (no-op).
fn collect_expired_wake_deadlines(sched: &Scheduler) -> ([TaskId; 8], usize) {
    if ACTIVE_WAKE_DEADLINES.load(Ordering::Relaxed) == 0 {
        return ([TaskId(0); 8], 0);
    }
    let now = crate::arch::x86_64::interrupts::tick_count();
    let mut expired: [TaskId; 8] = [TaskId(0); 8];
    let mut n = 0usize;

    for task in sched.tasks.iter() {
        if task.wake_deadline.is_none_or(|d| d > now) {
            continue;
        }
        if !matches!(
            task.state,
            TaskState::BlockedOnRecv
                | TaskState::BlockedOnSend
                | TaskState::BlockedOnReply
                | TaskState::BlockedOnNotif
                | TaskState::BlockedOnFutex
        ) {
            // Not Blocked* — stale deadline.  Don't touch it here; the next
            // state transition (or the next scan after wake_task_v2 clears
            // the deadline) will cover it.  Counters may temporarily lag but
            // are eventually consistent.
            continue;
        }
        if n < expired.len() {
            expired[n] = task.id;
            n += 1;
        }
    }

    (expired, n)
}

/// Drive deadline expiry for all tasks whose `wake_deadline` has passed.
///
/// Phase 1: collect candidate TaskIds under SCHEDULER.lock (no pi_lock touch).
/// Phase 2: drop SCHEDULER.lock, then call `wake_task_v2` for each candidate —
/// the canonical pi_lock-outer / scheduler_lock-inner wake path.
///
/// This replaces the previous `scan_expired_wake_deadlines` which violated
/// the lock-ordering invariant by acquiring pi_lock while holding
/// SCHEDULER.lock.
fn drive_expired_wake_deadlines() {
    let (expired, n) = {
        let sched = scheduler_lock();
        collect_expired_wake_deadlines(&sched)
        // SCHEDULER.lock dropped here.
    };
    for id in &expired[..n] {
        // wake_task_v2 acquires pi_lock OUTER, then scheduler_lock INNER.
        // CAS only succeeds for Blocked*; spurious calls (state already
        // Ready) return AlreadyAwake and are no-ops.
        let _ = wake_task_v2(*id);
    }
}

/// (~500ms at 100 Hz).
static BALANCE_COUNTER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Called from the BSP's scheduler loop. Every 50 ticks (~500ms), checks
/// queue imbalance and migrates one task if the longest queue exceeds the
/// shortest by >2. Tasks that were recently migrated are skipped (cooldown).
pub fn maybe_load_balance() {
    let cnt = BALANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    if !cnt.is_multiple_of(50) {
        return;
    }
    let cores = crate::smp::core_count();
    if cores <= 1 {
        return;
    }
    let mut longest_core = 0u8;
    let mut longest_len = 0usize;
    let mut shortest_core = 0u8;
    let mut shortest_len = usize::MAX;
    for id in 0..cores {
        if let Some(data) = crate::smp::get_core_data(id) {
            let len = data.run_queue.lock().len();
            if len > longest_len {
                longest_len = len;
                longest_core = id;
            }
            if len < shortest_len {
                shortest_len = len;
                shortest_core = id;
            }
        }
    }
    if longest_len <= shortest_len + 2 {
        return; // Balanced enough — require > 2 difference to avoid thrashing.
    }
    let current_tick = crate::arch::x86_64::interrupts::tick_count();
    // Migrate one task from longest to shortest.
    // Lock ordering: SCHEDULER first, then run_queue (matches pick_next).
    if let Some(src) = crate::smp::get_core_data(longest_core) {
        let sched = scheduler_lock();
        let mut q = src.run_queue.lock();
        // Find a migratable task: affinity-compatible, not pinned by fork_ctx,
        // and not recently assigned/woken/yielded on its current core.
        let mut found = None;
        for i in 0..q.len() {
            if let Some(&idx) = q.get(i)
                && idx < sched.tasks.len()
                && sched.tasks[idx].fork_ctx.is_none()
                && sched.tasks[idx].affinity_mask & (1u64 << shortest_core) != 0
                && current_tick.saturating_sub(sched.tasks[idx].last_migrated_tick)
                    >= MIGRATE_COOLDOWN
            {
                found = Some(i);
                break;
            }
        }
        if let Some(pos) = found
            && let Some(idx) = q.remove(pos)
        {
            drop(q);
            drop(sched);
            // Update assigned_core and migration timestamp.
            {
                let mut sched = scheduler_lock();
                if idx < sched.tasks.len() {
                    sched.tasks[idx].assigned_core = shortest_core;
                    sched.tasks[idx].last_migrated_tick = current_tick;
                }
            }
            enqueue_to_core(shortest_core, idx);
            log::debug!(
                "[sched] load balance: task {} moved core {} -> {}",
                idx,
                longest_core,
                shortest_core
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Priority API (Phase 35, Track D)
// ---------------------------------------------------------------------------

/// Adjust the priority of the current task by `increment`.
/// Returns the new priority. Non-root users are clamped to the lowest normal
/// priority (10) if the result would fall into the real-time range (0-9).
pub fn sys_nice(increment: i32, uid: u32) -> i64 {
    let mut sched = scheduler_lock();
    let idx = match get_current_task_idx() {
        Some(i) => i,
        None => return -1,
    };
    let old = sched.tasks[idx].priority as i32;
    let mut new_prio = (old + increment).clamp(0, 30) as u8;
    // Non-root cannot set real-time priorities (0-9).
    if new_prio < 10 && uid != 0 {
        new_prio = 10;
    }
    sched.tasks[idx].priority = new_prio;
    new_prio as i64
}

// ---------------------------------------------------------------------------
// CPU affinity API (Phase 35, Track F)
// ---------------------------------------------------------------------------

/// Set the CPU affinity mask for a task identified by PID.
/// `pid == 0` means current task.
pub fn sys_sched_setaffinity(pid: u32, mask: u64) -> i64 {
    let cores = crate::smp::core_count() as u64;
    let valid_mask = if cores >= 64 {
        u64::MAX
    } else {
        (1u64 << cores) - 1
    };
    let effective = mask & valid_mask;
    if effective == 0 {
        return -22; // -EINVAL
    }
    let mut sched = scheduler_lock();
    let idx = if pid == 0 {
        match get_current_task_idx() {
            Some(i) => i,
            None => return -3, // -ESRCH
        }
    } else {
        // Find task by scanning for matching PID.
        let mut found = None;
        for (i, t) in sched.tasks.iter().enumerate() {
            if t.pid == pid {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) => i,
            None => return -3, // -ESRCH
        }
    };
    sched.tasks[idx].affinity_mask = effective;
    // If currently assigned to a disallowed core, reassign and migrate.
    let old_core = sched.tasks[idx].assigned_core;
    if effective & (1u64 << old_core) == 0 {
        // Find first allowed core.
        let mut new_core = old_core;
        for c in 0..64u8 {
            if effective & (1u64 << c) != 0 {
                new_core = c;
                break;
            }
        }
        sched.tasks[idx].assigned_core = new_core;
        // If the task is Ready, migrate it from the old core's run queue to the
        // new core's queue so pick_next doesn't drop it as ineligible.
        if new_core != old_core && sched.tasks[idx].state == TaskState::Ready {
            // Remove from old queue (if present).
            if let Some(old_data) = crate::smp::get_core_data(old_core) {
                let mut q = old_data.run_queue.lock();
                if let Some(pos) = q.iter().position(|&i| i == idx) {
                    q.remove(pos);
                }
            }
            drop(sched);
            enqueue_to_core(new_core, idx);
            return 0;
        }
    }
    0
}

/// Get the CPU affinity mask for a task identified by PID.
/// `pid == 0` means current task.
pub fn sys_sched_getaffinity(pid: u32) -> i64 {
    let sched = scheduler_lock();
    let idx = if pid == 0 {
        match get_current_task_idx() {
            Some(i) => i,
            None => return -3, // -ESRCH
        }
    } else {
        let mut found = None;
        for (i, t) in sched.tasks.iter().enumerate() {
            if t.pid == pid {
                found = Some(i);
                break;
            }
        }
        match found {
            Some(i) => i,
            None => return -3, // -ESRCH
        }
    };
    sched.tasks[idx].affinity_mask as i64
}

// ---------------------------------------------------------------------------
// G.1 — Stuck-task watchdog
// ---------------------------------------------------------------------------
//
// `watchdog_scan` is placed here (rather than in the sibling `watchdog.rs`
// module) because `SCHEDULER` is `pub(super)` — accessible within the `task`
// module but not from child modules of `task`. The `task::watchdog` module
// re-exports this symbol via `pub use super::scheduler::watchdog_scan`.
//
// Integration: called from the BSP's scheduler dispatch loop (core_id == 0),
// matching the existing pattern for `drain_dead`, `drain_pending_waiters`,
// and `maybe_load_balance`. The `WATCHDOG_COUNTER` gates the O(n) scan so
// the dispatch hot path sees only a single atomic increment on most calls.

/// Tick counter gating watchdog scans (BSP-only, matches `BALANCE_COUNTER`).
static WATCHDOG_COUNTER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Periodic stuck-task watchdog scan.
///
/// Called from the BSP's scheduler dispatch loop on every iteration.
/// On most calls returns immediately after incrementing `WATCHDOG_COUNTER`
/// (O(1)). Every [`kernel_core::watchdog_policy::WATCHDOG_SCAN_INTERVAL_TICKS`]
/// ticks, acquires `SCHEDULER.lock` and iterates the task table (O(n_tasks)),
/// logging a structured WARN for any `Blocked*` task that has exceeded the
/// stuck threshold.
///
/// # Logging format
///
/// ```text
/// [WARN] [sched] task pid=X name=Y state=Z stuck-since=Wms (no waker registered)
/// [WARN] [sched] task pid=X name=Y state=Z stuck-since=Wms (deadline expired Dms ago — scanner may be stuck)
/// ```
pub fn watchdog_scan() {
    use kernel_core::watchdog_policy::{
        WATCHDOG_SCAN_INTERVAL_TICKS, WatchdogVerdict, watchdog_verdict,
    };

    let cnt = WATCHDOG_COUNTER.fetch_add(1, Ordering::Relaxed);
    // WATCHDOG_SCAN_INTERVAL_TICKS (10_000) fits in u32.
    let interval = WATCHDOG_SCAN_INTERVAL_TICKS as u32;
    if !cnt.is_multiple_of(interval) {
        return;
    }

    let now = crate::arch::x86_64::interrupts::tick_count();
    // Acquire lock and scan. Release before any logging (log macros may
    // allocate internally; holding SCHEDULER.lock during alloc is safe but
    // keeping the critical section short is good practice).
    let mut warnings: [(u32, &'static str, super::TaskState, u64, Option<u64>); 8] =
        [(0, "", super::TaskState::Dead, 0, None); 8];
    let mut n_warn = 0usize;
    {
        let sched = scheduler_lock();
        for task in sched.tasks.iter() {
            let is_blocked = matches!(
                task.state,
                super::TaskState::BlockedOnRecv
                    | super::TaskState::BlockedOnSend
                    | super::TaskState::BlockedOnReply
                    | super::TaskState::BlockedOnNotif
                    | super::TaskState::BlockedOnFutex
            );
            if !is_blocked {
                continue;
            }
            let verdict = watchdog_verdict(now, task.blocked_since_tick, task.wake_deadline);
            if verdict != WatchdogVerdict::Ok && n_warn < warnings.len() {
                warnings[n_warn] = (
                    task.pid,
                    task.name,
                    task.state,
                    task.blocked_since_tick,
                    task.wake_deadline,
                );
                n_warn += 1;
            }
        }
        // SCHEDULER.lock released here.
    }

    // Log after releasing the lock.
    for (pid, name, state, blocked_since, wake_deadline) in &warnings[..n_warn] {
        let verdict = watchdog_verdict(now, *blocked_since, *wake_deadline);
        match verdict {
            WatchdogVerdict::Ok => {}
            WatchdogVerdict::StuckNoWaker => {
                let stuck_ms = now.saturating_sub(*blocked_since);
                log::warn!(
                    "[sched] task pid={} name={} state={:?} stuck-since={}ms (no waker registered)",
                    pid,
                    name,
                    state,
                    stuck_ms,
                );
            }
            WatchdogVerdict::StuckDeadlineExpired => {
                let stuck_ms = now.saturating_sub(*blocked_since);
                let deadline_age_ms = wake_deadline.map(|d| now.saturating_sub(d)).unwrap_or(0);
                log::warn!(
                    "[sched] task pid={} name={} state={:?} stuck-since={}ms (deadline expired {}ms ago — scanner may be stuck)",
                    pid,
                    name,
                    state,
                    stuck_ms,
                    deadline_age_ms,
                );
            }
        }
    }
}
