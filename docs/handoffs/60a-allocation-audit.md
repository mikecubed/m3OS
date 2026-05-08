# Phase 60 — Track A: Kernel Allocation-Site Audit

**Status:** Complete
**Source Ref:** phase-60 Track A.1
**Method:** `grep -rn 'Box::new\|Arc::new' kernel/src/` followed by per-site classification.
**Commit baseline:** `feat/phase-60-slab-migration-closeout` branched from `main` at commit `94ff3ba`.

## Why this audit exists

The original Phase 60 plan named five "hottest object families" (`Task`, `Endpoint`,
`Notification`, `FdEntry`, `VmRegion`) for slab migration without verifying that
each was actually heap-allocated. A grep-driven walk of `kernel/src/` shows that
only `Task` (and its 1:1 companion `XSaveArea`) is currently allocated through
`Box::new(...)` on the hot path. Every other named "candidate" is stored
inline in a fixed-size slot array, which is a different (and equally
legitimate) form of allocator avoidance.

This document is the durable record of that finding so that future phases
proposing slab migration of any inline-slot family will be challenged against
this audit before reopening the question.

## Method

Single grep over the entire kernel:

```text
grep -rn 'Box::new\|Arc::new' kernel/src/
```

Each hit was classified by:

1. **Type allocated** — what struct/array goes into the `Box`/`Arc`.
2. **Frequency tier** — once-at-boot, once-per-CPU, per-process, per-task,
   per-syscall, or per-IPC.
3. **Fixed-size?** — does every allocation produce the same byte size?
4. **Allocator path** — global heap (`linked_list_allocator`), buddy
   (large allocations spill there via `GlobalAlloc`), or already a slab.
5. **Slab candidate?** — yes/no with the structural reason.

## Ranked findings

### Migration candidates (this phase)

| # | Site | Type | Frequency | Size | Current path | Candidate? |
|---|---|---|---|---|---|---|
| 1 | `kernel/src/task/scheduler.rs:1097, 3651` | `Task` | per-task spawn | `core::mem::size_of::<Task>()` (asserted at compile time in B.1) | global heap via `Box::new` | **Yes** — migrated to `task_cache` in B.1 |
| 2 | `kernel/src/task/scheduler.rs:1092, 1098` | `XSaveArea` | per-task spawn (1:1 with `Task`) | `XSAVE_AREA_SIZE = 832` (`kernel/src/arch/x86_64/cpuid.rs:42`) | global heap via `Box::new` | **Yes** — migrated to new `xsave_cache` in B.2 |

### Non-candidates — inline slot arrays

These families are stored in fixed-size slot arrays, not on the heap. The
slot array is itself a form of allocator avoidance: a process or registry
holds the storage statically (or behind a single `Vec` allocation), and
each "object" is an `Option<T>` slot reused in place. Migrating to a slab
would require an architectural refactor (extracting each entry behind a
pointer) that has its own design cost — and would replace one
allocator-avoidance scheme with another, not eliminate one.

| Family | Storage form | File:line | Why not a slab candidate |
|---|---|---|---|
| `FdEntry` | `[Option<FdEntry>; MAX_FDS]` per process; `MAX_FDS = 32` | `kernel/src/process/mod.rs:218, 724` | Fixed per-process capacity, lives inline in `Process`; never `Box::new`'d. |
| `Endpoint` | `slots: Vec<Option<Endpoint>>` inside `EndpointRegistry` (single shared `Vec`, grows by chunks) | `kernel/src/ipc/endpoint.rs:66-67` | Single `Vec` allocation; per-`Endpoint` slab would not change allocator pressure. |
| `Notification` | Fixed 64-slot ISR-safe pool (`MAX_NOTIFS = 64`) | `kernel/src/ipc/notification.rs` (module header at lines 25-36) | ISR-safety constraint requires the static pool; making it slab-backed would force re-entrancy work for no allocator-pressure win. |
| `Pipe` | Slot table pattern (same shape as `EndpointRegistry`) | `kernel/src/pipe.rs` | Same slot-array reasoning as `Endpoint`. |
| `UnixSocket` | Slot table pattern | `kernel/src/net/unix.rs` | Same slot-array reasoning as `Endpoint`. |

### Non-candidate — BTreeMap node

| Family | Storage form | File:line | Why not a slab candidate |
|---|---|---|---|
| `MemoryMapping` (the actual VMA type) | `BTreeMap<u64, MemoryMapping>` inside `VmaTree` | `kernel-core/src/mm.rs:13, 27` | The BTreeMap node allocator already amortises per-mapping cost. A per-`MemoryMapping` slab would not match the access pattern (range queries by key); replacing the BTreeMap with a custom slab-backed structure is post-1.0 performance work. |

