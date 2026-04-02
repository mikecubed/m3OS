# Phase 33 — Kernel Memory Improvements: Task List

**Status:** Complete
**Source Ref:** phase-33
**Depends on:** Phase 17 (Memory Reclamation) ✅, Phase 25 (SMP) ✅
**Builds on:** Extends Phase 17 refcounted frame reclamation and Phase 25 TLB shootdown, while replacing the earlier free-list frame allocator and `munmap()` stub.
**Goal:** Make the kernel memory subsystem robust and efficient: OOM-resilient heap, buddy frame allocator, slab caches for kernel objects, working `munmap()`, userspace heap coalescing, and memory statistics reporting.
**Deferred Follow-ups:** A.4 dedicated OOM QEMU stress test, C.4 broad slab-backed object migration, D.4 userspace `munmap` loop binary, E.3 userspace heap coalescing binary, G.2 full memory stress test.

## Prerequisite Analysis

Baseline before Phase 33 work (preserved from the original planning pass):
- Kernel heap: `linked_list_allocator::LockedHeap` at `0xFFFF_8000_0000_0000`
  - 4 MiB initial, grows dynamically up to 64 MiB cap
  - `try_grow_on_oom()` attempts 1 MiB growth but `alloc_error_handler` cannot retry
- Frame allocator: intrusive free-list with per-frame refcounting (`AtomicU16`)
  - Double-free detection via magic sentinel
  - No buddy system — O(1) alloc/free but no coalescing of adjacent frames
- `munmap()`: stub (returns 0 but does not reclaim frames)
- Userspace `BrkAllocator`: first-fit linked-list, no free-block coalescing
- SMP: multi-core boot with TLB shootdown infrastructure (needed for `munmap`)
- No slab allocator, no buddy allocator, no heap statistics

Already implemented before this phase:
- Per-frame refcounting (Phase 17)
- Dynamic kernel heap growth (`grow_heap()`)
- TLB shootdown via IPI (Phase 25)
- User page table management (map/unmap primitives exist)
- `brk` syscall with per-process tracking

Phase 33 added or replaced the following:
- OOM retry wrapper around the global allocator instead of relying on the diverging error handler
- Buddy allocator for page-granularity frame management
- Slab cache infrastructure for fixed-size kernel allocations
- Working `munmap()` with frame reclamation and TLB invalidation
- Free-block coalescing in userspace `BrkAllocator`
- Kernel heap, frame, and slab statistics reporting

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | OOM retry: wrap global allocator to retry after heap growth | — | Done (A.4 QEMU test deferred) |
| B | Buddy frame allocator: replace free-list with buddy system | — | Done |
| C | Slab allocator: fixed-size caches for kernel objects | B | Done (C.4 migration deferred) |
| D | Working `munmap()`: frame reclamation + TLB shootdown | B | Done (D.4 test binary deferred) |
| E | Userspace heap coalescing in `BrkAllocator` | — | Done (E.3 test binary deferred) |
| F | Kernel heap statistics and reporting | A, C | Done |
| G | Integration testing and documentation | All | Done (G.2 stress test deferred) |

### Implementation Notes

- **Buddy allocator in `kernel-core`**: Pure logic lives in `kernel-core/src/buddy.rs` so it can be unit-tested on the host via `cargo test -p kernel-core`. The kernel-side wrapper in `kernel/src/mm/frame_allocator.rs` handles physical memory specifics.
- **Slab allocator also in `kernel-core`**: The slab cache logic is also pure data-structure management. Kernel integration in `kernel/src/mm/slab.rs` only provides the backing pages and boot-time wiring.
- **OOM retry cannot live in `alloc_error_handler`**: The handler is `-> !` (diverging). Phase 33 therefore moves retry logic into `RetryAllocator`, which can grow the heap and retry before panicking.
- **Buddy allocator preserves refcounting**: The existing `AtomicU16` refcount table remains authoritative for shared frames. The buddy allocator manages free/allocated state; refcounts still gate reclamation.
- **`munmap()` extends earlier VM work**: It reuses prior page-table primitives and the Phase 25 TLB shootdown path, but replaces the earlier stubbed Linux-compatible syscall behavior.
- **Deferred work stays explicit**: The allocator, `munmap()`, and stats paths are complete, but several dedicated stress binaries remain intentionally deferred and are kept visible below.

---

## Track A — OOM Retry Allocator Wrapper

This track replaces the old “panic on first allocation failure” behavior with a retrying allocator that can grow the heap before falling through to the panic-only error handler.

### A.1 — Create `RetryAllocator` wrapper struct

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `RetryAllocator`
**Why it matters:** This wrapper replaces the old direct global allocator so kernel allocations can retry after heap growth instead of immediately failing.

