# Phase 84 — Spectre / KPTI / Retpoline / IBRS Mitigations (Learning Doc)

**Status:** Spectre-v2 layer implemented (kernel 0.84.0); KPTI (Meltdown) **activation** is a bare-metal-validated follow-up
**Source Ref:** phase-84
**Depends on:** Phase 75 (W^X Enforcement), Phase 77 (Pre-1.0 Correctness — SMEP + SMAP baseline), Phase 83 (Release 1.0 Gate)
**Builds on:** the Phase 77 SMEP + SMAP baseline — those two CR4 bit flips are the cheap class of CPU mitigations; this phase adds the expensive class: KPTI (Kernel Page Table Isolation) for Meltdown, retpoline for Spectre-v2 branch-target injection, and the `IA32_SPEC_CTRL` MSR family (IBRS/eIBRS/IBPB/STIBP), each targeting a distinct transient-execution threat.
**Primary Components:** `kernel/src/mm/mod.rs` (`new_process_page_table` — split the per-process PML4 into a kernel/user pair) + `kernel/src/mm/paging.rs`, `kernel/src/arch/x86_64/syscall/mod.rs` (`syscall_entry` — PTI CR3 trampoline) + `kernel/src/arch/x86_64/interrupts.rs` (IRQ/IST symmetry), `xtask/src/main.rs` (kernel build flags — `-Zretpoline` on the existing `-Zbuild-std`), `kernel/src/arch/x86_64/cpuid.rs` (CPUID feature detect + `IA32_SPEC_CTRL`/IBRS — mirrors the Phase 77 `probe_smep_smap`/`enable_smep_smap` pattern), `kernel-core/src/spectre.rs` (host-tested CPUID/MSR/`mitigations=` decode)

## Milestone Goal

m3OS implements the post-Meltdown / post-Spectre-v2 mitigations that mature OSes shipped between 2018 and 2020. Indirect branches in kernel code are retpoline-protected; the `IA32_SPEC_CTRL` MSR family (IBRS/eIBRS/IBPB/STIBP) is applied per each CPU's capabilities; and kernel/user address-space isolation (KPTI, for Meltdown) is **designed and scaffolded** — all behind a `mitigations=off|auto|full` boot policy with an honest `m3ctl mitigations status` reporter. This is an explicitly post-1.0 phase because the work is large (~2000 LOC) and the 1.0 cohort (Phase 77's SMEP + SMAP) already captures the cheap class of mitigations.

> ## Implementation status (be precise — this is a learning OS)
>
> **Landed and validated (kernel 0.84.0):**
> - **Retpoline (Spectre-v2 BTI):** active and verified — the linked kernel ELF has **zero** residual indirect branches and ~1900 `__llvm_retpoline_r11` thunk references (a `cargo xtask check` gate enforces this).
> - **`IA32_SPEC_CTRL` family:** eIBRS set-once + IBPB-on-cross-process-switch + STIBP opt-in mechanisms, all gated on the host-tested CPUID/MSR decode (no `#GP` on CPUs lacking the bits). The *legacy* per-entry IBRS toggle shares the KPTI trampoline asm and lands with KPTI activation.
> - **`mitigations=off|auto|full` policy + `m3ctl mitigations status` reporter** — the reporter reads the boot snapshot and reports Meltdown honestly (`Vulnerable` / `Not affected` on `RDCL_NO`), **never** a false `Mitigated` while KPTI is not enforcing.
> - **KPTI scaffolding:** the host-tested user-shadow-PML4 invariant model (`kernel_core::kpti`), the GLOBAL-bit guard (A.4), the `RDCL_NO` auto-skip policy (A.6), and the per-core CR3-trampoline plumbing.
>
> **Deferred to a bare-metal-validated follow-up:** the KPTI **activation** — the CR3-trampoline rewrite of the syscall + all IRQ/IST entry paths (A.2/A.3), PCID (A.5), and the Meltdown-PoC gate (E.1). QEMU/TCG does not model speculation, so KPTI's leak-prevention property is unverifiable in QEMU by construction; its correctness proof is the bare-metal Meltdown PoC. SMP additionally requires a fixed per-CPU entry-area (`cpu_entry_area`-equivalent) so a per-process user PML4 can reach the running core's RSP0/TSS/per-core data — m3OS allocates those per-core on the heap today. See the task list's Track A for the precise activation plan.

