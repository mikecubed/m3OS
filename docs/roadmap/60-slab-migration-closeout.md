# Phase 60 — Phase 33 Slab Migration Closeout

**Status:** Planned
**Source Ref:** phase-60
**Depends on:** Phase 33 (Kernel Memory Improvements) ✅, Phase 53a (Kernel Memory Modernization) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Builds on:** Delivers the Phase 33 headline deliverable that was scaffolded but never landed — broad migration of hot kernel object families from the global linked-list heap onto the slab caches introduced in Phase 33 and enhanced (per-CPU magazines) in Phase 53a.
**Primary Components:** `kernel/src/mm/slab.rs` (existing slab-cache infrastructure), `kernel/src/task/scheduler.rs` (`Task` struct allocation), `kernel/src/ipc/` (`Endpoint`, `Notification` object allocation), `kernel/src/fs/` (`FdEntry`, `FileDescription` allocation), `kernel/src/mm/` (`VmRegion` allocation), `kernel-core/src/` (host-side slab tests)

## Milestone Goal

The five hottest kernel object families — `Task`, `Endpoint`, `Notification`, `FdEntry`, and `VmRegion` — are allocated from typed slab caches rather than the global linked-list heap. Kernel heap fragmentation under a sustained workload (50 tasks, active IPC) is measurably reduced. Phase 33's Task C.4 checkbox ("broad object-allocation migration") is flipped from `[ ] Deferred` to `[x]`, with the slab-cache utilization measurement recorded in a handoff note.

## Why This Phase Exists

Phase 33's design doc and task doc both state that the slab *infrastructure* shipped but the broad object-family migration was deferred. Phase 53a added per-CPU page caches and magazine-based slab layers but also did not perform the migration. As of the 2026-05-08 audit, most kernel allocations still flow through the global linked-list heap despite the slab cache having been available for multiple major phases. The Phase 33 row in the roadmap README claims "buddy + slab + working munmap" as the primary outcome; the audit notes that the slab contribution to that outcome is the infrastructure only, not the allocation migration that would justify it as a delivered feature.

This matters for a 1.0 release for two reasons: (1) the slab infrastructure is carrying no production load, meaning its correctness is not exercised by the workloads users encounter; (2) the global heap's fragmentation under IPC-heavy workloads is a latent reliability risk for long-running sessions.

