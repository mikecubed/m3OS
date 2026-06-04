# Phase 84 — Spectre / KPTI / Retpoline / IBRS Mitigations: Task List

**Status:** Planned (post-1.0)
**Source Ref:** phase-84
**Depends on:** Phase 75 (W^X Enforcement) ✅, Phase 77 (Pre-1.0 Correctness — SMEP + SMAP baseline) ✅, Phase 83 (Release 1.0 Gate) ✅; grading evidence from the Phase 74a pre-1.0 audit (`docs/appendix/audit-status/74a-pre-1.0-audit.md` row 9, "No Spectre/SMEP/SMAP/KPTI mitigations on real silicon", graded **HIGH for SMEP+SMAP; deferrable for KPTI** — which is exactly why SMEP+SMAP landed in Phase 77 and KPTI/retpoline/IBRS are this post-1.0 phase).
**Goal:** Land the **expensive** post-Meltdown / post-Spectre-v2 mitigations on top of the cheap Phase 77 SMEP+SMAP baseline: KPTI (Kernel Page Table Isolation), retpoline indirect-branch hardening, and the `IA32_SPEC_CTRL` MSR family (IBRS/eIBRS/IBPB/STIBP), all behind a `mitigations=off|auto|full` boot policy with a `m3ctl mitigations status` reporter. The phase reuses the Phase 77 detect→enable→status pattern already in `kernel/src/arch/x86_64/cpuid.rs` (`probe_smep_smap`/`enable_smep_smap`/`cr4_smep_enabled`), puts every bit of CPUID/MSR/`mitigations=` decode logic in host-testable `kernel-core` (mirroring `kernel_core::storage`), and rewrites the syscall/IRQ entry-exit asm in `kernel/src/arch/x86_64/syscall/mod.rs` + `kernel/src/arch/x86_64/interrupts.rs` to carry a CR3 trampoline. The keystone change is in `kernel/src/mm/mod.rs::new_process_page_table`, which **today clones the kernel's PML4 entries `[1..512]` into every process PML4** — KPTI replaces that with a kernel/user PML4 **pair** so the user-mode CR3 cannot even speculate into kernel memory. The headline correctness proof is a userspace **Meltdown PoC** gate (bare-metal-validated; QEMU TCG does not model speculation), because a stack-switch-only "PTI" compiles, boots, and passes an ordinary smoke test while still leaking kernel memory — the exact trap Redox fell into (its `pti.rs` is feature-gated off with its kernel-unmap commented out).

> **This is a planning task list authored ahead of implementation (post-1.0).** Implementation acceptance items below are **unchecked `[ ]`** — they are the implementation contract for a future Phase 84 PR, not work already done. The only items completed in the **task-authoring PR** (this PR) are the documentation-reconciliation items explicitly marked *(landed in this authoring PR)* in Track E (E.6 README Tasks-cell flip, E.7 design-doc reconciliation). Mirrors the Phase 83 pattern (commit `440c74b` authored the task list + reconciled the design doc before the implementation PR).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | **KPTI** — split the per-process PML4 into a kernel/user pair (`kernel/src/mm/mod.rs`), a CR3 trampoline on the syscall path (`syscall/mod.rs`) and IRQ/IST path (`interrupts.rs`), GLOBAL-bit removal, PCID/INVPCID TLB-flush avoidance, and `RDCL_NO` auto-skip | Phase 11 process model, C.1 (feature decode) | Planned |
| B | **Retpoline** — enable `-Zretpoline` on the existing `-Zbuild-std` kernel build (`xtask`), an optional hand-written `__x86_indirect_thunk_r11` external thunk, and an `objdump` residual-indirect-branch verification gate wired into `cargo xtask check` | — | Planned |
| C | **SPEC_CTRL MSR** — host-testable CPUID/MSR feature decode in `kernel-core`, IBRS/eIBRS detect+enable in `cpuid.rs` (mirroring `probe/enable_smep_smap`), IBPB on cross-process `switch_context`, STIBP per-process opt-in | C.1 | Planned |
| D | **Configuration surface** — host-testable `mitigations=` parser + per-vuln bug map + status vocabulary in `kernel-core`, boot-flag plumbing that gates A/C with a single global off-switch, and a `m3ctl mitigations status` reporter | A, B, C, C.1 | Planned |
| E | **Validation + release closeout** — the Meltdown-PoC smoke gate, the ≤30% perf-regression gate, kernel `0.83.0`→`0.84.0`, the Phase 84 learning doc + `docs/security/` operator reference, README/AGENTS alignment, and design-doc reconciliation | A–D | Planned (E.6/E.7 reconciliation **landed in this authoring PR**) |

> **Ordering note.** The pure-logic feature/parse/report decode (**C.1** and **D.1**) is written and host-tested **first** — every CPUID bit (`CPUID.07H.0:EDX[26]`=IBRS+IBPB, `[27]`=STIBP, `[29]`=ARCH_CAPABILITIES, `[31]`=SSBD), the `IA32_ARCH_CAPABILITIES` (MSR `0x10A`) `RDCL_NO`/`IBRS_ALL` decode, the eIBRS-vs-legacy classification, and the `mitigations=` parser are proven by `cargo xtask check` with no QEMU, exactly as Phase 82 put the AHCI register/FIS math in `kernel-core::storage` and Phase 77 put SMEP/SMAP detection in `cpuid.rs`. Then **A** (KPTI, the headline architectural change) and **C** (SPEC_CTRL) consume that decode; **B** (retpoline) is an independent build-system + asm change; **D** wires the boot policy + reporter; **E** proves it with the Meltdown PoC and performs the version bump + doc cut. The single load-bearing review check is **E.1**: a Meltdown PoC that actually attempts a kernel read, because a half-built KPTI silently passes everything else.

> **What KPTI does and does not buy (honesty invariant).** KPTI defeats **Meltdown only** (a privileged-data transient-load leak) — it does **not** mitigate Spectre. Retpoline + IBRS/IBPB cover **Spectre-v2** (branch-target injection). Spectre-v1, MDS, L1TF, SSB/Spectre-v4, Retbleed, and Downfall/GDS are **out of scope** and must be reported as `UNADDRESSED` (D.3), not silently omitted. Moving NVMe/e1000/AHCI drivers to ring 3 (the m3OS microkernel posture) does **not** by itself mitigate Spectre between userspace components — Grimsdal et al. (NordSec 2019) showed Flush+Reload/Spectre work across component boundaries regardless of microkernel separation. And, per seL4's verification-scope statement, m3OS makes **no claim** of freedom from microarchitectural **timing** channels.

