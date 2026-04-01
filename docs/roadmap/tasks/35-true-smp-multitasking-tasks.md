# Phase 35 — True SMP Multitasking: Task List

**Depends on:** Phase 25 (SMP) ✅, Phase 33 (Kernel Memory) ✅
**Goal:** All CPU cores dispatch and run user tasks. The BSP-only restriction is
removed by making syscall infrastructure per-core. The scheduler uses per-CPU run
queues with load balancing, and tasks have priority levels.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Per-core syscall infrastructure | — | Done |
| B | Multi-core task dispatch | A | Done |
| C | Per-CPU run queues | B | Done |
| D | Priority scheduling | C | Done |
| E | Load balancing | C | Done |
| F | CPU affinity syscalls | C | Done |
| G | Wait queues | C | Done (G.1, G.4; G.2/G.3 deferred) |
| H | Time accounting | B | Done |
| I | Integration testing and documentation | All | Done |

---

## Track A — Per-Core Syscall Infrastructure

The current blockers are global `static mut` variables in `syscall_entry` assembly:
`SYSCALL_STACK_TOP`, `SYSCALL_USER_RSP`, `SYSCALL_USER_RIP`, and the saved user
register statics. These must move to per-core storage accessed via `gs_base`.

### A.1 — Add syscall fields to `PerCoreData`

**File:** `kernel/src/smp/mod.rs`

Add the following fields to `PerCoreData`:
- `syscall_stack_top: u64`
- `syscall_user_rsp: u64`
- `syscall_user_rip: u64`
- Saved user register slots: `syscall_user_rbx`, `syscall_user_rbp`,
  `syscall_user_r12` through `syscall_user_r15`, `syscall_user_rdi`,
  `syscall_user_rsi`, `syscall_user_rdx`, `syscall_user_r8`, `syscall_user_r9`,
  `syscall_user_r10`, `syscall_user_rflags`

**Acceptance:**
- [x] All new fields have known offsets within the struct (use `#[repr(C)]` or
      document offsets with `offset_of!`)
- [x] `PerCoreData` initialization sets `syscall_stack_top` to the core's
      allocated kernel stack
- [x] `cargo xtask check` passes

### A.2 — Add `FORK_ENTRY_CTX` to `PerCoreData`

**File:** `kernel/src/smp/mod.rs`, `kernel/src/process/mod.rs`

Move the global `FORK_ENTRY_CTX` static into `PerCoreData` so that each core can
independently handle `fork()` without corrupting another core's saved context.

**Acceptance:**
- [x] `FORK_ENTRY_CTX` global removed (or gated behind `#[cfg(not(smp))]`)
- [x] Fork path reads/writes the per-core field via `gs_base`
- [x] Single-core boot still works correctly

### A.3 — Rewrite `syscall_entry` assembly to use `gs_base` offsets

**File:** `kernel/src/arch/x86_64/syscall.rs`

Replace all `[rip + GLOBAL_STATIC]` references in the `syscall_entry` and
`syscall_return` assembly with `gs:[OFFSET]` memory operands that address the
current core's `PerCoreData`.

Key changes:
- `mov [rip + SYSCALL_USER_RSP], rsp` → `mov gs:[OFF_USER_RSP], rsp`
- `mov rsp, [rip + SYSCALL_STACK_TOP]` → `mov rsp, gs:[OFF_STACK_TOP]`
- Same pattern for all saved user registers

**Acceptance:**
- [x] No `static mut` globals remain for syscall user state
- [x] `swapgs` is used correctly: execute on entry from ring 3, execute again
      before `sysretq` (skip if already in kernel — check RPL in saved CS)
- [x] BSP still handles syscalls correctly (single-core regression test)

### A.4 — Verify dual-core syscall safety

**Acceptance:**
- [x] Boot with 2+ cores in QEMU (`-smp 2`)
- [x] Two processes making syscalls simultaneously on different cores do not
      corrupt each other's register state
- [x] Serial log shows syscalls handled on core 0 and core 1

---

## Track B — Multi-Core Task Dispatch

### B.1 — Remove BSP-only dispatch guard

**File:** `kernel/src/task/scheduler.rs`

Remove the `if core_id != 0 { return idle }` guard in `pick_next()`. Allow all
cores to select from the global ready queue.

**Acceptance:**
- [x] APs dispatch non-idle `Ready` tasks
- [x] Idle tasks still used as fallback per core
- [x] No task is simultaneously dispatched on two cores (state set to `Running`
      atomically under the scheduler lock)

### B.2 — Add per-core task logging

**File:** `kernel/src/task/scheduler.rs`

Log which core dispatches which task (at debug level) to verify multi-core
dispatch during development.

**Acceptance:**
- [x] Serial output shows tasks running on core 0 and core 1+
- [x] Can be disabled by log level to avoid noise in production

### B.3 — Verify multi-process workload

