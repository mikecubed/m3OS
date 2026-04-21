//! Multi-task cooperative executor with `block_on` and `spawn`.

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;
#[cfg(not(feature = "std"))]
use alloc::collections::VecDeque;
#[cfg(not(feature = "std"))]
use alloc::sync::Arc;

#[cfg(feature = "std")]
use std::collections::VecDeque;
#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(feature = "std")]
use core::cell::Cell;
use core::future::Future;
use core::pin::Pin;
#[cfg(not(feature = "std"))]
use core::sync::atomic::{AtomicPtr, Ordering};
use core::task::{Context, Poll};

use crate::reactor::Reactor;
use crate::slab::Slab;
use crate::task::{TaskHeader, create_task_parts, header_waker};

// Re-export JoinHandle so users can get it from executor or task.
pub use crate::task::JoinHandle;

// ---------------------------------------------------------------------------
// Global executor and reactor pointers
// ---------------------------------------------------------------------------

#[cfg(feature = "std")]
thread_local! {
    static EXECUTOR_PTR: Cell<*mut Executor> = const { Cell::new(core::ptr::null_mut()) };
    static REACTOR_PTR: Cell<*mut Reactor> = const { Cell::new(core::ptr::null_mut()) };
}

#[cfg(not(feature = "std"))]
static EXECUTOR_PTR: AtomicPtr<Executor> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(not(feature = "std"))]
static REACTOR_PTR: AtomicPtr<Reactor> = AtomicPtr::new(core::ptr::null_mut());

fn get_executor_ptr() -> *mut Executor {
    #[cfg(feature = "std")]
    return EXECUTOR_PTR.with(|c| c.get());
    #[cfg(not(feature = "std"))]
    return EXECUTOR_PTR.load(Ordering::Relaxed);
}

fn set_executor_ptr(ptr: *mut Executor) {
    #[cfg(feature = "std")]
    EXECUTOR_PTR.with(|c| c.set(ptr));
    #[cfg(not(feature = "std"))]
    EXECUTOR_PTR.store(ptr, Ordering::Relaxed);
}

fn set_reactor_ptr(ptr: *mut Reactor) {
    #[cfg(feature = "std")]
    REACTOR_PTR.with(|c| c.set(ptr));
    #[cfg(not(feature = "std"))]
    REACTOR_PTR.store(ptr, Ordering::Relaxed);
}

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
    let ptr = REACTOR_PTR.load(Ordering::Relaxed);

    assert!(!ptr.is_null(), "reactor() called outside of block_on");
    unsafe { &mut *ptr }
}

// ---------------------------------------------------------------------------
// Executor internals
// ---------------------------------------------------------------------------

struct TaskSlot {
    future: Pin<Box<dyn Future<Output = ()>>>,
    header: Arc<TaskHeader>,
}

struct Executor {
    tasks: Slab<TaskSlot>,
    run_queue: VecDeque<usize>,
    /// Tracks the highest slab index + 1 for scanning.
    high_water: usize,
}

impl Executor {
    fn new() -> Self {
        Self {
            tasks: Slab::new(),
            run_queue: VecDeque::new(),
            high_water: 0,
        }
    }

    fn insert(
        &mut self,
        future: Pin<Box<dyn Future<Output = ()>>>,
        header: Arc<TaskHeader>,
    ) -> usize {
        let id = self.tasks.insert(TaskSlot { future, header });
        if id >= self.high_water {
            self.high_water = id + 1;
        }
        self.tasks
            .get_mut(id)
            .expect("executor insert returned invalid task id")
            .header
            .mark_queued();
        self.run_queue.push_back(id);
        h9_log_spawn(id);
        id
    }

