# Phase 84 - Spectre / KPTI / Retpoline / IBRS Mitigations

**Status:** Planned (post-1.0)
**Source Ref:** phase-84
**Depends on:** Phase 75 (W^X Enforcement) ✅, Phase 77 (Pre-1.0 Correctness — SMEP + SMAP baseline) ✅, Phase 83 (Release 1.0 Gate) ✅
**Builds on:** Extends the cheap Phase 77 SMEP + SMAP mitigations with the expensive set: KPTI (Kernel Page Table Isolation), retpoline / IBRS for Spectre-v2, and the surrounding microarchitectural defenses that real OSes turned on after the 2018 disclosures
**Primary Components:** `kernel/src/mm/mod.rs` (`new_process_page_table` — split the per-process PML4 into a kernel/user pair) + `kernel/src/mm/paging.rs`, `kernel/src/arch/x86_64/syscall/mod.rs` (`syscall_entry` entry/exit asm — PTI CR3 trampoline) + `kernel/src/arch/x86_64/interrupts.rs` (IRQ/IST symmetry), `xtask/src/main.rs` kernel build flags (`-Zretpoline` on the existing `-Zbuild-std`), `kernel/src/arch/x86_64/cpuid.rs` (CPUID feature detect + `IA32_SPEC_CTRL`/IBRS — mirrors the Phase 77 `probe_smep_smap`/`enable_smep_smap` pattern), with host-tested CPUID/MSR/`mitigations=` decode in `kernel-core/src/spectre.rs`

## Milestone Goal

m3OS implements the post-Meltdown / post-Spectre-v2 mitigations that mature OSes shipped between 2018 and 2020. After this phase, kernel/user address spaces are isolated (KPTI), indirect branches in kernel code are retpoline-protected, and the `IA32_SPEC_CTRL` MSR family (IBRS/IBPB/STIBP) is applied per the silicon's capability (legacy IBRS toggled on kernel entry/exit, Enhanced IBRS set once at boot), all behind a `mitigations=off|auto|full` boot policy. This is explicitly a post-1.0 phase because the work is large (~2000 LOC) and the 1.0 cohort (Phase 77's SMEP + SMAP) already captures the cheap class of mitigations.

## Why This Phase Exists

The Phase 74a pre-1.0 audit (row 9, "No Spectre/SMEP/SMAP/KPTI mitigations on real silicon") grades this **HIGH for SMEP+SMAP; deferrable for KPTI** — silent on QEMU TCG, exploitable on metal. Phase 77 lands SMEP + SMAP because they are CR4 bit flips with trivial code impact. KPTI, retpoline, and IBRS are expensive — they touch the kernel/user transition asm and the entire indirect-branch codegen story — so they get their own phase after the 1.0 gate.

The post-1.0 placement is honest: m3OS at 1.0 is a learning microkernel, not a hardened production OS. Users running on Spectre-vulnerable silicon should know what they are running.

## Learning Goals

- Understand how KPTI splits a process's page tables into a user-mode subset (full process map but no kernel) and a kernel-mode subset (full map plus a **minimal entry set** — the syscall/IRQ trampoline text, the IDT, the GDT/TSS, and a per-CPU entry stack — visible to user mode for entry / exit), and why that set is a few pages, not one
- See why Meltdown breaks the assumption that ring-0-only pages are inaccessible to ring-3 code via speculation — and that KPTI defends **Meltdown only**, not Spectre, so immune silicon (`IA32_ARCH_CAPABILITIES.RDCL_NO`) can skip it
- Learn how retpoline replaces every indirect branch (`call *rax`, `jmp *rax`) with a sequence that traps the CPU's speculative execution into a `pause; lfence` predicted-but-mispredicted loop, and that on a Rust kernel this is a `rustc` `-Zretpoline` codegen change (not a `-Ctarget-feature`) that requires rebuilding `core` via `-Zbuild-std`
- Understand IBRS / IBPB / STIBP as the alternative microarchitectural mitigation when retpoline is not sufficient (Skylake-and-later), and the difference between **legacy IBRS** (toggled on every kernel entry/exit) and **Enhanced IBRS** (`IBRS_ALL`, set once at boot)
- See the performance tradeoffs: KPTI costs ~5–30% of syscall throughput depending on workload (and requires dropping the `GLOBAL` bit from kernel PTEs, which PCID/INVPCID then recovers); retpoline costs ~5% of indirect-branch-heavy code

