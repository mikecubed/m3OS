# Phase 75 — W^X Enforcement: Task List

**Status:** Planned
**Source Ref:** phase-75
**Depends on:** Phase 11 (Process Model) ✅, Phase 36 (Memory Subsystem Expansion) ✅
**Goal:** Enforce write-XOR-execute for all userspace pages by fixing the ELF loader's uniform flag mapping, adding a W^X guard in `mprotect`, and marking all stack and heap mappings non-executable. Remove the `// Phase 6+ deferred` comment from `user_space.rs`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | ELF loader PT_LOAD flag dispatch | Phase 11 ✅ | Planned |
| B | `mprotect` W^X validation guard | Phase 36 ✅ | Planned |
| C | Stack and heap NX enforcement | Phase 36 ✅ | Planned |
| D | JIT exception documentation | A, B | Planned |
| E | Migration validation — all existing binaries | A–C | Planned |
| F | Phase 11 and Phase 36 design doc updates | E | Planned |
| G | Test binary for W^X scenarios | B, C | Planned |
| H | Documentation and Release | A–G | Planned |

---

## Track A — ELF Loader PT_LOAD Flag Dispatch

### A.1 — Read `p_flags` per segment in `map_segment`

**File:** `kernel/src/elf/loader.rs`
**Symbol:** `map_segment`
**Why it matters:** The loader currently passes a uniform `WRITABLE | USER_ACCESSIBLE` flag to every PT_LOAD segment, making code pages writable and the absence of `NO_EXECUTE` leaves data pages executable — both are wrong.

**Acceptance:**
- [ ] `map_segment` reads `segment.flags()` and branches: `PF_X` only → `R-X`; `PF_W` only → `RW-|NX`; neither → `R--|NX`; both → `ENOEXEC`
- [ ] A segment with both `PF_W | PF_X` is rejected with `ENOEXEC`; a log line identifies the offending binary and segment offset
- [ ] The flag-dispatch logic is covered by a host-side unit test in `kernel-core` that exercises all four flag combinations

### A.2 — Remove `WRITABLE` from code-segment call sites in `user_space.rs`

**File:** `kernel/src/mm/user_space.rs`
**Symbol:** `map_user_pages` (call sites at lines 135 and 143)
**Why it matters:** These two call sites are the direct cause of the audit finding at § E1; removing `WRITABLE` from them closes the vulnerability.

**Acceptance:**
- [ ] Line 135 no longer passes `WRITABLE` for executable segments
- [ ] Line 143 no longer passes `WRITABLE` for executable segments
- [ ] The comment `// Phase 6+ deferred` (or equivalent) is removed; replaced with `// W^X enforced: Phase 75`
- [ ] No other call sites in `user_space.rs` pass `WRITABLE` for text segments

---

## Track B — `mprotect` W^X Validation Guard

### B.1 — Add W^X guard to `sys_mprotect`

**File:** `kernel/src/syscall/mm.rs`
**Symbol:** `sys_mprotect`
**Why it matters:** Without this guard, a program can map a page `PROT_WRITE`, fill it with shellcode, then call `mprotect(PROT_WRITE | PROT_EXEC)` to execute it.

