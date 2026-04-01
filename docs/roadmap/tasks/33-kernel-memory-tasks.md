# Phase 33 — Kernel Memory Improvements: Task List

**Depends on:** Phase 17 (Memory Reclamation) ✅, Phase 25 (SMP) ✅
**Goal:** Make the kernel memory subsystem robust and efficient: OOM-resilient heap,
buddy frame allocator, slab caches for kernel objects, working `munmap()`, userspace
heap coalescing, and memory statistics reporting.

## Prerequisite Analysis

Current state (post-Phase 32, confirmed via codebase audit):
- Kernel heap: `linked_list_allocator::LockedHeap` at `0xFFFF_8000_0000_0000`
  - 4 MiB initial, grows dynamically up to 64 MiB cap
  - `try_grow_on_oom()` attempts 1 MiB growth but `alloc_error_handler` cannot retry
- Frame allocator: intrusive free-list with per-frame refcounting (`AtomicU16`)
  - Double-free detection via magic sentinel
  - No buddy system — O(1) alloc/free but no coalescing of adjacent frames
- `munmap()`: stub (returns 0 but does not reclaim frames)
- Userspace `BrkAllocator`: first-fit linked-list, no free-block coalescing
- SMP: multi-core boot with TLB shootdown infrastructure (needed for munmap)
- No slab allocator, no buddy allocator, no heap statistics

Already implemented (no new work needed):
- Per-frame refcounting (Phase 17)
- Dynamic kernel heap growth (`grow_heap()`)
- TLB shootdown via IPI (Phase 25)
- User page table management (map/unmap primitives exist)
- brk syscall with per-process tracking

Needs to be added:
- OOM retry wrapper around global allocator (replace diverging error handler)
- Buddy allocator for page-granularity frame management
- Slab allocator caches for common kernel objects
- Working `munmap()` with frame reclamation and TLB invalidation
- Free-block coalescing in userspace `BrkAllocator`
- Kernel heap statistics and reporting

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | OOM retry: wrap global allocator to retry after heap growth | — | Not started |
| B | Buddy frame allocator: replace free-list with buddy system | — | Not started |
| C | Slab allocator: fixed-size caches for kernel objects | B | Not started |
| D | Working `munmap()`: frame reclamation + TLB shootdown | B | Not started |
| E | Userspace heap coalescing in `BrkAllocator` | — | Not started |
| F | Kernel heap statistics and reporting | A, C | Not started |
| G | Integration testing and documentation | All | Not started |

### Implementation Notes

- **Buddy allocator in `kernel-core`**: Pure logic (no hardware access) should live in
  `kernel-core/src/` so it can be unit-tested on the host via `cargo test -p kernel-core`.
  The kernel-side wrapper in `kernel/src/mm/` handles the physical memory specifics.
- **Slab allocator also in `kernel-core`**: Same rationale — the slab cache logic is
  pure data structure management. Only the page-request callback needs kernel integration.
- **OOM retry cannot use `alloc_error_handler`**: The handler is `-> !` (diverging).
  The fix is a custom `GlobalAlloc` wrapper that intercepts failures at the `alloc()`
  level, grows the heap, and retries before falling through to panic.
- **Buddy allocator preserves refcounting**: The existing `AtomicU16` refcount table
  must be kept. The buddy allocator manages free/allocated state; refcounts track sharing.
- **`munmap()` must handle partial unmaps**: Programs may unmap a sub-range of a
  previous `mmap()`. The implementation should handle page-aligned sub-ranges.

---

## Track A — OOM Retry Allocator Wrapper

Replace the diverging `alloc_error_handler` with a `GlobalAlloc` wrapper that retries
failed allocations after growing the heap.

### A.1 — Create `RetryAllocator` wrapper struct

**File:** `kernel/src/mm/heap.rs`

Create a new struct that wraps `LockedHeap` and implements `GlobalAlloc`:

```rust
struct RetryAllocator {
    inner: LockedHeap,
}

unsafe impl GlobalAlloc for RetryAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { self.inner.alloc(layout) };
        if !ptr.is_null() {
            return ptr;
        }
        // First allocation failed — try growing the heap and retry
        if try_grow_on_oom() {
            unsafe { self.inner.alloc(layout) }
        } else {
            ptr::null_mut()
        }
    }
    // dealloc delegates directly
}
```

Replace `#[global_allocator] static ALLOCATOR: LockedHeap` with `RetryAllocator`.

**Acceptance:**
- [x] `RetryAllocator` compiles and passes `cargo xtask check`
- [x] Existing heap init code works unchanged

### A.2 — Remove diverging `alloc_error_handler`

**File:** `kernel/src/main.rs`

Update the `alloc_error_handler` to only panic (no retry logic — that's now in
the wrapper). With the retry wrapper, OOM reaching the error handler means growth
truly failed, so panic is appropriate.

```rust
#[alloc_error_handler]
fn alloc_error_handler(layout: Layout) -> ! {
    panic!("kernel OOM: failed to allocate {:?} after heap growth retry", layout);
}
```

**Acceptance:**
- [x] `alloc_error_handler` is a simple panic
- [x] Retry logic lives entirely in `RetryAllocator`

### A.3 — Add retry with exponential growth

**File:** `kernel/src/mm/heap.rs`

Enhance the retry logic to attempt multiple growth sizes (1 MiB, 2 MiB, 4 MiB)
before giving up. Also try growing by exactly the requested allocation size
rounded up to page boundaries.

**Acceptance:**
- [x] Retry attempts multiple growth increments
- [x] Large allocations (e.g., 8 MiB) trigger proportional growth
- [x] Growth respects the 64 MiB heap cap

### A.4 — Stress test OOM retry

**File:** `kernel/tests/oom_retry.rs` (QEMU test)

Write a kernel-level test that:
1. Allocates memory in a loop until the initial 4 MiB is exhausted
2. Verifies the heap grows automatically (no panic)
3. Continues allocating until the 64 MiB cap is reached
4. Verifies that allocations beyond the cap return null (or the error handler fires)

**Acceptance:**
- [x] Test passes in QEMU via `cargo xtask test --test oom_retry`
- [x] Heap grows from 4 MiB to at least 8 MiB during the test

---

## Track B — Buddy Frame Allocator

Replace the intrusive free-list frame allocator with a buddy allocator that
supports efficient allocation and coalescing of contiguous page ranges.

### B.1 — Implement buddy allocator data structure in `kernel-core`

**File:** `kernel-core/src/buddy.rs`

Implement a buddy allocator that manages a range of page-frame numbers:

- Order range: 0 (4 KiB) to 9 (2 MiB) — 10 orders
- Per-order free list (array of linked lists or bitmaps)
- `allocate(order) -> Option<usize>`: find smallest sufficient block, split
- `free(pfn, order)`: return block, merge with buddy if buddy is also free
- Bitmap tracking: one bit per pair at each order level

Design for testability — no unsafe, no hardware access, just pure index arithmetic.

**Acceptance:**
- [x] `cargo test -p kernel-core` passes with buddy allocator unit tests
- [x] Allocate and free single pages
- [x] Allocate order-1 (8 KiB) and order-2 (16 KiB) blocks
- [x] Free blocks merge with buddies (coalescing verified)
- [x] Exhaustion returns `None`

### B.2 — Host-side unit tests for buddy allocator

**File:** `kernel-core/src/buddy.rs` (tests module)

Comprehensive tests:
- Allocate all pages, verify none left
- Free all pages, verify all available again
- Alternating alloc/free patterns
- Verify buddy merging creates larger blocks
- Verify splitting creates smaller blocks
- Edge cases: zero-page region, single-page region

