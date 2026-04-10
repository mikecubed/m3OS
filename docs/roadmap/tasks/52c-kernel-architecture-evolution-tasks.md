# Phase 52c — Kernel Architecture Evolution: Task List

**Status:** Planned
**Source Ref:** phase-52c
**Depends on:** Phase 52b (Kernel Structural Hardening)
**Goal:** Larger-scale architecture improvements that improve scalability, remove resource limits, and unify duplicated subsystems.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Per-core scheduler with work-stealing | None | Planned |
| B | Dynamic IPC resource pools | None | Planned |
| C | Unified kernel-side line discipline | None | Planned |
| D | VMA tree structure | 52b Track A (AddressSpace) | Planned |
| E | ISR-direct notification wakeup | A (per-core scheduler) | Planned |

---

## Track A — Per-Core Scheduler with Work-Stealing

### A.1 — Define PerCoreScheduler and TaskRegistry

**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/task/mod.rs`

**Symbol:** `PerCoreScheduler`, `TaskRegistry`
**Why it matters:** The current global `SCHEDULER: Mutex<Scheduler>` is a contention bottleneck on multi-core systems. All scheduler operations (spawn, wake, pick_next, block) acquire this lock.

**Acceptance:**
- [ ] `PerCoreScheduler` has `local_queue: VecDeque<TaskHandle>` and `steal_queue: Mutex<VecDeque<TaskHandle>>`
- [ ] `TaskRegistry` holds the global `Vec<Task>` (spawn/exit only, not dispatch hot path)
- [ ] `pick_next` checks local queue, then steal queue, then steals from other cores
- [ ] The dispatch hot path does not acquire any global lock

### A.2 — Implement work-stealing

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `PerCoreScheduler::steal_one`, `PerCoreScheduler::pick_next`
**Why it matters:** Without work-stealing, idle cores cannot take work from busy cores. Load imbalance leads to poor CPU utilization.

**Acceptance:**
- [ ] `steal_one()` transfers tasks from local queue to steal queue, then pops one
- [ ] Stealing prefers same-cluster cores (if topology data available) before cross-cluster
- [ ] A workload with 8 tasks on 4 cores distributes approximately evenly

### A.3 — Generation-based task slot reuse

**File:** `kernel/src/task/mod.rs`
**Symbol:** `TaskHandle`, `TaskRegistry`
**Why it matters:** The current `SCHEDULER.tasks` vec only grows. Dead tasks keep their index. Over time, the vec becomes large. Generation-based reuse prevents ABA races on recycled slots.

**Acceptance:**
- [ ] `TaskHandle` encodes `(index: u32, generation: u32)`
- [ ] Stale handles with wrong generation are rejected
- [ ] Dead task slots are returned to a free list for reuse

### A.4 — Re-enable load balancing with per-task cooldown

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `maybe_load_balance`
**Why it matters:** Load balancing was disabled due to migration thrashing. With per-core queues and a migration cooldown timer per task, thrashing is prevented.

**Acceptance:**
- [ ] Each task has a `last_migrated_tick` field
- [ ] Load balancing skips tasks that migrated within the last N ticks (cooldown)
- [ ] Load balancing is re-enabled in the scheduler loop
- [ ] Task migration thrashing does not occur with short-lived userspace processes

---

## Track B — Dynamic IPC Resource Pools

### B.1 — Growable endpoint pool

**File:** `kernel/src/ipc/endpoint.rs`
**Symbol:** `EndpointPool`
**Why it matters:** `MAX_ENDPOINTS = 16` limits the number of concurrent services. Phase 54 (Deep Serverization) will need many more endpoints.

**Acceptance:**
- [ ] Endpoints are allocated from a slab-backed pool that grows in 16-slot chunks
- [ ] Freed endpoint IDs are recycled via a free list
- [ ] Creating 32 endpoints succeeds (previously limited to 16)

### B.2 — Growable capability table

**File:** `kernel-core/src/ipc/capability.rs`
**Symbol:** `CapabilityTable`
**Why it matters:** Fixed 64-slot capability table limits processes that need many handles. A growable `Vec` removes this limit.

**Acceptance:**
- [ ] `CapabilityTable` uses `Vec<Option<Capability>>` instead of fixed array
- [ ] Initial size is 64 (matches current), grows in chunks of 64
- [ ] A process can hold 128+ capabilities without exhaustion

### B.3 — Growable notification and service registry pools

**Files:**
- `kernel/src/ipc/notification.rs`
- `kernel-core/src/ipc/registry.rs`

**Symbol:** `NotificationPool`, `Registry`
**Why it matters:** `MAX_NOTIFS = 16` and `MAX_SERVICES = 16` limit the number of IRQ-driven services and named services.

**Acceptance:**
- [ ] Notification pool grows dynamically (slab or Vec)
- [ ] Service registry grows dynamically
- [ ] Creating 32 notifications and 32 services succeeds

---

## Track C — Unified Kernel-Side Line Discipline

### C.1 — Create LineDiscipline module in kernel-core

**File:** `kernel-core/src/tty.rs`
**Symbol:** `LineDiscipline`
**Why it matters:** Line discipline logic (ICRNL, ISIG, canonical editing, echo) is duplicated between `serial_stdin_feeder_task` (kernel) and `stdin_feeder` (userspace). A single kernel-side implementation eliminates duplication and avoids the `copy_to_user` workaround.

**Acceptance:**
- [ ] `LineDiscipline::process_byte(byte) -> LdiscResult` handles iflag transforms, signal generation, canonical editing, echo
- [ ] Unit tests in `kernel-core` verify ICRNL, ISIG, VERASE, VKILL, VWERASE, VEOF behavior
- [ ] `LdiscResult` enum covers `Consumed`, `Signal(sig)`, `Pushed`, `Echo(bytes)`

### C.2 — Refactor serial_stdin_feeder to use LineDiscipline

**File:** `kernel/src/main.rs`
**Symbol:** `serial_stdin_feeder_task`
**Why it matters:** The serial feeder currently has its own inline line discipline implementation. Refactoring it to use the shared `LineDiscipline` validates the module design.

**Acceptance:**
- [ ] `serial_stdin_feeder_task` calls `ldisc.process_byte(byte)` instead of inline iflag/isig/canonical logic
- [ ] Serial input behavior is unchanged (ICRNL, echo, signal generation all work)

### C.3 — Add `push_raw_input` syscall and simplify stdin_feeder

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `userspace/stdin_feeder/src/main.rs`

**Symbol:** `PUSH_RAW_INPUT` syscall, `stdin_feeder::main`
**Why it matters:** The userspace `stdin_feeder` currently implements its own line discipline and uses register-return workaround syscalls to avoid the `copy_to_user` bug. With a kernel-side `push_raw_input` syscall, the feeder only needs to decode scancodes to ASCII and push raw bytes — the kernel handles line discipline.

**Acceptance:**
- [ ] `PUSH_RAW_INPUT` syscall accepts a byte and calls `LineDiscipline::process_byte`
- [ ] `stdin_feeder` no longer reads termios flags (no `GET_TERMIOS_LFLAG/IFLAG/OFLAG` calls)
- [ ] `stdin_feeder` no longer implements ISIG, ICANON, echo, or ICRNL logic
- [ ] `stdin_feeder` only decodes scancodes to ASCII and calls `push_raw_input`
- [ ] Keyboard input works identically to before (canonical editing, signals, echo)

---

## Track D — VMA Tree Structure

### D.1 — Replace Vec<MemoryMapping> with BTreeMap

**File:** `kernel/src/mm/mod.rs` (AddressSpace from Phase 52b)
**Symbol:** `VmaTree`
**Why it matters:** `find_vma` scans `Vec<MemoryMapping>` linearly in the page fault handler. With many mappings, this becomes a performance bottleneck.

**Acceptance:**
- [ ] `AddressSpace.vmas` uses `BTreeMap<u64, MemoryMapping>` keyed by start address
- [ ] `find_containing(addr)` uses `range(..=addr).next_back()` for O(log n) lookup
- [ ] `munmap` range removal correctly splits partially overlapping VMAs
- [ ] `mprotect` range update correctly splits VMAs at boundaries
- [ ] Page fault handler performance with 100+ mappings is measurably improved

---

## Track E — ISR-Direct Notification Wakeup

### E.1 — Add per-core IsrWakeQueue

**File:** `kernel/src/smp/mod.rs`
**Symbol:** `IsrWakeQueue`
**Why it matters:** ISR-delivered notifications currently wait up to 10ms for `drain_pending_waiters()` on the BSP tick. A per-core lock-free queue allows the scheduler to process ISR wakeups immediately.

**Acceptance:**
- [ ] `IsrWakeQueue` is a 32-entry ring buffer of `AtomicU64` task IDs
- [ ] `push(task_id)` is lock-free and ISR-safe (no scheduler lock)
- [ ] `drain()` returns an iterator of pending task IDs
- [ ] Queue full returns `false` (no panic from ISR context)

### E.2 — Modify signal_irq to use IsrWakeQueue

**File:** `kernel/src/ipc/notification.rs`
**Symbol:** `signal_irq`
**Why it matters:** Instead of just setting `PENDING` bits and waiting for the tick, `signal_irq` pushes the blocked waiter's task ID to the local core's `IsrWakeQueue`.

**Acceptance:**
- [ ] `signal_irq` checks `WAITERS[idx]` (read-only, no lock) for a registered waiter
- [ ] If waiter exists, pushes waiter ID to current core's `IsrWakeQueue`
- [ ] `signal_reschedule()` still called to interrupt the scheduler's HLT

### E.3 — Drain IsrWakeQueue in scheduler loop

**File:** `kernel/src/task/mod.rs` or `kernel/src/task/scheduler.rs`
**Symbol:** `run` (scheduler loop)
**Why it matters:** The scheduler must process ISR wakeup requests on every iteration, not just on timer ticks.

**Acceptance:**
- [ ] Scheduler loop drains `IsrWakeQueue` before `pick_next`
- [ ] For each drained task ID, calls `wake_task(id)` under the per-core scheduler lock
- [ ] `drain_pending_waiters()` tick-based path is removed or demoted to a safety fallback
- [ ] Keyboard IRQ to kbd_server wakeup latency is under 1ms

---

## Documentation Notes

- All design details and external comparisons are in `docs/appendix/architecture/next/`
- Track A corresponds to `docs/appendix/architecture/next/04-scheduler-smp.md` Section 1
- Track B corresponds to `docs/appendix/architecture/next/03-ipc-and-wakeups.md` Section 2
- Track C corresponds to `docs/appendix/architecture/next/05-terminal-pty.md` Section 1
- Track D corresponds to `docs/appendix/architecture/next/01-memory-management.md` Section 4
- Track E corresponds to `docs/appendix/architecture/next/03-ipc-and-wakeups.md` Section 3
- External kernel comparisons with verified source references are in `docs/appendix/architecture/next/sources.md`
