//! Bounded multi-producer, single-consumer (MPSC) async channel.
//!
//! Uses `Rc` for shared state since the channel is confined to a single
//! executor thread. The channel is bounded: senders block (return Pending)
//! when the buffer is full.

#[cfg(not(feature = "std"))]
use alloc::collections::VecDeque;
#[cfg(not(feature = "std"))]
use alloc::rc::Rc;

#[cfg(feature = "std")]
use std::collections::VecDeque;
#[cfg(feature = "std")]
use std::rc::Rc;

use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

// ---------------------------------------------------------------------------
// Shared channel state
// ---------------------------------------------------------------------------

struct ChannelInner<T> {
    buffer: RefCell<VecDeque<T>>,
    capacity: usize,
    closed: Cell<bool>,
    rx_waker: RefCell<Option<Waker>>,
    tx_waiters: RefCell<VecDeque<Waker>>,
    sender_count: Cell<usize>,
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Error returned when sending on a closed channel.
///
/// Contains the value that could not be sent.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T> core::fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "channel closed")
    }
}

// ---------------------------------------------------------------------------
// Channel constructor
// ---------------------------------------------------------------------------

/// Create a bounded MPSC channel with the given capacity.
///
/// Returns a `(Sender, Receiver)` pair. The channel can buffer up to
/// `capacity` messages before senders block.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    assert!(capacity > 0, "channel capacity must be at least 1");
    let inner = Rc::new(ChannelInner {
        buffer: RefCell::new(VecDeque::with_capacity(capacity)),
        capacity,
        closed: Cell::new(false),
        rx_waker: RefCell::new(None),
        tx_waiters: RefCell::new(VecDeque::new()),
        sender_count: Cell::new(1),
    });
    (
        Sender {
            inner: inner.clone(),
        },
        Receiver { inner },
    )
}

// ---------------------------------------------------------------------------
// Sender
// ---------------------------------------------------------------------------

/// The sending half of a bounded MPSC channel.
///
/// Can be cloned to create multiple producers.
pub struct Sender<T> {
    inner: Rc<ChannelInner<T>>,
}

impl<T> Sender<T> {
    /// Send a value into the channel.
    ///
    /// Returns a future that resolves once the value has been accepted
    /// into the buffer. If the buffer is full, the future will yield
    /// `Pending` until space is available.
    pub fn send(&self, value: T) -> SendFuture<'_, T> {
        SendFuture {
            sender: self,
            value: Some(value),
            queued: false,
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let count = self.inner.sender_count.get();
        self.inner.sender_count.set(count + 1);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let count = self.inner.sender_count.get();
        self.inner.sender_count.set(count - 1);
        if count == 1 {
            // Last sender dropped — mark channel as closed.
            self.inner.closed.set(true);
            // Wake receiver so it can observe the closed state.
            if let Some(w) = self.inner.rx_waker.borrow_mut().take() {
                w.wake();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SendFuture
// ---------------------------------------------------------------------------

/// Future returned by [`Sender::send`].
pub struct SendFuture<'a, T> {
    sender: &'a Sender<T>,
    value: Option<T>,
    queued: bool,
}

impl<T> Future for SendFuture<'_, T> {
    type Output = Result<(), SendError<T>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: we never move `self` out of the Pin; `T` doesn't need pinning.
        let this = unsafe { self.get_unchecked_mut() };
        let inner = &this.sender.inner;

        // Channel closed — return the unsent value.
        if inner.closed.get() {
            if let Some(val) = this.value.take() {
                return Poll::Ready(Err(SendError(val)));
            }
        }

        let mut buf = inner.buffer.borrow_mut();
        if buf.len() < inner.capacity {
            // There is space — push the value.
            if let Some(val) = this.value.take() {
                buf.push_back(val);
                drop(buf);
                // Wake the receiver.
                if let Some(w) = inner.rx_waker.borrow_mut().take() {
                    w.wake();
                }
                return Poll::Ready(Ok(()));
            }
        }
        drop(buf);

        // Buffer is full — register our waker and return Pending.
        // Only enqueue once; the waker remains valid across re-polls in
        // a single-threaded executor (same TaskHeader-based waker each time).
        if !this.queued {
            inner.tx_waiters.borrow_mut().push_back(cx.waker().clone());
            this.queued = true;
        }
        Poll::Pending
    }
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

/// The receiving half of a bounded MPSC channel.
///
/// Not `Clone` — there is only one consumer.
pub struct Receiver<T> {
    inner: Rc<ChannelInner<T>>,
}

impl<T> Receiver<T> {
    /// Receive a value from the channel.
    ///
    /// Returns `Some(value)` when a message is available, or `None` when
    /// all senders have been dropped and the buffer is empty.
    pub fn recv(&self) -> RecvFuture<'_, T> {
        RecvFuture { receiver: self }
    }
}

// ---------------------------------------------------------------------------
// RecvFuture
// ---------------------------------------------------------------------------

/// Future returned by [`Receiver::recv`].
pub struct RecvFuture<'a, T> {
    receiver: &'a Receiver<T>,
}

