//! SMP-aware per-core scheduler with work-stealing.
//!
//! # Design (Phase 25, refined Phase 52c)
//!
//! Task state lives in a global `SCHEDULER` (TaskRegistry) protected by a
//! mutex. The dispatch hot path (`pick_next`) operates entirely on the
//! per-core run queue without acquiring the global lock. The global lock is
//! only acquired for:
//! - Spawn / exit / drain-dead (task lifecycle)
//! - Reading task state for dispatch (saved_rsp, marking Running)
//! - Post-switch: saving RSP, clearing switching_out
//! - Wake / block / IPC operations that modify task state
//!
//! When a core's local run queue is empty, it attempts to steal work from
//! other cores before falling back to its idle task.
//!
//! Dead task slots are recycled via a free list to bound memory growth.
//!
//! A task voluntarily returns control by calling [`yield_now`], which uses
//! `switch_context` to the calling core's scheduler RSP.
//!
//! The timer ISR calls [`signal_reschedule`], which sets the calling core's
//! reschedule flag. The scheduler loop checks this flag before halting.
#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use core::{cell::UnsafeCell, sync::atomic::Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;

use super::{Task, TaskId, TaskState, switch_context};
use crate::ipc::{CapError, CapHandle, Capability, EndpointId, Message};

// ---------------------------------------------------------------------------
// Statics
// ---------------------------------------------------------------------------

pub(super) static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

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
    tasks: Vec<Task>,
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
        self.tasks.get(idx)
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

    /// Return the server endpoint registered for this task.
    pub fn server_endpoint(&self, id: TaskId) -> Option<EndpointId> {
        let idx = self.find(id)?;
        self.tasks[idx].server_endpoint
    }

    /// Reclaim dead tasks: free their stacks and add their indices to the
    /// free list for reuse by future spawns (Phase 52c A.3).
    fn drain_dead(&mut self) {
        // With SMP, removing tasks from the vec would invalidate indices held
        // by per-core run queues, current_task_idx, and PENDING_REENQUEUE.
        // Instead, dead tasks remain in the vec and are skipped by pick_next.
        // Their stack memory is released here to avoid leaks.
        for (i, task) in self.tasks.iter_mut().enumerate() {
            if task.state == TaskState::Dead && !task.switching_out && task.saved_rsp != 0 {
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
    /// Phase 52c: Uses ONLY the per-core run queue (no global fallback scan).
    /// If the local queue is empty, attempts to steal from other cores.
    /// Falls back to this core's idle task as a last resort.
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
            if idx >= self.tasks.len()
                || self.tasks[idx].state != TaskState::Ready
                || self.idle_tasks.contains(&Some(idx))
                || self.tasks[idx].affinity_mask & core_bit == 0
            {
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
fn enqueue_to_core(core_id: u8, idx: usize) {
    debug_assert!(
        (core_id as usize) < crate::smp::MAX_CORES,
        "enqueue_to_core: core_id={} exceeds MAX_CORES={}",
        core_id,
        crate::smp::MAX_CORES
    );
    if let Some(data) = crate::smp::get_core_data(core_id) {
        data.run_queue.lock().push_back(idx);
        crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::RunQueueEnqueue {
            task_idx: idx as u32,
            core: core_id,
        });
        data.reschedule.store(true, Ordering::Relaxed);
    }
}

/// Allocate a slot for a new task, reusing a dead slot from the free list
/// if available, otherwise appending to the task vec.
fn alloc_task_slot(sched: &mut Scheduler, task: Task) -> usize {
    if let Some(idx) = sched.free_list.pop() {
        // Reuse a dead slot.
        sched.tasks[idx] = task;
        idx
    } else {
        let idx = sched.tasks.len();
        sched.tasks.push(task);
        idx
    }
}

/// Spawn a new kernel task. The task is assigned to the least-loaded core
/// and enqueued to that core's run queue.
pub fn spawn(entry: fn() -> !, name: &'static str) {
    let mut task = Task::new(entry, name);
    let mut sched = SCHEDULER.lock();
    let target = least_loaded_core(&sched);
    task.assigned_core = target;
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
    task.assigned_core = core;
    let mut sched = SCHEDULER.lock();
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
    let mut sched = SCHEDULER.lock();
    task.assigned_core = current_core;
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
        core: current_core,
    });
    enqueue_to_core(current_core, idx);

    current_core
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
    let mut sched = SCHEDULER.lock();
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

/// Per-core task index currently switching back to the scheduler. The
/// scheduler clears `Task::switching_out` for this task after `switch_context`
/// has stored its up-to-date `saved_rsp`.
#[allow(clippy::declare_interior_mutable_const)]
static PENDING_SWITCH_OUT: [core::sync::atomic::AtomicI32; crate::smp::MAX_CORES] = {
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

/// Yield the current task back to the scheduler.
pub fn yield_now() {
    let idx = {
        let mut sched = SCHEDULER.lock();
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
        // switch_context saves the RSP. This prevents the global fallback from
        // picking up the task with a stale saved_rsp on another core.
        sched.tasks[idx].switching_out = true;
        set_current_task_idx(None);
        idx
    };
    // Store pending re-enqueue for the scheduler to process after switch_context.
    let my_core = crate::smp::per_core().core_id as usize;
    PENDING_SWITCH_OUT[my_core].store(idx as i32, Ordering::Release);
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
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };
}

// ---------------------------------------------------------------------------
// IPC scheduler primitives
// ---------------------------------------------------------------------------

/// Store a PID in the current task so the scheduler can restore per-core
/// process context on re-dispatch.
pub fn set_current_task_pid(pid: u32) {
    if let Some(idx) = get_current_task_idx() {
        SCHEDULER.lock().tasks[idx].pid = pid;
    }
}

pub fn take_current_task_fork_ctx() -> Option<crate::process::ForkChildCtx> {
    let idx = get_current_task_idx()?;
    SCHEDULER.lock().tasks[idx].fork_ctx.take()
}

/// Return the PID associated with the given task index.
fn task_pid(idx: usize) -> u32 {
    SCHEDULER.lock().tasks[idx].pid
}

/// Return the user and system tick counts for the current task.
pub fn current_task_times() -> Option<(u64, u64)> {
    let idx = get_current_task_idx()?;
    let sched = SCHEDULER.lock();
    Some((sched.tasks[idx].user_ticks, sched.tasks[idx].system_ticks))
}

/// Return the [`TaskId`] of the task currently running on this core.
pub fn current_task_id() -> Option<TaskId> {
    let idx = get_current_task_idx()?;
    let sched = SCHEDULER.lock();
    Some(sched.tasks[idx].id)
}

/// Helper: block the current task with the given state.
fn block_current(state: TaskState) {
    let idx = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        debug_assert!(
            sched.tasks[idx].state == TaskState::Running,
            "block_current: task idx={} was {:?} before block, expected Running",
            idx,
            sched.tasks[idx].state
        );
        accumulate_ticks(&mut sched, idx);
        sched.tasks[idx].state = state;
        sched.tasks[idx].switching_out = true;
        set_current_task_idx(None);
        idx
    };
    let core = crate::smp::per_core().core_id;
    PENDING_SWITCH_OUT[core as usize].store(idx as i32, Ordering::Release);
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    debug_assert!(
        sched_rsp != 0,
        "block_current: scheduler RSP is zero on core {}",
        core
    );
    crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::BlockCurrent {
        task_idx: idx as u32,
        core,
        new_state: state as u8,
    });
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };
}

