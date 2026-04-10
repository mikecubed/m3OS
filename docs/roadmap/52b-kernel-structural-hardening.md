# Phase 52b - Kernel Structural Hardening

**Status:** Complete
**Source Ref:** phase-52b
**Depends on:** Phase 52a (Kernel Reliability Fixes) ✅, Phase 33 (Kernel Memory) ✅, Phase 35 (True SMP) ✅, Phase 40 (Threading) ✅
**Builds on:** Replaces the fragile per-core mutable scratch pattern and implicit address-space identity with structurally sound alternatives. Informed by Redox, seL4, and Zircon architecture analysis.
**Primary Components:** kernel/src/mm/mod.rs, kernel/src/mm/user_mem.rs, kernel/src/mm/paging.rs, kernel/src/mm/frame_allocator.rs, kernel/src/smp/mod.rs, kernel/src/smp/tlb.rs, kernel/src/task/mod.rs, kernel/src/process/mod.rs, kernel/src/arch/x86_64/syscall/mod.rs

## Milestone Goal

The kernel's memory management, context management, and TLB coherence subsystems are structurally hardened against the classes of bugs discovered during Phase 52. Address-space identity is a first-class object, user buffers are typed at the syscall boundary, syscall return state is task-owned, TLB shootdowns are batched and targeted, and freed frames are zeroed.

## Why This Phase Exists

The Phase 52 bug investigations revealed that the `copy_to_user` reliability bug and the SSHD hang are symptoms of deeper structural patterns:

1. **Per-core mutable scratch for return state:** ~40 manual `restore_caller_context` call sites, each required to prevent the same class of bug. Any new blocking syscall path that misses this call silently introduces a wrong-RSP return to userspace.

2. **No first-class address-space object:** The kernel cannot track which CPUs are using an address space, cannot send targeted TLB shootdowns, and cannot detect stale mappings. The `copy_to_user` bug investigation needed this tracking but had to use ad-hoc CR3 logging.

3. **Raw user pointers scattered across syscall handlers:** No typed validation at the syscall boundary makes auditing copy sites difficult and errors invisible until runtime.

4. **Single-address TLB shootdown with broadcast:** O(pages) IPIs for bulk operations, hitting all cores regardless of which address space is affected.

5. **Freed frames retain stale data:** A stale TLB mapping to a freed-and-reused frame exposes prior tenant contents.

Phase 52a fixes the immediate bugs. This phase eliminates the structural patterns that made those bugs possible.

## Learning Goals

- Understand why address-space identity should be a first-class kernel object (Redox: `AddrSpaceWrapper`, Zircon: `VmAspace`)
- Learn how task-owned return state eliminates manual restore requirements (Redox: kernel-stack-based return values)
- See how typed user-buffer wrappers centralize validation (Redox: `UserSliceRo/Wo/Rw`)
- Understand targeted vs. broadcast TLB shootdown (Redox: `used_by` + `tlb_ack`)
- Learn the tradeoffs of zero-on-free vs. zero-on-allocate for frame security

## Feature Scope

### First-class AddressSpace object

Replace the raw `PhysAddr` in `Process::page_table_root` with `Arc<AddressSpace>` that wraps the PML4 physical address, a generation counter (incremented on any mapping change), an `active_on_cores` bitmask (updated by scheduler dispatch), and the VMA collection. Threads sharing an address space (CLONE_VM) share the same `Arc`.

**Design reference:** `docs/appendix/architecture/next/01-memory-management.md` Section 1.
**Comparison:** Redox `AddrSpaceWrapper` (`src/context/memory.rs`), Zircon `VmAspace` (`zircon/kernel/vm/include/vm/vm_aspace.h`).

### Typed UserBuffer wrappers

Introduce `UserSliceRo`, `UserSliceWo`, `UserSliceRw` wrapper types that validate user pointers at the syscall boundary and carry read/write intent in the type. Convert all `copy_to_user`/`copy_from_user` call sites to use the wrappers.

**Design reference:** `docs/appendix/architecture/next/02-process-context.md` Section 2.
**Comparison:** Redox `UserSlice<const READ: bool, const WRITE: bool>` (`src/syscall/usercopy.rs`).

### Task-owned syscall return state

Move `syscall_user_rsp`, `syscall_stack_top`, and `fs_base` from `PerCoreData` mutable scratch into a `UserReturnState` struct in the `Task`. The scheduler dispatch loop restores this state automatically when selecting a task. Remove all ~40 `restore_caller_context` call sites.

**Design reference:** `docs/appendix/architecture/next/02-process-context.md` Section 1.
**Comparison:** Redox stores return values in the kernel-stack interrupt frame (`(*stack).scratch.rax = ret`), making them task-owned. seL4 stores all task state in the TCB.

### Batch TLB shootdown with address-space targeting

Replace the single-address `SHOOTDOWN_ADDR` protocol with a range-based `ShootdownRequest` that sends one IPI covering the entire range. Use `AddressSpace.active_on_cores` to send IPIs only to cores running the affected address space, not broadcast to all.