## Feature Scope

### Track A — KPTI (Kernel Page Table Isolation)

- **A.1** — Per-process page-table pair: `kernel/src/mm/mod.rs::new_process_page_table` today clones the kernel's PML4 entries `[1..512]` into **every** process PML4; KPTI keeps that as the "kernel PML4" and builds a new "user PML4" carrying only PML4[0] (user pages) plus the **minimal entry set** (trampoline text + IDT + GDT/TSS + per-CPU entry stack), with kernel `.text`/heap/direct-map PTEs absent or NX in the user half.
- **A.2** — Syscall entry / exit asm trampoline: in `syscall_entry`, switch CR3 to kernel-PML4 **first** (before any kernel-stack/global access, using only a scratch register and a trampoline stack mapped in the user PML4), switch back before `sysretq`. ~200 LOC of asm. Preserve the existing `SFMASK` flag-masking on the rewrite.
- **A.3** — IRQ / IST entry / exit symmetry: hardware interrupts must do the same CR3 switch; NMI/`#DF`/IST vectors must **save-and-restore** the entry CR3 (paranoid path) since they can interrupt either address space.
- **A.4** — Keep the `GLOBAL` bit off kernel PTEs under KPTI (global pages survive a CR3 reload and silently defeat the isolation). m3OS does **not** mark kernel PTEs `GLOBAL` or enable `CR4.PGE` today, so this is a **guard** — and if PGE is ever introduced for off-path speed, suppress it whenever KPTI is active (or rely on PCID's no-flush instead).
- **A.5** — PCID (Process Context Identifier) + INVPCID to avoid a full TLB flush on every CR3 switch (~100 LOC, big perf win on Westmere+); the SMP TLB-shootdown path must flush **both** the kernel and user PCID of the target ASID.
- **A.6** — `RDCL_NO` auto-skip: under `mitigations=auto`, leave KPTI **off** on Meltdown-immune silicon (`IA32_ARCH_CAPABILITIES.RDCL_NO`); `full` forces it on, `off` forces it off.

### Track B — Retpoline

- **B.1** — Rust codegen flag: `-Zretpoline` (the dedicated nightly flag — `-Ctarget-feature=+retpoline-*` is a *target modifier* and is hard-rejected), added to the kernel's **existing** `-Zbuild-std=core,compiler_builtins,alloc` invocation in `xtask` (retpoline's ABI requires `core` be rebuilt with the thunk; do **not** silence the mismatch with `-Cunsafe-allow-abi-mismatch=retpoline`). Retpoline is compile-time-unconditional — the `mitigations=` boot flag cannot disable it.
- **B.2** — (Optional, learning) hand-written external thunk: with `-Zretpoline-external-thunk` rustc/LLVM emit a **single** r11-keyed `call __x86_indirect_thunk_r11` (not the GCC `__x86_indirect_thunk_rax..r15` family, and no linker rewrites call sites), so the kernel provides exactly one `global_asm!` thunk (`call; capture; pause; lfence; jmp` loop). The default learning path uses the compiler's internal `__llvm_retpoline_r11`.
- **B.3** — Verify on the fully-linked kernel after build-std: `objdump -d kernel.elf | grep -E '\b(call|callq|jmp|jmpq)[ \t]+\*'` returns zero hits (covers indirect **JMPs** — tail-call/trait dispatch — as well as CALLs), wired into `cargo xtask check`.

### Track C — SPEC_CTRL MSR toggling (IBRS / IBPB / STIBP)

