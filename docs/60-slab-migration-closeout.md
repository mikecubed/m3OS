# Phase 33 Slab Migration Closeout

**Aligned Roadmap Phase:** Phase 60
**Status:** Complete
**Source Ref:** phase-60
**Supersedes Legacy Doc:** new (no prior learning doc — Phase 60 is a closure phase for the deferred Phase 33 Track C.4)

## Overview

Phase 33 shipped a kernel slab allocator with named per-class caches
(`task_cache`, `fd_cache`, `endpoint_cache`, `pipe_cache`, `socket_cache`),
but only `fd_cache`'s direct allocate/free path was ever exercised — and only
by an in-kernel smoke test. The original Phase 33 plan deferred the
"migrate hot kernel object families to slab caches" work as Track C.4. Phase
60 closes that deferred track by:

1. **Auditing every `Box::new` and `Arc::new` site under `kernel/src/`** and
   classifying each by frequency, fixed-size-ness, and current allocator
   path. The audit (recorded in `docs/handoffs/60a-allocation-audit.md`)
   found that most "candidate" object families named in the original Phase
   60 plan — `FdEntry`, `Endpoint`, `Notification`, `Pipe`, `UnixSocket` —
   are stored inline in fixed-size slot arrays. A slot array is itself a
   form of allocator avoidance, and migrating those families to slab caches
   would require an architectural refactor with no clear benefit.
2. **Migrating the two genuinely heap-allocated hot kernel object
   families** — `Task` and `XSaveArea` — onto the existing slab
   infrastructure. `Task` now lives in `task_cache`; a new `xsave_cache`
   member is added to `KernelSlabCaches` for `XSaveArea`. Both are wrapped
   in a new `SlabBox<T>` newtype (`kernel/src/mm/slab_box.rs`) whose
   `Drop` returns the slot to the same cache.
3. **Recording the measurement** — before/after `task_cache` and
   `xsave_cache` activity from `cargo xtask run` boots, captured in
   `docs/handoffs/60c-slab-heap-measurement.md`. The post-migration boot
   shows 9 active `Task` slots and 9 active `XSaveArea` slots in the
   dedicated caches; the pre-migration boot shows 0.
4. **Flipping Phase 33 task-doc C.4** from `[ ] Deferred` to `[x] Migrated
   in Phase 60` with a citation to the measurement doc.

The honest accounting is: most kernel object families avoid the global
heap by living in fixed-size slot arrays; the rest are once-per-CPU
allocations that don't benefit from a slab. The remaining heap-allocated
hot families are `Task` and its 1:1 companion `XSaveArea`. Phase 60 routes
those two through dedicated caches and stops there — exactly the "at
least two" target Phase 33 C.4's acceptance bar set.

## What This Doc Covers

- The Track A audit and why most "candidate" object families are not slab
  candidates after all.
- The `SlabBox<T>` newtype and how it owns slab-allocated slots in a
  `Box`-like API without going through the global allocator's `dealloc`.
- The `Task` and `XSaveArea` migrations and the const-assert tripwires that
  catch future struct-size drift.
- The measurement deliverable and how the post-migration `task_cache` /
  `xsave_cache` activity count proves the migration is wired to the real
  allocation path.

## Core Implementation

### The audit comes first

Phase 60's first deliverable is `docs/handoffs/60a-allocation-audit.md`.
It walks every `Box::new` / `Arc::new` site under `kernel/src/` and
classifies each:

- **Migration candidates (this phase):** `Task`, `XSaveArea` — both in
  `kernel/src/task/scheduler.rs`, allocated once per task spawn.
- **Inline-slot-array non-candidates:** `FdEntry`, `Endpoint`,
  `Notification`, `Pipe`, `UnixSocket` — all stored in fixed-size slot
  arrays per process or per registry.
- **BTreeMap-node non-candidate:** `MemoryMapping` lives inside a
  `BTreeMap<u64, MemoryMapping>` whose node allocator already amortises
  per-mapping cost.
- **Once-per-CPU non-candidates:** `PerCoreData`, `TaskStateSegment`,
  `GlobalDescriptorTable`, kernel stacks — none benefit from a slab cache.
