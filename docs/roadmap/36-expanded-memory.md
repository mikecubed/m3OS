# Phase 36 - Expanded Memory

## Milestone Goal

The kernel supports demand paging, large `mmap()` regions, and `mprotect()`. Virtual
pages are mapped lazily — physical frames are allocated on first access via the page
fault handler. This unlocks running large cross-compiled binaries (Clang, Python,
Node.js, git) that allocate hundreds of megabytes via `mmap()` but only touch a
fraction of the mapped pages. QEMU RAM is increased to support these workloads.

## Learning Goals

- Understand demand paging: why real OSes don't back every virtual page with a physical
  frame at map time.
- Learn how the page fault handler distinguishes lazy faults, stack growth, CoW, and
  true segfaults.
- See how `mprotect()` enables JIT compilers (V8) and stack guard pages (Go runtime).
- Understand memory overcommit and why it's the default on Linux.

## Feature Scope

### Demand Paging (Lazy Allocation)

`mmap()` maps virtual pages without immediately allocating physical frames. The page
fault handler allocates frames on first access:

1. Process calls `mmap(NULL, size, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)`.
2. Kernel records the virtual memory area (VMA) but does NOT allocate physical frames.
3. Process touches a page in the mapped region.
4. CPU generates a #PF (page fault).
5. Page fault handler checks: is the faulting address in a valid VMA?
   - Yes, and the page is lazy-mapped → allocate a frame, zero-fill it, map it, resume.
   - Yes, and the page is CoW → copy the frame (existing Phase 17 logic).
   - No → deliver SIGSEGV to the process.

**Virtual Memory Area (VMA) tracking:** Add a per-process VMA list that records
mapped regions (start, length, protection, flags). The page fault handler consults
this list to determine whether a fault is valid.

### Large `mmap()` Regions

Support allocations of 256+ MB contiguous virtual address space. The current `mmap`
implementation must handle regions this large without running out of virtual address
space or kernel metadata.

### `mprotect()` Syscall

Change page permissions on mapped regions:

```
mprotect(addr, len, prot)
```

Walk the page table for the specified range and update permission bits (read, write,
execute). Flush the TLB for affected pages (IPI shootdown on SMP).

**Use cases:**
- V8 JIT: allocate pages as RW, write machine code, then `mprotect()` to RX.
- Go runtime: stack guard pages use `mprotect(PROT_NONE)` to detect stack overflow.
- Sanitizers: shadow memory regions with varying permissions.

### QEMU RAM Increase

Raise default QEMU memory from 256 MB to 1 GB (or configurable). This is a one-line
change in xtask's QEMU arguments (`-m 1G`).

### Disk Image Expansion

Grow the ext2 data partition from 128 MB to 1 GB to accommodate cross-compiled
toolchains (Clang ~150 MB, Python ~60 MB, git ~20 MB, Node.js ~80 MB).

### Optional: Overcommit

Allow `mmap()` to promise more virtual memory than physical RAM available, relying
on demand paging to only consume physical memory for touched pages. Conservative
approach: allow overcommit up to 2x physical RAM.

## What This Does NOT Include (Deferred)

- **Swap to disk** — page out cold memory to virtio-blk. Deferred to a future phase.
- **`setrlimit()` per-process limits** — resource limits. Not needed yet.
- **OOM killer** — kernel still panics on true physical memory exhaustion. A proper
  OOM killer that terminates the most appropriate process is deferred.
- **Huge pages (2 MiB / 1 GiB)** — performance optimization, not required for
  correctness.

## Acceptance Criteria

- [ ] `mmap(MAP_ANONYMOUS)` returns immediately without allocating physical frames.
- [ ] First access to a lazy-mapped page triggers a page fault that allocates a frame.
- [ ] Programs can `mmap()` 256 MB regions without exhausting physical memory.
- [ ] `mprotect()` changes page permissions and flushes TLB.
- [ ] Existing CoW fork (Phase 17) still works correctly alongside demand paging.
- [ ] QEMU boots with 1 GB RAM.
- [ ] ext2 partition is 1 GB.
- [ ] A large cross-compiled binary (e.g., Python ~8 MB) loads and runs.

## How Real OSes Differ

Real kernels (Linux, FreeBSD) have sophisticated VM subsystems with:
- Red-black trees for VMA lookup (O(log n) instead of our linear scan).
- Reverse mapping (rmap) to find all PTEs mapping a given physical page.
- Memory-mapped files (not just anonymous mappings).
- NUMA-aware page allocation.
- Transparent huge pages (THP) that automatically promote 4K pages to 2M pages.
- KSM (Kernel Same-page Merging) for deduplication.
- Swap with multiple swap devices and priority ordering.

Our implementation is minimal: a VMA list, demand paging for anonymous mappings,
and `mprotect()`. This is sufficient for running cross-compiled toolchains.
