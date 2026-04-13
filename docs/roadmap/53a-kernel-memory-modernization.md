# Phase 53a - Kernel Memory Modernization

**Status:** Planned
**Source Ref:** phase-53a
**Depends on:** Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP) ✅, Phase 36 (Expanded Memory) ✅, Phase 52c (Kernel Architecture Evolution) ✅, Phase 52d (Kernel Completion) ✅
**Builds on:** Replaces the Phase 33 linked-list heap and unintegrated slab-cache scaffolding with an SMP-scalable allocator stack informed by `docs/research/m3os-allocator-analysis.md`, `docs/research/allocator-theory.md`, and `docs/research/memory-allocator-survey.md`
**Primary Components:** kernel/src/mm/, kernel-core/src/buddy.rs, kernel-core/src/slab.rs, kernel/src/mm/heap.rs, kernel/src/mm/frame_allocator.rs, kernel/src/mm/slab.rs

## Milestone Goal

The kernel memory subsystem becomes SMP-scalable and auditable: per-CPU page caching removes the global frame-allocator lock from the common single-page path, a magazine-based slab allocator with out-of-line slab metadata replaces the linked-list heap for small kernel objects, and the allocator preserves Phase 52b's zero-before-user-exposure guarantee while eliminating the double-lock and O(n) buddy removal paths.

## Why This Phase Exists

Phase 33 established a correct-but-minimal memory subsystem: buddy allocator, linked-list heap, and slab cache infrastructure. That was appropriate for the learning goal at the time. Since then, Phase 25 added SMP, Phase 35 added per-core syscalls and scheduling, and Phase 52 began extracting services to userspace — all of which increase allocation pressure and concurrency. The current allocator has fundamental scalability limitations:

- Every allocation (heap or frame) serializes through a single global `spin::Mutex`
- The slab caches defined in Phase 33 were never integrated — all objects go through the O(n) linked-list heap
- There is no interrupt-safety mechanism — a page fault while the frame lock is held causes deadlock
- The buddy allocator's coalescing path has an O(n) linear scan (`remove_free`)

Phase 52d already re-audited several roadmap claims; Phase 53a continues that pattern for memory. The goal is not just a faster allocator, but a roadmap-accurate one: Phase 33 shipped buddy allocation, heap growth, and slab-cache scaffolding, but it did not broadly migrate hot kernel object families onto slab caches. Phase 54 (Deep Serverization) will extract storage, namespace, and networking to userspace, creating heavy IPC and cross-CPU allocation patterns. The allocator must be modernized before that work begins, and Phase 53's release gates should be defined early enough that 53a has a concrete validation target.

## Learning Goals

- Understand why per-CPU caching is the dominant design pattern in every production kernel allocator
- Learn the magazine/bucket pattern (Bonwick & Adams, FreeBSD UMA) and how it amortizes lock contention
- Understand size-class-based allocation and why geometric spacing bounds internal fragmentation
- See where Rust's type system can encode memory-state invariants even when full type-state frame wrappers are deferred
- Learn the trade-offs between embedded freelists (SLUB) and bitmap allocation (current m3OS)

## Feature Scope

### Per-CPU page cache (Track A)

Each CPU maintains a hot-page list (batch size 32-64). `allocate_frame()` pops from the per-CPU list without acquiring the global buddy lock. When the list is empty, a batch refill acquires the buddy lock once for 32-64 frames. `free_frame()` pushes to the per-CPU list; when it exceeds the high watermark, a coordinated drain returns pages to the buddy. Because frames are allocated in page-fault context today, local cache mutation must be IRQ-safe; global reclaim uses owner-CPU self-drain or equivalent synchronized handoff rather than unsafely mutating another CPU's lockless cache in place.

### Slab allocator with per-CPU magazines (Track B)

Replace the unused `SlabCache` with an embedded-freelist slab allocator and per-CPU magazine layer. Each size class has two magazines per CPU (loaded + previous, per the Bonwick/Adams pattern). The local fast path mutates only CPU-local magazines with interrupts masked, avoiding both lock contention and IRQ re-entrancy bugs. Cross-CPU frees use a remote atomic queue. Slab metadata lives out-of-line in a page-base/span table so the 4 KiB size class remains usable.

### Size-class-based GlobalAlloc (Track C)

Replace `linked_list_allocator::LockedHeap` with a custom `GlobalAlloc` that routes allocations to the appropriate size-class slab cache. 13 geometric size classes (32B to 4KiB, 4 steps per doubling, <20% max internal waste) cover the small-object path. Larger or stronger-aligned layouts use a page-backed mapped allocation path with explicit metadata for deallocation; the buddy allocator remains the physical backend.

### Foundation fixes (Track D)

- Fix `free_frame` double-lock: cache `phys_offset` in a static after init
- Fix `remove_free` O(n): replace linear-scanned free-block removal with an O(1)-removable metadata structure that preserves `kernel-core` host testability
- Fix SeqCst refcount ordering: downgrade to Acquire/Release
- Move unconditional frame zeroing off the free path while preserving zero-before-user-exposure guarantees
- Add freelist pointer hardening (XOR with random + address)

