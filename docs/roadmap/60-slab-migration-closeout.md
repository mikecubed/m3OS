# Phase 60 — Phase 33 Slab Migration Closeout

**Status:** Complete
**Source Ref:** phase-60
**Depends on:** Phase 33 (Kernel Memory Improvements) ✅, Phase 53a (Kernel Memory Modernization) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Builds on:** Delivers the migration half of Phase 33's slab-cache deliverable. Phase 33 shipped `KernelSlabCaches` with named members (`task_cache`, `fd_cache`, `endpoint_cache`, `pipe_cache`, `socket_cache`) but only `task_cache`'s allocation path was ever exercised — and only by a smoke test in `kernel/src/main.rs`. Phase 53a layered per-CPU magazines on top. Phase 60 routes the genuinely heap-allocated hot kernel objects through those caches.
**Primary Components:** `kernel/src/mm/slab.rs` (existing `KernelSlabCaches` infrastructure, plus a new `xsave_cache` member added by this phase), `kernel/src/task/scheduler.rs` (`Task` and `XSaveArea` allocation sites at lines 1092, 1097, 1098, 3651), `kernel/src/task/mod.rs` (`Task` and `XSaveArea` struct definitions; `KERNEL_STACK_SIZE`), `kernel/src/process/mod.rs` (kernel-stack allocation site at line 987 — audited and explicitly deferred), `kernel-core/src/slab.rs` (host-side slab tests).

## Milestone Goal

The two genuinely heap-allocated hot kernel object families — `Task` and `XSaveArea` — are allocated from typed slab caches rather than the global linked-list heap. A Track-A audit produces a written record of why every other family the original Phase 60 plan named (`FdEntry`, `Endpoint`, `Notification`, `Pipe`, `UnixSocket`) is **not** a slab-migration candidate: each is stored inline in a fixed-size slot array, which is a different (and equally legitimate) form of allocator avoidance. Phase 33 task-doc C.4 — whose acceptance bar is "at least two frequently allocated kernel object types use slab-backed allocation paths" — is flipped from `[ ] Deferred` to `[x]` with a citation to this phase's measurement note.

## Why This Phase Exists

The 2026-05-08 roadmap audit flagged Phase 33 C.4 as deferred. The original Phase 60 plan named five "hottest object families" for migration: `Task`, `Endpoint`, `Notification`, `FdEntry`, `VmRegion`. A grep-driven audit during the readiness review of those plans revealed three things:

1. Only `Task` (and its 1:1 companion `XSaveArea`) is genuinely allocated through `Box::new(...)` on the hot path. The other four families are stored inline in fixed-size slot arrays: `FdEntry` in `[Option<FdEntry>; MAX_FDS]` per process, `Endpoint` in `EndpointRegistry.slots`, `Notification` in a 64-slot ISR-safe atomic pool, `Pipe` in a slot table, `UnixSocket` similarly. Migrating them to slabs would first require a refactor that boxes each entry — a bigger architectural change than Phase 60's scope.
2. `VmRegion` does not exist as a kernel struct. The actual virtual-memory-area type is `kernel_core::mm::MemoryMapping` stored inside a `BTreeMap<u64, MemoryMapping>` (`VmaTree`). The BTreeMap nodes hold many entries per allocation, so per-mapping slab caching is not the natural fit.
3. The slab API the original plan assumed (`slab_alloc!(FAMILY_CACHE)` macros with per-family `lazy_static`s) does not exist. The actual API in `kernel/src/mm/slab.rs` exposes `caches().NAME_cache.lock().allocate(&mut page_alloc_callback)` returning `Option<usize>` (raw address) and `.free(addr)`, plus a class-based magazine fast-path used by the global allocator backing. The page-allocator callback wires the named-cache request to `frame_allocator::allocate_frame()`; a private helper `slab_page_alloc()` already exists in `kernel/src/mm/slab.rs:292` for this purpose.

The honest accounting is: most kernel object families already avoid the global heap by living in fixed-size slot arrays. The remaining heap-allocated hot objects are `Task` and its 1:1 `XSaveArea`. Routing those two through `task_cache` and a new `xsave_cache` is the cleanest closure of Phase 33 C.4 without inventing prerequisite refactors.

