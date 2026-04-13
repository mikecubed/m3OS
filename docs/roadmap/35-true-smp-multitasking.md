# Phase 35 - True SMP Multitasking

## Milestone Goal

All CPU cores dispatch and run user tasks. The BSP-only restriction is removed by
making syscall infrastructure per-core. The scheduler introduces per-CPU run queues
with load balancing and task priorities, substantially improving dispatch locality
and multi-core throughput even though the global scheduler lock remains on the
dispatch hot path for task-state coordination.

## Learning Goals

- Understand why per-core state (stacks, saved registers) is essential for SMP syscalls.
- Learn the trade-offs between a single global run queue and per-CPU run queues.
- See how load balancing migrates tasks between cores to equalize utilization.
- Understand priority scheduling: real-time vs interactive vs batch tasks.
- Learn about CPU affinity and why it matters for cache performance.

## Feature Scope

### Per-Core Syscall Infrastructure

The current blockers for multi-core dispatch are global statics used during syscall
entry. Make these per-core:

| Global static | Fix |
|---|---|
| `SYSCALL_STACK_TOP` | Allocate a kernel stack per core, store in `PerCoreData` |
| `SYSCALL_USER_RSP` | Move to `PerCoreData` |
| `SYSCALL_USER_RIP` | Move to `PerCoreData` |
| `FORK_ENTRY_CTX` | Move to `PerCoreData` |

Update `syscall_entry` assembly to load the per-core syscall stack from `gs_base`
offset instead of a global variable.

### Remove BSP-Only Dispatch Restriction

Once per-core syscall infrastructure is in place:
1. Remove the `if core_id != 0 { return idle }` guard in `pick_next()`.
2. Allow APs to dispatch any `Ready` task.
3. Verify with a workload that spawns more processes than there are cores.

### Per-CPU Run Queues

Replace the single `SCHEDULER` mutex with per-CPU queues:

```
PerCoreData {
    run_queue: VecDeque<TaskId>,  // local ready queue
    current_task: Option<TaskId>,
    ...
}
```

**Task assignment:**
- New tasks are assigned to the least-loaded core.
- `yield_now()` re-enqueues to the local queue.
- `wake()` enqueues to the target core's queue (or the waker's queue).

**Shipped state (audited Phase 52d):** Per-core run queues, work-stealing,
load-balancing with migration cooldown, and dead-slot recycling all landed.
However, the global `SCHEDULER` lock is still acquired on every dispatch iteration
for task-state reads, transitions, and post-switch bookkeeping.

**Benefits:**
- Ready-queue contention is reduced compared to a single global queue.
- Cache-warm tasks stay on the same core.

### Load Balancing

Periodic load balancing (every N ticks) migrates tasks from overloaded to underloaded cores:

1. Each core tracks its queue length.
2. Every 100ms (10 ticks), the BSP checks queue imbalance.
3. If imbalance exceeds threshold, migrate one task from longest to shortest queue.
4. Send IPI to the destination core to wake it.

### Priority Levels

Add a priority field to the `Task` struct:

| Priority class | Range | Scheduling |
|---|---|---|
| Real-time | 0–9 | Always runs before normal tasks |
| Normal | 10–29 | Default; round-robin within class |
| Idle | 30 | Only runs when no other tasks are ready |

**Default:** All tasks start at priority 20 (normal, middle).

### CPU Affinity

Add optional core affinity to tasks:
- `sched_setaffinity(pid, mask)` — restrict task to specific cores.
- `sched_getaffinity(pid, mask)` — query current affinity.
- Scheduler respects affinity mask when selecting tasks.

### Sleeping Locks (Blocking Mutex)

Replace busy-wait spinlocks with sleeping locks for long-held locks:

**Spinlocks** (keep for short critical sections):
- Interrupt handler contexts
- Scheduler lock (very short hold times)

**Blocking mutexes** (new, for longer holds):
- File system operations
- Network stack operations
- IPC blocking

A blocking mutex puts the caller to sleep (changes task state to `Blocked`) and
wakes it when the lock is released, instead of spinning.

### Wait Queues

Add per-resource wait queues instead of the current "change state and hope the
scheduler notices" approach:

```rust
struct WaitQueue {
    waiters: VecDeque<TaskId>,
}

impl WaitQueue {
    fn sleep(&self);        // block current task, add to queue
    fn wake_one(&self);     // wake first waiter
    fn wake_all(&self);     // wake all waiters
}
```

Attach wait queues to: pipes, sockets, IPC endpoints, mutexes, notifications.

### Time Accounting

Track per-task and per-core time:
- User time (ticks spent in ring 3)
- System time (ticks spent in ring 0 handling syscalls)
- Idle time (ticks spent in halt)

Expose via `times()` syscall and future `/proc/stat`.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 25 (SMP) | AP boot, per-core data, IPI infrastructure |
| Phase 33 (Memory) | Slab allocator for efficient per-core data structures |

## Implementation Outline

1. Move syscall statics into `PerCoreData` and update `syscall_entry` assembly.
2. Verify: two cores can handle syscalls simultaneously without corruption.
3. Remove BSP-only guard in `pick_next()`.
4. Verify: tasks run on multiple cores (log which core runs which task).
5. Implement per-CPU run queues and move ready-queue contention off the single global queue while keeping the global scheduler lock for task-state coordination.
6. Implement load balancing (periodic migration).
7. Add priority field to `Task`; update `pick_next()` to respect priorities.
8. Implement `sched_setaffinity` / `sched_getaffinity` syscalls.
9. Implement wait queues; attach to pipes and sockets.
10. Add time accounting; implement `times()` syscall.
11. Stress test with many concurrent processes across all cores.

## Acceptance Criteria

- Tasks run on all available cores (verified via per-core logging).
- Two processes making syscalls simultaneously on different cores do not corrupt state.
- Per-CPU run queues reduce ready-queue contention, but the global scheduler lock remains on the dispatch hot path until later work.
- Load balancing distributes tasks across cores (no core is idle while another has >1 ready task).
- Priority 0 (real-time) tasks always preempt priority 20 (normal) tasks.
- `sched_setaffinity` pins a task to a specific core.
- Wait queues: a task sleeping on a pipe wakes immediately when data is written.
- Time accounting: `times()` returns nonzero user and system time.
- All existing tests pass without regression.

## Companion Task List

- [Phase 35 Task List](./tasks/35-true-smp-multitasking-tasks.md)

## How Real OS Implementations Differ

Linux's Completely Fair Scheduler (CFS):
- Uses a red-black tree ordered by "virtual runtime" for O(log n) scheduling.
- Per-CPU run queues with work-stealing load balancer.
- 140 priority levels (0–99 real-time, 100–139 nice-based).
- SCHED_FIFO, SCHED_RR, SCHED_DEADLINE, SCHED_OTHER policies.
- NUMA-aware: prefers to schedule tasks near their memory.
- CPU bandwidth throttling via cgroups.
- Tickless (NO_HZ) idle and full-system NO_HZ for latency-sensitive workloads.
- Preemptible kernel (CONFIG_PREEMPT) — can preempt even inside kernel code.

Our approach implements the essential concepts (per-CPU queues, priorities, load
balancing, affinity) without CFS's virtual runtime model or cgroup integration.

## Deferred Until Later

- CFS / virtual runtime scheduling
- NUMA-aware scheduling
- CPU bandwidth throttling (cgroups)
- Tickless idle (NO_HZ)
- Kernel preemption
- SCHED_DEADLINE (earliest deadline first)
- CPU hotplug
- Power-aware scheduling (race-to-idle, frequency scaling)
