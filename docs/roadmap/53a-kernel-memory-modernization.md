# Phase 53a - Kernel Memory Modernization

**Status:** Planned
**Source Ref:** phase-53a
**Depends on:** Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP) ✅, Phase 36 (Expanded Memory) ✅, Phase 52c (Kernel Architecture Evolution) ✅, Phase 52d (Kernel Completion) ✅
**Builds on:** Replaces the Phase 33 linked-list heap and unused slab caches with a modern, SMP-scalable, per-CPU-cached allocator stack informed by the research in `docs/research/`
**Primary Components:** kernel/src/mm/, kernel-core/src/buddy.rs, kernel-core/src/slab.rs, kernel/src/mm/heap.rs, kernel/src/mm/frame_allocator.rs, kernel/src/mm/slab.rs

## Milestone Goal

The kernel memory subsystem becomes SMP-scalable and production-grade: per-CPU page caching eliminates the global frame-allocator lock from the hot path, a magazine-based slab allocator with geometric size classes replaces the linked-list heap for kernel objects, and foundational correctness issues (interrupt deadlock risk, double-lock, O(n) coalescing) are eliminated.

## Why This Phase Exists

Phase 33 established a correct-but-minimal memory subsystem: buddy allocator, linked-list heap, and slab cache infrastructure. That was appropriate for the learning goal at the time. Since then, Phase 25 added SMP, Phase 35 added per-core syscalls and scheduling, and Phase 52 began extracting services to userspace — all of which increase allocation pressure and concurrency. The current allocator has fundamental scalability limitations:

- Every allocation (heap or frame) serializes through a single global `spin::Mutex`
- The slab caches defined in Phase 33 were never integrated — all objects go through the O(n) linked-list heap
- There is no interrupt-safety mechanism — a page fault while the frame lock is held causes deadlock
- The buddy allocator's coalescing path has an O(n) linear scan (`remove_free`)

Phase 54 (Deep Serverization) will extract storage, namespace, and networking to userspace, creating heavy IPC and cross-CPU allocation patterns. The allocator must be modernized before that work begins.

## Learning Goals

- Understand why per-CPU caching is the dominant design pattern in every production kernel allocator
- Learn the magazine/bucket pattern (Bonwick & Adams, FreeBSD UMA) and how it amortizes lock contention
- Understand size-class-based allocation and why geometric spacing bounds internal fragmentation
- See how Rust's type system can enforce memory safety invariants at compile time (type-state frames)
- Learn the trade-offs between embedded freelists (SLUB) and bitmap allocation (current m3OS)

## Feature Scope

### Per-CPU page cache (Track A)

Each CPU maintains a hot-page list (batch size 32-64). `allocate_frame()` pops from the per-CPU list without acquiring the global buddy lock. When the list is empty, a batch refill acquires the buddy lock once for 32-64 frames. `free_frame()` pushes to the per-CPU list; when it exceeds the high watermark, a batch drain returns pages to the buddy. This reduces buddy lock contention by 32-64x.

### Slab allocator with per-CPU magazines (Track B)

Replace the unused `SlabCache` with an embedded-freelist slab allocator and per-CPU magazine layer. Each size class has two magazines per CPU (loaded + previous, per the Bonwick/Adams pattern). The allocation fast path pops from the loaded magazine with preemption disabled — no lock, no atomic. When magazines are exhausted, they swap with the depot (per-size-class lock). The slab layer uses embedded freelist pointers inside free objects for O(1) allocation.

### Size-class-based GlobalAlloc (Track C)

Replace `linked_list_allocator::LockedHeap` with a custom `GlobalAlloc` that routes allocations to the appropriate size-class slab cache. 13 geometric size classes (32B to 4KiB, 4 steps per doubling, <20% max internal waste). Allocations >4 KiB go directly to the buddy allocator.

### Foundation fixes (Track D)

- Fix `free_frame` double-lock: cache `phys_offset` in a static after init
- Fix `remove_free` O(n): replace Vec-based free lists with intrusive lists in buddy
- Fix SeqCst refcount ordering: downgrade to Acquire/Release
- Move frame zeroing from free-time to alloc-time (zero only when requested)
- Add freelist pointer hardening (XOR with random + address)

### Cross-CPU free path (Track E)

When a CPU frees an object allocated on a different CPU's slab, it pushes to the owner CPU's atomic free list via CAS (mimalloc MPSC pattern). The owning CPU batch-collects via `atomic_exchange` during its next allocation. No lock acquisition for cross-CPU free.

## Important Components and How They Work

### Per-CPU page cache (`kernel/src/mm/frame_allocator.rs`)

A fixed-size array of physical frame addresses per CPU, accessed via `smp::per_core()`. Preemption is not a concern (m3OS has cooperative scheduling). The per-CPU cache sits between callers and the global `BuddyAllocator`, absorbing the vast majority of single-page allocations.

### Magazine depot (`kernel/src/mm/slab.rs` or new `kernel/src/mm/magazine.rs`)

