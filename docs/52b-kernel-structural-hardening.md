# Kernel Structural Hardening

**Aligned Roadmap Phase:** Phase 52b
**Status:** Complete
**Source Ref:** phase-52b
**Supersedes Legacy Doc:** (none -- new content)

## Overview

Phase 52b replaces the fragile kernel patterns that made the Phase 52 bugs
possible. Where Phase 52a patched the immediate symptoms, this phase introduces
structural defenses: a first-class `AddressSpace` object with per-core tracking,
typed `UserSliceRo`/`UserSliceWo` wrappers at the syscall boundary, task-owned
`UserReturnState` that eliminates ~40 manual restore call sites, batch TLB
shootdown with address-space targeting, and frame zeroing on free. These patterns
are drawn from Redox, seL4, and Zircon architecture analysis.

## What This Doc Covers

- AddressSpace as a first-class object with generation counter and active-core
  tracking
- Typed UserBuffer wrappers that centralize user-pointer validation
- Task-owned syscall return state that removes the per-core mutable scratch
  fragility
- Batch TLB shootdown with address-space-targeted IPIs
- Frame zeroing to prevent stale data exposure

## Core Implementation

### AddressSpace object

`kernel/src/mm/mod.rs` introduces `AddressSpace` wrapping the PML4 physical
address, a generation counter (bumped on every mapping change), and an
`active_on_cores` bitmask. Threads sharing an address space via `CLONE_VM` share
the same `Arc<AddressSpace>`. The scheduler calls `activate_on_core()` when
dispatching a task and `deactivate_on_core()` when switching away, so TLB
shootdown can send IPIs only to cores running the affected address space.

### Typed UserBuffer wrappers

`kernel-core/src/user_range.rs` defines `UserSliceRo` (read-only from
userspace), `UserSliceWo` (write to userspace), and validation at construction
time. All syscall boundary copy sites use these wrappers instead of raw pointer
arithmetic, making it impossible to accidentally swap read/write direction.

### Task-owned return state

`UserReturnState` in `kernel/src/task/mod.rs` stores `user_rsp`, `fs_base`, and
other per-task state. The scheduler dispatch loop restores it automatically.
`PerCoreData` fields become write-once-at-entry scratch rather than long-lived
state, eliminating the class of bug where a context switch overwrites another
task's return values.

### Batch TLB shootdown

`kernel/src/smp/tlb.rs` replaces the single-address `SHOOTDOWN_ADDR` broadcast
with a `ShootdownRequest` carrying a start address and page count. One IPI
covers an entire range. `AddressSpace.active_on_cores` ensures IPIs go only to
cores that have the affected address space loaded.

### Frame zeroing

`kernel/src/mm/frame_allocator.rs` zeroes freed frames before returning them to
the free pool. This closes the amplifier effect identified in the `copy_to_user`
investigation: a stale TLB mapping to a freed-and-reused frame no longer exposes
prior tenant data.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/mm/mod.rs` | `AddressSpace` struct with generation and active-core tracking |
| `kernel/src/mm/user_mem.rs` | `UserSliceRo`, `UserSliceWo` typed wrappers |
| `kernel/src/task/mod.rs` | `UserReturnState` in the Task struct |
| `kernel/src/smp/tlb.rs` | Batch TLB shootdown with address-space targeting |
| `kernel/src/mm/frame_allocator.rs` | Zero-on-free for user-accessible frames |
| `kernel/src/process/mod.rs` | Process uses `Arc<AddressSpace>` |

## How This Phase Differs From Later Work

- Phase 52c extends `AddressSpace` with a VMA tree (BTreeMap) for O(log n)
  lookup, replacing the Vec that this phase introduces.
- Phase 52c adds per-core scheduler queues and work-stealing, building on the
  per-core infrastructure established here.
- Phase 53 (Headless Hardening) will make reliability claims that depend on the
  structural guarantees established in this phase.

## Related Roadmap Docs

- [Phase 52b roadmap doc](./roadmap/52b-kernel-structural-hardening.md)
- [Phase 52b task doc](./roadmap/tasks/52b-kernel-structural-hardening-tasks.md)
- [Phase 52a -- Kernel Reliability Fixes](./52a-kernel-reliability-fixes.md)
- [Phase 52 -- First Service Extractions](./52-first-service-extractions.md)
- [Architecture analysis (next)](./appendix/architecture/next/README.md)

## Deferred or Later-Phase Topics

- VMA tree structure with O(log n) lookup (Phase 52c)
- Per-core scheduler with work-stealing (Phase 52c)
- Dynamic IPC resource pools (Phase 52c)
- Unified kernel-side line discipline (Phase 52c)
- ISR-direct notification wakeup (Phase 52c)