- **C.1** — Host-testable feature decode in `kernel-core/src/spectre.rs`: `CPUID.07H.0:EDX[26]` enumerates **both** IBRS and IBPB (one bit), `[27]`=STIBP, `[29]`=ARCH_CAPABILITIES-present, `[31]`=SSBD; `IA32_ARCH_CAPABILITIES` (MSR `0x10A`) `RDCL_NO` (bit 0) / `IBRS_ALL` (bit 1). Classify eIBRS-vs-legacy. Host-tested like `kernel_core::storage`, with the `CPUID.0:EAX >= 7` max-leaf guard `probe_smep_smap` already enforces.
- **C.2** — Detect + enable IBRS in `cpuid.rs` (mirroring `probe_smep_smap`/`enable_smep_smap`): every `IA32_SPEC_CTRL` (MSR `0x48`) write gated on `EDX[26]`; **Enhanced IBRS** (`IBRS_ALL`) set **once at boot**, **legacy IBRS** toggled in the A.2/A.3 trampoline (set on entry, clear on exit). ~100 LOC. Cache a `spec_ctrl_base` so a write never clobbers STIBP/SSBD. eIBRS covers same-thread BTI only — STIBP (C.4) is still required for SMT siblings.
- **C.3** — IBPB barrier: issue `IA32_PRED_CMD` (MSR `0x49`, **write-only** — `rdmsr` faults) bit 0 on `switch_context` between **distinct** address spaces (not thread-to-thread within a process).
- **C.4** — STIBP (`CPUID.07H.0:EDX[27]`) for SMT siblings — opt-in per-process via an **m3OS-native** syscall/capability (m3OS has no `prctl`). Default off to avoid the perf cost.

### Track D — Configuration surface

- **D.1** — Host-testable `mitigations=` parser + per-vuln bug map + status vocabulary in `kernel-core` (`Not affected` / `Vulnerable` / `Mitigation: …` / `UNADDRESSED`), with the UNADDRESSED classes (MDS/L1TF/SSB/Retbleed/Downfall) always present in the map so a deferred class can never read as covered.
- **D.2** — Boot-flag plumbing: `mitigations=off|auto|full` gating Tracks A/C, with every selector consulting one global off-switch (the Linux `cpu_mitigations_off` discipline). This is a **net-new boot-cmdline surface** — m3OS has no kernel `/proc/cmdline` today, only per-process `/proc/<pid>/cmdline`.
- **D.3** — `m3ctl mitigations status` — read-only display of which mitigations are active on the current CPU, reading the boot-populated snapshot (not a re-`rdmsr` of the write-mostly SPEC_CTRL MSR), marking retpoline compiled-in, and enumerating the UNADDRESSED classes + the Grimsdal "ring-3 driver isolation ≠ Spectre mitigation" caveat.

## Important Components and How They Work

### KPTI page-table pair

