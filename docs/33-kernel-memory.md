# Phase 33 — Kernel Memory Improvements

**Depends on:** Phase 17 (Memory Reclamation), Phase 25 (SMP)

## Goal

Make the kernel memory subsystem robust and efficient: OOM-resilient heap,
buddy frame allocator, slab caches for kernel objects, working `munmap()`,
userspace heap coalescing, and memory statistics reporting.

## Architecture

```
                    ┌─────────────────────────┐
                    │     RetryAllocator       │  ← #[global_allocator]
                    │  (wraps LockedHeap)      │
                    │  OOM → grow → retry      │
                    └────────────┬────────────┘
                                 │ extends heap by mapping
                                 │ frames from buddy
                    ┌────────────▼────────────┐
                    │    Buddy Allocator       │  ← kernel-core/src/buddy.rs
                    │  10 orders (4K → 2M)     │
                    │  bitmap + free lists     │
                    ├─────────────────────────┤
                    │  Per-frame refcounting   │  ← Vec<AtomicU16>
                    │  (CoW fork support)      │
                    └────────────┬────────────┘
                                 │ page alloc
                    ┌────────────▼────────────┐
                    │     Slab Caches          │  ← kernel-core/src/slab.rs
                    │  task(512), fd(64),      │
                    │  endpoint(128), pipe(4K),│
                    │  socket(256)             │
                    └─────────────────────────┘
```

## Design Decisions

### OOM retry at the allocator level (Track A)

The old `alloc_error_handler` approach could not retry because the handler's
signature is `-> !`. The `RetryAllocator` wrapper intercepts failures at
`GlobalAlloc::alloc()`, grows the heap, and retries before returning null.
Growth sizes escalate: layout-proportional, 1 MiB, 2 MiB, 4 MiB.

### Buddy allocator in kernel-core (Track B)

The buddy allocator is pure logic (no unsafe, no hardware access) implemented
in `kernel-core` for host-testability. It uses per-order free lists and bitmaps.
Two-phase initialization handles the chicken-and-egg problem: the buddy needs
`Vec` (heap), but the heap needs frame allocation:

1. **Bootstrap:** free-list collects frames before heap init
2. **Upgrade:** after heap init, `init_buddy()` drains the free list into the
   buddy allocator which coalesces adjacent frames into larger blocks

### Slab allocator (Track C)

Fixed-size caches avoid heap fragmentation for frequently allocated kernel
objects. Each slab is a page divided into equal-size slots with a bitmap.
Infrastructure is in place; actual migration of kernel object allocations
(Task, FD, Endpoint) is deferred for incremental adoption.

### Working munmap (Track D)

Replaces the Phase 12 stub. Uses `OffsetPageTable::unmap()` to clear PTEs,
frees frames through the buddy allocator (refcount-aware for CoW pages),
and sends SMP TLB shootdown IPIs. Per-process `MemoryMapping` list tracks
mmap allocations for validation.

### Userspace heap coalescing (Track E)

The `BrkAllocator` free list is now sorted by address. On dealloc, adjacent
free blocks are merged (left-merge, right-merge, or three-way merge). This
prevents fragmentation in long-running userspace processes.

### Memory statistics (Track F)

`heap_stats()`, `frame_stats()`, and `all_slab_stats()` provide kernel memory
observability. The `meminfo` command (syscall 0x1001) formats all stats into
a user buffer for shell display.

## Files Changed

| File | Change |
|---|---|
| `kernel-core/src/buddy.rs` | New: buddy allocator data structure + 13 tests |
| `kernel-core/src/slab.rs` | New: slab cache data structure + 7 tests |
| `kernel/src/mm/heap.rs` | RetryAllocator wrapper, heap_stats() |
| `kernel/src/mm/frame_allocator.rs` | Buddy integration, contiguous alloc, frame_stats() |
| `kernel/src/mm/slab.rs` | New: kernel slab cache instances |
| `kernel/src/mm/mod.rs` | Init sequence: buddy + slab after heap |
| `kernel/src/arch/x86_64/syscall.rs` | Real munmap, meminfo syscall |
| `kernel/src/process/mod.rs` | MemoryMapping tracking |
| `userspace/syscall-lib/src/heap.rs` | Sorted free list, coalescing dealloc |
| `userspace/syscall-lib/src/lib.rs` | meminfo() syscall wrapper |
| `userspace/coreutils-rs/src/meminfo.rs` | New: meminfo command |

## Deferred Items

- **A.4:** OOM stress QEMU test (requires test infrastructure for allocation loops)
- **C.4:** Migration of kernel object allocations to slab caches (incremental)
- **D.4:** mmap/munmap loop userspace test binary
- **E.3:** Userspace heap coalescing test binary
- **G.2:** Full memory stress QEMU test
