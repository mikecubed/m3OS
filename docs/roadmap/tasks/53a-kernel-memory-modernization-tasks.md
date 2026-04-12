# Phase 53a — Kernel Memory Modernization: Task List

**Status:** Planned
**Source Ref:** phase-53a
**Depends on:** Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP) ✅, Phase 52d (Kernel Completion) ✅
**Goal:** Replace the global-lock, linked-list-heap allocator stack with a per-CPU-cached, size-class-based, SMP-scalable kernel memory subsystem.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| D | Foundation fixes (double-lock, O(n) remove_free, SeqCst, frame zeroing) | None | Planned |
| A | Per-CPU page cache for frame allocation | D | Planned |
| B | Slab allocator rewrite with embedded freelist and per-CPU magazines | D | Planned |
| C | Size-class GlobalAlloc replacement | A, B | Planned |
| E | Cross-CPU atomic free path | B | Planned |

---

## Track D — Foundation Fixes

### D.1 — Cache phys_offset in a static to eliminate double-lock in free_frame

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `free_frame`
**Why it matters:** `free_frame()` currently acquires the frame allocator lock twice — once to read `phys_offset` (a constant after init) and once to call `free_to_pool()`. The first acquisition is unnecessary and doubles lock contention on every free.

**Acceptance:**
- [ ] `phys_offset` is stored in a `static AtomicU64` or `Once<u64>` during `init()`, read without the frame allocator lock
- [ ] `free_frame()` acquires `FRAME_ALLOCATOR.0.lock()` exactly once
- [ ] All existing QEMU tests pass unchanged

### D.2 — Replace Vec-based buddy free lists with intrusive doubly-linked lists

**Files:**
- `kernel-core/src/buddy.rs`
- `kernel/src/mm/frame_allocator.rs`

**Symbol:** `BuddyAllocator::free_lists`, `BuddyAllocator::remove_free`
**Why it matters:** `remove_free` does an O(n) linear scan of a `Vec<usize>` to find and remove a buddy during coalescing. With thousands of free blocks, this defeats the O(log n) guarantee. Intrusive lists stored in the free pages themselves give O(1) removal and eliminate the heap dependency.

**Acceptance:**
- [ ] `BuddyAllocator` no longer uses `Vec<usize>` for free lists
- [ ] Free-list nodes are stored intrusively in the free page data (pointer pair at known offset)
- [ ] `remove_free` is O(1) via direct doubly-linked list removal
- [ ] `BuddyAllocator::new()` does not require heap allocation (`alloc::Vec`)
- [ ] All existing `cargo test -p kernel-core` buddy tests pass
- [ ] At least 3 new tests cover intrusive list operations (insert, remove, coalesce)

### D.3 — Downgrade refcount atomics from SeqCst to Acquire/Release

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `refcount_inc`, `refcount_dec`, `refcount_get`
**Why it matters:** All refcount operations use `Ordering::SeqCst`, which is stronger than necessary. The "increment / decrement / check-and-free" pattern requires only Acquire/Release for correctness. SeqCst adds unnecessary memory fence overhead on x86_64.

**Acceptance:**
- [ ] `refcount_inc` uses `Ordering::Relaxed` (no ordering needed for increment)
- [ ] `refcount_dec` uses `Ordering::AcqRel` (release the decrement, acquire to check if zero)
- [ ] `refcount_get` uses `Ordering::Acquire`
- [ ] CoW fork and free behavior unchanged (QEMU fork-test passes)

### D.4 — Move frame zeroing from free-time to alloc-time

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `free_frame`, `allocate_frame`
**Why it matters:** `free_frame()` unconditionally zeroes every 4 KiB frame (~1 microsecond). This wastes cycles when the frame is immediately reused (common with per-CPU caching). Real allocators zero on allocation only when requested (Linux `__GFP_ZERO`, FreeBSD `UMA_ZONE_ZINIT`).

**Acceptance:**
- [ ] `free_frame()` does not zero the frame
- [ ] `allocate_frame()` returns an unzeroed frame by default
- [ ] A new `allocate_frame_zeroed()` function returns a zeroed frame
- [ ] All callers that require zeroed frames (page table creation, user page mapping) are updated to use `allocate_frame_zeroed()`
- [ ] Existing QEMU tests pass (no user-visible behavior change)

### D.5 — Add freelist pointer hardening

**File:** `kernel-core/src/slab.rs` (new slab implementation)
**Symbol:** `encode_freeptr`, `decode_freeptr`
**Why it matters:** Embedded freelist pointers inside free objects are a target for heap corruption attacks. XOR-encoding with a per-cache random value and the object address makes exploitation significantly harder with near-zero overhead (two XOR instructions).

**Acceptance:**
- [ ] Freelist pointers stored as `real_ptr ^ cache_random ^ &object`
- [ ] `cache_random` is initialized once per slab cache from a PRNG seed
- [ ] Decoding produces the original pointer (round-trip test)
- [ ] Corrupted freelist pointer (single bit flip) is detected on decode (validation test)

---

