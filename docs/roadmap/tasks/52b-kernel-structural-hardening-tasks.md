# Phase 52b — Kernel Structural Hardening: Task List

**Status:** Complete
**Source Ref:** phase-52b
**Depends on:** Phase 52a (Kernel Reliability Fixes)
**Goal:** Replace fragile per-core scratch patterns with structurally sound alternatives, making the classes of bugs found in Phase 52 impossible rather than merely fixed.

> **Post-phase audit note:** The checked-in code contains the major Phase 52b
> structures, and the former 52d follow-up is now closed: syscall-entry
> snapshots and scheduler-driven restore are the authoritative return-state
> path, and generation-based user-copy diagnostics are active.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | AddressSpace object | None | Complete |
| B | Targeted batch TLB shootdown | A | Complete |
| C | Task-owned syscall return state | None | Complete |
| D | Typed UserBuffer wrappers | None | Complete |
| E | Frame zeroing on free | None | Complete |

---

## Track A — First-Class AddressSpace Object

### A.1 — Define AddressSpace struct

**File:** `kernel/src/mm/mod.rs`
**Symbol:** `AddressSpace`
**Why it matters:** The kernel currently has no address-space abstraction. A process's address space is a raw `PhysAddr` with no metadata, no refcount, and no per-CPU tracking. This makes targeted TLB shootdown, stale-mapping detection, and thread address-space sharing impossible.

**Acceptance:**
- [ ] `AddressSpace` struct wraps `pml4_phys: PhysAddr`, `generation: AtomicU64`, `active_on_cores: AtomicU64`
- [ ] `activate_on_core(core_id)` and `deactivate_on_core(core_id)` methods update `active_on_cores` atomically
- [ ] `bump_generation()` increments the generation counter
- [ ] `active_cores()` returns the current bitmask

### A.2 — Update Process to use Arc<AddressSpace>

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process::page_table_root` → `Process::addr_space`
**Why it matters:** Replacing the raw `Option<PhysAddr>` with `Arc<AddressSpace>` enables shared address spaces for CLONE_VM threads and provides the foundation for all other Track A/B tasks.

**Acceptance:**
- [ ] `Process.page_table_root` replaced with `Process.addr_space: Option<Arc<AddressSpace>>`
- [ ] All existing `page_table_root` access sites updated to go through `addr_space`
- [ ] `sys_fork` creates a new `Arc<AddressSpace>` for the child
- [ ] `clone(CLONE_VM)` threads share the same `Arc<AddressSpace>`
- [ ] `cargo xtask check` passes

### A.3 — Add per-core `current_addrspace` tracking

**File:** `kernel/src/smp/mod.rs`
**Symbol:** `PerCoreData::current_addrspace`
**Why it matters:** Each CPU needs to know which address space is currently loaded so that TLB shootdowns can be targeted. This is the Redox `PercpuBlock.current_addrsp` equivalent.

**Acceptance:**
- [ ] `PerCoreData` has `current_addrspace: *const AddressSpace` field
- [ ] Scheduler dispatch sets `current_addrspace` and calls `AddressSpace::activate_on_core`
- [ ] Context switch clears old AS via `deactivate_on_core` before loading new CR3
- [ ] Ordering: `deactivate_on_core` → memory fence → `activate_on_core` (prevents shootdown races)

### A.4 — Add generation counter assertions

**Files:**
- `kernel/src/mm/user_mem.rs`
- `kernel/src/arch/x86_64/syscall/mod.rs`

**Symbol:** `copy_to_user`, `copy_from_user`, syscall entry/exit
**Why it matters:** Generation counter assertions detect the address-space mapping divergence that causes the `copy_to_user` bug. If a mapping changes during a copy, the generation will have incremented.

**Acceptance:**
- [ ] `copy_to_user` logs a warning if `AddressSpace.generation` changes between translation and write
- [ ] Syscall entry asserts `per_core().current_addrspace` matches the calling process's `addr_space`
- [ ] Scheduler dispatch asserts `Cr3::read()` matches `addr_space.pml4_phys` after load

---

## Track B — Targeted Batch TLB Shootdown

### B.1 — Implement range-based `tlb_shootdown_range`

**File:** `kernel/src/smp/tlb.rs`
**Symbol:** `tlb_shootdown_range`
**Why it matters:** The current `tlb_shootdown()` handles one address at a time with a global lock and broadcast IPI. A `munmap(ptr, 1 MiB)` requires 256 sequential shootdowns. The new function handles ranges in a single IPI round-trip.

**Acceptance:**
- [ ] `tlb_shootdown_range(addr_space, start, end)` sends one IPI covering the entire range
- [ ] For ranges over `INVLPG_THRESHOLD` (32 pages), uses full CR3 reload instead of per-page `invlpg`
- [ ] IPIs sent only to cores in `addr_space.active_cores()`, not broadcast to all
- [ ] Cores not running the affected address space skip the flush in the IPI handler
- [ ] `munmap(ptr, 1 MiB)` completes in 1 IPI round-trip

### B.2 — Update munmap and mprotect to use range shootdown

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_linux_munmap`, `sys_mprotect`
**Why it matters:** These are the primary consumers of TLB shootdown. Switching them to the range API eliminates the O(pages) IPI cost.