## Why This Phase Exists

The Phase 74a pre-1.0 audit graded the absence of Spectre/KPTI mitigations as **HIGH for SMEP+SMAP; deferrable for KPTI** — silent on QEMU TCG (which does not model out-of-order speculation), exploitable on real silicon. Phase 77 landed SMEP + SMAP because they are CR4 bit flips with trivial code impact. KPTI, retpoline, and IBRS are expensive — they touch the kernel/user transition assembly and the entire indirect-branch codegen story — so they get their own phase after the 1.0 gate.

The post-1.0 placement is honest: m3OS at 1.0 is a learning microkernel, not a hardened production OS. Users running on Spectre-vulnerable silicon should know exactly what they are and are not running.

## Learning Goals

- Understand how speculation breaks ring-0 isolation: a CPU that speculatively executes a privileged load (even one that would architecturally fault) leaves the loaded value in the cache, where a cache side channel lets ring-3 code observe it — the Meltdown attack.
- See how KPTI defeats Meltdown by making kernel memory structurally unreachable from the user-mode page table, and why KPTI defends **Meltdown only** — it does nothing for Spectre, and on Meltdown-immune silicon (`IA32_ARCH_CAPABILITIES.RDCL_NO`) it should be skipped.
- Understand how retpoline defeats Spectre-v2 (branch-target injection) by replacing indirect branches with a sequence that traps speculative execution into a benign loop, and why on a Rust kernel this is a compiler codegen flag (`-Zretpoline`), not a target-feature.
- Learn IBRS, eIBRS, IBPB, and STIBP as the MSR-based complement to retpoline — each targeting a specific cross-boundary prediction attack surface — and the decode path that distinguishes legacy IBRS (toggled per kernel entry/exit) from Enhanced IBRS (set once at boot).
- See the performance tradeoffs: KPTI costs ~5–30% of syscall throughput, which PCID/INVPCID recovers; retpoline costs ~1–5% on most code.
- Understand the honesty limits: what Phase 84 covers, what it explicitly does not, and why an honest learning OS reports deferred vulnerabilities as `UNADDRESSED` rather than silently omitting them.

## Why Speculation Breaks Ring-0 Isolation

Modern CPUs execute instructions speculatively — the CPU fetches and executes instructions before it knows they are architecturally permitted. If the CPU guesses wrong (a branch misprediction, a TLB miss, a permission check still being resolved), it rolls back the architectural state — the register file, memory writes, exceptions — but does **not** roll back the cache. Data loaded transiently during speculative execution leaves a footprint in the cache that a timing-based side channel can read.

**Meltdown** (Lipp et al., 2018) exploits this in the simplest possible way. Ring-3 user code issues a load from a ring-0-only kernel address. Architecturally this raises a page fault. But transiently, before the fault is raised, the CPU speculatively executes the load and its dependent instructions, loading the secret kernel byte into a register and using its value to touch a cache line in a user-accessible region. The attacker then times accesses to that region (Flush+Reload) to recover the byte. The CPU's ring check fires and terminates the faulting path — but the cache footprint survives and the byte is already gone.

The critical insight is that the CPU's **permission checks and its speculative execution pipeline are not synchronized**. Speculation happens first; the permission check resolves later. For the window between those two events, the kernel/user boundary is invisible to the speculative engine.

**Spectre** (Kocher et al., 2018) takes the same basic observation — speculative execution leaks through the cache — and applies it to a different mechanism: the CPU's **branch predictor**. An attacker can train the indirect branch predictor (the Branch Target Buffer, BTB) by executing code that conditions it to predict a specific target address. When the victim kernel code then encounters an indirect branch, the CPU speculatively jumps to the attacker-chosen target and executes a few instructions there before realizing the branch was mispredicted. If those speculative instructions touch memory based on a secret, that secret leaks into the cache. Spectre-v2 specifically targets **indirect calls and jumps** (`call *rax`, `jmp *rax`) — the mechanism retpoline defends.