## Track A — Per-CPU Page Cache

### A.1 — Define per-CPU page cache data structure

**Files:**
- `kernel/src/mm/frame_allocator.rs`
- `kernel/src/smp/mod.rs` (per-core data extension)

**Symbol:** `PerCpuPageCache`
**Why it matters:** The per-CPU page cache is the single highest-impact change — it eliminates the global buddy lock from ~95% of frame allocations by interposing a per-CPU buffer.

**Acceptance:**
- [ ] `PerCpuPageCache` struct with fixed-size array of physical addresses (capacity 64)
- [ ] `count` field tracks current fill level
- [ ] Structure is `#[repr(align(64))]` to prevent false sharing between CPUs
- [ ] Integrated into `smp::PerCoreData` and accessible via `smp::per_core()`

### A.2 — Per-CPU fast path for allocate_frame

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `allocate_frame`
**Why it matters:** The hot path — pop a frame from the per-CPU cache without any lock acquisition.

**Acceptance:**
- [ ] `allocate_frame()` checks per-CPU cache first via `smp::per_core()`
- [ ] If cache is non-empty, returns frame without acquiring `FRAME_ALLOCATOR` lock
- [ ] If cache is empty, performs batch refill: acquires buddy lock once, pops 32 frames, fills per-CPU cache
- [ ] BSP path works before SMP init (falls through to buddy directly)
- [ ] Benchmark: frame allocation throughput improves on 2+ core QEMU configurations

### A.3 — Per-CPU fast path for free_frame

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `free_frame`
**Why it matters:** The corresponding free path — push a frame to the per-CPU cache without lock acquisition.

**Acceptance:**
- [ ] `free_frame()` pushes to per-CPU cache if below high watermark (48 of 64)
- [ ] If cache exceeds high watermark, batch drain: acquires buddy lock once, returns 32 frames
- [ ] Refcount-aware: only reaches per-CPU cache after refcount reaches zero
- [ ] BSP path works before SMP init

### A.4 — Per-CPU cache drain on memory pressure

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `drain_per_cpu_caches`
**Why it matters:** When the buddy allocator is low on free pages, per-CPU caches should be drainable to recover hoarded frames.

**Acceptance:**
- [ ] `drain_per_cpu_caches()` empties all CPUs' page caches back to buddy
- [ ] Called from `grow_heap()` OOM path before giving up
- [ ] Safe to call from any CPU (acquires per-CPU data + buddy lock in correct order)

---

## Track B — Slab Allocator with Per-CPU Magazines

### B.1 — Rewrite SlabCache with embedded freelist

**File:** `kernel-core/src/slab.rs`
**Symbol:** `SlabCache`, `Slab`
**Why it matters:** The current bitmap-based slab has O(slabs) linear scan for both allocation and free. Embedded freelists give O(1) for both. This is the data structure that per-CPU magazines will cache into.

**Acceptance:**
- [ ] Free objects contain an embedded next-pointer (encoded with D.5 hardening)
- [ ] Slab metadata (freelist head, inuse count, total objects) stored separately from slab data pages
- [ ] Allocation: follow freelist head pointer, O(1)
- [ ] Free: prepend to freelist head, O(1)
- [ ] Partial slab list: doubly-linked list of slabs with free objects
- [ ] Full slabs removed from partial list; re-added when an object is freed
- [ ] At least 8 host tests covering alloc, free, slab-full, slab-empty, partial-list management

### B.2 — Define magazine data structure

**File:** `kernel-core/src/magazine.rs` (new)
**Symbol:** `Magazine`, `MagazineDepot`
**Why it matters:** Magazines are the per-CPU caching layer. Each is a fixed-capacity array of object pointers. The depot is the shared pool of full/empty magazines.

**Acceptance:**
- [ ] `Magazine` struct: `[*mut u8; MAGAZINE_CAPACITY]` array + count (capacity = 32)
- [ ] `MagazineDepot` struct: per-size-class stack of full magazines + stack of empty magazines + spinlock
- [ ] `Magazine::push()` and `Magazine::pop()` are O(1) with no synchronization
- [ ] `MagazineDepot::exchange_empty_for_full()` and `exchange_full_for_empty()` under lock
- [ ] At least 5 host tests covering push/pop, full/empty detection, depot exchange

### B.3 — Wire per-CPU magazines into slab allocation

**Files:**
- `kernel/src/mm/slab.rs`
- `kernel/src/smp/mod.rs` (per-core data extension)

**Symbol:** `PerCpuMagazines`
**Why it matters:** Connects the magazine layer to actual kernel allocation. Each CPU gets a loaded + previous magazine per size class.

**Acceptance:**
- [ ] Per-CPU structure with 2 magazines (loaded + previous) per size class (13 classes)
- [ ] Allocation fast path: pop from loaded magazine (no lock, no atomic)
- [ ] If loaded empty: swap loaded and previous
- [ ] If both empty: exchange empty for full from depot (depot lock)
- [ ] If depot empty: fill from slab layer (slab lock)
- [ ] Free fast path: push to previous magazine (no lock, no atomic)
- [ ] If previous full: swap loaded and previous
- [ ] If both full: exchange full for empty to depot (depot lock)