    /// Poll and remove completed spawned tasks from the run queue.
    fn poll_spawned_tasks(&mut self) {
        let mut to_poll = core::mem::take(&mut self.run_queue);

        for task_id in to_poll.drain(..) {
            let slot = match self.tasks.get_mut(task_id) {
                Some(s) => s as *mut TaskSlot,
                None => continue,
            };

            // Safety: single-threaded executor, exclusive access.
            let slot_ref = unsafe { &mut *slot };
            slot_ref.header.clear_queued();

            if !slot_ref.header.is_woken() {
                continue;
            }
            slot_ref.header.clear_woken();

            let waker = header_waker(&slot_ref.header);
            let mut cx = Context::from_waker(&waker);

            match slot_ref.future.as_mut().poll(&mut cx) {
                Poll::Ready(()) => {
                    self.tasks.remove(task_id);
                }
                Poll::Pending => {}
            }
        }

        // Preserve tasks spawned while polling this batch. `spawn()` pushes
        // directly into `self.run_queue`, so replacing it here would drop
        // those freshly spawned tasks before their first poll.
        if !to_poll.is_empty() {
            self.run_queue.append(&mut to_poll);
        }
    }

    /// Re-scan all tasks for woken state and add to run queue.
    fn requeue_woken(&mut self) {
        for i in 0..self.high_water {
            if let Some(slot) = self.tasks.get(i) {
                if slot.header.is_woken() && !slot.header.is_queued() {
                    slot.header.mark_queued();
                    self.run_queue.push_back(i);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// H9 instrumentation: low-volume `block_on` heartbeat
// ---------------------------------------------------------------------------

/// Emit a single line to fd=1 describing executor state at this tick.
///
/// Format: `[h9-block-on] iter=N pid=P run_queue=L root_woken=B\n`
/// Used to distinguish "executor spinning" from "executor stalled" when
/// paired with kernel-side dispatch counters during the SSH wedge
/// investigation.
#[cfg(not(feature = "std"))]
fn h9_log_block_on_tick(iter: u64, run_queue_len: usize, root_woken: bool) {
    use syscall_lib::{getpid, write_str, write_u64};
    write_str(1, "[h9-block-on] iter=");
    write_u64(1, iter);
    write_str(1, " pid=");
    write_u64(1, getpid() as u64);
    write_str(1, " run_queue=");
    write_u64(1, run_queue_len as u64);
    write_str(1, " root_woken=");
    write_str(1, if root_woken { "1" } else { "0" });
    write_str(1, "\n");
}

#[cfg(feature = "std")]
fn h9_log_block_on_tick(_iter: u64, _run_queue_len: usize, _root_woken: bool) {
    // Std builds (host tests) intentionally silent; the wedge only repros in
    // the no_std target.
}

/// Emit one line per occupied slab slot describing its waker state.
///
/// Format: `[h9-tasks] iter=N pid=P slot=S woken=B queued=B wake_count=W\n`
/// Pairs with `h9_log_block_on_tick` so a session-child wedge can be
/// attributed to the specific spawned future that keeps the run queue
/// non-empty.
#[cfg(not(feature = "std"))]
fn h9_log_executor_tasks(iter: u64, executor: &Executor) {
    use core::sync::atomic::Ordering;
    use syscall_lib::{getpid, write_str, write_u64};
    let pid = getpid() as u64;
    for slot_id in 0..executor.high_water {
        let Some(slot) = executor.tasks.get(slot_id) else {
            continue;
        };
        let woken = slot.header.is_woken();
        let queued = slot.header.is_queued();
        let wake_count = slot.header.wake_count.load(Ordering::Relaxed);
        write_str(1, "[h9-tasks] iter=");
        write_u64(1, iter);
        write_str(1, " pid=");
        write_u64(1, pid);
        write_str(1, " slot=");
        write_u64(1, slot_id as u64);
        write_str(1, " woken=");
        write_str(1, if woken { "1" } else { "0" });
        write_str(1, " queued=");
        write_str(1, if queued { "1" } else { "0" });
        write_str(1, " wake_count=");
        write_u64(1, wake_count);
        write_str(1, "\n");
    }
}

#[cfg(feature = "std")]
fn h9_log_executor_tasks(_iter: u64, _executor: &Executor) {}

/// Emit a single line on `Executor::insert` so spawn order can be matched
/// against existing `sshd:` spawn-site log lines.
///
/// Format: `[h9-spawn] pid=P slot=S\n`
#[cfg(not(feature = "std"))]
fn h9_log_spawn(slot_id: usize) {
    use syscall_lib::{getpid, write_str, write_u64};
    write_str(1, "[h9-spawn] pid=");
    write_u64(1, getpid() as u64);
    write_str(1, " slot=");
    write_u64(1, slot_id as u64);
    write_str(1, "\n");
}

#[cfg(feature = "std")]
fn h9_log_spawn(_slot_id: usize) {}

/// Dump global wake-source counters (Notify::signal, Mutex guard-drop) so
/// per-task `wake_count` growth can be attributed to a specific async-rt
/// primitive.
///
/// Format: `[h9-sources] iter=N pid=P notify_fired=A notify_pending=B mutex_handoff=C mutex_drop_idle=D\n`
#[cfg(not(feature = "std"))]
fn h9_log_wake_sources(iter: u64) {
    use crate::sync::mutex::{MUTEX_DROP_NO_WAITER, MUTEX_HANDOFF_WAKES};
    use crate::sync::notify::{NOTIFY_SIGNAL_FIRED, NOTIFY_SIGNAL_PENDING};
    use core::sync::atomic::Ordering;
    use syscall_lib::{getpid, write_str, write_u64};
    write_str(1, "[h9-sources] iter=");
    write_u64(1, iter);
    write_str(1, " pid=");
    write_u64(1, getpid() as u64);
    write_str(1, " notify_fired=");
    write_u64(1, NOTIFY_SIGNAL_FIRED.load(Ordering::Relaxed));
    write_str(1, " notify_pending=");
    write_u64(1, NOTIFY_SIGNAL_PENDING.load(Ordering::Relaxed));
    write_str(1, " mutex_handoff=");
    write_u64(1, MUTEX_HANDOFF_WAKES.load(Ordering::Relaxed));
    write_str(1, " mutex_drop_idle=");
    write_u64(1, MUTEX_DROP_NO_WAITER.load(Ordering::Relaxed));
    write_str(1, "\n");
}

#[cfg(feature = "std")]
fn h9_log_wake_sources(_iter: u64) {}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Drive a future to completion, running all spawned tasks.
///
/// This is the main entry point for the async executor. The root future
/// is kept on the stack (no `'static` bound needed). Spawned tasks live
/// in the slab.
pub fn block_on<F: Future>(reactor: &mut Reactor, future: F) -> F::Output {
    let prev_executor = get_executor_ptr();
    let prev_reactor = {
        #[cfg(feature = "std")]
        {
            REACTOR_PTR.with(|c| c.get())
        }
        #[cfg(not(feature = "std"))]
        {
            REACTOR_PTR.load(Ordering::Relaxed)
        }
    };

    let mut executor = Executor::new();
    set_executor_ptr(&mut executor as *mut Executor);
    set_reactor_ptr(reactor as *mut Reactor);

    // Root task header — used to build a waker for the root future.
    let root_header = Arc::new(TaskHeader::new());
    let mut future = core::pin::pin!(future);

    // H9 instrumentation: block_on iteration counter. Distinguishes "process
    // is spinning in its executor" from "process not running at all" when
    // paired with kernel-side dispatch counters. Iter <= 10 always logs to
    // confirm warm-up; thereafter every 200th iteration so the steady-state
    // cadence is visible without spam.
    let mut h9_iter: u64 = 0;
    let result = loop {
        h9_iter = h9_iter.wrapping_add(1);
        if h9_iter <= 10 || h9_iter.is_multiple_of(200) {
            h9_log_block_on_tick(h9_iter, executor.run_queue.len(), root_header.is_woken());
            h9_log_executor_tasks(h9_iter, &executor);
            h9_log_wake_sources(h9_iter);
        }

        // 1. Poll spawned tasks first (they may wake the root future)
        executor.poll_spawned_tasks();

        // 2. Poll root future if woken
        if root_header.is_woken() {
            root_header.clear_woken();
            let waker = header_waker(&root_header);
            let mut cx = Context::from_waker(&waker);

            match future.as_mut().poll(&mut cx) {
                Poll::Ready(val) => {
                    // Give remaining ready spawned tasks one last chance to run.
                    executor.requeue_woken();
                    executor.poll_spawned_tasks();
                    break val;
                }
                Poll::Pending => {}
            }
        }

        // 3. Always do a non-blocking I/O check so tasks waiting on FD
        //    readiness are not starved by tasks that yield_once (which
        //    immediately re-wake themselves).
        reactor.poll_once(0);
        executor.requeue_woken();

        // 4. If nothing is runnable, block on reactor until an event arrives.
        if executor.run_queue.is_empty() && !root_header.is_woken() {
            reactor.poll_once(100);
            executor.requeue_woken();
        }
    };

    set_executor_ptr(prev_executor);
    set_reactor_ptr(prev_reactor);
    result
}

/// Spawn a new task on the current executor.
///
/// # Panics
/// Panics if called outside of `block_on`.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let executor_ptr = get_executor_ptr();
    assert!(
        !executor_ptr.is_null(),
        "spawn() called outside of block_on"
    );

    let (header, result_cell) = create_task_parts::<F::Output>();
    let result_cell2 = result_cell.clone();
    let header2 = header.clone();

    let adapter = Box::pin(async move {
        let val = future.await;
        // Safety: single-threaded executor — no concurrent access.
        unsafe {
            *result_cell2.get() = Some(val);
        }
        header2.mark_completed();
    });

    // Safety: single-threaded — we are inside block_on's poll loop.
    let executor = unsafe { &mut *executor_ptr };
    executor.insert(adapter, header.clone());

    JoinHandle {
        result: result_cell,
        header,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactor::Reactor;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    // Backward compat: block_on(async { 42 }) returns 42
    #[test]
    fn test_block_on_immediate() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async { 42 });
        assert_eq!(result, 42);
    }

    // Backward compat: pending-then-ready future
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

    // Reactor-driven wakeup via pipe (backward compat)
    #[test]
    fn test_block_on_reactor_driven() {
        let mut reactor = Reactor::new();

        let (read_fd, write_fd) = {
            let mut fds = [0i32; 2];
            unsafe { libc::pipe(fds.as_mut_ptr()) };
            (fds[0], fds[1])
        };

        struct AwaitPipe {
            fd: i32,
            registered: Cell<bool>,
        }

        impl Future for AwaitPipe {
            type Output = ();
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.registered.get() {
                    Poll::Ready(())
                } else {
                    self.registered.set(true);
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

    // T007.1: spawn 3 tasks returning different values, await all JoinHandles
    #[test]
    fn test_spawn_and_join() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let h1 = spawn(async { 10 });
            let h2 = spawn(async { 20 });
            let h3 = spawn(async { 30 });
            let v1 = h1.await.unwrap();
            let v2 = h2.await.unwrap();
            let v3 = h3.await.unwrap();
            (v1, v2, v3)
        });
        assert_eq!(result, (10, 20, 30));
    }

    // T007.2: spawn a task that itself spawns another task, await both
    #[test]
    fn test_nested_spawn() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let outer = spawn(async {
                let inner = spawn(async { 7 });
                inner.await.unwrap() + 1
            });
            outer.await.unwrap()
        });
        assert_eq!(result, 8);
    }

