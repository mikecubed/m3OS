//! Wait queue primitive for blocking kernel tasks (Phase 35, Track G).
//!
//! A `WaitQueue` holds a list of tasks waiting for some event. Tasks call
//! `sleep()` to block; other code calls `wake_one()` or `wake_all()` to
//! unblock them.
//!
//! Each waiter carries an atomic `woken` flag so that a `wake_one()` or
//! `wake_all()` that races with the window between enqueue and block does
//! not lose the wakeup.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::{TaskId, scheduler};

/// An entry in the wait queue: task id + atomic woken token.
struct WaitEntry {
    id: TaskId,
    woken: Arc<AtomicBool>,
}

/// A queue of tasks waiting for an event.
pub struct WaitQueue {
    waiters: Mutex<VecDeque<WaitEntry>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        WaitQueue {
            waiters: Mutex::new(VecDeque::new()),
        }
    }

    /// Block the current task and add it to this wait queue.
    ///
    /// The task is set to `BlockedOnRecv` state and will be woken when
    /// `wake_one()` or `wake_all()` is called.
    ///
    /// Uses an atomic `woken` flag to prevent lost wakeups: if a waker
    /// sets the flag between enqueue and the blocking call, the task
    /// skips blocking entirely.
    pub fn sleep(&self) {
        if let Some(id) = scheduler::current_task_id() {
            let woken = Arc::new(AtomicBool::new(false));
            self.waiters.lock().push_back(WaitEntry {
                id,
                woken: Arc::clone(&woken),
            });
            // Atomically check woken flag under the scheduler lock before blocking.
            // This prevents the TOCTOU race where a waker sets woken between our
            // check and the actual block call.
            scheduler::block_current_unless_woken(&woken);
        }
    }

    /// Register a task on this wait queue without blocking.
    ///
    /// Used by poll/select to register on multiple wait queues before
    /// doing a single block. The caller provides a shared `woken` flag
    /// so that a wakeup on ANY queue prevents blocking.
    pub fn register(&self, id: TaskId, woken: &Arc<AtomicBool>) {
        self.waiters.lock().push_back(WaitEntry {
            id,
            woken: Arc::clone(woken),
        });
    }

    /// Remove all entries for the given task from this wait queue.
    pub fn deregister(&self, id: TaskId) {
        self.waiters.lock().retain(|e| e.id != id);
    }

    /// Wake the first waiting task, if any.
    pub fn wake_one(&self) {
        if let Some(entry) = self.waiters.lock().pop_front() {
            entry.woken.store(true, Ordering::Release);
            scheduler::wake_task(entry.id);
        }
    }

    /// Wake all waiting tasks.
    pub fn wake_all(&self) {
        let waiters: VecDeque<WaitEntry> = {
            let mut q = self.waiters.lock();
            core::mem::take(&mut *q)
        };
        for entry in waiters {
            entry.woken.store(true, Ordering::Release);
            scheduler::wake_task(entry.id);
        }
    }

    /// Return the number of waiters.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.waiters.lock().len()
    }

    /// Return true if no waiters.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.waiters.lock().is_empty()
    }
}