> **Redox is a cautionary tale, not the KPTI model.** Other m3OS task docs cite Redox heavily for ring-3 driver conventions, but Redox has **no working KPTI**: `src/arch/x86_shared/pti.rs` is gated on a `pti` Cargo feature that is **absent from `default`** (`#TODO: remove when threading issues are fixed`), its kernel-heap-PML4 unmap is **commented out**, and the syscall trampoline's `// TODO: Map PTI` call sites are commented out — so Redox runs with kernel mappings live in the user CR3 and performs no CR3 switch. Redox also ships **zero** retpoline/IBRS/IBPB/STIBP code. Source A.x acceptance from **Linux** (`entry_SYSCALL_64`, `SWITCH_TO_KERNEL_CR3`/`SWITCH_TO_USER_CR3`, `cpu_entry_area`, `pti_clone_entry_text`) and the **KAISER** paper instead. The parts Redox *did* ship that belong in the A.2 trampoline rewrite (SMAP/`IA32_FMASK` flag-clearing, the `sysret` canonical-RCX guard) are called out as regression invariants, not new design.

---

## Track A — KPTI (Kernel Page Table Isolation)

### A.1 — Split the per-process PML4 into a kernel/user pair

**Files:**
- `kernel/src/mm/mod.rs` (`new_process_page_table`, `AddressSpace`, `KERNEL_PML4_PHYS`, `restore_kernel_cr3`)
- `kernel/src/mm/paging.rs` (mapper construction)
- `kernel/src/task/mod.rs` (`UserReturnState.cr3_phys` — the per-task kernel CR3 snapshotted by the syscall return path; **not** a `Task` field)
- `kernel/src/process/mod.rs` (`AddressSpace::new`, `pml4_phys`, `spawn_process_with_cr3`)

**Symbol:** `new_process_page_table`; a new `build_user_shadow_pml4` (companion to it); `AddressSpace` gains a second `user_pml4_phys` frame; `UserReturnState` gains a second `user_cr3_phys` beside its existing `cr3_phys`
**Why it matters:** `new_process_page_table` (mm/mod.rs:282) currently does `for i in 1..512 { new_pml4[i] = cur_pml4[i].clone() }` — it copies the **entire** kernel half (and kernel low-half) into every process's single PML4, so the kernel is fully mapped while ring 3 runs. That is precisely the Meltdown-exposed design. The per-process CR3 machinery already exists (`UserReturnState.cr3_phys`, `KERNEL_PML4_PHYS`, `restore_kernel_cr3` at mm/mod.rs:169) — so KPTI **refactors** the single per-process PML4 into a pair, it does not invent CR3 switching: keep the current per-process PML4 as the **kernel** CR3 and build a second **user** PML4 that maps **only** PML4[0] (user pages — the loader's `0x400000` `USER_VADDR_MIN` convention) plus the minimal entry/exit trampoline set (A.2). The CPU literally cannot speculate into kernel memory from the user CR3.

**Acceptance:**
- [ ] With `mitigations=full`, `AddressSpace` carries two PML4 frames (kernel + user) and `Task` carries both `cr3_phys` (kernel) and `user_cr3_phys`; `new_process_page_table` no longer leaves the full kernel half present in the user-visible PML4.
- [ ] A kernel self-test (or host test over a `kernel-core` model of the walk) walks the user PML4 and asserts it maps **only** the user lower-half (PML4[0]) plus the A.2 trampoline set, and that kernel `.text`/heap/direct-map/page-table PTEs are **absent or NX** in the user half (mirrors Linux's NX-on-user-kernel-text trap).
- [ ] With `mitigations=off`, `new_process_page_table` behaves exactly as today (single combined PML4) — no perf or correctness change on the off path.

### A.2 — Syscall entry/exit CR3 trampoline

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`syscall_entry` `global_asm!` block, the `sysretq` tail)
**Symbol:** `syscall_entry`; new `SWITCH_TO_KERNEL_CR3` / `SWITCH_TO_USER_CR3` asm macros
**Why it matters:** the `syscall_entry` stub today switches to the per-core kernel stack (`mov rsp, gs:[OFF_STACK_TOP]`) but performs **no CR3 switch** — safe only because the kernel is mapped in the user PML4. Under KPTI the first instructions of `syscall_entry` execute on the **user** CR3, so the CR3 switch must come **first**, using only a scratch register and a trampoline stack mapped in the user PML4, *before* any kernel-stack store. A useful m3OS-specific simplification: the stub does **not** `swapgs` (m3OS sets `GS_BASE == KERNEL_GS_BASE` to the same per-core pointer because the user cannot change GS — see syscall/mod.rs:1117), so `gs:[OFF_STACK_TOP]` is valid on either CR3 — but the per-core data page (and the trampoline stack) it reads must be in the user PML4 minimal set (A.1). The reverse switch goes immediately before `sysretq`. This is the single most error-prone change in the phase (an out-of-order switch faults with no reachable handler).

**Acceptance:**
- [ ] On `mitigations=full`, `syscall_entry` writes the kernel CR3 **before** the first kernel-stack store; a debug build faults loudly if kernel-stack memory is touched while the user CR3 is still loaded (ordering pin).
- [ ] The per-core data page, the trampoline stack, and the entry text the stub touches before the switch are mapped in the **user** PML4 (A.1); the path from SYSCALL to the CR3 switch reads no page absent from the user half.
- [ ] `sysretq` is preceded by `SWITCH_TO_USER_CR3`; a round-trip syscall under `mitigations=full` returns to userspace with the user CR3 active.
- [ ] **Regression invariant (do not drop while rewriting this asm):** the entry path still masks `IF` via `SFMASK` (the existing `sti`/`cli` window is preserved) — a test asserts this still holds on the rewritten stub.
- [ ] *(Optional hardening, net-new — not an existing invariant.)* m3OS uses AMD-style `SYSCALL`/`SYSRET` (`Star`/`LStar`) and has **no** `sysret` canonical-RCX guard today; if the CVE-2012-0217-class sign-extension guard is added (the Intel-SYSRET non-canonical-RIP `#GP`-in-ring-0 fault model), the KPTI asm rewrite is the natural place — but confirm m3OS's `sysretq` path is actually exposed before adding a guard the AMD semantics may not need.

### A.3 — IRQ / IST entry-exit CR3 symmetry (paranoid path)

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs` (`init_idt`, the `extern "x86-interrupt"` stubs)
- `kernel/src/arch/x86_64/gdt.rs` (TSS / IST stacks)

**Symbol:** the interrupt entry stubs; a paranoid `SAVE_AND_SWITCH_TO_KERNEL_CR3` / `RESTORE_CR3` pair
**Why it matters:** a hardware interrupt taken from ring 3 also enters on the **user** CR3, so every IDT entry needs the same switch as the syscall path. IST/NMI/`#DF` handlers can interrupt **either** address space, so they must **save-and-restore** the prior CR3 (the "paranoid" path) rather than assuming kernel CR3 — otherwise a fault taken on the user CR3 restores the wrong space on `iret`.

