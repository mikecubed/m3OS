# Phase 61 — Phase 35 SMP Load Balancing Closeout

**Status:** Planned
**Source Ref:** phase-61
**Depends on:** Phase 25 (SMP) ✅, Phase 35 (True SMP Multitasking) ✅, Phase 52d (Kernel Completion and Roadmap Alignment) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Builds on:** Delivers the Phase 35 headline — per-CPU run queues with active load balancing — by uncommenting `maybe_load_balance()` in the scheduler dispatch loop and completing the five supporting mechanical items that the 2026-05-08 audit found unimplemented: per-run-queue length counter (E.1), TLB shootdown in `munmap` (Phase 25 P25-T033 closure), pipe wait-queue attachment (G.2), IPC wait-queue attachment (G.3), and child CPU times reporting (H.3).
**Primary Components:** `kernel/src/task/scheduler.rs` (`maybe_load_balance`, `pick_next`, per-CPU queue length counter), `kernel/src/mm/user_space.rs` (`munmap` TLB shootdown), `kernel/src/ipc/endpoint.rs` (IPC wait-queue), `kernel/src/fs/pipe.rs` (pipe wait-queue), `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_wait4` / `sys_getrusage` child CPU times)

## Milestone Goal

SMP load balancing is operationally active: tasks migrate between per-CPU run queues when run-queue length imbalance exceeds a configurable threshold. `munmap` on one core sends TLB-shootdown IPIs to all cores that map the unmapped region, closing the SMP correctness hazard that Phase 25 deferred and Phase 35 inherited. Pipe and IPC consumers attached to wait queues wake correctly when their producer migrates cores. Child CPU times are reported by `sys_wait4`/`sys_getrusage`. After this phase, Phase 35's Task E.2 ("queue length counter"), E.1 (`maybe_load_balance` uncommented), Phase 25's P25-T033 (`munmap` TLB shootdown), and Phase 35's G.2/G.3/H.3 checkboxes are all `[x]`.

## Why This Phase Exists

Phase 35 was declared "True SMP Multitasking" with per-CPU run queues as its primary structural contribution. The audit found that `maybe_load_balance()` — the single call site that would make cross-core migration happen — was commented out in the scheduler dispatch loop, with a note that the global `SCHEDULER` lock was still acquired on every dispatch and load balancing would require per-core locking to be safe. Phase 52d acknowledged the per-core lock-free dispatch as deferred; it assigned no owner phase for the simpler alternative of uncommenting `maybe_load_balance()` under the existing global lock.

This phase takes the explicit position from Phase 52d's deferral: "true per-core lock-free dispatch is deferred; simpler global-lock load balancing is deliverable now." It does not remove the global `SCHEDULER` lock (that remains deferred per 52d and the Phase 57e post-mortem reasoning). It uncomments `maybe_load_balance()`, adds the run-queue length counter it needs to function correctly, and closes the five mechanical gaps the audit found open.

The `munmap` TLB shootdown is bundled here (not in Phase 25 or a standalone correctness phase) because it requires the SMP IPI infrastructure Phase 25 built and the `maybe_load_balance` work validates the IPI path. Doing both in one phase avoids a partial-SMP-correction state.

## Learning Goals

- Why load balancing requires an accurate per-queue length counter that can be read without holding the main scheduler lock.
- How TLB shootdown IPIs work: the initiating core unmaps pages, increments a sequence counter, sends IPIs to all other cores, and waits for acknowledgements before returning.
- How wait-queue attachments must survive task migration between cores — the wait queue must be per-object (pipe, endpoint), not per-CPU.
- What `struct rusage` fields correspond to child CPU times and how the kernel accumulates them across `wait4`.

## Feature Scope

### Track A — Per-Run-Queue Length Counter

Phase 35 E.1 requires an atomic per-run-queue length counter that `maybe_load_balance()` reads to decide which queue is overloaded. The counter must be maintained on every enqueue and dequeue without holding the global scheduler lock — an `AtomicUsize` per CPU is the standard approach.

### Track B — Uncomment and Harden `maybe_load_balance`

The commented-out call to `maybe_load_balance()` in the dispatch loop is the gating item for Phase 35 E.1 / Red Flag #3. Uncommenting it reveals whatever bugs prevented it from being uncommented originally. This track uncomments the call, fixes any issues found, and adds a configurable imbalance threshold (default: length difference > 2).

Explicitly deferred per Phase 52d: true per-core lock-free dispatch. The global `SCHEDULER` lock continues to be acquired on every dispatch iteration. `maybe_load_balance` acquires it during the balancing check; the balancing overhead is bounded to one check per N dispatch cycles (configurable).

### Track C — `munmap` TLB Shootdown

Phase 25 P25-T033: wire `tlb_shootdown` into `munmap`. `kernel/src/mm/user_space.rs` currently calls `invlpg` on the unmapping core only. On SMP, other cores retain stale TLB entries for the unmapped range until their next CR3 load. This is a correctness hazard that Phase 25 deferred to Phase 35, and Phase 35 also did not deliver.