fn block_current_unless_message(state: TaskState) {
    let idx = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        if sched.tasks[idx].pending_msg.is_some() {
            return;
        }
        accumulate_ticks(&mut sched, idx);
        sched.tasks[idx].state = state;
        sched.tasks[idx].switching_out = true;
        set_current_task_idx(None);
        idx
    };
    PENDING_SWITCH_OUT[crate::smp::per_core().core_id as usize]
        .store(idx as i32, Ordering::Release);
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };
}

pub fn block_current_on_recv() {
    block_current(TaskState::BlockedOnRecv);
}

pub fn block_current_on_recv_unless_message() {
    block_current_unless_message(TaskState::BlockedOnRecv);
}

pub fn block_current_on_send() {
    block_current(TaskState::BlockedOnSend);
}

pub fn block_current_on_notif() {
    block_current(TaskState::BlockedOnNotif);
}

pub fn block_current_on_reply() {
    block_current(TaskState::BlockedOnReply);
}

pub fn block_current_on_reply_unless_message() {
    block_current_unless_message(TaskState::BlockedOnReply);
}

pub fn block_current_on_futex() {
    block_current(TaskState::BlockedOnFutex);
}

/// Block the current task on a futex unless the woken flag is already set.
///
/// The check is performed under the scheduler lock so that a concurrent
/// `wake_task()` call cannot slip between the flag check and the state
/// transition, which would cause a missed wakeup.
pub fn block_current_on_futex_unless_woken(woken: &core::sync::atomic::AtomicBool) {
    let idx = {
        let mut sched = SCHEDULER.lock();
        if woken.load(core::sync::atomic::Ordering::Acquire) {
            return;
        }
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        accumulate_ticks(&mut sched, idx);
        sched.tasks[idx].state = TaskState::BlockedOnFutex;
        sched.tasks[idx].switching_out = true;
        set_current_task_idx(None);
        idx
    };
    PENDING_SWITCH_OUT[crate::smp::per_core().core_id as usize]
        .store(idx as i32, Ordering::Release);
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };
}

