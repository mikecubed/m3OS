# Phase 17: Memory Reclamation

**Aligned Roadmap Phase:** Phase 17
**Status:** Complete
**Source Ref:** phase-17

This document covers the memory reclamation infrastructure added in Phase 17:
the free-list frame allocator, per-frame reference counting, copy-on-write
fork, growable kernel heap, kernel stack lifecycle, and process page table
teardown. For foundational memory concepts (physical vs virtual, 4-level
paging, address space layout), see [docs/02-memory.md](02-memory.md).

## Free-List Frame Allocator

### Data structure

The allocator is an intrusive singly-linked list threaded through the free
frames themselves. Each free 4 KiB frame stores two 64-bit words via the
bootloader's physical-memory offset mapping:

```
Bytes 0..8:   physical address of the next free frame (0 = end of list)
Bytes 8..16:  FREE_MAGIC sentinel (0xDEAD_F4EE_F4EE_DEAD)
```

The `FreeListAllocator` struct holds `head` (physical address of the first
free frame), `free_count`, `total_frames`, and `phys_offset`. It lives
behind a `spin::Mutex` in the global `FRAME_ALLOCATOR`.

### Initialization

`init()` walks the bootloader's `MemoryRegion` array. For each `Usable`
region, it aligns the start up and end down to 4 KiB boundaries, skips
anything below `ALLOC_MIN_ADDR` (1 MiB), and pushes each frame onto the
free list. It also records `max_frame_number` (highest usable physical
frame number), which sizes the refcount table later.

### Allocation path

`allocate_frame()` pops from the head:

1. Read `head`; if zero, return `None` (out of memory).
2. Read the next pointer from `head`'s first 8 bytes.
3. Clear the magic sentinel (so double-free detection works if the frame
   is freed again later).
4. Advance `head`, decrement `free_count`.
5. If refcounting is initialized, set the frame's refcount to 1.

### Free path

`free_frame(phys)` checks refcounting first:

1. If refcounting is active and the frame's refcount is > 0, decrement it.
   If the new count is still > 0, the frame is shared -- return without
   freeing.
2. Otherwise, push the frame onto the free list via `free_to_list()`:
   - Assert the address is >= 1 MiB and page-aligned.
   - Read bytes 8..16; if they equal `FREE_MAGIC`, panic with a
     double-free diagnostic.
   - Write the current `head` into bytes 0..8, write `FREE_MAGIC` into
     bytes 8..16, set `head = phys`, increment `free_count`.

Frames allocated before refcounting was enabled have refcount 0 and are
freed directly without decrementing.

## Frame Reference Counting

A global `Vec<AtomicU16>` table indexed by frame number (`phys_addr / 4096`)
provides per-frame reference counts. The table is heap-allocated after
`heap::init_heap` and sized to `max_frame_number + 1` entries, all
initialized to zero.

Key operations (all use `SeqCst` ordering):

| Function | Behavior |
|----------|----------|
| `refcount_inc(phys)` | Atomic increment; panics on overflow (> `u16::MAX`) |
| `refcount_dec(phys)` | Atomic decrement; panics on underflow; returns new count |
| `refcount_get(phys)` | Atomic load of the current count |

### Interaction with CoW

- `allocate_frame()` sets refcount to 1 for freshly allocated frames.
- `cow_clone_user_pages()` calls `refcount_inc()` for each shared frame,
  bringing the count to 2 (parent + child).
- `resolve_cow_fault()` calls `free_frame()` on the old frame after
  copying, which decrements its refcount. If the frame is still shared
  (count > 0), it stays allocated; if it reaches 0, it returns to the
  free list.
- `free_process_page_table()` calls `free_frame()` for each user leaf
  page, which decrements refcounts and only reclaims frames with no
  remaining references.

## Copy-on-Write Fork

### Step 1: Clone page table entries (`cow_clone_user_pages`)

Called from `sys_fork` after allocating the child's PML4. Walks all
user-half PTEs (PML4 indices 0..256) in the parent's page table:

```
For each present, user-accessible leaf PTE:
  1. Compute child_flags:
     - If the page was WRITABLE: clear WRITABLE, set BIT_9 (CoW marker)
     - If already read-only (.text/.rodata): keep flags unchanged
  2. Map the same physical frame in the child via map_to_with_table_flags()
     - Leaf PTE uses child_flags (no WRITABLE, BIT_9 for CoW pages)
     - Intermediate entries (PD, PDPT) use PRESENT | WRITABLE | USER_ACCESSIBLE
       so that writes succeed after CoW resolution restores the leaf WRITABLE bit
  3. If the parent page was writable, update the parent PTE to match
     (clear WRITABLE, set BIT_9)
  4. Increment the frame's reference count
```

After the walk, the parent's TLB is flushed via a CR3 reload to ensure
the CPU sees the newly cleared WRITABLE bits.

The `map_to_with_table_flags()` call is critical: using plain `map_to()`
would derive intermediate entry flags from the leaf flags, creating
non-writable PD/PDPT entries. x86_64 checks WRITABLE at every page table
level, so CoW resolution would silently fail even after setting the leaf
PTE writable.