**Acceptance:**
- [ ] On `mitigations=full`, an interrupt taken from ring 3 switches to kernel CR3 on entry and restores the user CR3 on `iret`; an interrupt taken from ring 0 leaves the kernel CR3 active.
- [ ] NMI / `#DF` / IST-using vectors save the entry CR3 and restore exactly it on return; a test takes such an exception from **both** the user and kernel CR3 and verifies the correct space is restored.
- [ ] The paranoid save/restore is a **distinct** code path from the existing `mm::restore_kernel_cr3()` (mm/mod.rs:169), which unconditionally writes `KERNEL_PML4_PHYS` with `Cr3Flags::empty()` — routing the NMI/`#DF` path through it would clobber the saved entry CR3 and drop any PCID/no-flush bits (A.5); the paranoid restore writes back the *captured* CR3 verbatim.
- [ ] The `rustc` `x86-interrupt` calling convention's automatic frame is preserved; the CR3 save/restore is added without breaking the existing `preempt_trap_frame` path.

### A.4 — Keep the GLOBAL bit off kernel PTEs under KPTI (guard)

**Files:**
- `kernel/src/mm/paging.rs` / `kernel/src/mm/mod.rs` (kernel mapping `PageTableFlags`)

**Symbol:** the kernel-mapping `PageTableFlags` (a `GLOBAL`/`CR4.PGE` guard, **not** an existing removal site)
**Why it matters:** PTEs marked `GLOBAL` survive a CR3 reload, so a KPTI that leaves kernel pages global lets kernel TLB entries persist into userspace — the isolation becomes a no-op and a Meltdown PoC may **still** read kernel data from a stale global TLB entry, the most insidious silent-failure mode of a first KPTI (Redox encodes exactly this: `startup/memory.rs` `.global(... not(feature = "pti"))`). **Important repo fact:** m3OS does **not** currently mark kernel PTEs `GLOBAL` or enable `CR4.PGE` (verified — no `PageTableFlags::GLOBAL` site in `kernel/src/mm/`), so this task is a **guard**, not a removal: it must keep that property true under KPTI, and if `CR4.PGE`/`GLOBAL` is ever introduced as an off-path throughput optimization, it must be suppressed whenever KPTI is active.

**Acceptance:**
- [ ] The task first establishes the current state (no kernel-PTE `GLOBAL`, no `CR4.PGE`); with `mitigations=full` and **no** PCID (A.5 inactive), kernel-half PTEs remain non-`GLOBAL`, so each CR3 switch actually evicts kernel translations.
- [ ] If a future `CR4.PGE`/`GLOBAL` optimization exists on the `mitigations=off` path, it is provably suppressed under KPTI (a self-test asserts no kernel PTE visible across a KPTI CR3 switch carries `GLOBAL`); with PCID active (A.5), the CR3 writes set the no-flush bit instead of paying a full flush.

### A.5 — PCID / INVPCID TLB-flush avoidance

**Files:**
- `kernel/src/arch/x86_64/cpuid.rs` (CR4.PCIDE enable after a CPUID probe, beside `enable_smep_smap`)
- `kernel/src/arch/x86_64/syscall/mod.rs` + `interrupts.rs` (the CR3-build helper used in the trampolines)
- `kernel/src/smp/` (TLB-shootdown IPI path)

**Symbol:** new `probe_pcid` / `enable_pcid`; a `build_cr3(pml4_phys, pcid, noflush) -> u64` helper
**Why it matters:** without PCID every KPTI CR3 swap (twice per syscall) flushes the whole TLB — the source of the original 5–30% overhead. PCID tags TLB entries with a 12-bit ASID in `CR3[11:0]`; the kernel and user halves of one process coexist under two PCIDs (a chosen high bit — e.g. bit 11, the Linux `PTI_USER_PCID_BIT` convention — distinguishes the user PCID from the kernel PCID of the same ASID, which **halves** the usable PCID space, a deliberate design choice not an architectural requirement), and setting `CR3` bit 63 (no-flush) on a switch skips the flush. `INVPCID` (which takes a 16-byte descriptor + an invalidation-type selector; type-2 = single-context) selectively invalidates a non-current PCID on unmap.

**Acceptance:**
- [ ] PCID is enabled **only** when `CPUID.01H:ECX[17]` (PCID) is set, and `INVPCID` use only when `CPUID.07H.0:EBX[10]` is set; absent either, the code falls back to plain full-flush CR3 writes (gated, with the decode in C.1).
- [ ] The SMP TLB-shootdown path flushes **both** the kernel and user PCID of the target process's ASID (not just the active CR3's PCID); a test forces an unmap and asserts a subsequent read observes the new mapping (no stale no-flush hit).
- [ ] The ≤30% syscall-throughput budget (E.2) is met **only** when PCID is active; the perf criterion is explicitly gated on PCID presence.

### A.6 — `RDCL_NO` auto-skip under `mitigations=auto`

**Files:**
- `kernel/src/arch/x86_64/cpuid.rs` (`IA32_ARCH_CAPABILITIES` read)
- the boot mitigation selector (D.2)

**Symbol:** the `auto`-mode KPTI decision consuming `SpecCtrlFeatures.rdcl_no` (C.1)
**Why it matters:** `IA32_ARCH_CAPABILITIES` (MSR `0x10A`) bit 0 `RDCL_NO` advertises a CPU that is **not** susceptible to Meltdown (all AMD + recent Intel). Linux leaves PTI **off** by default on such CPUs even in `auto` mode — paying KPTI's cost on immune silicon is pure waste. `full` still forces KPTI on (for testing); `off` always disables it.

**Acceptance:**
- [ ] Under `mitigations=auto`, KPTI is **OFF** when `RDCL_NO` is set (and `m3ctl mitigations status` reports Meltdown `Not affected`), and **ON** otherwise.
- [ ] Under `mitigations=full`, KPTI is forced on regardless of `RDCL_NO`; under `mitigations=off`, KPTI is off regardless.
- [ ] The `RDCL_NO` decode is the host-tested C.1 function, not an ad-hoc inline bit test.

---

## Track B — Retpoline (Spectre-v2 indirect-branch hardening)

### B.1 — Enable retpoline codegen via `-Zretpoline` on the existing `-Zbuild-std` kernel build