**Acceptance:**
- [ ] `sys_linux_munmap` calls `tlb_shootdown_range` once after unmapping all pages
- [ ] `sys_mprotect` calls `tlb_shootdown_range` once after modifying all PTEs
- [ ] The old per-page `tlb_shootdown()` function is removed or deprecated

### B.3 — Add fork CoW SMP shootdown

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `cow_clone_user_pages`
**Why it matters:** Fork CoW marking currently uses local CR3 reload only — no SMP shootdown. If another CPU has stale WRITABLE TLB entries for the parent, it could write through without triggering a page fault.

**Acceptance:**
- [ ] After CoW-marking all pages, `cow_clone_user_pages` calls `tlb_shootdown_range` for the parent's user VA range
- [ ] The shootdown targets only cores in `parent_addr_space.active_cores()`

---

## Track C — Task-Owned Syscall Return State

### C.1 — Define UserReturnState and add to Task

**File:** `kernel/src/task/mod.rs`
**Symbol:** `UserReturnState`, `Task::user_return`
**Why it matters:** Moving user return state from per-core scratch to the task eliminates the ~40 manual `restore_caller_context` call sites and makes the stale-return-state bug class impossible.

**Acceptance:**
- [ ] `UserReturnState` struct contains `user_rsp`, `user_rip`, `user_rflags`, callee-saved regs, `kernel_stack_top`, `fs_base`
- [ ] `Task` has `user_return: Option<UserReturnState>` (None for kernel tasks)

### C.2 — Save user state to Task at syscall entry

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `syscall_handler` or early dispatch
**Why it matters:** The user state must be saved to the task before any blocking can occur, so the scheduler can restore it on re-dispatch.

**Acceptance:**
- [ ] At syscall handler entry (before any blocking path), `PerCoreData` user state is copied into `Task.user_return`
- [ ] The copy happens once per syscall, not at every blocking site

### C.3 — Restore from UserReturnState in scheduler dispatch

**File:** `kernel/src/task/mod.rs`
**Symbol:** `run` (scheduler dispatch loop)
**Why it matters:** The scheduler must automatically restore `syscall_user_rsp`, `syscall_stack_top`, `fs_base`, and CR3 from the task's saved state. This replaces all manual `restore_caller_context` calls.

**Acceptance:**
- [ ] Scheduler dispatch reads `task.user_return` and restores per-core state from it
- [ ] CR3 is loaded from the task's `addr_space`, not from a separate PROCESS_TABLE lookup
- [ ] TSS.RSP0 is set from `UserReturnState.kernel_stack_top`
- [ ] FS.base MSR is written from `UserReturnState.fs_base`

### C.4 — Remove all restore_caller_context calls

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `restore_caller_context`
**Why it matters:** With scheduler-driven restore, the manual restore function and all ~40 call sites become dead code. Removing them eliminates the possibility of missing a restore on a new blocking path.

**Acceptance:**
- [ ] `restore_caller_context` function is deleted
- [ ] All `saved_user_rsp` capture-and-restore patterns are deleted from syscall handlers
- [ ] `grep -r restore_caller_context kernel/` returns no results
- [ ] All existing tests pass (`cargo xtask check`, `cargo xtask test`)

---

## Track D — Typed UserBuffer Wrappers

### D.1 — Define UserSliceRo, UserSliceWo, UserSliceRw

**File:** `kernel/src/mm/user_mem.rs`
**Symbol:** `UserSliceRo`, `UserSliceWo`, `UserSliceRw`
**Why it matters:** Raw `(u64, usize)` pointer/length pairs scattered across syscall handlers make auditing copy sites difficult. Typed wrappers validate at the boundary and carry intent in the type.