/// Block the current task unless `woken` is already set.
/// The check is performed under the SCHEDULER lock to be atomic with wake_task.
pub fn block_current_unless_woken(woken: &core::sync::atomic::AtomicBool) {
    let idx = {
        let mut sched = SCHEDULER.lock();
        if woken.load(core::sync::atomic::Ordering::Acquire) {
            return;
        }
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        accumulate_ticks(&mut sched, idx);
        sched.tasks[idx].state = TaskState::BlockedOnRecv;
        sched.tasks[idx].switching_out = true;
        set_current_task_idx(None);
        idx
    };
    PENDING_SWITCH_OUT[crate::smp::per_core().core_id as usize]
        .store(idx as i32, Ordering::Release);
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(per_core_switch_save_rsp_ptr(), sched_rsp) };
}

/// Permanently mark the current task as dead and switch back to the scheduler.
pub fn mark_current_dead() -> ! {
    let idx = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => loop {
                x86_64::instructions::hlt();
            },
        };
        sched.tasks[idx].state = TaskState::Dead;
        sched.tasks[idx].switching_out = true;
        set_current_task_idx(None);
        idx
    };
    PENDING_SWITCH_OUT[crate::smp::per_core().core_id as usize]
        .store(idx as i32, Ordering::Release);
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
    let mut sched = SCHEDULER.lock();
    for task in sched.tasks.iter_mut() {
        if task.pid == pid && task.state != TaskState::Dead {
            task.state = TaskState::Dead;
            return true;
        }
    }
    false
}

/// Wake a blocked task, making it `Ready` for the next scheduler tick.
pub fn wake_task(id: TaskId) -> bool {
    let (enqueue, woke) = {
        let mut sched = SCHEDULER.lock();
        if let Some(idx) = sched.find(id) {
            debug_assert!(
                idx < sched.tasks.len(),
                "wake_task: idx={} out of bounds (len={})",
                idx,
                sched.tasks.len()
            );
            match sched.tasks[idx].state {
                TaskState::BlockedOnRecv
                | TaskState::BlockedOnSend
                | TaskState::BlockedOnReply
                | TaskState::BlockedOnNotif
                | TaskState::BlockedOnFutex => {
                    let prev_state = sched.tasks[idx].state as u8;
                    if sched.tasks[idx].switching_out {
                        sched.tasks[idx].wake_after_switch = true;
                        (None, true)
                    } else {
                        sched.tasks[idx].state = TaskState::Ready;
                        (
                            Some((sched.tasks[idx].assigned_core, idx, prev_state)),
                            true,
                        )
                    }
                }
                _ => (None, false),
            }
        } else {
            (None, false)
        }
    };
    if let Some((core, idx, prev_state)) = enqueue {
        crate::trace::trace_event(kernel_core::trace_ring::TraceEvent::WakeTask {
            task_idx: idx as u32,
            state_before: prev_state,
            core,
        });
        enqueue_to_core(core, idx);
        true
    } else {
        woke
    }
}

/// Store a [`Message`] in a task's pending slot.
pub fn deliver_message(id: TaskId, msg: Message) {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_msg = Some(msg);
    }
}

/// Remove and return the pending message for a task.
pub fn take_message(id: TaskId) -> Option<Message> {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_msg.take()
    } else {
        None
    }
}

/// Insert a capability into a task's capability table.
pub fn insert_cap(id: TaskId, cap: Capability) -> Result<CapHandle, CapError> {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].caps.insert(cap)
    } else {
        Err(CapError::InvalidHandle)
    }
}

/// Insert a capability into a task's capability table at a specific slot.
pub fn insert_cap_at(id: TaskId, handle: CapHandle, cap: Capability) -> Result<(), CapError> {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].caps.insert_at(handle, cap)
    } else {
        Err(CapError::InvalidHandle)
    }
}

/// Look up a capability in a task's capability table.
pub fn task_cap(id: TaskId, handle: CapHandle) -> Result<Capability, CapError> {
    let sched = SCHEDULER.lock();
    sched.cap(id, handle)
}

/// Remove a capability from a task's capability table.
pub fn remove_task_cap(id: TaskId, handle: CapHandle) -> Result<Capability, CapError> {
    let mut sched = SCHEDULER.lock();
    sched.remove_cap(id, handle)
}

