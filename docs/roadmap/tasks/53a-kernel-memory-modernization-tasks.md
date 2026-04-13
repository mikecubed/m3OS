# Phase 53a — Kernel Memory Modernization: Task List

**Status:** Complete
**Source Ref:** phase-53a
**Depends on:** Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP) ✅, Phase 36 (Expanded Memory) ✅, Phase 52c (Kernel Architecture Evolution) ✅, Phase 52d (Kernel Completion) ✅
**Goal:** Replace the global-lock, linked-list-heap allocator stack with a per-CPU-cached, size-class-based, SMP-scalable kernel memory subsystem.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| D | Foundation fixes (double-lock, buddy removal, refcount ordering, zeroing invariant) | None | Complete |
| A | Per-CPU page cache for frame allocation | D | Complete |
| B | Slab allocator rewrite with embedded freelist and per-CPU magazines | D | Complete |
| C | Size-class GlobalAlloc replacement and bootstrap cutover | A, B | Complete |
| E | Cross-CPU atomic free path | B | Complete |
| F | Interrupt safety, reclaim, stats compatibility, and validation | D | Complete |

---

## Track D — Foundation Fixes

### D.1 — Cache phys_offset in a static to eliminate double-lock in free_frame

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `free_frame`
**Why it matters:** `free_frame()` currently acquires the frame allocator lock twice — once to read `phys_offset` (a constant after init) and once to call `free_to_pool()`. The first acquisition is unnecessary and doubles lock contention on every free.

**Acceptance:**
- [x] `phys_offset` is stored in a `static AtomicU64` or `Once<u64>` during `init()`, read without the frame allocator lock
- [x] `free_frame()` acquires `FRAME_ALLOCATOR.0.lock()` exactly once
- [x] All existing QEMU tests pass unchanged

### D.2 — Replace O(n) buddy removal with an O(1)-removable free-block representation

**Files:**
- `kernel-core/src/buddy.rs`
- `kernel/src/mm/frame_allocator.rs`

**Symbol:** `BuddyAllocator::free_lists`, `BuddyAllocator::remove_free`
**Why it matters:** `remove_free` does an O(n) linear scan of a `Vec<usize>` to find and remove a buddy during coalescing. With thousands of free blocks, this defeats the O(log n) guarantee. The replacement must preserve `kernel-core` host testability instead of assuming direct access to real free-page bodies inside the pure data-structure crate.

**Acceptance:**
- [x] `remove_free` no longer linearly scans `Vec::position()`
- [x] The chosen free-block representation supports O(1) removal while keeping `kernel-core::buddy` host-testable and pure
- [x] If page-body nodes are used, they are managed by kernel-side wrapper metadata rather than by `kernel-core` directly
- [x] Any remaining heap/bootstrap requirements (`Vec<u64>` bitmaps, init staging buffers) are documented explicitly instead of being hand-waved away
- [x] All existing `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` buddy tests pass
- [x] At least 3 new tests cover the chosen free-block representation's insert / remove / coalesce behavior

### D.3 — Downgrade refcount atomics from SeqCst to Acquire/Release

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `refcount_inc`, `refcount_dec`, `refcount_get`
**Why it matters:** All refcount operations use `Ordering::SeqCst`, which is stronger than necessary. The "increment / decrement / check-and-free" pattern requires only Acquire/Release for correctness. SeqCst adds unnecessary memory fence overhead on x86_64.

**Acceptance:**
- [x] `refcount_inc` uses `Ordering::Relaxed` (no ordering needed for increment)
- [x] `refcount_dec` uses `Ordering::AcqRel` (release the decrement, acquire to check if zero)
- [x] `refcount_get` uses `Ordering::Acquire`
- [x] CoW fork and free behavior unchanged (QEMU fork-test passes)

### D.4 — Move unconditional frame zeroing off the free path while preserving zero-before-exposure guarantees

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `free_frame`, `free_contiguous`, `allocate_frame`
**Why it matters:** `free_frame()` and `free_contiguous()` currently zero every returned frame. This wastes cycles when the frame is immediately reused (common with per-CPU caching), but Phase 52b also made stale-mapping hardening depend on zeroing before user-visible reuse. The change must preserve that security invariant, not just move a memset around.