**Acceptance:**
- [x] `RetryAllocator` wraps `LockedHeap` and is installed as the `#[global_allocator]`
- [x] Existing heap initialization flow continues to work without changing the boot sequence

### A.2 — Remove diverging `alloc_error_handler` retry path

**File:** `kernel/src/main.rs`
**Symbol:** `alloc_error_handler`
**Why it matters:** Keeping the error handler panic-only makes the retry path live entirely in `RetryAllocator`, where allocation can still return and retry safely.

**Acceptance:**
- [x] `alloc_error_handler` is reduced to a panic-only terminal path
- [x] Heap growth retry logic no longer lives in the diverging error handler

### A.3 — Add retry with exponential growth

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `try_grow_on_oom_for_layout`
**Why it matters:** This extends the earlier heap-growth behavior so larger allocations can trigger proportional growth instead of only fixed-size fallback attempts.

**Acceptance:**
- [x] Retry attempts multiple growth increments instead of a single fixed retry
- [x] Large allocations can trigger layout-aware growth before falling back to 1 MiB, 2 MiB, and 4 MiB retries
- [x] Growth still respects the 64 MiB heap cap

### A.4 — Stress test OOM retry

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `RetryAllocator`
**Why it matters:** A dedicated QEMU stress test would prove the retrying allocator remains stable when the heap crosses the original 4 MiB limit and approaches the hard cap.

**Deferred:** The allocator behavior is implemented, but the standalone QEMU stress test called out in the original plan remains deferred.

**Acceptance:**
- [ ] A dedicated QEMU test exercises repeated allocations until automatic heap growth occurs
- [ ] The test verifies growth beyond the initial heap size and eventual failure at the configured cap

---

## Track B — Buddy Frame Allocator

This track replaces the earlier intrusive free-list allocator with a coalescing buddy allocator while preserving the refcounting introduced by the memory-reclamation work.

### B.1 — Implement buddy allocator data structure in `kernel-core`

**File:** `kernel-core/src/buddy.rs`
**Symbol:** `BuddyAllocator`
**Why it matters:** The host-testable buddy allocator provides split-and-merge frame management that the earlier free-list design could not do.

**Acceptance:**
- [x] `BuddyAllocator` manages page-frame ranges without kernel-only dependencies
- [x] Allocation works for single pages and higher orders
- [x] Freeing a block merges with its buddy when possible
- [x] Exhaustion returns `None`

### B.2 — Host-side unit tests for buddy allocator

**File:** `kernel-core/src/buddy.rs`
**Symbol:** `BuddyAllocator`
**Why it matters:** Host-side tests make the new allocator behavior verifiable without booting QEMU for every split, merge, and exhaustion scenario.

**Acceptance:**
- [x] Unit tests cover allocation, exhaustion, splitting, merging, and small-region edge cases
- [x] The buddy allocator test module passes via `cargo test -p kernel-core`

### B.3 — Integrate buddy allocator into kernel frame allocator

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `init_buddy`
**Why it matters:** This integration replaces the earlier free-list pool with buddy-backed frame allocation while keeping the kernel’s refcounted reclamation rules intact.

**Acceptance:**
- [x] The kernel boots successfully with the buddy allocator backing frame allocation
- [x] Single-frame allocation and free paths delegate through the buddy allocator after initialization
- [x] Free-count and refcount-based safety checks continue to work

### B.4 — Add multi-page allocation API

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `allocate_contiguous`
**Why it matters:** Contiguous allocation extends the frame allocator so later slab and DMA-style consumers can request multi-page backing from the buddy system.

**Acceptance:**
- [x] `allocate_contiguous` exposes higher-order buddy allocations through the kernel frame allocator API
- [x] Single-page and multi-page requests both return the base physical frame for the allocated range
- [x] Freed higher-order blocks merge back into the buddy allocator

---

## Track C — Slab Allocator

This track adds slab-cache infrastructure on top of the buddy allocator, preparing the kernel to replace generic heap allocation for frequent fixed-size objects.

### C.1 — Implement slab cache data structure in `kernel-core`

**File:** `kernel-core/src/slab.rs`
**Symbol:** `SlabCache`
**Why it matters:** The slab cache provides reusable fixed-size allocation logic that reduces fragmentation compared with using the general-purpose heap for every kernel object.

**Acceptance:**
- [x] `SlabCache` allocates and frees fixed-size objects correctly
- [x] New slabs are requested when existing slabs are full
- [x] Empty slabs can be detected and released back to the page source

### C.2 — Host-side unit tests for slab cache

**File:** `kernel-core/src/slab.rs`
**Symbol:** `SlabCache`
**Why it matters:** Host tests validate slab reuse and object-slot accounting independently from the kernel integration path.

**Acceptance:**
- [x] Unit tests cover full-slab growth, freeing, mixed alloc/free patterns, and multiple object sizes
- [x] The slab allocator test module passes via `cargo test -p kernel-core`