## KPTI — Kernel Page Table Isolation

### The Problem

In m3OS before Phase 84, `kernel/src/mm/mod.rs::new_process_page_table` copies the kernel's PML4 entries `[1..512]` into every new process PML4. The result: while ring-3 code runs, the kernel's text, heap, and direct-map are all present in the page table — they are marked supervisor-only and ring-3 code cannot access them *architecturally*. But Meltdown demonstrated that the CPU speculatively crosses the ring boundary, and those pages being present in the table is all it needs to speculate into them.

### The Solution

KPTI splits each process's page table into two:

- **Kernel PML4** — the full map: user pages (PML4[0]) plus kernel pages (PML4[1..511]). The CPU uses this CR3 while in ring 0.
- **User PML4** — a restricted map: user pages (PML4[0]) plus a **minimal entry set** (described below). The CPU uses this CR3 while in ring 3.

From the user-mode CR3, kernel `.text`, the heap, the direct-map, and page-table pages are **entirely absent**. The CPU cannot speculate into memory it cannot even find in the page table.

### The Minimal Entry Set

The user PML4 is not one page — it is a small, precisely chosen set of pages that ring-3 code shares with the kernel because the entry and exit paths require them to be reachable before the CR3 switch:

- **Trampoline text** — the first instructions of `syscall_entry` and the IRQ stubs execute on the user CR3 (the CPU has not switched to the kernel CR3 yet). Every byte those instructions touch before the switch must be in the user PML4.
- **IDT** — the interrupt descriptor table (hardware-accessed, not software-accessed, but must be mapped).
- **GDT and TSS** — the segment/task-state structures the CPU reads on ring transitions.
- **Per-CPU entry stack** — the stack the trampoline uses from the moment of entry until it can safely switch CR3.

This is Linux's `cpu_entry_area`. The set is a handful of pages, not one.

### The CR3 Trampoline

Every kernel entry path — SYSCALL, every IDT vector, NMI, double fault — must switch CR3 from the user PML4 to the kernel PML4 **before** touching any kernel data. The switch must use only a scratch register and the minimal-entry-set pages, because the kernel stack, globals, and per-CPU data are not yet reachable. Similarly, every exit path must switch back to the user CR3 immediately before `sysretq`/`iret`.

NMI, `#DF`, and IST-using vectors have an extra constraint: they can fire from either address space, so they must **save the entry CR3 and restore it on exit** (the "paranoid" path), not unconditionally write the kernel CR3.

### The GLOBAL Bit Guard

TLB entries marked `GLOBAL` survive a CR3 reload — a CR3 switch does not evict them. If kernel pages were marked global, KPTI would be a no-op: the kernel TLB entries would persist into userspace and Meltdown could still read through them. m3OS does not currently mark kernel PTEs `GLOBAL` or enable `CR4.PGE`; KPTI maintains and enforces that property. (With PCID active, TLB entries are tagged with an ASID rather than surviving CR3 switches unconditionally — a different, non-conflicting mechanism.)

### PCID — Recovering the Performance Cost

Without PCID, every CR3 switch (twice per syscall — entry and exit) flushes the entire TLB. On syscall-heavy workloads this is the source of the ~5–30% overhead. PCID (Process Context Identifier) tags TLB entries with a 12-bit ASID; the kernel and user halves of a process use two distinct PCIDs. Setting bit 63 of the CR3 value on switch (`NOFLUSH`) skips the full flush and instead evicts only the entries for the replaced PCID. `INVPCID` selectively invalidates a non-current PCID on unmap. Together, PCID+INVPCID recover most of the KPTI cost on Westmere-and-later silicon.

### What KPTI Does Not Cover

KPTI defends **Meltdown only**. It does nothing for Spectre. On silicon where `IA32_ARCH_CAPABILITIES.RDCL_NO` (bit 0) is set, the CPU is not susceptible to Meltdown, and under `mitigations=auto` KPTI is skipped entirely. All modern AMD and recent Intel silicon sets `RDCL_NO`.

Paper: Gruss et al., "KASLR is Dead: Long Live KASLR" (the KAISER precursor, 2017); Lipp et al., "Meltdown: Reading Kernel Memory from User Space" (USENIX Security, 2018).

