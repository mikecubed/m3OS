# m3OS Spectre / Meltdown Mitigations — Operator Reference

**Phase:** 84 (post-1.0)
**Kernel version:** 0.84.0
**Source Ref:** phase-84
**Companion learning doc:** [docs/84-spectre-mitigations.md](../84-spectre-mitigations.md)

This document is the operator-facing reference for the transient-execution mitigations introduced in Phase 84. It describes which silicon families are protected by which mitigation, how each is enabled, and — critically — which vulnerability classes remain unaddressed.

> **Implementation status (read first).** The **Spectre-v2 layer is active**: retpoline is compiled in (verified — zero residual indirect branches in the kernel ELF), and eIBRS / IBPB / STIBP are applied per the CPU's capabilities. **KPTI (the Meltdown mitigation) is designed and scaffolded but its CR3-trampoline *activation* is deferred to a bare-metal-validated follow-up** (QEMU cannot model speculation, so KPTI's leak-prevention is unverifiable there; SMP also needs a per-CPU entry-area first). Until KPTI activation lands, `m3ctl mitigations status` reports Meltdown **honestly** — `Vulnerable` on susceptible silicon (never a false `Mitigation: PTI`), or `Not affected` on `RDCL_NO` parts (all AMD + recent Intel, where Meltdown does not apply).

---

## Checking What Is Active

```
m3ctl mitigations status
```

This command reads the boot-populated `MitigationState` snapshot and prints a per-vulnerability line using the vocabulary below. It does **not** re-read the `IA32_SPEC_CTRL` MSR at runtime. Retpoline is reported separately as `compiled-in (cannot disable at boot)`.

Example output **at the current implementation stage** (Meltdown-susceptible silicon, `mitigations=auto`) — KPTI activation pending, so Meltdown is reported honestly as `Vulnerable`:

```
mitigations: level=auto
  Meltdown: Vulnerable
  Spectre-v1: UNADDRESSED
  Spectre-v2: Mitigation: Retpoline, IBPB
  MDS: UNADDRESSED
  L1TF: UNADDRESSED
  SSB (Spectre-v4): UNADDRESSED
  Retbleed: UNADDRESSED
  Downfall/GDS: UNADDRESSED
  Spectre-v2 (retpoline): compiled-in (cannot disable at boot)
note: UNADDRESSED — MDS, L1TF, SSB, Retbleed, Downfall/GDS are not mitigated.
note: ring-3 driver isolation does not by itself mitigate Spectre between userspace components (Grimsdal et al., NordSec 2019); m3OS makes no claim of freedom from microarchitectural timing channels (seL4 verification-scope framing).
```

`Meltdown: Vulnerable` because KPTI is designed + scaffolded but its activation is a bare-metal follow-up — the reporter never prints a false `Mitigation: PTI` while KPTI is not enforcing. Note that retpoline is reported on its own `Spectre-v2 (retpoline): compiled-in` line, distinct from the runtime-gated `Spectre-v2:` (IBRS/IBPB) line.

Once KPTI activation lands and `mitigations=full` (or `auto` on susceptible silicon) is in effect, the `Meltdown` line becomes `Mitigation: PTI`. On Meltdown-immune silicon (`RDCL_NO` set) the `Meltdown` line is already `Not affected` regardless of KPTI activation:

```
mitigations: level=auto
  Meltdown: Not affected
  Spectre-v1: UNADDRESSED
  Spectre-v2: Mitigation: Retpoline, IBPB
  MDS: UNADDRESSED
  L1TF: UNADDRESSED
  SSB (Spectre-v4): UNADDRESSED
  Retbleed: UNADDRESSED
  Downfall/GDS: UNADDRESSED
  Spectre-v2 (retpoline): compiled-in (cannot disable at boot)
note: UNADDRESSED — MDS, L1TF, SSB, Retbleed, Downfall/GDS are not mitigated.
note: ring-3 driver isolation does not by itself mitigate Spectre between userspace components (Grimsdal et al., NordSec 2019); m3OS makes no claim of freedom from microarchitectural timing channels (seL4 verification-scope framing).
```

(The two `note:` lines are always printed — the formatter appends them unconditionally after the retpoline line.)