### Cross-CPU free path (Track E)

When a CPU frees an object allocated on a different CPU's slab, it pushes to the owner CPU's atomic free list via CAS (mimalloc MPSC pattern). The owning CPU batch-collects via `atomic_exchange` during its next allocation. No lock acquisition for cross-CPU free.

### Integration, reclaim, and validation (Track F)

Make the allocator safe to land incrementally. Define the minimal allocation-context contract for page-fault/IRQ-sensitive callers, add allocator-local reclaim before high-order/OOM failure, preserve stats and `/proc/meminfo` / `sys_meminfo` semantics across the cutover, and add a kill switch plus concurrency validation (including loom for new Acquire/Release queues).

### Minimal allocation-context contract (Track F.1)

Phase 53a adopts a deliberately small contract before full GFP-style flags exist:

| Context | Contract |
|---|---|
| **IRQ-sensitive / page-fault-adjacent** | Local per-CPU page-cache and magazine mutations run with interrupts masked plus a same-core non-reentrancy guard. Re-entrant allocs bypass CPU-local caches/magazines and use a best-effort cold path (or fail with `None`); re-entrant slab frees route to the owner CPU's lock-free queue rather than touching magazine state twice. |
| **Sleepable** | Callers may tolerate the contended cold path, trigger reclaim, or retry after `None`. Future reclaim that might sleep is restricted to this context. |

Cold paths still use spin locks only today, so "sleepable" means "allowed to take the contended path / retry" rather than "allocator literally sleeps already."

## Important Components and How They Work

### Per-CPU page cache (`kernel/src/mm/frame_allocator.rs`)

A fixed-size array of physical frame addresses per CPU, accessed via `smp::per_core()`. Cooperative scheduling removes kernel task migration during the fast path, but IRQ re-entrancy still exists, so local mutations occur with interrupts masked or an equivalent local non-reentrancy guard. The per-CPU cache sits between callers and the global `BuddyAllocator`, absorbing the vast majority of single-page allocations while coordinated reclaim drains caches via owner-CPU self-drain or equivalent synchronized handoff.

### Magazine depot (`kernel/src/mm/slab.rs` or new `kernel/src/mm/magazine.rs`)

Per-size-class stacks of full and empty magazines. Protected by a per-size-class spinlock. Magazines are fixed-capacity arrays of object pointers (capacity 32). When a CPU's loaded magazine empties, it swaps with the previous magazine. If both are empty, the empty loaded magazine is exchanged for a full one from the depot. This amortizes the depot lock to 1 acquisition per 32 allocations while keeping local magazine mutations IRQ-safe.

### Embedded-freelist slab (`kernel-core/src/slab.rs` rewrite)

Each free object contains a pointer to the next free object. Allocation is a single pointer follow. Slab metadata (freelist head, in-use count, owning CPU, size class, and partial-list linkage) is stored out-of-line in span metadata keyed by slab page base, preserving cache separation and keeping the 4 KiB size class viable. Partial slabs are linked in a list per size class for O(1) slab selection.

### Size-class router (`kernel/src/mm/heap.rs` rewrite)

The `#[global_allocator]` maps allocation sizes to slab caches via a lookup table. `Layout::size()` → size class index → slab cache covers the common small-object path. Large or stronger-aligned layouts use a page-backed mapped allocation path that records enough metadata to free the allocation correctly. This replaces the O(n) linked-list scan with O(1) dispatch for small objects without pretending every allocation can be satisfied by a raw buddy frame.

### Buddy allocator improvements (`kernel-core/src/buddy.rs`)

Replace `Vec::position()`-based free-block removal with an O(1)-removable per-order representation while preserving `kernel-core` as a pure, host-testable data structure. If page-body metadata is used, keep it in the kernel-side wrapper rather than baking physical-page assumptions into `kernel-core`.

### Compatibility and observability surfaces (`kernel/src/mm/*`, `kernel/src/fs/procfs.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`)

`frame_stats()`, `heap_stats()`, `/proc/meminfo`, and `sys_meminfo` currently assume a single global frame pool and a fixed heap/slab shape. Phase 53a must define how per-CPU cached pages, size-class slabs, remote free queues, and page-backed large allocations appear in those stats so debugging and tests stay meaningful during the cutover.

### Memory accounting policy (Track F.3 — Linux-like semantics)

The project adopts Linux-like memory accounting:

| Metric | Definition | `/proc/meminfo` field |
|---|---|---|
| **MemFree** | Buddy-managed frames immediately allocatable without draining any per-CPU cache | `MemFree` |
| **MemAvailable** | `MemFree` + reclaimable per-CPU cached pages | `MemAvailable` |
| **Allocated** | `MemTotal − MemAvailable`.  Frames actively backing kernel or user mappings | `Allocated` |
| **PerCpuCached** | Frames held in per-CPU page caches; excluded from MemFree, included in MemAvailable | `PerCpuCached` |

Key invariants enforced by `frame_stats_consistent()`:
- `total_frames == available_frames + allocated_frames`
- `available_frames == free_frames + per_cpu_cached`
- `sum(order_count × 2^order) == free_frames` (buddy orders sum to buddy-only free count)

