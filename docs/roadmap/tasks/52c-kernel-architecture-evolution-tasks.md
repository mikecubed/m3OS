# Phase 52c — Kernel Architecture Evolution: Task List

**Status:** Complete
**Source Ref:** phase-52c
**Depends on:** Phase 52b (Kernel Structural Hardening) ✅
**Goal:** Larger-scale architecture improvements that improve scalability, remove resource limits, and unify duplicated subsystems.

> **Post-phase audit note (Phase 52d Track A + Track D):** The current
> implementation still has open items: Track A.1 acceptance item "dispatch hot
> path does not acquire any global lock" is unchecked — the global `SCHEDULER`
> lock is acquired on every dispatch iteration; Track B.3 notification pool
> remains fixed-size (`MAX_NOTIFS = 64`) for ISR safety rather than dynamically
> growable; Track C.3 `stdin_feeder` still duplicates line discipline in
> userspace.  See Phase 52d Track D for the reconciliation rationale and
> Phase 52d Track C for the keyboard convergence plan.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Per-core scheduler with work-stealing | None | ✅ Done |
| B | Dynamic IPC resource pools | None | ✅ Done |
| C | Unified kernel-side line discipline | None | ✅ Done |
| D | VMA tree structure | 52b Track A (AddressSpace) | ✅ Done |
| E | ISR-direct notification wakeup | A (per-core scheduler) | ✅ Done |

---

## Track A — Per-Core Scheduler with Work-Stealing

### A.1 — Define PerCoreScheduler and TaskRegistry