**Acceptance:**
- [ ] `UserSliceRo::new(vaddr, len)` validates range (null check, kernel boundary, MAX_COPY_LEN)
- [ ] `UserSliceWo::new(vaddr, len)` same validation
- [ ] `UserSliceRo` has `copy_to_kernel(&self, dst: &mut [u8])` and `read_val<T: Copy>(&self)`
- [ ] `UserSliceWo` has `copy_from_kernel(&self, src: &[u8])` and `write_val<T: Copy>(&self, val: &T)`
- [ ] Cannot call write methods on `UserSliceRo` (compile-time enforcement)

### D.2 — Convert TCGETS/TCSETS handlers

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_linux_ioctl` (TCGETS, TCSETS, TCSETSW, TCSETSF paths)
**Why it matters:** These are the specific paths that triggered the `copy_to_user` bug investigation. Converting them validates the wrapper design against the known failure case.

**Acceptance:**
- [ ] TCGETS uses `UserSliceWo::new(arg, TERMIOS_SIZE)` then `write_val(&termios)`
- [ ] TCSETS/TCSETSW/TCSETSF use `UserSliceRo::new(arg, TERMIOS_SIZE)` then `read_val::<Termios>()`
- [ ] No raw `copy_to_user` or `copy_from_user` calls remain in the ioctl handler

### D.3 — Convert remaining copy sites

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/ipc/mod.rs`

**Symbol:** All `copy_to_user` and `copy_from_user` call sites
**Why it matters:** Full conversion ensures no raw user-pointer operations remain. `grep copy_to_user` should find only the wrapper implementation, not direct calls.

**Acceptance:**
- [ ] All syscall handler copy sites use `UserSliceRo` or `UserSliceWo` wrappers
- [ ] `copy_to_user` and `copy_from_user` are private to the `user_mem` module (not pub)
- [ ] `grep -r "copy_to_user\|copy_from_user" kernel/src/arch/` returns only wrapper usage

---

## Track E — Frame Zeroing on Free

### E.1 — Zero frames in free_frame before returning to pool

**File:** `kernel/src/mm/frame_allocator.rs`
**Symbol:** `free_frame`
**Why it matters:** Freed frames retain stale data. If a stale TLB mapping points to a freed-and-reused frame, userspace sees prior tenant contents. Zeroing on free prevents this.

**Acceptance:**
- [ ] `free_frame` calls `core::ptr::write_bytes(phys_offset + phys, 0, 4096)` before `free_to_pool`
- [ ] Only user-accessible frames are zeroed (kernel-internal frames can optionally skip for performance)
- [ ] A test verifies that accessing a freed-and-reallocated frame reads all zeros

---

## Cross-Phase Dependencies

These completed phases have load-bearing code paths that must be preserved or migrated:

| Phase | Risk | What Must Be Preserved |
|---|---|---|
| 36 (Expanded Memory) | `MemoryMapping` struct moves into `AddressSpace` | `mprotect` TLB shootdown path, demand paging |
| 38 (Filesystem) | `/proc/<pid>/maps` reads VMA list directly | Must update procfs maps generator after VMA restructure |
| 40 (Threading) | Futex table keyed by `(page_table_root, vaddr)` | **Must update futex key type** when CR3 is wrapped in `AddressSpace` |
| 43a (Crash Diagnostics) | Assertions guard exact bug class being fixed | Update `pick_next` RSP assertions after task-owned state migration |
| 43b (Trace Ring) | `TraceRing` stored in `PerCoreData` | Preserve `trace_ring` field if restructuring `PerCoreData` |
| 43c (Regression/Stress) | Proptest models test scheduler invariants | Update proptest models after per-core scheduler changes |
| 48 (Security) | `setuid`/`setgid` enforcement in syscall handler | Preserve credential checks when converting to typed `UserBuffer` |

## Documentation Notes

- All design details are in `docs/appendix/architecture/next/`
- External kernel comparisons are in `docs/appendix/architecture/next/sources.md`
- The current architecture documentation is in `docs/appendix/architecture/current/`
- Track A directly addresses the root cause identified in `docs/appendix/copy-to-user-reliability-bug.md`
- Track B addresses the TLB coherence gaps documented in `docs/appendix/architecture/current/01-memory-management.md` Section 4.7
- Track C is the long-term replacement for the stop-gap fix in Phase 52a Track A
