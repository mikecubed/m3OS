# Phase 40 - Threading Primitives

**Status:** Planned
**Source Ref:** phase-40
**Depends on:** Phase 25 (SMP) ✅, Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP) ✅
**Builds on:** Extends the single-threaded process model from Phase 11 with kernel-level threads that share an address space, and replaces the Phase 39 futex stub with a real wait/wake implementation
**Primary Components:** kernel/src/process/mod.rs, kernel/src/task/mod.rs, kernel/src/arch/x86_64/syscall.rs, kernel/src/net/futex.rs

## Milestone Goal

The OS supports kernel-level threads via `clone()` with `CLONE_THREAD` and `CLONE_VM`.
Multiple threads share an address space and can synchronize via `futex()`. Thread-local
storage works via `arch_prctl(ARCH_SET_FS)`. This enables multi-threaded C programs
and improves musl libc compatibility (which uses threads internally even for
single-threaded programs).

## Why This Phase Exists

The current process model is strictly single-threaded: each process has one kernel
task, one address space, and one set of file descriptors. This prevents running any
multi-threaded program, including musl-linked programs that use `pthread_create()`.
Even single-threaded musl programs call `set_tid_address()` and use futex-based locks
internally, so the existing stubs (futex always "succeeds", `set_tid_address` is a
no-op, `gettid` returns PID) limit libc compatibility. Threading is a prerequisite
for later phases that need concurrent execution within a single address space.

## Learning Goals

- Understand the relationship between processes and threads at the kernel level:
  threads are tasks that share an address space.
- Learn how `futex()` enables efficient userspace synchronization (mutex, condvar,
  semaphore) with kernel assistance only on contention.
- See how thread-local storage (TLS) works via the FS segment base register.
- Understand thread lifecycle: creation, join, detach, exit.

## Feature Scope

### `clone()` with Thread Flags

Extend the current `clone()` (which only accepts `SIGCHLD` and delegates to `sys_fork`)
to support thread creation:

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

The `Process` struct in `kernel/src/process/mod.rs` gains `tid` and `tgid` fields.
A new `ThreadGroup` struct holds shared state (page table, fd table, signal actions)
with `Arc` references so all threads in a group share the same objects.

```rust
struct ThreadGroup {
    leader_tid: u32,
    members: Mutex<Vec<u32>>,          // TIDs of all threads
}
```

The `Task` struct in `kernel/src/task/mod.rs` is unchanged — it already has `pid`
which maps to the kernel task's associated process entry. Thread identity is tracked
in the `Process` struct via new `tid` and `tgid` fields.

### `futex()` -- Full Implementation

The current futex (syscall 202) is a single-threaded stub that force-clears the
futex word and pretends wake succeeded. Replace with a real implementation:

**Operations:**
- `FUTEX_WAIT` -- if `*uaddr == val`, sleep until woken. Atomic check-and-sleep.
- `FUTEX_WAKE` -- wake up to `val` threads waiting on `uaddr`.
- `FUTEX_WAIT_BITSET` -- wait with a bitmask (used by musl).
- `FUTEX_WAKE_BITSET` -- wake with a bitmask.

**Kernel data structure:**
```rust
// Global hash table of futex wait queues, keyed by (page_table_root, vaddr)
static FUTEX_TABLE: Mutex<BTreeMap<(u64, u64), Vec<FutexWaiter>>>;
```

**Key properties:**
- The futex word is in userspace memory -- the kernel only intervenes on contention.
- Address is resolved to (address-space, virtual-address) pair for correctness.
- Supports `FUTEX_PRIVATE_FLAG` optimization (no cross-process sharing needed).

### Thread-Local Storage (TLS)

- `arch_prctl(ARCH_SET_FS, addr)` -- already implemented, sets FS base per-process.
- `CLONE_SETTLS` -- set FS base for new thread during clone.
- Each thread has its own FS base register, saved/restored on context switch.
- musl uses TLS for `errno`, thread-specific data, and internal state.

### `set_tid_address()` -- Proper Implementation

Currently a no-op that returns PID. Implement:
- Store the `clear_child_tid` address in the process struct.
- On thread exit: write 0 to `*clear_child_tid` and call `futex_wake(clear_child_tid, 1)`.
- This is how `pthread_join()` works in musl.

### Thread Exit and Cleanup

When a thread exits (via `exit()` or `exit_group()`):
- `exit()` (syscall 60) -- exit only this thread.
- `exit_group()` (syscall 231) -- exit all threads in the group.
- On single-thread exit: clean up thread stack, wake joiners via `clear_child_tid`.
- On group exit: send `SIGKILL` to all other threads in the group.

### `gettid()` Syscall

Implement syscall 186 (`gettid`) properly:
- Return the thread's own TID (currently aliases to `sys_getpid()`).

### Signal Delivery to Thread Groups

Update signal handling for thread groups:
- Process-directed signals (e.g., `kill(pid, sig)`) -- delivered to any thread in the group.
- Thread-directed signals (e.g., `tkill(tid, sig)`) -- delivered to specific thread.
- `SIGSTOP`/`SIGCONT` affect the entire thread group.

## Important Components and How They Work

