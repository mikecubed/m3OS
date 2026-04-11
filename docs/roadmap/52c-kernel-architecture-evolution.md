# Phase 52c - Kernel Architecture Evolution

**Status:** Complete
**Source Ref:** phase-52c
**Depends on:** Phase 52b (Kernel Structural Hardening) ✅, Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅
**Builds on:** Extends the Phase 52b structural hardening with larger-scale architecture improvements that improve scalability, reduce resource limits, and unify duplicated subsystems
**Primary Components:** kernel/src/task/mod.rs, kernel/src/task/scheduler.rs, kernel/src/ipc/endpoint.rs, kernel/src/ipc/notification.rs, kernel/src/tty.rs, kernel/src/pty.rs, kernel/src/process/mod.rs, kernel/src/main.rs, userspace/stdin_feeder

## Milestone Goal

Phase 52c established the architecture direction for per-core scheduling,
growable IPC resources, a unified kernel-side line discipline, O(log n) VMA
lookup, and ISR wakeup improvements. In the checked-in tree, the VMA tree,
growable endpoint/capability tables, `LineDiscipline`, `push_raw_input`, and the
ISR wake queue landed, but the keyboard path still duplicates policy in
userspace, the scheduler hot path still relies on the global `SCHEDULER` lock,
and notifications remain fixed-size for ISR safety.

## Post-Phase Audit Note

Phase 52c landed several important pieces — `LineDiscipline`, `push_raw_input`,
the VMA tree, the ISR wake queue, and growable endpoint/capability tables — but
the post-phase audit found that the checked-in code still diverges from part of
this phase's completion story in three areas:

1. **Keyboard input path (partial):** `userspace/stdin_feeder` still reads
   termios flags and implements `ICANON`, `ISIG`, echo, and canonical editing
   itself instead of forwarding raw scancodes via `push_raw_input`. The kernel
   `LineDiscipline` exists but is not yet the sole live path for keyboard input.

2. **Scheduler hot path (partial):** The dispatch path still acquires the global
   `SCHEDULER` lock. Per-core queues and work-stealing are designed but not yet
   the active dispatch mechanism.

3. **Notification pool (partial):** Notifications remain backed by fixed-size
   arrays with `MAX_NOTIFS` for ISR safety. A growable pool design exists but
   has not shipped because safe ISR-context growth requires additional work.

Phase 52d either completes these items or explicitly re-defers them with
matching code comments.

## Why This Phase Exists

Phase 52b addresses the structural patterns that caused the Phase 52 bugs. This phase goes further, addressing scalability bottlenecks and design limitations that would otherwise constrain Phase 53+ work:

- The global `SCHEDULER` lock limits multi-core scalability
- Hard limits of 16 endpoints/notifications/services constrain the service extraction roadmap (Phases 52, 54)
- Duplicated line discipline between kernel and userspace creates maintenance burden and forced workarounds
- O(n) VMA lookup in the page fault handler limits application complexity
- Up to 10ms ISR notification wakeup latency is too high for real-time service response

These are not bugs — they are design limitations that become increasingly costly as the system grows.

## Learning Goals

- Understand per-core scheduling with work-stealing (Zircon: WAVL-tree fair scheduler with `StealWork()`)
- Learn how lock-free data structures enable ISR-to-scheduler communication
- See how a unified kernel line discipline simplifies the terminal subsystem (Linux: N_TTY ldisc)
- Understand why VMA lookup structure matters for page fault performance
- Learn the tradeoffs of dynamic vs. fixed-size kernel resource pools

## Feature Scope

### Per-core scheduler with work-stealing

Replace the single global `SCHEDULER: Mutex<Scheduler>` with per-core scheduler state. Each core has a local ready queue and a steal-enabled queue. Work-stealing balances load without a global lock on the dispatch hot path.

**Shipped state (audited Phase 52d):** Per-core run queues, work-stealing
(`try_steal`), load-balancing with per-task migration cooldown, and dead-slot
recycling all landed.  However, the global `SCHEDULER` lock is still acquired
on every dispatch iteration for task state reads, state transitions, and
post-switch bookkeeping.  True lock-free per-core dispatch — where the hot path
never acquires the global lock — is deferred (see Phase 52d Track D).