**File:** `xtask/src/main.rs` (the kernel cargo invocation — it already passes `-Zbuild-std=core,compiler_builtins,alloc` + `-Zbuild-std-features=compiler-builtins-mem` for the builtin `x86_64-unknown-none` target)
**Symbol:** the kernel build args / `RUSTFLAGS` env in the kernel build path
**Why it matters:** the design doc's `-C target-feature=+retpoline-indirect-branches,+retpoline-indirect-calls` is **wrong** — on current `rustc` those are *target modifiers* and `-Ctarget-feature` is hard-rejected (`cannot be enabled with -Ctarget-feature: use -Zretpoline`). The correct flag is the dedicated nightly `-Zretpoline`. Retpoline is an ABI-affecting target modifier, so `core` **must** be rebuilt with it — which m3OS already does via `-Zbuild-std`, so this is a small additive change, not new build machinery. The flag belongs in the **xtask kernel build invocation** (a JSON target spec like `x86_64-m3os.json`, which is not even the default target, cannot carry `-Z` flags).

**Acceptance:**
- [ ] The kernel builds with `-Zretpoline` added to the existing `-Zbuild-std=core,compiler_builtins,alloc` invocation; LLVM reroutes indirect calls through its internal `__llvm_retpoline_r11` thunk.
- [ ] The build does **not** pass `-Cunsafe-allow-abi-mismatch=retpoline` (that escape hatch links an unprotected `core` and defeats the mitigation); removing `-Zbuild-std` reproduces the ABI-mismatch error, proving `core` is being rebuilt with the thunk.
- [ ] Retpoline is **compile-time-unconditional** (baked into codegen) — it cannot be toggled by the `mitigations=` boot flag like KPTI/IBRS. D.3 reports it as `compiled-in (cannot disable at boot)`; this is stated explicitly so a reader does not expect a runtime switch.

### B.2 — (Optional, learning) hand-written `__x86_indirect_thunk_r11` external thunk

**File:** `kernel/src/arch/x86_64/retpoline.rs` (new — a single `global_asm!` block)
**Symbol:** `__x86_indirect_thunk_r11`
**Why it matters:** `-Zretpoline-external-thunk` makes the kernel **provide** the thunk instead of using the compiler's internal one. The design doc's `__x86_indirect_thunk_rax` *family* and "linker scripts force the call into the thunk" are both wrong: rustc/LLVM emit a single **r11-keyed** call (`mov <target>,%r11; call __x86_indirect_thunk_r11`), so `__x86_indirect_thunk_r11` is the **only** undefined retpoline symbol the kernel must define — and the compiler emits that call directly (no linker rewrite). The per-register `rax..r15` set is the GCC `-mindirect-branch=thunk-extern` convention Linux uses, not LLVM's.

**Acceptance:**
- [ ] With `-Zretpoline-external-thunk`, `nm kernel.elf` shows `__x86_indirect_thunk_r11` **defined** and it is the **only** undefined retpoline symbol; no `__x86_indirect_thunk_rax..r15` family is referenced.
- [ ] The thunk body is the canonical capture loop — `call .Lcapture; .Lspec: pause; lfence; jmp .Lspec; .Lcapture: mov %r11,(%rsp); ret` — and **uses `lfence`** (PAUSE alone is not a speculation barrier on AMD).
- [ ] The thunk is in `.text`, W^X-mapped executable, and guarded against `--gc-sections` dropping it (`KEEP`/`#[used]`/`.global`).
- [ ] Recorded recommendation: the learning phase **defaults to `-Zretpoline`** (compiler-provided `__llvm_retpoline_r11`); the external thunk is an explicit exercise, not the default, since it adds a link dependency for marginal benefit absent hot-patching.

### B.3 — `objdump` residual-indirect-branch verification gate

**File:** `xtask/src/main.rs` (a new sub-step of `cmd_check`, run on the fully-linked `kernel.elf`)
**Symbol:** the verification step
**Why it matters:** the mitigation is void if even one indirect branch survives, so it must be **mechanically verified**. The design doc's `grep -E 'call[ \t]+\*'` is incomplete — it misses indirect **JMPs** (`jmp *reg`, the tail-call-optimized trait/fn-pointer dispatch that lowers to a jump) and the `q`-suffixed mnemonics. The check must run on the **fully-linked** `kernel.elf` (after `-Zbuild-std`) so rebuilt `core` is included.

**Acceptance:**
- [ ] `objdump -d <kernel.elf> | grep -E '\b(call|callq|jmp|jmpq)[ \t]+\*'` returns **zero** lines (covers indirect CALL **and** indirect JMP, both operand forms), run against the fully-linked kernel after build-std.
- [ ] A positive cross-check asserts the thunk is actually used: `objdump -dr <kernel.elf> | grep -c '__llvm_retpoline_r11'` (or `__x86_indirect_thunk_r11` in the external-thunk variant) is non-zero.
- [ ] The thunk and reroute sites contain **zero** XMM/SSE (soft-float clean) — consistent with the kernel's `-mmx,-sse,+soft-float`, documented so a reviewer does not assume an FPU/XSAVE interaction exists.
- [ ] The gate is wired into `cargo xtask check` so a regression (a new un-thunked indirect branch) **fails the build**, not just a smoke test.

### B.4 — RSB-stuffing + Retbleed honesty (scoping)

**Files:**
- `docs/security/spectre-mitigations.md` (E.5) — the residual-risk statement
- (optional) the A.2/A.3 entry asm — `FILL_RETURN_BUFFER` on kernel entry

**Symbol:** the documented scope boundary; optional 32-deep RSB-stuff macro
**Why it matters:** Skylake-class cores can underflow the Return Stack Buffer into the (poisonable) BTB, so Linux adds RSB stuffing on entry/context-switch; and Retbleed (2022) showed retpoline alone is insufficient on some parts, which is why Linux added `RETHUNK`/`__x86_return_thunk`. For a learning phase these may be deferred, but the docs must **not** claim retpoline = complete Spectre-v2 coverage.

**Acceptance:**
- [ ] The security doc (E.5) states retpoline is **not** complete Spectre-v2 coverage and lists RSB-stuffing and `RETHUNK`/`__x86_return_thunk` (Retbleed) as **Deferred Until Later**.
- [ ] (Optional) if implemented, a 32-deep `FILL_RETURN_BUFFER` runs on syscall+IRQ entry; a unit test counts the spin/`lfence` sequence. If not implemented, it is named explicitly in the Deferred list (no silent omission).

---

## Track C — `IA32_SPEC_CTRL` MSR family (IBRS / eIBRS / IBPB / STIBP)

### C.1 — Host-testable mitigation-feature decode in `kernel-core`

**Files:**
- `kernel-core/src/spectre.rs` (new)
- `kernel-core/src/lib.rs` (`pub mod spectre;`)

