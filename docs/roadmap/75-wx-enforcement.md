# Phase 75 - W^X Enforcement

**Status:** Complete
**Source Ref:** phase-75
**Depends on:** Phase 11 (Process Model) ✅, Phase 36 (Memory Subsystem Expansion) ✅
**Builds on:** Hardens the ELF loader and `mprotect` path introduced in Phase 11 and Phase 36 to enforce write-XOR-execute for all userspace code and data pages
**Primary Components:** `kernel/src/mm/elf.rs`, `kernel/src/mm/user_space.rs`, `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_mprotect`, `sys_linux_brk`, `sys_linux_mmap`)

## Milestone Goal

Every userspace process launched by m3OS has its code pages mapped read-execute and its data pages mapped read-write, with no page simultaneously writable and executable. An explicit `mprotect(PROT_WRITE | PROT_EXEC)` call from userspace returns `EINVAL`. Stack and heap pages carry the `NO_EXECUTE` bit at all times. All existing in-tree binaries continue to function correctly under these constraints.

## Why This Phase Exists

Two W^X gaps exist today. (1) The modern ELF loader in `kernel/src/mm/elf.rs` already derives per-segment PTE flags via `segment_flags()` (`elf.rs:245`) — PF_W adds WRITABLE, absence of PF_X adds NO_EXECUTE — but it does *not* reject a malformed `PF_W | PF_X` segment. A linker bug or hostile binary could therefore still create a W+X code segment. (2) The legacy `setup_user_memory` path in `kernel/src/mm/user_space.rs` (lines 214–231) maps user code pages with `WRITABLE | USER_ACCESSIBLE` and no `NO_EXECUTE` (the `// W^X enforcement is deferred to Phase 6+` comment marks the site). That function currently has *zero callers* (every live binary load goes through `mm::elf::load_elf_into`), so the runtime exposure is nil — but the dead-code shape leaves a trap for future contributors and matches the audit finding (§ E1). This is a well-known exploit enabler in principle: a vulnerability that achieves an arbitrary write into a code page can immediately redirect execution. The audit identified this as a pre-1.0 security hardening item.

W^X is a foundational memory safety guarantee present in every modern OS since OpenBSD 3.3 (2003). It costs nothing at runtime (no extra page faults on normal code execution) and eliminates an entire class of memory-corruption exploit primitives. It must be in place before Phase 80 (Node.js) and Phase 81 (Claude Code) introduce JIT compilation, which requires a documented exception path.

SRP: the ELF loader's sole concern is splitting PT_LOAD segments by `p_flags`; a single `mprotect` validator then enforces W^X uniformly across all later `mmap`/`mprotect` calls — no scattered permission checks elsewhere in the kernel. TDD: `is_wx_violation(prot)` is a pure predicate that host-tests trivially in `kernel-core`; the kernel-side enforcement is validated in QEMU with a two-case test binary — the negative case (`mprotect(PROT_WRITE | PROT_EXEC)` → `EINVAL`) and the positive case (the JIT `RW-` → `R-X` toggle) together prove that enforcement is both present and non-breaking.

## Learning Goals

- Understand how x86_64 page-table `NX` bits enforce non-executable mappings at the hardware level
- Learn how an ELF loader distinguishes text (PT_LOAD, execute) from data (PT_LOAD, no-execute) segments
- See why W^X requires the ELF loader, `mprotect`, and the initial stack/heap setup to all agree
- Understand the JIT exception pattern: `mprotect` to `RW-` to write code, then `mprotect` to `R-X` to execute it

## Feature Scope

### ELF loader text/data segment separation

The modern ELF loader in `kernel/src/mm/elf.rs` already derives per-segment PTE flags from `p_flags` via `segment_flags()` (`elf.rs:245`): `PF_W` adds `WRITABLE`, absence of `PF_X` adds `NO_EXECUTE`. What is missing is the malformed-segment guard: a PT_LOAD with both `PF_W` and `PF_X` is currently mapped as a W+X page. This phase adds the rejection branch in `map_load_segment` (`elf.rs:270`): if `p_flags & (PF_W | PF_X) == (PF_W | PF_X)`, the loader logs a warning identifying the offending binary and segment offset and aborts the load with `ElfError::MappingFailed("PT_LOAD with PF_W|PF_X — W^X violation")`, which surfaces to `execve` as `-ENOEXEC`.

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