**Design reference:** `docs/appendix/architecture/next/04-scheduler-smp.md` Section 1.
**Comparison:** Zircon hybrid fair+deadline scheduler with per-CPU queues and cluster-aware stealing (`scheduler.cc`).

### Dynamic IPC resource pools

Replace the fixed-size arrays (`MAX_ENDPOINTS = 16`, `MAX_NOTIFS = 16`, `MAX_SERVICES = 16`, `CapabilityTable.slots = [Option; 64]`) with growable pools backed by slab allocation or `Vec`. Free IDs are recycled via a free list.

**Shipped state (audited Phase 52d):** Endpoints and capabilities are now
growable (`Vec`-backed with free-list recycling).  The service registry is
growable.  **Notifications remain fixed-size** (`MAX_NOTIFS = 64`) because
`PENDING` and `ISR_WAITERS` must be accessible from ISR context using only
lock-free atomics; a growable pool would require allocation or lock-based
indirection that is not ISR-safe.  The fixed-size constraint is documented
in `kernel/src/ipc/notification.rs` with exhaustion diagnostics.

**Design reference:** `docs/appendix/architecture/next/03-ipc-and-wakeups.md` Section 2.
**Comparison:** Zircon handle table (growable, per-process). seL4 CNode tree (arbitrarily deep).

### Unified kernel-side line discipline

Move all line discipline processing (canonical editing, echo, signal generation, input flag translation) into a single kernel-side `LineDiscipline` module. The keyboard input path feeds raw bytes to the same kernel function the serial path uses. The userspace `stdin_feeder` no longer implements its own line discipline — it calls a `push_raw_input` syscall instead.

**Design reference:** `docs/appendix/architecture/next/05-terminal-pty.md` Section 1.
**Comparison:** Linux N_TTY line discipline (`drivers/tty/n_tty.c`).

### VMA tree structure

Replace `Vec<MemoryMapping>` with a `BTreeMap<u64, MemoryMapping>` keyed by start address. VMA lookup becomes O(log n) instead of O(n), significantly improving page fault handler performance for processes with many mappings.

**Design reference:** `docs/appendix/architecture/next/01-memory-management.md` Section 4.
**Comparison:** Linux maple tree (v6.1+, `include/linux/maple_tree.h`).

### ISR-direct notification wakeup

Replace the tick-dependent `drain_pending_waiters()` mechanism with a per-core lock-free wakeup queue. ISRs push task IDs to the queue (lock-free, no scheduler lock needed). The scheduler drains the queue on every iteration, not just on timer ticks.

**Shipped state (audited Phase 52d):** The per-core `IsrWakeQueue` and
`ISR_WAITERS` lock-free mirror are implemented.  `signal_irq` pushes the
waiter to the ISR wake queue when available; the scheduler drains it each
iteration.  `drain_pending_waiters()` is retained as a BSP-only safety
fallback for edge cases (queue full, or waiter not yet registered when the
IRQ fired).

**Design reference:** `docs/appendix/architecture/next/03-ipc-and-wakeups.md` Section 3.
**Comparison:** seL4 immediate notification delivery; Zircon interrupt ports.

## Important Components and How They Work

### Per-core scheduler

Each `PerCoreData` has a `run_queue: Mutex<VecDeque<usize>>` and an
`IsrWakeQueue`. `pick_next()` checks the local queue, then steals from other
cores, then falls back to the idle task. The global `SCHEDULER` lock is still
acquired on each dispatch iteration for task state reads and transitions;
true lock-free per-core dispatch is deferred.

### ISR wake queue

A per-core `IsrWakeQueue` is a fixed-size ring buffer of `AtomicU64` entries. The ISR pushes a task index (lock-free SPSC via `compare_exchange` to prevent duplicates). The scheduler drains all entries on every loop iteration and calls `wake_task` under the global `SCHEDULER` lock. `drain_pending_waiters()` is retained on the BSP as a safety fallback.

### LineDiscipline module

A `LineDiscipline` struct holds a reference to the TTY state and provides `process_byte(byte) -> LdiscResult` that applies iflag transforms, signal generation, canonical editing, and echo. Both `serial_stdin_feeder_task` and the new `push_raw_input` syscall call this same function.

## How This Builds on Earlier Phases

