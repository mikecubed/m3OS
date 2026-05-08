# Phase 75 - W^X Enforcement

**Status:** Planned
**Source Ref:** phase-75
**Depends on:** Phase 11 (Process Model) ✅, Phase 36 (Memory Subsystem Expansion) ✅
**Builds on:** Hardens the ELF loader and `mprotect` path introduced in Phase 11 and Phase 36 to enforce write-XOR-execute for all userspace code and data pages
**Primary Components:** `kernel/src/elf/`, `kernel/src/mm/user_space.rs`, `kernel/src/syscall/` (`sys_mprotect`)

## Milestone Goal

Every userspace process launched by m3OS has its code pages mapped read-execute and its data pages mapped read-write, with no page simultaneously writable and executable. An explicit `mprotect(PROT_WRITE | PROT_EXEC)` call from userspace returns `EINVAL`. Stack and heap pages carry the `NO_EXECUTE` bit at all times. All existing in-tree binaries continue to function correctly under these constraints.

## Why This Phase Exists

The current ELF loader in `kernel/src/mm/user_space.rs` maps all code segments with `WRITABLE | USER_ACCESSIBLE` and no `NO_EXECUTE` separation (`user_space.rs:135, 143`). This is a well-known exploit enabler: a vulnerability that achieves an arbitrary write into a code page can immediately redirect execution. The audit (§ E1) identified this as a pre-1.0 security hardening item.

W^X is a foundational memory safety guarantee present in every modern OS since OpenBSD 3.3 (2003). It costs nothing at runtime (no extra page faults on normal code execution) and eliminates an entire class of memory-corruption exploit primitives. It must be in place before Phase 80 (Node.js) and Phase 81 (Claude Code) introduce JIT compilation, which requires a documented exception path.

SRP: the ELF loader's sole concern is splitting PT_LOAD segments by `p_flags`; a single `mprotect` validator then enforces W^X uniformly across all later `mmap`/`mprotect` calls — no scattered permission checks elsewhere in the kernel. TDD: `is_wx_violation(prot)` is a pure predicate that host-tests trivially in `kernel-core`; the kernel-side enforcement is validated in QEMU with a two-case test binary — the negative case (`mprotect(PROT_WRITE | PROT_EXEC)` → `EINVAL`) and the positive case (the JIT `RW-` → `R-X` toggle) together prove that enforcement is both present and non-breaking.

## Learning Goals

- Understand how x86_64 page-table `NX` bits enforce non-executable mappings at the hardware level
- Learn how an ELF loader distinguishes text (PT_LOAD, execute) from data (PT_LOAD, no-execute) segments
- See why W^X requires the ELF loader, `mprotect`, and the initial stack/heap setup to all agree
- Understand the JIT exception pattern: `mprotect` to `RW-` to write code, then `mprotect` to `R-X` to execute it

## Feature Scope

### ELF loader text/data segment separation

The ELF loader currently maps all PT_LOAD segments with identical flags. This phase teaches it to read the `p_flags` field on each PT_LOAD segment: `PF_X` means execute (map `R-X`); absence of `PF_X` means data (map `RW-`). A segment with both `PF_W` and `PF_X` in the ELF header is an error: the loader logs a warning and refuses to map it, returning `ENOEXEC`.

### `mprotect` W^X validation

`sys_mprotect` already exists. This phase adds a pre-mapping validation: if the requested protection includes both `PROT_WRITE` and `PROT_EXEC`, the syscall returns `EINVAL` immediately, before touching any page table entries. The error is documented in the syscall reference as the expected W^X enforcement behavior.

### Stack and heap NX enforcement

The initial user stack mapping (created by the ELF loader before entering userspace) and all future `brk`/`mmap(PROT_READ | PROT_WRITE)` mappings apply `NO_EXECUTE` unconditionally. The check is added at the stack-setup site in `user_space.rs` and in the `brk`/`mmap` kernel paths.

### JIT exception documentation

A JIT engine (future Node.js in Phase 80, Cranelift, etc.) cannot use a page that is simultaneously `PROT_WRITE | PROT_EXEC`. The supported pattern is: (1) allocate with `mmap(PROT_WRITE)`, (2) write machine code, (3) `mprotect(PROT_READ | PROT_EXEC)`. This is documented in `docs/appendix/architecture-and-syscalls.md` as the required JIT code-generation pattern. No new syscall is needed; the pattern works with existing `mmap` and `mprotect`.

### Phase 11 and Phase 36 design doc updates

The "Deferred Until Later" section of the Phase 11 design doc notes W^X as deferred. The comment in `user_space.rs` referencing "Phase 6+ deferred" is removed. Both the Phase 11 and Phase 36 docs are updated to record W^X as a baseline guarantee as of Phase 75.

## Important Components and How They Work

### ELF loader PT_LOAD flag dispatch