**Symbol:** `SpecCtrlFeatures::from_cpuid(leaf7_edx: u32, arch_caps: u64) -> SpecCtrlFeatures { ibrs_ibpb, stibp, ssbd, arch_caps_present, rdcl_no, eibrs }`; `IbrsMode { None, Legacy, Enhanced }`; `classify_ibrs(features) -> IbrsMode`
**Why it matters:** AGENTS.md mandates pure logic be host-tested in `kernel-core` (the kernel is `no_std` and cannot be `cargo test`ed in QEMU). The decode is exactly the error-prone part the design doc got wrong, so pinning it in host tests (modeled on `cpuid.rs::XSaveFeatures::from_raw`) makes a bit-transcription slip a **failing test**, not a silent `#GP` or an unprotected boot. `CPUID.(EAX=07H,ECX=0):EDX[26]` enumerates **both IBRS and IBPB** (one bit); `[27]` is STIBP; `[29]` is ARCH_CAPABILITIES-present; `[31]` is SSBD. `IA32_ARCH_CAPABILITIES` (MSR `0x10A`) `[0]`=`RDCL_NO`, `[1]`=`IBRS_ALL` (eIBRS).

**Acceptance:**
- [ ] Host test: `EDX[26]` set → `ibrs_ibpb == true` (gates **both**); `EDX[27]` → `stibp` only; `EDX[31]` → `ssbd`; `EDX[29]` → `arch_caps_present` (and only then is `arch_caps` consulted) (`kernel_core::spectre::tests::edx_bits`).
- [ ] **Max-basic-leaf guard (the same trap `probe_smep_smap` defends at cpuid.rs:241):** `from_cpuid`/its caller treats leaf-7 EDX as **zero** when `CPUID.0:EAX < 7` — executing `CPUID` with an unsupported basic leaf returns the *highest supported leaf's* data, whose bits 26/27/31 could otherwise be mis-read as IBRS/STIBP/SSBD on an old/VM CPU (`tests::leaf7_absent_reads_zero`).
- [ ] Host test: `arch_caps[1]` (`IBRS_ALL`) set → `classify_ibrs == Enhanced` (set-once-at-boot); `ibrs_ibpb` set but `IBRS_ALL` clear → `Legacy` (per-entry toggle); neither → `None` (`tests::ibrs_mode`).
- [ ] Host test: `arch_caps[0]` (`RDCL_NO`) set → `rdcl_no == true` (Meltdown-immune; drives A.6) (`tests::rdcl_no`).
- [ ] `cargo xtask check` compiles and runs the new module (`kernel-core` is already in the check list; no new crate entry needed — recorded in E.6).

### C.2 — IBRS/eIBRS detect + enable in `cpuid.rs` (mirror `probe/enable_smep_smap`)

**File:** `kernel/src/arch/x86_64/cpuid.rs`
**Symbol:** new `probe_spec_ctrl()` / `enable_ibrs()` / `spec_ctrl_active()` beside `probe_smep_smap`/`enable_smep_smap`/`cr4_smep_enabled`; MSR access via `Msr::new(0x48)` (the `x86_64`-crate wrapper already used in `microcode.rs`)
**Why it matters:** Phase 77 already established the exact detect→enable→status shape in this file; Track C reuses it so the SPEC_CTRL path is idiomatic. Every `rdmsr`/`wrmsr` of `0x48` must be gated on the C.1 `ibrs_ibpb` bit (an unguarded MSR access `#GP`s on a CPU that lacks it). The eIBRS-vs-legacy split (C.1 `IbrsMode`) decides *when* the MSR is written. Note eIBRS is **not** full Spectre-v2 coverage: `IBRS_ALL` restricts *same-thread* cross-privilege BTI only — SMT-sibling isolation still requires STIBP (C.4) even on eIBRS silicon, so "set IBRS once and done" does not retire the STIBP control.

**Acceptance:**
- [ ] `IA32_SPEC_CTRL` (MSR `0x48`) is `rdmsr`/`wrmsr`-accessed **only** when C.1 reports `ibrs_ibpb`; booting on a CPU lacking the bit performs **no** SPEC_CTRL access (no `#GP`) — asserted by a probe that runs on a feature-stripped CPUID path.
- [ ] `IbrsMode::Enhanced` → IBRS (`SPEC_CTRL` bit 0) is written **once at boot** and never toggled; `IbrsMode::Legacy` → IBRS is toggled in the A.2/A.3 trampolines (set on kernel entry, cleared on user exit). A test on a legacy path observes `SPEC_CTRL.IBRS == 0` in userspace and `1` in kernel; on eIBRS it stays `1`.
- [ ] A blind full-MSR write never clobbers `STIBP` (bit 1) / `SSBD` (bit 2): the kernel caches a `spec_ctrl_base` (mirroring Linux `x86_spec_ctrl_base`) and writes the combined value; `spec_ctrl_active()` reads the cached snapshot, not a re-`rdmsr` of the write-mostly MSR.

### C.3 — IBPB barrier on cross-process `switch_context`

**Files:**
- `kernel/src/task/scheduler.rs` (the `switch_context` / address-space-switch boundary)
- `kernel/src/arch/x86_64/cpuid.rs` (`Msr::new(0x49)`)

**Symbol:** `issue_ibpb()` invoked at the process-switch point
**Why it matters:** IBPB (`IA32_PRED_CMD` MSR `0x49` bit 0, **write-only**) flushes the indirect branch predictor at a security-domain boundary. Issuing it between **distinct** processes (not thread-to-thread within one address space) stops a prior process from having trained the predictor against the next. `PRED_CMD` is write-only — an `rdmsr` of `0x49` faults.

**Acceptance:**
- [ ] `PRED_CMD` (`0x49`) bit 0 is written `1` **only** on a switch between distinct address spaces (`as_gen`/PML4 differs), gated on C.1 `ibrs_ibpb`; thread switches within one process issue no IBPB.
- [ ] `PRED_CMD` is **never** `rdmsr`'d (write-only).
- [ ] With `mitigations=off`, no IBPB is issued (the switch path consults the global off-switch, D.2).

### C.4 — STIBP per-process opt-in (default off)

**Files:**
- a new m3OS-native opt-in surface (a `sys_*`/capability, **not** Linux `prctl`)
- `kernel/src/arch/x86_64/cpuid.rs` (`SPEC_CTRL` bit 1)

**Symbol:** the opt-in control + the `SPEC_CTRL.STIBP` set/clear
**Why it matters:** STIBP (`SPEC_CTRL` bit 1) stops an SMT sibling thread from influencing this thread's branch predictor, at a real perf cost, so it is **default-off** and opt-in. The design doc references Linux `prctl(PR_SET_SPECULATION_CTRL, ...)`, but **m3OS has no `prctl`** — the opt-in must be an m3OS-native syscall or capability.

**Acceptance:**
- [ ] STIBP (`SPEC_CTRL` bit 1) is set **only** for processes that opt in via the m3OS-native control, gated on C.1 `stibp` (`CPUID.07H.0:EDX[27]`); default-off is verified (a process that does not opt in runs with STIBP clear).
- [ ] The opt-in is documented as an m3OS surface, **not** presented as Linux `prctl`.
- [ ] Setting STIBP composes with C.2's `spec_ctrl_base` cache (no clobber of IBRS/SSBD bits).

