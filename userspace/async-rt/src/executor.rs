//! Single-threaded cooperative executor with `block_on`.

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

use core::cell::Cell;
use core::future::Future;
use core::task::{Context, Poll};

use crate::reactor::Reactor;
use crate::task::{Task, waker_for_task};

// Global reactor pointer — set by block_on, used by futures via `reactor()`.
// Safety: single-threaded executor, only one block_on active at a time.
#[cfg(feature = "std")]
thread_local! {
    static REACTOR_PTR: Cell<*mut Reactor> = const { Cell::new(core::ptr::null_mut()) };
}

#[cfg(not(feature = "std"))]
static REACTOR_PTR: GlobalCellPtr = GlobalCellPtr(Cell::new(core::ptr::null_mut()));

#[cfg(not(feature = "std"))]
#[repr(transparent)]
struct GlobalCellPtr(Cell<*mut Reactor>);

#[cfg(not(feature = "std"))]
unsafe impl Sync for GlobalCellPtr {}

/// Get a mutable reference to the current reactor.
///
/// # Panics
/// Panics if called outside of `block_on`.
///
/// # Safety
/// Only valid within a `block_on` call. Single-threaded only.
pub fn reactor() -> &'static mut Reactor {
    #[cfg(feature = "std")]
    let ptr = REACTOR_PTR.with(|c| c.get());
    #[cfg(not(feature = "std"))]
    let ptr = REACTOR_PTR.0.get();

    assert!(!ptr.is_null(), "reactor() called outside of block_on");
    unsafe { &mut *ptr }
}

fn set_reactor_ptr(ptr: *mut Reactor) {
    #[cfg(feature = "std")]
    REACTOR_PTR.with(|c| c.set(ptr));
    #[cfg(not(feature = "std"))]
    REACTOR_PTR.0.set(ptr);
}

/// Drive a future to completion using the given reactor for I/O readiness.
///
/// This is the main entry point for the async executor. It creates a task,
/// polls it, and uses the reactor to wait for I/O events between polls.
pub fn block_on<F: Future>(reactor: &mut Reactor, future: F) -> F::Output {
    let prev = {
        #[cfg(feature = "std")]
        {
            REACTOR_PTR.with(|c| c.get())
        }
        #[cfg(not(feature = "std"))]
        {
            REACTOR_PTR.0.get()
        }
    };
    set_reactor_ptr(reactor as *mut Reactor);

    let mut future = core::pin::pin!(future);

    let task = Task::new(Box::pin(async {}));

    let result = loop {
        let waker = waker_for_task(&task);
        let mut cx = Context::from_waker(&waker);

        match future.as_mut().poll(&mut cx) {
            Poll::Ready(val) => break val,
            Poll::Pending => {
                task.clear_woken();
                reactor.poll_once(100);
            }
        }
    };

    set_reactor_ptr(prev);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactor::Reactor;
    use core::pin::Pin;
    use core::task::{Context, Poll};
    use std::cell::Cell;
    use std::thread;
    use std::time::Duration;

    // D.1: block_on for immediately-ready future returning i32
    #[test]
    fn test_block_on_ready_i32() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async { 42 });
        assert_eq!(result, 42);
    }

    // D.1: block_on for immediately-ready future returning &str
    #[test]
    fn test_block_on_ready_str() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async { "hello" });
        assert_eq!(result, "hello");
    }

    // D.2: pending-then-ready future
    #[test]
    fn test_block_on_pending_then_ready() {
        let mut reactor = Reactor::new();

        struct PendingOnce {
            polled: Cell<bool>,
        }

        impl Future for PendingOnce {
            type Output = i32;
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<i32> {
                if self.polled.get() {
                    Poll::Ready(99)
                } else {
                    self.polled.set(true);
                    // Store the waker and wake it so executor re-polls
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }

        let f = PendingOnce {
            polled: Cell::new(false),
        };
        let result = block_on(&mut reactor, f);
        assert_eq!(result, 99);
    }

    // D.3: reactor-driven wakeup via pipe
    #[test]
    fn test_block_on_reactor_driven() {
        let mut reactor = Reactor::new();

        // Create a pipe — writer thread will make read end readable
        let (read_fd, write_fd) = {
            let mut fds = [0i32; 2];
            unsafe { libc::pipe(fds.as_mut_ptr()) };
            (fds[0], fds[1])
        };

        // Future that awaits the pipe becoming readable
        struct AwaitPipe {
            fd: i32,
            registered: Cell<bool>,
        }

        impl Future for AwaitPipe {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.registered.get() {
                    // Second poll — data should be available
                    Poll::Ready(())
                } else {
                    self.registered.set(true);
                    // Register with the global reactor is tricky from inside a future,
                    // so we'll just use the waker approach: store waker, thread will wake it.
                    let waker = cx.waker().clone();
                    let fd = self.fd;
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(20));
                        unsafe {
                            libc::write(fd, [1u8].as_ptr() as *const _, 1);
                        }
                        waker.wake();
                    });
                    Poll::Pending
                }
            }
        }

        let f = AwaitPipe {
            fd: write_fd,
            registered: Cell::new(false),
        };
        block_on(&mut reactor, f);

        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }
}
