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

use super::scheduler::IrqSafeMutex;
use super::{TaskId, scheduler};

/// An entry in the wait queue: task id + atomic woken token.
struct WaitEntry {
    id: TaskId,
    woken: Arc<AtomicBool>,
}

/// A queue of tasks waiting for an event.
///
/// Phase 57b G.7 — `waiters` uses `IrqSafeMutex` so it inherits Track F.1's
/// preempt-discipline.  The wait-queue API is task-context only (sleep,
/// register, deregister, wake_one, wake_all are all called from kernel task
/// paths — never from an ISR; ISR-side wakers signal `AtomicBool` flags and
/// invoke `wake_task_v2` directly).
pub struct WaitQueue {
    waiters: IrqSafeMutex<VecDeque<WaitEntry>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        WaitQueue {
            waiters: IrqSafeMutex::new(VecDeque::new()),
        }
    }

    /// Block the current task until this wait queue is woken.
    ///
    /// The task transitions `Running → BlockedOnRecv` and yields to the
    /// scheduler.  It accumulates no CPU time while blocked.
    ///
    /// **Wake source:** any caller of [`WaitQueue::wake_one`] or
    /// [`WaitQueue::wake_all`], which set the per-waiter `woken` flag and
    /// call `wake_task_v2` to enqueue the task on the run queue.
    ///
    /// **Expected wake latency:** ≤ one scheduler quantum after the waker
    /// runs (typically < 10 ms for the 1 kHz tick, or within the same
    /// quantum on a multi-core system).
    ///
    /// **Lost-wakeup safety:** an atomic `woken` flag per waiter closes the
    /// TOCTOU window between enqueue and block.  If `wake_one`/`wake_all`
    /// races with the enqueue, `block_current_until`'s step-3 recheck
    /// observes the flag already `true` and self-reverts to `Running`
    /// without yielding.
    pub fn sleep(&self) {
        if let Some(id) = scheduler::current_task_id() {
            let woken = Arc::new(AtomicBool::new(false));
            self.waiters.lock().push_back(WaitEntry {
                id,
                woken: Arc::clone(&woken),
            });
            // F.6: under sched-v2 use block_current_until (v2 CAS primitive)
            // with no deadline; under v1 retain block_current_unless_woken.
            // The woken flag is set by wake_one/wake_all before calling
            // wake_task/wake_task_v2, so the TOCTOU window is closed in both
            // cases by the flag check inside block_current_until / pi_lock.
            {
                let _ = scheduler::block_current_until(
                    crate::task::TaskState::BlockedOnRecv,
                    &woken,
                    None,
                );
            }
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
            // F.6: under sched-v2 use wake_task_v2 (CAS-based); under v1 use wake_task.
            {
                let _ = scheduler::wake_task_v2(entry.id);
            }
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
            // F.6: under sched-v2 use wake_task_v2 (CAS-based); under v1 use wake_task.
            {
                let _ = scheduler::wake_task_v2(entry.id);
            }
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