### B.4 — Define size classes and size-to-class mapping

**File:** `kernel-core/src/slab.rs` or `kernel-core/src/size_class.rs` (new)
**Symbol:** `SIZE_CLASSES`, `size_to_class`
**Why it matters:** Maps arbitrary allocation sizes to the appropriate slab cache. Geometric 4-steps-per-doubling bounds internal waste at <20%.

**Acceptance:**
- [ ] 13 size classes: 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048, 4096
- [ ] `size_to_class(size: usize) -> Option<usize>` returns class index, `None` for >4096
- [ ] Compile-time constant table (no runtime computation)
- [ ] Host test: every size 1..=4096 maps to the smallest class >= that size
- [ ] Host test: internal waste never exceeds 20% for any size

---

## Track C — Size-Class GlobalAlloc Replacement

### C.1 — Implement size-class-based GlobalAlloc

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `SizeClassAllocator` (replaces `RetryAllocator`)
**Why it matters:** This is the final integration point — all `Box::new`, `Vec::push`, `Arc::new` route through here. Replaces the O(n) linked-list scan with O(1) size-class dispatch to slab caches.

**Acceptance:**
- [ ] `SizeClassAllocator` implements `GlobalAlloc`
- [ ] `alloc(layout)`: if size <= 4096, route to slab cache for `size_to_class(size)`; else allocate pages from buddy
- [ ] `dealloc(ptr, layout)`: if size <= 4096, return to slab cache; else free pages to buddy
- [ ] Alignment requirements from `Layout` are respected (may require rounding up to next size class)
- [ ] `linked_list_allocator` crate removed from `Cargo.toml`
- [ ] All existing QEMU tests pass
- [ ] `meminfo` command updated to report per-size-class slab statistics

### C.2 — Bootstrap path for pre-slab allocations

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `SizeClassAllocator::alloc` (early-boot path)
**Why it matters:** During early boot, before slab caches are initialized, the allocator must still work. The buddy allocator needs allocations to set up the slab caches.

**Acceptance:**
- [ ] Before slab init, allocations fall back to a simple bump or page-granularity allocator
- [ ] After slab init, all allocations route through slab caches
- [ ] Deallocations from the bootstrap path are correctly handled (address-range check, like Theseus)
- [ ] The transition is invisible to callers (`Vec::push` works identically before and after slab init)

---

## Track E — Cross-CPU Atomic Free Path

### E.1 — Add per-CPU atomic free list for cross-CPU slab frees

**Files:**
- `kernel/src/mm/slab.rs`
- `kernel/src/smp/mod.rs`

**Symbol:** `CrossCpuFreeList`
**Why it matters:** When CPU B frees an object allocated on CPU A's slab, it must not acquire CPU A's magazine lock. Instead, it pushes to an atomic MPSC (multiple-producer, single-consumer) list. CPU A batch-collects on its next allocation.

**Acceptance:**
- [ ] Each CPU has a per-size-class atomic free list head (`AtomicPtr`)
- [ ] Cross-CPU free: CAS push to victim CPU's atomic list (Acquire/Release ordering)
- [ ] Owning CPU: `atomic_exchange(NULL)` to collect entire queue in one operation
- [ ] Collected objects spliced into local magazine or slab freelist
- [ ] No lock acquisition on the cross-CPU free path
- [ ] Host test: concurrent push from multiple threads, single-thread collect, all objects recovered

### E.2 — Detect cross-CPU free and route correctly

**File:** `kernel/src/mm/slab.rs`
**Symbol:** `slab_free`
**Why it matters:** The free path must determine whether the object belongs to the current CPU's slab or another CPU's. This requires O(1) lookup of the owning CPU from the object address.

**Acceptance:**
- [ ] Slab pages store the owning CPU ID in metadata (embedded at fixed offset in slab page, like Theseus)
- [ ] `slab_free(ptr)`: read owning CPU from page metadata, compare with current CPU
- [ ] Same-CPU: push to local magazine (fast path)
- [ ] Different-CPU: push to victim's atomic free list (cross-CPU path)
- [ ] O(1) address-to-slab-metadata lookup via page alignment masking

---

## Documentation Notes

- Phase 33 introduced the buddy allocator, slab cache infrastructure, and OOM-retry heap. Phase 53a replaces the heap and slab implementations while preserving the buddy as the backend.
- Phase 36 added demand paging and mprotect. These are unaffected — they use `allocate_frame()` which retains the same API.
- Phase 52c replaced `Vec<MemoryMapping>` with `VmaTree` (BTreeMap-backed). Phase 53a follows the same pattern: replace simple data structures with scalable ones.
- The research documents in `docs/research/` provide the theoretical foundation and cross-system analysis that informed these design choices.
- All new pure-logic code belongs in `kernel-core` for host testability. Only hardware-dependent wiring belongs in `kernel/src/mm/`.