**Acceptance:**
- [x] `free_frame()` and `free_contiguous()` no longer zero frames unconditionally on free
- [x] The invariant "any frame that can become user-visible is zeroed before exposure" is documented and preserved
- [x] Single-page and contiguous allocation APIs expose explicit zeroing helpers or an equivalently documented zeroing path
- [x] All zero-sensitive callsites (page-table creation, lazy page faults, ELF/user mapping, CoW, `mprotect` / `munmap`) are audited and updated
- [x] Regressions cover stale mapping, lazy `mmap`, CoW fork, and `mprotect` / `munmap` interactions

### D.5 — Add freelist pointer hardening

**File:** `kernel-core/src/slab.rs` (new slab implementation)
**Symbol:** `encode_freeptr`, `decode_freeptr`
**Why it matters:** Embedded freelist pointers inside free objects are a target for heap corruption attacks. XOR-encoding with a per-cache random value and the object address makes exploitation significantly harder with near-zero overhead (two XOR instructions).

**Acceptance:**
- [x] Freelist pointers stored as `real_ptr ^ cache_random ^ &object`
- [x] `cache_random` is initialized once per slab cache from a PRNG seed
- [x] Decoding produces the original pointer (round-trip test)
- [x] Decode validates alignment / range / cache ownership and rejects obviously invalid or corrupted pointers (validation test)

---

## Track A — Per-CPU Page Cache

### A.1 — Define per-CPU page cache data structure

**Files:**
- `kernel/src/mm/frame_allocator.rs`
- `kernel/src/smp/mod.rs` (per-core data extension)

**Symbol:** `PerCpuPageCache`
**Why it matters:** The per-CPU page cache is the single highest-impact change — it eliminates the global buddy lock from ~95% of frame allocations by interposing a per-CPU buffer.

**Acceptance:**
- [x] `PerCpuPageCache` struct with fixed-size array of physical addresses (capacity 64)
- [x] `count` field tracks current fill level
- [x] Structure is `#[repr(align(64))]` to prevent false sharing between CPUs
- [x] Integrated into `smp::PerCoreData` and accessible via `smp::per_core()`

### A.2 — Per-CPU fast path for allocate_frame

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `allocate_frame`
**Why it matters:** The hot path — pop a frame from the per-CPU cache without any lock acquisition.

**Acceptance:**
- [x] `allocate_frame()` checks per-CPU cache first via `smp::per_core()`
- [x] Cache-hit fast path mutates only CPU-local state with interrupts masked or an equivalent local non-reentrancy guard
- [x] If cache is non-empty, returns frame without acquiring `FRAME_ALLOCATOR` lock
- [x] If cache is empty, performs batch refill: acquires buddy lock once, pops 32 frames, fills per-CPU cache
- [x] BSP path works before SMP init (falls through to buddy directly)
- [x] Lock/counter instrumentation or benchmark shows the cache-hit path avoids global frame-allocator locking on 2+ core QEMU configurations

### A.3 — Per-CPU fast path for free_frame

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `free_frame`
**Why it matters:** The corresponding free path — push a frame to the per-CPU cache without lock acquisition.

**Acceptance:**
- [x] `free_frame()` mutates only CPU-local cache state with interrupts masked or an equivalent local non-reentrancy guard
- [x] `free_frame()` pushes to per-CPU cache if below high watermark (48 of 64)
- [x] If cache exceeds high watermark, batch drain: acquires buddy lock once, returns 32 frames
- [x] Refcount-aware: only reaches per-CPU cache after refcount reaches zero
- [x] BSP path works before SMP init

### A.4 — Coordinated per-CPU cache drain on memory pressure

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `drain_per_cpu_caches`
**Why it matters:** When the buddy allocator is low on free pages, per-CPU caches should be drainable to recover hoarded frames. A remote drain cannot simply mutate another CPU's lockless cache in place.

**Acceptance:**
- [x] `drain_per_cpu_caches()` uses owner-CPU self-drain, IPI, or an equivalent synchronized handoff instead of unsafely mutating another CPU's lockless cache in place
- [x] Called from `grow_heap()` OOM path and before final failure of high-order / contiguous allocation retries
- [x] Drain ordering and locking / interrupt rules are documented and regression-tested

---

## Track B — Slab Allocator with Per-CPU Magazines

### B.1 — Rewrite SlabCache with embedded freelist

