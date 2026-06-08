//! `eventfd(2)` objects — Phase 86d Track D.
//!
//! A minimal, Linux-compatible eventfd: an 8-byte counter plus a wait queue.
//! Go 1.21+'s runtime creates an eventfd (`EFD_CLOEXEC | EFD_NONBLOCK`) as its
//! cross-thread M-wakeup primitive (`netpollBreak`): write 8 bytes to signal,
//! read 8 bytes to drain, and `epoll` for readability. Go's eventfd is
//! non-blocking (an empty read returns `EAGAIN`), so for Go the hot path is the
//! `epoll`/`poll` wake path that resumes a blocked `epoll_wait` when another
//! thread writes. A *blocking* eventfd read (no `EFD_NONBLOCK`) is also
//! supported: the reader parks on the object's wait queue until a write makes
//! the counter non-zero (see the `FdBackend::EventFd` arm in `sys_linux_read`),
//! mirroring the pipe read pattern. Writes never block here — Go's
//! single-increment writes never approach the counter cap.
//!
//! The object is keyed by an integer id stored in `FdBackend::EventFd`, shared
//! across the threads of a process via the shared fd table (`CLONE_FILES`),
//! exactly like the pipe table.

use crate::task::TaskId;
use crate::task::scheduler::IrqSafeMutex;
use crate::task::wait_queue::WaitQueue;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;

/// `EFD_SEMAPHORE` — read returns 1 and decrements (rather than draining the
/// whole counter).
pub const EFD_SEMAPHORE: u64 = 0x0000_0001;
/// `EFD_CLOEXEC` — same value as `O_CLOEXEC`.
pub const EFD_CLOEXEC: u64 = 0x0008_0000;
/// `EFD_NONBLOCK` — same value as `O_NONBLOCK`.
pub const EFD_NONBLOCK: u64 = 0x0000_0800;
/// All recognized flag bits (used to reject unknown flags with EINVAL).
pub const EFD_KNOWN_FLAGS: u64 = EFD_SEMAPHORE | EFD_CLOEXEC | EFD_NONBLOCK;

/// Linux caps the eventfd counter at `2^64 - 2`; a write that would reach
/// `2^64 - 1` blocks (or `EAGAIN`s for non-blocking fds). Sourced from the
/// host-tested `kernel_core::eventfd` so the cap can't drift from the logic.
const EVENTFD_COUNTER_MAX: u64 = kernel_core::eventfd::EVENTFD_COUNTER_MAX;
/// Upper bound on concurrently-live eventfd objects.
const EVENTFD_MAX: usize = 128;

struct EventFdObject {
    counter: u64,
    semaphore: bool,
    refcount: u32,
}

static EVENTFD_TABLE: IrqSafeMutex<Vec<Option<EventFdObject>>> = IrqSafeMutex::new(Vec::new());
/// Per-object epoll/poll wait queues (index-aligned with `EVENTFD_TABLE`).
pub static EVENTFD_WAITQUEUES: IrqSafeMutex<Vec<Option<WaitQueue>>> = IrqSafeMutex::new(Vec::new());

/// Outcome of a non-blocking `eventfd_write`.
pub enum EventFdWriteErr {
    /// The id does not refer to a live eventfd object.
    BadFd,
    /// The value `0xffff_ffff_ffff_ffff` is rejected (`EINVAL`).
    Invalid,
    /// The add would overflow the counter cap (`EAGAIN` for non-blocking).
    WouldBlock,
}

/// Allocate a new eventfd object initialized to `initval`. Returns the object
/// id, or `None` if the table is exhausted.
pub fn eventfd_create(initval: u64, semaphore: bool) -> Option<usize> {
    let mut table = EVENTFD_TABLE.lock();
    let mut wqs = EVENTFD_WAITQUEUES.lock();
    let id = match table.iter().position(|s| s.is_none()) {
        Some(i) => i,
        None => {
            if table.len() >= EVENTFD_MAX {
                return None;
            }
            table.push(None);
            id_align(&mut wqs, table.len());
            table.len() - 1
        }
    };
    table[id] = Some(EventFdObject {
        counter: initval,
        semaphore,
        refcount: 1,
    });
    id_align(&mut wqs, id + 1);
    wqs[id] = Some(WaitQueue::new());
    Some(id)
}

/// Grow `wqs` so index `len-1` is valid.
fn id_align(wqs: &mut Vec<Option<WaitQueue>>, len: usize) {
    while wqs.len() < len {
        wqs.push(None);
    }
}

