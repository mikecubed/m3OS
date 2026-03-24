//! Round-robin kernel scheduler.
//!
//! Preserved from Phase 4.  Phase 5 enters userspace directly via
//! `arch::enter_userspace` and does not use the scheduler.  Phase 6+
//! will re-activate this module for multi-task userspace scheduling.
#![allow(dead_code)]
//!
//! # Design
//!
//! The scheduler owns all [`Task`]s in a `Vec`.  On each timer tick the PIC
//! ISR calls [`signal_reschedule`], which atomically sets [`RESCHEDULE`].
//! The scheduler loop (running on the boot stack) checks the flag, picks the
//! next `Ready` non-idle task in round-robin order, and uses [`switch_context`]
//! to transfer execution to it.  If no non-idle task is ready, the idle task
//! (registered via [`spawn_idle`]) is selected instead.
//!
//! A task voluntarily returns control by calling [`yield_now`], which calls
//! `switch_context` back to the scheduler's saved RSP.
//!
//! # Why round-robin?
//!
//! Round-robin is the simplest fair scheduler: every ready task gets equal
//! CPU time, it requires no per-task priority bookkeeping, and the
//! implementation fits in ~50 lines.  It is ideal for a teaching OS where
//! clarity matters more than throughput.  Real schedulers (CFS, O(1))
//! introduce weighted fair queuing, per-CPU run queues, and sleep/wake
//! priority boosting — all of which are covered in the "Future" section of
//! `docs/05-tasking.md`.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::interrupts;

use super::{switch_context, Task, TaskId, TaskState};
use crate::ipc::{CapError, CapHandle, Capability, EndpointId, Message};

// ---------------------------------------------------------------------------
// Statics
// ---------------------------------------------------------------------------

pub(super) static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Set by the timer ISR; cleared by the scheduler loop before picking a task.
static RESCHEDULE: AtomicBool = AtomicBool::new(false);

/// RSP of the scheduler loop (boot stack).  Written by `switch_context` when
/// transitioning scheduler → task; read by `yield_now` to switch back.
///
/// Only meaningful on single-CPU; no concurrent writes.
static mut SCHEDULER_RSP: u64 = 0;

// ---------------------------------------------------------------------------
// Scheduler struct
// ---------------------------------------------------------------------------

pub(super) struct Scheduler {
    tasks: Vec<Task>,
    /// Index of the last non-idle task that was dispatched (for round-robin).
    last_run: usize,
    /// Index of the task currently holding the CPU, or `None` while the
    /// scheduler loop itself is running.
    current: Option<usize>,
    /// Index of the dedicated idle task in `tasks`, registered via
    /// [`spawn_idle`].  The idle task is excluded from the normal round-robin
    /// rotation and is selected only when no non-idle task is `Ready`.
    idle_task: Option<usize>,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            tasks: Vec::new(),
            last_run: 0,
            current: None,
            idle_task: None,
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
    ///
    /// Also repairs `idle_task` and `last_run` indices after the drain so
    /// subsequent `pick_next` calls see a consistent view.
    fn drain_dead(&mut self) {
        // Walk backwards so that removing an element does not shift earlier
        // indices and invalidate `i`.
        let mut i = self.tasks.len();
        while i > 0 {
            i -= 1;
            if self.tasks[i].state == TaskState::Dead {
                self.tasks.remove(i);
                // Fix up idle_task index.
                self.idle_task = self.idle_task.and_then(|idle| {
                    if idle == i {
                        None // the idle task itself was removed (unusual)
                    } else if idle > i {
                        Some(idle - 1)
                    } else {
                        Some(idle)
                    }
                });
                // Keep last_run in-bounds.
                if !self.tasks.is_empty() {
                    self.last_run = self.last_run.min(self.tasks.len() - 1);
                } else {
                    self.last_run = 0;
                }
            }
        }
    }