---

## Track D — Configuration surface (`mitigations=` + reporter)

### D.1 — Host-testable `mitigations=` parser + per-vuln bug map + status vocabulary

**Files:**
- `kernel-core/src/spectre.rs` (extend C.1)

**Symbol:** `parse_mitigations(&str) -> MitigationLevel { Off, Auto, Full }`; `build_vuln_map(features, level) -> [(Vuln, Status)]`; `Status { NotAffected, Vulnerable, Mitigated(&str), Unaddressed }`
**Why it matters:** the parse + per-vuln status mapping is pure logic and belongs in host tests, with the string vocabulary modeled on Linux `/sys/devices/system/cpu/vulnerabilities/*` (`Not affected` / `Vulnerable` / `Mitigation: <name>`). Getting `auto` right (consult `RDCL_NO`) and never letting a SKIP-class become silent is the whole correctness story for the reporter.

**Acceptance:**
- [ ] Host test: `"off"`/`"auto"`/`"full"` parse to the three levels; an unknown value defaults to `Auto` and is flagged (`tests::parse_mitigations`).
- [ ] Host test: `Auto` + `rdcl_no` → Meltdown `NotAffected` and KPTI suppressed in the map; `Off` → every addressed vuln `Vulnerable`; `Full` → `Mitigated("PTI")` / `Mitigated("Retpoline, IBPB")` etc. (`tests::vuln_map_tracks_level`).
- [ ] Host test: the **UNADDRESSED** classes (MDS, L1TF, SSB/Spectre-v4, Retbleed, Downfall/GDS) are **always** present in the map as `Unaddressed`, regardless of level (no silent omission) (`tests::unaddressed_always_listed`).

### D.2 — Boot-flag plumbing gating A/C, with a single global off-switch

**Files:**
- the kernel boot path that receives the command line (UEFI load options / `BootInfo`; add a minimal `cmdline` field if absent)
- the boot mitigation selector (consumes C.1/D.1), read by A.1/A.6/C.2/C.3

**Symbol:** a single boot-populated `MitigationState` snapshot; `mitigations_off()` global check
**Why it matters:** `mitigations=off` must consistently and **early** disable KPTI (A), skip IBRS/IBPB writes (C), and report `Vulnerable` — not leave a half-applied state. The classic Linux bug (`cpu_select_mitigations`) is a per-track selector that checks its own flag but forgets the global `cpu_mitigations_off()`; every m3OS selector must consult the one global switch. Retpoline (B) is compile-time and **cannot** be runtime-disabled — D.3 must report that honestly.

**Acceptance:**
- [ ] A single `MitigationState` snapshot is populated once at boot from `parse_mitigations` + C.1 features and drives A.1 (KPTI on/off), A.6 (RDCL_NO auto-skip), C.2 (IBRS), and C.3 (IBPB); no track re-parses the flag independently.
- [ ] Flipping `mitigations=off` leaves **no** track half-applied — a regression boots with `off` and asserts KPTI is inactive, no SPEC_CTRL/PRED_CMD writes occur, and the reporter says `Vulnerable` for the addressed vulns.
- [ ] **Net-new surface (verified):** m3OS has **no** kernel boot-cmdline today — only per-process `/proc/<pid>/cmdline` (procfs), and `BootInfo` carries no cmdline field — so adding the cmdline source (a `BootInfo.cmdline` from UEFI load options, or a build-time default) is a named, first-class part of D.2's effort, not a contingency.

### D.3 — `m3ctl mitigations status` reporter

**File:** `userspace/m3ctl/src/` (a new `mitigations status` subcommand)
**Symbol:** the `mitigations status` subcommand reading the boot snapshot via a syscall/procfs surface
**Why it matters:** the reporter reads the **boot-populated** `MitigationState` snapshot (D.2), **not** a re-`rdmsr` of the per-core write-mostly `SPEC_CTRL` MSR (which is not a reliable "is it active" signal). It mirrors Linux's per-vuln vocabulary and — crucially for an honest learning OS — enumerates the UNADDRESSED classes and prints the Grimsdal caveat.

**Acceptance:**
- [ ] `m3ctl mitigations status` prints, per vuln, one of `Mitigation: <name>` / `Not affected` / `Vulnerable` / `UNADDRESSED`, exactly tracking the boot flag (D.1 map): `off` → addressed vulns `Vulnerable`; `full` → `Mitigation: ...`.
- [ ] Retpoline is reported as `compiled-in (cannot disable at boot)` (B.1), distinct from the runtime-gated KPTI/IBRS lines.
- [ ] The output enumerates the UNADDRESSED classes (MDS, L1TF, SSB, Retbleed, Downfall/GDS) and includes a one-line **Grimsdal caveat** that ring-3 driver isolation does not by itself mitigate Spectre between userspace components; the reporter reads the snapshot, never the MSR.

---

## Track E — Validation gates + release closeout

### E.1 — Meltdown PoC + smoke gate (the load-bearing proof)

**Files:**
- a new userspace PoC binary (e.g. `userspace/tests/meltdown-poc/` — four-place wiring per AGENTS.md)
- `xtask/src/main.rs` (`cmd_spectre_smoke`, a `DeviceSet`/serial+exit gate)
- `AGENTS.md` (a new opt-in gate row)

