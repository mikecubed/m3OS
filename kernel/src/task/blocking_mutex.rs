//! Blocking mutex that sleeps waiters instead of spinning (Phase 35, Track G).
//!
//! Suitable for long-held locks (filesystem, network). NOT suitable for
//! interrupt handlers or the scheduler itself — those keep spin locks.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use super::wait_queue::WaitQueue;

/// A mutex that puts contending tasks to sleep instead of spinning.
pub struct BlockingMutex<T> {
    locked: AtomicBool,
    queue: WaitQueue,
    data: UnsafeCell<T>,
}

// Safety: BlockingMutex provides synchronized access via the atomic flag
// and wait queue. Only one task accesses the data at a time.
unsafe impl<T: Send> Send for BlockingMutex<T> {}
unsafe impl<T: Send> Sync for BlockingMutex<T> {}

impl<T> BlockingMutex<T> {
    pub const fn new(data: T) -> Self {
        BlockingMutex {
            locked: AtomicBool::new(false),
            queue: WaitQueue::new(),
            data: UnsafeCell::new(data),
        }
    }

    /// Acquire the lock, sleeping if it is already held.
    pub fn lock(&self) -> BlockingMutexGuard<'_, T> {
        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.queue.sleep();
        }
        BlockingMutexGuard { mutex: self }
    }
}

/// RAII guard for [`BlockingMutex`]. Releases the lock and wakes one
/// waiter on drop.
pub struct BlockingMutexGuard<'a, T> {
    mutex: &'a BlockingMutex<T>,
}

impl<T> Deref for BlockingMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> DerefMut for BlockingMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for BlockingMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
        self.mutex.queue.wake_one();
    }
}