    /// Pick the next task to run.
    ///
    /// Prefers non-idle `Ready` tasks using round-robin starting after
    /// `last_run`.  Falls back to the idle task if no non-idle task is ready.
    /// Returns `(saved_rsp, index)`, or `None` if there are no tasks at all.
    fn pick_next(&mut self) -> Option<(u64, usize)> {
        let n = self.tasks.len();
        if n == 0 {
            return None;
        }

        let start = (self.last_run + 1) % n;
        for i in 0..n {
            let idx = (start + i) % n;
            // Skip the idle task in the main rotation.
            if Some(idx) == self.idle_task {
                continue;
            }
            // Skip dead tasks (they will be drained before pick_next is called).
            if self.tasks[idx].state == TaskState::Dead {
                continue;
            }
            if self.tasks[idx].state == TaskState::Ready {
                self.last_run = idx;
                return Some((self.tasks[idx].saved_rsp, idx));
            }
        }

        // No non-idle task is ready — fall back to the idle task.
        if let Some(idle_idx) = self.idle_task {
            if self.tasks[idle_idx].state == TaskState::Ready {
                // Do not update `last_run` for idle so the next non-idle
                // search continues from where it left off.
                return Some((self.tasks[idle_idx].saved_rsp, idle_idx));
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Signal the scheduler to run on the next opportunity.
///
/// This is the only scheduler function called from an interrupt handler and
/// must be async-signal-safe: it performs only an atomic store — no
/// allocation, no locks, no IPC.
pub fn signal_reschedule() {
    RESCHEDULE.store(true, Ordering::Relaxed);
}

/// Spawn a new kernel task.  The task is immediately placed in the `Ready`
/// state and will be picked up by the scheduler on the next tick.
pub fn spawn(entry: fn() -> !, name: &'static str) {
    let task = Task::new(entry, name);
    SCHEDULER.lock().tasks.push(task);
}

/// Register the idle task.  Unlike [`spawn`], the idle task is excluded from
/// the normal round-robin rotation and is selected only when no other task is
/// `Ready` (P4-T005, P4-T009).
///
/// # Panics (debug builds)
///
/// Panics if called more than once — the scheduler supports exactly one idle
/// task.
pub fn spawn_idle(entry: fn() -> !) {
    let task = Task::new(entry, "idle");
    let mut sched = SCHEDULER.lock();
    assert!(
        sched.idle_task.is_none(),
        "spawn_idle must be called at most once"
    );
    let idx = sched.tasks.len();
    sched.tasks.push(task);
    sched.idle_task = Some(idx);
}

/// Yield the current task back to the scheduler.
///
/// Marks the task `Ready`, clears `sched.current`, then context-switches to
/// the scheduler loop.  Returns when the scheduler dispatches this task again.
///
/// `switch_context` saves RFLAGS (including the IF bit) as part of the
/// register frame, so the task's interrupt state is preserved across yields
/// without any extra `cli`/`sti` in the scheduler.
pub fn yield_now() {
    // Extract a raw pointer to the current task's `saved_rsp` field.  We drop
    // the MutexGuard before calling `switch_context` to avoid holding the lock
    // across a context switch (the scheduler loop also locks `SCHEDULER`).
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match sched.current {
            Some(i) => i,
            None => return, // called outside task context — no-op
        };
        sched.tasks[idx].state = TaskState::Ready;
        sched.current = None;
        // Safety: addr_of_mut! avoids creating a &mut reference to a field
        // of data that may be aliased through the Mutex.
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
        // MutexGuard drops here, releasing the lock before switch_context.
    };
    // Signal a reschedule so the scheduler loop does not hlt after this
    // task yields — other tasks may already be Ready and should run
    // immediately without waiting for the next timer IRQ.
    RESCHEDULE.store(true, Ordering::Relaxed);
    // Safety: SCHEDULER_RSP was written by the scheduler loop in `run()`.
    let sched_rsp = unsafe { SCHEDULER_RSP };
    // Safety: task_rsp_ptr is a valid aligned pointer inside a live Task.
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
    // Execution resumes here when the scheduler dispatches this task again.
}

// ---------------------------------------------------------------------------
// IPC scheduler primitives
// ---------------------------------------------------------------------------

/// Return the [`TaskId`] of the task currently running on the CPU, or `None`
/// if called from outside a task context (e.g., during early boot before any
/// task has been dispatched).
///
/// **Not ISR-safe** — acquires `SCHEDULER.lock()`.  Must only be called from
/// task context (syscall handlers, kernel threads).  Calling from an interrupt
/// handler while a task holds the scheduler lock will deadlock.
pub fn current_task_id() -> Option<TaskId> {
    let sched = SCHEDULER.lock();
    sched.current.map(|idx| sched.tasks[idx].id)
}

/// Block the current task waiting for an IPC message on an endpoint.
///
/// Sets state to [`TaskState::BlockedOnRecv`] and switches to the scheduler.
/// Returns when another task calls [`wake_task`] on this task.
///
/// For notification waits, use [`block_current_on_notif`] instead.
pub fn block_current_on_recv() {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match sched.current {
            Some(i) => i,
            None => return,
        };
        sched.tasks[idx].state = TaskState::BlockedOnRecv;
        sched.current = None;
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    // Signal a reschedule so the scheduler loop does not hlt after this
    // task blocks — another task may already be Ready and should run
    // immediately without waiting for the next timer IRQ.
    RESCHEDULE.store(true, Ordering::Relaxed);
    let sched_rsp = unsafe { SCHEDULER_RSP };
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

/// Block the current task waiting for its send to be picked up.
///
/// Sets state to [`TaskState::BlockedOnSend`] and switches to the scheduler.
pub fn block_current_on_send() {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match sched.current {
            Some(i) => i,
            None => return,
        };
        sched.tasks[idx].state = TaskState::BlockedOnSend;
        sched.current = None;
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    RESCHEDULE.store(true, Ordering::Relaxed);
    let sched_rsp = unsafe { SCHEDULER_RSP };
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

/// Block the current task waiting for a notification bit to be set.
///
/// Sets state to [`TaskState::BlockedOnNotif`] and switches to the scheduler.
/// Returns when another context calls [`wake_task`] on this task or when
/// [`signal_irq`][crate::ipc::notification::signal_irq] triggers a reschedule
/// that allows the task to drain pending bits in its [`wait`][crate::ipc::notification::wait] loop.
pub fn block_current_on_notif() {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match sched.current {
            Some(i) => i,
            None => return,
        };
        sched.tasks[idx].state = TaskState::BlockedOnNotif;
        sched.current = None;
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    RESCHEDULE.store(true, Ordering::Relaxed);
    let sched_rsp = unsafe { SCHEDULER_RSP };
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

/// Permanently mark the current task as dead and switch back to the scheduler.
///
/// The scheduler loop will remove the dead task entry on its next iteration.
/// This function never returns.
///
/// Called from `sys_exit` and `fault_kill_trampoline` — locations that run in
/// ring-0 task context (never inside an ISR) so locking and context-switching
/// are safe.
pub fn mark_current_dead() -> ! {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match sched.current {
            Some(i) => i,
            None => {
                // Not in a task context — just halt.
                loop {
                    x86_64::instructions::hlt();
                }
            }
        };
        sched.tasks[idx].state = TaskState::Dead;
        sched.current = None;
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    RESCHEDULE.store(true, Ordering::Relaxed);
    let sched_rsp = unsafe { SCHEDULER_RSP };
    // Safety: same preconditions as block_current_on_recv.
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
    // Unreachable — the dead task is never rescheduled.
    loop {
        x86_64::instructions::hlt();
    }
}

/// Block the current task waiting for a reply after a `call`.
///
/// Sets state to [`TaskState::BlockedOnReply`] and switches to the scheduler.
pub fn block_current_on_reply() {
    let task_rsp_ptr: *mut u64 = {
        let mut sched = SCHEDULER.lock();
        let idx = match sched.current {
            Some(i) => i,
            None => return,
        };
        sched.tasks[idx].state = TaskState::BlockedOnReply;
        sched.current = None;
        core::ptr::addr_of_mut!(sched.tasks[idx].saved_rsp)
    };
    RESCHEDULE.store(true, Ordering::Relaxed);
    let sched_rsp = unsafe { SCHEDULER_RSP };
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
}

/// Wake a blocked task, making it `Ready` for the next scheduler tick.
///
/// No-op if the task is not currently blocked.
///
/// **Not ISR-safe** — acquires `SCHEDULER.lock()`.  Must only be called from
/// task context.  IRQ handlers that need to trigger a wakeup should instead
/// set a pending bit atomically and call [`signal_reschedule`]; the blocked
/// task will drain the bits in its wait loop on the next scheduler dispatch.
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
    // Signal a reschedule so the scheduler loop picks up the newly-ready task.
    RESCHEDULE.store(true, Ordering::Relaxed);
}

/// Store a [`Message`] in a task's pending slot so it can retrieve it on wake.
///
/// Overwrites any previously pending message (the task should drain it before
/// being made ready again).
pub fn deliver_message(id: TaskId, msg: Message) {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_msg = Some(msg);
    }
}

/// Remove and return the pending message for a task, or `None` if none is set.
///
/// `None` indicates a scheduler / IPC logic bug — a task was woken without
/// a corresponding `deliver_message` call.  Callers should `debug_assert` or
/// propagate the error rather than silently using a zeroed message.
pub fn take_message(id: TaskId) -> Option<Message> {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].pending_msg.take()
    } else {
        None
    }
}

/// Insert a capability into a task's capability table.
///
/// Returns the handle on success, or [`CapError::TableFull`] / [`CapError::InvalidHandle`]
/// on failure.  Callers in the IPC syscall path should propagate the error as
/// `u64::MAX` rather than panicking — a full table should be an IPC error, not
/// a kernel panic.
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

/// Remove a capability from a task's capability table (consumes one-shot caps).
pub fn remove_task_cap(id: TaskId, handle: CapHandle) -> Result<Capability, CapError> {
    let mut sched = SCHEDULER.lock();
    sched.remove_cap(id, handle)
}

/// Register the endpoint this task acts as server for.
///
/// Stored so that `reply_recv` can find the right endpoint after replying.
pub fn set_server_endpoint(id: TaskId, ep_id: EndpointId) {
    let mut sched = SCHEDULER.lock();
    if let Some(idx) = sched.find(id) {
        sched.tasks[idx].server_endpoint = Some(ep_id);
    }
}

/// Return the server endpoint for a task (used by reply_recv).
pub fn server_endpoint(id: TaskId) -> Option<EndpointId> {
    let sched = SCHEDULER.lock();
    sched.server_endpoint(id)
}

/// The main scheduler loop.  Must be called once after all subsystems are
/// initialized.  Never returns.
///
/// Runs on the boot stack.  Checks the `RESCHEDULE` flag before halting to
/// avoid sleeping through a tick that was set while the previous task was
/// running (P4-T006).  `switch_context` saves/restores RFLAGS so each task
/// carries its own interrupt state — no `without_interrupts` wrapper needed.
///
/// The idle path uses `disable()` + `enable_and_hlt()` to close the
/// lost-wakeup race: if a timer IRQ sets `RESCHEDULE` after we clear the flag
/// but before `hlt`, it is queued while interrupts are disabled and fires
/// immediately when `enable_and_hlt()` re-enables IF, waking us at once.
pub fn run() -> ! {
    loop {
        // Disable interrupts before swapping RESCHEDULE to close the
        // lost-wakeup race: an IRQ that fires between swap(false) and hlt
        // would be queued (not lost) and wakes us the moment we re-enable IF.
        interrupts::disable();
        if !RESCHEDULE.swap(false, Ordering::AcqRel) {
            // No pending reschedule; atomically enable interrupts and halt.
            // Any IRQ queued while we had interrupts off fires immediately,
            // so we never sleep with a pending tick.
            interrupts::enable_and_hlt();
            // After waking, loop back — the IRQ may or may not have been a
            // timer tick, so we re-check RESCHEDULE before dispatching.
            continue;
        }
        // A tick is pending; re-enable interrupts before picking a task.
        interrupts::enable();

        // Wake any notification waiters whose PENDING bits were set by
        // signal_irq().  signal_irq() cannot call wake_task() (not ISR-safe),
        // so we drain here in task context on each scheduler tick.
        crate::ipc::notification::drain_pending_waiters();

        // Remove any tasks that exited (Dead state) since the last tick.
        // This reclaims kernel stack memory and keeps the task vec bounded.
        {
            let mut sched = SCHEDULER.lock();
            sched.drain_dead();
        }

        // Pick the next ready task.
        let next = {
            let mut sched = SCHEDULER.lock();
            sched.pick_next()
        };

        let (task_rsp, task_idx) = match next {
            Some(t) => t,
            None => continue, // no tasks registered yet — check flag again
        };

        // Mark the chosen task as Running.
        {
            let mut sched = SCHEDULER.lock();
            sched.tasks[task_idx].state = TaskState::Running;
            sched.current = Some(task_idx);
            // last_run is updated inside pick_next for non-idle tasks.
        }

        // Switch to the task.  Returns here when the task calls yield_now() or
        // one of the block_current_on_* IPC primitives.
        // switch_context restores the task's saved RFLAGS (IF=1 for a fresh
        // task, whatever the task last had for a resumed one).
        // Safety: SCHEDULER_RSP is only written here (single CPU).
        //         task_rsp is the value read from Task::saved_rsp.
        unsafe {
            switch_context(core::ptr::addr_of_mut!(SCHEDULER_RSP), task_rsp);
        }

        // The task returned to the scheduler via yield_now() or a block_current
        // primitive.  Both clear sched.current and set the appropriate state.
        // Just loop back to pick the next ready task.
    }
}
