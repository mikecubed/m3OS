# Phase 36 — Expanded Memory: Task List

**Status:** Planned
**Source Ref:** phase-36
**Depends on:** Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP Multitasking) ✅
**Goal:** Convert the eager mmap allocator to demand paging, implement mprotect,
extend VMA tracking with protection bits, and increase QEMU RAM and disk image
size to support large cross-compiled binaries.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | VMA tracking with protection bits | — | Planned |
| B | Demand paging (lazy mmap) | A | Planned |
| C | mprotect syscall | A | Planned |
| D | QEMU and disk image expansion | — | Planned |
| E | Integration testing and documentation | A, B, C, D | Planned |

---

## Track A — VMA Tracking with Protection Bits

The existing `MemoryMapping` struct only tracks start and length. Demand paging and
mprotect both require knowing the protection bits and flags of each mapped region.

### A.1 — Extend `MemoryMapping` with prot and flags fields

**File:** `kernel/src/process/mod.rs`
**Symbol:** `MemoryMapping`
**Why it matters:** The page fault handler needs protection bits to know what
permissions to set when demand-mapping a frame, and mprotect needs flags to split
and update VMAs correctly.

**Acceptance:**
- [ ] `MemoryMapping` has `prot: u64` field storing `PROT_READ | PROT_WRITE | PROT_EXEC` bits
- [ ] `MemoryMapping` has `flags: u64` field storing `MAP_PRIVATE | MAP_ANONYMOUS` bits
- [ ] Existing mmap callsites populate both fields
- [ ] Existing munmap logic unchanged — still works with the extended struct

### A.2 — Update mmap to record prot and flags in VMA

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_mmap`
**Why it matters:** The VMA entry created by mmap must carry the caller's requested
protection and flags so later fault handling and mprotect can use them.

**Acceptance:**
- [ ] `sys_linux_mmap()` stores `prot` and `flags` arguments in the `MemoryMapping`
- [ ] VMA list remains correctly ordered and non-overlapping
- [ ] `cargo xtask check` passes

### A.3 — Add VMA lookup helper

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process::find_vma`
**Why it matters:** Both the page fault handler and mprotect need to find the VMA
containing a given virtual address. A shared helper avoids duplicating the linear scan.

**Acceptance:**
- [ ] `find_vma(addr)` returns `Option<&MemoryMapping>` for the VMA containing `addr`
- [ ] Returns `None` for addresses not in any VMA
- [ ] Used by the page fault handler (Track B) and mprotect (Track C)

---

## Track B — Demand Paging (Lazy mmap)

Convert mmap from eager frame allocation to lazy allocation. Frames are allocated
on first access by the page fault handler.

### B.1 — Remove eager frame allocation from mmap

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_mmap`
**Why it matters:** This is the core change — mmap must stop allocating physical
frames at map time. It records the VMA and returns the virtual address immediately.

**Acceptance:**
- [ ] `sys_linux_mmap()` does NOT call `allocate_frame()` for anonymous mappings
- [ ] `sys_linux_mmap()` does NOT map any pages into the page table
- [ ] Returns a valid virtual address from the process's mmap region
- [ ] VMA is recorded in the process's mapping list with correct prot/flags

### B.2 — Extend page fault handler with VMA-based demand mapping

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `page_fault_handler`
**Why it matters:** This is the demand paging implementation — the page fault handler
must check the VMA list to decide whether to allocate a frame or deliver SIGSEGV.

**Acceptance:**
- [ ] Faulting address inside a valid VMA triggers frame allocation, zero-fill, and mapping
- [ ] Page permissions match the VMA's `prot` field (read-only VMA produces read-only PTE)
- [ ] Faulting address outside all VMAs delivers SIGSEGV
- [ ] CoW faults (Phase 17) still resolved before VMA check — existing behavior preserved
- [ ] Stack demand-paging still works alongside VMA demand-paging

### B.3 — Update `demand_map_user_page()` to accept protection flags

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `demand_map_user_page`
**Why it matters:** The existing function always maps pages as user-writable. Demand
paging for VMA regions must respect the VMA's protection bits.

**Acceptance:**
- [ ] `demand_map_user_page()` accepts a `prot` parameter
- [ ] Maps read-only pages without the writable bit
- [ ] Maps executable pages with the execute bit (no NX)
- [ ] Existing stack demand-paging callers updated to pass appropriate flags
- [ ] TLB flushed after mapping (local core; SMP shootdown not needed for fresh mappings)

### B.4 — Large mmap region validation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_linux_mmap`
**Why it matters:** With demand paging, 256 MB+ mmap regions should be near-free in
physical memory. This task validates that the virtual address space management
handles large regions correctly.

**Acceptance:**
- [ ] `mmap(NULL, 256*1024*1024, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)` succeeds
- [ ] Physical memory usage does not increase until pages are touched
- [ ] Touching individual pages in the region triggers demand faults that succeed
- [ ] munmap of a large region frees only the frames that were actually allocated

---

## Track C — mprotect Syscall

Replace the current `mprotect` stub (returns 0, does nothing) with a real
implementation that changes page permissions.

### C.1 — Implement mprotect page table walk