In `kernel/src/elf/loader.rs`, the current `map_segment` function calls `user_space::map_user_pages` with a fixed flag set. After this phase it branches on `segment.flags() & PF_X`:

- `PF_X` set, `PF_W` clear → `PAGE_PRESENT | PAGE_USER | PAGE_EXECUTABLE` (no `PAGE_WRITABLE`)
- `PF_W` set, `PF_X` clear → `PAGE_PRESENT | PAGE_USER | PAGE_WRITABLE | PAGE_NX`
- Both set → log `ENOEXEC` and abort the load
- Neither set → `PAGE_PRESENT | PAGE_USER | PAGE_NX` (read-only data section)

### `mprotect` W^X check

In `kernel/src/syscall/mm.rs` (or wherever `sys_mprotect` is implemented), immediately after parsing the `prot` argument: `if prot.contains(PROT_WRITE) && prot.contains(PROT_EXEC) { return Err(EINVAL); }`. This guard runs before the walk of the VMA list.

### `user_space.rs` deferral comment removal

Lines 135 and 143 of `kernel/src/mm/user_space.rs` currently pass `WRITABLE | USER_ACCESSIBLE` for code segments. This phase changes those call sites to pass the segment-flag-derived protection. The comment referencing "Phase 6+ deferred" is removed.

## How This Builds on Earlier Phases

- Extends Phase 11's ELF loader to read `p_flags` per segment instead of using a uniform flag set
- Extends Phase 36's `mprotect` implementation with a W^X validation guard
- Reuses Phase 36's VMA (virtual memory area) data structures to track per-region protection flags
- The `NO_EXECUTE` page-table bit is already plumbed through the Phase 2 page-table layer; this phase is the first to use it for userspace data mappings

## Implementation Outline

1. Audit all PT_LOAD segment mappings in `kernel/src/elf/loader.rs`; add `p_flags` dispatch
2. Update `kernel/src/mm/user_space.rs` lines 135 and 143; remove deferral comment
3. Add W^X validation guard to `sys_mprotect` in the kernel syscall layer
4. Ensure all `brk`, `mmap(PROT_READ|PROT_WRITE)`, and stack setup paths apply `NO_EXECUTE`
5. Run all existing binaries under QEMU to verify no regressions
6. Write a small test binary that attempts `mprotect(PROT_WRITE | PROT_EXEC)` and verifies `EINVAL`
7. Update `docs/appendix/architecture-and-syscalls.md` with the JIT exception pattern
8. Update Phase 11 and Phase 36 design docs; remove the `// Phase 6+ deferred` comment

## Acceptance Criteria

- Every in-tree userspace binary's text segment pages are mapped without the `WRITABLE` bit; verified by reading `/proc/<pid>/maps`-equivalent kernel debug output
- An attempt to `mprotect` any page to `PROT_WRITE | PROT_EXEC` returns `EINVAL`
- Stack and heap pages are mapped with `NO_EXECUTE` set; a jump to a stack address causes a page fault with the `#PF` error code indicating an NX violation
- All existing in-tree binaries (`exit0`, `term`, `coreutils`, `init`, `sh0`, `edit`, `doom`, etc.) boot and operate correctly under the new mapping rules
- The JIT pattern (allocate `RW-`, write code, `mprotect` to `R-X`) succeeds; a test binary executes a short hand-written instruction sequence mapped this way

## Companion Task List

- [Phase 75 Task List](./tasks/75-wx-enforcement-tasks.md)

## How Real OS Implementations Differ

- OpenBSD enforces strict W^X system-wide and refuses to run binaries with W|X PT_LOAD segments entirely; m3OS currently logs a warning and returns `ENOEXEC`, which is equivalent behavior
- Linux applies W^X at the architecture level but allows `mprotect(PROT_EXEC | PROT_WRITE)` by default; personality flags (`READ_IMPLIES_EXEC`, `MMAP_PAGE_ZERO`) exist for legacy compatibility — m3OS has no such legacy burden
- gVisor and QEMU sandbox layers enforce W^X at the hypervisor page-table level in addition to the guest OS level; m3OS's single page table is the only enforcement point
- Production kernels (Linux, XNU) separate `mmap` into distinct anonymous/file paths with fine-grained flag validation; m3OS's simpler VMA model makes the enforcement addition straightforward

## Deferred Until Later

- Address-space layout randomization (ASLR) — the ELF loader still maps segments at deterministic virtual addresses; ASLR is a separate security hardening item
- Stack canaries and shadow stacks (CET) — hardware-assisted stack protection for the userspace ABI
- Executable-space protection for kernel memory (SMEP/SMAP enforcement) — already partially in place via the x86_64 CR4 bits set during boot; a full audit is deferred
- W^X validation for kernel module loading — m3OS has no loadable kernel modules, so this is moot