Per-size-class stacks of full and empty magazines. Protected by a per-size-class spinlock. Magazines are fixed-capacity arrays of object pointers (capacity 32). When a CPU's loaded magazine empties, it swaps with the previous magazine. If both are empty, the empty loaded magazine is exchanged for a full one from the depot. This amortizes the depot lock to 1 acquisition per 32 allocations.

### Embedded-freelist slab (`kernel-core/src/slab.rs` rewrite)

Each free object contains a pointer to the next free object. Allocation is a single pointer follow. The freelist head is stored in slab metadata (not in the slab page data), preserving cache separation. Partial slabs are linked in a list per size class for O(1) slab selection.

### Size-class router (`kernel/src/mm/heap.rs` rewrite)

The `#[global_allocator]` maps allocation sizes to slab caches via a lookup table. `Layout::size()` → size class index → slab cache. This replaces the O(n) linked-list scan with O(1) dispatch.

### Buddy allocator improvements (`kernel-core/src/buddy.rs`)

Replace `Vec<usize>` free lists with intrusive doubly-linked lists stored in the free pages themselves (the bootstrap free-list already demonstrates this pattern). This eliminates the heap dependency and makes `remove_free` O(1) via direct list removal.

## How This Builds on Earlier Phases

- Replaces Phase 33's `LockedHeap` + unused `SlabCache` with a production-grade allocator stack
- Preserves Phase 33's `BuddyAllocator` as the cold-path backend, improving it with intrusive lists
- Preserves Phase 36's demand paging and mprotect — the new allocator uses the same frame allocation API
- Preserves Phase 17's per-frame refcount semantics for CoW fork
- Uses Phase 25/35's per-CPU infrastructure (`smp::per_core()`, APIC ID) for per-CPU caches
- Informed by Phase 52c's architectural evolution pattern (VmaTree replaced Vec, same approach here)

## Implementation Outline

1. **Track D (foundation fixes):** Fix double-lock, O(n) remove_free, SeqCst ordering, frame zeroing. These are low-risk, high-value fixes that can land independently.
2. **Track A (per-CPU page cache):** Add per-CPU frame lists, modify `allocate_frame()` / `free_frame()` to check per-CPU first. Batch refill/drain from buddy.
3. **Track B (slab + magazines):** Rewrite `kernel-core/src/slab.rs` with embedded freelist. Add magazine layer. Create size-class caches.
4. **Track C (GlobalAlloc replacement):** Wire size-class slab caches into a new `GlobalAlloc` implementation. Remove `linked_list_allocator` dependency.
5. **Track E (cross-CPU free):** Add atomic free list per CPU per size class. Implement MPSC push + batch collection.

Each track has independent host-testable unit tests in `kernel-core` and QEMU integration validation.

## Acceptance Criteria

- `allocate_frame()` fast path does not acquire any global lock (per-CPU cache hit)
- Kernel object allocation (`Box::new`, `Vec::push`) routes through size-class slab caches, not a linked-list scan
- Per-CPU magazine allocation completes without lock acquisition in the common case
- `free_frame()` acquires the frame allocator lock at most once (not twice)
- Buddy `remove_free` is O(1) via intrusive list removal
- Refcount atomics use Acquire/Release ordering (not SeqCst)
- All existing QEMU tests pass unchanged (API-compatible)
- `cargo test -p kernel-core` passes with new allocator unit tests (buddy intrusive lists, slab embedded freelist, magazine fill/drain, size-class routing)
- `meminfo` command shows updated statistics reflecting the new allocator layers
- Cross-CPU free of slab objects does not acquire the victim CPU's slab lock
- Frame zeroing occurs at allocation time (when requested), not unconditionally on free

## Companion Task List

- [Phase 53a Task List](./tasks/53a-kernel-memory-modernization-tasks.md)

## How Real OS Implementations Differ

- Linux SLUB uses a lockless CAS+tid scheme for per-CPU allocation; m3OS uses the simpler preemption-disabled approach (viable because m3OS has no kernel preemption)
- FreeBSD UMA supports constructor/destructor caching for objects with expensive initialization; Phase 53a defers this as it adds complexity with uncertain benefit for m3OS's current object types
- Linux has 6 migration types for anti-fragmentation; m3OS defers migration-type support as it's primarily valuable for memory compaction and hotplug
- Production allocators support NUMA-aware per-domain caching; m3OS defers NUMA but the per-CPU design is forward-compatible
- Linux KASAN provides compile-time-instrumented shadow memory for all memory accesses; m3OS defers full sanitizer support but adds freelist pointer hardening

## Deferred Until Later

- NUMA-aware per-domain slab and page caches (forward-compatible design, implement when NUMA target exists)
- Constructor/destructor object caching (add when profiling shows object init is a bottleneck)
- Full memory debugging suite (red zones, poison fill, KFENCE-style sampling) — planned for a later sub-phase or Phase 53
- Memory pressure callbacks (shrinker interface) — needed before Phase 54 deep serverization
- Allocation-context flags (GFP-like system) — add incrementally as interrupt-context allocation paths are identified
- Type-state `Frames<Free/Allocated/Mapped>` wrappers — high value but large refactor surface; evaluate after core allocator lands