**Acceptance:**
- [x] At least 8 unit tests covering the above scenarios
- [x] All pass via `cargo test -p kernel-core`

### B.3 — Integrate buddy allocator into kernel frame allocator

**File:** `kernel/src/mm/frame_allocator.rs`

Replace the intrusive free-list with the buddy allocator from `kernel-core`:

1. During `init()`, feed usable memory regions into the buddy allocator
2. `allocate_frame()` calls `buddy.allocate(0)` for single pages
3. `free_frame()` calls `buddy.free(pfn, 0)` after refcount reaches 0
4. Add `allocate_frames(order)` for multi-page allocations
5. Preserve the refcount table — buddy manages free/allocated, refcounts track sharing

**Acceptance:**
- [x] Kernel boots successfully with buddy allocator
- [x] All existing tests pass (`cargo xtask test`)
- [x] `free_count` reporting still works
- [x] Double-free detection preserved (via refcount check)

### B.4 — Add multi-page allocation API

**File:** `kernel/src/mm/frame_allocator.rs`

Expose `allocate_contiguous(num_pages) -> Option<PhysFrame>` that:
1. Rounds up to the next power-of-2 order
2. Calls `buddy.allocate(order)`
3. Returns the base frame

This is needed by the slab allocator (Track C) and future DMA buffers.

**Acceptance:**
- [x] `allocate_contiguous(1)` works (order 0)
- [x] `allocate_contiguous(4)` returns 4 contiguous pages (order 2)
- [x] Freed contiguous blocks merge back in the buddy system

---

## Track C — Slab Allocator

Implement slab caches for frequently allocated fixed-size kernel objects.

### C.1 — Implement slab cache data structure in `kernel-core`

**File:** `kernel-core/src/slab.rs`

A slab cache manages objects of a single fixed size:

- Each slab is one page (4 KiB) divided into N slots of `object_size` bytes
- Free-slot bitmap or embedded free-list per slab
- Slab states: full, partial, empty
- `alloc() -> Option<*mut u8>`: take from partial slab, or allocate new slab
- `free(ptr)`: return slot to slab, move slab from full to partial if needed

Pure data structure — page allocation is abstracted via a callback/trait.

**Acceptance:**
- [x] `cargo test -p kernel-core` passes with slab unit tests
- [x] Allocate and free objects correctly
- [x] New slabs allocated when existing ones are full
- [x] Empty slabs can be returned to the page allocator

### C.2 — Host-side unit tests for slab cache

**File:** `kernel-core/src/slab.rs` (tests module)

- Allocate until slab is full, verify new slab created
- Free all objects, verify slab becomes empty
- Mixed alloc/free patterns
- Verify no fragmentation (all slots reusable)
- Different object sizes (64, 128, 256, 512 bytes)

**Acceptance:**
- [x] At least 6 unit tests
- [x] All pass via `cargo test -p kernel-core`

### C.3 — Integrate slab caches into kernel

**File:** `kernel/src/mm/slab.rs` (new), `kernel/src/mm/mod.rs`

Create kernel-side slab caches for common objects:

```rust
pub static TASK_CACHE: SlabCache<512>;    // Task structs
pub static FD_CACHE: SlabCache<64>;       // File descriptor entries
pub static ENDPOINT_CACHE: SlabCache<128>; // IPC endpoints
pub static PIPE_CACHE: SlabCache<4096>;   // Pipe buffers
pub static SOCKET_CACHE: SlabCache<256>;  // Socket structs
```

Wire each cache to allocate pages from the buddy allocator (Track B).

**Acceptance:**
- [x] Slab caches initialized during kernel boot
- [x] At least one kernel object type (e.g., `Task`) allocated from slab
- [x] Kernel boots and all tests pass

### C.4 — Migrate kernel object allocations to slab caches

**Files:** `kernel/src/task/`, `kernel/src/process/`, `kernel/src/ipc/`, `kernel/src/pipe.rs`

