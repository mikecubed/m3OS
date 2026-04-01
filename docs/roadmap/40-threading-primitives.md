# Phase 40 - Threading Primitives

## Milestone Goal

The OS supports kernel-level threads via `clone()` with `CLONE_THREAD` and `CLONE_VM`.
Multiple threads share an address space and can synchronize via `futex()`. Thread-local
storage works via `arch_prctl(ARCH_SET_FS)`. This enables multi-threaded C programs
and improves musl libc compatibility (which uses threads internally even for
single-threaded programs).

## Learning Goals

- Understand the relationship between processes and threads at the kernel level:
  threads are tasks that share an address space.
- Learn how `futex()` enables efficient userspace synchronization (mutex, condvar,
  semaphore) with kernel assistance only on contention.
- See how thread-local storage (TLS) works via the FS segment base register.
- Understand thread lifecycle: creation, join, detach, exit.

## Feature Scope

### `clone()` with Thread Flags

Extend the current `clone()` (which only accepts `SIGCHLD`) to support thread creation:

**Required flags for threads:**

| Flag | Meaning |
|---|---|
| `CLONE_VM` | Share virtual address space (same page table) |
| `CLONE_FS` | Share filesystem info (cwd, umask) |
| `CLONE_FILES` | Share file descriptor table |
| `CLONE_SIGHAND` | Share signal handlers |
| `CLONE_THREAD` | Same thread group (same PID, different TID) |
| `CLONE_PARENT_SETTID` | Write TID to parent's memory |
| `CLONE_CHILD_CLEARTID` | Clear TID and wake futex on exit |
| `CLONE_SETTLS` | Set TLS (FS base) for new thread |