**Symbol:** the PoC binary + `cmd_spectre_smoke`
**Why it matters:** a stack-switch-only "PTI" (Redox's abandoned `pti.rs`) compiles, boots, and passes every ordinary smoke test while still leaking kernel memory — so only a PoC that **actually attempts a kernel read** proves Track A works. QEMU TCG does **not** model out-of-order speculation, so the leak is unobservable there; the gate is bare-metal/VFIO-validated with an explicit QEMU **skip-with-reason** (the Phase 79/80/82 precedent for QEMU-blind hardware), and the smoke gate asserts at minimum that the PoC's privileged read **faults** under `mitigations=full`.

**Acceptance:**
- [ ] A userspace Meltdown PoC attempts to read a known kernel sentinel; under `mitigations=full` the read **faults** (and on real out-of-order silicon, the Flush+Reload recovery reads zero / the wrong byte), proving the kernel is not mapped in the user CR3.
- [ ] The gate **skips-with-reason** on QEMU TCG (speculation not modeled) and is recorded as **bare-metal/VFIO-validated**, mirroring the `wifi-smoke`/`ahci` hot-plug skip pattern; a `M3OS_SPECTRE_REGRESSION=1` opt-in row is added to the AGENTS.md gate table.
- [ ] Under `mitigations=off` the PoC's kernel read **succeeds** on vulnerable silicon (the negative control proving the gate actually discriminates) — documented in the runbook even if only manually exercised.

### E.2 — Perf-regression gate (≤30%, gated on PCID)

**File:** `xtask/src/main.rs` (a syscall-throughput micro-benchmark step)
**Symbol:** the perf-smoke step
**Why it matters:** the design doc's acceptance is "≤30% slower with `mitigations=full` vs `off`," which is only realistic with PCID (A.5); without it the KPTI double-CR3-flush per syscall is far worse. The gate must state the PCID dependency, not assert an unconditional bound.

**Acceptance:**
- [ ] A syscall-throughput micro-benchmark runs under `mitigations=full` and `mitigations=off`; with **PCID active** the full/off ratio is within 30%.
- [ ] The gate documents that without PCID the overhead is higher (the bound is explicitly conditioned on PCID), so a slow result on a non-PCID CPU is not a false failure.

### E.3 — Bump kernel crate `0.83.0` → `0.84.0`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.84.0"`
**Why it matters:** the `0.NN.0 = Phase NN` convention (0.83.0 = Phase 83) requires the Phase 84 cut to land as `0.84.0`; mirrors Phase 82 Track F (`0.81.0`→`0.82.0`) and Phase 83 Track D.1 (`0.82.0`→`0.83.0`).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` reads `0.84.0`; `cargo xtask check` builds clean and the boot banner / procfs / `uname` (which read `env!("CARGO_PKG_VERSION")`) report `0.84.0`.
- [ ] No reference bumps the kernel crate to `1.0.0` (the Phase 83 phase-tracked-`0.NN.0` posture is unchanged).

### E.4 — Create the Phase 84 learning doc + index it

**Files:**
- `docs/84-spectre-mitigations.md` (new)
- `docs/README.md` (the `### Phase-Aligned Learning Docs` table)

**Symbol:** a learning doc following the shape of `docs/82-ahci-sata.md` / `docs/83-release-1-0-gate.md`; a new `[Spectre / KPTI Mitigations](./84-spectre-mitigations.md) | 84 | …` row
**Why it matters:** every phase ships a learning doc (the roadmap "Required Documentation for Every Phase" rule). This one teaches *why* speculation breaks the ring-0-isolation assumption (Meltdown), how KPTI/retpoline/IBRS each defeat a specific transient-execution threat, and the honest limits (timing channels, the UNADDRESSED classes).

**Acceptance:**
- [ ] `docs/84-spectre-mitigations.md` exists and follows the learning-doc structure used by `docs/82`/`docs/83` (Status/Source Ref/Depends on/Builds on/Primary Components header, Milestone Goal, Why This Phase Exists, Learning Goals, Important Components and How They Work, How Real OS Implementations Differ, Deferred Until Later), explaining KPTI/retpoline/IBRS in learner-friendly terms and citing the KAISER/Meltdown/Spectre papers.
- [ ] `docs/README.md`'s `### Phase-Aligned Learning Docs` table has a Phase-84 row linking the new doc.
- [ ] The learning doc links the design doc, this task doc, and the `docs/security/spectre-mitigations.md` operator reference (E.5).

### E.5 — Create the `docs/security/` operator reference

**File:** `docs/security/spectre-mitigations.md` (new directory + file)
**Symbol:** the operator/security reference named in the design doc's Acceptance Criteria
**Why it matters:** the design doc's acceptance criteria require a doc describing **which silicon families are protected by which mitigation and which residual risks remain**. This is the operator-facing companion to the learner-facing E.4 doc, and the canonical home for the honesty framing (RDCL_NO/eIBRS behavior, the UNADDRESSED list, the seL4 timing-channel caveat).

**Acceptance:**
- [ ] `docs/security/spectre-mitigations.md` exists with a per-mitigation silicon matrix: KPTI = Meltdown only (auto-skipped on `RDCL_NO`); retpoline = Spectre-v2 BTI, compile-time-unconditional; IBRS/eIBRS (legacy toggle vs set-once); IBPB on cross-process switch; STIBP opt-in.
- [ ] A residual-risk section lists MDS / L1TF / SSB / Retbleed / Downfall-GDS as **UNADDRESSED**, and states m3OS makes no claim of freedom from microarchitectural **timing** channels (seL4 verification-scope framing), with the Grimsdal caveat on microkernel driver isolation.
- [ ] The doc is linked from E.4 (learning doc) and the design doc's Acceptance Criteria reference.

### E.6 — README row flip + AGENTS.md gate row + check-list note

**Files:**
- `docs/roadmap/README.md` (the Phase 84 row)
- `AGENTS.md` (the opt-in regression-gate table + the kernel version line + check-list)

**Symbol:** the Phase 84 Tasks cell; a `spectre-smoke` gate row; the `0.84.0` version line
**Why it matters:** the roadmap README is the authoritative phase index, and AGENTS.md is the always-loaded inventory. The Tasks-cell flip is the only safe edit to make in the **authoring** PR (the row Status stays `Planned` until implementation lands); the AGENTS.md gate row + version bump land with the **implementation** PR.

**Acceptance:**
- [ ] *(landed in this authoring PR)* the Phase 84 README row's Tasks cell links `./tasks/84-spectre-mitigations-tasks.md` (was "Deferred until implementation planning"); Status stays `Planned`.
- [ ] *(on implementation landing)* the row Status flips `Planned` → `Complete` (kernel `0.84.0`); AGENTS.md gains a `spectre-smoke` / `M3OS_SPECTRE_REGRESSION=1` gate-table row, its kernel version reads `0.84.0`, and the `kernel-core` host-test list note covers the new `spectre` module.
- [ ] *(on implementation landing)* an AGENTS.md capability bullet is added **only** if KPTI/Spectre hardening is judged a new capability class; per the maintenance policy it is a hardening layer on the existing CPU-hardening bullet → version bump only, no new bullet.

### E.7 — Design-doc reconciliation *(landed in this authoring PR)*

**File:** `docs/roadmap/84-spectre-mitigations.md`
**Symbol:** the Primary Components paths, the Track A/B/C scope prose, the Learning Goals / How-Real-OS-Differ sections, the Companion Task List link
**Why it matters:** the design doc was written before grounding and carries factual errors this task list corrects; reconciling it in the same PR (the Phase 83 D.6 pattern) keeps the design doc and task doc from contradicting each other on day one.

**Acceptance:**
- [x] *(landed in this authoring PR)* **Primary Components** paths corrected: `kernel/src/mm/page_table.rs` → `kernel/src/mm/mod.rs` (`new_process_page_table`) + `mm/paging.rs`; `kernel/src/arch/x86_64/cpu.rs` → `kernel/src/arch/x86_64/cpuid.rs` + `arch/x86_64/mod.rs`.
- [x] *(landed in this authoring PR)* **Track B** corrected: the codegen flag is `-Zretpoline` (+ the existing `-Zbuild-std`), **not** `-C target-feature=+retpoline-*`; the external thunk is the single `__x86_indirect_thunk_r11` (no per-register family, no linker rewrite); the `objdump` verifier greps `jmp *` as well as `call *`.
- [x] *(landed in this authoring PR)* **Track C** corrected: `CPUID.07H.0:EDX[26]` enumerates **both** IBRS and IBPB, `[27]` is STIBP (the design doc's "IBPB via EDX[27]" is wrong); the eIBRS (`IA32_ARCH_CAPABILITIES.IBRS_ALL`) set-once branch and the `RDCL_NO` auto-skip are added; the `prctl` reference is reframed as an m3OS-native control.
- [x] *(landed in this authoring PR)* the "one trampoline page mapped in both PML4s" claim is corrected to the **minimal set** (trampoline text + IDT + GDT/TSS + per-CPU entry stack), KPTI is stated to defend **Meltdown only**, and the GLOBAL-bit-removal requirement is added.
- [x] *(landed in this authoring PR)* **How Real OS Implementations Differ** is enriched: Redox as a cautionary tale (PTI feature-off, unmap commented out, zero retpoline/IBRS), the seL4 timing-channel verification-scope caveat, and the Grimsdal microkernel-isolation finding; the **Companion Task List** links this task doc.
- [x] *(landed in this authoring PR — from the adversarial review)* design ↔ task **sub-ID alignment**: Track A renumbered so **A.4 = GLOBAL-bit guard** and **A.5 = PCID/INVPCID** in both docs; design-doc Track C expanded to **C.1–C.4** and Track D to **D.1–D.3** to match this task doc's granularity; the `GLOBAL` bit reframed as a **guard** (m3OS sets no kernel-PTE `GLOBAL`/`CR4.PGE` today, verified) rather than a removal; the boot flag noted as a **net-new** surface (no kernel `/proc/cmdline`); and the Phase 74a citation corrected from a non-verbatim quote to the actual row-9 grade ("HIGH for SMEP+SMAP; deferrable for KPTI"). This task doc's own repo citations were corrected too: `UserReturnState.cr3_phys` (not `Task.cr3_phys`).

---

## Documentation Notes

- **What changed relative to the design doc.** This task list settles the design doc's technical errors (verified against rustc/LLVM, the Intel SDM CPUID/MSR encodings, the Linux PTI/Spectre docs, and the real m3OS source) and the **design-doc + README reconciliation landed in this same authoring PR** (E.6/E.7): wrong file paths fixed, the retpoline flag corrected to `-Zretpoline`+`-Zbuild-std`, the SPEC_CTRL CPUID bit corrected (`EDX[26]`=IBRS+IBPB), the trampoline "one page" claim corrected to a minimal set, and the eIBRS / `RDCL_NO` / GLOBAL-bit requirements added.
- **Pure logic lives in `kernel-core`.** All CPUID/MSR/`mitigations=` decode (C.1, D.1) is host-tested in `kernel-core::spectre` exactly like `kernel_core::storage`, so a bit-transcription slip is a failing `cargo xtask check`, not a silent `#GP` or an unprotected boot.
- **The Meltdown-PoC gate is the integrity invariant.** A stack-switch-only KPTI passes every other gate while still leaking kernel memory (Redox's exact trap), so **E.1** — a PoC that actually attempts a kernel read — is the single most important review check for the implementation phase; it is bare-metal-validated because QEMU TCG does not model speculation.
- **Honesty over breadth.** KPTI = Meltdown only; retpoline+IBRS = Spectre-v2; everything else (MDS/L1TF/SSB/Retbleed/Downfall) is reported `UNADDRESSED`; timing channels are out of scope (seL4 framing); ring-3 driver isolation ≠ Spectre mitigation between userspace components (Grimsdal). The reporter (D.3) must never let a deferred class read as covered.
- **Prefer exact targets.** Reference exact files (`kernel/src/mm/mod.rs::new_process_page_table`), exact MSRs (`IA32_SPEC_CTRL=0x48`, `IA32_PRED_CMD=0x49`, `IA32_ARCH_CAPABILITIES=0x10A`), exact CPUID leaves (`CPUID.07H.0:EDX[26/27/29/31]`), and exact flags (`-Zretpoline`, `-Zbuild-std`) over directories or "the codegen flag".

## Authoring Record

- **Authored ahead of implementation (post-1.0):** this task list is the implementation contract for a future Phase 84 PR; all Track A–D acceptance items are intentionally **unchecked**. Only the E.6 README Tasks-cell flip and the E.7 design-doc reconciliation were performed in the authoring PR.
- **Research-grounded:** Redox (`pti.rs` feature-off/commented-out, `alternative.rs`/`KcpuFeatures`, zero retpoline), Linux (`entry_SYSCALL_64`/`SWITCH_TO_*_CR3`, `cpu_entry_area`, `x86_spec_ctrl_base`, `cpu_select_mitigations`, PCID/INVPCID), seL4 (timing-channel verification scope), Grimsdal et al. NordSec 2019 (microkernel ≠ Spectre-immune), the KAISER/Meltdown/Spectre papers, and an empirical rustc-nightly check confirming `-Zretpoline`/`-Zbuild-std` are required and the external thunk is the single `__x86_indirect_thunk_r11`.
- **Repo-grounded:** real symbols verified in `kernel/src/mm/mod.rs` (`new_process_page_table` clones PML4[1..512]; `restore_kernel_cr3` at :169), `kernel/src/arch/x86_64/cpuid.rs` (`probe/enable_smep_smap`, `cr4_smep_enabled`, the `CPUID.0:EAX >= 7` max-leaf guard), `kernel/src/arch/x86_64/syscall/mod.rs` (`syscall_entry` global_asm, no CR3 switch + no `swapgs` today), `kernel/src/task/mod.rs` (`UserReturnState.cr3_phys`, **not** a `Task` field), `xtask/src/main.rs` (kernel build already passes `-Zbuild-std`), `userspace/m3ctl` (extended by D.3), and `kernel/Cargo.toml` (`0.83.0` → `0.84.0`).
- **Adversarially reviewed:** a four-lens subagent review (template-conformance, technical-accuracy, completeness-vs-research, repo-grounding) ran against the authored docs. Template conformance **PASS**; the other three **PASS_WITH_FIXES** — the four blockers (design↔task A.4/A.5 transposition, Track C/D sub-ID drift, the non-existent `Task.cr3_phys`, and the non-existent `PageTableFlags::GLOBAL` site) plus the high-value accuracy notes (max-leaf guard, eIBRS≠STIBP, paranoid-path-≠-`restore_kernel_cr3`, no-swapgs per-core-page requirement, net-new cmdline surface) were all folded back in before commit.
