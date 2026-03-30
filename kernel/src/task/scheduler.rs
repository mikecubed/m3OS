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
fn get_current_task_idx() -> Option<usize> {
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
    /// Prefers non-idle `Ready` tasks using round-robin. Falls back to this
    /// core's idle task if no non-idle task is ready.
    ///
    /// **SMP restriction**: Only the BSP (core 0) dispatches non-idle tasks.
    /// APs only run their idle tasks. This is because the syscall entry stub
    /// uses global `static mut` variables (SYSCALL_STACK_TOP, SYSCALL_USER_*,
    /// FORK_ENTRY_CTX) that are not yet per-core. Running userspace tasks on
    /// APs would corrupt these statics. Making them per-core requires changing
    /// the assembly syscall entry path to use gs-relative addressing.
    fn pick_next(&mut self, core_id: u8) -> Option<(u64, usize)> {
        // APs: only dispatch the idle task (SMP hardening — see doc above).
        if core_id != 0 {
            if let Some(idle_idx) = self.idle_tasks[core_id as usize]
                && self.tasks[idle_idx].state == TaskState::Ready
            {
                return Some((self.tasks[idle_idx].saved_rsp, idle_idx));
            }
            return None;
        }

        // BSP: normal round-robin dispatch.
        let n = self.tasks.len();
        if n == 0 {
            return None;
        }

        let start = (self.last_run + 1) % n;
        for i in 0..n {
            let idx = (start + i) % n;
            if self.idle_tasks.contains(&Some(idx)) {
                continue;
            }
            if self.tasks[idx].state == TaskState::Dead {
                continue;
            }
            if self.tasks[idx].state == TaskState::Ready {
                self.last_run = idx;
                return Some((self.tasks[idx].saved_rsp, idx));
            }
        }

        // BSP idle task.
        if let Some(idle_idx) = self.idle_tasks[0]
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

/// Spawn a new kernel task. The task is placed in the `Ready` state and
/// will be picked up by the BSP's scheduler on the next tick (APs are
/// idle-only until per-core syscall statics are implemented).
pub fn spawn(entry: fn() -> !, name: &'static str) {
    let task = Task::new(entry, name);
    SCHEDULER.lock().tasks.push(task);
}

/// Register an idle task for a specific core.
///
/// Each core should have its own idle task that runs when no other task
/// is ready on that core.
pub fn spawn_idle_for_core(entry: fn() -> !, core_id: u8) {
    assert!((core_id as usize) < crate::smp::MAX_CORES);
    let task = Task::new(entry, "idle");
    let mut sched = SCHEDULER.lock();
    let idx = sched.tasks.len();
    sched.tasks.push(task);
    sched.idle_tasks[core_id as usize] = Some(idx);
}

/// Register the idle task (legacy single-core API — registers for core 0).
pub fn spawn_idle(entry: fn() -> !) {
    spawn_idle_for_core(entry, 0);
}

/// Yield the current task back to the scheduler.
pub fn yield_now() {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match get_current_task_idx() {
            Some(i) => i,
            None => return,
        };
        sched.tasks[idx].state = TaskState::Ready;
        set_current_task_idx(None);
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    per_core_reschedule().store(true, Ordering::Relaxed);
    let sched_rsp = per_core_scheduler_rsp();
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

// ---------------------------------------------------------------------------
// IPC scheduler primitives
// ---------------------------------------------------------------------------

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
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        match sched.tasks[idx].state {
            TaskState::BlockedOnRecv
            | TaskState::BlockedOnSend
            | TaskState::BlockedOnReply
            | TaskState::BlockedOnNotif => {
                sched.tasks[idx].state = TaskState::Ready;
            }
            _ => {}
        }
    }
    // Signal reschedule on the BSP (only the BSP dispatches non-idle tasks).
    per_core_reschedule().store(true, Ordering::Relaxed);
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

        // Switch to the task.
        unsafe {
            switch_context(per_core_scheduler_rsp_ptr(), task_rsp);
        }
    }
}
