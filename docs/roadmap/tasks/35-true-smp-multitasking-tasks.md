# Phase 35 — True SMP Multitasking: Task List

**Status:** Complete
**Source Ref:** phase-35
**Depends on:** Phase 25 (SMP) ✅, Phase 33 (Kernel Memory) ✅
**Builds on:** Extends Phase 25 SMP bring-up by replacing the BSP-only dispatch path and shared syscall statics with per-core syscall state, per-core run queues, and scheduler-aware task metadata.
**Goal:** All CPU cores dispatch and run user tasks. The BSP-only restriction is
removed by making syscall infrastructure per-core. The scheduler uses per-CPU run
queues with load balancing, and tasks have priority levels.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Per-core syscall infrastructure | — | ✅ Done |
| B | Multi-core task dispatch | A | ✅ Done |
| C | Per-CPU run queues | B | ✅ Done |
| D | Priority scheduling | C | ✅ Done |
| E | Load balancing | C | ✅ Done (runtime hook follow-up noted below) |
| F | CPU affinity syscalls | C | ✅ Done |
| G | Wait queues | C | ✅ Done (`G.1`, `G.4`; `G.2`/`G.3` deferred) |
| H | Time accounting | B | ✅ Done (`times()` child accounting follow-up deferred) |
| I | Integration testing and documentation | All | ✅ Done |

---

## Track A — Per-Core Syscall Infrastructure

The Phase 25 SMP boot work still left syscall entry using shared global state. This
track replaces those shared statics with per-core storage reached through `gs_base`.

### A.1 — Add syscall fields to `PerCoreData`
**File:** `kernel/src/smp/mod.rs`
**Symbol:** `PerCoreData`, `offsets::SYSCALL_STACK_TOP`
**Why it matters:** Per-core syscall save slots are the prerequisite for letting more than one core enter the syscall path safely at the same time.
**Acceptance:**
- [x] All new fields have known offsets within the struct (`#[repr(C)]` plus `offset_of!` constants in `offsets`)
- [x] `PerCoreData` initialization sets `syscall_stack_top` to the core's allocated kernel stack
- [x] `cargo xtask check` passes

### A.2 — Add `FORK_ENTRY_CTX` to `PerCoreData`
**Files:**
- `kernel/src/smp/mod.rs`
- `kernel/src/arch/x86_64/mod.rs`
**Symbol:** `PerCoreData::fork_entry_ctx`, `ForkEntryCtx`
**Why it matters:** Moving fork restore state into per-core data prevents one core's `fork()` child setup from clobbering another core's saved userspace context.
**Acceptance:**
- [x] `FORK_ENTRY_CTX` global removed in favor of `PerCoreData::fork_entry_ctx`
- [x] Fork entry reads and writes the per-core field via the `gs_base`-backed per-core pointer
- [x] Single-core boot still works correctly

### A.3 — Rewrite `syscall_entry` assembly to use `gs_base` offsets
**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `syscall_entry`
**Why it matters:** This is the core replacement step that swaps the earlier shared syscall statics for per-core register save slots.
**Acceptance:**
- [x] No `static mut` globals remain for syscall user state
- [x] Syscall entry and return agree with the chosen gs_base/PerCoreData model (no swapgs required in the current implementation)
- [x] BSP still handles syscalls correctly as a single-core regression check

### A.4 — Verify dual-core syscall safety
**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `syscall_entry`
**Why it matters:** The per-core syscall conversion only matters if simultaneous syscalls on different cores stop corrupting each other's saved register state.
**Acceptance:**
- [x] Boot with 2+ cores in QEMU (`-smp 2`)
- [x] Two processes making syscalls simultaneously on different cores do not corrupt each other's register state
- [x] Serial log shows syscalls handled on core 0 and core 1

---

## Track B — Multi-Core Task Dispatch

### B.1 — Remove BSP-only dispatch guard
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `pick_next`
**Why it matters:** This is the Phase 35 scheduler pivot that replaces the earlier Phase 25 BSP-only dispatch rule with all cores participating in task selection.
**Acceptance:**
- [x] APs dispatch non-idle `Ready` tasks
- [x] Idle tasks still used as fallback per core
- [x] No task is simultaneously dispatched on two cores (state set to `Running` atomically under the scheduler lock)

### B.2 — Add per-core task logging
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `run`
**Why it matters:** Per-core scheduler visibility is the easiest way to prove that work really moved beyond the BSP-only path.
**Acceptance:**
- [x] Serial output shows tasks running on core 0 and core 1+
- [x] Can be disabled by log level to avoid noise in production

### B.3 — Verify multi-process workload
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `spawn`
**Why it matters:** A multi-process workload confirms that the new dispatch path works under load instead of only in single-task bring-up.
**Acceptance:**
- [x] Spawn 4+ processes on a 2-core QEMU and observe both cores running userspace tasks
- [x] All processes complete successfully
- [x] No deadlocks or panics

---

## Track C — Per-CPU Run Queues

