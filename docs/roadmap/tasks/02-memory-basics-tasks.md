# Phase 02 — Memory Basics: Task List

**Status:** Complete
**Source Ref:** phase-02
**Depends on:** Phase 1 ✅
**Goal:** Copy the bootloader memory map into kernel-owned structures, implement a frame allocator, set up page-table helpers, and initialize a kernel heap so that `alloc` types (`Box`, `Vec`, `String`) are available.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Memory map + frame allocator | Phase 1 | ✅ Done |
| B | Page mapping helpers + kernel heap | A | ✅ Done |
| C | Validation + docs | B | ✅ Done |

---

## Track A — Memory Map + Frame Allocator

### A.1 — Copy the bootloader memory map into kernel-owned structures

**File:** `kernel/src/mm/memory_map.rs`
**Symbol:** `init`
**Why it matters:** The bootloader-provided memory map must be copied into stable kernel storage before `BootInfo` references become invalid.

**Acceptance:**
- [x] `memory_map::init()` copies `MemoryRegion` data into a kernel-owned static
- [x] `regions()` provides read access to the stored memory map

### A.2 — Implement a simple frame allocator

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbols:** `BumpAllocator`, `init`, `allocate_frame`
**Why it matters:** Physical frame allocation is the foundation for all virtual memory operations — page tables, heap, and later userspace mappings all require frames.

**Acceptance:**
- [x] `init()` scans usable memory regions and builds the allocator state
- [x] `allocate_frame()` returns a fresh physical frame
- [x] Reserved or already-allocated memory is never reused
- [x] Reclaiming freed physical frames is explicitly deferred to later phases

---

## Track B — Page Mapping + Kernel Heap

### B.1 — Add safe wrappers around page-table manipulation

**File:** `kernel/src/mm/paging.rs`
**Symbols:** `init`, `GlobalFrameAlloc`
**Why it matters:** The kernel needs a safe interface to map virtual addresses to physical frames without writing raw page-table entries everywhere.

**Acceptance:**
- [x] `init()` reconstructs the page-table mapper from the physical memory offset
- [x] `GlobalFrameAlloc` implements `FrameAllocator<Size4KiB>` for use with the `x86_64` crate mapper

### B.2 — Reserve and map a fixed-size kernel heap region

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `init_heap`
**Why it matters:** Without a mapped heap region, the `#[global_allocator]` has no backing memory and all `alloc` usage will fault.

**Acceptance:**
- [x] `init_heap()` maps a contiguous virtual region backed by physical frames
- [x] `#[global_allocator]` is initialized after the heap is mapped

### B.3 — Add debug helpers for memory diagnostics

**File:** `kernel/src/mm/debug.rs`
**Symbols:** `log_memory_map`, `log_frame_stats`, `log_reserved_below_1mib`
**Why it matters:** Memory setup logs must provide enough information to troubleshoot boot failures and verify allocator correctness.

**Acceptance:**
- [x] Frame and page translations are logged at boot
- [x] Memory setup logs enough information to troubleshoot failures

---

## Track C — Validation + Docs

### C.1 — Verify heap allocations

**Why it matters:** Confirms that the heap is functional and the global allocator is correctly wired.

**Acceptance:**
- [x] Small `Box`, `Vec`, and `String` allocations succeed after heap initialization

### C.2 — Document memory architecture

**Why it matters:** Future phases depend on understanding the distinction between frames, pages, and the heap.

**Acceptance:**
- [x] The difference between physical frames, virtual pages, and the fixed kernel heap is documented
- [x] Allocator strategy and its limitations are documented
- [x] A note explains how mature kernels add reclaim, paging policy, and more complex allocators

---

## Documentation Notes

- Adds the `mm/` module tree, building on the serial and boot infrastructure from Phase 1.
- Frame allocator and heap enable `alloc` usage for all subsequent phases.