**Acceptance:**
- [ ] `sys_mprotect` returns `EINVAL` immediately (before any VMA walk) if `prot` contains both `PROT_WRITE` and `PROT_EXEC`
- [ ] Existing calls with `PROT_READ | PROT_WRITE` succeed unchanged
- [ ] Existing calls with `PROT_READ | PROT_EXEC` succeed unchanged (the JIT pattern's second step)
- [ ] A kernel-side unit test exercises the `PROT_WRITE | PROT_EXEC` case and asserts `EINVAL`

---

## Track C — Stack and Heap NX Enforcement

### C.1 — Initial stack mapping with `NO_EXECUTE`

**File:** `kernel/src/elf/loader.rs`
**Symbol:** `setup_user_stack`
**Why it matters:** Stack-executable mappings are the target of classic return-to-stack shellcode attacks; this closes that vector.

**Acceptance:**
- [ ] `setup_user_stack` passes `PAGE_NX` (or equivalent) when mapping the initial user stack
- [ ] The stack mapping appears in the kernel's VMA list with `PROT_READ | PROT_WRITE` and no `PROT_EXEC`
- [ ] A jump to a stack address from a test binary causes a `#PF` with NX error code (bit 4 of the error code word set)

### C.2 — `brk` and `mmap(PROT_READ|PROT_WRITE)` NX enforcement

**File:** `kernel/src/syscall/mm.rs`
**Symbol:** `sys_brk`, `sys_mmap`
**Why it matters:** Heap allocations that are executable are equivalent to a stack-execute vulnerability; the entire heap must be NX by default.

**Acceptance:**
- [ ] `sys_brk` maps all new pages with `PAGE_NX`
- [ ] `sys_mmap` with `prot = PROT_READ | PROT_WRITE` (and without `PROT_EXEC`) maps with `PAGE_NX`
- [ ] `sys_mmap` with `prot = PROT_READ | PROT_EXEC` (JIT pattern step 2) maps without `PAGE_NX`
- [ ] A heap allocation accessed as executable (via function pointer cast) causes a `#PF` NX fault in a test binary

---

## Track D — JIT Exception Documentation

### D.1 — Document the `mprotect`-toggle JIT pattern

**File:** `docs/appendix/architecture-and-syscalls.md`
**Symbol:** N/A
**Why it matters:** Phase 80 Node.js and Phase 81 Claude Code will require JIT code generation; developers need the correct pattern before they hit `EINVAL` from `mprotect`.

**Acceptance:**
- [ ] A new subsection "JIT Code Generation Pattern" is added to the architecture reference
- [ ] The pattern is documented as: (1) `mmap(NULL, size, PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)`, (2) write machine code, (3) `mprotect(ptr, size, PROT_READ | PROT_EXEC)`
- [ ] The doc explicitly states that `PROT_WRITE | PROT_EXEC` is rejected by `sys_mprotect` and is not an available alternative
- [ ] A note references Phase 80 (Node.js) as the first consumer

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
- [ ] `cargo xtask run` boots successfully with all existing binaries after the ELF loader change
- [ ] No binary produces an `ENOEXEC` rejection under the new loader (if any does, it must be fixed or explicitly noted)
- [ ] The full `cargo xtask test` suite passes with no new failures
- [ ] `doom` (Phase 47/70), `term` (Phase 57), and `edit` (Phase 21) all run correctly — these three exercise the widest range of ELF loading patterns

### E.2 — Verify code-page `WRITABLE` bit is absent at runtime

**File:** `kernel/src/syscall/debug.rs` (or equivalent VMA query path)
**Symbol:** `sys_query_vma` (or an equivalent debug query)
**Why it matters:** The acceptance criterion requires a verifiable runtime check, not just a code review.

**Acceptance:**
- [ ] A test binary or a QEMU-side inspection confirms that the text segment VMA of a loaded binary has no `WRITABLE` bit in its PTE
- [ ] The check is recorded in the PR description with a QEMU serial-output snippet showing the PTE flags

---

## Track F — Phase 11 and Phase 36 Design Doc Updates

### F.1 — Update Phase 11 design doc "Deferred Until Later"

**File:** `docs/roadmap/11-process-model.md`
**Symbol:** N/A
**Why it matters:** Phase 11's doc notes W^X as deferred; that claim must be retired now that it is enforced.

**Acceptance:**
- [ ] Phase 11 "Deferred Until Later" entry for W^X is updated to "Delivered in Phase 75"
- [ ] A note in Phase 11 "Feature Scope" or "How This Builds on Earlier Phases" references Phase 75 as the enforcement phase

### F.2 — Update Phase 36 design doc

**File:** `docs/roadmap/36-memory-subsystem-expansion.md`
**Symbol:** N/A
**Why it matters:** Phase 36 introduced `mprotect`; its doc may reference W^X as a future concern.

**Acceptance:**
- [ ] Phase 36 "Deferred Until Later" entries referencing W^X or `mprotect` validation are updated to point to Phase 75
- [ ] No entry in Phase 36 claims that W^X is "not yet enforced"

---

## Track G — Test Binaries for W^X Scenarios

### G.1 — W^X violation test binary

**File:** `userspace/tests/wx_violation/src/main.rs`
**Symbol:** `main`
**Why it matters:** Acceptance criteria require a programmatic verification that `mprotect(PROT_WRITE | PROT_EXEC)` returns `EINVAL`; a manual QEMU run is not reproducible.

**Acceptance:**
- [ ] Binary calls `mmap(PROT_READ | PROT_WRITE)` then `mprotect(ptr, sz, PROT_WRITE | PROT_EXEC)` and asserts the result is `EINVAL`
- [ ] Binary calls `mmap(PROT_READ | PROT_WRITE)` then `mprotect(ptr, sz, PROT_READ | PROT_EXEC)` (JIT pattern) and asserts success
- [ ] Binary is registered as a QEMU test under `cargo xtask test --test wx_violation`
- [ ] Test exits with `QEMU_EXIT_SUCCESS` (0x21) when all assertions pass

---

---

## Track H — Documentation and Release

### H.1 — Create the aligned legacy learning doc

**File:** `docs/75-wx-enforcement.md`
**Symbol:** N/A
**Why it matters:** A learner-friendly doc scoped to Phase 75 consolidates the W^X enforcement story — ELF loader, `mprotect` guard, heap/stack NX, and the JIT exception pattern — in one place, so readers do not need to cross-reference Phase 11, Phase 36, and Phase 80 documentation.

**Acceptance:**
- [ ] File exists at `docs/75-wx-enforcement.md`
- [ ] All required template fields populated: `**Aligned Roadmap Phase:** Phase 75`, `**Status:** Planned`, `**Source Ref:** phase-75`, `**Supersedes Legacy Doc:** new`
- [ ] Overview is learner-friendly (explains what W^X is and why it matters before describing the implementation)
- [ ] Key Files table cites real files this phase touches: `kernel/src/elf/loader.rs`, `kernel/src/mm/user_space.rs`, `kernel/src/syscall/mm.rs`, `docs/appendix/architecture-and-syscalls.md`, `userspace/tests/wx_violation/src/main.rs`
- [ ] Related Roadmap Docs links `docs/roadmap/75-wx-enforcement.md` and `docs/roadmap/tasks/75-wx-enforcement-tasks.md`

### H.2 — Bump kernel version to 0.75.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md`

**Symbol:** `version` in `kernel/Cargo.toml` `[package]`
**Why it matters:** Project convention is one minor-version bump per shipped phase; the 2026-05-08 audit found `AGENTS.md` stale and discipline in version tracking signals a complete, shippable phase.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.75.0"`
- [ ] `Cargo.lock` regenerated (run `cargo check` or `cargo xtask check` to trigger it)
- [ ] `AGENTS.md` "Kernel v0.75.0" updated
- [ ] `docs/roadmap/README.md` Phase 75 row Status updated to "Complete" at merge time
- [ ] `cargo xtask check` passes
- [ ] Git tag `v0.75.0` recommended at phase merge

---

## Documentation Notes

- Track A's `map_segment` change is the most load-bearing; it touches every binary load path and must be tested with `doom`, `term`, `init`, and `sh0` specifically (as the widest ELF variety in the tree).
- Track B's guard position matters: it must execute before any VMA walk or page-table modification so that no partial state is left on `EINVAL`.
- Track C must cover both `brk` and `mmap`; omitting one leaves a hole that attackers can exploit.
- The `// Phase 6+ deferred` comment in `kernel/src/mm/user_space.rs` is the specific textual marker from the audit (§ E1); its removal is an explicit deliverable of Track A.2.
- Track G's JIT-pattern test (second bullet in G.1) is the positive-case proof that the enforcement does not break future JIT use; it is as important as the negative case.
- Track H.1 learning doc should be authored after Track G so it can cite the actual test binary paths and serial-output snippets as concrete examples.
