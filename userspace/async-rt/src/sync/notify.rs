//! Single-consumer async notification primitive.
//!
//! `Notify` allows one or more signalers to wake a single waiting task.
//! It is edge-triggered: `signal()` sets a flag and wakes the stored waker.
//! `wait()` returns immediately if a signal is pending, or suspends until
//! the next `signal()` call.
//!
//! Designed for single-threaded cooperative executors — uses `Cell`, not
//! `Send` or `Sync`.

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicU64, Ordering};
use core::task::{Context, Poll, Waker};

/// H9 instrumentation: total `Notify::signal()` calls that found a stored
/// waker and fired it. Read by the executor to attribute per-task wake
/// counts.
pub static NOTIFY_SIGNAL_FIRED: AtomicU64 = AtomicU64::new(0);
/// H9 instrumentation: total `Notify::signal()` calls that landed on an
/// empty (no-stored-waker) Notify. These do not increment a `wake_count`
/// but mark the signal as pending.
pub static NOTIFY_SIGNAL_PENDING: AtomicU64 = AtomicU64::new(0);

/// A single-consumer notification signal.
///
/// Used to wake a task when an event occurs. Multiple `signal()` calls
/// before a `wait()` coalesce into a single wakeup.
pub struct Notify {
    waker: Cell<Option<Waker>>,
    signalled: Cell<bool>,
    /// H9 instrumentation: per-instance counter of `signal()` calls that
    /// fired a stored waker.
    fired: AtomicU64,
    /// H9 instrumentation: per-instance counter of `signal()` calls that
    /// landed on an empty Notify (marked pending but no wake).
    pending: AtomicU64,
}

impl Notify {
    /// Create a new `Notify` in the unsignalled state.
    pub fn new() -> Self {
        Self {
            waker: Cell::new(None),
            signalled: Cell::new(false),
            fired: AtomicU64::new(0),
            pending: AtomicU64::new(0),
        }
    }

    /// H9: per-instance count of `signal()` calls that woke a stored waker.
    pub fn debug_fired(&self) -> u64 {
        self.fired.load(Ordering::Relaxed)
    }

    /// H9: per-instance count of `signal()` calls that landed on an empty
    /// Notify (signal stored as pending; no waker fired).
    pub fn debug_pending(&self) -> u64 {
        self.pending.load(Ordering::Relaxed)
    }

    /// Signal the waiting task. If a waker is registered, it is woken.
    /// If no task is currently waiting, the signal is stored and the
    /// next `wait()` will return immediately.
    pub fn signal(&self) {
        self.signalled.set(true);
        // Take the waker via replace rather than Cell::take (which isn't
        // available for non-Copy types on Cell). Use a swap-with-None pattern.
        let waker = self.waker.replace(None);
        if let Some(w) = waker {
            self.fired.fetch_add(1, Ordering::Relaxed);
            NOTIFY_SIGNAL_FIRED.fetch_add(1, Ordering::Relaxed);
            w.wake();
        } else {
            self.pending.fetch_add(1, Ordering::Relaxed);
            NOTIFY_SIGNAL_PENDING.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Returns a future that resolves when `signal()` is called.
    /// If a signal is already pending, resolves immediately.
    pub fn wait(&self) -> NotifyWait<'_> {
        NotifyWait { notify: self }
    }
}

/// Future returned by [`Notify::wait`].
pub struct NotifyWait<'a> {
    notify: &'a Notify,
}

impl<'a> Future for NotifyWait<'a> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.notify.signalled.replace(false) {
            // Signal was pending — consume it and return ready.
            Poll::Ready(())
        } else {
            // Store our waker for the next signal() call.
            self.notify.waker.set(Some(cx.waker().clone()));
            Poll::Pending
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
    #[cfg(feature = "std")]
    use std::rc::Rc;

    // Pre-signalled notify resolves immediately.
    #[test]
    fn test_signal_before_wait() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let n = Notify::new();
            n.signal();
            n.wait().await;
            42
        });
        assert_eq!(result, 42);
    }

    // Signal from another task wakes the waiter.
    #[test]
    fn test_signal_from_spawned_task() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let n = Rc::new(Notify::new());

            let n2 = n.clone();
            let _signaler = spawn(async move {
                n2.signal();
            });

            n.wait().await;
            99
        });
        assert_eq!(result, 99);
    }

    // Multiple signals before wait coalesce into one.
    #[test]
    fn test_multiple_signals_coalesce() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let n = Notify::new();
            n.signal();
            n.signal();
            n.signal();
            n.wait().await;
            // After consuming the signal, next wait should pend.
            // Can't test pending easily, but the first wait returned.
            1
        });
        assert_eq!(result, 1);
    }

    // Signal consumed by wait — second wait needs a new signal.
    #[test]
    fn test_signal_consumed_by_wait() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let n = Rc::new(Notify::new());
            n.signal();
            n.wait().await; // consumes the signal

            let n2 = n.clone();
            let _signaler = spawn(async move {
                n2.signal();
            });

            n.wait().await; // needs a fresh signal
            77
        });
        assert_eq!(result, 77);
    }

    // Waiter and signaler coordinate: producer signals, consumer wakes.
    #[test]
    fn test_signal_wakes_waiter_task() {
        use core::cell::Cell;

        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let n = Rc::new(Notify::new());
            let counter = Rc::new(Cell::new(0u32));

            // Consumer waits for a single signal, reads counter.
            let c2 = counter.clone();
            let n2 = n.clone();
            let consumer = spawn(async move {
                n2.wait().await;
                c2.get()
            });

            // Producer sets counter then signals.
            let c3 = counter.clone();
            let n3 = n.clone();
            let _producer = spawn(async move {
                c3.set(42);
                n3.signal();
            });

            consumer.await.unwrap()
        });
        assert_eq!(result, 42);
    }
}