**File:** `kernel-core/src/slab.rs`
**Symbol:** `SlabCache`, `Slab`
**Why it matters:** The current bitmap-based slab has O(slabs) linear scan for both allocation and free. Embedded freelists give O(1) for both. This is the data structure that per-CPU magazines will cache into.

**Acceptance:**
- [x] Free objects contain an embedded next-pointer (encoded with D.5 hardening)
- [x] Slab metadata (freelist head, inuse count, total objects, owning CPU, size class) is stored in out-of-line span metadata keyed by slab page base
- [x] Allocation: follow freelist head pointer, O(1)
- [x] Free: prepend to freelist head, O(1)
- [x] Partial slab list: doubly-linked list of slabs with free objects
- [x] Full slabs removed from partial list; re-added when an object is freed
- [x] 4096-byte objects remain allocatable because slab metadata does not consume client-object space inside the slab page
- [x] At least 8 host tests covering alloc, free, slab-full, slab-empty, partial-list management

### B.2 — Define magazine data structure

**File:** `kernel-core/src/magazine.rs` (new)
**Symbol:** `Magazine`, `MagazineDepot`
**Why it matters:** Magazines are the per-CPU caching layer. Each is a fixed-capacity array of object pointers. The depot is the shared pool of full/empty magazines.

**Acceptance:**
- [x] `const MAGAZINE_CAPACITY: usize = 32` defines the shared capacity
- [x] `Magazine` struct: `[*mut u8; MAGAZINE_CAPACITY]` array + count
- [x] `MagazineDepot` struct: per-size-class stack of full magazines + stack of empty magazines + spinlock
- [x] `Magazine::push()` and `Magazine::pop()` are O(1) with no synchronization
- [x] `MagazineDepot::exchange_empty_for_full()` and `exchange_full_for_empty()` under lock
- [x] At least 5 host tests covering push/pop, full/empty detection, depot exchange

### B.3 — Wire per-CPU magazines into slab allocation

**Files:**
- `kernel/src/mm/slab.rs`
- `kernel/src/smp/mod.rs` (per-core data extension)

**Symbol:** `PerCpuMagazines`
**Why it matters:** Connects the magazine layer to actual kernel allocation. Each CPU gets a loaded + previous magazine per size class.

**Acceptance:**
- [x] Per-CPU structure with 2 magazines (loaded + previous) per size class (13 classes)
- [x] Allocation fast path: pop from loaded magazine with interrupts masked or equivalent local non-reentrancy guard (no lock, no atomic)
- [x] If loaded empty: swap loaded and previous
- [x] If both empty: exchange empty for full from depot (depot lock)
- [x] If depot empty: fill from slab layer (slab lock)
- [x] Free fast path: push to previous magazine with interrupts masked or equivalent local non-reentrancy guard (no lock, no atomic)
- [x] If previous full: swap loaded and previous
- [x] If both full: exchange full for empty to depot (depot lock)

### B.4 — Define size classes and size-to-class mapping

**File:** `kernel-core/src/slab.rs` or `kernel-core/src/size_class.rs` (new)
**Symbol:** `SIZE_CLASSES`, `size_to_class`
**Why it matters:** Maps arbitrary allocation sizes to the appropriate slab cache. The shipped Phase 53a table keeps the exact 13-class contract while bounding waste to <34% through the 32..=1024 geometric region and <50% overall for 32..=4096 requests.

**Acceptance:**
- [x] 13 size classes: 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 2048, 4096
- [x] `size_to_class(size: usize) -> Option<usize>` returns class index, `None` for >4096
- [x] Compile-time constant table (no runtime computation)
- [x] Host test: every size 1..=4096 maps to the smallest class >= that size
- [x] Host test: internal waste stays below 34% in the 32..=1024 geometric region and below 50% overall for 32..=4096 requests

---

## Track C — Size-Class GlobalAlloc Replacement

### C.1 — Implement size-class-based GlobalAlloc and page-backed large-allocation path

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `SizeClassAllocator` (replaces `RetryAllocator`)
**Why it matters:** This is the final integration point — all `Box::new`, `Vec::push`, `Arc::new` route through here. Replaces the O(n) linked-list scan with O(1) size-class dispatch to slab caches.

