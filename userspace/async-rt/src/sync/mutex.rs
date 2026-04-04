//! Async mutex for the single-threaded cooperative executor.
//!
//! This mutex is designed for use within a single-threaded async executor.
//! It uses `Cell`/`RefCell` internally and is NOT `Send` or `Sync`.

#[cfg(not(feature = "std"))]
use alloc::collections::VecDeque;
#[cfg(feature = "std")]
use std::collections::VecDeque;

use core::cell::{Cell, RefCell, UnsafeCell};
use core::future::Future;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

/// An async mutex for single-threaded executors.
///
/// This mutex is not `Send` or `Sync` — it relies on `Cell` and `RefCell`
/// for interior mutability, which is safe in a cooperative single-threaded
/// executor where only one task runs at a time.
pub struct Mutex<T> {
    locked: Cell<bool>,
    value: UnsafeCell<T>,
    waiters: RefCell<VecDeque<Waker>>,
}

impl<T> Mutex<T> {
    /// Create a new unlocked mutex wrapping the given value.
    pub fn new(value: T) -> Self {
        Self {
            locked: Cell::new(false),
            value: UnsafeCell::new(value),
            waiters: RefCell::new(VecDeque::new()),
        }
    }

    /// Try to acquire the lock without blocking.
    /// Returns `Some(MutexGuard)` if the lock was not held, `None` otherwise.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self.locked.get() {
            None
        } else {
            self.locked.set(true);
            Some(MutexGuard { mutex: self })
        }
    }

    /// Returns a future that resolves to a `MutexGuard` once the lock is acquired.
    pub fn lock(&self) -> MutexLockFuture<'_, T> {
        MutexLockFuture {
            mutex: self,
            queued: false,
        }
    }
}

/// Future returned by [`Mutex::lock`].
pub struct MutexLockFuture<'a, T> {
    mutex: &'a Mutex<T>,
    queued: bool,
}

impl<'a, T> Future for MutexLockFuture<'a, T> {
    type Output = MutexGuard<'a, T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !self.mutex.locked.get() {
            // Fast path: lock is free.
            self.mutex.locked.set(true);
            Poll::Ready(MutexGuard { mutex: self.mutex })
        } else if !self.queued {
            // First time we see contention: enqueue our waker.
            self.mutex
                .waiters
                .borrow_mut()
                .push_back(cx.waker().clone());
            self.queued = true;
            Poll::Pending
        } else {
            // Already queued — waker is in the queue, just stay pending.
            Poll::Pending
        }
    }
}

/// RAII guard returned by [`Mutex::lock`].
///
/// Dereferences to the inner value. Unlocks the mutex and wakes one
/// waiting task when dropped.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // Safety: we hold the lock, and the executor is single-threaded,
        // so no other code can access the UnsafeCell concurrently.
        unsafe { &*self.mutex.value.get() }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        // Safety: same as Deref — exclusive access guaranteed by the lock.
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        self.mutex.locked.set(false);
        // Wake the next waiter, if any.
        if let Some(waker) = self.mutex.waiters.borrow_mut().pop_front() {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{block_on, spawn};
    use crate::reactor::Reactor;

    #[cfg(not(feature = "std"))]
    use alloc::rc::Rc;
    #[cfg(not(feature = "std"))]
    use alloc::vec::Vec;
    #[cfg(feature = "std")]
    use std::rc::Rc;

    /// A yield-once future: returns `Pending` the first time, then `Ready`.
    struct Yield {
        done: bool,
    }

    impl Yield {
        fn new() -> Self {
            Self { done: false }
        }
    }

    impl Future for Yield {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.done {
                Poll::Ready(())
            } else {
                self.done = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    // T009.1: Uncontended lock — lock, modify, unlock, verify.
    #[test]
    fn test_uncontended_lock() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let m = Mutex::new(0u32);
            {
                let mut guard = m.lock().await;
                *guard += 42;
            }
            let guard = m.lock().await;
            *guard
        });
        assert_eq!(result, 42);
    }

    // T009.2: Two tasks contend — each increments 100 times with yield, total 200.
    #[test]
    fn test_two_task_contention() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let m = Rc::new(Mutex::new(0u32));

            let m1 = m.clone();
            let h1 = spawn(async move {
                for _ in 0..100 {
                    {
                        let mut guard = m1.lock().await;
                        *guard += 1;
                    }
                    Yield::new().await;
                }
            });

            let m2 = m.clone();
            let h2 = spawn(async move {
                for _ in 0..100 {
                    {
                        let mut guard = m2.lock().await;
                        *guard += 1;
                    }
                    Yield::new().await;
                }
            });

            h1.await.unwrap();
            h2.await.unwrap();

            let guard = m.lock().await;
            *guard
        });
        assert_eq!(result, 200);
    }

    // T009.3: Three tasks contend — verify all three get access.
    #[test]
    fn test_three_task_fairness() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let m = Rc::new(Mutex::new(Vec::<u8>::new()));

            let m1 = m.clone();
            let h1 = spawn(async move {
                for _ in 0..5 {
                    {
                        let mut guard = m1.lock().await;
                        guard.push(1);
                    }
                    Yield::new().await;
                }
            });

            let m2 = m.clone();
            let h2 = spawn(async move {
                for _ in 0..5 {
                    {
                        let mut guard = m2.lock().await;
                        guard.push(2);
                    }
                    Yield::new().await;
                }
            });

            let m3 = m.clone();
            let h3 = spawn(async move {
                for _ in 0..5 {
                    {
                        let mut guard = m3.lock().await;
                        guard.push(3);
                    }
                    Yield::new().await;
                }
            });

            h1.await.unwrap();
            h2.await.unwrap();
            h3.await.unwrap();

            let guard = m.lock().await;
            let v = guard.clone();
            v
        });

        // All three task ids should be present.
        assert!(result.contains(&1), "task 1 never got access");
        assert!(result.contains(&2), "task 2 never got access");
        assert!(result.contains(&3), "task 3 never got access");
        assert_eq!(result.len(), 15);
    }

    // T009.4: Guard drop wakes waiter.
    #[test]
    fn test_guard_drop_wakes_waiter() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let m = Rc::new(Mutex::new(0u32));

            // Task A: lock, yield (so B can try), then unlock.
            let m_a = m.clone();
            let h_a = spawn(async move {
                let mut guard = m_a.lock().await;
                *guard = 1;
                Yield::new().await;
                // guard is dropped here at end of scope
            });

            // Task B: tries to lock — should be pending until A drops guard.
            let m_b = m.clone();
            let h_b = spawn(async move {
                let mut guard = m_b.lock().await;
                // Should see the value A wrote.
                assert_eq!(*guard, 1);
                *guard = 2;
            });

            h_a.await.unwrap();
            h_b.await.unwrap();

            let guard = m.lock().await;
            *guard
        });
        assert_eq!(result, 2);
    }

    // T009.5: Uncontended lock/unlock does not touch the waiters queue.
    #[test]
    fn test_fast_path_no_allocation() {
        let mut reactor = Reactor::new();
        block_on(&mut reactor, async {
            let m = Mutex::new(42u32);

            // Lock and unlock — waiters queue should remain empty.
            {
                let guard = m.lock().await;
                assert_eq!(*guard, 42);
            }
            {
                let mut guard = m.lock().await;
                *guard = 99;
            }

            // The waiters queue should still be empty (no one ever contended).
            assert!(m.waiters.borrow().is_empty());

            let guard = m.lock().await;
            assert_eq!(*guard, 99);
        });
    }
}
