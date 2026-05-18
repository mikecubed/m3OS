//! Phase 69d follow-up — POSIX advisory `flock(2)`.
//!
//! Two layers:
//!
//! 1. `PerFdLocks` — a side table keyed by `(pid, fd)` carrying the
//!    current lock mode for that fd in that process.  Used as the
//!    behavioural primitive for `LOCK_UN` and for surfacing the
//!    "what does this fd currently hold" question.
//!
//! 2. `UnixSocketLocks` — a `(handle → exclusive_owner)` registry that
//!    makes flock visible **across** file descriptors that point at the
//!    same `UnixSocket` kernel object.  This is what tmux actually
//!    relies on: two `tmux new-session` invocations open the same
//!    socket file and the second `flock(LOCK_EX | LOCK_NB)` must fail
//!    with `EWOULDBLOCK`.
//!
//! Both layers live entirely in kernel state; the syscall layer in
//! `arch/x86_64/syscall/mod.rs` is the only consumer.

extern crate alloc;

use alloc::collections::BTreeMap;
use spin::Mutex;

/// `flock(2)` operation codes.  Linux defines these in
/// `include/uapi/asm-generic/fcntl.h`.
pub const LOCK_SH: i32 = 1;
pub const LOCK_EX: i32 = 2;
pub const LOCK_NB: i32 = 4;
pub const LOCK_UN: i32 = 8;

/// Mode an fd currently holds.  `None` (absence from the per-fd map) is
/// the implicit "unlocked" state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlockMode {
    Shared,
    Exclusive,
}

// ===========================================================================
// Per-(pid, fd) lock state — covers all FD backends.
// ===========================================================================

static PER_FD: Mutex<BTreeMap<(u32, u32), FlockMode>> = Mutex::new(BTreeMap::new());

/// Set or clear the per-fd lock state.  Passing `None` removes the entry.
pub fn set_per_fd(pid: u32, fd: u32, mode: Option<FlockMode>) {
    let mut t = PER_FD.lock();
    match mode {
        Some(m) => {
            t.insert((pid, fd), m);
        }
        None => {
            t.remove(&(pid, fd));
        }
    }
}

/// Read the current per-fd lock state.
pub fn get_per_fd(pid: u32, fd: u32) -> Option<FlockMode> {
    PER_FD.lock().get(&(pid, fd)).copied()
}

/// Drop any per-fd state for the given (pid, fd).  Called from `close(2)`
/// so a closed fd doesn't leak its lock entry.
pub fn release_per_fd(pid: u32, fd: u32) {
    PER_FD.lock().remove(&(pid, fd));
}

/// Drop every per-fd entry for a process — called during process exit
/// so PID-recycle can't surface a stale lock.
pub fn release_all_for_pid(pid: u32) {
    let mut t = PER_FD.lock();
    t.retain(|&(p, _), _| p != pid);
}

// ===========================================================================
// Cross-fd lock state for UnixSocket kernel objects.
//
// tmux's client/server coordination relies on this: open the socket file,
// `flock(LOCK_EX | LOCK_NB)`, and treat failure as "another server is
// already running".  We model an `Exclusive` holder as a single
// `(pid, fd)` tuple and a `Shared` holder set as a small vector — that
// vector is intentionally not bounded because shared-lock contention is
// not a realistic load on m3OS.
// ===========================================================================

#[derive(Debug, Clone, Default)]
struct UnixSocketLockState {
    exclusive: Option<(u32, u32)>,
    shared: alloc::vec::Vec<(u32, u32)>,
}

static UNIX_SOCKET_LOCKS: Mutex<BTreeMap<usize, UnixSocketLockState>> = Mutex::new(BTreeMap::new());

/// Result of attempting to acquire a flock on a `UnixSocket` handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlockOutcome {
    /// Lock acquired (or re-acquired — flock is idempotent on the same fd).
    Acquired,
    /// Lock could not be acquired immediately and `LOCK_NB` was set.
    WouldBlock,
}

/// Attempt to take an exclusive lock on a Unix socket handle for the
/// given `(pid, fd)`.  Idempotent: re-locking by the same `(pid, fd)`
/// always returns `Acquired`.
pub fn unix_socket_acquire_exclusive(handle: usize, pid: u32, fd: u32) -> FlockOutcome {
    let mut t = UNIX_SOCKET_LOCKS.lock();
    let state = t.entry(handle).or_default();
    if let Some(holder) = state.exclusive {
        if holder == (pid, fd) {
            return FlockOutcome::Acquired;
        }
        return FlockOutcome::WouldBlock;
    }
    if !state.shared.is_empty() && state.shared.iter().any(|&h| h != (pid, fd)) {
        return FlockOutcome::WouldBlock;
    }
    state.shared.clear();
    state.exclusive = Some((pid, fd));
    FlockOutcome::Acquired
}