### Step 2: Detect CoW faults (page fault handler)

The page fault ISR checks three conditions:

1. The fault came from ring 3 (`stack_frame.code_segment.rpl() == Ring3`)
2. The error code has `CAUSED_BY_WRITE` and `PROTECTION_VIOLATION`
   (write to a present, non-writable page)
3. The PTE for the faulting address has `BIT_9` set (CoW marker)

If all three hold, `resolve_cow_fault()` is called. Read-only pages
without BIT_9 (e.g., .text segments) remain genuine protection violations
and kill the process.

### Step 3: Resolve the fault (`resolve_cow_fault`)

Manually walks the 4-level page table to find the faulting PTE, then:

**Fast path** (refcount <= 1, sole owner):
- Set the PTE WRITABLE, clear BIT_9.
- Flush the TLB entry. No copy, no allocation.

**Slow path** (refcount > 1, shared frame):
1. Allocate a fresh frame. On OOM, return `false` -- the page fault
   handler falls through to kill the process rather than panicking the
   kernel.
2. Copy 4 KiB from the old frame to the new frame via the physical-memory
   offset mapping.
3. Update the PTE: point to the new frame, set WRITABLE, clear BIT_9.
4. Flush the TLB entry.
5. Call `free_frame()` on the old physical address, which decrements its
   refcount and reclaims it if no other process still maps it.

## Heap Growth Strategy

The kernel heap starts at `HEAP_START = 0xFFFF_8000_0000_0000` with 1 MiB
mapped at boot. The virtual region is reserved up to `HEAP_MAX_SIZE`
(64 MiB); additional pages are mapped on demand.

`grow_heap(additional_bytes)`:

1. Round up to page boundary.
2. Check that `current_mapped + additional_bytes <= HEAP_MAX_SIZE`;
   refuse if exceeded.
3. For each new page in the range, allocate a frame and `map_to()` it
   with `PRESENT | WRITABLE`.
4. If frame allocation or mapping fails mid-way, stop (partial growth).
   A failed `map_to` returns the frame to the allocator to avoid leaking.
5. Call `ALLOCATOR.lock().extend(bytes_mapped)` to tell the linked-list
   allocator about the newly available memory.
6. Update `HEAP_MAPPED`.

The `#[alloc_error_handler]` calls `try_grow_on_oom()`, which attempts a
1 MiB growth. Because the handler's signature is `fn(Layout) -> !`, it
cannot retry the failed allocation -- the extended memory becomes available
for the *next* allocation, and the current one still panics. In practice,
the next identical allocation succeeds.

## Kernel Stack Lifecycle

Each kernel task gets a stack allocated from the heap:

```rust
let mut stack = alloc::vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice();
```

The `Task` struct owns this allocation via the `_stack: Box<[u8]>` field.
When a process exits or is killed by a fault:

1. `sys_exit` or `fault_kill_trampoline` marks the task as `Dead`.
2. The scheduler's `drain_dead()` removes `Dead` tasks from the task vec.
3. Removing a `Task` drops it, which drops `_stack`, returning the heap
   allocation to the linked-list allocator.

Before Phase 17, `free_frame()` was a no-op, so heap memory backing
kernel stacks could never be reused even after the `Box` was dropped
(the heap allocator could reuse the virtual region, but once the heap was
full, no new frames could be mapped). With the free-list allocator, both
the heap recycling and frame-level reclamation paths work correctly.

## Process Page Table Cleanup (`free_process_page_table`)

Called from `sys_exit` and `fault_kill_trampoline` after switching to the
kernel's CR3. Walks the dying process's PML4 and frees all process-private
frames:

1. **Identify private PML4 entries**: iterate PML4[0..256]. Skip entries
   whose PDPT frame address matches the kernel's PML4 (shared kernel
   mappings at the same physical address are not process-private).

2. **Walk PDPT -> PD -> PT**: for each private PDPT, collect non-huge
   child PD addresses; for each PD, collect non-huge child PT addresses;
   for each PT, collect user-accessible leaf frame addresses.

3. **Free leaf frames**: call `free_frame()` on each user leaf page.
   This decrements refcounts -- CoW-shared frames are only reclaimed
   when the last process unmaps them.

4. **Free structure frames**: after freeing leaves, free the PT frame
   (if refcount <= 1), then the PD frame, then the PDPT frame, then
   the PML4 frame. The refcount check prevents freeing page table
   structure frames that might still be referenced.

5. **Scoping discipline**: all `&PageTable` references are scoped within
   a `collect_children()` helper that returns a `Vec<u64>` of addresses.
   The references drop before `free_frame()` writes allocator metadata
   into the frame, avoiding use-after-free.

The `collect_children` helper validates physical addresses (non-zero,
page-aligned) before dereferencing, and uses a `filter` function parameter
to distinguish non-huge intermediate entries from user-accessible leaf
entries.