This matters for a 1.0 release because (a) Phase 33's task doc claims slab caches are infrastructure-complete with migration deferred — Phase 60 closes the migration gap honestly rather than pretending five families are migration-ready; and (b) the existing per-family caches (`task_cache`, `fd_cache`, `endpoint_cache`, `pipe_cache`, `socket_cache`) are presently dead code except for a single in-kernel `#[test_case]` that exercises `fd_cache` (`kernel/src/main.rs:1330-1370`) — wiring `task_cache` into the real `Task` allocation path proves the Phase 53a per-CPU magazine layer works under production load.

## Learning Goals

- How to identify genuine heap-allocation hot spots in a `no_std` kernel by grepping `Box::new` / `Arc::new` and classifying each by: (a) is it on a hot path, (b) is it a fixed-size object, (c) does it have a corresponding slot-array alternative already.
- Why a fixed-size slot array (`[Option<T>; N]`) is functionally equivalent to a slab cache for many kernel-object families — and when one is preferable to the other.
- How to wire `caches().X_cache.lock().allocate(&mut page_alloc_callback)` / `.free(addr)` into a Rust struct allocation site without changing the struct's public API.
- How to measure global-heap relief from inside the kernel using the existing `heap_stats()` and `all_slab_stats()` debug surfaces.

## Feature Scope

### Track A — Audit and Selection

Walk every `Box::new` and `Arc::new` site in `kernel/src/`. Classify each by: type allocated, allocation frequency tier (per-syscall, per-process, per-CPU, once-at-boot), fixed-size-ness, and current allocator path. Record the full table in `docs/handoffs/60a-allocation-audit.md`. The deliverable explicitly documents that `FdEntry`, `Endpoint`, `Notification`, `Pipe`, and `UnixSocket` are stored inline in slot arrays and therefore not slab candidates, and that `MemoryMapping` lives inside a `BTreeMap` whose node allocator is not a per-object slab fit. The audit confirms `Task` and `XSaveArea` as the only hot heap-allocated families that match the "fixed-size, hot, currently on global heap" criteria.

### Track B — Per-Family Slab Migration

Two real migrations:

**B.1 `Task`** — `sched.tasks.push(Box::new(task))` at `kernel/src/task/scheduler.rs:1097` and `:3651` is replaced with a `SlabBox<Task>` newtype that wraps a raw pointer obtained from `caches().task_cache.lock().allocate(&mut slab_page_alloc)`, with `core::ptr::write` to initialise the slot. `SlabBox<T>`'s `Drop` impl runs `core::ptr::drop_in_place(self.ptr)` then returns the slot to the owning cache via `.free(addr)`. (A plain `Box::from_raw` cannot be reused here because the global allocator's `dealloc` does not know about the slab cache.) `Vec<Box<Task>>` becomes `Vec<SlabBox<Task>>`; access via `Deref`/`DerefMut` is unchanged. The existing cache is sized at 512 bytes; B.1 first asserts `core::mem::size_of::<Task>()` and resizes the cache if needed before wiring the call site. B.1 also makes `slab_page_alloc` (currently `fn` at `kernel/src/mm/slab.rs:292`) `pub(crate)` so the helper is reachable from the scheduler.

**B.2 `XSaveArea`** — `sched.fpu_states.push(Box::new(XSaveArea::new()))` at `:1092` and `:1098` allocates 1:1 with `Task`. B.2 adds a new `xsave_cache: IrqSafeMutex<SlabCache>` member to `KernelSlabCaches` (sized to `XSAVE_AREA_SIZE` from `kernel/src/arch/x86_64/cpuid.rs:42`, currently 832 bytes) and converts `Vec<Box<XSaveArea>>` to `Vec<SlabBox<XSaveArea>>` using the same helper pattern.

The scheduler's `Vec` storage form changes from `Box<T>` to `SlabBox<T>` for both families; the public surface (`Deref`, `DerefMut`, indexing) is identical. Drop ordering for `Task` is unchanged because `SlabBox::drop` invokes `drop_in_place` (which runs `Task`'s field-drop chain in declaration order) before returning the slot to the cache.

### Track C — Heap Relief Measurement

After both migrations land, capture before/after measurements using the existing `kernel::mm::heap::heap_stats()` and `kernel::mm::slab::all_slab_stats()` surfaces. Boot QEMU with `cargo xtask run`, fork 50 tasks, run a 60-second IPC workload, and record the global heap free-list state plus `task_cache` and `xsave_cache` hit rates from the kernel serial dump. Record in `docs/handoffs/60c-slab-heap-measurement.md`.