## Retpoline — Spectre-v2 Indirect-Branch Hardening

### The Problem

Every indirect call or jump (`call *rax`, `jmp *rax`) in the kernel is an opportunity for Spectre-v2. The CPU's BTB is a shared, cross-process resource. An attacker in one process can condition the BTB to predict a specific kernel address at a specific kernel indirect-branch site. When the kernel executes that branch, the CPU speculatively jumps to the attacker's chosen address and executes code there — potentially leaking kernel data through a cache side channel.

### The Retpoline Sequence

Retpoline replaces `jmp *%rax` with a sequence that exploits the **Return Stack Buffer** (RSB), which predicts `ret` targets based on the call stack. The RSB has different (and harder to poison) prediction behavior than the BTB. The canonical sequence:

```asm
    call .Lcapture          ; pushes the address of .Lspec onto the RSB
.Lspec:
    pause                   ; yield the execution port during speculation
    lfence                  ; LFENCE — the actual speculation barrier
    jmp  .Lspec             ; predicted: fall into the pause/lfence loop
.Lcapture:
    mov  %rax, (%rsp)       ; overwrite the return address with the real target
    ret                     ; architecturally: jump to target; speculatively: predicted by RSB to .Lspec
```

The `lfence` is essential — `pause` alone is not a speculation barrier on AMD processors. The loop ensures that speculative execution spins harmlessly without touching any secret-dependent memory.

### Rust and the `-Zretpoline` Flag

On a Rust kernel, indirect calls arise from trait objects, function pointers, and match dispatch. The compiler lowers these to `call *reg` instructions. To replace all of them with retpoline sequences, the correct flag is:

```
-Zretpoline
```

This is a dedicated nightly Rust flag, **not** `-Ctarget-feature=+retpoline-...` (which is a target modifier and is rejected). Because retpoline changes the ABI of indirect calls, `core` must be rebuilt with the same flag — which m3OS already does via `-Zbuild-std=core,compiler_builtins,alloc`. The flag is added to the existing kernel build invocation in `xtask/src/main.rs`.

Under this flag, LLVM routes every indirect call through an internal thunk (`__llvm_retpoline_r11`). The kernel optionally provides a single hand-written external thunk (`__x86_indirect_thunk_r11`) instead. Note: this is a **single** r11-keyed thunk, not the per-register family (`rax..r15`) that GCC's `-mindirect-branch=thunk-extern` uses — LLVM's convention is different.

After build, the kernel verifies the mitigation is complete:

```bash
objdump -d kernel.elf | grep -E '\b(call|callq|jmp|jmpq)[ \t]+\*'
```

This must return zero lines. The check covers indirect JMPs (tail calls, trait dispatch lowered to a jump) as well as indirect CALLs, and runs on the fully-linked binary so rebuilt `core` is included.

### Compile-Time Unconditional

Retpoline is baked into the compiled binary. It is not a runtime toggle — the `mitigations=` boot flag cannot disable it. `m3ctl mitigations status` reports it as `compiled-in (cannot disable at boot)`.

### Retpoline Is Not Complete Spectre-v2 Coverage

Skylake-class cores can underflow the Return Stack Buffer into the poisonable BTB, so a complete defense also requires RSB stuffing on kernel entry. Additionally, Retbleed (2022) showed retpoline alone is insufficient on some silicon families, motivating `RETHUNK`/`__x86_return_thunk`. Phase 84 does not implement RSB stuffing or Retbleed; they are listed in Deferred Until Later.

Paper: Kocher et al., "Spectre Attacks: Exploiting Speculative Execution" (IEEE S&P, 2018).

## IBRS, eIBRS, IBPB, and STIBP — The `IA32_SPEC_CTRL` MSR Family

These are the microarchitectural controls exposed through the `IA32_SPEC_CTRL` MSR (`0x48`) and related MSRs. Each addresses a distinct attack surface on specific silicon families.

### Feature Detection

