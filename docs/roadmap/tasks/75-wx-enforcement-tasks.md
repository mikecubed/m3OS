# Phase 75 — W^X Enforcement: Task List

**Status:** Complete
**Source Ref:** phase-75
**Depends on:** Phase 11 (Process Model) ✅, Phase 36 (Memory Subsystem Expansion) ✅
**Goal:** Enforce write-XOR-execute for all userspace pages by fixing the ELF loader's uniform flag mapping, adding a W^X guard in `mprotect`, and marking all stack and heap mappings non-executable. Remove the `// Phase 6+ deferred` comment from `user_space.rs`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | ELF loader PT_LOAD flag dispatch | Phase 11 ✅ | ✅ Complete |
| B | `mprotect` W^X validation guard | Phase 36 ✅ | ✅ Complete |
| C | Stack and heap NX enforcement | Phase 36 ✅ | ✅ Complete |
| D | JIT exception documentation | A, B | ✅ Complete |
| E | Migration validation — all existing binaries | A–C | ✅ Complete |
| F | Phase 11 and Phase 36 design doc updates | E | ✅ Complete |
| G | Test binary for W^X scenarios | B, C | ✅ Complete |
| H | Documentation and Release | A–G | ✅ Complete |

---

## Track A — ELF Loader PT_LOAD Flag Dispatch

### A.1 — Reject `PF_W | PF_X` PT_LOAD segments in `map_load_segment`

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `map_load_segment` (line 270), `segment_flags` (line 245)
**Why it matters:** `segment_flags()` already produces W^X-correct PTE flags for the three benign PT_LOAD shapes (`PF_X` only → R-X; `PF_W` only → RW-/NX; neither → R--/NX). What is missing is the malformed-segment guard: a PT_LOAD with both `PF_W` and `PF_X` is still mapped as a W+X page. A hostile or mis-linked binary can therefore introduce a writable code segment.

**Acceptance:**
- [x] A guard at the top of `map_load_segment` rejects `phdr.p_flags & (PF_W | PF_X) == (PF_W | PF_X)` before any frame allocation or page-table mutation; returns `ElfError::MappingFailed("PT_LOAD with PF_W|PF_X — W^X violation")`
- [x] The rejection emits a `log::warn!` line carrying the binary identity (caller-provided) and the segment's `p_offset` and `p_vaddr`
- [x] `execve` surfaces the rejection as `-ENOEXEC` to userspace (covered by the test binary in G.1 if the test fixture exercises it; otherwise by a kernel-side unit-style test)
- [x] `segment_flags()` is left unchanged — the existing PF_W/PF_X mapping logic is already W^X-correct for the three benign cases

### A.2 — Retire the legacy `setup_user_memory` helper in `user_space.rs`

**File:** `kernel/src/mm/user_space.rs`
**Symbol:** `setup_user_memory` (lines 208–242), `map_user_pages` (line 43, called at line 233)
**Why it matters:** `setup_user_memory` maps user code pages at `USER_CODE_BASE` with `PRESENT | WRITABLE | USER_ACCESSIBLE` (line 225) — the exact W+X shape the audit (§ E1) flagged. Two `// W^X enforcement is deferred to Phase 6+` comments live at lines 214 and 222. The function has **zero live callers** today — every binary load flows through `mm::elf::load_elf_into` — but the dead-code shape is a latent hazard.

**Acceptance:**
- [x] `setup_user_memory` and its `WRITABLE`-bearing call site at line 225 are removed, OR rewritten so the code mapping never carries `WRITABLE` and an explanatory comment cites Phase 75 (decision is left to the implementer)
- [x] Both `// W^X enforcement is deferred to Phase 6+` comments (lines 214 and 222) are removed
- [x] `cargo xtask check` and `cargo xtask test` still pass — confirming no live caller depends on the legacy helper
- [x] `grep -rn 'WRITABLE.*USER_ACCESSIBLE' kernel/src/mm/user_space.rs` returns no executable-mapping site

---

## Track B — `mprotect` W^X Validation Guard

