# Phase 84 - Spectre / KPTI / Retpoline / IBRS Mitigations

**Status:** Planned (post-1.0)
**Source Ref:** phase-84
**Depends on:** Phase 75 (W^X Enforcement) ✅, Phase 77 (Pre-1.0 Correctness — SMEP + SMAP baseline), Phase 83 (Release 1.0 Gate)
**Builds on:** Extends the cheap Phase 77 SMEP + SMAP mitigations with the expensive set: KPTI (Kernel Page Table Isolation), retpoline / IBRS for Spectre-v2, and the surrounding microarchitectural defenses that real OSes turned on after the 2018 disclosures
**Primary Components:** `kernel/src/mm/page_table.rs` (separate user/kernel page tables), `kernel/src/arch/x86_64/syscall/mod.rs` (entry/exit asm — PTI trampoline), `xtask` Rust flags (retpoline codegen), `kernel/src/arch/x86_64/cpu.rs` (MSR_IA32_SPEC_CTRL / IBRS toggling)

## Milestone Goal

m3OS implements the post-Meltdown / post-Spectre-v2 mitigations that mature OSes shipped between 2018 and 2020. After this phase, kernel/user address spaces are isolated (KPTI), indirect branches in kernel code are retpoline-protected, and the SPEC_CTRL MSR is toggled on kernel entry / exit. This is explicitly a post-1.0 phase because the work is large (~2000 LOC) and the 1.0 cohort (Phase 77's SMEP + SMAP) already captures the cheap class of mitigations.

## Why This Phase Exists

Phase 74a §1 row 9 grades Spectre/SMEP/SMAP/KPTI as HIGH ("silent on QEMU TCG, exploitable on metal"). Phase 77 lands SMEP + SMAP because they are CR4 bit flips with trivial code impact. KPTI, retpoline, and IBRS are expensive — they touch the kernel/user transition asm and the entire indirect-branch codegen story — so they get their own phase after the 1.0 gate.

The post-1.0 placement is honest: m3OS at 1.0 is a learning microkernel, not a hardened production OS. Users running on Spectre-vulnerable silicon should know what they are running.

## Learning Goals

- Understand how KPTI splits a process's page tables into a user-mode subset (full process map but no kernel) and a kernel-mode subset (full map plus a tiny trampoline visible to user mode for syscall entry / exit)
- See why Meltdown breaks the assumption that ring-0-only pages are inaccessible to ring-3 code via speculation
- Learn how retpoline replaces every indirect branch (`call *rax`, `jmp *rax`) with a sequence that traps the CPU's speculative execution into an infinite predicted-but-mispredicted loop
- Understand IBRS / IBPB / STIBP as the alternative microarchitectural mitigation when retpoline is not sufficient (Skylake-and-later)
- See the performance tradeoffs: KPTI costs ~5–30% of syscall throughput depending on workload; retpoline costs ~5% of indirect-branch-heavy code

## Feature Scope

### Track A — KPTI (Kernel Page Table Isolation)

- **A.1** — Per-process page-table pair: the existing per-process PML4 becomes the "kernel PML4," and a new "user PML4" carries only the user-visible mappings plus the syscall-entry trampoline.
- **A.2** — Syscall entry / exit asm trampoline: switch CR3 to kernel-PML4 on entry (after saving user state on the trampoline stack), switch back on exit. ~200 LOC of asm.
- **A.3** — IRQ entry / exit symmetry: hardware interrupts must do the same CR3 switch.
- **A.4** — PCID (Process Context Identifier) use to avoid full TLB flush on every CR3 switch (~100 LOC, big perf win on Westmere+).

### Track B — Retpoline

- **B.1** — Rust codegen flag: `-C target-feature=+retpoline-indirect-branches,+retpoline-indirect-calls` (where supported by `rustc`). Audit emitted code for residual indirect branches.
- **B.2** — Asm hand-written retpoline thunks for any remaining indirect branch sites (`__x86_indirect_thunk_rax` etc.). Linker scripts force the existing `call *reg` to call into the thunk instead.
- **B.3** — Verify: `objdump -d kernel.elf | grep -E 'call[ \t]+\*'` returns zero hits (all indirects routed through thunks).

### Track C — SPEC_CTRL MSR toggling (IBRS / IBPB / STIBP)

- **C.1** — Detect IBRS via `CPUID.07h.EDX[26]`. On kernel entry, write `MSR_IA32_SPEC_CTRL` with the IBRS bit set; clear on exit. ~100 LOC.
- **C.2** — Detect IBPB via `CPUID.07h.EDX[27]`. Issue `MSR_IA32_PRED_CMD` on context switches between security domains.
- **C.3** — STIBP for SMT siblings — opt-in per-process via `prctl(PR_SET_SPECULATION_CTRL, ...)`. Default off to avoid the perf cost.

### Track D — Configuration surface

- **D.1** — `/proc/cmdline`-equivalent boot flags: `mitigations=off|auto|full` controlling whether the above turn on at boot.
- **D.2** — `m3ctl mitigations status` — read-only display of which mitigations are active on the current CPU.

## Important Components and How They Work

### KPTI page-table pair

In single-PML4 design, every user-mode process page table maps both user pages (`USER_ACCESSIBLE`) and kernel pages (kernel-only). Meltdown demonstrated that speculation across a privileged-load instruction leaks the value into a cache-observable side channel — so the CPU can be tricked into reading kernel memory while user code is the architecturally permitted reader. KPTI's answer: the user-mode CR3 points to a PML4 that does not contain the kernel mappings at all. The CPU literally cannot speculate into them. The trampoline page (containing exactly the few bytes of asm needed to switch CR3 and jump to the kernel handler) is the one kernel page mapped in both PML4s.

### Retpoline

`call *%rax` lets the CPU speculate based on the Branch Target Buffer's prediction for that indirect site. An attacker who controls another process can train the BTB to predict an attacker-chosen kernel address, causing the kernel to speculatively execute attacker-chosen code. Retpoline replaces the indirect call with a sequence that pushes the target onto the stack, then `ret`s. The CPU's Return Stack Buffer has different (and harder-to-poison) prediction behavior, and the retpoline thunk sets up the RSB to predict an infinite loop, neutering speculation.

### Cost / benefit tradeoff

KPTI is the headline mitigation. On Skylake-and-later silicon Intel introduced PCID + INVPCID which dropped KPTI's overhead from ~30% to ~5% on syscall-heavy workloads. Retpoline is cheap on most code (~1–2% overhead) but expensive on hot indirect-branch sites (~10–15%). IBRS toggling per kernel entry is moderately expensive; many production OSes use IBRS only on the affected silicon families (Skylake-derived) and retpoline elsewhere.

## How This Builds on Earlier Phases

- Extends the Phase 75 W^X model with KPTI — together they cover the bulk of the "code injection via memory-corruption + speculation" attack surface.
- Extends Phase 77's SMEP + SMAP with the more expensive class of mitigations.
- Reuses the Phase 11 process model — each process already owns its PML4; this phase just splits each PML4 into a pair.

## Implementation Outline

1. Implement KPTI first — it's the headline benefit and the architectural change.
2. Add PCID immediately after to recover the syscall-throughput cost.
3. Add retpoline codegen + thunks.
4. Add SPEC_CTRL MSR toggling.
5. Implement the `mitigations=` boot flag + `m3ctl mitigations status`.
6. Performance-regression test: the smoke suite should be no more than 30% slower with `mitigations=full` versus `mitigations=off` on Skylake-class silicon.
7. Bump kernel to `0.84.0`.

## Acceptance Criteria

- With `mitigations=full`, a Meltdown PoC (the public 2018 reference exploit, ported to m3OS) fails to read kernel memory.
- With `mitigations=full`, all existing smoke + regression gates pass; syscall throughput is at most 30% slower than `mitigations=off`.
- `m3ctl mitigations status` reports the active set on the booted CPU.
- Documentation under `docs/security/spectre-mitigations.md` describes which silicon families are protected by which mitigation, and which residual risks remain.
- Kernel bumped to `0.84.0`.

## Companion Task List

- [Phase 84 Task List](./tasks/84-spectre-mitigations-tasks.md) — to be authored when implementation planning begins.

## How Real OS Implementations Differ

- Linux's mitigations matrix has ~30 distinct vulnerability identifiers (Spectre-v1, v2, RSB-underflow, MDS, TAA, ITLB-multihit, SRBDS, SRSO, Inception, ...) — m3OS at Phase 84 ships the four headline 2018-era ones.
- Real OSes have CPU-specific tunings choosing IBRS vs. retpoline based on the silicon's known characteristics (Skylake gets IBRS; pre-Skylake gets retpoline; AMD has its own table). m3OS uses the simple-and-conservative "retpoline always, IBRS when available" rule.
- Linux's `mitigations=` flag supports per-vuln granularity (e.g., `nospectre_v2`, `nopti`, `nopti=force`); m3OS at this phase exposes only the three coarse levels.
- L1TF, MDS, TAA — not addressed at this phase.

## Deferred Until Later

- L1TF / MDS / TAA / Zombieload / RIDL / Fallout
- SRSO (Inception)
- Branch History Injection
- DownFall / GDS
- Per-vulnerability mitigation toggles
- Fine-grained STIBP / SMT scheduling integration
- Speculative-load hardening compiler pass (LLVM SLH)