**Design reference:** `docs/appendix/architecture/next/01-memory-management.md` Section 2.
**Comparison:** Redox `AddrSpaceWrapper.used_by` + `tlb_ack`, seL4 per-VSpace CPU bitmap in PML4.

### Frame zeroing on free

Zero all user-accessible frames before returning them to the free pool. This prevents stale data exposure through stale TLB mappings (the "amplifier" effect identified in the `copy_to_user` investigation).

**Design reference:** `docs/appendix/architecture/next/01-memory-management.md` Section 3.

## Important Components and How They Work

### AddressSpace struct

```rust
pub struct AddressSpace {
    pml4_phys: PhysAddr,
    generation: AtomicU64,
    active_on_cores: AtomicU64,
    vmas: Mutex<Vec<MemoryMapping>>,  // Phase 52c upgrades to BTreeMap
    brk_current: AtomicU64,
    mmap_next: AtomicU64,
}
```

The scheduler calls `activate_on_core(core_id)` when dispatching a task and `deactivate_on_core(core_id)` when switching away. TLB shootdown reads `active_cores()` to determine which cores need IPIs.

### UserReturnState in Task

```rust
pub struct UserReturnState {
    pub user_rsp: u64,
    pub user_rip: u64,
    pub user_rflags: u64,
    pub user_rbx: u64, pub user_rbp: u64,
    pub user_r12: u64, pub user_r13: u64,
    pub user_r14: u64, pub user_r15: u64,
    pub kernel_stack_top: u64,
    pub fs_base: u64,
}
```

Saved at syscall entry (from `PerCoreData` temporary state), restored by scheduler dispatch. `PerCoreData` fields become write-once-at-entry scratch, not long-lived state.

## How This Builds on Earlier Phases

- Extends Phase 33 (Kernel Memory) with frame zeroing and eventual VMA improvements
- Extends Phase 35 (True SMP) with targeted TLB shootdown and per-core address-space tracking
- Extends Phase 40 (Threading) with `Arc<AddressSpace>` sharing for CLONE_VM threads
- Directly addresses the root causes identified in the Phase 52 bug investigations
- Prepares the kernel for Phase 53 (Headless Hardening) reliability claims
- Prepares the kernel for Phase 54 (Deep Serverization) which needs robust IPC and memory management

## Implementation Outline

1. Define `AddressSpace` struct, wrap existing `page_table_root` + VMAs
2. Update `Process` to use `Arc<AddressSpace>` instead of `Option<PhysAddr>`
3. Update scheduler dispatch to call `activate_on_core`/`deactivate_on_core`
4. Add `current_addrspace` pointer to `PerCoreData`
5. Implement batch `tlb_shootdown_range` using `active_on_cores`
6. Define `UserReturnState`, add to `Task`
7. Save user state to `UserReturnState` at syscall entry
8. Restore from `UserReturnState` in scheduler dispatch
9. Remove all `restore_caller_context` calls
10. Define `UserSliceRo`/`UserSliceWo`/`UserSliceRw`, convert syscall handlers
11. Add frame zeroing to `free_frame`
12. Add generation counter assertions around `copy_to_user`

## Acceptance Criteria

- No `restore_caller_context` calls remain in the codebase
- `UserSliceRo`/`UserSliceWo` are used at all syscall boundary copy sites
- `AddressSpace.active_on_cores` correctly tracks which CPUs have each AS loaded
- TLB shootdown for `munmap(ptr, 1 MiB)` sends 1 IPI (not 256)
- Freed frames read as all-zeros when accessed through a stale mapping
- `copy_to_user` reliability reproducer (real DeviceTTY/keyboard-IRQ path on SMP4) no longer fails
- `cargo xtask check` passes
- `cargo xtask test` passes

## Companion Task List

- [Phase 52b Task List](./tasks/52b-kernel-structural-hardening-tasks.md)

## How Real OS Implementations Differ

- Redox uses `Arc<AddrSpaceWrapper>` with `used_by: LogicalCpuSet` and `tlb_ack: AtomicU32` for the same purpose. Source: `src/context/memory.rs`.
- Zircon's `VmAspace` is independently ref-counted with `active_cpus_` bitmap and PCID-deferred TLB flush. Source: `zircon/kernel/vm/include/vm/vm_aspace.h`.
- seL4 embeds a TLB bitmap in unused PML4 entries (`tlb_bitmap.h`) — a clever space-saving trick m3OS could consider.
- Linux uses `mm_struct` with `cpu_bitmap` for lazy TLB mode and targeted shootdown.

## Deferred Until Later

- VMA tree (BTreeMap or interval tree) — deferred to Phase 52c
- Per-core scheduler with work-stealing — deferred to Phase 52c
- Dynamic IPC resource pools — deferred to Phase 52c
- ISR-direct notification wakeup — deferred to Phase 52c
