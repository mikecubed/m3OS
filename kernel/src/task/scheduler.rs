//! Round-robin kernel scheduler.
//!
//! # Design
//!
//! The scheduler owns all [`Task`]s in a `Vec`.  On each timer tick the PIC
//! ISR calls [`signal_reschedule`], which atomically sets [`RESCHEDULE`].
//! The scheduler loop (running on the boot stack) wakes from `hlt`, checks the
//! flag, picks the next `Ready` task in round-robin order, and uses
//! [`switch_context`] to transfer execution to it.
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

use super::{switch_context, Task, TaskState};

// ---------------------------------------------------------------------------
// Statics
// ---------------------------------------------------------------------------

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

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

struct Scheduler {
    tasks: Vec<Task>,
    /// Index of the last task that was dispatched (for round-robin wrap).
    last_run: usize,
    /// Index of the task currently holding the CPU, or `None` while the
    /// scheduler loop itself is running.
    current: Option<usize>,
}

impl Scheduler {
    const fn new() -> Self {
        Scheduler {
            tasks: Vec::new(),
            last_run: 0,
            current: None,
        }
    }

    /// Pick the next `Ready` task using round-robin, starting after
    /// `last_run`.  Returns `(saved_rsp, index)` or `None` if no task is
    /// ready.
    fn pick_next(&mut self) -> Option<(u64, usize)> {
        let n = self.tasks.len();
        if n == 0 {
            return None;
        }
        let start = (self.last_run + 1) % n;
        for i in 0..n {
            let idx = (start + i) % n;
            if self.tasks[idx].state == TaskState::Ready {
                return Some((self.tasks[idx].saved_rsp, idx));
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

/// Yield the current task back to the scheduler.
///
/// Marks the task `Ready`, clears `sched.current`, then context-switches to
/// the scheduler loop.  Returns when the scheduler dispatches this task again.
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
    // Safety: SCHEDULER_RSP was written by the scheduler loop in `run()`.
    let sched_rsp = unsafe { SCHEDULER_RSP };
    // Safety: task_rsp_ptr is a valid aligned pointer inside a live Task.
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
    // Execution resumes here when the scheduler dispatches this task again.
}

/// The main scheduler loop.  Must be called once after all subsystems are
/// initialized.  Never returns.
///
/// Runs on the boot stack.  On each timer tick (`RESCHEDULE` set) it picks the
/// next ready task and switches to it.  When no task is ready it halts again —
/// this is the idle behavior (P4-T005).
pub fn run() -> ! {
    loop {
        // Halt until the next interrupt (conserves power, avoids busy-spin).
        x86_64::instructions::hlt();

        if !RESCHEDULE.swap(false, Ordering::AcqRel) {
            continue;
        }

        // Pick the next ready task.
        let next = {
            let mut sched = SCHEDULER.lock();
            sched.pick_next()
        };

        let (task_rsp, task_idx) = match next {
            Some(t) => t,
            None => continue, // nothing ready — hlt again (idle path, P4-T009)
        };

        // Mark the chosen task as Running.
        {
            let mut sched = SCHEDULER.lock();
            sched.tasks[task_idx].state = TaskState::Running;
            sched.current = Some(task_idx);
            sched.last_run = task_idx;
        }

        // Switch to the task.  Returns here when the task calls yield_now().
        // Safety: SCHEDULER_RSP is only written here (single CPU).
        //         task_rsp is the value read from Task::saved_rsp.
        unsafe {
            switch_context(core::ptr::addr_of_mut!(SCHEDULER_RSP), task_rsp);
        }

        // yield_now() has already cleared sched.current and marked the task
        // Ready, so we just loop back to pick the next one.
    }
}