```
CPUID.(EAX=07H, ECX=0):EDX
  [26] — IBRS_IBPB: IBRS and IBPB both present (one combined bit)
  [27] — STIBP: Single Thread Indirect Branch Predictors
  [29] — ARCH_CAPABILITIES: IA32_ARCH_CAPABILITIES MSR present
  [31] — SSBD: Speculative Store Bypass Disable

IA32_ARCH_CAPABILITIES MSR (0x10A) — only if EDX[29] is set
  [0]  — RDCL_NO: not susceptible to Meltdown (drives the KPTI auto-skip)
  [1]  — IBRS_ALL: Enhanced IBRS (eIBRS) — set once at boot rather than per-entry
```

Note the `CPUID.0:EAX >= 7` max-basic-leaf guard: on CPUs where leaf 7 is unsupported, the CPUID instruction returns the highest supported leaf's data — whose bits 26/27/31 might accidentally read as IBRS/STIBP/SSBD on an old CPU. Every decode path must check the max leaf first.

All of this decode logic lives in host-tested `kernel-core/src/spectre.rs`, exactly as `kernel_core::storage` holds the AHCI/ATA math. A bit-transcription error becomes a failing test, not a silent `#GP` at boot.

### IBRS — Indirect Branch Restricted Speculation

`IA32_SPEC_CTRL` bit 0. Restricts speculative indirect branches from crossing ring or address-space boundaries, preventing an attacker in ring 3 from influencing kernel indirect branches. Available when `CPUID.07H.0:EDX[26]` is set.

Two modes:

- **Legacy IBRS** (`IBRS_ALL` = 0): must be toggled on every kernel entry and cleared on every kernel exit. Moderately expensive, but necessary on Skylake-derived parts.
- **Enhanced IBRS / eIBRS** (`IBRS_ALL` = 1, indicated by `IA32_ARCH_CAPABILITIES.IBRS_ALL`): set once at boot and left enabled permanently. The CPU self-manages the protection without the per-entry cost.

eIBRS covers **same-thread** cross-privilege BTI. It does not protect SMT siblings — that still requires STIBP.

### IBPB — Indirect Branch Predictor Barrier

`IA32_PRED_CMD` MSR `0x49`, bit 0. Write-only — `rdmsr` of this MSR raises `#GP`. Writing bit 0 flushes the BTB and other indirect branch prediction state. Issued on **cross-process context switches** (switching between distinct address spaces, not between threads within the same process) to prevent one process from having pre-conditioned the predictor for the next. Gated on the same `CPUID.07H.0:EDX[26]` bit as IBRS.

### STIBP — Single Thread Indirect Branch Predictors

`IA32_SPEC_CTRL` bit 1. Prevents indirect branch predictors from being influenced by code running on an SMT sibling (a second hardware thread sharing the same physical core). Default off — the performance cost is real and SMT is not universally deployed. Opt-in per-process via an m3OS-native capability surface (not Linux `prctl`, which m3OS does not have). Gated on `CPUID.07H.0:EDX[27]`.

### The `spec_ctrl_base` Convention

The kernel never blindly overwrites `IA32_SPEC_CTRL`. It maintains a cached `spec_ctrl_base` value that tracks the current base setting (IBRS + any per-process STIBP). Every write OR-s in the requested bits over the base, so no write silently clears STIBP while setting IBRS, or vice versa.

## The `mitigations=` Boot Policy

m3OS exposes a `mitigations=` boot command-line flag with three levels:

| Level | KPTI | IBRS/IBPB | Notes |
|---|---|---|---|
| `off` | Off | Off | No runtime mitigations applied; retpoline still compiled in |
| `auto` (default) | On unless `RDCL_NO` | On if IBRS present | Best performance on immune silicon, full protection on vulnerable silicon |
| `full` | On (even on `RDCL_NO`) | On if IBRS present | Forces all applicable mitigations regardless of silicon report |

Every track (KPTI, IBRS, IBPB) consults a **single global off-switch** populated once at boot. No track re-parses the flag independently. Retpoline is compile-time and does not participate in this policy.

`m3ctl mitigations status` reports the active set on the booted CPU, reading the boot-populated snapshot (not a re-`rdmsr` of the write-mostly `SPEC_CTRL` MSR). The output enumerates every vulnerability class — addressed and unaddressed — so no gap can silently read as covered.

