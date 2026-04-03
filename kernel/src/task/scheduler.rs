//! SMP-aware round-robin kernel scheduler.
//!
//! # Design (Phase 25)
//!
//! The scheduler uses a single global task list protected by `SCHEDULER` mutex.
//! Each core runs its own scheduler loop (`run()`), picking Ready tasks from
//! the shared list. Per-core state (scheduler RSP, reschedule flag, current
//! task index) is stored in [`PerCoreData`] and accessed via `gs_base`.
//!
//! A task voluntarily returns control by calling [`yield_now`], which uses
//! `switch_context` to the calling core's scheduler RSP.
//!
//! The timer ISR calls [`signal_reschedule`], which sets the calling core's
//! reschedule flag. The scheduler loop checks this flag before halting.
#![allow(dead_code)]

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::Ordering;
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

pub(super) struct Scheduler {
    tasks: Vec<Task>,
    /// Index of the last non-idle task that was dispatched (for round-robin).
    last_run: usize,
    /// Indices of per-core idle tasks. Index by core_id.
    idle_tasks: [Option<usize>; crate::smp::MAX_CORES],
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            tasks: Vec::new(),
            last_run: 0,
            idle_tasks: [const { None }; crate::smp::MAX_CORES],
        }
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

    /// Remove all tasks in the `Dead` state from the task vec.
    fn drain_dead(&mut self) {
        // With SMP, removing tasks from the vec would invalidate indices held
        // by per-core run queues, current_task_idx, and PENDING_REENQUEUE.
        // Instead, dead tasks remain in the vec and are skipped by pick_next.
        // Their stack memory is released here to avoid leaks.
        for task in &mut self.tasks {
            if task.state == TaskState::Dead && task.saved_rsp != 0 {
                // Drop the stack allocation to free memory.
                let _ = task._stack.take();
                // Mark as drained so we don't try to free again.
                task.saved_rsp = 0;
            }
        }
    }

    /// Pick the next task to run on the given core.
    ///
    /// First checks the per-core run queue for an O(1) dequeue. Falls back
    /// to a global round-robin scan if the local queue is empty (handles
    /// tasks that haven't been assigned to a queue yet). Finally, falls back
    /// to this core's idle task.
    fn pick_next(&mut self, core_id: u8) -> Option<(u64, usize)> {
        let core_bit = 1u64 << core_id;

        // Scan local run queue: find the highest-priority (lowest numeric) Ready
        // task in a single pass, then remove only that entry. No heap allocation.
        if let Some(data) = crate::smp::get_core_data(core_id) {
            let mut q = data.run_queue.lock();
            let mut best_pos: Option<usize> = None;
            let mut best_prio: u8 = u8::MAX;

            // First pass: discard stale entries from the front while scanning.
            let mut i = 0;
            while i < q.len() {
                let idx = q[i];
                if idx >= self.tasks.len()
                    || self.tasks[idx].state != TaskState::Ready
                    || self.idle_tasks.contains(&Some(idx))
                    || self.tasks[idx].affinity_mask & core_bit == 0
                {
                    // Stale or ineligible entry — discard.
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
                self.last_run = idx;
                return Some((self.tasks[idx].saved_rsp, idx));
            }
        }

        // Fallback: global round-robin scan for unqueued Ready tasks.
        // Tasks found here have valid saved_rsp because they were re-enqueued
        // by the scheduler loop AFTER switch_context (not by yield_now).
        let n = self.tasks.len();
        if n > 0 {
            let start = (self.last_run + 1) % n;
            for i in 0..n {
                let idx = (start + i) % n;
                if self.idle_tasks.contains(&Some(idx)) {
                    continue;
                }
                if self.tasks[idx].state == TaskState::Ready
                    && self.tasks[idx].affinity_mask & core_bit != 0
                {
                    self.last_run = idx;
                    return Some((self.tasks[idx].saved_rsp, idx));
                }
            }
        }

        // Fall back to this core's idle task.
        if let Some(idle_idx) = self.idle_tasks[core_id as usize]
            && self.tasks[idle_idx].state == TaskState::Ready
        {
            return Some((self.tasks[idle_idx].saved_rsp, idle_idx));
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

/// Find the core with the shortest run queue for task assignment.
fn least_loaded_core() -> u8 {
    let n = crate::smp::core_count();
    if n <= 1 {
        return 0;
    }
    let mut best_core = 0u8;
    let mut best_len = usize::MAX;
    for id in 0..n {
        if let Some(data) = crate::smp::get_core_data(id) {
            let len = data.run_queue.lock().len();
            if len < best_len {
                best_len = len;
                best_core = id;
            }
        }
    }
    best_core
}

/// Enqueue a task index into a specific core's run queue and signal it.
fn enqueue_to_core(core_id: u8, idx: usize) {
    if let Some(data) = crate::smp::get_core_data(core_id) {
        data.run_queue.lock().push_back(idx);
        data.reschedule.store(true, Ordering::Relaxed);
    }
}

/// Spawn a new kernel task. The task is assigned to the least-loaded core
/// and enqueued to that core's run queue.
pub fn spawn(entry: fn() -> !, name: &'static str) {
    let mut task = Task::new(entry, name);
    let target = least_loaded_core();
    task.assigned_core = target;
    let mut sched = SCHEDULER.lock();
    let idx = sched.tasks.len();
    sched.tasks.push(task);
    drop(sched);
    enqueue_to_core(target, idx);
}

/// Spawn a new kernel task on the calling core.
///
/// Used for fork children so they stay on the same core as the parent,
/// avoiding cross-core context migration during fork+exec.
pub fn spawn_on_current_core(entry: fn() -> !, name: &'static str) {
    let mut task = Task::new(entry, name);
    let core = crate::smp::per_core().core_id;
    task.assigned_core = core;
    let mut sched = SCHEDULER.lock();
    let idx = sched.tasks.len();
    sched.tasks.push(task);
    drop(sched);
    enqueue_to_core(core, idx);
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
    let idx = sched.tasks.len();
    sched.tasks.push(task);
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

/// Yield the current task back to the scheduler.
pub fn yield_now() {
    let (task_rsp_ptr, idx) = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        accumulate_ticks(&mut sched, idx);
        // Keep state as Running — the scheduler will set Ready + enqueue AFTER
        // switch_context saves the RSP. This prevents the global fallback from
        // picking up the task with a stale saved_rsp on another core.
        set_current_task_idx(None);
        (core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp), idx)
    };
    // Store pending re-enqueue for the scheduler to process after switch_context.
    let my_core = crate::smp::per_core().core_id as usize;
    PENDING_REENQUEUE[my_core].store(idx as i32, Ordering::Release);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
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
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        accumulate_ticks(&mut sched, idx);
        sched.tasks[idx].state = state;
        set_current_task_idx(None);
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

pub fn block_current_on_recv() {
    block_current(TaskState::BlockedOnRecv);
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

pub fn block_current_on_futex() {
    block_current(TaskState::BlockedOnFutex);
}

/// Block the current task unless `woken` is already set.
/// The check is performed under the SCHEDULER lock to be atomic with wake_task.
pub fn block_current_unless_woken(woken: &core::sync::atomic::AtomicBool) {
    let task_rsp_ptr: *mut u64 = {
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
        set_current_task_idx(None);
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

/// Permanently mark the current task as dead and switch back to the scheduler.
pub fn mark_current_dead() -> ! {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => loop {
                x86_64::instructions::hlt();
            },
        };
        sched.tasks[idx].state = TaskState::Dead;
        set_current_task_idx(None);
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
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
pub fn wake_task(id: TaskId) {
    let enqueue = {
        let mut sched = SCHEDULER.lock();
        if let Some(idx) = sched.find(id) {
            match sched.tasks[idx].state {
                TaskState::BlockedOnRecv
                | TaskState::BlockedOnSend
                | TaskState::BlockedOnReply
                | TaskState::BlockedOnNotif
                | TaskState::BlockedOnFutex => {
                    sched.tasks[idx].state = TaskState::Ready;
                    Some((sched.tasks[idx].assigned_core, idx))
                }
                _ => None,
            }
        } else {
            None
        }
    };
    if let Some((core, idx)) = enqueue {
        enqueue_to_core(core, idx);
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

        // Drain notification waiters (only BSP does this to avoid contention).
        if core_id == 0 {
            crate::ipc::notification::drain_pending_waiters();
        }

        // Remove dead tasks (BSP only to avoid contention).
        if core_id == 0 {
            let mut sched = SCHEDULER.lock();
            sched.drain_dead();
        }

        // Periodic load balancing (BSP only, every 50 scheduler ticks).
        // Disabled for now — causes task migration thrashing that interferes
        // with short-lived userspace processes.  Re-enable once per-task
        // cooldown or work-stealing is implemented.
        // if core_id == 0 { maybe_load_balance(); }

        // Pick the next ready task and atomically mark it Running.
        let next = {
            let mut sched = SCHEDULER.lock();
            if let Some((rsp, idx)) = sched.pick_next(core_id) {
                sched.tasks[idx].state = TaskState::Running;
                sched.tasks[idx].start_tick = crate::arch::x86_64::interrupts::tick_count();
                set_current_task_idx(Some(idx));
                Some((rsp, idx))
            } else {
                None
            }
        };

        let (task_rsp, _task_idx) = match next {
            Some(t) => t,
            None => continue,
        };

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

        // Switch to the task.
        unsafe {
            switch_context(per_core_scheduler_rsp_ptr(), task_rsp);
        }

        // --- Scheduler resumes here after the task yields back ---
        // The task's RSP has now been saved by switch_context.
        // Re-enqueue any pending task that yielded on this core.
        let pending = PENDING_REENQUEUE[core_id as usize].swap(-1, Ordering::Acquire);
        if pending >= 0 {
            let pidx = pending as usize;
            let target_core = {
                let mut sched = SCHEDULER.lock();
                if pidx < sched.tasks.len() {
                    sched.tasks[pidx].state = TaskState::Ready;
                    sched.tasks[pidx].assigned_core
                } else {
                    core_id
                }
            };
            enqueue_to_core(target_core, pidx);
        }
    }
}

// ---------------------------------------------------------------------------
// Load balancing (Phase 35, Track E)
// ---------------------------------------------------------------------------

/// Periodic load balancer tick counter. BSP calls `maybe_load_balance()`
/// from the timer interrupt path every tick; actual migration happens every
/// 50 ticks (~500ms at 100 Hz). Note: load balancing is currently disabled
/// in the scheduler loop due to task migration thrashing (see `run()`).
static BALANCE_COUNTER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Called from the BSP's timer path. Every 50 ticks (~500ms), checks queue
/// imbalance and migrates one task if the longest queue exceeds the shortest by >2.
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
    // Migrate one task from longest to shortest.
    // Lock ordering: SCHEDULER first, then run_queue (matches pick_next).
    if let Some(src) = crate::smp::get_core_data(longest_core) {
        let sched = SCHEDULER.lock();
        let mut q = src.run_queue.lock();
        // Find a migratable (non-pinned) task.
        let mut found = None;
        for i in 0..q.len() {
            if let Some(&idx) = q.get(i)
                && idx < sched.tasks.len()
                && sched.tasks[idx].affinity_mask & (1u64 << shortest_core) != 0
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
            // Update assigned_core.
            {
                let mut sched = SCHEDULER.lock();
                if idx < sched.tasks.len() {
                    sched.tasks[idx].assigned_core = shortest_core;
                }
            }
            enqueue_to_core(shortest_core, idx);
            log::debug!(
                "[sched] load balance: task {} moved core {} → {}",
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
