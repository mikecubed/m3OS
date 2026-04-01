# Phase 33 — Kernel Memory Improvements

**Status:** Complete
**Source Ref:** phase-33
**Depends on:** Phase 17 (Memory Reclamation) ✅, Phase 25 (SMP) ✅
**Builds on:** Replaces the bump frame allocator from Phase 17 with a buddy allocator; adds a slab allocator layer for kernel objects; fixes the stub munmap from Phase 17; adds SMP-aware TLB shootdown from Phase 25 to munmap
**Primary Components:** kernel-core/src/buddy.rs, kernel-core/src/slab.rs, kernel/src/mm/, userspace/coreutils-rs/ (meminfo)

## Milestone Goal

The kernel heap allocator is robust, efficient, and recoverable. Out-of-memory conditions
no longer panic the kernel — the allocator grows on demand and retries failed allocations.
A slab allocator handles fixed-size kernel objects with O(1) performance. Userspace
`munmap()` actually reclaims memory, and the userspace heap coalesces free blocks.

## Why This Phase Exists

The kernel's original memory allocator was a simple bump allocator that could never
reclaim freed frames, and the heap's OOM handler panicked unconditionally. As the OS
grew to support multiple processes, networking, and a compiler, memory pressure became
a real problem. This phase replaces the allocator stack with production-quality
components: a buddy allocator for efficient frame management, a slab allocator for
fast fixed-size kernel object allocation, working `munmap()` for virtual memory
reclamation, and an OOM retry path so the kernel does not crash under load.

## Learning Goals

- Understand the trade-offs between allocator designs: linked-list, buddy, slab.
- Learn why OOM-kill and graceful degradation matter more than panicking.
- See how slab allocators exploit fixed-size allocation patterns for near-zero fragmentation.
- Understand virtual memory reclamation and why `munmap()` must return frames.

## Feature Scope

### Kernel Heap OOM Retry

The current `alloc_error_handler` is `-> !` (diverging) — it calls `try_grow_on_oom()`
but then panics even if growth succeeded because it cannot retry the allocation.

**Fix:** Replace the global allocator with a wrapper that catches allocation failures,
grows the heap, and retries before invoking the panic path. Options:

1. Custom `GlobalAlloc` wrapper that calls `grow_heap()` then retries `alloc()`.
2. Use Rust's `allocator_api` (nightly) with a fallible allocator.
3. Increase the initial heap size and growth increments to reduce OOM frequency.

### Slab Allocator for Kernel Objects

Add a slab allocator layer on top of the page allocator for common fixed-size objects:

| Object type | Approximate size | Allocation frequency |
|---|---|---|
| `Task` struct | ~512 bytes | Every process spawn |
| File descriptor entry | ~64 bytes | Every open/dup |
| IPC endpoint | ~128 bytes | Every endpoint create |
| Page table page | 4096 bytes | Every address space op |
| Pipe buffer | ~4096 bytes | Every pipe create |
| Socket struct | ~256 bytes | Every socket create |

Each slab cache:
- Pre-allocates pages divided into fixed-size slots.
- Maintains a free-list of available slots (O(1) alloc and free).
- Grows by allocating additional pages when the free-list is empty.
- Optionally returns empty pages to the frame allocator.

### Buddy Allocator for Page-Granularity Allocations

Replace or augment the bump frame allocator with a buddy allocator:

- Powers-of-2 splitting and merging for allocations from 4 KiB to 2 MiB.
- O(log n) allocation and deallocation.
- Efficient coalescing of adjacent free blocks.
- Supports the slab allocator's page requests and large `mmap()` allocations.

### Working `munmap()`

The current `munmap()` is a stub that does nothing. Implement actual reclamation:

1. Walk the process page table and unmap the specified virtual range.
2. Free the underlying physical frames back to the frame allocator.
3. Invalidate TLB entries (with SMP TLB shootdown if needed).
4. Update the process's `mmap_next` tracking if appropriate.

### Userspace Heap Coalescing

The `BrkAllocator` in `syscall-lib` uses a first-fit linked-list but never merges
adjacent free blocks. Add coalescing:

1. On `dealloc()`, check if the freed block is adjacent to neighbors in the free list.
2. Merge contiguous free blocks into a single larger block.
3. Optionally return trailing pages to the kernel via `brk()` shrink.