### C.1 — Add per-core run queue to `PerCoreData`
**Files:**
- `kernel/src/smp/mod.rs`
- `kernel/src/task/scheduler.rs`
**Symbol:** `PerCoreData::run_queue`, `enqueue_to_core`
**Why it matters:** Local run queues replace the earlier shared ready-queue scan with scheduler state that scales naturally across cores.
**Acceptance:**
- [x] Each core has its own run queue
- [x] Queue operations do not require the global `SCHEDULER` lock

### C.2 — Implement task assignment on spawn
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `spawn`
**Why it matters:** New tasks need an explicit home core once scheduling stops being BSP-only.
**Acceptance:**
- [x] New tasks are distributed across cores
- [x] `yield_now()` re-enqueues to the local core's queue
- [x] `wake_task()` enqueues to the target task's assigned core

### C.3 — Update `pick_next()` to use local run queue
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `pick_next`
**Why it matters:** This replaces the earlier global task-list scan with an O(1)-style local dequeue path that keeps cache-warm tasks on the same core.
**Acceptance:**
- [x] `pick_next()` dequeues from the local queue and only requeues skipped work locally
- [x] Global scheduler locking is reserved for cross-core operations such as spawn or migration
- [x] Cache-warm tasks stay on the same core

### C.4 — Handle task exit and cleanup across cores
**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `mark_current_dead`, `sys_waitpid`
**Why it matters:** Cross-core scheduling is incomplete if task teardown or parent waiting still assumes that work only ran on the BSP.
**Acceptance:**
- [x] Task exit on any core cleans up correctly
- [x] `wait()` / `waitpid()` works for tasks that ran on a different core than the parent
- [x] No leaked task entries in any core's queue

---

## Track D — Priority Scheduling

### D.1 — Add priority field to `Task` struct
**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task`
**Why it matters:** Priority metadata is the new scheduler input that Phase 25 never had.
**Acceptance:**
- [x] All existing tasks default to priority 20 (normal, middle)
- [x] Priority stored and accessible via `Task`
- [x] Idle tasks created with priority 30

### D.2 — Update `pick_next()` to respect priorities
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `pick_next`
**Why it matters:** Priority-aware selection is what turns the per-core queues into a real policy upgrade instead of only a data-structure rewrite.
**Acceptance:**
- [x] A priority-0 task always runs before a priority-20 task on the same core
- [x] Equal-priority tasks are scheduled round-robin
- [x] Normal tasks still run when higher-priority tasks yield

### D.3 — Implement `nice()` / `setpriority()` syscall
**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_nice`
**Why it matters:** Userspace needs a syscall-facing way to exercise the new scheduler priority model introduced in this phase.
**Acceptance:**
- [x] `nice(increment)` adjusts the current task's priority
- [x] Non-root cannot set priority below 10
- [x] Priority changes take effect on the next scheduling decision

---

## Track E — Load Balancing

### E.1 — Track per-core queue lengths
**Files:**
- `kernel/src/smp/mod.rs`
- `kernel/src/task/scheduler.rs`
**Symbol:** `PerCoreData::run_queue`, `least_loaded_core`
**Why it matters:** Load balancing extends the per-core queue design by measuring queue depth per core instead of relying on the earlier shared scheduler state.
**Acceptance:**
- [x] Queue-length snapshots are used when assigning new work and when evaluating imbalance
- [x] The implementation derives lengths from `run_queue.lock().len()` instead of a separate `queue_length` field
- [ ] Deferred — a standalone `queue_length: AtomicU32` counter from the original plan has not been added

### E.2 — Implement periodic load balancer
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `maybe_load_balance`
**Why it matters:** Periodic migration is the scheduler feature that keeps one core from going idle while another stays overloaded.
**Acceptance:**
- [x] `maybe_load_balance()` implements queue comparison and one-task migration logic
- [x] Migrated tasks update `assigned_core` and re-enter the destination core's queue
- [ ] Deferred — the scheduler loop currently leaves the `maybe_load_balance()` hook commented out pending the follow-up noted in code

### E.3 — Prevent migration of affinity-pinned tasks
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `maybe_load_balance`
**Why it matters:** Load balancing has to extend the new affinity rules instead of overriding them.
**Acceptance:**
- [x] Candidate migration checks respect each task's `affinity_mask`
- [x] Only migratable tasks are considered for movement to the destination core

---

## Track F — CPU Affinity Syscalls

### F.1 — Add affinity mask to `Task` / `Process`
**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::affinity_mask`
**Why it matters:** Affinity metadata lets later scheduler decisions express “may run here” independently from the task's current assigned core.
**Acceptance:**
- [x] Default mask allows all cores
- [x] Mask persists across fork so children inherit parent affinity

### F.2 — Implement `sched_setaffinity` / `sched_getaffinity` syscalls
**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_sched_setaffinity`, `sys_sched_getaffinity`
**Why it matters:** These syscalls expose the new per-core scheduler policy to userspace without reintroducing BSP-only assumptions.
**Acceptance:**
- [x] Setting affinity to a single core pins the task to that core
- [x] If the task is currently assigned to a disallowed core, it is reassigned on the next scheduling cycle
- [x] Invalid masks (no bits set, or bits for non-existent cores) return `-EINVAL`
- [x] `pid == 0` means current task

