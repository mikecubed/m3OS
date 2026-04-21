//! Arc-based waker and task primitives for the async executor.

#[cfg(not(feature = "std"))]
use alloc::sync::Arc;
#[cfg(feature = "std")]
use std::sync::Arc;

use core::cell::UnsafeCell;
use core::future::Future;
use core::pin::Pin;
#[cfg(not(feature = "std"))]
use core::sync::atomic::AtomicI32;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

// ---------------------------------------------------------------------------
// Self-pipe support (unchanged from Phase 0)
// ---------------------------------------------------------------------------

#[cfg(feature = "std")]
use core::cell::Cell;

#[cfg(feature = "std")]
thread_local! {
    static WAKE_PIPE_FD: Cell<i32> = const { Cell::new(-1) };
}

#[cfg(not(feature = "std"))]
static WAKE_PIPE_FD: AtomicI32 = AtomicI32::new(-1);

/// Set the wake pipe write-end FD.
pub fn set_wake_pipe_fd(fd: i32) {
    #[cfg(feature = "std")]
    WAKE_PIPE_FD.with(|c| c.set(fd));
    #[cfg(not(feature = "std"))]
    WAKE_PIPE_FD.store(fd, Ordering::Relaxed);
}

/// Get the wake pipe write-end FD.
pub fn get_wake_pipe_fd() -> i32 {
    #[cfg(feature = "std")]
    return WAKE_PIPE_FD.with(|c| c.get());
    #[cfg(not(feature = "std"))]
    return WAKE_PIPE_FD.load(Ordering::Relaxed);
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

// ---------------------------------------------------------------------------
// TaskHeader — shared state for a spawned task
// ---------------------------------------------------------------------------

/// Shared header for a task, used by both the executor and wakers.
pub struct TaskHeader {
    /// Whether this task has been woken and needs polling.
    pub woken: AtomicBool,
    /// Whether this task is already present in the executor run queue.
    pub queued: AtomicBool,
    /// Whether this task has completed execution.
    pub completed: AtomicBool,
    /// H9 instrumentation: how many times this task has been woken since
    /// creation. Incremented by every `wake()` / `wake_by_ref()`. Used by
    /// `block_on`'s per-iteration dump to identify the persistent waker.
    pub wake_count: AtomicU64,
    /// Waker registered by a `JoinHandle` awaiting this task's completion.
    pub join_waker: UnsafeCell<Option<Waker>>,
}

// Safety: `join_waker` is only accessed by the single-threaded executor.
// The AtomicBool fields are inherently Send+Sync.
unsafe impl Send for TaskHeader {}
unsafe impl Sync for TaskHeader {}

impl TaskHeader {
    /// Create a new task header, initially woken (so executor polls immediately).
    pub fn new() -> Self {
        Self {
            woken: AtomicBool::new(true),
            queued: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            wake_count: AtomicU64::new(0),
            join_waker: UnsafeCell::new(None),
        }
    }

    /// Check whether this task has been woken.
    pub fn is_woken(&self) -> bool {
        self.woken.load(Ordering::Acquire)
    }

    /// Clear the woken flag (called before polling).
    pub fn clear_woken(&self) {
        self.woken.store(false, Ordering::Release);
    }

    /// Check whether this task is already queued for polling.
    pub fn is_queued(&self) -> bool {
        self.queued.load(Ordering::Acquire)
    }

    /// Mark the task as queued.
    pub fn mark_queued(&self) {
        self.queued.store(true, Ordering::Release);
    }

    /// Clear the queued flag after the executor pops the task.
    pub fn clear_queued(&self) {
        self.queued.store(false, Ordering::Release);
    }

    /// Mark the task as completed and wake any join waker.
    pub fn mark_completed(&self) {
        self.completed.store(true, Ordering::Release);
        // Safety: single-threaded executor — no concurrent access to join_waker.
        let join_waker = unsafe { &mut *self.join_waker.get() };
        if let Some(w) = join_waker.take() {
            w.wake();
        }
    }
}

impl Default for TaskHeader {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Arc-based RawWaker vtable
// ---------------------------------------------------------------------------

const VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, wake_fn, wake_by_ref_fn, drop_fn);

unsafe fn clone_fn(ptr: *const ()) -> RawWaker {
    let arc = unsafe { Arc::from_raw(ptr as *const TaskHeader) };
    let cloned = arc.clone();
    // Don't drop the original — we still hold a reference.
    let _ = Arc::into_raw(arc);
    RawWaker::new(Arc::into_raw(cloned) as *const (), &VTABLE)
}

unsafe fn wake_fn(ptr: *const ()) {
    let arc = unsafe { Arc::from_raw(ptr as *const TaskHeader) };
    arc.wake_count.fetch_add(1, Ordering::Relaxed);
    arc.woken.store(true, Ordering::Release);
    signal_wake_pipe();
    // arc is dropped here, decrementing refcount
}

unsafe fn wake_by_ref_fn(ptr: *const ()) {
    let arc = unsafe { Arc::from_raw(ptr as *const TaskHeader) };
    arc.wake_count.fetch_add(1, Ordering::Relaxed);
    arc.woken.store(true, Ordering::Release);
    signal_wake_pipe();
    // Don't drop — this is wake_by_ref
    let _ = Arc::into_raw(arc);
}

unsafe fn drop_fn(ptr: *const ()) {
    let _ = unsafe { Arc::from_raw(ptr as *const TaskHeader) };
}

/// Build a `Waker` from an `Arc<TaskHeader>`.
pub fn header_waker(header: &Arc<TaskHeader>) -> Waker {
    let ptr = Arc::into_raw(header.clone()) as *const ();
    let raw = RawWaker::new(ptr, &VTABLE);
    // Safety: RawWaker constructed with valid vtable and refcounted pointer.
    unsafe { Waker::from_raw(raw) }
}

// ---------------------------------------------------------------------------
// JoinHandle
// ---------------------------------------------------------------------------

/// Error type returned when joining a task fails.
#[derive(Debug)]
pub struct JoinError;

impl core::fmt::Display for JoinError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "task join failed")
    }
}