---

## Configuration (Build-Time Policy)

m3OS has no kernel boot command line (`bootloader_api::BootInfo` carries none), so the `mitigations=` policy is selected at **build time**, not at boot. The level comes from the `M3OS_MITIGATIONS` environment variable (default `auto`), baked into the kernel via `option_env!`; `kernel/build.rs` re-runs the build when the value changes:

```
M3OS_MITIGATIONS=full cargo xtask run     # build + boot with mitigations=full
M3OS_MITIGATIONS=off  cargo xtask run     # build + boot with mitigations=off
```

The selected level (default `auto`) maps to the policy:

| Value | Effect |
|---|---|
| `off` | Disables all runtime mitigations (KPTI, IBRS, IBPB). Retpoline is compiled in and cannot be disabled. All addressed vulnerabilities are reported `Vulnerable`. |
| `auto` | Applies mitigations appropriate to the booted CPU. KPTI is skipped on `RDCL_NO` silicon. IBRS/IBPB applied when the CPU reports support. Default. |
| `full` | Forces all applicable mitigations on, even on `RDCL_NO` silicon (useful for testing or paranoid deployments). |

All tracks (KPTI, IBRS, IBPB) consult a single global off-switch populated once at boot. No track applies a mitigation that the global policy disables.

---

## Per-Mitigation Silicon Matrix

### KPTI — Kernel Page Table Isolation

| Item | Detail |
|---|---|
| **Vulnerability addressed** | Meltdown (CVE-2017-5754) — privileged transient-load data leak |
| **Silicon applicable** | Any x86_64 CPU that does **not** report `IA32_ARCH_CAPABILITIES.RDCL_NO` (bit 0). Affects Intel Haswell through Coffee Lake and equivalents. All modern AMD and recent Intel set `RDCL_NO` and are immune. |
| **How enabled** | `mitigations=auto` (default): enabled unless `RDCL_NO` is set. `mitigations=full`: always enabled. `mitigations=off`: always disabled. |
| **Mechanism** | Per-process page-table pair: the user-mode CR3 maps only user pages plus the minimal kernel entry set (trampoline text, IDT, GDT/TSS, per-CPU entry stack). Kernel `.text`, heap, and direct-map are absent from the user PML4. A CR3 trampoline on every `syscall`/IRQ/IST entry-exit path performs the switch. |
| **Performance** | ~5–30% syscall throughput reduction without PCID. PCID+INVPCID (Westmere+, present on all modern Intel) recovers most of the overhead. |
| **What it does NOT cover** | Spectre (any variant). KPTI removes kernel mappings from the user-mode page table; it does not restrict branch prediction. |

### Retpoline — Spectre-v2 BTI (Indirect-Branch Hardening)

| Item | Detail |
|---|---|
| **Vulnerability addressed** | Spectre-v2 / Branch Target Injection (CVE-2017-5715) |
| **Silicon applicable** | All x86_64 silicon (compile-time unconditional). |
| **How enabled** | Baked into the kernel binary at compile time via `rustc -Zretpoline` on the `-Zbuild-std` kernel build. **Cannot be disabled at boot.** |
| **Mechanism** | Every indirect `call *reg` / `jmp *reg` in the kernel (including rebuilt `core`) is replaced with a retpoline sequence that routes speculative execution into a `pause; lfence` spin, making the mispredicted path harmless. Verified post-build: `objdump -d kernel.elf | grep -E '\b(call\|callq\|jmp\|jmpq)[ \t]+\*'` must return zero lines. |
| **Performance** | ~1–5% on most workloads. Higher on indirect-branch-heavy code. |
| **What it does NOT cover** | RSB underflow (Skylake); Retbleed / SRSO (requires `RETHUNK`/`__x86_return_thunk`, deferred). Retpoline is **not** complete Spectre-v2 coverage on all silicon. |

### IBRS / eIBRS — Indirect Branch Restricted Speculation