## Important Components and How They Work

### Host-Tested Decode: `kernel-core/src/spectre.rs`

All CPUID bit extraction, `IA32_ARCH_CAPABILITIES` parsing, the eIBRS-vs-legacy classification, the `mitigations=` string parser, and the per-vulnerability status map live here. This mirrors the Phase 82 pattern of putting AHCI register/FIS math in `kernel-core::storage`: the logic is proven by `cargo xtask check` with no QEMU, so a bit-transcription error is a failing test, not a silent `#GP` at boot. The CPUID max-leaf guard (the same guard `probe_smep_smap` enforces at `cpuid.rs:241`) is exercised here too.

The status vocabulary mirrors Linux's `/sys/devices/system/cpu/vulnerabilities/*`: `Not affected`, `Vulnerable`, `Mitigation: <name>`, or `UNADDRESSED`. The UNADDRESSED classes (MDS, L1TF, SSB, Retbleed, Downfall/GDS) are always present in the map regardless of the `mitigations=` level — a deferred vulnerability cannot read as covered.

### KPTI Page-Table Pair: `kernel/src/mm/mod.rs`

`new_process_page_table` today clones kernel PML4 entries `[1..512]` into every process PML4. KPTI replaces this with a paired structure: the existing per-process PML4 becomes the **kernel CR3**, and a new **user PML4** carries only PML4[0] (user pages) plus the minimal entry set. The user-mode CR3 literally cannot reach kernel memory, so the CPU cannot speculate into it.

### CR3 Trampoline: `kernel/src/arch/x86_64/syscall/mod.rs`

The `syscall_entry` stub today switches to the kernel stack but performs **no CR3 switch** — safe only because the kernel is mapped in the user PML4. Under KPTI the switch to the kernel CR3 must be the very first substantive act, using only a scratch register and the minimal-entry-set pages. The reverse switch happens immediately before `sysretq`. This is the single most error-prone part of the phase: an out-of-order switch faults with no reachable handler.

### IBRS/eIBRS in `cpuid.rs`

New `probe_spec_ctrl()` / `enable_ibrs()` / `spec_ctrl_active()` functions beside `probe_smep_smap`/`enable_smep_smap`/`cr4_smep_enabled` from Phase 77. Every `rdmsr`/`wrmsr` of `IA32_SPEC_CTRL` is gated on the C.1 `ibrs_ibpb` feature bit — an unguarded MSR access on a CPU that lacks the bit raises `#GP`. MSR access uses the `Msr::new(0x48)` wrapper the `x86_64` crate already provides (as in `microcode.rs`).

### Performance: PCID + INVPCID

PCID (enabled in `CR4.PCIDE` when `CPUID.01H:ECX[17]` is set) assigns 12-bit ASIDs to TLB entries. The kernel and user PML4s of one process share an ASID but are distinguished by a high bit convention (the Linux `PTI_USER_PCID_BIT` pattern). Setting CR3 bit 63 (`NOFLUSH`) on a switch skips the full TLB flush. `INVPCID` (when `CPUID.07H.0:EBX[10]` is set) invalidates a non-current PCID on unmap. The SMP TLB-shootdown path must flush **both** the kernel and user PCIDs of the target process's ASID, not just the active one.

## How This Builds on Earlier Phases

- Extends the Phase 75 W^X model with KPTI — together they cover the bulk of the "code injection via memory-corruption + speculation" attack surface.
- Extends Phase 77's SMEP + SMAP with the more expensive class of CPU mitigations, reusing the same detect→enable→status pattern in `cpuid.rs`.
- Reuses the Phase 11 process model — each process already owns its PML4; this phase splits each PML4 into a pair without changing the per-process ownership model.
- Reuses the Phase 55b/67 IOMMU substrate and ring-3 driver hosting machinery unchanged — the mitigation layer is entirely in the kernel proper.

## How Real OS Implementations Differ

