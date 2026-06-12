//! `timerfd(2)` objects — Phase 89 Track A.1.
//!
//! A pollable fd that becomes readable when an armed timer expires. libuv's
//! Linux backend (`src/unix/linux.c`) arms a `timerfd` for the next event-loop
//! due-timer and registers it in its epoll set, so `setTimeout`/`setInterval`
//! ride a real `timerfd` rather than only the `epoll_wait` timeout argument.
//!
//! Mirrors `kernel/src/eventfd.rs`: an integer-id table guarded by an
//! `IrqSafeMutex`, a per-object wait queue (index-aligned), and refcounting for
//! `fork`/`dup`/`CLONE_FILES` sharing. The pure expiry/rearm arithmetic and the
//! ns↔tick rounding live in the host-tested `kernel_core::timerfd`; this object
//! owns the kernel state and reads the monotonic tick clock.
//!
//! **Time unit.** All deadlines are stored in absolute scheduler ticks
//! (`TICKS_PER_SEC = 1000`, 1 tick = 1 ms). That is the granularity of the
//! scheduler's `wake_deadline`, which is the only IRQ-safe way to wake a
//! `poll`/`epoll_wait` blocked on a timer expiry — `WaitQueue::wake_all` is
//! task-context-only and must never be called from the timer ISR. So a blocked
//! poller is woken not by this object signaling from the tick, but by the
//! poll/epoll_wait block deadline being **clamped** to the nearest armed
//! timerfd expiry (see `timerfd_next_expiry_tick` and its callers in
//! `arch::x86_64::syscall`). The wait queue here is used for the blocking
//! `read(2)` path and to re-evaluate a poller when `timerfd_settime` re-arms a
//! timer earlier than its previously-clamped deadline.

use crate::task::TaskId;
use crate::task::scheduler::IrqSafeMutex;
use crate::task::wait_queue::WaitQueue;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::AtomicBool;

/// `TFD_CLOEXEC` — same value as `O_CLOEXEC`.
pub const TFD_CLOEXEC: u64 = 0x0008_0000;
/// `TFD_NONBLOCK` — same value as `O_NONBLOCK`.
pub const TFD_NONBLOCK: u64 = 0x0000_0800;
/// All recognized `timerfd_create` flag bits.
pub const TFD_CREATE_KNOWN_FLAGS: u64 = TFD_CLOEXEC | TFD_NONBLOCK;

/// `TFD_TIMER_ABSTIME` — `timerfd_settime` flag: `it_value` is an absolute time
/// on the timer's clock rather than a relative duration.
pub const TFD_TIMER_ABSTIME: u64 = 0x0000_0001;
/// `TFD_TIMER_CANCEL_ON_SET` — accepted and ignored (we have no
/// discontinuous-clock-change cancellation).
pub const TFD_TIMER_CANCEL_ON_SET: u64 = 0x0000_0002;
/// All recognized `timerfd_settime` flag bits.
pub const TFD_SETTIME_KNOWN_FLAGS: u64 = TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET;

/// Clock IDs accepted by `timerfd_create` (Linux ABI). `CLOCK_BOOTTIME` is
/// treated as `CLOCK_MONOTONIC` (no separate suspend-aware clock here).
pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;
pub const CLOCK_BOOTTIME: u32 = 7;

/// Upper bound on concurrently-live timerfd objects.
const TIMERFD_MAX: usize = 128;

struct TimerFdObject {
    /// The clock the timer is interpreted against (only matters for
    /// `TFD_TIMER_ABSTIME` conversion, which the syscall layer resolves before
    /// calling `timerfd_settime`).
    clockid: u32,
    /// Whether the timer is currently armed (an `it_value` of 0 disarms).
    armed: bool,
    /// Absolute scheduler tick of the next fire (valid iff `armed`).
    expiry_tick: u64,
    /// Period in ticks (`0` = one-shot). After a one-shot fires and is `read`,
    /// the timer disarms; after an interval fires and is `read`, `expiry_tick`
    /// advances to the next deadline.
    interval_tick: u64,
    refcount: u32,
}

static TIMERFD_TABLE: IrqSafeMutex<Vec<Option<TimerFdObject>>> = IrqSafeMutex::new(Vec::new());
/// Per-object poll/epoll/blocking-read wait queues (index-aligned with
/// `TIMERFD_TABLE`).
pub static TIMERFD_WAITQUEUES: IrqSafeMutex<Vec<Option<WaitQueue>>> = IrqSafeMutex::new(Vec::new());