    // T007.3: spawn 10 tasks, each returning their index
    #[test]
    fn test_spawn_many() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let mut handles = Vec::new();
            for i in 0..10 {
                handles.push(spawn(async move { i }));
            }
            let mut values = Vec::new();
            for h in handles {
                values.push(h.await.unwrap());
            }
            values
        });
        assert_eq!(result, (0..10).collect::<Vec<_>>());
    }

    // T007.5: block_on future that spawns tasks and awaits them
    #[test]
    fn test_block_on_with_spawn() {
        let mut reactor = Reactor::new();
        let result = block_on(&mut reactor, async {
            let a = spawn(async { 100 });
            let b = spawn(async { 200 });
            a.await.unwrap() + b.await.unwrap()
        });
        assert_eq!(result, 300);
    }

    // T007.6: spawn a task that waits on a pipe, verify executor blocks
    #[test]
    fn test_no_busy_spin() {
        let mut reactor = Reactor::new();

        let result = block_on(&mut reactor, async {
            let (read_fd, write_fd) = {
                let mut fds = [0i32; 2];
                unsafe { libc::pipe(fds.as_mut_ptr()) };
                (fds[0], fds[1])
            };

            let handle = spawn(async move {
                use crate::io::AsyncFd;
                let async_fd = AsyncFd::new(read_fd);
                async_fd.readable().await;
                let mut buf = [0u8; 1];
                unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut _, 1) };
                buf[0]
            });

            // Writer thread: write after 50ms
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(50));
                unsafe { libc::write(write_fd, [42u8].as_ptr() as *const _, 1) };
            });

            let start = Instant::now();
            let val = handle.await.unwrap();
            let elapsed = start.elapsed();

            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }

            assert!(
                elapsed >= Duration::from_millis(30),
                "elapsed too short: {:?}",
                elapsed,
            );
            val
        });
        assert_eq!(result, 42);
    }

    // T007.7: spawn two tasks each waiting on different pipes
    #[test]
    fn test_spawn_with_reactor_io() {
        let mut reactor = Reactor::new();

        let result = block_on(&mut reactor, async {
            let (r1, w1) = {
                let mut fds = [0i32; 2];
                unsafe { libc::pipe(fds.as_mut_ptr()) };
                (fds[0], fds[1])
            };
            let (r2, w2) = {
                let mut fds = [0i32; 2];
                unsafe { libc::pipe(fds.as_mut_ptr()) };
                (fds[0], fds[1])
            };

            let h1 = spawn(async move {
                use crate::io::AsyncFd;
                let fd = AsyncFd::new(r1);
                fd.readable().await;
                let mut buf = [0u8; 1];
                unsafe { libc::read(r1, buf.as_mut_ptr() as *mut _, 1) };
                buf[0]
            });

            let h2 = spawn(async move {
                use crate::io::AsyncFd;
                let fd = AsyncFd::new(r2);
                fd.readable().await;
                let mut buf = [0u8; 1];
                unsafe { libc::read(r2, buf.as_mut_ptr() as *mut _, 1) };
                buf[0]
            });

            // Write to both pipes from threads
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                unsafe { libc::write(w1, [10u8].as_ptr() as *const _, 1) };
            });
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                unsafe { libc::write(w2, [20u8].as_ptr() as *const _, 1) };
            });

            let v1 = h1.await.unwrap();
            let v2 = h2.await.unwrap();

            unsafe {
                libc::close(r1);
                libc::close(r2);
            }

            (v1, v2)
        });
        assert_eq!(result, (10, 20));
    }

    // T007.8: spawn a task, drop the JoinHandle, verify block_on completes
    #[test]
    fn test_detached_task() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = ran.clone();

        let mut reactor = Reactor::new();
        block_on(&mut reactor, async move {
            let handle = spawn(async move {
                ran2.store(true, Ordering::Release);
                42
            });
            // Drop the handle without awaiting
            drop(handle);
        });

        // The spawned task should have run (it was ready immediately).
        assert!(ran.load(Ordering::Acquire));
    }

    #[test]
    fn test_requeue_woken_does_not_duplicate_run_queue_entries() {
        let mut executor = Executor::new();
        let (header, _result) = create_task_parts::<()>();
        let future = Box::pin(async {});
        let id = executor.insert(future, header.clone());

        assert_eq!(executor.run_queue.len(), 1);
        assert!(header.is_queued());

        // Requeueing a still-woken task should not add duplicates.
        executor.requeue_woken();
        executor.requeue_woken();
        assert_eq!(executor.run_queue.len(), 1);

        // Once the task is popped, it can be requeued again.
        let _ = executor.run_queue.pop_front();
        header.clear_queued();
        executor.requeue_woken();
        assert_eq!(executor.run_queue.len(), 1);
        assert_eq!(executor.run_queue.front().copied(), Some(id));
    }

    #[test]
    fn test_spawn_during_poll_is_not_dropped() {
        use core::future::poll_fn;
        use std::sync::atomic::{AtomicBool, Ordering};

        let ran = Arc::new(AtomicBool::new(false));
        let ran2 = ran.clone();

        let mut reactor = Reactor::new();
        block_on(&mut reactor, async move {
            let parent = spawn(async move {
                spawn(async move {
                    ran2.store(true, Ordering::Release);
                });

                // Return Pending once so the executor completes another pass
                // after the nested spawn.
                poll_fn(|cx| {
                    cx.waker().wake_by_ref();
                    Poll::<()>::Pending
                })
                .await;
            });

            drop(parent);

            // Keep the root future alive for another executor turn.
            poll_fn(|cx| {
                if ran.load(Ordering::Acquire) {
                    Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
        });

        assert!(ran.load(Ordering::Acquire));
    }
}
