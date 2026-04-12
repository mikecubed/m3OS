# Kernel Architecture Evolution

**Aligned Roadmap Phase:** Phase 52c
**Status:** Complete
**Source Ref:** phase-52c
**Supersedes Legacy Doc:** (none -- new content)

## Overview

Phase 52c addresses five scalability bottlenecks and design limitations that
would constrain Phase 53+ work. It introduced per-core run queues and
work-stealing, growable endpoint/capability/service tables, a shared kernel
line-discipline path, O(log n) VMA lookup, and ISR-delivered wake queues.
Phase 52d then closed the live keyboard-path convergence and recorded the two
remaining honest limits: dispatch still acquires the global `SCHEDULER` lock,
and notifications remain fixed-size for ISR safety.

## What This Doc Covers

- Per-core scheduler dispatch infrastructure with work-stealing and task slot reuse
- Growable IPC resource pools for endpoints, capabilities, and services
- Unified kernel-side line discipline with the `push_raw_input` keyboard path
- BTreeMap-backed VMA tree for O(log n) address lookup
- ISR-direct notification wakeup via lock-free per-core queues

## Core Implementation

### Per-core scheduler with work-stealing (Track A)

The single global `SCHEDULER: Mutex<Scheduler>` was the dispatch hot-path
bottleneck. Phase 52c landed the per-core `run_queue`, work-stealing, migration
cooldowns, and dead-slot recycling that reduce cross-core contention. The Phase
52d audit clarified that the dispatch hot path still acquires the global lock
for task-state reads and transitions, so true lock-free per-core dispatch
remains later work.

When a core's local queue is empty, it steals from the busiest other core's
queue (affinity-checked). Dead task slots are recycled via a free list with
generation tracking, preventing unbounded Vec growth. Load balancing is
re-enabled with a 100-tick migration cooldown to prevent thrashing.

### Dynamic IPC resource pools (Track B)

Fixed-size arrays (`MAX_ENDPOINTS=16`, `CapabilityTable::SIZE=64`,
`MAX_SERVICES=16`) are replaced with growable `Vec`-backed pools that start at
the old size and grow in chunks. `MAX_NOTIFS` is increased from 16 to 64 but
remains a fixed-size array because `signal_irq()` indexes it from ISR context
where mutexes cannot be acquired.

Freed slots are reused via linear scan. Endpoint and service pools grow by 16.
Capability tables grow by 64. Endpoint IDs are capped at 256 (u8 limit).

### Unified kernel-side line discipline (Track C)

Line discipline logic (ICRNL, ISIG, ICANON, echo, VERASE, VKILL, VWERASE,
VEOF) was duplicated between the kernel's `serial_stdin_feeder_task` and the
userspace `stdin_feeder`. A single `LineDiscipline` struct in `kernel-core`
now owns both the `Termios` and `EditBuffer`, exposing `process_byte()` with
a callback for pushed data and a `LdiscResult` enum for echo and signal output.

`TtyState` wraps `LineDiscipline` instead of separate termios/edit_buf fields.
The `serial_stdin_feeder_task` delegates to `ldisc.process_byte()`. A new
`push_raw_input` syscall (0x1010) lets the userspace `stdin_feeder` send
decoded bytes directly to the kernel's line discipline. Phase 52d then reduced
`stdin_feeder` to a pure scancode-to-byte bridge so the live keyboard path also
stopped duplicating terminal policy in userspace.

### VMA tree structure (Track D)

`Vec<MemoryMapping>` in the Process struct is replaced with a `VmaTree` backed
by `BTreeMap<u64, MemoryMapping>`. `find_containing()` uses
`range(..=addr).next_back()` for O(log n) lookup in the page fault handler.
`remove_range()` correctly splits partially overlapping VMAs (left trim, right
trim, hole-punch). `update_range_prot()` handles mprotect with boundary
splitting.