| Item | Detail |
|---|---|
| **Vulnerability addressed** | Spectre-v2 BTI, same-thread cross-privilege variant |
| **Silicon applicable** | CPUs reporting `CPUID.(EAX=07H,ECX=0):EDX[26]` (IBRS+IBPB present). Skylake-derived Intel; AMD Zen 2+ with `IBRS_ALL`; some older AMD via legacy IBRS. |
| **How enabled** | Enabled under `mitigations=auto` and `mitigations=full` when `EDX[26]` is set. Disabled by `mitigations=off`. |
| **Mechanism (legacy IBRS)** | `IA32_SPEC_CTRL` (MSR `0x48`) bit 0 toggled on every kernel entry (set) and exit (clear). Available when `EDX[26]` is set but `IA32_ARCH_CAPABILITIES.IBRS_ALL` is clear. |
| **Mechanism (eIBRS / Enhanced IBRS)** | `IA32_SPEC_CTRL` bit 0 set **once at boot** and left enabled. Indicated by `IA32_ARCH_CAPABILITIES.IBRS_ALL` (MSR `0x10A` bit 1). Present on most Intel Ice Lake+, Tiger Lake+, and AMD Zen 3+. No per-entry toggle cost. |
| **Important limit** | eIBRS covers same-thread cross-privilege BTI only. **It does not protect SMT siblings.** STIBP is still required for full SMT isolation even on eIBRS silicon. |
| **Performance** | Legacy IBRS: moderate per-entry overhead. eIBRS: negligible (set-once). |

### IBPB — Indirect Branch Predictor Barrier

| Item | Detail |
|---|---|
| **Vulnerability addressed** | Cross-process Spectre-v2 BTI (one process pre-poisoning the BTB for the next) |
| **Silicon applicable** | CPUs reporting `CPUID.(EAX=07H,ECX=0):EDX[26]`. Same bit as IBRS. |
| **How enabled** | Issued automatically by the kernel on every cross-address-space `switch_context`. No user-visible control required. Disabled by `mitigations=off`. |
| **Mechanism** | Write `1` to `IA32_PRED_CMD` (MSR `0x49`, write-only). Flushes the BTB and related indirect branch prediction state. Issued only on switches between **distinct address spaces**; thread switches within a single process do not issue IBPB. |
| **Note** | `IA32_PRED_CMD` is write-only — any attempt to `rdmsr` it raises `#GP`. |

### STIBP — Single Thread Indirect Branch Predictors

| Item | Detail |
|---|---|
| **Vulnerability addressed** | Spectre-v2 BTI from an SMT sibling thread |
| **Silicon applicable** | CPUs reporting `CPUID.(EAX=07H,ECX=0):EDX[27]`. Relevant only on systems with SMT/Hyper-Threading enabled. |
| **How enabled** | **Default off.** Opt-in per-process via an m3OS-native capability surface. Not a system-wide toggle. |
| **Mechanism** | `IA32_SPEC_CTRL` (MSR `0x48`) bit 1. Prevents the SMT sibling's indirect branch predictor from influencing this thread. Composes with the `spec_ctrl_base` cache so setting STIBP never clears IBRS. |
| **Performance** | Real cost; varies by workload and CPU. Left opt-in to avoid penalizing all processes for an SMT risk that only some face. |

---

## CPUID and MSR Quick Reference

| Feature | CPUID bit | MSR | Notes |
|---|---|---|---|
| IBRS + IBPB present | `CPUID.07H.0:EDX[26]` | — | One combined bit for both features |
| STIBP present | `CPUID.07H.0:EDX[27]` | — | |
| `IA32_ARCH_CAPABILITIES` present | `CPUID.07H.0:EDX[29]` | — | Guard before reading MSR 0x10A |
| SSBD present | `CPUID.07H.0:EDX[31]` | — | Not used in Phase 84 |
| PCID support | `CPUID.01H:ECX[17]` | — | Required for KPTI NOFLUSH optimization |
| INVPCID support | `CPUID.07H.0:EBX[10]` | — | Required for selective PCID invalidation |
| RDCL_NO (Meltdown immune) | via `ARCH_CAPABILITIES` | `IA32_ARCH_CAPABILITIES` (0x10A) bit 0 | Drives KPTI auto-skip |
| IBRS_ALL / eIBRS | via `ARCH_CAPABILITIES` | `IA32_ARCH_CAPABILITIES` (0x10A) bit 1 | Drives set-once vs. per-entry IBRS |
| IBRS / eIBRS control | — | `IA32_SPEC_CTRL` (0x48) bit 0 | Read/write |
| STIBP control | — | `IA32_SPEC_CTRL` (0x48) bit 1 | Read/write |
| IBPB flush | — | `IA32_PRED_CMD` (0x49) bit 0 | **Write-only** |