/// Attempt to take a shared lock on a Unix socket handle.
pub fn unix_socket_acquire_shared(handle: usize, pid: u32, fd: u32) -> FlockOutcome {
    let mut t = UNIX_SOCKET_LOCKS.lock();
    let state = t.entry(handle).or_default();
    if let Some(holder) = state.exclusive {
        if holder == (pid, fd) {
            // Downgrade — caller already owns exclusive on this fd.
            state.exclusive = None;
            state.shared.push((pid, fd));
            return FlockOutcome::Acquired;
        }
        return FlockOutcome::WouldBlock;
    }
    if !state.shared.contains(&(pid, fd)) {
        state.shared.push((pid, fd));
    }
    FlockOutcome::Acquired
}

/// Drop any lock the given `(pid, fd)` holds on a Unix socket handle.
pub fn unix_socket_release(handle: usize, pid: u32, fd: u32) {
    let mut t = UNIX_SOCKET_LOCKS.lock();
    if let Some(state) = t.get_mut(&handle) {
        if state.exclusive == Some((pid, fd)) {
            state.exclusive = None;
        }
        state.shared.retain(|&h| h != (pid, fd));
        if state.exclusive.is_none() && state.shared.is_empty() {
            t.remove(&handle);
        }
    }
}

/// Forget every lock on a given Unix socket handle — called when the
/// last reference to a `UnixSocket` is freed so stale entries don't pile
/// up.
pub fn unix_socket_purge(handle: usize) {
    UNIX_SOCKET_LOCKS.lock().remove(&handle);
}

/// Release every Unix-socket flock entry owned by `pid` across every
/// handle in the registry.  Called from `do_full_process_exit` so a
/// process that exits without explicitly closing a locked Unix socket
/// fd cannot leave its `(pid, fd)` exclusive holder behind — the
/// socket object may still be alive (another process or an inflight
/// SCM_RIGHTS copy holds a refcount), in which case `unix_socket_purge`
/// is never called and the dead owner would otherwise block future
/// lockers indefinitely (PR #177 review fix).
pub fn release_unix_socket_locks_for_pid(pid: u32) {
    let mut t = UNIX_SOCKET_LOCKS.lock();
    t.retain(|_handle, state| {
        if state.exclusive.map(|(p, _)| p) == Some(pid) {
            state.exclusive = None;
        }
        state.shared.retain(|&(p, _)| p != pid);
        state.exclusive.is_some() || !state.shared.is_empty()
    });
}

/// Release every Unix-socket flock entry keyed on a specific handle for
/// any pid in `pids`.  Called from the close path when the closing
/// thread is a CLONE_FILES sibling of the original lock holder: the
/// shared fd table is about to lose the slot, so any sibling's
/// `(member_pid, fd)` entry is now stale (PR #177 review fix).
pub fn unix_socket_release_for_pids(handle: usize, pids: &[u32], fd: u32) {
    let mut t = UNIX_SOCKET_LOCKS.lock();
    if let Some(state) = t.get_mut(&handle) {
        if let Some((p, f)) = state.exclusive
            && f == fd
            && pids.contains(&p)
        {
            state.exclusive = None;
        }
        state
            .shared
            .retain(|&(p, f)| !(f == fd && pids.contains(&p)));
        if state.exclusive.is_none() && state.shared.is_empty() {
            t.remove(&handle);
        }
    }
}

/// Release every per-fd flock entry for `fd` across any pid in `pids`.
/// Companion to [`unix_socket_release_for_pids`] for backends that have
/// no cross-fd lock state (PR #177 review fix).
pub fn release_per_fd_for_pids(pids: &[u32], fd: u32) {
    let mut t = PER_FD.lock();
    t.retain(|&(p, f), _| !(f == fd && pids.contains(&p)));
}

// ===========================================================================
// Tests (host-only, exercised via `cargo test -p kernel` is not feasible
// because the kernel is no_std/QEMU-only.  The state machine is small
// enough to validate by inspection; the integration coverage comes from
// the userspace `sendmsg-test` regression and from the tmux smoke flow).
// ===========================================================================