### C.3 — Integrate slab caches into kernel

**Files:**
- `kernel/src/mm/slab.rs`
- `kernel/src/mm/mod.rs`

**Symbol:** `KernelSlabCaches`
**Why it matters:** This wires the new slab infrastructure into kernel boot and makes buddy-backed caches available for future object-specific migration.

**Acceptance:**
- [x] Kernel slab caches are initialized during memory-subsystem startup
- [x] Cache backing pages come from the Phase 33 buddy allocator integration
- [x] Kernel boot and existing tests continue to pass with slab infrastructure enabled

### C.4 — Migrate kernel object allocations to slab caches

**Files:**
- `kernel/src/task/mod.rs`
- `kernel/src/process/mod.rs`
- `kernel/src/ipc/endpoint.rs`
- `kernel/src/pipe.rs`

**Symbol:** `KernelSlabCaches`
**Why it matters:** Migrating hot kernel objects to slab caches is the follow-on step that would replace generic heap allocation for the most frequent fixed-size structures.

**Deferred:** The slab-cache infrastructure is in place, but the broad object-allocation migration from the original plan remains deferred.

**Acceptance:**
- [ ] At least two frequently allocated kernel object types use slab-backed allocation paths
- [ ] Those migrated objects avoid general-purpose heap traversal on their fast allocation path
- [ ] Existing tests continue to pass after the migration work lands

---

## Track D — Working `munmap()`

This track replaces the earlier stubbed syscall with real unmapping, frame reclamation, and SMP-safe TLB invalidation.

### D.1 — Implement `sys_linux_munmap()` with page table walk

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/mm/user_space.rs`

**Symbol:** `sys_linux_munmap`
**Why it matters:** This replaces the earlier no-op `munmap()` behavior with actual page-table teardown and frame reclamation.

**Acceptance:**
- [x] `munmap()` validates alignment, length, and userspace address range before unmapping
- [x] Unmapped pages have their physical frames released through the refcount-aware frame allocator path
- [x] Page-table entries are cleared for successfully unmapped pages

### D.2 — SMP TLB shootdown for `munmap()`

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/smp/tlb.rs`

**Symbol:** `tlb_shootdown`
**Why it matters:** Reusing the earlier Phase 25 shootdown path ensures other cores do not keep stale translations after `munmap()` clears page-table entries.

**Acceptance:**
- [x] `munmap()` triggers TLB shootdown for pages that were actually unmapped
- [x] The unmap path reuses the SMP invalidation infrastructure instead of inventing a separate flush mechanism
- [x] Unmapped addresses fault correctly instead of remaining accessible through stale TLB state

### D.3 — Track process memory mappings

**File:** `kernel/src/process/mod.rs`
**Symbol:** `MemoryMapping`
**Why it matters:** Tracking active mappings extends the earlier `mmap_next` bookkeeping so `munmap()` can validate, shrink, or split real mapped ranges.

**Acceptance:**
- [x] `Process` keeps a list of active `MemoryMapping` ranges
- [x] `mmap()` records new mappings and `munmap()` updates or removes them
- [x] Unmapping an invalid range is rejected instead of silently succeeding