In single-PML4 design, every user-mode process page table maps both user pages (`USER_ACCESSIBLE`) and kernel pages (kernel-only) — in m3OS, `kernel/src/mm/mod.rs::new_process_page_table` literally clones the kernel's PML4 entries `[1..512]` into every process PML4. Meltdown demonstrated that speculation across a privileged-load instruction leaks the value into a cache-observable side channel — so the CPU can be tricked into reading kernel memory while user code is the architecturally permitted reader. KPTI's answer: the user-mode CR3 points to a PML4 that does not contain the kernel mappings at all. The CPU literally cannot speculate into them. What stays mapped in **both** PML4s is not a single page but a **minimal entry set** — the syscall/IRQ entry-and-exit trampoline text, the IDT, the GDT/TSS, and a per-CPU entry stack (Linux's `cpu_entry_area`) — just enough to switch CR3 to the kernel copy and reach the real handler; the first instructions after SYSCALL/an IRQ run on the user CR3, so anything they touch before the switch must live in that set. One more subtlety: kernel PTEs must lose the `GLOBAL` bit (or be PCID-tagged) under KPTI, or stale global TLB entries survive the CR3 reload and leak anyway (m3OS does not set `GLOBAL`/enable `CR4.PGE` today, so this is a guard the KPTI work must hold, not a removal). And KPTI defends **Meltdown only** — it does nothing for Spectre, and on `RDCL_NO` silicon (Meltdown-immune) it should be skipped entirely.

### Retpoline

`call *%rax` lets the CPU speculate based on the Branch Target Buffer's prediction for that indirect site. An attacker who controls another process can train the BTB to predict an attacker-chosen kernel address, causing the kernel to speculatively execute attacker-chosen code. Retpoline replaces the indirect call with a sequence that pushes the target onto the stack, then `ret`s. The CPU's Return Stack Buffer has different (and harder-to-poison) prediction behavior, and the retpoline thunk sets up the RSB to predict a `pause; lfence` spin loop (the `lfence` matters — `pause` alone is not a speculation barrier on AMD), neutering speculation. On a Rust kernel the compiler emits these reroutes itself under `rustc -Zretpoline` (the `call __x86_indirect_thunk_r11` / internal `__llvm_retpoline_r11` lowering) — no linker script rewrites call sites — and because retpoline is an ABI-affecting target modifier, `core` must be rebuilt with it (`-Zbuild-std`). Note retpoline is **not** complete Spectre-v2 coverage on all silicon: Skylake-class parts also want RSB stuffing on entry, and Retbleed (2022) motivated `RETHUNK`/`__x86_return_thunk` — both deferred here.

### Cost / benefit tradeoff

KPTI is the headline mitigation. On Skylake-and-later silicon Intel introduced PCID + INVPCID which dropped KPTI's overhead from ~30% to ~5% on syscall-heavy workloads. Retpoline is cheap on most code (~1–2% overhead) but expensive on hot indirect-branch sites (~10–15%). IBRS toggling per kernel entry is moderately expensive; many production OSes use IBRS only on the affected silicon families (Skylake-derived) and retpoline elsewhere.

## How This Builds on Earlier Phases

- Extends the Phase 75 W^X model with KPTI — together they cover the bulk of the "code injection via memory-corruption + speculation" attack surface.
- Extends Phase 77's SMEP + SMAP with the more expensive class of mitigations.
- Reuses the Phase 11 process model — each process already owns its PML4; this phase just splits each PML4 into a pair.

## Implementation Outline

1. Write the host-testable CPUID/MSR/`mitigations=` decode in `kernel-core/src/spectre.rs` **first** (proven by `cargo xtask check`, no QEMU) — the bits the design originally got wrong are pinned here before any kernel wiring.
2. Implement KPTI — the headline architectural change (page-table pair, CR3 trampoline on the syscall + IRQ/IST paths, drop the `GLOBAL` bit).
3. Add PCID + INVPCID immediately after to recover the syscall-throughput cost.
4. Add retpoline codegen: `-Zretpoline` on the existing `-Zbuild-std` kernel build + the `objdump` verification gate.
5. Add SPEC_CTRL: legacy-IBRS toggle vs eIBRS set-once (branch on `IBRS_ALL`), IBPB on cross-process switch, optional STIBP.
6. Implement the `mitigations=off|auto|full` boot flag (single global off-switch, `RDCL_NO` auto-skip) + `m3ctl mitigations status`.
7. **Prove it with a Meltdown PoC** that attempts a kernel read (bare-metal-validated — QEMU TCG does not model speculation); a stack-switch-only KPTI passes everything else while still leaking.
8. Performance-regression test: the smoke suite should be no more than 30% slower with `mitigations=full` versus `mitigations=off` on Skylake-class silicon **when PCID is active**.
9. Bump kernel to `0.84.0`; cut the learning doc (`docs/84-spectre-mitigations.md`) + the operator reference (`docs/security/spectre-mitigations.md`).

## Acceptance Criteria

- With `mitigations=full`, a Meltdown PoC (the public 2018 reference exploit, ported to m3OS) fails to read kernel memory. This is **bare-metal/VFIO-validated** — QEMU TCG does not model speculative out-of-order execution, so the gate skips-with-reason there (the Phase 79/80/82 QEMU-blind-hardware precedent).
- With `mitigations=full`, all existing smoke + regression gates pass; syscall throughput is at most 30% slower than `mitigations=off` **when PCID is active** (the bound is conditioned on PCID).
- Under `mitigations=auto`, KPTI is skipped on `RDCL_NO` silicon; IBRS is set-once on eIBRS (`IBRS_ALL`) parts and toggled per-entry on legacy parts; `mitigations=off` leaves no track half-applied (every selector consults the one global off-switch).
- `m3ctl mitigations status` reports the active set on the booted CPU, marks retpoline as compiled-in (not runtime-togglable), and explicitly enumerates the **UNADDRESSED** classes (MDS/L1TF/SSB/Retbleed/Downfall) rather than silently omitting them.
- Documentation: a learner-facing `docs/84-spectre-mitigations.md` (indexed in `docs/README.md`) and an operator-facing `docs/security/spectre-mitigations.md` describe which silicon families are protected by which mitigation and which residual risks remain (including the seL4 timing-channel and Grimsdal microkernel-isolation caveats).
- Kernel bumped to `0.84.0`.

## Companion Task List

- [Phase 84 Task List](./tasks/84-spectre-mitigations-tasks.md) — authored ahead of implementation (Tracks A–E with host-tested `kernel-core` decode, the Meltdown-PoC integrity gate, and the version-bump + learning-doc closeout).

## How Real OS Implementations Differ

- Linux's mitigations matrix has ~30 distinct vulnerability identifiers (Spectre-v1, v2, RSB-underflow, MDS, TAA, ITLB-multihit, SRBDS, SRSO, Inception, ...) — m3OS at Phase 84 ships the four headline 2018-era ones. Linux's KPTI is a paired 8 KiB PGD (two adjacent 4 KiB halves selected by `PTI_USER_PGTABLE_BIT`); the user half clones only `cpu_entry_area` (`pti_clone_entry_text`), and `SWITCH_TO_KERNEL_CR3`/`SWITCH_TO_USER_CR3` bracket `entry_SYSCALL_64`.
- Real OSes have CPU-specific tunings choosing IBRS vs. retpoline based on the silicon's known characteristics (Skylake gets IBRS; pre-Skylake gets retpoline; AMD has its own table). m3OS uses the simple-and-conservative "retpoline always, IBRS/eIBRS when available, KPTI unless `RDCL_NO`" rule.
- Linux's `mitigations=` flag supports per-vuln granularity (e.g., `nospectre_v2`, `nopti`, `nopti=force`); m3OS at this phase exposes only the three coarse levels.
- **Redox (the nearest Rust microkernel) is a cautionary tale, not a model:** its `src/arch/x86_shared/pti.rs` is gated on a `pti` Cargo feature that is *absent from `default`* (`#TODO: remove when threading issues are fixed`), the kernel-PML4 unmap is **commented out**, and the syscall trampoline's PTI calls are commented out — Redox runs with kernel mappings live in the user CR3 and ships **zero** retpoline/IBRS/IBPB code, so m3OS Phase 84 lands strictly *ahead* of Redox's shipped x86 hardening. SerenityOS likewise shares kernel mappings into every process (no KPTI). m3OS therefore sources Track A from Linux + the KAISER paper, and Track B has no Rust-OS prior art (cite rustc/LLVM + Linux `retpoline.S`).
- **A microkernel does not get Spectre-immunity for free:** Grimsdal et al. (NordSec 2019) showed Flush+Reload and Spectre work across component boundaries on Genode/OKL4/NOVA regardless of separation — moving NVMe/e1000/AHCI drivers to ring 3 does *not* mitigate Spectre between them. And, mirroring **seL4**'s verified-confidentiality scope (which explicitly *excludes* microarchitectural timing channels, "dealt with empirically"), m3OS makes no claim of freedom from timing channels.
- L1TF, MDS, TAA — not addressed at this phase.

## Deferred Until Later

- L1TF / MDS / TAA / Zombieload / RIDL / Fallout
- SRSO (Inception)
- Retbleed — RSB stuffing on entry and `RETHUNK`/`__x86_return_thunk` (retpoline alone is not complete Spectre-v2 coverage on all silicon)
- Branch History Injection
- DownFall / GDS
- Per-vulnerability mitigation toggles
- Fine-grained STIBP / SMT scheduling integration
- Speculative-load hardening compiler pass (LLVM SLH)
- Microarchitectural **timing** channels generally (the seL4-style time-protection problem) — explicitly out of scope