---

## Track G — Wait Queues

### G.1 — Implement `WaitQueue` primitive
**File:** `kernel/src/task/wait_queue.rs`
**Symbol:** `WaitQueue`
**Why it matters:** `WaitQueue` is the reusable sleep/wake primitive that later tracks can plug into pipes, IPC, and blocking locks.
**Acceptance:**
- [x] `sleep()` queues the current task and blocks it
- [x] `wake_one()` wakes the first waiter
- [x] `wake_all()` wakes all waiters
- [x] The primitive exists as a reusable scheduler-facing abstraction

### G.2 — Attach wait queues to pipes
**File:** `kernel/src/pipe.rs`
**Symbol:** `pipe_read`, `pipe_write`
**Why it matters:** This follow-up would replace the earlier Phase 14 pipe would-block path with an explicit per-resource wait queue.
**Acceptance:**
- [ ] Deferred — `pipe_read()` and `pipe_write()` still use the older would-block return path rather than `WaitQueue`
- [ ] Deferred — pipe sleep/wake integration with `WaitQueue` remains future work
- [ ] Deferred — cross-core pipe wake behavior will be validated after that replacement lands

### G.3 — Attach wait queues to IPC endpoints
**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `call_msg`, `reply_recv`
**Why it matters:** This follow-up would replace the existing endpoint sender/receiver blocking path with the generic wait-queue primitive.
**Acceptance:**
- [ ] Deferred — IPC `call()`/`reply_recv()` still use endpoint-local sender/receiver queues plus scheduler block helpers
- [ ] Deferred — userspace IPC behavior is intentionally unchanged until the `WaitQueue` swap happens
- [ ] Deferred — endpoint-specific `WaitQueue` wiring remains future work

### G.4 — Implement blocking mutex using wait queues
**File:** `kernel/src/task/blocking_mutex.rs`
**Symbol:** `BlockingMutex`
**Why it matters:** `BlockingMutex` proves the new wait-queue primitive can replace long-held spinlocks without touching interrupt-safe short critical sections.
**Acceptance:**
- [x] Contended lock puts the caller to sleep instead of spinning
- [x] Lock release wakes one waiter
- [x] The type is suitable for long-held filesystem or network locks
- [x] Interrupt handlers and the scheduler still keep spin-based locking

---

## Track H — Time Accounting

### H.1 — Add time fields to `Task` / `Process`
**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task`
**Why it matters:** Per-task time accounting extends the scheduler with measurable CPU usage instead of only runnable state.
**Acceptance:**
- [x] `user_ticks`, `system_ticks`, and `start_tick` are initialized on task creation
- [x] `start_tick` updates when a task is dispatched

### H.2 — Accumulate time on context switch and syscall entry/exit
**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `current_task_times`
**Why it matters:** Raw accounting fields only become meaningful once context-switch and syscall paths feed them with elapsed tick data.
**Acceptance:**
- [x] `user_ticks` increases for processes running in ring 3
- [x] `system_ticks` increases during syscall handling
- [x] Idle time is kept separate from user-task accounting

### H.3 — Implement `times()` syscall
**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/task/scheduler.rs`
**Symbol:** `sys_times`
**Why it matters:** `times()` is the userspace-facing API for the new CPU accounting data added in this phase.
**Acceptance:**
- [x] Returns nonzero `tms_utime` and `tms_stime` for a process that has done work
- [x] Return value is clock ticks since boot
- [ ] Deferred — child `tms_cutime` / `tms_cstime` accumulation is still stubbed as zero in the current implementation

---

## Track I — Integration Testing and Documentation

### I.1 — Multi-core stress test
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `run`
**Why it matters:** Scheduler stress testing is where the earlier BSP-only design either proves it is gone or resurfaces as cross-core corruption.
**Acceptance:**
- [x] Boot with `-smp 4` and spawn 8+ processes
- [x] All processes complete without corruption, deadlock, or panic
- [x] Load is visibly distributed across multiple cores

### I.2 — Run full existing test suite
**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `pick_next`
**Why it matters:** Phase 35 changes some of the riskiest kernel paths, so full-regression validation is mandatory even though the phase is primarily architectural.
**Acceptance:**
- [x] All existing QEMU tests pass
- [x] All kernel-core host tests pass
- [x] `cargo xtask check` clean (no warnings)

### I.3 — Update documentation
**Files:**
- `docs/roadmap/35-true-smp-multitasking.md`
- `docs/roadmap/tasks/35-true-smp-multitasking-tasks.md`
- `docs/25-smp.md`
**Symbol:** `Phase 35 - True SMP Multitasking`, `Phase 35 — True SMP Multitasking: Task List`, `Phase 25 - Symmetric Multiprocessing (SMP)`
**Why it matters:** The roadmap docs need to explain exactly where Phase 35 replaced Phase 25's BSP-only behavior and where deferred follow-ups still remain.
**Acceptance:**
- [x] Phase 35 design doc exists at `docs/roadmap/35-true-smp-multitasking.md`
- [x] Task list updated with completion status and deferred follow-ups
- [x] `docs/25-smp.md` updated with per-core syscall and scheduling details