### D.4 — Userspace test: `mmap`/`munmap` loop

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_munmap`
**Why it matters:** A dedicated userspace loop binary would verify that repeated map/unmap cycles stay memory-stable from the syscall boundary outward.

**Deferred:** The syscall implementation is complete, but the standalone userspace regression binary from the original plan remains deferred.

**Acceptance:**
- [ ] A userspace test repeatedly maps, writes, unmaps, and reuses memory without exhausting system memory
- [ ] The test demonstrates bounded memory usage over many iterations

---

## Track E — Userspace Heap Coalescing

This track extends the existing userspace `BrkAllocator` so free blocks merge instead of fragmenting the process heap over time.

### E.1 — Add coalescing on `dealloc()`

**File:** `userspace/syscall-lib/src/heap.rs`
**Symbol:** `BrkAllocator::dealloc`
**Why it matters:** Coalescing adjacent free blocks fixes the fragmentation behavior left behind by the earlier first-fit-only heap implementation.

**Acceptance:**
- [x] Freeing a block merges with adjacent free blocks when their addresses are contiguous
- [x] Reallocation can reuse the resulting larger coalesced block
- [x] The merge logic avoids double-free and free-list corruption

### E.2 — Maintain sorted free list for efficient coalescing

**File:** `userspace/syscall-lib/src/heap.rs`
**Symbol:** `BrkAllocator::dealloc`
**Why it matters:** Keeping the free list sorted by address makes left-merge and right-merge detection reliable without changing the allocator’s earlier first-fit search model.

**Acceptance:**
- [x] Freed blocks are reinserted in address order
- [x] Coalescing works for both left-neighbor and right-neighbor cases
- [x] First-fit allocation semantics continue to work correctly

### E.3 — Userspace heap coalescing test

**File:** `userspace/syscall-lib/src/heap.rs`
**Symbol:** `BrkAllocator`
**Why it matters:** A dedicated userspace binary would prove that the coalescing logic restores large allocations after many small blocks are freed.

**Deferred:** The coalescing implementation is present in `syscall-lib`, but the standalone userspace regression binary remains deferred.

**Acceptance:**
- [ ] A userspace test frees many small blocks and then succeeds at a large allocation that depends on coalescing
- [ ] The test runs successfully under QEMU

---

## Track F — Kernel Heap Statistics

This track adds observability for the new memory subsystem so the kernel can report heap, frame, and slab behavior under load.

### F.1 — Implement `heap_stats()` function

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `heap_stats`
**Why it matters:** Heap statistics make the new retrying allocator observable instead of leaving heap growth and allocation churn hidden.

**Acceptance:**
- [x] `HeapStats` reports total, used, and free heap bytes
- [x] Allocation and deallocation counts are tracked through atomics in the retry allocator path

### F.2 — Add frame allocator statistics

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `frame_stats`
**Why it matters:** Buddy-specific frame statistics show whether the new allocator is preserving free space across orders instead of fragmenting the frame pool.

**Acceptance:**
- [x] `FrameStats` reports total, free, and allocated frame counts
- [x] Per-order buddy statistics are exposed for the current frame allocator state

### F.3 — Add slab cache statistics

**File:** `kernel/src/mm/slab.rs`
**Symbol:** `all_slab_stats`
**Why it matters:** Per-cache slab statistics show whether each kernel slab cache is populated, active, and reusable before broader migration work lands.

**Acceptance:**
- [x] Each kernel slab cache reports slab and slot utilization information
- [x] Slab statistics are available to kernel debug and reporting paths

### F.4 — Expose stats via `meminfo` debug syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_meminfo`
**Why it matters:** Exposing memory stats to userspace makes Phase 33 observable from the shell instead of only through kernel-internal instrumentation.

**Acceptance:**
- [x] `meminfo` reports heap, frame, and slab statistics through the syscall path
- [x] The reported values remain useful under load rather than being placeholder output

---

## Track G — Integration Testing and Documentation

This track verifies that the new allocators, `munmap()` behavior, and reporting surfaces work together and are documented for later phases.

### G.1 — Run full test suite

**Files:**
- `kernel-core/src/buddy.rs`
- `kernel-core/src/slab.rs`
- `xtask/src/main.rs`

**Symbol:** `BuddyAllocator`
**Why it matters:** Host and QEMU validation are what confirm the Phase 33 replacements behave correctly when combined with the rest of the kernel.

**Acceptance:**
- [x] Existing QEMU tests pass with the new memory subsystem enabled
- [x] Existing `kernel-core` host tests pass for the buddy and slab implementations
- [x] `cargo xtask check` remains clean

### G.2 — Memory stress test

**Files:**
- `kernel/src/mm/heap.rs`
- `kernel/src/arch/x86_64/syscall.rs`

**Symbol:** `sys_meminfo`
**Why it matters:** A dedicated memory stress test would validate that the retry allocator, buddy allocator, slab infrastructure, and `munmap()` stay stable together over repeated pressure cycles.

**Deferred:** Core functionality is implemented, but the standalone end-to-end stress binary from the original plan remains deferred.

**Acceptance:**
- [ ] A dedicated stress test exercises heavy allocation, process churn, and repeated `mmap`/`munmap` cycles
- [ ] The test demonstrates that memory usage stays bounded across repeated cycles

### G.3 — Update documentation

**Files:**
- `docs/02-memory.md`
- `docs/33-kernel-memory.md`
- `docs/roadmap/tasks/33-kernel-memory-tasks.md`

**Symbol:** `Phase 33 — Kernel Memory Improvements: Task List`
**Why it matters:** The documentation captures what changed from earlier phases so later roadmap and snippet-generation work can explain the new memory architecture accurately.

**Acceptance:**
- [x] The memory architecture docs describe the buddy allocator, slab layer, and `munmap()` behavior
- [x] The Phase 33 design doc records the main design decisions and trade-offs
- [x] This task list reflects completion status and deferred follow-up work accurately

---

## Deferred Follow-ups

- **A.4** — Add a dedicated QEMU OOM stress test for `RetryAllocator`.
- **C.4** — Migrate hot kernel objects from generic heap allocation to slab caches.
- **D.4** — Add a userspace regression binary for repeated `mmap()`/`munmap()` cycles.
- **E.3** — Add a userspace regression binary that proves coalescing restores large allocations.
- **G.2** — Add a full end-to-end memory stress test spanning allocators, mappings, and reclamation.
