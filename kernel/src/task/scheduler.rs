//! Round-robin kernel scheduler.
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
    // Safety: SCHEDULER_RSP was written by the scheduler loop in `run()`.
    let sched_rsp = unsafe { SCHEDULER_RSP };
    // Safety: task_rsp_ptr is a valid aligned pointer inside a live Task.
    unsafe { switch_context(task_rsp_ptr, sched_rsp) };
    // Execution resumes here when the scheduler dispatches this task again.
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

        // Switch to the task.  Returns here when the task calls yield_now().
        // switch_context restores the task's saved RFLAGS (IF=1 for a fresh
        // task, whatever the task last had for a resumed one).
        // Safety: SCHEDULER_RSP is only written here (single CPU).
        //         task_rsp is the value read from Task::saved_rsp.
        unsafe {
            switch_context(core::ptr::addr_of_mut!(SCHEDULER_RSP), task_rsp);
        }

        // yield_now() has already cleared sched.current and marked the task
        // Ready, so we just loop back to pick the next one.
    }
}