**Acceptance:**
- [x] `SizeClassAllocator` implements `GlobalAlloc`
- [x] `alloc(layout)`: if `layout` fits the small-object path (size <= 4096 and alignment within class guarantees), route to slab cache for `size_to_class(size)`; otherwise use a page-backed mapped allocation path backed by buddy frames
- [x] `dealloc(ptr, layout)`: uses allocation metadata to distinguish slab-backed vs page-backed allocations and returns memory to the correct backend
- [x] Alignment requirements from `Layout` are respected without silently misclassifying large-align requests as slab allocations
- [x] `linked_list_allocator` crate removed from `Cargo.toml`
- [x] `frame_stats()`, `heap_stats()`, `/proc/meminfo`, and `sys_meminfo` are updated to reflect the new allocator layers
- [x] Userspace `meminfo` output remains non-truncated after the stats surface changes
- [x] All existing QEMU tests pass

### C.2 — Bootstrap and cutover path for pre-slab allocations

**File:** `kernel/src/mm/heap.rs`
**Symbol:** `SizeClassAllocator::alloc` (early-boot path)
**Why it matters:** During early boot, before slab caches are initialized, the allocator must still work. The bootstrap path must cover buddy setup, refcount-table allocation, slab metadata bootstrap, and later BSP/AP per-core allocations without recursing into an allocator that is not fully initialized yet.

**Acceptance:**
- [x] Before slab init, allocations fall back to an explicitly documented bootstrap allocator used by buddy init, refcount-table setup, slab metadata bootstrap, and BSP/AP per-core bring-up
- [x] The cutover point from bootstrap allocation to the size-class allocator is explicit and occurs only after slab caches and metadata tables are initialized
- [x] After cutover, all eligible small allocations route through slab caches
- [x] Deallocations from the bootstrap path are correctly handled via explicit range or tagged metadata
- [x] The transition is invisible to callers (`Vec::push` works identically before and after slab init)

---

## Track E — Cross-CPU Atomic Free Path

### E.1 — Add per-CPU atomic free list for cross-CPU slab frees

**Files:**
- `kernel/src/mm/slab.rs`
- `kernel/src/smp/mod.rs`

**Symbol:** `CrossCpuFreeList`
**Why it matters:** When CPU B frees an object allocated on CPU A's slab, it must not acquire CPU A's magazine lock. Instead, it pushes to an atomic MPSC (multiple-producer, single-consumer) list. CPU A batch-collects on its next allocation.

**Acceptance:**
- [x] Each CPU has a per-size-class atomic free list head (`AtomicPtr`)
- [x] Cross-CPU free: CAS push to victim CPU's atomic list (Acquire/Release ordering)
- [x] Owning CPU: `atomic_exchange(NULL)` to collect entire queue in one operation
- [x] Collected objects spliced into local magazine or slab freelist
- [x] No lock acquisition on the cross-CPU free path
- [x] Host test: concurrent push from multiple threads, single-thread collect, all objects recovered

### E.2 — Detect cross-CPU free and route correctly

**File:** `kernel/src/mm/slab.rs`
**Symbol:** `slab_free`
**Why it matters:** The free path must determine whether the object belongs to the current CPU's slab or another CPU's. This requires O(1) lookup of the owning CPU from the object address.

**Acceptance:**
- [x] O(1) page-base / span lookup resolves slab metadata from the object address
- [x] Slab metadata stores owning CPU and size class out-of-line so the 4096-byte size class remains usable
- [x] `slab_free(ptr)`: read owning CPU from slab metadata, compare with current CPU
- [x] Same-CPU: push to local magazine (fast path)
- [x] Different-CPU: push to victim's atomic free list (cross-CPU path)
- [x] Address-to-metadata lookup and metadata lifetime rules are documented and host-tested

---

## Track F — Interrupt Safety, Reclaim, Stats, and Validation

### F.1 — Define allocator context contract and IRQ-safe fast-path rules

**Files:**
- `kernel/src/mm/frame_allocator.rs`
- `kernel/src/mm/slab.rs`

**Symbol:** `allocate_frame`, `free_frame`, `slab_alloc`, `slab_free`
**Why it matters:** Cooperative scheduling removes kernel-preemption concerns, but current code still allocates in page-fault context. The allocator therefore needs explicit rules for IRQ-sensitive / non-sleepable paths instead of relying on "per-CPU means safe."