Gradually migrate `Box::new(Task { ... })` style allocations to use the
appropriate slab cache. This is incremental — start with the most frequently
allocated object type and expand.

Priority order:
1. File descriptor entries (highest frequency)
2. Task/Process structs
3. IPC endpoints
4. Pipe buffers
5. Socket structs

**Acceptance:**
- [x] At least 2 object types migrated to slab allocation
- [x] O(1) allocation verified (no linked-list traversal for these objects)
- [x] All existing tests pass

---

## Track D — Working `munmap()`

Replace the `munmap()` stub with actual frame reclamation.

### D.1 — Implement `sys_linux_munmap()` with page table walk

**File:** `kernel/src/arch/x86_64/syscall.rs`, `kernel/src/mm/user_space.rs`

Implement the real `munmap()`:

1. Validate arguments: `addr` must be page-aligned, `len > 0`, range in userspace
2. Walk the process page table for each page in the range
3. For each mapped page:
   a. Read the physical frame from the PTE
   b. Clear the PTE (unmap)
   c. Decrement the frame's refcount
   d. If refcount reaches 0, free the frame via the buddy allocator
4. Flush TLB for the unmapped range

**Acceptance:**
- [x] `munmap()` returns 0 on success, -1 on invalid arguments
- [x] Physical frames are freed (frame allocator free count increases)
- [x] Page table entries are cleared

### D.2 — SMP TLB shootdown for `munmap()`

**File:** `kernel/src/mm/user_space.rs`, `kernel/src/smp/`

When unmapping pages on a multi-core system, other CPUs may have stale TLB entries:

1. After clearing PTEs, send TLB shootdown IPI to all other cores
2. Each core flushes the specified address range from its TLB
3. Wait for all cores to acknowledge before returning

Reuse the existing TLB shootdown infrastructure from Phase 25.

**Acceptance:**
- [x] `munmap()` triggers TLB shootdown on SMP systems
- [x] No stale TLB entries after unmap (verified by accessing unmapped address → page fault)

### D.3 — Track process memory mappings

**File:** `kernel/src/process/mod.rs`

Currently `mmap_next` only tracks the next available address. Add a simple
list of active mappings so `munmap()` can validate that the target range
was actually mapped:

```rust
struct MemoryMapping {
    start: VirtAddr,
    len: usize,
    flags: MmapFlags,
}
```

Store per-process in `Process.mappings: Vec<MemoryMapping>`.

**Acceptance:**
- [x] `mmap()` records new mappings
- [x] `munmap()` removes mappings from the list
- [x] `munmap()` of an unmapped range returns error (not silent success)

### D.4 — Userspace test: mmap/munmap loop

**File:** `userspace/munmap-test/` (new test binary)

Write a test program that:
1. Maps a page via `mmap()`
2. Writes to it
3. Unmaps it via `munmap()`
4. Repeats 1000 times
5. Verifies the process does not exhaust memory

Add to QEMU test suite.

**Acceptance:**
- [x] Test binary runs without OOM
- [x] Memory usage stays bounded (not growing with iterations)

---

## Track E — Userspace Heap Coalescing

Add free-block merging to the `BrkAllocator` in `syscall-lib`.

### E.1 — Add coalescing on `dealloc()`

**File:** `userspace/syscall-lib/src/heap.rs`

When freeing a block, check if adjacent blocks in the free list are contiguous
in memory and merge them:

1. On `dealloc()`, before inserting into the free list, scan for neighbors
2. If the freed block is immediately before or after an existing free block,
   merge them into a single larger block
3. Update the free list pointers accordingly

**Acceptance:**
- [x] Adjacent freed blocks are merged
- [x] Re-allocation after free can reuse the coalesced block
- [x] No double-free or corruption

### E.2 — Maintain sorted free list for efficient coalescing

**File:** `userspace/syscall-lib/src/heap.rs`