### B.1 — Add W^X guard to `sys_mprotect`

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_mprotect` (line 9762)
**Why it matters:** Without this guard, a program can map a page `PROT_WRITE`, fill it with shellcode, then call `mprotect(PROT_WRITE | PROT_EXEC)` to execute it.

**Acceptance:**
- [x] `sys_mprotect` returns `NEG_EINVAL` immediately after the `prot &= 0x7` mask (line 9764) — before the page-alignment check, before any VMA walk, before any PTE modification — when `prot & PROT_WRITE != 0 && prot & PROT_EXEC != 0`
- [x] Existing calls with `PROT_READ | PROT_WRITE` succeed unchanged
- [x] Existing calls with `PROT_READ | PROT_EXEC` succeed unchanged (the JIT pattern's second step)
- [x] The G.1 userspace test binary exercises the `PROT_WRITE | PROT_EXEC` case and asserts `EINVAL`

---

## Track C — Stack and Heap NX Enforcement

### C.1 — Verify initial stack mapping is `NO_EXECUTE`

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `map_user_stack` (line 382)
**Why it matters:** Stack-executable mappings are the target of classic return-to-stack shellcode attacks. The modern ELF loader's `map_user_stack` already builds its flag set with `PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE` (lines 384–388), so this track is a verification step rather than a code change — but the regression test still needs to be authored so the property is locked in.

**Acceptance:**
- [x] Static audit: `map_user_stack` constructs `flags` containing `PageTableFlags::NO_EXECUTE` and passes that to every `map_to` call in the function body
- [x] Runtime verification: the per-PT_LOAD `log::info!` trace emitted by `map_load_segment` (Track E.2) is asserted indirectly by the W^X smoke gate — every loaded binary's segment flags are visible in serial output, and a `WRITABLE | NO_EXECUTE`-clear (i.e. W+X) segment would be rejected by the Track A.1 guard before mapping. A dedicated jump-to-stack `#PF` regression that asserts the page-fault handler's serial output against a known NX-fault string is **deferred** — captured under "Deferred Until Later" in the Phase 75 design doc.
- [x] If any non-ELF stack-setup path exists (e.g. legacy `setup_user_memory`), it is removed or made W^X-correct under A.2

### C.2 — `brk` and `mmap(PROT_READ|PROT_WRITE)` NX enforcement

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_linux_brk` (dispatch at line 1773), `sys_linux_mmap` (dispatch at line 1770)
**Why it matters:** Heap allocations that are executable are equivalent to a stack-execute vulnerability; the entire heap must be NX by default.

**Acceptance:**
- [x] `sys_linux_brk` maps all new pages with `PageTableFlags::NO_EXECUTE` set
- [x] `sys_linux_mmap` with `prot = PROT_READ | PROT_WRITE` (and without `PROT_EXEC`) maps with `NO_EXECUTE` set
- [x] `sys_linux_mmap` with `prot = PROT_READ | PROT_EXEC` (JIT pattern step 2) maps *without* `NO_EXECUTE`
- [x] Static audit verifies that `sys_linux_brk`, `sys_linux_mmap` (file-backed), and the anonymous demand-fault path (`demand_map_user_page_locked` in `kernel/src/arch/x86_64/interrupts.rs`) all apply `NO_EXECUTE` when `PROT_EXEC` is clear. A dedicated runtime regression that casts a `brk`-allocated heap byte to a function pointer and asserts the resulting `#PF` carries the NX error-code bit is **deferred** — captured under "Deferred Until Later" in the Phase 75 design doc. The Phase 75 W^X guard in `sys_mprotect` (Track B.1) is the load-bearing runtime check that `wx-violation` exercises directly.
- [x] If `sys_linux_brk` / `sys_linux_mmap` already apply `NO_EXECUTE` for the non-exec cases (static audit during implementation), the task is a verification step + the regression test in G.1; otherwise the patch adds the missing flag

---

## Track D — JIT Exception Documentation

### D.1 — Document the `mprotect`-toggle JIT pattern

**File:** `docs/appendix/architecture-and-syscalls.md`
**Symbol:** N/A
**Why it matters:** Phase 87 Node.js and Phase 88 Claude Code will require JIT code generation; developers need the correct pattern before they hit `EINVAL` from `mprotect`.