Phase 60 is bounded to the migration of the five families audited as hottest by allocation frequency. It does not redesign the slab API (that is Phase 53a's domain) and it does not add new slab features.

## Learning Goals

- How to identify allocation hotspots in a `no_std` kernel without profiling infrastructure — using call-site analysis and object lifetime patterns.
- What makes a kernel object family a good candidate for slab caching: fixed-size, high-allocation-frequency, short-to-medium lifetime.
- How to wire a `slab_alloc!`/`slab_free!` pair into a Rust `struct` without changing the type's public API.
- How to measure heap fragmentation and slab utilization from inside the kernel (using the existing debug serial port).

## Feature Scope

### Track A — Audit and Selection

Walk every kernel allocation site in `kernel/src/` and classify each allocated type by: size, allocation frequency (call-site count as a proxy), and whether the type is fixed-size. Produce a ranked table of candidate families. Confirm that the five target families (`Task`, `Endpoint`, `Notification`, `FdEntry`, `VmRegion`) are in the top tier, and record the full table in a handoff note for future phases.

### Track B — Per-Family Slab Migration

For each of the five target families, replace all `Box::new(...)` / global-heap alloc calls with `slab_alloc!(FAMILY_CACHE)` and the corresponding Drop path with `slab_free!(FAMILY_CACHE, ptr)`. Where the type has a derived `Drop` that also frees child allocations, the slab-free must happen after the child drops, not before.

Each family migration is a separate sub-task with its own compilation check and host-side test run. Families with cross-references to one another (e.g., `Task` holds an `FdEntry` table) are migrated in dependency order: `FdEntry` before `Task`.

### Track C — Heap Relief Measurement

After all five families are migrated, run a measurement pass: boot QEMU with `cargo xtask run`, start 50 tasks (using the existing `cargo xtask test` harness or a new stress script), run IPC-heavy workload for 60 seconds, and capture the global heap's free-list depth and the slab caches' hit rates from the kernel's serial debug dump. Record in `docs/handoffs/60c-slab-heap-measurement.md`.

The goal is not a specific number; it is a before/after comparison that confirms the migrated families are no longer consuming global heap.

### Track D — Regression Suite

Run `cargo xtask test` (all QEMU tests) and `cargo test -p kernel-core` (host-side slab unit tests) after each family migration. No regression in any existing test is acceptable. Add at minimum one new host-side `kernel-core` unit test per migrated family that exercises alloc/free/reuse through the slab path.

### Track E — Phase 33 Doc Closure

Flip Phase 33 task-doc C.4 from the current deferral note to `[x]` with a citation to this phase and the measurement handoff note. Update Phase 33's design doc audit note to replace "The slab-cache infrastructure and direct cache tests landed, but the main hot kernel object families were not broadly migrated" with the actual migration record.

## Important Components and How They Work

### `kernel/src/mm/slab.rs`

The existing slab infrastructure (Phase 33 + 53a) provides a `SlabCache<T>` type backed by the buddy allocator. Each cache holds a free list of fixed-size slots. The per-CPU magazine layer (Phase 53a) makes `slab_alloc` lock-free on the common path. The `slab_alloc!` / `slab_free!` macros wrap the cache in a lazy static and provide the typed interface.

### `Task` allocation

`Task` structs are currently allocated via `Box::new(Task { ... })` at `fork` / `exec` time. The migration replaces this with a slab-allocated slot. `Task` is large (includes scheduler fields, file descriptor table handle, signal state); ensuring the slab slot size matches the current `Task` size is the primary correctness concern.

### `Endpoint` and `Notification` allocation

IPC objects are allocated at `sys_endpoint_create` / `sys_notification_create` time. These are small fixed-size structs and are the highest-frequency allocation sites in IPC-heavy workloads.

### `FdEntry` allocation

Each open file descriptor allocates an `FdEntry`. Long-running server processes (sshd, display_server) accumulate hundreds of these. The slab cache amortizes the per-file-open allocation cost.

### `VmRegion` allocation

Virtual memory regions are allocated on each `mmap` call. Under Phase 54's serverized network stack, each new connection results in several `mmap` calls. Migrating `VmRegion` to the slab reduces heap pressure from connection churn.

## How This Builds on Earlier Phases

- Extends Phase 33 by finally delivering the object-migration half of its headline feature.
- Extends Phase 53a by putting the per-CPU magazine layer under real production load.
- Does not change the `SlabCache<T>` API — only adds call sites.
- Addresses the audit's Red Flag #4 and Blocker C2 directly.

## Implementation Outline

1. Run Track A: audit call sites, confirm the five target families, record the ranked table.
2. Migrate `FdEntry` (Track B.1) — smallest and most self-contained; run Track D regression.
3. Migrate `Notification` (Track B.2); run Track D regression.
4. Migrate `Endpoint` (Track B.3); run Track D regression.
5. Migrate `VmRegion` (Track B.4); run Track D regression.
6. Migrate `Task` (Track B.5) — largest and most complex; run full Track D suite.
7. Run Track C: 60-second IPC workload, capture before/after heap measurement.
8. Run Track E: flip Phase 33 C.4, update design doc audit note.

## Acceptance Criteria

- All five families (`Task`, `Endpoint`, `Notification`, `FdEntry`, `VmRegion`) allocate from slab caches, not the global linked-list heap.
- `cargo xtask test` passes with no regression after all five migrations.
- `cargo test -p kernel-core` passes with at least five new per-family slab unit tests.
- `docs/handoffs/60c-slab-heap-measurement.md` contains a before/after heap fragmentation comparison confirming reduced global-heap usage.
- Phase 33 task-doc C.4 is `[x]` citing this phase and the measurement doc.
- Phase 33 design doc audit note updated to reflect the migration landing.

## Companion Task List

- [Phase 60 Task List](./tasks/60-slab-migration-closeout-tasks.md)

## How Real OS Implementations Differ

- Linux's SLUB/SLAB allocators are integrated at the kernel startup path; every major type has a `kmem_cache` declared at compile time. m3OS's lazy-static approach is a practical simplification for a learning OS.
- Production kernels use `kmemleak` and `kasan` to verify that every slab-allocated object is freed correctly. m3OS does not have these tools; the regression suite and manual inspection are the equivalents.
- Linux slab caches expose `/proc/slabinfo` for runtime monitoring. m3OS exposes the equivalent via a serial debug dump on SIGQUIT equivalent; post-1.0, this could be surfaced via a syscall.

## Deferred Until Later

- Migrating lower-frequency object families (socket state, page-table entries, capability-table entries) — post-1.0 backlog.
- Implementing `/proc/slabinfo` equivalent as a kernel debug interface — post-1.0.
- SLAB_HWCACHE_ALIGN equivalent (cache-line alignment for hot objects) — post-1.0 performance work.
- Automated fragmentation regression test in CI — post-1.0.