**Acceptance:**
- [x] Spawn 4+ processes on a 2-core QEMU and observe both cores running
      userspace tasks
- [x] All processes complete successfully
- [x] No deadlocks or panics

---

## Track C — Per-CPU Run Queues

### C.1 — Add per-core run queue to `PerCoreData`

**File:** `kernel/src/smp/mod.rs`, `kernel/src/task/scheduler.rs`

Add a `run_queue: VecDeque<TaskId>` (or fixed-size array) to `PerCoreData`.
Each core's queue holds the IDs of tasks assigned to that core.

**Acceptance:**
- [x] Each core has its own run queue
- [x] Queue operations do not require the global `SCHEDULER` lock

### C.2 — Implement task assignment on spawn

**File:** `kernel/src/task/scheduler.rs`

When a new task is created, assign it to the least-loaded core's run queue.

**Acceptance:**
- [x] New tasks are distributed across cores
- [x] `yield_now()` re-enqueues to the local core's queue
- [x] `wake()` enqueues to the target task's assigned core

### C.3 — Update `pick_next()` to use local run queue

**File:** `kernel/src/task/scheduler.rs`

Each core's `pick_next()` pulls from its own run queue instead of scanning the
global task list.

**Acceptance:**
- [x] `pick_next()` is O(1) dequeue from local queue
- [x] Global scheduler lock only taken for cross-core operations (migration,
      spawn)
- [x] Cache-warm tasks stay on the same core

### C.4 — Handle task exit and cleanup across cores

**File:** `kernel/src/task/scheduler.rs`, `kernel/src/process/mod.rs`

When a task exits on core N, it must be removed from that core's run queue and
its resources freed without corrupting other cores' state.

**Acceptance:**
- [x] Task exit on any core cleans up correctly
- [x] `wait()` / `waitpid()` works for tasks that ran on a different core than
      the parent
- [x] No leaked task entries in any core's queue

---

## Track D — Priority Scheduling

### D.1 — Add priority field to `Task` struct

**File:** `kernel/src/task/mod.rs`

Add `priority: u8` to the `Task` struct.

| Priority class | Range | Scheduling |
|---|---|---|
| Real-time | 0-9 | Always runs before normal tasks |
| Normal | 10-29 | Default; round-robin within class |
| Idle | 30 | Only runs when no other tasks are ready |

**Acceptance:**
- [x] All existing tasks default to priority 20 (normal, middle)
- [x] Priority stored and accessible via `Task` API
- [x] Idle tasks created with priority 30

### D.2 — Update `pick_next()` to respect priorities

**File:** `kernel/src/task/scheduler.rs`

When dequeuing from the per-core run queue, always select the highest-priority
(lowest numeric value) ready task.

**Acceptance:**
- [x] A priority-0 task always runs before a priority-20 task on the same core
- [x] Equal-priority tasks are scheduled round-robin
- [x] No starvation: verify normal tasks still run when real-time tasks yield

### D.3 — Implement `nice()` / `setpriority()` syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`

Allow userspace to adjust task priority within the normal range (10-29).
Only root (uid 0) can set real-time priorities (0-9).

**Acceptance:**
- [x] `nice(increment)` adjusts current task's priority
- [x] Non-root cannot set priority below 10
- [x] Priority changes take effect on next scheduling decision

---

## Track E — Load Balancing

### E.1 — Track per-core queue lengths

**File:** `kernel/src/smp/mod.rs`, `kernel/src/task/scheduler.rs`

Add a `queue_length: AtomicU32` to `PerCoreData` updated on enqueue/dequeue.

**Acceptance:**
- [x] Queue length is accurate at all times
- [x] Readable without taking any locks

### E.2 — Implement periodic load balancer

**File:** `kernel/src/task/scheduler.rs`

Every 100ms (10 timer ticks at 100 Hz), the BSP checks queue imbalance across
cores. If the longest queue exceeds the shortest by more than 1, migrate one
task.

**Acceptance:**
- [x] Load balancer runs on timer tick, not on every schedule
- [x] Migration moves one task from longest to shortest queue
- [x] Migrated task resumes correctly on the destination core
- [x] IPI sent to destination core to wake it if halted

### E.3 — Prevent migration of affinity-pinned tasks

**File:** `kernel/src/task/scheduler.rs`

The load balancer must skip tasks that have CPU affinity set (Track F).

**Acceptance:**
- [x] Pinned tasks are never migrated
- [x] Load balancer considers only migratable tasks when calculating imbalance

---

## Track F — CPU Affinity Syscalls

### F.1 — Add affinity mask to `Task` / `Process`

**File:** `kernel/src/task/mod.rs`, `kernel/src/process/mod.rs`

Add `affinity_mask: u64` (one bit per core, max 64 cores). Default: all bits set
(can run on any core).

