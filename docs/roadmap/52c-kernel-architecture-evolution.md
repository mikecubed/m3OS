# Phase 52c - Kernel Architecture Evolution

**Status:** Planned
**Source Ref:** phase-52c
**Depends on:** Phase 52b (Kernel Structural Hardening), Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅
**Builds on:** Extends the Phase 52b structural hardening with larger-scale architecture improvements that improve scalability, reduce resource limits, and unify duplicated subsystems
**Primary Components:** kernel/src/task/mod.rs, kernel/src/task/scheduler.rs, kernel/src/ipc/endpoint.rs, kernel/src/ipc/notification.rs, kernel/src/tty.rs, kernel/src/pty.rs, kernel/src/process/mod.rs, kernel/src/main.rs, userspace/stdin_feeder

## Milestone Goal

The kernel scheduler uses per-core queues with work-stealing, IPC resource pools are dynamically growable, the line discipline is implemented once in the kernel (not duplicated in userspace), VMA lookup is O(log n), and ISR-delivered notifications wake tasks immediately rather than waiting for the next scheduler tick.

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

**Design reference:** `docs/appendix/architecture/next/04-scheduler-smp.md` Section 1.
**Comparison:** Zircon hybrid fair+deadline scheduler with per-CPU queues and cluster-aware stealing (`scheduler.cc`).

### Dynamic IPC resource pools

Replace the fixed-size arrays (`MAX_ENDPOINTS = 16`, `MAX_NOTIFS = 16`, `MAX_SERVICES = 16`, `CapabilityTable.slots = [Option; 64]`) with growable pools backed by slab allocation or `Vec`. Free IDs are recycled via a free list.

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

**Design reference:** `docs/appendix/architecture/next/03-ipc-and-wakeups.md` Section 3.
**Comparison:** seL4 immediate notification delivery; Zircon interrupt ports.

## Important Components and How They Work

### Per-core scheduler

Each `PerCoreData` gains a `PerCoreScheduler` struct with `local_queue: VecDeque<TaskHandle>` and `steal_queue: Mutex<VecDeque<TaskHandle>>`. `pick_next()` checks local queue, then steal queue, then steals from other cores. The global `TaskRegistry` is only used for spawn and exit, not the dispatch hot path.

### ISR wake queue

A per-core `IsrWakeQueue` is a fixed-size ring buffer of `AtomicU64` entries. The ISR pushes a task ID (lock-free SPSC). The scheduler drains all entries on every loop iteration and calls `wake_task` under the per-core scheduler lock (not the global lock).

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

- Scheduler dispatch path does not acquire a global lock
- IPC can create more than 16 endpoints without exhaustion
- A process can hold more than 64 capabilities
- Only one line discipline implementation exists in the codebase (kernel-side)
- `stdin_feeder` does not contain any canonical editing, echo, or ISIG logic
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
- Atomic `reply_recv` (seL4-style) — nice optimization but not critical
- Preemptive scheduling from interrupt context — requires deeper `switch_context` redesign
- Dynamic PTY pool — can grow the fixed array to a larger number as a simpler intermediate step