### Process struct (tid/tgid extension)

The `Process` struct in `kernel/src/process/mod.rs` gains `tid: u32` (unique thread
ID, equal to PID for the group leader and main-thread-only processes) and `tgid: u32`
(thread group ID, always the leader's PID). For single-threaded processes, `tid == tgid == pid`,
preserving backward compatibility. When `CLONE_THREAD` creates a new thread, the child
gets a fresh `tid` but inherits the parent's `tgid`.

### ThreadGroup shared state

When `clone(CLONE_THREAD)` is called, the parent's `fd_table` and `signal_actions`
become `Arc`-shared with the child. Both threads point to the same underlying data.
The `ThreadGroup` struct tracks membership so `exit_group()` can find and kill all
siblings.

### Futex wait queue table

A global `BTreeMap<(u64, u64), Vec<FutexWaiter>>` keyed by `(page_table_root, vaddr)`
stores sleeping threads. `FUTEX_WAIT` atomically checks the futex word and enqueues
the caller. `FUTEX_WAKE` dequeues and wakes waiters. The `FUTEX_PRIVATE_FLAG` lets
the kernel skip address-space resolution for process-private futexes.

### Context switch FS base restore

The existing `switch_context` saves/restores callee-saved registers. After switching,
the scheduler must also restore the new thread's `fs_base` via `wrmsr(IA32_FS_BASE)`.
This already happens in the syscall return path via `process.fs_base`, but must be
verified for thread-to-thread switches on the same core.

## How This Builds on Earlier Phases

- Extends Phase 11's process model by allowing multiple kernel tasks to share a single
  address space and file descriptor table via `CLONE_VM` and `CLONE_FILES`.
- Replaces the Phase 39 futex stub (force-clear word, pretend wake) with a real
  wait-queue-based implementation using `FUTEX_WAIT`/`FUTEX_WAKE`.
- Reuses Phase 35's per-core scheduling and SMP infrastructure to run threads on
  different cores simultaneously.
- Reuses Phase 33's slab allocator for efficient kernel stack allocation for new threads.
- Builds on the existing `arch_prctl(ARCH_SET_FS)` from the POSIX compatibility work
  to provide per-thread TLS.

## Implementation Outline

1. Add `tid`, `tgid`, and `clear_child_tid` fields to `Process`; implement `gettid()`.
2. Implement `ThreadGroup` struct and membership tracking.
3. Extend `clone()` to accept `CLONE_VM | CLONE_THREAD | CLONE_FILES | CLONE_SIGHAND`.
4. Allocate per-thread kernel stacks; share page table root with `CLONE_VM`.
5. Implement `CLONE_SETTLS` (set FS base for new thread).
6. Implement `CLONE_CHILD_CLEARTID` and `set_tid_address()`.
7. Implement `FUTEX_WAIT` and `FUTEX_WAKE` with proper wait queues.
8. Implement `FUTEX_WAIT_BITSET` and `FUTEX_WAKE_BITSET`.
9. Update `exit()` for thread-only exit vs `exit_group()` for group exit.
10. Update signal delivery for thread groups (`tkill`, process-directed signals).
11. Build userspace thread test program.
12. Validate with musl `pthread_create` / `pthread_join` if possible.

## Acceptance Criteria

- `clone(CLONE_VM | CLONE_THREAD | ...)` creates a thread sharing the parent's address space.
- `gettid()` returns different values for threads in the same group.
- `getpid()` returns the same value for all threads in a group.
- `futex(FUTEX_WAIT)` sleeps and `futex(FUTEX_WAKE)` wakes the sleeper.
- `pthread_mutex_lock/unlock` works (built on futex).
- Thread-local variables work via per-thread FS base.
- `set_tid_address()` stores the clear_child_tid pointer and wakes futex on thread exit.
- `exit()` (syscall 60) terminates only the calling thread.
- `exit_group()` (syscall 231) terminates all threads in the group.
- Multiple threads on different cores can run simultaneously.
- All existing tests pass (single-threaded programs unaffected).

## Companion Task List

- [Phase 40 Task List](./tasks/40-threading-primitives-tasks.md)

## How Real OS Implementations Differ

- **NPTL** (Native POSIX Threads Library) -- musl and glibc implement pthreads using
  `clone()` + `futex()`, exactly the mechanism we implement.
- **Robust futexes** -- automatically release on thread death; we defer this.
- **Priority inheritance futexes** -- prevent priority inversion; we defer this.
- **Thread-specific signal masks** via per-thread `rt_sigprocmask`; we use process-wide masks initially.
- Linux uses `clone3()` (syscall 435) as the modern replacement for `clone()`; we use the classic `clone()`.
- Real kernels use a hash table with bucket locks for the futex table; we use a single `BTreeMap` under one lock.

## Deferred Until Later

- Robust futexes (auto-release on death)
- Priority inheritance (PI) futexes
- `clone3()` syscall
- Thread naming (`prctl PR_SET_NAME`)
- Per-thread CPU affinity (extend existing per-process affinity)
- POSIX thread cancellation
- Per-thread signal masks
- User-level threading (M:N model)
- `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE` / `FUTEX_WAKE_OP`