**Linux** is the primary reference for Track A and Track C. Linux's KPTI uses paired 8 KiB PGDs (two adjacent 4 KiB halves, selected by `PTI_USER_PGTABLE_BIT`); `SWITCH_TO_KERNEL_CR3` / `SWITCH_TO_USER_CR3` bracket `entry_SYSCALL_64`; the `cpu_entry_area` is the kernel's name for the minimal entry set. Linux's mitigations matrix has roughly 30 distinct vulnerability identifiers (Spectre-v1/v2/v2-user, RSB underflow, MDS, TAA, ITLB-Multihit, SRBDS, SRSO, Inception, BHI, GDS, ...) — m3OS at Phase 84 ships the four headline 2018-era ones. Linux also applies CPU-specific tuning, choosing IBRS vs. retpoline based on silicon microarchitecture; m3OS uses the simpler conservative rule: retpoline always, IBRS/eIBRS when available, KPTI unless `RDCL_NO`.

**Redox** (the nearest Rust microkernel) is a cautionary tale, not a model for Track A. Its `src/arch/x86_shared/pti.rs` is gated on a `pti` Cargo feature that is **absent from `default`** (marked `#TODO: remove when threading issues are fixed`). The kernel-heap PML4 unmap is commented out. The syscall trampoline's PTI calls are commented out — `// TODO: Map PTI`. The result: Redox runs with kernel mappings live in the user CR3 and performs no CR3 switch. Redox also ships zero retpoline, IBRS, IBPB, or STIBP code. m3OS Phase 84 therefore lands strictly ahead of Redox's shipped x86 hardening. Track A is sourced from Linux and the KAISER paper; Track B has no Rust-OS prior art.

**SerenityOS** shares kernel mappings into every process's page table — no KPTI. Its ring-3 driver architecture is different enough that direct comparison is not meaningful here.

**A microkernel does not get Spectre-immunity for free.** Moving NVMe, e1000, and AHCI drivers to ring 3 is not a Spectre mitigation. Grimsdal et al. (NordSec 2019) demonstrated that Flush+Reload and Spectre work across component boundaries on Genode, OKL4, and NOVA regardless of the separation model — Spectre exploits the CPU's shared prediction and cache structures, not the OS's trust boundaries. Ring-3 driver isolation does **not** mitigate Spectre between userspace components.

Additionally, mirroring **seL4**'s verified-confidentiality scope — which explicitly excludes microarchitectural timing channels, treating them as an empirical rather than formal matter — m3OS makes **no claim** of freedom from microarchitectural timing channels. The seL4 team explicitly scoped out timing channels from their formal proof of confidentiality. m3OS adopts the same honest position.

## Deferred Until Later

The following are explicitly out of scope for Phase 84 and reported as `UNADDRESSED` by `m3ctl mitigations status`:

- **L1TF / Foreshadow** (Intel SGX and OS/VMM variants)
- **MDS** (Microarchitectural Data Sampling: Zombieload, RIDL, Fallout)
- **TAA** (TSX Asynchronous Abort)
- **SSB / Spectre-v4** (Speculative Store Bypass)
- **SRSO / Inception** (Speculative Return Stack Overflow, AMD)
- **Retbleed** — RSB stuffing on kernel entry and `RETHUNK`/`__x86_return_thunk` (retpoline alone is not complete Spectre-v2 coverage on Skylake-derived and Zen 2-derived silicon)
- **BHI** (Branch History Injection)
- **Downfall / GDS** (Gather Data Sampling)
- Per-vulnerability mitigation toggles (Linux-style `nospectre_v2`, `nopti` granularity)
- Fine-grained STIBP / SMT-aware scheduling integration
- Speculative-load hardening compiler pass (LLVM SLH)
- Microarchitectural **timing** channels generally — the seL4-style time-protection problem, explicitly out of scope

## Companion Docs

- [Phase 84 design doc](./roadmap/84-spectre-mitigations.md) — the implementation contract: tracked components, acceptance criteria, feature scope, and implementation outline
- [Phase 84 task list](./roadmap/tasks/84-spectre-mitigations-tasks.md) — the per-track breakdown with exact CPUID bits, MSR addresses, and acceptance checks
- [Operator security reference](./security/spectre-mitigations.md) — per-mitigation silicon matrix, residual-risk register, and `m3ctl mitigations status` usage