The fix: after `invlpg` on the local core, send a TLB-shootdown IPI to all other cores that share the address space, wait for acknowledgements (using an existing IPI completion sequence from Phase 35's IPI infrastructure), and return.

### Track D — Wait-Queue Attachments

Phase 35 G.2 (pipe wait-queue) and G.3 (IPC wait-queue) require that wait queues be attached to the object (pipe buffer, endpoint) rather than the CPU, so that a producer migrating to a different core still wakes the consumer. This track verifies and, if needed, corrects the wait-queue attachment point for both pipe and IPC wakeup paths.

### Track E — Child CPU Times

Phase 35 H.3: `sys_wait4` and `sys_getrusage` report child CPU time (user + kernel ticks). The `rusage` struct fields for `ru_utime` and `ru_stime` must be populated from the exiting child's task accounting fields.

### Track F — Regression and Soak

After all functional tracks, run `cargo xtask test` (full suite), a 10-minute QEMU SMP soak with all logical CPUs active, and verify no regressions in IPC-latency-sensitive tests (sshd connect, display_server startup).

### Track G — Phase 35 and Phase 25 Doc Updates

Flip the audit-identified unchecked items in Phase 35's task doc (E.1, G.2, G.3, H.3) and Phase 25's task doc (P25-T033). Update both design docs with a "Phase 61 delivered" note in the relevant sections.

## Important Components and How They Work

### `maybe_load_balance()` in `kernel/src/task/scheduler.rs`

Currently commented out with a note about the global-lock concern. The function reads the run-queue length of the current core and the least-loaded core; if the difference exceeds the threshold, it migrates one task. Migration is a dequeue from the overloaded queue + enqueue to the target queue + cross-core IPI to wake the target core.

### TLB Shootdown IPI

Phase 35 and Phase 25 both established the IPI infrastructure. The TLB-shootdown handler on each receiving core calls `invlpg` for the unmapped range and sends an acknowledgement (via atomic counter). The initiating core spins on the acknowledgement counter before returning from `munmap`. The spin is bounded: acknowledgements arrive in microseconds on QEMU and low-latency hardware.

### Per-CPU `AtomicUsize` run-queue length

An `AtomicUsize` field per `PerCpuScheduler` that is incremented/decremented on every `enqueue_to_core` / `dequeue_from_core`. `maybe_load_balance()` reads this without the global lock to decide whether balancing is needed. The global lock is acquired only if balancing proceeds.

### Wait-queue object attachment

The canonical design: the wait queue `head` lives in the `PipeBuf` or `Endpoint` struct, not in a per-CPU list. All cores can wake a task by `push`-ing to the same wait-queue head. Phase 35's wait-queue implementation may already be object-attached; this track verifies and corrects if not.

## How This Builds on Earlier Phases

- Extends Phase 35 by delivering the dispatch behaviour its structural changes (per-CPU queues) were designed to enable.
- Closes Phase 25 P25-T033 by finally wiring `tlb_shootdown` into `munmap`.
- Reuses Phase 35's IPI infrastructure for both load-balancing migration IPIs and TLB-shootdown IPIs.
- Does not change the per-CPU queue data structures introduced in Phase 35 — only activates the balancing path.

## Implementation Outline

For Tracks A and B, follow a TDD approach: write the failing `test_load_balance_distributes_tasks` test (or a host-side `kernel-core` model test for the queue-length counter logic) before uncommenting `maybe_load_balance`. The test becomes the specification; uncommenting the call and adding the threshold constant is the implementation step. Run `cargo xtask test` as the QEMU smoke gate after each step to prevent silent regressions from accumulating across the five tracks.

1. Track A: add `AtomicUsize` queue-length counter to each `PerCpuScheduler`; update enqueue/dequeue to maintain it.
2. Track B: uncomment `maybe_load_balance()`; add threshold constant; fix any compilation errors; run `cargo xtask test`.
3. Track C: wire `tlb_shootdown` IPI into `munmap`; run SMP memory-map test.
4. Track D: inspect pipe and IPC wait-queue attachment; correct if needed; write a multi-core wakeup test.
5. Track E: populate `rusage.ru_utime` / `ru_stime` from child task accounting; verify with a `wait4` test.
6. Track F: run full regression suite + 10-minute SMP soak.
7. Track G: flip Phase 35 and Phase 25 task-doc checkboxes; update design docs.

## Acceptance Criteria

- `maybe_load_balance()` is uncommented and called in the scheduler dispatch loop.
- A test with 8 CPU-bound tasks across 2 QEMU cores shows tasks distributed ≤2 apart on each run queue within 5 seconds of workload start.
- `munmap` on core 0 sends TLB-shootdown IPIs to all other active cores; no stale TLB fault observed in SMP memory-map test.
- Phase 25 P25-T033 checkbox is `[x]`.
- Phase 35 E.1, G.2, G.3, H.3 checkboxes are `[x]`.
- `cargo xtask test` passes with no regression.
- 10-minute SMP soak produces no panics.

## Companion Task List

- [Phase 61 Task List](./tasks/61-smp-load-balancing-closeout-tasks.md)

## How Real OS Implementations Differ

- Linux's load balancer (`load_balance()` in `kernel/sched/fair.c`) is considerably more sophisticated: it runs in response to NOHZ idle ticks, uses scheduler domains for NUMA-aware balancing, and tracks CPU capacity. m3OS's threshold-based approach is a deliberate simplification.
- Linux's TLB shootdown coalesces range flushes across multiple `munmap` calls using `mmu_gather`. m3OS flushes immediately per call — acceptable at learning-OS scale.
- Linux's `rusage` is populated atomically during `wait4` and includes I/O statistics. m3OS populates only `ru_utime` / `ru_stime` in this phase; I/O stats are post-1.0.

## Deferred Until Later

- True per-core lock-free dispatch (removing the global `SCHEDULER` lock from the dispatch hot path) — explicitly deferred per Phase 52d; no owner phase assigned.
- NUMA-aware load balancing — post-1.0.
- Scheduler domain hierarchy — post-1.0.
- Coalesced TLB-shootdown range flushing (`mmu_gather` equivalent) — post-1.0.
- Populating `rusage` I/O accounting fields (`ru_inblock`, `ru_oublock`) — post-1.0.