**Thread group model:**
- A thread group shares a PID (the leader's PID).
- Each thread has a unique TID (thread ID).
- `getpid()` returns the group leader's PID.
- `gettid()` returns the thread's own TID.
- Signals can target a thread group (kill PID) or individual thread (tkill TID).

### Kernel Data Structure Changes

```rust
struct Task {
    tid: u64,                          // unique thread ID
    tgid: u64,                         // thread group ID (= leader's TID)
    thread_group: Option<Arc<ThreadGroup>>,  // shared state
    // ... existing fields ...
}

struct ThreadGroup {
    leader_tid: u64,
    page_table_root: PhysAddr,         // shared address space
    fd_table: Arc<Mutex<FdTable>>,     // shared file descriptors
    signal_actions: Arc<Mutex<SignalTable>>,  // shared signal handlers
    members: Mutex<Vec<u64>>,          // TIDs of all threads
}
```

### `futex()` — Full Implementation

The current futex is a stub. Implement the real thing:

**Operations:**
- `FUTEX_WAIT` — if `*uaddr == val`, sleep until woken. Atomic check-and-sleep.
- `FUTEX_WAKE` — wake up to `val` threads waiting on `uaddr`.
- `FUTEX_WAIT_BITSET` — wait with a bitmask (used by musl).
- `FUTEX_WAKE_BITSET` — wake with a bitmask.

**Kernel data structure:**
```rust
// Global hash table of futex wait queues
static FUTEX_TABLE: Mutex<HashMap<(PageTableRoot, VirtAddr), WaitQueue>>;
```

**Key properties:**
- The futex word is in userspace memory — the kernel only intervenes on contention.
- Address is resolved to (address-space, virtual-address) pair for correctness.
- Supports `FUTEX_PRIVATE_FLAG` optimization (no cross-process sharing needed).

### Thread-Local Storage (TLS)

- `arch_prctl(ARCH_SET_FS, addr)` — already implemented, sets FS base.
- `CLONE_SETTLS` — set FS base for new thread during clone.
- Each thread has its own FS base register, saved/restored on context switch.
- musl uses TLS for `errno`, thread-specific data, and internal state.

### `set_tid_address()` — Proper Implementation

Currently a no-op. Implement:
- Store the `clear_child_tid` address in the task struct.
- On thread exit: write 0 to `*clear_child_tid` and call `futex_wake(clear_child_tid, 1)`.
- This is how `pthread_join()` works in musl.

### Thread Exit and Cleanup

When a thread exits (via `exit()` or `exit_group()`):
- `exit()` (syscall 60) — exit only this thread.
- `exit_group()` (syscall 231) — exit all threads in the group.
- On single-thread exit: clean up thread stack, wake joiners via `clear_child_tid`.
- On group exit: send `SIGKILL` to all other threads in the group.

### `gettid()` Syscall

Implement syscall 186 (`gettid`) properly:
- Return the thread's own TID (currently returns PID).

### Signal Delivery to Thread Groups

Update signal handling for thread groups:
- Process-directed signals (e.g., `kill(pid, sig)`) — delivered to any thread in the group.
- Thread-directed signals (e.g., `tkill(tid, sig)`) — delivered to specific thread.
- `SIGSTOP`/`SIGCONT` affect the entire thread group.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 35 (SMP) | Per-core stacks and multi-core dispatch for concurrent threads |
| Phase 33 (Memory) | Slab allocator for efficient thread creation |

## Implementation Outline

1. Add `tid` and `tgid` fields to `Task`; implement `gettid()`.
2. Implement `ThreadGroup` shared state structure.
3. Extend `clone()` to accept `CLONE_VM | CLONE_THREAD | CLONE_FILES | CLONE_SIGHAND`.
4. Allocate per-thread kernel stacks; share page table root with `CLONE_VM`.
5. Implement `CLONE_SETTLS` (set FS base for new thread).
6. Implement `CLONE_CHILD_CLEARTID` and `set_tid_address()`.
7. Implement `FUTEX_WAIT` and `FUTEX_WAKE` with proper wait queues.
8. Implement `FUTEX_WAIT_BITSET` and `FUTEX_WAKE_BITSET`.
9. Update `exit()` for thread-only exit vs `exit_group()` for group exit.
10. Update signal delivery for thread groups.
11. Test with musl's `pthread_create` / `pthread_join`.
12. Stress test: multiple threads incrementing a shared counter via futex mutex.

## Acceptance Criteria

- `clone(CLONE_VM | CLONE_THREAD | ...)` creates a thread sharing the parent's address space.
- `gettid()` returns different values for threads in the same group.
- `getpid()` returns the same value for all threads in a group.
- `futex(FUTEX_WAIT)` sleeps and `futex(FUTEX_WAKE)` wakes the sleeper.
- A musl-linked program using `pthread_create` and `pthread_join` works.
- `pthread_mutex_lock/unlock` works (built on futex).
- Thread-local variables (`__thread` / `_Thread_local`) work.
- `exit_group()` terminates all threads in the group.
- Multiple threads on different cores can run simultaneously.
- All existing tests pass (single-threaded programs unaffected).

## Companion Task List

- Phase 40 Task List — *not yet created*

## How Real OS Implementations Differ

Linux threading:
- **NPTL** (Native POSIX Threads Library) — musl and glibc implement pthreads using
  `clone()` + `futex()`, exactly the mechanism we implement.
- **Robust futexes** — automatically release on thread death.
- **Priority inheritance futexes** — prevent priority inversion.
- **Thread-specific signal masks** via `rt_sigprocmask`.
- **CPU affinity per thread** via `sched_setaffinity`.
- **Thread naming** via `prctl(PR_SET_NAME)`.
- **Thread cgroups** — resource limits per thread group.
- Linux uses `clone3()` (syscall 435) as the modern replacement for `clone()`.

Our implementation provides the essential clone/futex/TLS infrastructure that musl
needs, without robust futexes or priority inheritance.

## Deferred Until Later

- Robust futexes (auto-release on death)
- Priority inheritance (PI) futexes
- clone3() syscall
- Thread naming (prctl PR_SET_NAME)
- Per-thread CPU affinity
- POSIX thread cancellation
- Thread-specific signal masks
- User-level threading (M:N model)