The goal is a recorded before/after comparison confirming that the migrated families no longer consume global heap. No specific numeric target — the measurement is the deliverable.

### Track D — Regression Suite

Run `cargo xtask test` (full QEMU suite including SMP) and `cargo test -p kernel-core` after B.1 and again after B.2. No regression in any existing test is acceptable. Add at minimum one new host-side `kernel-core` unit test exercising `Task`-sized and `XSaveArea`-sized slab alloc/free/reuse cycles (the test uses raw byte buffers because `Task` and `XSaveArea` carry kernel-only globals — the test exercises the allocation path, not the type semantics).

### Track E — Phase 33 Doc Closure

Flip `docs/roadmap/tasks/33-kernel-memory-tasks.md` C.4 from `[ ] Deferred` to `[x] Migrated in Phase 60 — see docs/handoffs/60c-slab-heap-measurement.md`. C.4's acceptance bar is "At least two frequently allocated kernel object types use slab-backed allocation paths"; Phase 60 delivers exactly two (`Task` and `XSaveArea`), satisfying the bar without overcommitting. Update `docs/roadmap/33-kernel-memory-improvements.md` audit note to replace "the main hot kernel object families were not broadly migrated" with a factual record citing Phase 60's audit conclusion (most families use slot arrays; `Task` + `XSaveArea` migrated).

### Track F — Documentation and Release

Aligned legacy learning doc (`docs/60-slab-migration-closeout.md`) and kernel version bump from `0.58.0` to `0.60.0` in `kernel/Cargo.toml`, `Cargo.lock`, and `AGENTS.md`.

## Important Components and How They Work

### `kernel/src/mm/slab.rs` — `KernelSlabCaches`

The existing `KernelSlabCaches` struct (Phase 33 C.3) holds named `IrqSafeMutex<SlabCache>` members. Each cache backs a fixed-size class. The Phase 53a per-CPU magazine layer sits in front of the size-class caches (used by the global allocator); the named caches expose the lower-level direct API: `caches().task_cache.lock().allocate(&mut slab_page_alloc)` returns `Some(addr: usize)` or `None` on exhaustion; `.free(addr)` returns the slot. There is no `slab_alloc!`/`slab_free!` macro layer — Phase 60 introduces a small `SlabBox<T>` helper (constructed in B.1, reused by B.2) that owns a slab-allocated slot and frees it through the cache on drop.

Phase 60 adds one new member, `xsave_cache`, to this struct. No API change to existing callers.

### `Task` allocation (`kernel/src/task/scheduler.rs:1097, 3651`)

`Task` structs are heap-allocated via `Box::new(task)` then pushed into the scheduler's `Vec<Box<Task>>`. The migration replaces the `Box::new` with `task_cache.lock().allocate(&mut slab_page_alloc)` + `core::ptr::write` of the `Task` into the returned slot, wrapped in a `SlabBox<Task>` newtype whose `Drop` impl runs `Task::drop` then returns the slot to `task_cache` via `.free(addr)`. The scheduler's `Vec` is changed to hold `SlabBox<Task>` rather than `Box<Task>`; the access pattern through `Deref`/`DerefMut` is unchanged.

### `XSaveArea` allocation (`kernel/src/task/scheduler.rs:1092, 1098`)

`XSaveArea` is the per-task FPU/SSE/AVX save area. Allocated 1:1 with `Task` and pushed into a parallel `Vec<Box<XSaveArea>>` that B.2 converts to `Vec<SlabBox<XSaveArea>>`. `XSAVE_AREA_SIZE` is a compile-time `const` in `kernel/src/arch/x86_64/cpuid.rs` (currently 832 bytes); the cache is sized to that constant when `xsave_cache` is added to `init()` in `kernel/src/mm/slab.rs`.

### `kernel-core/src/slab.rs` — pure logic

The slab-cache data structure is pure logic in `kernel-core` and host-testable. Phase 60's new `kernel-core` tests verify alloc/free/reuse with object sizes matching `Task` and `XSaveArea`.

## How This Builds on Earlier Phases