### Non-candidate — once-per-CPU / once-at-boot

These are allocated exactly once per CPU (or once at boot) and live for the
entire kernel uptime. A slab cache would offer no benefit over the global
heap for objects that are never freed.

| Family | File:line | Frequency | Comment |
|---|---|---|---|
| `PerCoreData` | `kernel/src/smp/mod.rs:636, 750` | once per CPU | Boxed because the struct contains a `TraceRing<N>` that exceeds the kernel stack frame; see comment at `kernel/src/smp/mod.rs:299-311`. Lives forever. |
| `TaskStateSegment` | `kernel/src/smp/mod.rs:728` | once per CPU | Lives forever. |
| `GlobalDescriptorTable` | `kernel/src/smp/mod.rs:738` | once per CPU | Lives forever. |

### Non-candidate — kernel stack (deferred to post-1.0)

| Family | File:line | Size | Comment |
|---|---|---|---|
| Kernel stack | `kernel/src/process/mod.rs:987` | `KERNEL_STACK_SIZE = 32 KiB` (8 pages) | Buddy-allocator-sized region, not a slab fit. A dedicated stack pool is post-1.0 work and is explicitly listed under "Deferred Until Later" in `docs/roadmap/60-slab-migration-closeout.md`. |

### Non-candidate — `Arc<T>` reference-counted shared state

`Arc<AddressSpace>`, `Arc<AtomicBool>` (wait-tokens), `Arc<ThreadGroup>`,
`Arc<IrqSafeMutex<FdTable>>`. Reference-counted shared state with variable
lifetime is a poor slab fit because:

1. The `Arc` adds 16 bytes of `ArcInner` overhead before the payload, so a
   slab's fixed slot size would have to accommodate `size_of::<ArcInner<T>>`
   rather than `size_of::<T>`.
2. These objects are not always the same size class — `AddressSpace`,
   `AtomicBool`, and `ThreadGroup` are different types with different sizes.
3. Allocation frequency is moderate (per-fork, per-IPC-wait), well within
   the global heap's design envelope.

| File | Lines (representative) |
|---|---|
| `kernel/src/ipc/registry.rs` | 143 |
| `kernel/src/task/wait_queue.rs` | 64 |
| `kernel/src/process/mod.rs` | 1193, 1258 |
| `kernel/src/syscall/device_host.rs` | 2485 |
| `kernel/src/task/scheduler.rs` | 2816, 2895, 2928, 2963 |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 4322, 4514, 5141, 5163, 5238, 12998, 13026, 13047, 13517, 15807, 16095, 16519 |

### Non-candidate — test-only or sanity-check sites

| File | Line | Purpose |
|---|---|---|
| `kernel/src/main.rs` | 124-125 | One-shot `Box::new(42u64)` boot sanity check. |
| `kernel/src/mm/slab.rs` | 962, 968 | `#[cfg(test)]` slab reclaim test. |
| `kernel/src/task/mod.rs` | 963, 975 | `#[cfg(test)]` stable-address filler tasks. |
| `kernel/src/task/scheduler.rs` | 3651 | `#[cfg(test)] install_test_task_idx` filler — note that the *production* Box::new at line 1097 is the migration target; this test-only site is migrated for type-coherence with the scheduler's `Vec<SlabBox<Task>>`. |

## Conclusion

The honest accounting is:

1. Most kernel object families already avoid the global heap by living in
   fixed-size slot arrays. The slot arrays are not a defect — they are a
   deliberate ISR-safety / capacity-bound design choice.
2. The remaining heap-allocated hot objects are `Task` and its 1:1
   `XSaveArea`. Routing those two through `task_cache` and a new
   `xsave_cache` is the cleanest closure of Phase 33 C.4 without inventing
   prerequisite refactors.
3. Once-per-CPU and once-at-boot allocations do not benefit from a slab.
4. Reference-counted shared state (`Arc<T>`) is a poor slab fit because of
   the `ArcInner` size overhead and variable type sizes.

Phase 60 therefore migrates exactly two object families — `Task` and
`XSaveArea` — satisfying Phase 33 C.4's acceptance bar ("at least two
frequently allocated kernel object types use slab-backed allocation paths")
without overcommitting.

## Future-phase guardrail

Any future proposal to slab-migrate `FdEntry`, `Endpoint`, `Notification`,
`Pipe`, `UnixSocket`, or `MemoryMapping` must first justify why the existing
storage form (slot array or BTreeMap node) is unsuitable, and must include a
measured before/after showing that the slab variant reduces global-heap
pressure or improves allocation latency. This audit is the durable artifact
that the proposal must address.