- **`Arc<T>` shared-state non-candidates:** `Arc<AddressSpace>`,
  `Arc<AtomicBool>`, `Arc<ThreadGroup>`, `Arc<IrqSafeMutex<FdTable>>` —
  `ArcInner` overhead and variable type sizes make these a poor slab fit.

The audit is the most important durable artefact of this phase. Future
proposals to slab-migrate any of the inline-slot families must address it.

### `SlabBox<T>` — owning slab slot

`Box<T>` cannot be reused for slab-allocated pointers because the global
allocator's `dealloc` does not know about the slab cache. `SlabBox<T>`
solves that:

```rust
pub struct SlabBox<T: ?Sized> {
    ptr: NonNull<T>,
    cache: &'static IrqSafeMutex<SlabCache>,
    _marker: PhantomData<T>,
}
```

`SlabBox::new_in(&'static cache, value)` allocates a slot from the cache,
moves `value` into it via `core::ptr::write`, and returns the owning
pointer. `Drop` runs `core::ptr::drop_in_place` then returns the slot via
`cache.lock().free(addr)`. The `Deref` / `DerefMut` / `AsRef` / `AsMut`
implementations make `SlabBox<T>` substitute for `Box<T>` at every access
site.

### `Task` migration (B.1)

The scheduler's `tasks: Vec<Box<Task>>` field becomes
`Vec<SlabBox<Task>>`. Both `Box::new(task)` sites in `alloc_task_slot`
and `install_test_task_idx` are replaced with
`SlabBox::<Task>::new_in(&caches().task_cache, task)`. A const-assert
catches future field additions that push `size_of::<Task>()` past the
cache slot size:

```rust
const _: () = assert!(
    core::mem::size_of::<Task>() <= crate::mm::slab::TASK_CACHE_SLOT_SIZE,
    "Task exceeds task_cache slot size; bump TASK_CACHE_SLOT_SIZE in \
     kernel/src/mm/slab.rs to the next power of two"
);
```

`TASK_CACHE_SLOT_SIZE` is `1024` — bumped from Phase 33's original `512`
because the post-Phase 57b/d preempt-frame and per-task syscall-snapshot
fields pushed `Task` past `512` bytes.

### `XSaveArea` migration (B.2)

A new `xsave_cache: IrqSafeMutex<SlabCache>` member is added to
`KernelSlabCaches` and sized to `XSAVE_CACHE_SLOT_SIZE = 832` (re-exported
from `crate::arch::x86_64::cpuid::XSAVE_AREA_SIZE`). The scheduler's
`fpu_states: Vec<Box<XSaveArea>>` field becomes
`Vec<SlabBox<XSaveArea>>`. Allocation is 1:1 with `Task` (every
`alloc_task_slot` call pushes both), so `xsave_cache.active_objects`
tracks `task_cache.active_objects` exactly under production load.

### Measurement (Track C)

`docs/handoffs/60c-slab-heap-measurement.md` records before/after
`task_cache` and `xsave_cache` stats from `cargo xtask run` against the
post-Track-A baseline vs. post-B.2:

- **Before:** `task_cache slabs=0 active=0 free=0`, `xsave_cache` absent.
- **After:** `task_cache slabs=3 active=9 free=3`, `xsave_cache slabs=3
  active=9 free=3`.