/// Current monotonic time in scheduler ticks.
fn now_ticks() -> u64 {
    crate::arch::x86_64::interrupts::tick_count()
}

/// Allocate a new (disarmed) timerfd object for `clockid`. Returns the object
/// id, or `None` if the table is exhausted.
pub fn timerfd_create(clockid: u32) -> Option<usize> {
    let mut table = TIMERFD_TABLE.lock();
    let mut wqs = TIMERFD_WAITQUEUES.lock();
    let id = match table.iter().position(|s| s.is_none()) {
        Some(i) => i,
        None => {
            if table.len() >= TIMERFD_MAX {
                return None;
            }
            table.push(None);
            id_align(&mut wqs, table.len());
            table.len() - 1
        }
    };
    table[id] = Some(TimerFdObject {
        clockid,
        armed: false,
        expiry_tick: 0,
        interval_tick: 0,
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

/// The clock id the timerfd was created with (for `TFD_TIMER_ABSTIME`
/// resolution in the syscall layer). `None` if the id is dead.
pub fn timerfd_clockid(id: usize) -> Option<u32> {
    TIMERFD_TABLE
        .lock()
        .get(id)
        .and_then(|s| s.as_ref())
        .map(|o| o.clockid)
}

/// Arm (or disarm) the timer. `expiry_tick`/`interval_tick` are absolute /
/// period ticks already resolved by the caller (the syscall layer converts the
/// `itimerspec`, including `TFD_TIMER_ABSTIME`, to a monotonic tick deadline).
/// `armed == false` disarms regardless of the tick values.
///
/// Returns the **previous** setting as `(old_remaining_ticks, old_interval_ticks)`
/// for `timerfd_settime`'s `old_value`, or `None` if the id is dead.
///
/// Wakes any poller/reader blocked on this timer so a re-arm to an *earlier*
/// deadline is re-clamped immediately (called from task context — safe).
pub fn timerfd_settime(
    id: usize,
    armed: bool,
    expiry_tick: u64,
    interval_tick: u64,
) -> Option<(u64, u64)> {
    let old = {
        let mut table = TIMERFD_TABLE.lock();
        let obj = table.get_mut(id)?.as_mut()?;
        let now = now_ticks();
        let old_remaining =
            kernel_core::timerfd::remaining(now, obj.armed, obj.expiry_tick, obj.interval_tick);
        let old_interval = obj.interval_tick;
        obj.armed = armed;
        obj.expiry_tick = expiry_tick;
        obj.interval_tick = interval_tick;
        (old_remaining, old_interval)
    };
    wake_timerfd(id);
    Some(old)
}

/// Read the timer's current setting as `(remaining_ticks, interval_ticks)` for
/// `timerfd_gettime`. `None` if the id is dead.
pub fn timerfd_gettime(id: usize) -> Option<(u64, u64)> {
    let table = TIMERFD_TABLE.lock();
    let obj = table.get(id)?.as_ref()?;
    let now = now_ticks();
    let remaining =
        kernel_core::timerfd::remaining(now, obj.armed, obj.expiry_tick, obj.interval_tick);
    Some((remaining, obj.interval_tick))
}

/// `read(2)` the expiration count. Returns `Some(count)` (>= 1) when the timer
/// has fired since the last read — advancing an interval timer to its next
/// deadline and disarming a one-shot — or `None` when it has not yet fired (the
/// caller maps `None` to `EAGAIN` or blocks).
pub fn timerfd_read(id: usize) -> Option<u64> {
    let mut table = TIMERFD_TABLE.lock();
    let obj = table.get_mut(id)?.as_mut()?;
    if !obj.armed {
        return None;
    }
    let now = now_ticks();
    let e = kernel_core::timerfd::expirations(now, obj.expiry_tick, obj.interval_tick)?;
    if obj.interval_tick == 0 {
        // One-shot: fired once, now disarms until the next `settime`.
        obj.armed = false;
    } else {
        // Interval: re-base to the next deadline strictly after `now`.
        obj.expiry_tick = e.next;
    }
    Some(e.count)
}

/// Whether the id still refers to a live object — drives the poll/epoll scan's
/// terminal-event reporting when the object is freed under a blocked waiter.
pub fn timerfd_exists(id: usize) -> bool {
    TIMERFD_TABLE
        .lock()
        .get(id)
        .map(|s| s.is_some())
        .unwrap_or(false)
}

/// Whether a `read` would return data (the timer has fired) — drives `POLLIN`.
pub fn timerfd_readable(id: usize) -> bool {
    let table = TIMERFD_TABLE.lock();
    match table.get(id).and_then(|s| s.as_ref()) {
        Some(obj) if obj.armed => {
            kernel_core::timerfd::expirations(now_ticks(), obj.expiry_tick, obj.interval_tick)
                .is_some()
        }
        _ => false,
    }
}

/// The next absolute-tick deadline of an armed, not-yet-expired timer, for
/// clamping a `poll`/`epoll_wait` block deadline. Returns `None` when the timer
/// is disarmed or has **already** expired (an expired timer is readable, so the
/// poller returns without blocking and no clamp is needed).
pub fn timerfd_next_expiry_tick(id: usize) -> Option<u64> {
    let table = TIMERFD_TABLE.lock();
    let obj = table.get(id).and_then(|s| s.as_ref())?;
    if obj.armed && now_ticks() < obj.expiry_tick {
        Some(obj.expiry_tick)
    } else {
        None
    }
}

/// Wake every task blocked (in `poll`/`epoll_wait`/blocking `read`) on this
/// timerfd. Task-context only.
pub fn wake_timerfd(id: usize) {
    let wqs = TIMERFD_WAITQUEUES.lock();
    if let Some(Some(wq)) = wqs.get(id) {
        wq.wake_all();
    }
}

/// Register `task_id` on the timerfd's wait queue (called from `poll`/`epoll_wait`
/// / blocking `read`). Returns false if the object was freed.
pub fn timerfd_register_waiter(id: usize, task_id: TaskId, woken: &Arc<AtomicBool>) -> bool {
    let wqs = TIMERFD_WAITQUEUES.lock();
    if let Some(Some(wq)) = wqs.get(id) {
        wq.register(task_id, woken);
        true
    } else {
        false
    }
}

/// Deregister `task_id` from the timerfd's wait queue.
pub fn timerfd_deregister_waiter(id: usize, task_id: TaskId) {
    let wqs = TIMERFD_WAITQUEUES.lock();
    if let Some(Some(wq)) = wqs.get(id) {
        wq.deregister(task_id);
    }
}

/// Increment the object refcount (fork/dup of a timerfd fd).
pub fn timerfd_add_ref(id: usize) {
    let mut table = TIMERFD_TABLE.lock();
    if let Some(obj) = table.get_mut(id).and_then(|s| s.as_mut()) {
        obj.refcount = obj.refcount.saturating_add(1);
    }
}

/// Decrement the object refcount; free it (and its wait queue) at zero.
pub fn timerfd_close(id: usize) {
    // Hold `TIMERFD_TABLE` across the wait-queue teardown. Splitting it into two
    // lock acquisitions (free the table slot, drop the lock, re-lock the wait
    // queues) opens a create-reuse window: between the two locks a concurrent
    // `timerfd_create` can grab this just-freed id and install a FRESH wait queue
    // at `wqs[id]`, which this close would then `wake_all` + null out from under
    // the new timerfd — corrupting it and losing its wakeups. Acquiring
    // `TIMERFD_WAITQUEUES` while still holding `TIMERFD_TABLE` keeps the id
    // reserved through the teardown. Lock order TABLE→WAITQUEUES matches
    // `timerfd_create` (the only other dual-lock site), so no deadlock; every
    // WAITQUEUES-only path (`wake_timerfd`/`timerfd_register_waiter`/
    // `timerfd_deregister_waiter` and `WaitQueue::{wake_all,register,deregister}`)
    // never re-locks TABLE. The wake stays before the slot is nulled so a sibling
    // blocked on the last fd is still woken on close.
    let mut table = TIMERFD_TABLE.lock();
    if let Some(obj) = table.get_mut(id).and_then(|s| s.as_mut()) {
        obj.refcount = obj.refcount.saturating_sub(1);
        if obj.refcount == 0 {
            table[id] = None;
            let mut wqs = TIMERFD_WAITQUEUES.lock();
            if let Some(Some(wq)) = wqs.get(id) {
                wq.wake_all();
            }
            if id < wqs.len() {
                wqs[id] = None;
            }
        }
    }
}