- Extends Phase 52b's `AddressSpace` object with VMA tree storage
- Extends Phase 35 (True SMP) scheduler with per-core design
- Extends Phase 37 (I/O Multiplexing) infrastructure with ISR-direct wakeup
- Extends Phase 22 (TTY) and Phase 29 (PTY) with unified line discipline
- Extends Phase 6 (IPC Core) and Phase 50 (IPC Completion) with dynamic resource pools
- Prepares the system for Phase 53 (Headless Hardening) scalability and reliability claims
- Prepares the system for Phase 54 (Deep Serverization) which needs more IPC capacity

## Implementation Outline

1. Replace `SCHEDULER` with `TaskRegistry` + per-core `PerCoreScheduler`
2. Implement work-stealing in `pick_next`
3. Re-enable load balancing with per-task cooldown
4. Replace `EndpointRegistry` fixed array with growable `EndpointPool`
5. Replace `CapabilityTable` fixed array with growable `Vec`
6. Replace notification and service registry fixed arrays similarly
7. Create `LineDiscipline` in `kernel-core/src/tty.rs`
8. Refactor `serial_stdin_feeder_task` to use `LineDiscipline`
9. Add `push_raw_input` syscall
10. Simplify `stdin_feeder` to scancode decode + `push_raw_input`
11. Replace `Vec<MemoryMapping>` with `BTreeMap` in `AddressSpace`
12. Add per-core `IsrWakeQueue` ring buffer
13. Modify `signal_irq` to push to `IsrWakeQueue` when waiter is registered
14. Drain `IsrWakeQueue` on every scheduler loop iteration

## Acceptance Criteria

- Scheduler dispatch path does not acquire a global lock *(deferred — see Post-Phase Audit Note; global lock still acquired in HEAD; per-core run queues and work-stealing provide dispatch locality but task state transitions require the global SCHEDULER lock; true lock-free per-core dispatch deferred to a future phase)*
- IPC can create more than 16 endpoints without exhaustion
- A process can hold more than 64 capabilities
- Only one line discipline implementation exists in the codebase (kernel-side) *(partial — see Post-Phase Audit Note; stdin_feeder still duplicates ldisc logic)*
- `stdin_feeder` does not contain any canonical editing, echo, or ISIG logic *(partial — see Post-Phase Audit Note)*
- VMA lookup for a process with 100 mappings is measurably faster than current
- Keyboard IRQ → kbd_server wakeup latency is under 1ms (not 10ms)
- `cargo xtask check` and `cargo xtask test` pass

## Companion Task List

- [Phase 52c Task List](./tasks/52c-kernel-architecture-evolution-tasks.md)

## How Real OS Implementations Differ

- Zircon uses a hybrid WAVL-tree fair scheduler with per-CPU queues, deadline scheduling, and cluster-aware work-stealing. m3OS's simpler VecDeque-based approach is sufficient for its scale.
- Linux's CFS uses a red-black tree of virtual runtime values. The EEVDF extension adds virtual deadline ordering.
- seL4 uses a two-level bitmap for O(1) priority selection — much simpler than tree-based fair schedulers, but appropriate for a formally verified kernel.
- MINIX3 uses a 16-queue multilevel feedback scheduler with a userspace SCHED server. The userspace scheduler concept is interesting but beyond m3OS's current scope.
- Linux's N_TTY line discipline handles all terminal processing in kernel space. This is the model m3OS should follow for its unified ldisc.

## Deferred Until Later

- Full fair scheduler with virtual runtime (Zircon WAVL / Linux CFS) — current priority + round-robin is sufficient
- **True per-core scheduling** (lock-free dispatch hot path) — per-core run queues and work-stealing landed, but task state transitions still require the global `SCHEDULER` lock; splitting task ownership per-core is a larger architectural change deferred past Phase 52
- **Growable notification pool** — notifications remain fixed-size (`MAX_NOTIFS = 64`) because ISR-safe access requires lock-free fixed-size arrays; a two-level design (fixed ISR-visible table + growable overflow) is possible but not needed at current scale
- Atomic `reply_recv` (seL4-style) — nice optimization but not critical
- Preemptive scheduling from interrupt context — requires deeper `switch_context` redesign
- Dynamic PTY pool — can grow the fixed array to a larger number as a simpler intermediate step