**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/task/mod.rs`

**Symbol:** `PerCoreScheduler`, `TaskRegistry`
**Why it matters:** The current global `SCHEDULER: Mutex<Scheduler>` is a contention bottleneck on multi-core systems. All scheduler operations (spawn, wake, pick_next, block) acquire this lock.

**Acceptance:**
- [x] `PerCoreScheduler` has `local_queue: VecDeque<TaskHandle>` and `steal_queue: Mutex<VecDeque<TaskHandle>>`
- [x] `TaskRegistry` holds the global `Vec<Task>` (spawn/exit only, not dispatch hot path)
- [x] `pick_next` checks local queue, then steal queue, then steals from other cores
- [ ] The dispatch hot path does not acquire any global lock *(deferred — per-core run queues landed but the global `SCHEDULER` lock is still acquired on each dispatch iteration for task state reads and transitions; see Phase 52d Track D)*

### A.2 — Implement work-stealing

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `PerCoreScheduler::steal_one`, `PerCoreScheduler::pick_next`
**Why it matters:** Without work-stealing, idle cores cannot take work from busy cores. Load imbalance leads to poor CPU utilization.

**Acceptance:**
- [x] `steal_one()` transfers tasks from local queue to steal queue, then pops one
- [x] Stealing prefers same-cluster cores (if topology data available) before cross-cluster
- [x] A workload with 8 tasks on 4 cores distributes approximately evenly

### A.3 — Generation-based task slot reuse

**File:** `kernel/src/task/mod.rs`
**Symbol:** `TaskHandle`, `TaskRegistry`
**Why it matters:** The current `SCHEDULER.tasks` vec only grows. Dead tasks keep their index. Over time, the vec becomes large. Generation-based reuse prevents ABA races on recycled slots.

**Acceptance:**
- [x] `TaskHandle` encodes `(index: u32, generation: u32)`
- [x] Stale handles with wrong generation are rejected
- [x] Dead task slots are returned to a free list for reuse

### A.4 — Re-enable load balancing with per-task cooldown

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `maybe_load_balance`
**Why it matters:** Load balancing was disabled due to migration thrashing. With per-core queues and a migration cooldown timer per task, thrashing is prevented.

**Acceptance:**
- [x] Each task has a `last_migrated_tick` field
- [x] Load balancing skips tasks that migrated within the last N ticks (cooldown)
- [x] Load balancing is re-enabled in the scheduler loop
- [x] Task migration thrashing does not occur with short-lived userspace processes

---

## Track B — Dynamic IPC Resource Pools

### B.1 — Growable endpoint pool

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `EndpointPool`
**Why it matters:** `MAX_ENDPOINTS = 16` limits the number of concurrent services. Phase 54 (Deep Serverization) will need many more endpoints.

**Acceptance:**
- [x] Endpoints are allocated from a slab-backed pool that grows in 16-slot chunks
- [x] Freed endpoint IDs are recycled via a free list
- [x] Creating 32 endpoints succeeds (previously limited to 16)

### B.2 — Growable capability table

**File:** `kernel-core/src/ipc/capability.rs`
**Symbol:** `CapabilityTable`
**Why it matters:** Fixed 64-slot capability table limits processes that need many handles. A growable `Vec` removes this limit.

**Acceptance:**
- [x] `CapabilityTable` uses `Vec<Option<Capability>>` instead of fixed array
- [x] Initial size is 64 (matches current), grows in chunks of 64
- [x] A process can hold 128+ capabilities without exhaustion

### B.3 — Growable notification and service registry pools

**Files:**
- `kernel/src/ipc/notification.rs`
- `kernel-core/src/ipc/registry.rs`

**Symbol:** `NotificationPool`, `Registry`
**Why it matters:** `MAX_NOTIFS = 16` and `MAX_SERVICES = 16` limit the number of IRQ-driven services and named services.

**Acceptance:**
- [ ] Notification pool grows dynamically (slab or Vec) *(deferred — fixed-size `MAX_NOTIFS = 64` retained because ISR-safe access requires lock-free fixed-size arrays; exhaustion diagnostics added in Phase 52d Track D; see `kernel/src/ipc/notification.rs` module doc for full rationale)*
- [x] Service registry grows dynamically
- [ ] Creating 32 notifications and 32 services succeeds *(32 notifications succeed — the pool is 64 slots — but growth is not dynamic; the acceptance criterion as written implied dynamic growth)*

---

## Track C — Unified Kernel-Side Line Discipline

### C.1 — Create LineDiscipline module in kernel-core

**File:** `kernel-core/src/tty.rs`
**Symbol:** `LineDiscipline`
**Why it matters:** Line discipline logic (ICRNL, ISIG, canonical editing, echo) is duplicated between `serial_stdin_feeder_task` (kernel) and `stdin_feeder` (userspace). A single kernel-side implementation eliminates duplication and avoids the `copy_to_user` workaround.

**Acceptance:**
- [x] `LineDiscipline::process_byte(byte) -> LdiscResult` handles iflag transforms, signal generation, canonical editing, echo
- [x] Unit tests in `kernel-core` verify ICRNL, ISIG, VERASE, VKILL, VWERASE, VEOF behavior
- [x] `LdiscResult` enum covers `Consumed`, `Signal(sig)`, `Pushed`, `Echo(bytes)`

### C.2 — Refactor serial_stdin_feeder to use LineDiscipline

**File:** `kernel/src/main.rs`
**Symbol:** `serial_stdin_feeder_task`
**Why it matters:** The serial feeder currently has its own inline line discipline implementation. Refactoring it to use the shared `LineDiscipline` validates the module design.

**Acceptance:**
- [x] `serial_stdin_feeder_task` calls `ldisc.process_byte(byte)` instead of inline iflag/isig/canonical logic
- [x] Serial input behavior is unchanged (ICRNL, echo, signal generation all work)

### C.3 — Add `push_raw_input` syscall and simplify stdin_feeder

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `userspace/stdin_feeder/src/main.rs`

**Symbol:** `PUSH_RAW_INPUT` syscall, `stdin_feeder::main`
**Why it matters:** The userspace `stdin_feeder` currently implements its own line discipline and uses register-return workaround syscalls to avoid the `copy_to_user` bug. With a kernel-side `push_raw_input` syscall, the feeder only needs to decode scancodes to ASCII and push raw bytes — the kernel handles line discipline.

**Acceptance:**
- [x] `PUSH_RAW_INPUT` syscall accepts a byte and calls `LineDiscipline::process_byte`
- [x] `stdin_feeder` no longer reads termios flags (no `GET_TERMIOS_LFLAG/IFLAG/OFLAG` calls)
- [x] `stdin_feeder` no longer implements ISIG, ICANON, echo, or ICRNL logic
- [x] `stdin_feeder` only decodes scancodes to ASCII and calls `push_raw_input`
- [x] Keyboard input works identically to before (canonical editing, signals, echo)

---

## Track D — VMA Tree Structure

### D.1 — Replace Vec<MemoryMapping> with BTreeMap

**File:** `kernel/src/mm/mod.rs` (AddressSpace from Phase 52b)
**Symbol:** `VmaTree`
**Why it matters:** `find_vma` scans `Vec<MemoryMapping>` linearly in the page fault handler. With many mappings, this becomes a performance bottleneck.

**Acceptance:**
- [x] `AddressSpace.vmas` uses `BTreeMap<u64, MemoryMapping>` keyed by start address
- [x] `find_containing(addr)` uses `range(..=addr).next_back()` for O(log n) lookup
- [x] `munmap` range removal correctly splits partially overlapping VMAs
- [x] `mprotect` range update correctly splits VMAs at boundaries
- [x] Page fault handler performance with 100+ mappings is measurably improved

---

## Track E — ISR-Direct Notification Wakeup

### E.1 — Add per-core IsrWakeQueue

**File:** `kernel/src/smp/mod.rs`
**Symbol:** `IsrWakeQueue`
**Why it matters:** ISR-delivered notifications currently wait up to 10ms for `drain_pending_waiters()` on the BSP tick. A per-core lock-free queue allows the scheduler to process ISR wakeups immediately.

**Acceptance:**
- [x] `IsrWakeQueue` is a 32-entry ring buffer of `AtomicU64` task IDs
- [x] `push(task_id)` is lock-free and ISR-safe (no scheduler lock)
- [x] `drain()` returns an iterator of pending task IDs
- [x] Queue full returns `false` (no panic from ISR context)

### E.2 — Modify signal_irq to use IsrWakeQueue

**File:** `kernel/src/ipc/notification.rs`
**Symbol:** `signal_irq`
**Why it matters:** Instead of just setting `PENDING` bits and waiting for the tick, `signal_irq` pushes the blocked waiter's task ID to the local core's `IsrWakeQueue`.

**Acceptance:**
- [x] `signal_irq` checks `WAITERS[idx]` (read-only, no lock) for a registered waiter
- [x] If waiter exists, pushes waiter ID to current core's `IsrWakeQueue`
- [x] `signal_reschedule()` still called to interrupt the scheduler's HLT

### E.3 — Drain IsrWakeQueue in scheduler loop

**File:** `kernel/src/task/mod.rs` or `kernel/src/task/scheduler.rs`
**Symbol:** `run` (scheduler loop)
**Why it matters:** The scheduler must process ISR wakeup requests on every iteration, not just on timer ticks.

**Acceptance:**
- [x] Scheduler loop drains `IsrWakeQueue` before `pick_next`
- [x] For each drained task ID, calls `wake_task(id)` under the global `SCHEDULER` lock *(originally stated "per-core scheduler lock" — corrected to match the actual implementation which uses the global lock)*
- [x] `drain_pending_waiters()` tick-based path is removed or demoted to a safety fallback
- [x] Keyboard IRQ to kbd_server wakeup latency is under 1ms

---

## Documentation Notes

- All design details and external comparisons are in `docs/appendix/architecture/next/`
- Track A corresponds to `docs/appendix/architecture/next/04-scheduler-smp.md` Section 1
- Track B corresponds to `docs/appendix/architecture/next/03-ipc-and-wakeups.md` Section 2
- Track C corresponds to `docs/appendix/architecture/next/05-terminal-pty.md` Section 1
- Track D corresponds to `docs/appendix/architecture/next/01-memory-management.md` Section 4
- Track E corresponds to `docs/appendix/architecture/next/03-ipc-and-wakeups.md` Section 3
- External kernel comparisons with verified source references are in `docs/appendix/architecture/next/sources.md`