**Files:**
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/mm/paging.rs`
**Symbol:** `sys_mprotect`
**Why it matters:** mprotect must walk the page table for the specified range and
update PTE permission bits in place. This is the core implementation.

**Acceptance:**
- [ ] `mprotect(addr, len, PROT_READ)` removes the writable bit from all PTEs in range
- [ ] `mprotect(addr, len, PROT_READ|PROT_WRITE|PROT_EXEC)` sets full permissions
- [ ] `mprotect(addr, len, PROT_NONE)` makes pages inaccessible (guard pages)
- [ ] Handles pages that are not yet demand-mapped (updates VMA prot only, no PTE to change)
- [ ] Returns `-EINVAL` for unaligned addresses or zero length
- [ ] Returns `-ENOMEM` for addresses not in any VMA

### C.2 — VMA splitting on mprotect boundaries

**File:** `kernel/src/process/mod.rs`
**Symbol:** `Process::split_vma`
**Why it matters:** If mprotect covers only part of a VMA, the VMA must be split
into separate regions with different protection bits.

**Acceptance:**
- [ ] mprotect on a sub-range of a VMA splits it into 2 or 3 VMAs with correct bounds
- [ ] The modified sub-range gets the new protection bits
- [ ] Surrounding sub-ranges retain original protection bits
- [ ] munmap still works correctly after VMA splits

### C.3 — TLB shootdown for mprotect

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_mprotect`
**Why it matters:** On SMP, changing page permissions requires flushing the TLB on
all cores that may have cached the old PTEs.

**Acceptance:**
- [ ] mprotect flushes TLB locally for affected pages
- [ ] mprotect sends IPI TLB shootdown to other cores (reusing Phase 35 infrastructure)
- [ ] A process running on core 1 sees updated permissions after mprotect on core 0

---

## Track D — QEMU and Disk Image Expansion

These are configuration-only changes that enable testing with larger workloads.

### D.1 — Increase QEMU RAM to 1 GB

**File:** `xtask/src/main.rs`
**Symbol:** `qemu_args`
**Why it matters:** 256 MB is insufficient for cross-compiled toolchains that rely
on demand paging. 1 GB provides headroom for Phase 50 workloads.

**Acceptance:**
- [ ] QEMU `-m` argument changed from `256` to `1024`
- [ ] Kernel boots successfully with 1 GB RAM
- [ ] Frame allocator correctly detects and manages the larger memory pool

### D.2 — Expand data partition to 1 GB

**File:** `xtask/src/main.rs`
**Symbol:** `create_data_disk`
**Why it matters:** 128 MB is insufficient for cross-compiled toolchains (Clang ~150 MB,
Python ~60 MB, Node.js ~80 MB, git ~20 MB).

**Acceptance:**
- [ ] `DISK_SIZE` constant changed from `128 * 1024 * 1024` to `1024 * 1024 * 1024`
- [ ] Data partition mounts and is usable at the larger size
- [ ] Existing filesystem tests still pass

---

## Track E — Integration Testing and Documentation

### E.1 — CoW fork regression test

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `page_fault_handler`
**Why it matters:** Demand paging adds a new path to the page fault handler. CoW
fork (Phase 17) must still work correctly — the handler must resolve CoW before
checking VMAs.

**Acceptance:**
- [ ] Fork-exec workloads still function correctly
- [ ] Parent and child processes with shared pages trigger CoW on write, not demand-map
- [ ] Multi-process shell workloads (pipe chains) pass without regression

### E.2 — Demand paging stress test

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `page_fault_handler`
**Why it matters:** Demand paging must work under pressure — many concurrent
processes faulting pages simultaneously on multiple cores.

**Acceptance:**
- [ ] Spawn 4+ processes each mapping 16 MB; all complete without panic
- [ ] Physical memory usage grows only as pages are touched
- [ ] No deadlocks in the page fault handler under concurrent faults

### E.3 — mprotect validation

**File:** `kernel/src/arch/x86_64/syscall.rs`
**Symbol:** `sys_mprotect`
**Why it matters:** The stub-to-real transition must not break musl's stack guard
page setup or any other existing mprotect callers.

**Acceptance:**
- [ ] musl-linked binaries still start correctly (musl calls mprotect for stack guard)
- [ ] Write to a `PROT_READ`-only page delivers SIGSEGV
- [ ] `PROT_NONE` guard pages trap on any access

### E.4 — Run full existing test suite

**File:** `xtask/src/main.rs`
**Symbol:** `test`
**Why it matters:** Memory subsystem changes are high-risk — every existing test
must still pass.

**Acceptance:**
- [ ] All QEMU integration tests pass
- [ ] All kernel-core host tests pass
- [ ] `cargo xtask check` clean (no warnings)

### E.5 — Update documentation

**Files:**
- `docs/roadmap/36-expanded-memory.md`
- `docs/roadmap/tasks/36-expanded-memory-tasks.md`
- `docs/roadmap/README.md`
- `docs/33-kernel-memory.md`
**Symbol:** n/a
**Why it matters:** Roadmap docs and the kernel memory learning doc must reflect
the demand paging extension.

**Acceptance:**
- [ ] Phase 36 design doc updated with completion status
- [ ] Task list updated with completion status and any deferred items
- [ ] Roadmap README row updated from "Planned" to "Complete"
- [ ] `docs/33-kernel-memory.md` updated to mention demand paging as a Phase 36 extension

---

## Documentation Notes

- Phase 36 converts the Phase 33 eager mmap allocator to lazy demand paging.
- The existing `demand_map_user_page()` function (used for stack growth) is generalized
  to handle VMA-based demand faults with per-region protection bits.
- The mprotect stub (Phase 21, returns 0) is replaced with a real implementation.
- VMA splitting is new — Phase 33's munmap already handles VMA shrinking and
  hole-punching, but mprotect introduces splitting at arbitrary boundaries.