**Acceptance:**
- [x] Local per-CPU page-cache and magazine mutations run with interrupts masked or an equivalent local non-reentrancy guard
- [x] Slow paths document whether they may block, retry, or fail in IRQ-sensitive / page-fault-adjacent contexts
- [x] A minimal allocation-context contract (sleepable vs IRQ-sensitive) is documented even if full GFP-style flags are deferred
- [x] Regression coverage demonstrates allocator activity does not deadlock when faults or interrupts hit during allocator hot paths

### F.2 — Reclaim allocator-local caches before OOM or high-order failure

**Files:**
- `kernel/src/mm/frame_allocator.rs`
- `kernel/src/mm/slab.rs`
- `kernel/src/mm/heap.rs`

**Symbol:** `drain_per_cpu_caches`, `reclaim_empty_slabs`, `collect_remote_frees`
**Why it matters:** Order-0 hoarding in per-CPU page caches, magazines, empty slabs, or remote-free queues can starve high-order contiguous allocations used by drivers even when a meaningful amount of memory is technically reclaimable.

**Acceptance:**
- [x] OOM and high-order allocation retry paths drain per-CPU page caches, collect pending remote frees, and reclaim empty slabs / magazines before final failure
- [x] Reclaim order and locking / interrupt rules are documented to avoid cross-CPU races
- [x] High-order / contiguous allocation regression demonstrates allocator-local reclaim can recover from order-0 hoarding before returning failure

### F.3 — Preserve stats and meminfo compatibility across the allocator cutover

**Files:**
- `kernel/src/mm/frame_allocator.rs`
- `kernel/src/mm/heap.rs`
- `kernel/src/mm/slab.rs`
- `kernel/src/fs/procfs.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `userspace/coreutils-rs/src/meminfo.rs`

**Symbol:** `frame_stats`, `heap_stats`, `render_meminfo`, `sys_meminfo`
**Why it matters:** Current tests and tools assume one global frame pool and a fixed heap/slab shape. Those assumptions will change once per-CPU caches, 13 size classes, and page-backed large allocations land.

**Acceptance:**
- [x] Documentation defines whether per-CPU cached pages count as free / available memory
- [x] `frame_stats()`, `heap_stats()`, `/proc/meminfo`, and `sys_meminfo` remain coherent after adding per-CPU caches and size classes
- [x] `meminfo` exposes the new allocator state without truncation or stale fixed-cache assumptions
- [x] Existing stats-based QEMU tests are updated to the new semantics and continue to pass

### F.4 — Add staged rollout, kill switch, and concurrency validation

**Files:**
- `kernel/src/mm/heap.rs`
- `kernel-core/tests/`
- `xtask/src/main.rs`

**Symbol:** `SizeClassAllocator`
**Why it matters:** Replacing the kernel allocator stack is invasive. Bring-up needs a fallback, and the new Acquire/Release queues need stronger validation than normal host unit tests.

**Acceptance:**
- [x] A compile-time or boot-time kill switch can fall back to the current allocator during bring-up
- [x] Host-side concurrency coverage includes loom tests for the new cross-CPU free list / ordering-sensitive queue behavior
- [x] The documented host-test command uses `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`
- [x] Validation covers allocator stress, multi-core contention, and stale-mapping / zeroing regressions before the cutover is considered complete

---

## Documentation Notes

- Phase 33 introduced the buddy allocator, slab-cache infrastructure, and OOM-retry heap. Phase 53a replaces the heap and slab implementations while preserving the buddy as the backend, and it also closes the gap left by Phase 33 not broadly migrating hot kernel object families onto slab caches.
- Phase 36 added demand paging and mprotect. These are unaffected — they use `allocate_frame()` which retains the same API.
- Phase 52b made zero-on-free part of stale-mapping hardening. Phase 53a may move zeroing off the free path only if the "zero before user-visible exposure" invariant stays explicit and tested.
- Phase 52c replaced `Vec<MemoryMapping>` with `VmaTree` (BTreeMap-backed). Phase 53a follows the same pattern: replace simple data structures with scalable ones.
- The research documents in `docs/research/` provide the theoretical foundation and cross-system analysis that informed these design choices.
- Host-side allocator tests should use `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`.
- All new pure-logic code belongs in `kernel-core` for host testability. Only hardware-dependent wiring belongs in `kernel/src/mm/`.