### Kernel Heap Statistics

Add a `heap_stats()` function that reports:
- Total heap size (current and maximum)
- Bytes allocated / bytes free
- Number of allocations / deallocations
- Largest free block
- Slab cache utilization per object type

Expose via a debug syscall or `/proc/meminfo`-style interface.

## Important Components and How They Work

### Buddy Allocator (`kernel-core/src/buddy.rs`)

Implements a binary buddy system for physical frame allocation. Free lists are
maintained per order (0 = 4 KiB through 9 = 2 MiB). On allocation, the smallest
sufficient block is split; on deallocation, adjacent buddies are coalesced. The
allocator is host-testable via `kernel-core`.

### Slab Allocator (`kernel-core/src/slab.rs`)

Provides O(1) allocation for fixed-size kernel objects. Each slab cache manages pages
divided into equal-sized slots with a free-list. When the free-list is exhausted, a new
page is requested from the buddy allocator. Empty pages can be returned to reduce
memory pressure.

### munmap Implementation

Walks the process page table for the specified virtual range, unmaps each page, frees
the physical frame to the buddy allocator, and performs TLB invalidation. On SMP
systems, a TLB shootdown IPI is sent to other cores that may have cached the mapping.

### meminfo Utility

A Rust userspace utility (`coreutils-rs`) that reads kernel heap statistics via a
debug syscall and displays memory usage information, similar to `/proc/meminfo` on
Linux.

## How This Builds on Earlier Phases

- **Replaces Phase 17 (Memory Reclamation):** The bump frame allocator is replaced by a buddy allocator; the stub `munmap()` is replaced with a working implementation.
- **Extends Phase 25 (SMP):** Uses IPI-based TLB shootdown from Phase 25 when `munmap()` invalidates mappings on multi-core systems.
- **Extends Phase 17 (CoW):** The buddy allocator properly supports CoW page duplication and reclamation.

## Implementation Outline

1. Implement OOM retry wrapper around the global allocator.
2. Verify: allocation-heavy workloads no longer panic (heap grows and retries).
3. Implement buddy allocator for page-granularity allocations.
4. Implement slab allocator caches for Task, FD, endpoint, pipe, socket objects.
5. Implement working `munmap()` with frame reclamation and TLB invalidation.
6. Add free-block coalescing to userspace `BrkAllocator`.
7. Add `heap_stats()` and verify with a stress-test program.
8. Run existing test suite to verify no regressions.

## Acceptance Criteria

- Allocation-heavy workloads (many fork/exec cycles) no longer panic with OOM.
- The kernel heap grows on demand and the retry path works.
- Slab-allocated objects (Task, FD) allocate and free in O(1).
- `munmap()` returns physical frames; a program that maps and unmaps in a loop
  does not exhaust memory.
- Userspace programs that allocate and free many small objects do not fragment
  the heap excessively (coalescing verified).
- `heap_stats()` shows reasonable utilization under load.
- All existing tests pass without regression.

## Companion Task List

- [Phase 33 Task List](./tasks/33-kernel-memory-tasks.md)

## How Real OS Implementations Differ

Linux uses a multi-level allocator hierarchy:
- **Buddy allocator** manages physical pages (zone allocator with ZONE_DMA, ZONE_NORMAL, ZONE_HIGHMEM).
- **SLAB/SLUB/SLOB** provides object-level caching on top of the buddy allocator.
- **vmalloc** handles virtually contiguous but physically discontiguous allocations.
- **OOM killer** selects and kills processes when memory is exhausted rather than panicking.
- **Memory compaction** moves pages to create larger contiguous blocks.
- **Transparent Huge Pages (THP)** opportunistically uses 2 MiB pages.

Our approach implements the essential layers (buddy + slab + OOM retry) without the
advanced features (compaction, THP, OOM killer, NUMA awareness).

## Deferred Until Later

- OOM killer (kill processes instead of failing allocations)
- Memory compaction and defragmentation
- Transparent Huge Pages
- NUMA-aware allocation
- Kernel memory accounting and cgroups
- vmalloc for large virtually-contiguous allocations
- Memory pressure notifications