/// A handle that can be awaited to get the result of a spawned task.
pub struct JoinHandle<T> {
    /// Shared storage for the task result.
    pub result: Arc<UnsafeCell<Option<T>>>,
    /// Shared task header to check completion and register join waker.
    pub header: Arc<TaskHeader>,
}

// Safety: single-threaded executor — no concurrent access to the UnsafeCell.
unsafe impl<T: Send> Send for JoinHandle<T> {}
unsafe impl<T: Send> Sync for JoinHandle<T> {}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.header.completed.load(Ordering::Acquire) {
            // Safety: single-threaded executor — no concurrent access.
            let result_cell = unsafe { &mut *self.result.get() };
            if let Some(val) = result_cell.take() {
                Poll::Ready(Ok(val))
            } else {
                // Result already taken (double-poll after completion)
                Poll::Ready(Err(JoinError))
            }
        } else {
            // Store the waker so we get notified when the task completes.
            // Safety: single-threaded executor — no concurrent access to join_waker.
            let join_waker = unsafe { &mut *self.header.join_waker.get() };
            *join_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// Create the shared parts for a spawned task: header and result cell.
pub fn create_task_parts<T>() -> (Arc<TaskHeader>, Arc<UnsafeCell<Option<T>>>) {
    (Arc::new(TaskHeader::new()), Arc::new(UnsafeCell::new(None)))
}

// ---------------------------------------------------------------------------
// Legacy compatibility shim (used by executor.rs until Phase 2 rewrite)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

/// A single-threaded async task wrapping a future and its waker state.
///
/// This is a compatibility shim that wraps the new `TaskHeader`.
/// It will be removed when the executor is rewritten in Phase 2.
pub struct Task {
    #[allow(dead_code)]
    pub(crate) future: Pin<Box<dyn Future<Output = ()>>>,
    pub(crate) header: Arc<TaskHeader>,
}

impl Task {
    /// Create a new task wrapping the given future.
    pub fn new(future: Pin<Box<dyn Future<Output = ()>>>) -> Self {
        Self {
            future,
            header: Arc::new(TaskHeader::new()),
        }
    }

    /// Check whether this task has been woken.
    pub fn is_woken(&self) -> bool {
        self.header.is_woken()
    }

    /// Clear the woken flag (called before polling).
    pub fn clear_woken(&self) {
        self.header.clear_woken();
    }
}

/// Build a `Waker` for a `Task`.
pub fn waker_for_task(task: &Task) -> Waker {
    header_waker(&task.header)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Arc waker tests ---

    #[test]
    fn test_arc_waker_woken_flag() {
        let header = Arc::new(TaskHeader::new());
        header.clear_woken();
        assert!(!header.is_woken());

        let waker = header_waker(&header);
        waker.wake_by_ref();
        assert!(header.is_woken());
    }

    #[test]
    fn test_arc_waker_clone_wake() {
        let header = Arc::new(TaskHeader::new());
        header.clear_woken();

        let waker = header_waker(&header);
        let cloned = waker.clone();
        drop(waker);
        cloned.wake_by_ref();
        assert!(header.is_woken());
    }

    #[test]
    fn test_arc_waker_clone_drop() {
        let header = Arc::new(TaskHeader::new());
        let waker = header_waker(&header);
        let cloned = waker.clone();
        drop(waker);
        drop(cloned);
        // no panic = pass
    }

    #[test]
    fn test_arc_waker_wake_consumes() {
        let header = Arc::new(TaskHeader::new());
        header.clear_woken();
        let waker = header_waker(&header);
        waker.wake(); // consumes the waker
        assert!(header.is_woken());
    }

    #[test]
    fn test_wake_idempotent() {
        let header = Arc::new(TaskHeader::new());
        header.clear_woken();
        let waker = header_waker(&header);
        waker.wake_by_ref();
        waker.wake_by_ref();
        assert!(header.is_woken());
    }

    #[test]
    fn test_self_pipe_wake() {
        let mut fds = [0i32; 2];
        unsafe {
            libc::pipe(fds.as_mut_ptr());
        }
        set_wake_pipe_fd(fds[1]);

        let header = Arc::new(TaskHeader::new());
        header.clear_woken();
        let waker = header_waker(&header);
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

    #[test]
    fn test_wake_no_pipe() {
        set_wake_pipe_fd(-1);
        let header = Arc::new(TaskHeader::new());
        header.clear_woken();
        let waker = header_waker(&header);
        waker.wake();
        assert!(header.is_woken());
    }

    // --- Legacy Task compatibility tests ---

    #[test]
    fn test_task_woken_flag() {
        let task = Task::new(Box::pin(async {}));
        task.clear_woken();
        assert!(!task.is_woken());

        let waker = waker_for_task(&task);
        waker.wake_by_ref();
        assert!(task.is_woken());
    }

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

    // --- JoinHandle tests ---

    #[test]
    fn test_join_handle_pending_when_not_complete() {
        let (header, result) = create_task_parts::<i32>();
        let mut handle = JoinHandle { result, header };

        // Poll should return Pending since task is not complete.
        let waker = Waker::noop();
        let mut cx = Context::from_waker(&waker);
        let poll = Pin::new(&mut handle).poll(&mut cx);
        assert!(poll.is_pending());
    }

    #[test]
    fn test_join_handle_resolves_on_completion() {
        let (header, result) = create_task_parts::<i32>();
        let mut handle = JoinHandle {
            result: result.clone(),
            header: header.clone(),
        };

        // Simulate task completing and storing result.
        // Safety: single-threaded test.
        unsafe {
            *result.get() = Some(42);
        }
        header.mark_completed();

        let waker = Waker::noop();
        let mut cx = Context::from_waker(&waker);
        let poll = Pin::new(&mut handle).poll(&mut cx);
        match poll {
            Poll::Ready(Ok(val)) => assert_eq!(val, 42),
            other => panic!("expected Ready(Ok(42)), got {:?}", other),
        }
    }

    #[test]
    fn test_join_handle_wakes_on_completion() {
        use std::sync::atomic::AtomicBool;

        let (header, result) = create_task_parts::<i32>();
        let mut handle = JoinHandle {
            result: result.clone(),
            header: header.clone(),
        };

        // First poll: should be Pending and register waker.
        // Use an Arc<AtomicBool> to track if our waker was called.
        let woken = Arc::new(AtomicBool::new(false));
        let woken2 = woken.clone();

        // Build a simple waker that sets our flag.
        let raw_waker = {
            let ptr = Arc::into_raw(woken2) as *const ();
            unsafe fn clone_fn(ptr: *const ()) -> RawWaker {
                let arc = unsafe { Arc::from_raw(ptr as *const AtomicBool) };
                let cloned = arc.clone();
                let _ = Arc::into_raw(arc);
                RawWaker::new(Arc::into_raw(cloned) as *const (), &TEST_VTABLE)
            }
            unsafe fn wake_fn(ptr: *const ()) {
                let arc = unsafe { Arc::from_raw(ptr as *const AtomicBool) };
                arc.store(true, Ordering::Release);
            }
            unsafe fn wake_by_ref_fn(ptr: *const ()) {
                let arc = unsafe { Arc::from_raw(ptr as *const AtomicBool) };
                arc.store(true, Ordering::Release);
                let _ = Arc::into_raw(arc);
            }
            unsafe fn drop_fn(ptr: *const ()) {
                let _ = unsafe { Arc::from_raw(ptr as *const AtomicBool) };
            }
            static TEST_VTABLE: RawWakerVTable =
                RawWakerVTable::new(clone_fn, wake_fn, wake_by_ref_fn, drop_fn);
            RawWaker::new(ptr, &TEST_VTABLE)
        };
        let test_waker = unsafe { Waker::from_raw(raw_waker) };
        let mut cx = Context::from_waker(&test_waker);

        let poll = Pin::new(&mut handle).poll(&mut cx);
        assert!(poll.is_pending());

        // Now simulate completion — should wake our waker.
        unsafe {
            *result.get() = Some(99);
        }
        header.mark_completed();

        assert!(woken.load(Ordering::Acquire));
    }

    #[test]
    fn test_create_task_parts() {
        let (header, result) = create_task_parts::<String>();
        assert!(header.woken.load(Ordering::Relaxed));
        assert!(!header.completed.load(Ordering::Relaxed));
        // Safety: single-threaded test.
        assert!(unsafe { (*result.get()).is_none() });
    }
}