In `kernel/src/mm/elf.rs`, the `map_load_segment` function (line 270) already calls `segment_flags()` (line 245) to derive PTE flags from `p_flags`. That helper produces:

- `PF_X` set, `PF_W` clear → `PRESENT | USER_ACCESSIBLE` (no `WRITABLE`, no `NO_EXECUTE`) — the R-X mapping
- `PF_W` set, `PF_X` clear → `PRESENT | USER_ACCESSIBLE | WRITABLE | NO_EXECUTE` — the RW- mapping
- Neither set → `PRESENT | USER_ACCESSIBLE | NO_EXECUTE` — read-only data
- Both set → still produces a W+X mapping (**this phase fixes that**)

This phase adds a guard at the top of `map_load_segment` (before flag derivation): if `phdr.p_flags & (PF_W | PF_X) == (PF_W | PF_X)`, the loader emits a warning identifying the offending binary and segment offset, then returns `ElfError::MappingFailed("PT_LOAD with PF_W|PF_X — W^X violation")`. `execve` surfaces this as `-ENOEXEC`.

### `mprotect` W^X check

In `kernel/src/arch/x86_64/syscall/mod.rs`, `sys_mprotect` (line 9762) currently parses `prot` into `new_flags` without checking the W+X combination. This phase adds an early guard immediately after the `prot` mask: `if prot & PROT_WRITE != 0 && prot & PROT_EXEC != 0 { return NEG_EINVAL; }`. The guard runs before any address validation, VMA walk, or page-table modification, so no partial state is left on rejection.

### `user_space.rs` deferral comment removal

`kernel/src/mm/user_space.rs` contains the legacy `setup_user_memory` (line 217) which maps code pages with `PRESENT | WRITABLE | USER_ACCESSIBLE` (line 225) and carries `// W^X enforcement is deferred to Phase 6+` comments at lines 214 and 222. The function has **zero live callers** — every binary load now flows through `mm::elf::load_elf_into`, so this is dead code rather than an active exposure, but it remains a hazard that future contributors could resurrect. This phase deletes `setup_user_memory` outright (along with its stale comments), or, if any near-term bring-up path needs an `setup_user_memory`-shaped helper, rewrites it to produce the same per-segment W^X-correct flags that `elf::segment_flags()` produces.

## How This Builds on Earlier Phases

- Extends Phase 11's ELF loader to read `p_flags` per segment instead of using a uniform flag set
- Extends Phase 36's `mprotect` implementation with a W^X validation guard
- Reuses Phase 36's VMA (virtual memory area) data structures to track per-region protection flags
- The `NO_EXECUTE` page-table bit is already plumbed through the Phase 2 page-table layer; this phase is the first to use it for userspace data mappings

## Implementation Outline

1. Add a `PF_W | PF_X` rejection branch to `map_load_segment` in `kernel/src/mm/elf.rs` (returns `ElfError::MappingFailed`, surfaced to `execve` as `-ENOEXEC`)
2. Delete the dead `setup_user_memory` helper in `kernel/src/mm/user_space.rs` (lines 208–242), removing the `// W^X enforcement is deferred to Phase 6+` comments at lines 214 and 222
3. Add a W^X validation guard to `sys_mprotect` in `kernel/src/arch/x86_64/syscall/mod.rs` (line 9762) — early-return `NEG_EINVAL` before address validation
4. Audit `sys_linux_brk` and `sys_linux_mmap` in `kernel/src/arch/x86_64/syscall/mod.rs` to confirm all `PROT_WRITE`-only mappings apply `NO_EXECUTE`; `map_user_stack` (`kernel/src/mm/elf.rs:382`) already does so and only needs verification
5. Run all existing binaries under QEMU (`cargo xtask run`, `cargo xtask test`, `cargo xtask tui-app-smoke`) to verify no regressions
6. Write a small test binary that attempts `mprotect(PROT_WRITE | PROT_EXEC)` and verifies `EINVAL`, and that exercises the positive JIT-pattern path
7. Update `docs/appendix/architecture-and-syscalls.md` with the JIT exception pattern
8. Update Phase 11 and Phase 36 design docs to record W^X as a baseline guarantee as of Phase 75

## Acceptance Criteria

- Every in-tree userspace binary's text segment pages are mapped without the `WRITABLE` bit; verified by a kernel-side log emitted from `map_load_segment` (or by a one-shot `dump_pte_walk_diagnostics` call against the loaded text segment), captured on the QEMU serial console
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
