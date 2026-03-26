# Phase 17 — Memory Reclamation: Implementation Progress

**Branch:** `phase-17-memory-reclamation`
**Status:** In Progress

## Track Layout

| Track | Scope | Dependencies | Status |
|-------|-------|-------------|--------|
| A | Free-list frame allocator | — | Done |
| B | Frame reference counting | A | Done |
| C | Process exit cleanup | A | Done |
| D | Growable kernel heap | A | Done |
| E | Copy-on-write fork | A, B | In Progress |
| F | Validation and documentation | C, D, E | Pending |

## Track A — Free-List Frame Allocator

- [x] P17-T001: Define `FreeListAllocator` struct
- [x] P17-T002: Implement `init(regions)` — build free list from usable regions
- [x] P17-T003: Implement `allocate_frame()` — pop from head
- [x] P17-T004: Implement `free_frame(phys)` — push to head
- [x] P17-T005: Double-free detection via magic value
- [x] P17-T006: Replace `BumpAllocator` with `FreeListAllocator`
- [x] P17-T007: Add `free_count()` and `total_frames()` accessors
- [x] P17-T008: Log frame allocator stats at boot

## Track B — Frame Reference Counting

- [x] P17-T009: Determine highest physical frame number
- [x] P17-T010: Allocate refcount table (`Vec<AtomicU16>`)
- [x] P17-T011: Implement `refcount_inc()`
- [x] P17-T012: Implement `refcount_dec() -> u16`
- [x] P17-T013: Implement `refcount_get() -> u16`
- [x] P17-T014: Hook `refcount_inc` into `allocate_frame()`
- [x] P17-T015: Hook `refcount_dec` into `free_frame()`

## Track C — Process Exit Cleanup

- [x] P17-T016: Call `free_process_page_table()` in `sys_exit`
- [x] P17-T017: Call `free_process_page_table()` in `fault_kill_trampoline`
- [x] P17-T018: Verify 4-level page table walk (fixed shared kernel entry detection)
- [x] P17-T019: Update `free_process_page_table()` to use refcounting
- [x] P17-T020: Reclaim kernel stacks in `drain_dead()` (already correct via Box drop)
- [x] P17-T021: Verify `Task::_stack` drop behavior (confirmed correct)

## Track D — Growable Kernel Heap

- [x] P17-T022: Increase heap virtual reservation (64 MiB ceiling)
- [x] P17-T023: Track current mapped extent with `AtomicUsize`
- [x] P17-T024: Implement `grow_heap(additional_bytes)`
- [x] P17-T025: Hook OOM path — attempt growth before panic
- [x] P17-T026: Safety cap on max heap size

## Track E — Copy-on-Write Fork

- [ ] P17-T027: Implement `cow_clone_user_pages()`
- [ ] P17-T028: Handle non-writable pages (share directly)
- [ ] P17-T029: Flush parent TLB after clearing writable bits
- [ ] P17-T030: Replace `copy_user_pages()` with CoW in `sys_fork`
- [ ] P17-T031: Detect CoW faults in page fault handler
- [ ] P17-T032: Implement CoW fault resolution
- [ ] P17-T033: Refcount-1 fast path (remap without copy)
- [ ] P17-T034: Ensure `execve` correctness with CoW pages

## Track F — Validation

- [ ] P17-T035: free_frame() returns frames; free_count() increases after exit
- [ ] P17-T036: Fork 100 + exit reclaims frames
- [ ] P17-T037: CoW sharing and fault resolution works
- [ ] P17-T038: Kernel heap grows past 1 MiB
- [ ] P17-T039: Kernel stacks reclaimed after fork+exit
- [ ] P17-T040: Double-free panics with diagnostic
- [ ] P17-T041: No regressions (shell, pipes, networking)
- [ ] P17-T042: `cargo xtask check` passes
- [ ] P17-T043: QEMU boot validation
- [ ] P17-T044: Update memory documentation