- Phase 33 shipped `KernelSlabCaches` with named members but never wired the named-cache API into a real allocation path. Phase 60 wires `task_cache` and adds `xsave_cache`.
- Phase 53a put the per-CPU magazine layer under the size-class fast path used by the global allocator. The magazine layer also fronts the named caches — Phase 60's measurement validates that magazine behaviour under production load.
- Does not change the `SlabCache<T>` API.
- Closes the audit's Red Flag #4 / Blocker C2 by delivering Phase 33 C.4's "at least two object families" bar honestly.

## Implementation Outline

1. Track A: walk `kernel/src/` `Box::new` / `Arc::new` sites, classify each, write the audit doc. Record explicit non-candidates (`FdEntry`, `Endpoint`, `Notification`, `Pipe`, `UnixSocket`, `MemoryMapping`) with the structural reason each is excluded.
2. Track B.1: assert `Task` size, resize `task_cache` if needed, migrate `tasks.push(Box::new(...))` at scheduler.rs:1097 and :3651. Run Track D regression.
3. Track B.2: add `xsave_cache` to `KernelSlabCaches`, sized to `XSAVE_AREA_SIZE`. Migrate `fpu_states.push(Box::new(...))` at scheduler.rs:1092 and :1098. Run Track D regression.
4. Track C: 60-second IPC workload, capture `heap_stats()` + `all_slab_stats()` before and after, record in handoff.
5. Track E: flip Phase 33 C.4, update Phase 33 audit note.
6. Track F: aligned legacy doc + version bump.

## Acceptance Criteria

- `Task` allocates from `task_cache` (cache-size-checked against `core::mem::size_of::<Task>()`).
- `XSaveArea` allocates from a new `xsave_cache` member of `KernelSlabCaches` sized to `XSAVE_AREA_SIZE`.
- `cargo xtask test` and `cargo test -p kernel-core` pass with no regression.
- `docs/handoffs/60a-allocation-audit.md` exists and documents why `FdEntry`, `Endpoint`, `Notification`, `Pipe`, `UnixSocket`, and `MemoryMapping` are not slab-migration candidates.
- `docs/handoffs/60c-slab-heap-measurement.md` contains before/after `heap_stats()` and `all_slab_stats()` output confirming reduced global-heap usage for `Task` and `XSaveArea`.
- `docs/roadmap/tasks/33-kernel-memory-tasks.md` C.4 is `[x]` citing this phase and the measurement doc.
- `docs/roadmap/33-kernel-memory-improvements.md` audit note updated to reflect the actual migration record.
- `kernel/Cargo.toml` version is `0.60.0`; `AGENTS.md` reflects the bump.

## Companion Task List

- [Phase 60 Task List](./tasks/60-slab-migration-closeout-tasks.md)

## How Real OS Implementations Differ

- Linux SLUB declares a `kmem_cache` per major type at compile time and routes every kernel object family through it. m3OS uses the simpler hybrid of fixed-size slot arrays (for low-cardinality kernel-managed pools like endpoints and notifications) plus slab caches (for genuinely heap-allocated hot objects). Both forms avoid the global heap; Phase 60 documents the split honestly.
- Production kernels run `kmemleak` and `kasan` to verify slab discipline. m3OS relies on `cargo xtask test` regression and the host-side `kernel-core` unit tests as functional equivalents for now.
- Linux exposes `/proc/slabinfo`. m3OS exposes `all_slab_stats()` via the kernel serial debug dump; surfacing this through a syscall is post-1.0 work.

## Deferred Until Later

- Migrating kernel stacks (`Box::new([0u8; KERNEL_STACK_SIZE])` at `kernel/src/process/mod.rs:987`) to a slab — `KERNEL_STACK_SIZE` is 32 KiB (8 pages), which is a buddy-allocator-sized region, not a slab fit. A dedicated stack pool is post-1.0 work.
- Refactoring `FdEntry`, `Endpoint`, `Notification`, `Pipe`, or `UnixSocket` away from their slot-array storage form — the slot arrays are not a defect, they are a deliberate ISR-safety / capacity-bound design choice. Any future refactor would be a separate phase with its own design rationale.
- Per-`MemoryMapping` slab caching — the BTreeMap node allocator already amortises per-mapping cost; replacing the BTreeMap with a custom slab-backed structure is post-1.0 performance work.
- Implementing `/proc/slabinfo` equivalent as a syscall — post-1.0.
- `SLAB_HWCACHE_ALIGN` equivalent (cache-line alignment for hot objects) — post-1.0 performance work.
- Automated fragmentation regression test in CI — post-1.0.
