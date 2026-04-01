//! Wait queue primitive for blocking kernel tasks (Phase 35, Track G).
//!
//! A `WaitQueue` holds a list of tasks waiting for some event. Tasks call
//! `sleep()` to block; other code calls `wake_one()` or `wake_all()` to
//! unblock them.

extern crate alloc;

use alloc::collections::VecDeque;
use spin::Mutex;

use super::{TaskId, scheduler};

/// A queue of tasks waiting for an event.
pub struct WaitQueue {
    waiters: Mutex<VecDeque<TaskId>>,
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
    pub fn sleep(&self) {
        if let Some(id) = scheduler::current_task_id() {
            self.waiters.lock().push_back(id);
            scheduler::block_current_on_recv();
        }
    }

    /// Wake the first waiting task, if any.
    pub fn wake_one(&self) {
        if let Some(id) = self.waiters.lock().pop_front() {
            scheduler::wake_task(id);
        }
    }

    /// Wake all waiting tasks.
    pub fn wake_all(&self) {
        let waiters: VecDeque<TaskId> = {
            let mut q = self.waiters.lock();
            core::mem::take(&mut *q)
        };
        for id in waiters {
            scheduler::wake_task(id);
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
