# Phase 40 — Threading Primitives: Task List

**Status:** Complete
**Source Ref:** phase-40
**Depends on:** Phase 25 (SMP) ✅, Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP) ✅
**Goal:** Add kernel-level threads via `clone(CLONE_THREAD)`, replace the futex stub
with real wait/wake queues, implement per-thread TLS via FS base, and update exit and
signal delivery for thread groups. Programs using `pthread_create`/`pthread_join` work.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Thread identity (tid/tgid) and gettid | — | Complete |
| B | Thread group shared state | A | Complete |
| C | clone(CLONE_THREAD) implementation | A, B | Complete |
| D | Futex wait/wake infrastructure | — | Complete |
| E | set_tid_address and clear_child_tid | A, D | Complete |
| F | Thread exit and exit_group | B, C, E | Complete |
| G | Thread-group signal delivery | B, F | Complete |
| H | Userspace test programs | C, D, F | Complete |
| I | Integration testing and documentation | A–H | Complete |

---

## Track A — Thread Identity (tid/tgid) and gettid

Add per-thread identity fields so each thread has a unique TID while threads
in the same group share a TGID (which equals the group leader's PID).

### A.1 — Add `tid` and `tgid` fields to `Process`

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process`
**Why it matters:** The current `Process` struct has only `pid` and no concept
of thread identity. Every thread-aware syscall (`gettid`, `set_tid_address`,
`tkill`, `clone CLONE_THREAD`) needs a per-thread TID distinct from the process
PID to correctly identify individual threads within a group.

**Acceptance:**
- [x] `Process` struct has `tid: u32` field (unique per thread, defaults to `pid` for single-threaded processes)
- [x] `Process` struct has `tgid: u32` field (thread group ID, always the leader's PID)
- [x] Existing process creation (`alloc_pid` / `create_process`) initializes `tid = pid` and `tgid = pid`
- [x] `getpid()` (syscall 39) returns `tgid` instead of `pid` so thread group members report the same PID

### A.2 — Implement `gettid()` as a distinct syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_gettid`
**Why it matters:** Syscall 186 currently aliases to `sys_getpid()`. Threads need
`gettid()` to return their unique TID so musl can track thread identity for
`pthread_self()`, `clear_child_tid` wakeups, and thread-directed signals.

**Acceptance:**
- [x] Syscall 186 dispatches to a new `sys_gettid()` function (not `sys_getpid()`)
- [x] `sys_gettid()` returns the calling thread's `tid` from its `Process` entry
- [x] For single-threaded processes, `gettid()` returns the same value as `getpid()` (backward compatible)

### A.3 — Add `clear_child_tid` field to `Process`

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process`
**Why it matters:** `set_tid_address()` and `CLONE_CHILD_CLEARTID` store a userspace
pointer that the kernel writes 0 to and futex-wakes on thread exit. This is the
mechanism `pthread_join()` uses to detect thread completion.

**Acceptance:**
- [x] `Process` struct has `clear_child_tid: u64` field (0 = disabled)
- [x] Field initialized to 0 in process creation
- [x] Field is per-thread (each thread in a group can have its own value)

---

## Track B — Thread Group Shared State

Introduce a `ThreadGroup` struct so threads created with `CLONE_THREAD` share
the same fd table, signal actions, and address space via `Arc` references.

### B.1 — Define `ThreadGroup` struct

**File:** `kernel/src/process/mod.rs`
**Symbol:** `ThreadGroup`
**Why it matters:** When `CLONE_THREAD` creates a thread, the child must share
the parent's fd table, signal handlers, and address space. A `ThreadGroup` struct
tracks membership and holds `Arc`-shared resources. Without it, each thread would
have independent copies, breaking POSIX thread semantics.

**Acceptance:**
- [x] `ThreadGroup` struct defined with `leader_tid: u32` and `members: Mutex<Vec<u32>>`
- [x] `Process` struct has `thread_group: Option<Arc<ThreadGroup>>` field
- [x] For single-threaded processes, `thread_group` is `None`
- [x] When a thread group is created, the leader and child both reference the same `Arc<ThreadGroup>`

### B.2 — Share fd table via `Arc` for thread groups

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process`, `FdEntry`
**Why it matters:** POSIX requires threads to share file descriptors. Currently
`fd_table` is a fixed array per-process. For thread groups, all members must see
the same fd table so that `open()` in one thread is visible to `read()` in another.

**Acceptance:**
- [x] `Process` gains a `shared_fd_table: Option<Arc<Mutex<[Option<FdEntry>; MAX_FDS]>>>` field
- [x] When `CLONE_FILES` is set, child references the same `Arc` as the parent
- [x] All fd operations (`open`, `close`, `dup`, `read`, `write`) use the shared table when present
- [x] Single-threaded processes continue using the per-process `fd_table` directly (no overhead)

### B.3 — Share signal actions via `Arc` for thread groups

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process`, `SignalAction`
**Why it matters:** POSIX requires threads to share signal dispositions. When one
thread calls `sigaction()`, the new handler applies to the entire thread group.
Without sharing, threads would have inconsistent signal behavior.

**Acceptance:**
- [x] `Process` gains a `shared_signal_actions: Option<Arc<Mutex<[SignalAction; 32]>>>` field
- [x] When `CLONE_SIGHAND` is set, child references the same `Arc` as the parent
- [x] `sys_rt_sigaction()` uses the shared table when present
- [x] Single-threaded processes continue using the per-process `signal_actions` directly

---

## Track C — clone(CLONE_THREAD) Implementation

Extend the existing `clone()` / `sys_fork()` path to create threads that share
the parent's address space, fd table, and signal handlers.

### C.1 — Parse clone flags in `sys_clone()`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_clone`
**Why it matters:** The current `sys_clone` (syscall 56) ignores flags and delegates
to `sys_fork`. To create threads, the kernel must parse and validate the clone flag
combination. Invalid flag combinations (e.g., `CLONE_THREAD` without `CLONE_VM`)
must be rejected to prevent undefined behavior.

**Acceptance:**
- [x] `sys_clone` reads `flags` (arg0), `child_stack` (arg1), `parent_tidptr` (arg2), `child_tidptr` (arg3), `tls` (arg4)
- [x] When `flags & CLONE_THREAD` is set, dispatches to a new `sys_clone_thread()` path
- [x] When `flags == SIGCHLD` (or 0x11), continues to delegate to `sys_fork()` for backward compatibility
- [x] Returns `-EINVAL` if `CLONE_THREAD` is set without `CLONE_VM`

### C.2 — Implement `sys_clone_thread()` — thread creation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_clone_thread`
**Why it matters:** This is the core thread creation path. It allocates a new process
entry and kernel task that shares the parent's page table, fd table, and signal actions.
The child starts execution at the provided stack pointer and entry point, enabling
musl's `__clone` to set up the new thread.

**Acceptance:**
- [x] Allocates a new PID slot for the child (used as TID)
- [x] Sets child `tgid` to parent's `tgid` (same thread group)
- [x] Sets child `page_table_root` to parent's `page_table_root` (shared address space, no CoW clone)
- [x] Shares fd table (`CLONE_FILES`) and signal actions (`CLONE_SIGHAND`) via `Arc`
- [x] Creates or joins the `ThreadGroup`, adding child TID to members list
- [x] Allocates a new kernel stack for the child thread
- [x] Child starts at `child_stack` with the entry point from the parent's `rip` (or `fn` arg from musl)
- [x] Returns child TID to parent, 0 to child

### C.3 — Implement `CLONE_PARENT_SETTID` and `CLONE_SETTLS`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_clone_thread`
**Why it matters:** musl's `__clone` wrapper passes `CLONE_PARENT_SETTID` so the
parent can read the child's TID before the child runs, and `CLONE_SETTLS` so each
thread has its own FS base for thread-local storage (errno, pthread struct).

**Acceptance:**
- [x] When `CLONE_PARENT_SETTID` is set, writes child TID to `*parent_tidptr` in userspace
- [x] When `CLONE_SETTLS` is set, stores `tls` argument as child's `fs_base`
- [x] When `CLONE_CHILD_CLEARTID` is set, stores `child_tidptr` as child's `clear_child_tid`
- [x] Child's FS base is restored on context switch (existing `fs_base` restore path)

---

## Track D — Futex Wait/Wake Infrastructure

Replace the single-threaded futex stub with a real implementation that sleeps
and wakes threads based on userspace memory words.

### D.1 — Define futex wait queue table

**File:** `kernel/src/process/futex.rs`
**Symbol:** `FUTEX_TABLE`, `FutexWaiter`
**Why it matters:** The futex table is the core kernel data structure for thread
synchronization. Every `pthread_mutex_lock` that contends, every `pthread_cond_wait`,
and every `pthread_join` ultimately sleeps on a futex. Without a real wait queue,
threads spin-wait and waste CPU.

**Acceptance:**
- [x] `FutexWaiter` struct holds `tid: u32` and `bitset: u32`
- [x] `FUTEX_TABLE` is a `Mutex<BTreeMap<(u64, u64), Vec<FutexWaiter>>>` keyed by `(page_table_root, vaddr)`
- [x] Table is statically initialized
- [x] Supports concurrent access from multiple cores via the mutex

### D.2 — Implement `FUTEX_WAIT` operation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_futex`
**Why it matters:** `FUTEX_WAIT` is the sleep side of futex synchronization. It
atomically checks that `*uaddr == val` and puts the thread to sleep if so. The
atomicity between the check and the sleep is critical — without it, a wake between
check and sleep would be lost, causing the thread to sleep forever.

**Acceptance:**
- [x] Reads the futex word from userspace at `uaddr`
- [x] If `*uaddr != val`, returns `-EAGAIN` immediately (spurious wakeup is OK)
- [x] If `*uaddr == val`, adds the calling thread to `FUTEX_TABLE` and blocks
- [x] Thread is put into a `BlockedOnFutex` state (new `TaskState` variant or reuse `BlockedOnWait`)
- [x] Returns 0 when woken by `FUTEX_WAKE`
- [x] Handles `FUTEX_PRIVATE_FLAG` (use pid 0 as page_table_root key for process-private futexes)

### D.3 — Implement `FUTEX_WAKE` operation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_futex`
**Why it matters:** `FUTEX_WAKE` is the wake side. When a thread releases a mutex
or signals a condvar, it wakes waiters so they can re-check the futex word and
proceed. Returning the count of woken threads lets callers know if anyone was waiting.

**Acceptance:**
- [x] Looks up `(page_table_root, uaddr)` in `FUTEX_TABLE`
- [x] Wakes up to `val` threads from the wait queue (FIFO order)
- [x] Woken threads transition from blocked to ready state
- [x] Returns the number of threads actually woken
- [x] If no waiters exist for the address, returns 0

### D.4 — Implement `FUTEX_WAIT_BITSET` and `FUTEX_WAKE_BITSET`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_futex`
**Why it matters:** musl uses `FUTEX_WAIT_BITSET` with `FUTEX_BITSET_MATCH_ANY`
(0xFFFFFFFF) as a replacement for plain `FUTEX_WAIT` in some code paths. Without
bitset support, musl's internal locks may fail or fall back to slower paths.

**Acceptance:**
- [x] `FUTEX_WAIT_BITSET` (op=9) behaves like `FUTEX_WAIT` but stores the bitset with the waiter
- [x] `FUTEX_WAKE_BITSET` (op=10) wakes only waiters whose bitset overlaps (AND) with the wake bitset
- [x] `FUTEX_BITSET_MATCH_ANY` (0xFFFFFFFF) matches all waiters (equivalent to plain WAIT/WAKE)
- [x] Plain `FUTEX_WAIT`/`FUTEX_WAKE` internally use `FUTEX_BITSET_MATCH_ANY`

---

## Track E — set_tid_address and clear_child_tid

Implement the `set_tid_address()` syscall and the thread-exit futex wake that
enables `pthread_join()`.

### E.1 — Implement `set_tid_address()` properly

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_set_tid_address`
**Why it matters:** The current implementation ignores the `tidptr` argument and
just returns PID. musl calls `set_tid_address()` during thread startup to register
the address where the kernel should write 0 and wake a futex on thread exit. This
is how `pthread_join()` detects thread completion.

**Acceptance:**
- [x] Stores `tidptr` argument in the calling thread's `clear_child_tid` field
- [x] Returns the calling thread's TID (not PID)
- [x] Works for both single-threaded processes and thread group members

### E.2 — Wake futex on thread exit via `clear_child_tid`

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_exit`
**Why it matters:** When a thread exits, the kernel must write 0 to
`*clear_child_tid` and call `futex_wake(clear_child_tid, 1)`. This is the
mechanism that unblocks a `pthread_join()` caller waiting in
`futex(FUTEX_WAIT, clear_child_tid, tid)`.

**Acceptance:**
- [x] On thread exit, if `clear_child_tid != 0`, writes 0 to that userspace address
- [x] After writing 0, calls `futex_wake(clear_child_tid, 1)` to wake one waiter
- [x] Safely handles invalid `clear_child_tid` addresses (skip if page not mapped)
- [x] Works correctly when the exiting thread is the last in its group

---

## Track F — Thread Exit and exit_group

Differentiate between single-thread exit and whole-group exit, and clean up
thread resources without destroying the shared address space.

### F.1 — Implement thread-only exit (syscall 60)

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_exit`
**Why it matters:** Currently syscall 60 and 231 are identical — both kill the
entire process. With threads, syscall 60 (`exit`) must only terminate the calling
thread, leaving siblings running. The shared address space and fd table must remain
alive until the last thread exits.

**Acceptance:**
- [x] Syscall 60 terminates only the calling thread when it is not the last in its group
- [x] Removes the thread's TID from `ThreadGroup.members`
- [x] Frees the thread's kernel stack but does NOT free the shared page table
- [x] Triggers `clear_child_tid` futex wake (Track E)
- [x] When the last thread in a group exits, performs full process cleanup (free page table, close fds)
- [x] For single-threaded processes, behavior is unchanged (full process exit)

### F.2 — Implement `exit_group()` (syscall 231)

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_exit_group`
**Why it matters:** `exit_group()` terminates all threads in the group, not just
the caller. This is what `exit()` in C (via musl) actually calls, and what happens
on `SIGKILL`. Without it, a multi-threaded program cannot cleanly terminate.

**Acceptance:**
- [x] Syscall 231 dispatches to a new `sys_exit_group()` function
- [x] Sends termination signal to all other threads in the same `ThreadGroup`
- [x] Each sibling thread is marked dead and cleaned up
- [x] The calling thread exits last, performing full process cleanup
- [x] For single-threaded processes, behaves identically to `sys_exit()`

---

## Track G — Thread-Group Signal Delivery

Update the existing signal infrastructure so process-directed signals reach
thread groups correctly, and add `tkill` for thread-directed signals.

### G.1 — Update `sys_kill()` for thread groups

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_kill`
**Why it matters:** When `kill(pid, sig)` targets a thread group, the signal must
be delivered to any one runnable thread in the group, not specifically to the thread
whose `pid` matches. Without this, signals may be silently dropped if the leader
thread is blocked.

**Acceptance:**
- [x] `sys_kill()` resolves `pid` to `tgid` and finds any non-blocked thread in the group
- [x] Signal is delivered to one arbitrary thread that does not have the signal blocked
- [x] If all threads block the signal, it remains pending on the group until one unblocks
- [x] Backward compatible: single-threaded processes behave exactly as before

### G.2 — Implement `tkill()` syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_tkill`
**Why it matters:** `tkill(tid, sig)` sends a signal to a specific thread, not
the group. musl uses this for `pthread_kill()` and internal thread cancellation.
Without it, there is no way to target a signal at a particular thread.

**Acceptance:**
- [x] Syscall 200 (`tkill`) dispatches to `sys_tkill(tid, sig)`
- [x] Delivers the signal to the specific thread identified by TID
- [x] Returns `-ESRCH` if no thread with the given TID exists
- [x] Permission checks match `sys_kill()` (same uid or CAP_KILL)

---

## Track H — Userspace Test Programs

Minimal test binaries that exercise threading from userspace.

### H.1 — Basic thread creation and join test

**File:** `userspace/thread-test/src/main.rs`
**Symbol:** `main`
**Why it matters:** Validates the core threading path end-to-end: `clone(CLONE_THREAD)`
creates a thread, the child runs on its own stack, and the parent can wait for
completion via the `clear_child_tid` futex mechanism.

**Acceptance:**
- [x] Uses raw `clone` syscall with `CLONE_VM | CLONE_THREAD | CLONE_FILES | CLONE_SIGHAND | CLONE_SETTLS | CLONE_CHILD_CLEARTID | CLONE_PARENT_SETTID`
- [x] Parent and child both call `gettid()` and verify they return different values
- [x] Parent and child both call `getpid()` and verify they return the same value
- [x] Child writes to a shared memory location, parent reads it after join
- [x] Parent joins via `futex(FUTEX_WAIT, &child_tid, child_tid_val)` on `clear_child_tid` address
- [x] Exits with 0 on success, non-zero on failure

### H.2 — Futex mutex stress test

**File:** `userspace/thread-test/src/main.rs`
**Symbol:** `test_futex_mutex`
**Why it matters:** A shared counter incremented by multiple threads under a futex
mutex validates that the synchronization primitives actually prevent data races.
This catches subtle bugs in the futex check-and-sleep atomicity.

**Acceptance:**
- [x] Creates 2-4 threads that each increment a shared counter N times
- [x] Counter protected by a userspace futex-based mutex (lock/unlock using `FUTEX_WAIT`/`FUTEX_WAKE`)
- [x] Final counter value equals (num_threads * N) exactly — no lost increments
- [x] Test passes consistently (not flaky)

### H.3 — Thread exit and exit_group test

**File:** `userspace/thread-test/src/main.rs`
**Symbol:** `test_exit_group`
**Why it matters:** Validates that `exit_group()` terminates all threads and that
single-thread `exit()` leaves siblings running. Incorrect exit behavior can cause
zombie threads or premature address space destruction.

**Acceptance:**
- [x] Creates a thread; child calls `exit(0)` (syscall 60) — parent continues running
- [x] Parent verifies it is still alive after child exits
- [x] Parent calls `exit_group(0)` (syscall 231) — all remaining threads terminate
- [x] Process exits cleanly with no zombie threads

---

## Track I — Integration Testing and Documentation

Final validation that all existing tests pass and documentation is updated.

### I.1 — Verify no regressions

**Files:**
- `kernel/tests/*.rs`
- `userspace/*/src/main.rs`
**Symbol:** (all existing tests)
**Why it matters:** Adding thread-group logic to `clone`, `exit`, `kill`, and the
fd table touches critical code paths. A regression in single-threaded process
behavior would break the entire system.

**Acceptance:**
- [x] `cargo xtask check` passes (clippy + fmt)
- [x] `cargo xtask test` passes (all existing QEMU tests)
- [x] `cargo test -p kernel-core` passes (host-side unit tests)

### I.2 — Update documentation

**Files:**
- `docs/roadmap/40-threading-primitives.md`
- `docs/roadmap/README.md`
**Symbol:** (documentation)
**Why it matters:** Roadmap docs must reflect the actual implementation state and
the README must link to the completed task list.

**Acceptance:**
- [x] Design doc status updated to `Complete` after implementation
- [x] README row updated with task list link and `Complete` status
- [x] Any deferred items accurately reflect what was and was not implemented

---

## Documentation Notes

- Phase 40 introduces the first multi-threaded execution in m3OS. Previously, each
  process had exactly one kernel task.
- The `Process` struct gains `tid` and `tgid` fields. For backward compatibility,
  single-threaded processes have `tid == tgid == pid`.
- `getpid()` now returns `tgid` (not `pid`) so all threads in a group report the
  same PID. This changes the semantics of `getpid()` but matches POSIX behavior.
- The futex stub in `sys_futex` (which force-cleared the futex word) is replaced
  with real wait queues. Single-threaded programs that previously relied on the stub
  behavior should continue to work because `FUTEX_WAIT` checks `*uaddr == val`
  before sleeping.
- `exit()` (syscall 60) changes from "exit entire process" to "exit this thread only"
  when the process has multiple threads. For single-threaded processes, behavior is
  unchanged.
- `exit_group()` (syscall 231) is now distinct from `exit()` and terminates all
  threads in the group.
- The shared fd table and signal actions use `Arc<Mutex<...>>` which adds locking
  overhead for thread groups. Single-threaded processes avoid this overhead by using
  the per-process fields directly.