---

## Residual Risk Register

The following vulnerability classes are **UNADDRESSED** in Phase 84. `m3ctl mitigations status` always reports them explicitly — they are never silently omitted.

| Vulnerability | Status | Notes |
|---|---|---|
| **MDS** (Microarchitectural Data Sampling: Zombieload, RIDL, Fallout) | UNADDRESSED | Requires `MD_CLEAR` VERW flushing on ring transition; not implemented |
| **L1TF / Foreshadow** (CVE-2018-3615/3620/3646) | UNADDRESSED | Requires L1D flush on context switch or VMX entry; not implemented |
| **SSB / Spectre-v4** (Speculative Store Bypass) | UNADDRESSED | Would require `SPEC_CTRL.SSBD`; not implemented |
| **Retbleed / SRSO** (RSB/RET-based Spectre v2 variants, AMD Inception) | UNADDRESSED | Requires `RETHUNK`/`__x86_return_thunk` and RSB stuffing; retpoline alone is insufficient on affected silicon |
| **BHI** (Branch History Injection) | UNADDRESSED | — |
| **Downfall / GDS** (Gather Data Sampling, CVE-2022-40982) | UNADDRESSED | Requires microcode update + kernel coordination |
| **TAA** (TSX Asynchronous Abort) | UNADDRESSED | — |

### Timing-Channel Disclaimer

m3OS makes **no claim** of freedom from microarchitectural **timing channels** — cache timing, memory-bus contention, DRAM row-hammer side channels, or similar. This mirrors seL4's verified-confidentiality scope, which explicitly excludes microarchitectural timing channels from its formal proof of confidentiality, treating them as an empirical rather than formal matter.

### Microkernel Isolation Does Not Confer Spectre Immunity

Moving NVMe, e1000, and AHCI drivers to ring 3 does not prevent Spectre attacks between those drivers or between a driver and the kernel. Grimsdal et al. (NordSec 2019) demonstrated that Flush+Reload and Spectre work across component boundaries on Genode, OKL4, and NOVA regardless of the separation model. Spectre exploits the CPU's shared prediction and cache structures — OS trust boundaries are invisible to the microarchitecture. **Ring-3 driver isolation does not by itself mitigate Spectre between userspace components.**

---

## Deferred Mitigations (Future Phases)

- RSB stuffing on kernel entry (`FILL_RETURN_BUFFER`)
- `RETHUNK` / `__x86_return_thunk` for Retbleed and SRSO
- `MD_CLEAR` VERW flushing for MDS
- L1D flush for L1TF
- `SPEC_CTRL.SSBD` for Spectre-v4 / SSB
- Per-vulnerability mitigation toggles (Linux-style `nospectre_v2`, `nopti`)
- Fine-grained STIBP + SMT-aware scheduler integration
- Speculative-load hardening compiler pass (LLVM SLH)
- Microarchitectural timing-channel defenses

---

## References

- Lipp et al., "Meltdown: Reading Kernel Memory from User Space," USENIX Security 2018
- Kocher et al., "Spectre Attacks: Exploiting Speculative Execution," IEEE S&P 2018
- Gruss et al., "KASLR is Dead: Long Live KASLR" (KAISER precursor), ESSoS 2017
- Grimsdal et al., "Spectre is Here to Stay: An Analysis of Side-Channels and Spectre," NordSec 2019
- Intel SDM Vol. 3A, §10.3: IA32_SPEC_CTRL, IA32_PRED_CMD, IA32_ARCH_CAPABILITIES
- Linux kernel documentation: `Documentation/admin-guide/hw-vuln/`
- seL4 Verified Confidentiality: https://sel4.systems/Info/Docs/seL4-white-paper.pdf (timing-channel scope note)
