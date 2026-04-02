# Phase 36 - Expanded Memory

**Status:** Planned
**Source Ref:** phase-36
**Depends on:** Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP Multitasking) ✅
**Builds on:** Extends Phase 33's buddy allocator, slab caches, and munmap with demand paging, VMA-based fault resolution, and mprotect. Reuses Phase 35's TLB shootdown infrastructure for cross-core permission updates.
**Primary Components:** mm/paging, mm/frame_allocator, process/mod, arch/x86_64/interrupts, arch/x86_64/syscall, xtask

## Milestone Goal

The kernel supports demand paging, large `mmap()` regions, and `mprotect()`. Virtual
pages are mapped lazily — physical frames are allocated on first access via the page
fault handler. This unlocks running large cross-compiled binaries (Clang, Python,
Node.js, git) that allocate hundreds of megabytes via `mmap()` but only touch a
fraction of the mapped pages. QEMU RAM is increased to support these workloads.

## Why This Phase Exists

The current `mmap()` implementation eagerly allocates a physical frame for every
virtual page at map time. A 256 MB mmap region immediately consumes 256 MB of
physical RAM even if the program only touches a few pages. This is the single
biggest blocker for running cross-compiled toolchains — Clang allocates large
arenas but only writes to a fraction. Without demand paging, the kernel runs out
of physical memory before large programs can start. Additionally, `mprotect()` is
stubbed as a no-op, which blocks JIT compilers (V8) and runtimes with stack guard
pages (Go).

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
   - Yes, and the page is lazy-mapped -> allocate a frame, zero-fill it, map it, resume.
   - Yes, and the page is CoW -> copy the frame (existing Phase 17 logic).
   - No -> deliver SIGSEGV to the process.

### VMA Tracking with Protection Bits

Extend the existing `MemoryMapping` struct to include protection flags (`prot`) and
mapping flags (`flags`). The page fault handler uses these to determine valid faults
and to set correct page permissions when demand-mapping a frame.

### Large `mmap()` Regions

Support allocations of 256+ MB contiguous virtual address space. With demand paging,
these regions consume near-zero physical memory until pages are touched.

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

Raise default QEMU memory from 256 MB to 1 GB. This is a one-line change in xtask's
QEMU arguments (`-m 1G`).

### Disk Image Expansion

Grow the data partition from 128 MB to 1 GB to accommodate cross-compiled toolchains
(Clang ~150 MB, Python ~60 MB, git ~20 MB, Node.js ~80 MB).

### Optional: Overcommit

Allow `mmap()` to promise more virtual memory than physical RAM available, relying
on demand paging to only consume physical memory for touched pages. Conservative
approach: allow overcommit up to 2x physical RAM.

## Important Components and How They Work

### VMA List (`MemoryMapping`)

Currently in `kernel/src/process/mod.rs`. Each process has a `Vec<MemoryMapping>`
that records mapped regions. Phase 36 extends this with `prot` and `flags` fields.
The page fault handler walks this list to validate lazy faults.

### Page Fault Handler

In `kernel/src/arch/x86_64/interrupts.rs`. Currently handles CoW faults (Phase 17)
and stack demand-paging. Phase 36 extends it to check the VMA list for any faulting
address. The decision chain becomes: CoW -> VMA lazy fault -> stack growth -> SIGSEGV.

### `demand_map_user_page()`

In `kernel/src/arch/x86_64/interrupts.rs`. Existing function that walks page tables,
allocates intermediate levels as needed, maps a zero-filled frame, and flushes TLB.
Phase 36 generalizes this to accept protection flags from the VMA.

### `sys_linux_mmap()`

In `kernel/src/arch/x86_64/syscall.rs`. Currently eagerly allocates frames in a loop.
Phase 36 changes it to only record the VMA and return the virtual address without
allocating any physical frames.

### `mprotect()` Implementation

New logic in the syscall handler that walks the page table for a virtual range,
updates PTE permission bits, splits VMAs at mprotect boundaries, and issues TLB
shootdown via existing SMP infrastructure.

## How This Builds on Earlier Phases

- Extends Phase 33 by converting the eager mmap allocator to a lazy one while keeping
  munmap, buddy allocator, and slab caches unchanged.
- Extends Phase 17's CoW page fault handling by adding a second fault resolution path
  for lazy-mapped VMA pages.
- Reuses Phase 35's TLB shootdown IPI for mprotect cross-core invalidation.
- Reuses Phase 33's `MemoryMapping` struct, adding protection and flags fields.

## Implementation Outline

1. Extend `MemoryMapping` with `prot` and `flags` fields.
2. Change `sys_linux_mmap()` to record the VMA without allocating frames.
3. Extend the page fault handler to check the VMA list and demand-map valid lazy faults.
4. Implement `mprotect()` syscall with page table walks and TLB shootdown.
5. Increase QEMU RAM to 1 GB.
6. Expand data partition to 1 GB.
7. Validate CoW fork still works alongside demand paging.
8. Test with large mmap regions (256 MB+) and a cross-compiled binary.

## Acceptance Criteria

- `mmap(MAP_ANONYMOUS)` returns immediately without allocating physical frames.
- First access to a lazy-mapped page triggers a page fault that allocates a frame.
- Programs can `mmap()` 256 MB regions without exhausting physical memory.
- `mprotect()` changes page permissions and flushes TLB.
- Existing CoW fork (Phase 17) still works correctly alongside demand paging.
- QEMU boots with 1 GB RAM.
- Data partition is 1 GB.
- A large cross-compiled binary (e.g., Python ~8 MB) loads and runs.

## Companion Task List

- [Phase 36 Task List](./tasks/36-expanded-memory-tasks.md)

## How Real OS Implementations Differ

- Real kernels (Linux, FreeBSD) use red-black trees for VMA lookup (O(log n) instead
  of our linear scan).
- Reverse mapping (rmap) to find all PTEs mapping a given physical page.
- Memory-mapped files (not just anonymous mappings).
- NUMA-aware page allocation.
- Transparent huge pages (THP) that automatically promote 4K pages to 2M pages.
- KSM (Kernel Same-page Merging) for deduplication.
- Swap with multiple swap devices and priority ordering.

## Deferred Until Later

- Swap to disk — page out cold memory to virtio-blk
- `setrlimit()` per-process limits
- OOM killer — kernel still panics on true physical memory exhaustion
- Huge pages (2 MiB / 1 GiB) — performance optimization
- Memory-mapped files (`mmap` with file descriptors)
- NUMA-aware allocation