/// Read-drain the counter. Returns `Some(value)` (draining), or `None` when the
/// counter is 0 — the caller maps `None` to `EAGAIN` (Go's eventfd is
/// non-blocking). With `EFD_SEMAPHORE`, returns 1 and decrements; otherwise
/// returns the whole counter and resets it to 0.
pub fn eventfd_read(id: usize) -> Option<u64> {
    use kernel_core::eventfd::ReadOutcome;
    let mut table = EVENTFD_TABLE.lock();
    let obj = table.get_mut(id)?.as_mut()?;
    match kernel_core::eventfd::read_outcome(obj.counter, obj.semaphore) {
        ReadOutcome::Empty => None,
        ReadOutcome::Value(val, new_counter) => {
            obj.counter = new_counter;
            Some(val)
        }
    }
}

/// Add `val` to the counter and wake any `epoll`/`poll` waiters. The counter is
/// mutated under the table lock; the wake happens after the lock is released.
pub fn eventfd_write(id: usize, val: u64) -> Result<(), EventFdWriteErr> {
    use kernel_core::eventfd::WriteOutcome;
    {
        let mut table = EVENTFD_TABLE.lock();
        let obj = match table.get_mut(id).and_then(|s| s.as_mut()) {
            Some(o) => o,
            None => return Err(EventFdWriteErr::BadFd),
        };
        match kernel_core::eventfd::write_outcome(obj.counter, val) {
            WriteOutcome::Ok(sum) => obj.counter = sum,
            WriteOutcome::Invalid => return Err(EventFdWriteErr::Invalid),
            WriteOutcome::WouldBlock => return Err(EventFdWriteErr::WouldBlock),
        }
    }
    wake_eventfd(id);
    Ok(())
}

/// Whether a read would return data (counter > 0) — drives `POLLIN`.
pub fn eventfd_readable(id: usize) -> bool {
    EVENTFD_TABLE
        .lock()
        .get(id)
        .and_then(|s| s.as_ref())
        .map(|o| o.counter > 0)
        .unwrap_or(false)
}

/// Whether a write would succeed (counter < cap) — drives `POLLOUT`. Always
/// true in practice for Go's single-increment writes.
pub fn eventfd_writable(id: usize) -> bool {
    EVENTFD_TABLE
        .lock()
        .get(id)
        .and_then(|s| s.as_ref())
        .map(|o| o.counter < EVENTFD_COUNTER_MAX)
        .unwrap_or(false)
}

/// Wake every task blocked (in `epoll`/`poll`) on this eventfd.
pub fn wake_eventfd(id: usize) {
    let wqs = EVENTFD_WAITQUEUES.lock();
    if let Some(Some(wq)) = wqs.get(id) {
        wq.wake_all();
    }
}

/// Register `task_id` on the eventfd's wait queue (called from `epoll_wait`).
pub fn eventfd_register_waiter(id: usize, task_id: TaskId, woken: &Arc<AtomicBool>) -> bool {
    let wqs = EVENTFD_WAITQUEUES.lock();
    if let Some(Some(wq)) = wqs.get(id) {
        wq.register(task_id, woken);
        true
    } else {
        false
    }
}

/// Deregister `task_id` from the eventfd's wait queue.
pub fn eventfd_deregister_waiter(id: usize, task_id: TaskId) {
    let wqs = EVENTFD_WAITQUEUES.lock();
    if let Some(Some(wq)) = wqs.get(id) {
        wq.deregister(task_id);
    }
}

/// Increment the object refcount (fork/dup of an eventfd fd).
pub fn eventfd_add_ref(id: usize) {
    let mut table = EVENTFD_TABLE.lock();
    if let Some(obj) = table.get_mut(id).and_then(|s| s.as_mut()) {
        obj.refcount = obj.refcount.saturating_add(1);
    }
}

/// Decrement the object refcount; free it (and its wait queue) at zero.
pub fn eventfd_close(id: usize) {
    let mut table = EVENTFD_TABLE.lock();
    if let Some(obj) = table.get_mut(id).and_then(|s| s.as_mut()) {
        obj.refcount = obj.refcount.saturating_sub(1);
        if obj.refcount == 0 {
            table[id] = None;
            let mut wqs = EVENTFD_WAITQUEUES.lock();
            if id < wqs.len() {
                wqs[id] = None;
            }
        }
    }
}