## How This Builds on Earlier Phases

- Replaces Phase 33's `LockedHeap` + unused `SlabCache` with a production-grade allocator stack
- Preserves Phase 33's `BuddyAllocator` as the cold-path backend, improving its free-block removal and metadata model without giving up host-testability
- Preserves Phase 36's demand paging and mprotect — the new allocator uses the same frame allocation API
- Preserves Phase 17's per-frame refcount semantics for CoW fork
- Preserves Phase 52b's stale-mapping hardening by keeping the invariant that user-visible frames are zeroed before exposure even if zeroing moves off the free path
- Uses Phase 25/35's per-CPU infrastructure (`smp::per_core()`, APIC ID) for per-CPU caches
- Informed by Phase 52c's architectural evolution pattern (VmaTree replaced Vec, same approach here)

## Implementation Outline

1. **Track D (foundation fixes):** Fix double-lock, O(n) remove_free, refcount ordering, and restate the exact zero-before-exposure guarantee before changing zeroing behavior.
2. **Track F.1 (contract):** Define IRQ/atomic-context rules, reclaim hooks, and observability surfaces before replacing hot paths.
3. **Track A (per-CPU page cache):** Add per-CPU frame lists, modify `allocate_frame()` / `free_frame()` to check per-CPU first, and implement coordinated owner-CPU drain/reclaim.
4. **Track B (slab + magazines):** Rewrite `kernel-core/src/slab.rs` with embedded freelist, out-of-line metadata, and the magazine layer. Create size-class caches.
5. **Track C (GlobalAlloc replacement):** Wire size-class slab caches into a new `GlobalAlloc` implementation, define bootstrap consumers/cutover, and migrate stats / meminfo surfaces.
6. **Track E (cross-CPU free):** Add atomic free list per CPU per size class and implement MPSC push + batch collection.
7. **Track F.2-F.4 (integration):** Add allocator-local reclaim, kill switch, and final stress / compatibility validation.

Each track has independent host-testable unit tests in `kernel-core` and QEMU integration validation.

## Acceptance Criteria

- `allocate_frame()` cache-hit fast path does not acquire any global lock and mutates only CPU-local state with interrupts masked or an equivalent local non-reentrancy guard
- Global cache drain / reclaim never performs unsynchronized remote mutation of another CPU's lockless cache
- Kernel object allocation (`Box::new`, `Vec::push`) routes through size-class slab caches for small objects, while layouts >4 KiB or requiring stronger alignment use a documented page-backed path
- Per-CPU magazine allocation completes without lock acquisition in the common case, with out-of-line slab metadata that keeps the 4 KiB size class usable
- `free_frame()` acquires the frame allocator lock at most once (not twice)
- Buddy free-block removal no longer linearly scans `Vec::position()`
- Refcount atomics use Acquire/Release ordering (not SeqCst)
- All existing QEMU tests pass unchanged (API-compatible)
- `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` passes with new allocator unit tests, and loom coverage exists for new cross-CPU free / ordering-sensitive queues
- `frame_stats()`, `heap_stats()`, `/proc/meminfo`, and `sys_meminfo` show updated statistics reflecting the new allocator layers
- Cross-CPU free of slab objects does not acquire the victim CPU's slab lock
- Any frame that can become user-visible is zeroed before exposure, including contiguous and lazy-fault / CoW-sensitive paths that depend on allocator zeroing
- A bring-up kill switch or equivalent fallback exists while the new allocator is being integrated

## Companion Task List

- [Phase 53a Task List](./tasks/53a-kernel-memory-modernization-tasks.md)

## How Real OS Implementations Differ

- Linux SLUB uses a lockless CAS+tid scheme for per-CPU allocation; m3OS uses the simpler CPU-local fast path with interrupt masking because it lacks kernel preemption but still must handle IRQ re-entrancy
- FreeBSD UMA supports constructor/destructor caching for objects with expensive initialization; Phase 53a defers this as it adds complexity with uncertain benefit for m3OS's current object types
- Linux has 6 migration types for anti-fragmentation; m3OS defers migration-type support as it's primarily valuable for memory compaction and hotplug
- Production allocators support NUMA-aware per-domain caching; m3OS defers NUMA but the per-CPU design is forward-compatible
- Linux KASAN provides compile-time-instrumented shadow memory for all memory accesses; m3OS defers full sanitizer support but adds freelist pointer hardening

## Deferred Until Later

- NUMA-aware per-domain slab and page caches (forward-compatible design, implement when NUMA target exists)
- Constructor/destructor object caching (add when profiling shows object init is a bottleneck)
- Full memory debugging suite (red zones, poison fill, KFENCE-style sampling) — planned for a later sub-phase or Phase 53
- Memory pressure callbacks (shrinker interface) for non-allocator caches — deferred to Phase 54; 53a only adds allocator-local drain/reclaim hooks
- Full GFP-like allocation-context flags — 53a defines a minimal sleepable vs IRQ-sensitive contract, richer policy comes later
- Type-state `Frames<Free/Allocated/Mapped>` wrappers — high value but large refactor surface; evaluate after core allocator lands