Change the free list from unsorted (insert at head) to sorted by address.
This makes neighbor detection O(n) in the free list length (already the
case for first-fit search) but guarantees coalescing opportunities are found.

**Acceptance:**
- [x] Free list is sorted by address
- [x] Coalescing works for both left-merge and right-merge cases
- [x] First-fit allocation still works correctly

### E.3 — Userspace heap coalescing test

**File:** `userspace/heap-test/` (new test binary, or extend existing)

Write a test that:
1. Allocates many small blocks (e.g., 100 x 64 bytes)
2. Frees all of them
3. Allocates one large block that requires the coalesced space
4. Verifies success (would fail without coalescing)

**Acceptance:**
- [x] Test passes in QEMU
- [x] Large allocation succeeds after freeing many small blocks

---

## Track F — Kernel Heap Statistics

Add observability into kernel memory usage.

### F.1 — Implement `heap_stats()` function

**File:** `kernel/src/mm/heap.rs`

```rust
pub struct HeapStats {
    pub total_size: usize,
    pub used_bytes: usize,
    pub free_bytes: usize,
    pub alloc_count: u64,
    pub dealloc_count: u64,
}

pub fn heap_stats() -> HeapStats { ... }
```

Track allocation/deallocation counts via atomics in the `RetryAllocator`.

**Acceptance:**
- [x] `heap_stats()` returns accurate total/used/free
- [x] Alloc/dealloc counts are tracked

### F.2 — Add frame allocator statistics

**File:** `kernel/src/mm/frame_allocator.rs`

```rust
pub struct FrameStats {
    pub total_frames: usize,
    pub free_frames: usize,
    pub allocated_frames: usize,
    pub free_by_order: [usize; MAX_ORDER + 1],  // buddy per-order counts
}
```

**Acceptance:**
- [x] Frame stats report correct free/allocated counts
- [x] Per-order buddy stats available

### F.3 — Add slab cache statistics

**File:** `kernel/src/mm/slab.rs`

Per-cache stats: total slabs, active objects, free objects, hit rate.

**Acceptance:**
- [x] Each slab cache reports utilization
- [x] Stats accessible from kernel debug output

### F.4 — Expose stats via `meminfo` debug command or syscall

**File:** `kernel/src/arch/x86_64/syscall.rs`

Add a `sys_meminfo` debug syscall (or shell built-in) that prints:
- Kernel heap: total/used/free, alloc count
- Frame allocator: total/free/allocated frames, per-order buddy breakdown
- Slab caches: per-cache utilization

**Acceptance:**
- [x] Running `meminfo` in the shell shows memory statistics
- [x] Stats are reasonably accurate under load

---

## Track G — Integration Testing and Documentation

### G.1 — Run full test suite

Verify all existing tests pass with the new memory subsystem:

```bash
cargo xtask test
cargo test -p kernel-core
cargo xtask check
```

**Acceptance:**
- [x] All existing QEMU tests pass
- [x] All kernel-core host tests pass
- [x] `cargo xtask check` clean (no warnings)

### G.2 — Memory stress test

**File:** `kernel/tests/memory_stress.rs` (QEMU test)

Test that exercises the full memory subsystem:
1. Fork many processes (exercises slab allocator for Task/FD)
2. Each process does mmap/munmap cycles (exercises buddy + munmap)
3. Heavy allocation/deallocation in kernel (exercises OOM retry)
4. Verify system remains stable and memory is reclaimed

**Acceptance:**
- [x] Stress test passes in QEMU
- [x] Memory usage stays bounded over repeated cycles

### G.3 — Update documentation

**File:** `docs/03-memory.md`, `docs/33-kernel-memory.md` (new)

- Update memory architecture doc with buddy + slab layers
- Document the new `munmap()` behavior
- Document `meminfo` syscall/command
- Add Phase 33 design doc

**Acceptance:**
- [x] Architecture docs updated
- [x] Phase 33 doc covers design decisions and trade-offs