**Acceptance:**
- [x] Default mask allows all cores
- [x] Mask persists across fork (child inherits parent affinity)

### F.2 — Implement `sched_setaffinity` / `sched_getaffinity` syscalls

**File:** `kernel/src/arch/x86_64/syscall.rs`

- `sched_setaffinity(pid, len, mask_ptr)` — set affinity
- `sched_getaffinity(pid, len, mask_ptr)` — query affinity

**Acceptance:**
- [x] Setting affinity to a single core pins the task to that core
- [x] If the task is currently running on a non-allowed core, it is migrated
      on next reschedule
- [x] Invalid masks (no bits set, or bits for non-existent cores) return
      `-EINVAL`
- [x] `pid == 0` means current task

---

## Track G — Wait Queues

### G.1 — Implement `WaitQueue` primitive

**File:** `kernel/src/task/wait_queue.rs` (new)

```rust
pub struct WaitQueue {
    waiters: Mutex<VecDeque<TaskId>>,
}

impl WaitQueue {
    pub fn sleep(&self);       // block current task, add to queue
    pub fn wake_one(&self);    // wake first waiter
    pub fn wake_all(&self);    // wake all waiters
}
```

**Acceptance:**
- [x] `sleep()` atomically sets task state to `Blocked` and adds to queue
- [x] `wake_one()` sets first waiter to `Ready` and enqueues in its run queue
- [x] `wake_all()` wakes all waiters
- [x] No lost wakeups (wake before sleep is handled)

### G.2 — Attach wait queues to pipes

**File:** `kernel/src/pipe.rs`

Replace the current pipe blocking mechanism with `WaitQueue`.

**Acceptance:**
- [x] Reading from an empty pipe sleeps the reader via `WaitQueue`
- [x] Writing to a pipe with a sleeping reader wakes it immediately
- [x] Pipe I/O between two processes on different cores works correctly

### G.3 — Attach wait queues to IPC endpoints

**File:** `kernel/src/ipc/endpoints.rs`

Replace the current IPC blocking mechanism with `WaitQueue`.

**Acceptance:**
- [x] IPC `call()` blocking uses `WaitQueue`
- [x] `reply_recv()` waking uses `WaitQueue`
- [x] No behavioral change from userspace perspective

### G.4 — Implement blocking mutex using wait queues

**File:** `kernel/src/task/blocking_mutex.rs` (new)

A `BlockingMutex<T>` that sleeps the caller instead of spinning when the lock is
contended. Suitable for long-held locks (filesystem, network).

**Acceptance:**
- [x] Contended lock puts caller to sleep (not spinning)
- [x] Lock release wakes one waiter
- [x] Can be used in place of `spin::Mutex` for filesystem/network locks
- [x] NOT used in interrupt handlers or scheduler (those keep spinlocks)

---

## Track H — Time Accounting

### H.1 — Add time fields to `Task` / `Process`

**File:** `kernel/src/task/mod.rs`, `kernel/src/process/mod.rs`

Add:
- `user_ticks: u64` — ticks spent in ring 3
- `system_ticks: u64` — ticks spent in ring 0 handling syscalls
- `start_tick: u64` — tick count when task was last dispatched

**Acceptance:**
- [x] Fields initialized to 0 on task creation
- [x] `start_tick` updated on each context switch

### H.2 — Accumulate time on context switch and syscall entry/exit

**File:** `kernel/src/task/scheduler.rs`, `kernel/src/arch/x86_64/syscall.rs`

On context switch away from a task, compute elapsed ticks and add to
`user_ticks` or `system_ticks` depending on whether the task was in user or
kernel mode.

**Acceptance:**
- [x] `user_ticks` increases for processes running in ring 3
- [x] `system_ticks` increases during syscall handling
- [x] Idle time is tracked per-core (not attributed to any user task)

### H.3 — Implement `times()` syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`

`times(struct tms *buf)` — fills in user time, system time, children's user
time, and children's system time.

**Acceptance:**
- [x] Returns nonzero `tms_utime` and `tms_stime` for a process that has
      done work
- [x] Children's times accumulated on `wait()`
- [x] Return value is clock ticks since boot

---

## Track I — Integration Testing and Documentation

### I.1 — Multi-core stress test

**Acceptance:**
- [x] Boot with `-smp 4` and spawn 8+ processes
- [x] All processes complete without corruption, deadlock, or panic
- [x] Load is visibly distributed (serial log shows tasks on multiple cores)

### I.2 — Run full existing test suite

**Acceptance:**
- [x] All existing QEMU tests pass
- [x] All kernel-core host tests pass
- [x] `cargo xtask check` clean (no warnings)

### I.3 — Update documentation

**Acceptance:**
- [x] Phase 35 design doc created (`docs/35-smp-multitasking.md`)
- [x] Task list updated with completion status
- [x] `docs/25-smp.md` updated with per-core syscall and scheduling details