`heap.used_bytes` drops by 19 KiB, consistent with moving 9 × (1024 +
832) bytes off the global heap. The migration is wired to the real
allocation path.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/mm/slab.rs` | `KernelSlabCaches` extended with new `xsave_cache`; `task_cache` slot size bumped from 512 → 1024 via new `TASK_CACHE_SLOT_SIZE` const; `slab_page_alloc` made `pub(crate)`; `all_slab_stats` extended with `xsave`. |
| `kernel/src/mm/slab_box.rs` | New module — `SlabBox<T>` newtype, the owning smart pointer for slab-allocated slots. |
| `kernel/src/task/scheduler.rs` | `tasks: Vec<SlabBox<Task>>`, `fpu_states: Vec<SlabBox<XSaveArea>>`, both `alloc_task_slot` allocation paths routed through the new caches; const-assert tripwires for both struct sizes. |
| `kernel/src/main.rs` | One-shot `[phase60]` log line in `init_task` post-`spawn_userspace_init`; new in-kernel `phase60_heap_relief_stats_dump` and `xsave_slab_cache_alloc_free` test cases. |
| `kernel-core/src/slab.rs` | Two new host-side regression tests: `task_sized_slab_cache_alloc_free_reuse` and `xsave_sized_slab_cache_alloc_free_reuse`. |
| `docs/handoffs/60a-allocation-audit.md` | The Track A allocation-site audit. |
| `docs/handoffs/60c-slab-heap-measurement.md` | Before/after measurement record. |
| `docs/roadmap/60-slab-migration-closeout.md` | Phase design doc. |
| `docs/roadmap/tasks/60-slab-migration-closeout-tasks.md` | Phase task list. |

## How This Phase Differs From Later Memory Work

- **Phase 33** introduced `KernelSlabCaches` and the per-cache direct
  `allocate`/`free` API. Phase 33 stopped at infrastructure; the migration
  was deferred as Track C.4.
- **Phase 53a** layered per-CPU magazines on top of the size-class slab
  caches (the magazine layer fronts both the size-class fast path used by
  the global allocator and the named caches). Phase 53a did not migrate
  any object families.
- **Phase 60** wires `task_cache` (already declared in Phase 33) into the
  real `Task` allocation path and adds a new `xsave_cache` for
  `XSaveArea`. The Phase 53a magazine layer is exercised by the migration
  under production load for the first time.
- **Inline slot arrays vs. slab caches.** Most kernel object families
  (`FdEntry`, `Endpoint`, `Notification`, `Pipe`, `UnixSocket`) avoid the
  global heap by living in fixed-size slot arrays — `[Option<FdEntry>;
  MAX_FDS]`, `Vec<Option<Endpoint>>` inside `EndpointRegistry`, a 64-slot
  ISR-safe atomic pool for `Notification`, etc. Slot arrays are not a
  defect: they are a deliberate ISR-safety / capacity-bound design choice.
  Migrating them to slabs would replace one allocator-avoidance scheme
  with another, not eliminate one. Phase 60 documents the split honestly
  rather than over-promising.
- **Future:** A dedicated kernel-stack pool (`kernel/src/process/mod.rs`
  line 987, `KERNEL_STACK_SIZE = 32 KiB`) is post-1.0 work — kernel
  stacks are buddy-allocator-sized, not slab-sized.

## Related Roadmap Docs

- [Phase 60 design doc](./roadmap/60-slab-migration-closeout.md)
- [Phase 60 task doc](./roadmap/tasks/60-slab-migration-closeout-tasks.md)
- [Phase 33 design doc](./roadmap/33-kernel-memory-improvements.md) — closed Track C.4 audit note
- [Phase 33 task doc](./roadmap/tasks/33-kernel-memory-tasks.md) — Track C.4 flipped to `[x]`
- [Phase 60 audit handoff](./handoffs/60a-allocation-audit.md)
- [Phase 60 measurement handoff](./handoffs/60c-slab-heap-measurement.md)

## Deferred or Later-Phase Topics

- Refactoring `FdEntry`, `Endpoint`, `Notification`, `Pipe`, or
  `UnixSocket` away from their slot-array storage form — the slot arrays
  are not a defect; any refactor would be a separate phase with its own
  design rationale.
- A dedicated kernel-stack pool — `KERNEL_STACK_SIZE = 32 KiB` is buddy-
  allocator-sized; a pool is post-1.0 work.
- Per-`MemoryMapping` slab caching — the BTreeMap node allocator already
  amortises per-mapping cost.
- A `/proc/slabinfo` syscall — post-1.0; for now, `all_slab_stats()` is
  reachable via the kernel `meminfo` syscall.
- `SLAB_HWCACHE_ALIGN` equivalent (cache-line alignment for hot objects)
  — post-1.0 performance work.
- Automated fragmentation regression test in CI — post-1.0.