VmaTree lives in `kernel-core/src/mm.rs` for host testability (23 unit tests).

### ISR-direct notification wakeup (Track E)

A 32-entry lock-free `IsrWakeQueue` (SPSC ring buffer) is added to
`PerCoreData`. `ISR_WAITERS: [AtomicI32; MAX_NOTIFS]` mirrors the mutex-
protected `WAITERS` array for lock-free ISR reads. When `signal_irq()` fires,
it reads `ISR_WAITERS` and pushes the waiter's task index to the local core's
queue. The scheduler drains the queue every iteration, waking blocked tasks
without waiting for the tick-driven `drain_pending_waiters()`.

`drain_pending_waiters()` is retained as a safety fallback for cases where
the queue was full or the ISR fired before the waiter registered.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/task/scheduler.rs` | Per-core dispatch, work-stealing, task slot reuse, load balancing |
| `kernel/src/task/mod.rs` | Task struct with `last_migrated_tick` for cooldown |
| `kernel/src/ipc/endpoint.rs` | Growable endpoint pool with Vec |
| `kernel-core/src/ipc/capability.rs` | Growable capability table with Vec |
| `kernel-core/src/ipc/registry.rs` | Growable service registry with Vec |
| `kernel/src/ipc/notification.rs` | MAX_NOTIFS=64, ISR_WAITERS mirror, signal_irq enhancement |
| `kernel-core/src/tty.rs` | LineDiscipline, SmallVec, LdiscResult (20 unit tests) |
| `kernel/src/tty.rs` | TtyState wraps LineDiscipline |
| `kernel/src/arch/x86_64/syscall/mod.rs` | push_raw_input syscall (0x1010) |
| `kernel-core/src/mm.rs` | VmaTree with BTreeMap (23 unit tests) |
| `kernel/src/process/mod.rs` | Process uses VmaTree instead of Vec |
| `kernel/src/smp/mod.rs` | IsrWakeQueue, try_per_core() |
| `userspace/syscall-lib/src/lib.rs` | push_raw_input wrapper |

## How This Phase Differs From Later Work

- The per-core scheduler uses VecDeque queues with priority scan, not a fair
  scheduler with virtual runtime (Zircon WAVL / Linux CFS). Priority +
  round-robin is sufficient for the current scale.
- Work-stealing is simple best-of-N, not cluster-aware. Topology awareness is
  deferred.
- The notification pool stays fixed-size (64) for ISR safety. True dynamic
  atomic arrays would require lock-free allocation infrastructure.
- The line discipline is minimal (N_TTY-compatible). Multi-ldisc switching and
  SLIP/PPP disciplines are out of scope.
- VMA tree uses BTreeMap, not Linux's maple tree or a custom interval tree.
- True lock-free per-core dispatch was re-deferred in Phase 52d even though the
  per-core queueing/work-stealing infrastructure from this phase remains active.

## Related Roadmap Docs

- [Phase 52c roadmap doc](./roadmap/52c-kernel-architecture-evolution.md)
- [Phase 52c task doc](./roadmap/tasks/52c-kernel-architecture-evolution-tasks.md)
- [Phase 52d -- Kernel Completion and Roadmap Alignment](./52d-kernel-completion-and-roadmap-alignment.md)
- [Phase 52b -- Kernel Structural Hardening](./52b-kernel-structural-hardening.md)
- [Phase 52a -- Kernel Reliability Fixes](./52a-kernel-reliability-fixes.md)
- [Phase 52 -- First Service Extractions](./52-first-service-extractions.md)
- [Architecture analysis (next)](./appendix/architecture/next/README.md)

## Deferred or Later-Phase Topics

- Full fair scheduler with virtual runtime (Zircon WAVL / Linux CFS)
- Atomic `reply_recv` (seL4-style optimized IPC)
- Preemptive scheduling from interrupt context
- Dynamic PTY pool
- Cluster-aware work-stealing topology