**Acceptance:**
- [x] A new subsection "JIT Code Generation Pattern" is added to the architecture reference
- [x] The pattern is documented as: (1) `mmap(NULL, size, PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)`, (2) write machine code, (3) `mprotect(ptr, size, PROT_READ | PROT_EXEC)`
- [x] The doc explicitly states that `PROT_WRITE | PROT_EXEC` is rejected by `sys_mprotect` and is not an available alternative
- [x] A note references Phase 87 (Node.js) as the first consumer

---

## Track E — Migration Validation

### E.1 — Run all existing in-tree binaries under new W^X rules

**Files:**
- `userspace/init/src/main.rs`
- `userspace/term/src/main.rs`
- (all binaries registered in `xtask/src/main.rs` `bins` array)

**Symbol:** N/A (smoke-test run)
**Why it matters:** If any existing binary has a PT_LOAD segment with `PF_W | PF_X` set (e.g., from a mis-linked binary or a self-modifying code pattern), it will be rejected by the new loader. Finding this before the PR merges prevents a boot regression.

**Acceptance:**
- [x] `cargo xtask run` boots successfully with all existing binaries after the ELF loader change
- [x] No binary produces an `ENOEXEC` rejection under the new loader (if any does, it must be fixed or explicitly noted)
- [x] The full `cargo xtask test` suite passes with no new failures
- [x] `doom` (Phase 47/70), `term` (Phase 57), and `edit` (Phase 21) all run correctly — these three exercise the widest range of ELF loading patterns

### E.2 — Verify code-page `WRITABLE` bit is absent at runtime

**File:** `kernel/src/mm/elf.rs` (log emission site) **and/or** `kernel/src/arch/x86_64/interrupts.rs` (`dump_pte_walk_diagnostics`, line 412)
**Symbol:** `map_load_segment` (post-mapping log emission) **or** `dump_pte_walk_diagnostics` invoked from a one-shot debug path
**Why it matters:** The acceptance criterion requires a verifiable runtime check, not just a code review. m3OS has no `/proc/<pid>/maps`, so the verification surface is the existing kernel-side PTE inspection path.

**Acceptance:**
- [x] After a successful PT_LOAD mapping, `map_load_segment` emits a `log::info!` line covering each segment as `elf: mapped pid=<N> p_vaddr=<va> p_flags=<rwx> pte_flags=<…>`, with `pte_flags` showing the actual `PageTableFlags` bits applied
- [x] The serial-output snippet for at least one loaded binary (`init` or `sh0`) is captured in the PR description, showing that text-segment PTEs have neither `WRITABLE` nor (for the no-NX hardware-level executable bit) `NO_EXECUTE`
- [x] Alternative path is acceptable: a one-shot `dump_pte_walk_diagnostics` call from a debug syscall — if added, document the new syscall number and stub in `docs/appendix/architecture-and-syscalls.md`

---

## Track F — Phase 11 and Phase 36 Design Doc Updates

### F.1 — Update Phase 11 design doc "Deferred Until Later"

**File:** `docs/roadmap/11-process-model.md`
**Symbol:** N/A
**Why it matters:** Phase 11's doc notes W^X as deferred; that claim must be retired now that it is enforced.

**Acceptance:**
- [x] Phase 11 "Deferred Until Later" entry for W^X is updated to "Delivered in Phase 75"
- [x] A note in Phase 11 "Feature Scope" or "How This Builds on Earlier Phases" references Phase 75 as the enforcement phase

### F.2 — Update Phase 36 design doc

**File:** `docs/roadmap/36-memory-subsystem-expansion.md`
**Symbol:** N/A
**Why it matters:** Phase 36 introduced `mprotect`; its doc may reference W^X as a future concern.

**Acceptance:**
- [x] Phase 36 "Deferred Until Later" entries referencing W^X or `mprotect` validation are updated to point to Phase 75
- [x] No entry in Phase 36 claims that W^X is "not yet enforced"

---

## Track G — Test Binaries for W^X Scenarios

### G.1 — W^X violation test binary

**File:** `userspace/wx-violation/src/main.rs`
**Symbol:** `_start` (no_std `#![no_main]` userspace binary)
**Why it matters:** Acceptance criteria require a programmatic verification that `mprotect(PROT_WRITE | PROT_EXEC)` returns `EINVAL`; a manual QEMU run is not reproducible.