/// Register the endpoint this task acts as server for.
pub fn set_server_endpoint(id: TaskId, ep_id: EndpointId) {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].server_endpoint = Some(ep_id);
    }
}

/// Return the server endpoint for a task.
pub fn server_endpoint(id: TaskId) -> Option<EndpointId> {
    let sched = SCHEDULER.lock();
    sched.server_endpoint(id)
}

/// The main scheduler loop. Called once per core. Never returns.
///
/// Each core runs its own instance, using per-core RESCHEDULE flag and
/// scheduler RSP. Tasks are picked from the shared global task list.
pub fn run() -> ! {
    let core_id = crate::smp::per_core().core_id;

    loop {
        let reschedule = per_core_reschedule();

        interrupts::disable();
        if !reschedule.swap(false, Ordering::AcqRel) {
            interrupts::enable_and_hlt();
            continue;
        }
        interrupts::enable();

        debug_assert!(
            per_core_scheduler_rsp() != 0,
            "core {}: scheduler RSP is zero",
            core_id
        );

        // Drain notification waiters (only BSP does this to avoid contention).
        if core_id == 0 {
            crate::ipc::notification::drain_pending_waiters();
        }

        // Remove dead tasks (BSP only to avoid contention).
        if core_id == 0 {
            let mut sched = SCHEDULER.lock();
            sched.drain_dead();
        }

        // Periodic load balancing with per-task cooldown (Phase 52c A.4).
        if core_id == 0 {
            maybe_load_balance();
        }

        // Pick the next ready task and atomically mark it Running.
        let next = {
            let mut sched = SCHEDULER.lock();
            if let Some((rsp, idx)) = sched.pick_next(core_id) {
                sched.tasks[idx].state = TaskState::Running;
                debug_assert!(
                    sched.tasks[idx].state == TaskState::Running,
                    "dispatch: task idx={} not Running after mark on core {}",
                    idx,
                    core_id
                );
                sched.tasks[idx].start_tick = crate::arch::x86_64::interrupts::tick_count();
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
        // Read the task's PID and update: current_pid, CR3, TSS.RSP0,
        // syscall_stack_top, and FS.base.
        {
            let pid = task_pid(_task_idx);
            crate::process::set_current_pid(pid);
            if pid != 0 {
                let (cr3_phys, kstack, fs) = {
                    let table = crate::process::PROCESS_TABLE.lock();
                    match table.find(pid) {
                        Some(p) => (p.page_table_root, p.kernel_stack_top, p.fs_base),
                        None => (None, 0, 0),
                    }
                };
                // Restore CR3 so the task's user-space pages are mapped.
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
            }
        }

        // F.1: Validate saved_rsp falls within the task's kernel stack.
        {
            let sched = SCHEDULER.lock();
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

        // Switch to the task.
        unsafe {
            switch_context(per_core_scheduler_rsp_ptr(), task_rsp);
        }

        // --- Scheduler resumes here after the task yields back ---
        // The task's RSP has now been saved by switch_context. Clear the
        // switching-out flag before honoring deferred wakes or yields.
        let switched = PENDING_SWITCH_OUT[core_id as usize].swap(-1, Ordering::Acquire);
        let pending = PENDING_REENQUEUE[core_id as usize].swap(-1, Ordering::Acquire);
        if switched >= 0 {
            let sidx = switched as usize;
            let saved_rsp = take_per_core_switch_save_rsp(core_id as usize);
            let enqueue = {
                let mut sched = SCHEDULER.lock();
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
                    task.switching_out = false;

                    let wake_after_switch = task.wake_after_switch;
                    let blocked = matches!(
                        task.state,
                        TaskState::BlockedOnRecv
                            | TaskState::BlockedOnSend
                            | TaskState::BlockedOnReply
                            | TaskState::BlockedOnNotif
                            | TaskState::BlockedOnFutex
                    );
                    let reenqueue_after_yield =
                        pending == switched && task.state == TaskState::Running;

                    task.wake_after_switch = false;

                    if (wake_after_switch && blocked) || reenqueue_after_yield {
                        task.state = TaskState::Ready;
                        Some((task.assigned_core, sidx))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
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
        let sched = SCHEDULER.lock();
        let mut q = src.run_queue.lock();
        // Find a migratable (non-pinned, not recently migrated) task.
        let mut found = None;
        for i in 0..q.len() {
            if let Some(&idx) = q.get(i)
                && idx < sched.tasks.len()
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
                let mut sched = SCHEDULER.lock();
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
    let mut sched = SCHEDULER.lock();
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
    let mut sched = SCHEDULER.lock();
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
    let sched = SCHEDULER.lock();
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
