//! Waker and Task primitives for the async executor.

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;
#[cfg(not(feature = "std"))]
use alloc::rc::Rc;
#[cfg(feature = "std")]
use std::rc::Rc;

use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
use core::task::{RawWaker, RawWakerVTable, Waker};

// Global write-end FD for the self-pipe. When a waker fires, it writes a byte
// here to unblock the reactor's `poll()`. Initialized to -1 (not set).
#[cfg(feature = "std")]
thread_local! {
    static WAKE_PIPE_FD: Cell<i32> = const { Cell::new(-1) };
}

#[cfg(not(feature = "std"))]
static WAKE_PIPE_FD: GlobalCell = GlobalCell(Cell::new(-1));

#[cfg(not(feature = "std"))]
#[repr(transparent)]
struct GlobalCell(Cell<i32>);

// Safety: single-threaded executor — no concurrent access.
#[cfg(not(feature = "std"))]
unsafe impl Sync for GlobalCell {}

/// Set the wake pipe write-end FD.
pub fn set_wake_pipe_fd(fd: i32) {
    #[cfg(feature = "std")]
    WAKE_PIPE_FD.with(|c| c.set(fd));
    #[cfg(not(feature = "std"))]
    WAKE_PIPE_FD.0.set(fd);
}

/// Get the wake pipe write-end FD.
pub fn get_wake_pipe_fd() -> i32 {
    #[cfg(feature = "std")]
    return WAKE_PIPE_FD.with(|c| c.get());
    #[cfg(not(feature = "std"))]
    return WAKE_PIPE_FD.0.get();
}

/// Shared inner state for a waker — holds the "woken" flag.
pub(crate) struct WakerInner {
    woken: Cell<bool>,
}

/// A single-threaded async task wrapping a future and its waker state.
pub struct Task {
    #[allow(dead_code)]
    pub(crate) future: Pin<Box<dyn Future<Output = ()>>>,
    pub(crate) inner: Rc<WakerInner>,
}

impl Task {
    /// Create a new task wrapping the given future.
    pub fn new(future: Pin<Box<dyn Future<Output = ()>>>) -> Self {
        Self {
            future,
            inner: Rc::new(WakerInner {
                woken: Cell::new(true), // start woken so executor polls immediately
            }),
        }
    }

    /// Check whether this task has been woken.
    pub fn is_woken(&self) -> bool {
        self.inner.woken.get()
    }

    /// Clear the woken flag (called before polling).
    pub fn clear_woken(&self) {
        self.inner.woken.set(false);
    }
}

/// Build a `Waker` from a task's inner state.
pub(crate) fn task_waker(inner: &Rc<WakerInner>) -> Waker {
    let ptr = Rc::into_raw(inner.clone()) as *const ();
    let raw = RawWaker::new(ptr, &VTABLE);
    // Safety: RawWaker constructed with valid vtable and refcounted pointer.
    unsafe { Waker::from_raw(raw) }
}

/// Build a `Waker` for a `Task`.
pub fn waker_for_task(task: &Task) -> Waker {
    task_waker(&task.inner)
}

const VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, wake_fn, wake_by_ref_fn, drop_fn);

unsafe fn clone_fn(ptr: *const ()) -> RawWaker {
    let rc = unsafe { Rc::from_raw(ptr as *const WakerInner) };
    let cloned = rc.clone();
    // Don't drop the original — we still need it.
    let _ = Rc::into_raw(rc);
    RawWaker::new(Rc::into_raw(cloned) as *const (), &VTABLE)
}

unsafe fn wake_fn(ptr: *const ()) {
    let rc = unsafe { Rc::from_raw(ptr as *const WakerInner) };
    rc.woken.set(true);
    signal_wake_pipe();
    // rc is dropped here, decrementing refcount
}

unsafe fn wake_by_ref_fn(ptr: *const ()) {
    let rc = unsafe { Rc::from_raw(ptr as *const WakerInner) };
    rc.woken.set(true);
    signal_wake_pipe();
    // Don't drop — this is wake_by_ref
    let _ = Rc::into_raw(rc);
}

unsafe fn drop_fn(ptr: *const ()) {
    let _ = unsafe { Rc::from_raw(ptr as *const WakerInner) };
    // rc is dropped here
}

/// Write a single byte to the self-pipe to unblock `poll()`.
fn signal_wake_pipe() {
    let fd = get_wake_pipe_fd();
    if fd < 0 {
        return;
    }
    let byte: [u8; 1] = [1];
    #[cfg(feature = "std")]
    {
        unsafe {
            libc::write(fd, byte.as_ptr() as *const _, 1);
        }
    }
    #[cfg(not(feature = "std"))]
    {
        syscall_lib::write(fd, &byte);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // B.1: Construct a Task, create a Waker, call wake(), assert is_woken()
    #[test]
    fn test_task_woken_flag() {
        let task = Task::new(Box::pin(async {}));
        task.clear_woken();
        assert!(!task.is_woken());

        let waker = waker_for_task(&task);
        waker.wake_by_ref();
        assert!(task.is_woken());
    }

    // B.1: Calling wake() twice is idempotent
    #[test]
    fn test_wake_idempotent() {
        let task = Task::new(Box::pin(async {}));
        task.clear_woken();

        let waker = waker_for_task(&task);
        waker.wake_by_ref();
        waker.wake_by_ref();
        assert!(task.is_woken());
    }

    // B.2: Clone a Waker, wake via the clone, verify original Task is woken
    #[test]
    fn test_waker_clone_wake() {
        let task = Task::new(Box::pin(async {}));
        task.clear_woken();

        let waker = waker_for_task(&task);
        let cloned = waker.clone();
        drop(waker);
        cloned.wake_by_ref();
        assert!(task.is_woken());
    }

    // B.2: Drop both original and clone without panic
    #[test]
    fn test_waker_clone_drop() {
        let task = Task::new(Box::pin(async {}));
        let waker = waker_for_task(&task);
        let cloned = waker.clone();
        drop(waker);
        drop(cloned);
        // no panic = pass
    }

    // B.3: Self-pipe wake integration
    #[test]
    fn test_self_pipe_wake() {
        let mut fds = [0i32; 2];
        unsafe {
            libc::pipe(fds.as_mut_ptr());
        }
        set_wake_pipe_fd(fds[1]);

        let task = Task::new(Box::pin(async {}));
        task.clear_woken();
        let waker = waker_for_task(&task);
        waker.wake_by_ref();

        let mut buf = [0u8; 1];
        let n = unsafe { libc::read(fds[0], buf.as_mut_ptr() as *mut _, 1) };
        assert_eq!(n, 1);

        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        set_wake_pipe_fd(-1);
    }

    // B.3: wake() when WAKE_PIPE_FD is -1 does not panic
    #[test]
    fn test_wake_no_pipe() {
        set_wake_pipe_fd(-1);
        let task = Task::new(Box::pin(async {}));
        task.clear_woken();
        let waker = waker_for_task(&task);
        waker.wake(); // should not panic
        assert!(task.is_woken());
    }
}
