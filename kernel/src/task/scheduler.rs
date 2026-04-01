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
        let mut i = self.tasks.len();
        while i > 0 {
            i -= 1;
            if self.tasks[i].state == TaskState::Dead {
                self.tasks.remove(i);
                // Fix up idle_task indices.
                for idle in self.idle_tasks.iter_mut() {
                    *idle = idle.and_then(|idx| {
                        if idx == i {
                            None
                        } else if idx > i {
                            Some(idx - 1)
                        } else {
                            Some(idx)
                        }
                    });
                }
                // Adjust last_run.
                if self.tasks.is_empty() {
                    self.last_run = 0;
                } else if i < self.last_run {
                    self.last_run -= 1;
                } else {
                    self.last_run = self.last_run.min(self.tasks.len() - 1);
                }
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
        // Drain local run queue, pick the highest-priority (lowest numeric) Ready task.
        let mut best: Option<usize> = None;
        let mut requeue = alloc::vec::Vec::new();

        if let Some(data) = crate::smp::get_core_data(core_id) {
            let mut q = data.run_queue.lock();
            while let Some(idx) = q.pop_front() {
                if idx >= self.tasks.len()
                    || self.tasks[idx].state != TaskState::Ready
                    || self.idle_tasks.contains(&Some(idx))
                {
                    // Stale entry — discard.
                    continue;
                }
                match best {
                    Some(b) if self.tasks[idx].priority < self.tasks[b].priority => {
                        // idx has higher priority — put old best back in queue.
                        requeue.push(b);
                        best = Some(idx);
                    }
                    Some(_) => {
                        // idx has equal or lower priority — keep it for later.
                        requeue.push(idx);
                    }
                    None => {
                        best = Some(idx);
                    }
                }
            }
            // Put non-selected entries back.
            for i in requeue {
                q.push_back(i);
            }
        }

        if let Some(idx) = best {
            self.last_run = idx;
            return Some((self.tasks[idx].saved_rsp, idx));
        }

        // Fallback: global round-robin scan for unqueued Ready tasks.
        let n = self.tasks.len();
        if n > 0 {
            let start = (self.last_run + 1) % n;
            for i in 0..n {
                let idx = (start + i) % n;
                if self.idle_tasks.contains(&Some(idx)) {
                    continue;
                }
                if self.tasks[idx].state == TaskState::Ready {
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

/// Accumulate elapsed ticks for the current task (user_ticks for simplicity).
fn accumulate_ticks(sched: &mut Scheduler, idx: usize) {
    let now = crate::arch::x86_64::interrupts::tick_count();
    let elapsed = now.saturating_sub(sched.tasks[idx].start_tick);
    sched.tasks[idx].user_ticks += elapsed;
}

/// Yield the current task back to the scheduler.
pub fn yield_now() {
    let (task_rsp_ptr, core_id, idx) = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        accumulate_ticks(&mut sched, idx);
        sched.tasks[idx].state = TaskState::Ready;
        let core = sched.tasks[idx].assigned_core;
        set_current_task_idx(None);
        (
            core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp),
            core,
            idx,
        )
    };
    // Re-enqueue to the local core's run queue.
    enqueue_to_core(core_id, idx);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

// ---------------------------------------------------------------------------
// IPC scheduler primitives
// ---------------------------------------------------------------------------

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

/// Wake a blocked task, making it `Ready` for the next scheduler tick.
pub fn wake_task(id: TaskId) {
    let enqueue = {
        let mut sched = SCHEDULER.lock();
        if let Some(idx) = sched.find(id) {
            match sched.tasks[idx].state {
                TaskState::BlockedOnRecv
                | TaskState::BlockedOnSend
                | TaskState::BlockedOnReply
                | TaskState::BlockedOnNotif => {
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

        // Pick the next ready task and atomically mark it Running.
        let next = {
            let mut sched = SCHEDULER.lock();
            if let Some((rsp, idx)) = sched.pick_next(core_id) {
                sched.tasks[idx].state = TaskState::Running;
                sched.tasks[idx].start_tick = crate::arch::x86_64::interrupts::tick_count();
                set_current_task_idx(Some(idx));
                let name = sched.tasks[idx].name;
                let id = sched.tasks[idx].id;
                Some((rsp, idx, name, id))
            } else {
                None
            }
        };

        let (task_rsp, _task_idx, _name, _id) = match next {
            Some(t) => t,
            None => continue,
        };

        log::debug!("[sched] core {} → task {}({})", core_id, _name, _id.0);

        // Switch to the task.
        unsafe {
            switch_context(per_core_scheduler_rsp_ptr(), task_rsp);
        }
    }
}

// ---------------------------------------------------------------------------
// Load balancing (Phase 35, Track E)
// ---------------------------------------------------------------------------

/// Periodic load balancer tick counter. BSP calls `maybe_load_balance()`
/// from the timer interrupt path every tick; actual migration happens every
/// 10 ticks (100ms at 100 Hz).
static BALANCE_COUNTER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Called from the BSP's timer path. Every 10 ticks, checks queue imbalance
/// and migrates one task if the longest queue exceeds the shortest by >1.
pub fn maybe_load_balance() {
    let cnt = BALANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    if !cnt.is_multiple_of(10) {
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
    if longest_len <= shortest_len + 1 {
        return; // Balanced enough.
    }
    // Migrate one task from longest to shortest.
    if let Some(src) = crate::smp::get_core_data(longest_core) {
        let mut q = src.run_queue.lock();
        // Find a migratable (non-pinned) task.
        let mut found = None;
        for i in 0..q.len() {
            if let Some(&idx) = q.get(i) {
                let sched = SCHEDULER.lock();
                if idx < sched.tasks.len()
                    && sched.tasks[idx].affinity_mask & (1u64 << shortest_core) != 0
                {
                    found = Some(i);
                    break;
                }
            }
        }
        if let Some(pos) = found
            && let Some(idx) = q.remove(pos)
        {
            drop(q);
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
/// Returns the new priority, or -EPERM if trying to set real-time without root.
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
        // Find task by scanning for matching PID in process table.
        // For kernel tasks, this is a no-op (they don't have PIDs).
        let mut found = None;
        for (i, t) in sched.tasks.iter().enumerate() {
            if t.id.0 as u32 == pid {
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
    // If currently assigned to a disallowed core, reassign.
    let current_core = sched.tasks[idx].assigned_core;
    if effective & (1u64 << current_core) == 0 {
        // Find first allowed core.
        for c in 0..64u8 {
            if effective & (1u64 << c) != 0 {
                sched.tasks[idx].assigned_core = c;
                break;
            }
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
            if t.id.0 as u32 == pid {
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