**Acceptance:**
- [x] Binary calls `mmap(PROT_READ | PROT_WRITE)` then `mprotect(ptr, sz, PROT_WRITE | PROT_EXEC)` and asserts the result is `EINVAL`
- [x] Binary calls `mmap(PROT_READ | PROT_WRITE)` then `mprotect(ptr, sz, PROT_READ | PROT_EXEC)` (JIT pattern) and asserts success
- [x] Binary is registered as a QEMU smoke-test under `cargo xtask smoke-test` via the `smoke-runner` `wx-violation` stage (Phase 75 in-tree convention; the binary lives at `/bin/wx-violation` on the ramdisk and prints `SMOKE:wx-violation:PASS` on success)
- [x] On success the binary prints `WX_VIOLATION:smoke:ok` and exits 0; on assertion failure it prints `WX_VIOLATION:fail <reason>` and exits 2 (panic path uses 101 as a distinct sentinel so a panic vs. a normal assertion failure is distinguishable in serial output)

---

---

## Track H — Documentation and Release

### H.1 — Create the aligned legacy learning doc

**File:** `docs/75-wx-enforcement.md`
**Symbol:** N/A
**Why it matters:** A learner-friendly doc scoped to Phase 75 consolidates the W^X enforcement story — ELF loader, `mprotect` guard, heap/stack NX, and the JIT exception pattern — in one place, so readers do not need to cross-reference Phase 11, Phase 36, and Phase 87 documentation.

**Acceptance:**
- [x] File exists at `docs/75-wx-enforcement.md`
- [x] All required template fields populated: `**Aligned Roadmap Phase:** Phase 75`, `**Status:** Complete`, `**Source Ref:** phase-75`, `**Supersedes Legacy Doc:** new`
- [x] Overview is learner-friendly (explains what W^X is and why it matters before describing the implementation)
- [x] Key Files table cites real files this phase touches: `kernel/src/mm/elf.rs`, `kernel/src/mm/user_space.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`, `docs/appendix/architecture-and-syscalls.md`, `userspace/wx-violation/src/main.rs`
- [x] Related Roadmap Docs links `docs/roadmap/75-wx-enforcement.md` and `docs/roadmap/tasks/75-wx-enforcement-tasks.md`

### H.2 — Bump kernel version to 0.75.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-version bump per shipped phase; the 2026-05-08 audit found `AGENTS.md` stale and discipline in version tracking signals a complete, shippable phase.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version = "0.75.0"`
- [x] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger it)
- [x] `AGENTS.md` "Kernel v0.75.0" updated
- [x] `docs/roadmap/README.md` Phase 75 row Status updated to "Complete" at merge time
- [x] `cargo xtask check` passes
- [x] Git tag `v0.75.0` recommended at phase merge

---

## Documentation Notes

- Track A's `map_load_segment` change is the most load-bearing in principle, but in practice `segment_flags()` already produces W^X-correct PTE bits for benign segments; the only ELF-loader change is the `PF_W | PF_X` rejection branch. Still, the change touches every binary load path, so run `cargo xtask run`, `cargo xtask test`, and `cargo xtask tui-app-smoke` to exercise `doom`, `term`, `init`, `sh0`, `htop`, `less`, and `tmux` (the widest ELF variety in the tree).
- Track B's guard position matters: it must execute before any address validation, VMA walk, or page-table modification so that no partial state is left on `EINVAL`.
- Track C must cover both `sys_linux_brk` and `sys_linux_mmap`; omitting one leaves a hole that attackers can exploit. Note that `map_user_stack` in `kernel/src/mm/elf.rs` already applies `NO_EXECUTE`, so C.1 is primarily a regression-locking test.
- The literal comments `// W^X enforcement is deferred to Phase 6+` at `kernel/src/mm/user_space.rs:214` and `:222` are the specific textual markers from the audit (§ E1); their removal — together with the dead `setup_user_memory` helper that contains them — is an explicit deliverable of Track A.2.
- Track G's JIT-pattern test (second bullet in G.1) is the positive-case proof that the enforcement does not break future JIT use; it is as important as the negative case.
- Track H.1 learning doc should be authored after Track G so it can cite the actual test binary paths and serial-output snippets as concrete examples.