impl<T> Future for RecvFuture<'_, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let inner = &this.receiver.inner;

        let mut buf = inner.buffer.borrow_mut();
        if let Some(val) = buf.pop_front() {
            drop(buf);
            // Wake one blocked sender now that there is space.
            if let Some(w) = inner.tx_waiters.borrow_mut().pop_front() {
                w.wake();
            }
            return Poll::Ready(Some(val));
        }
        drop(buf);

        // Buffer is empty.
        if inner.closed.get() {
            return Poll::Ready(None);
        }

        // Not closed, buffer empty — register waker and wait.
        *inner.rx_waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{block_on, spawn};
    use crate::reactor::Reactor;

    #[test]
    fn test_send_recv_basic() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let (tx, rx) = channel::<i32>(8);
            tx.send(1).await.unwrap();
            tx.send(2).await.unwrap();
            tx.send(3).await.unwrap();
            let a = rx.recv().await.unwrap();
            let b = rx.recv().await.unwrap();
            let c = rx.recv().await.unwrap();
            (a, b, c)
        });
        assert_eq!(result, (1, 2, 3));
    }

    #[test]
    fn test_bounded_backpressure() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let (tx, rx) = channel::<i32>(2);

            // Fill the buffer.
            tx.send(1).await.unwrap();
            tx.send(2).await.unwrap();

            // Third send should block — spawn it as a task.
            let tx2 = tx.clone();
            let handle = spawn(async move {
                tx2.send(3).await.unwrap();
                42
            });

            // Receive one value to make space.
            let first = rx.recv().await.unwrap();
            // Now the spawned send should complete.
            let completed = handle.await.unwrap();

            let second = rx.recv().await.unwrap();
            let third = rx.recv().await.unwrap();

            (first, second, third, completed)
        });
        assert_eq!(result, (1, 2, 3, 42));
    }

    #[test]
    fn test_receiver_gets_none_on_close() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let (tx, rx) = channel::<i32>(8);
            tx.send(10).await.unwrap();
            tx.send(20).await.unwrap();
            drop(tx);

            let a = rx.recv().await;
            let b = rx.recv().await;
            let c = rx.recv().await;
            (a, b, c)
        });
        assert_eq!(result, (Some(10), Some(20), None));
    }

    #[test]
    fn test_multiple_senders() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let (tx, rx) = channel::<i32>(16);
            let tx2 = tx.clone();

            let h1 = spawn(async move {
                tx.send(1).await.unwrap();
                tx.send(2).await.unwrap();
            });
            let h2 = spawn(async move {
                tx2.send(3).await.unwrap();
                tx2.send(4).await.unwrap();
            });

            h1.await.unwrap();
            h2.await.unwrap();

            let mut vals = Vec::new();
            for _ in 0..4 {
                vals.push(rx.recv().await.unwrap());
            }
            vals.sort();
            vals
        });
        assert_eq!(result, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_100_values_in_order() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let (tx, rx) = channel::<usize>(16);

            let producer = spawn(async move {
                for i in 0..100 {
                    tx.send(i).await.unwrap();
                }
            });

            let mut received = Vec::new();
            for _ in 0..100 {
                received.push(rx.recv().await.unwrap());
            }

            producer.await.unwrap();
            received
        });

        let expected: Vec<usize> = (0..100).collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_empty_recv_blocks() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let (tx, rx) = channel::<i32>(4);

            // Spawn receiver first — will block because channel is empty.
            let recv_handle = spawn(async move { rx.recv().await.unwrap() });

            // Spawn sender — will unblock the receiver.
            let send_handle = spawn(async move {
                tx.send(99).await.unwrap();
            });

            send_handle.await.unwrap();
            recv_handle.await.unwrap()
        });
        assert_eq!(result, 99);
    }

    // Verify that re-polling a blocked SendFuture does not enqueue duplicate
    // wakers.  With a capacity-1 channel, fill the buffer, then spawn a
    // sender that blocks.  Yield multiple times so the executor re-polls
    // the blocked send.  Then drain one value — exactly one waker should
    // fire, not N duplicates.
    #[test]
    fn test_send_no_duplicate_wakers() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let (tx, rx) = channel::<i32>(1);

            // Fill the single-slot buffer.
            tx.send(1).await.unwrap();

            // Spawn a sender that will block (buffer full).
            let tx2 = tx.clone();
            let handle = spawn(async move {
                tx2.send(2).await.unwrap();
                42
            });

            // Yield several times — the executor re-polls the blocked
            // SendFuture.  With the old code each re-poll would push
            // another waker into tx_waiters; with the fix only one entry
            // exists.
            for _ in 0..10 {
                core::future::poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    core::task::Poll::Ready(())
                })
                .await;
            }

            // Drain one value — should wake exactly one sender.
            let first = rx.recv().await.unwrap();
            let completed = handle.await.unwrap();

            // The channel should have exactly one value (the unblocked send).
            let second = rx.recv().await.unwrap();
            (first, second, completed)
        });
        assert_eq!(result, (1, 2, 42));
    }
}
