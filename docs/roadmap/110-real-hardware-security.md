# Phase 110 - Real-Hardware Security Hardening

**Status:** In progress — **Track C (argon2id) + Track B.1/B.2 (ASLR, stack canaries) landed + green** (RFC 9106 argon2id + BLAKE2b host-tested, passwd/adduser/login write argon2id, `verify_password` fallback + login re-hash, seeded images argon2id, `argon2-smoke` PASS). Tracks A (KPTI), B (ASLR/canaries/CET), D (Secure Boot) planned — the KPTI/CET/Secure-Boot arms are bare-metal-validation-gated.
**Source Ref:** phase-110
**Depends on:** Phase 84 (KPTI scaffolding + Spectre mitigations) ✅, Phase 48 (security foundation) ✅, Phase 10 (Secure Boot) ✅
**Builds on:** Activates the Phase 84 Track A KPTI CR3-trampoline (designed + host-tested as a PML4-pair model, with `KPTI_WIRED = false` so it has **never** actually switched CR3 on a kernel/user transition), hardens the Phase 27/48 password path, and validates the Phase 10 Secure Boot chain on real silicon — retiring the long-stale Phase 59 Track J item.
**Primary Components:** `kernel/src/mitigations.rs` (`KPTI_WIRED`, `MitigationState.kpti_active`), `kernel/src/mm/mod.rs` (`new_process_page_table` → PML4-pair split; `count_global_kernel_leaf_ptes` guard), `kernel/src/arch/x86_64/syscall/mod.rs` (`syscall_entry` CR3 trampoline) + `kernel/src/arch/x86_64/interrupts.rs` (IRQ/IST CR3 symmetry), `kernel/src/mm/elf.rs` (`map_segment`/`load_bias`/`ELF_STACK_TOP` randomization via `kernel_core::csprng::global_fill`), `x86_64-m3os.json` + new `kernel/src/arch/x86_64/cet.rs` (stack canaries + CET shadow stacks), `userspace/crypto-lib` (new `argon2.rs`) consumed by `userspace/syscall-lib/src/sha256.rs::verify_password` + `userspace/passwd/src/lib.rs` (`HASH_FORMAT_PREFIX`) + `userspace/lib/shadow`, `xtask/src/main.rs` (`sign_efi`, `generate_seeded_shadow_line`)

## Milestone Goal

Bring m3OS's security posture up to **daily-driver standard** now that there is a real reference machine (the Dell Tiger Lake laptop) storing real user data to validate against. After this phase: kernel and user address spaces are genuinely isolated by an **activated KPTI CR3 trampoline** (not the inert scaffold Phase 84 left), new processes load at **randomized bases** with **stack canaries** (and **CET shadow stacks** where the silicon supports CET), passwords hash with **argon2id** (with a fallback read path so old `$sha256i$` entries still verify), and the **Secure Boot** chain is formally validated and recorded on metal via `cargo xtask image --sign` + firmware MOK enrollment. `m3ctl mitigations status` reports the full v1/v2 + KPTI posture honestly.

## Why This Phase Exists

Running on real silicon as a workstation — storing real credentials and user files — is exactly when these mitigations stop being academic. The Phase 74a pre-1.0 audit graded "No Spectre/SMEP/SMAP/KPTI mitigations on real silicon" and Phase 98's re-charter audit re-confirmed the gaps that survived to the GUI-workstation arc:

- **KPTI is scaffolded but never activated.** Phase 84 landed the host-tested CPUID/MSR decode (`kernel_core::spectre`), eIBRS, retpoline codegen, the `mitigations=off|auto|full` policy, the `count_global_kernel_leaf_ptes` GLOBAL-bit guard, and the `MitigationState` reporter — but `kernel/src/mitigations.rs` carries `const KPTI_WIRED: bool = false`, so `kpti_active` is always `false` and `new_process_page_table` still clones the kernel PML4 into every process. The CR3-trampoline activation was *explicitly* deferred (Phase 84 Track A) pending bare-metal validation, because QEMU TCG models no speculation and so cannot prove or disprove Meltdown isolation. There is now hardware to validate against.
- **ASLR and stack canaries / CET are absent.** `kernel/src/mm/elf.rs` loads every PIE at a fixed bias (`INTERP_LOAD_BASE_HINT = 0x4000_0000`) and the user stack at a fixed `ELF_STACK_TOP`; the userspace target (`x86_64-m3os.json`) compiles with no `-Z stack-protector`; and CET (which Tiger Lake supports) is entirely unused. The CSPRNG to drive randomization already exists (Phase 86a `kernel_core::csprng`).
- **Passwords are not bcrypt/argon2.** `userspace/passwd/src/lib.rs` + `userspace/syscall-lib/src/sha256.rs` store `$sha256i$10000$<salt>$<hash>` — fixed-cost iterated SHA-256, no memory-hardness, weak against GPU/ASIC cracking of a stolen `/etc/shadow`.
- **Secure Boot was never HW-validated.** Phase 10 shipped `cargo xtask sign` / `image --sign` (`sbsign`) and was marked `Complete`, but Phase 59 Track J left the **real-hardware boot with a project-signed EFI binary** conditionally deferred on hardware availability (prior-audit #14). The hardware now exists.

## Learning Goals

- Understand how a **PML4-pair KPTI** splits each process's single page table into a kernel CR3 (full map) and a user CR3 (user pages plus a *minimal entry set* — trampoline text, IDT, GDT/TSS, per-CPU entry stack), and why the first instructions after `SYSCALL`/an IRQ must run entirely inside that minimal set before the CR3 switch.
- See why activating KPTI is a *validation* problem, not just a coding one: QEMU TCG cannot model the speculative out-of-order execution Meltdown exploits, so correctness-of-isolation can only be proven on real silicon — the canonical case for the Phase 98 bare-metal validation protocol.
- Learn how **ASLR** is sourced from a kernel CSPRNG at `execve` time (randomizing PIE load bias, mmap base, and stack top) and how **stack canaries** (`-Z stack-protector`) and **CET shadow stacks** (CR4.CET + `IA32_*_CET`/`IA32_PL3_SSP`) are complementary control-flow-integrity defenses — one compiler-emitted, one hardware-enforced.
- Understand why **argon2id** (memory-hard, side-channel-resistant hybrid) is the modern password-hashing answer where iterated SHA-256 is not, and how a shadow format migrates safely: write the new prefix, keep a fallback verify path for the old one, and re-hash on next successful login.
- See how UEFI Secure Boot is validated end-to-end on a real firmware: project key → `sbsign` the EFI binary → enroll the certificate as a MOK (Machine Owner Key) → boot with Secure Boot **enabled** and confirm the firmware accepts the signature.

## Feature Scope

### Track A — Activate + bare-metal-validate KPTI (Meltdown)

Turn the Phase 84 Track A scaffold into a live isolation boundary. Split `kernel/src/mm/mod.rs::new_process_page_table` (which today clones the kernel's PML4[1..512] into every process) into a **PML4 pair**: a kernel PML4 with the full map, and a user PML4 carrying only PML4[0] (user pages) plus the minimal entry set. Add the CR3 switch to `syscall_entry` (kernel CR3 first, before any kernel-stack/global access; user CR3 before `sysretq`) and the symmetric switch on the IRQ/IST paths in `interrupts.rs` (NMI/`#DF` paranoid save-and-restore of the entry CR3). Flip `KPTI_WIRED` to `true` so `MitigationState.kpti_active` becomes the real enforce state, with the existing `mitigations=auto` + `RDCL_NO` auto-skip and the `count_global_kernel_leaf_ptes() == 0` guard preserved. PCID/INVPCID to recover the TLB-flush cost is in scope as a follow-on within the track. **Validate on the 8-core Dell** that it boots and runs with the trampoline active (the multi-core requirement is why Phase 99 SMP robustness gates this), then run a Meltdown PoC that must **fail** to read kernel memory — the bare-metal-only proof.

### Track B — Userspace ASLR + stack canaries + CET shadow stacks

- **ASLR.** Randomize the PIE/`ET_DYN` load bias, the mmap base, and the stack top in `kernel/src/mm/elf.rs` using `kernel_core::csprng::global_fill` (the Phase 86a CSPRNG), with a bounded entropy budget that keeps mappings inside the canonical user range and clear of each other. Observable across runs: the same binary loads at different addresses each `execve`.
- **Stack canaries.** Add `-Z stack-protector=strong` (or `all`) to the userspace build (`x86_64-m3os.json` / the `build_userspace` flags), provide the `__stack_chk_guard` / `__stack_chk_fail` runtime symbols (canary seeded from the CSPRNG at process start), and prove a deliberate stack-smash traps rather than returning into corrupted state.
- **CET shadow stacks.** Where the CPU advertises CET (Tiger Lake does), enable user shadow stacks via a new `kernel/src/arch/x86_64/cet.rs` (CPUID detect mirroring `probe_smep_smap`/`probe_pku`, CR4.CET, `IA32_U_CET`/`IA32_PL3_SSP`, shadow-stack page allocation, signal-frame SSP save/restore). Graceful no-op on silicon without CET (QEMU TCG, older parts).

### Track C — argon2id password hashing

Add an argon2id implementation (new `userspace/crypto-lib/src/argon2.rs`, host-tested against RFC 9106 / reference test vectors) and a new canonical shadow prefix `$argon2id$v=19$m=…,t=…,p=…$<salt>$<hash>`. `passwd` and `adduser` write the new format; `userspace/syscall-lib/src/sha256.rs::verify_password` gains an `$argon2id$` arm **ahead of** the existing `$sha256i$` / legacy `$sha256$` arms (fallback read path — old entries keep verifying), and a successful login against an old-format entry triggers a transparent re-hash to argon2id. The `xtask` host-side seeded-shadow regenerator (`generate_seeded_shadow_line`) is updated to emit argon2id so seeded images match.

### Track D — Secure Boot on-metal validation + Phase 59 closeout

Formally validate the Phase 10 chain on the laptop: `cargo xtask image --sign` (project key/cert via `sign_efi` → `sbsign`), enroll the certificate as a MOK in the firmware, boot with Secure Boot **enabled**, and confirm the firmware accepts the signed `bootx64.efi` and m3OS reaches login. Record the run per the bare-metal validation convention and **close Phase 59 Track J** (and the Phase 10 C.3 deferral), retiring prior-audit #14.

## Important Components and How They Work

### `kernel/src/mitigations.rs` — the activation switch + reporter

`MitigationState` already carries `kpti_policy` (what the level implies) and `kpti_active` (what is actually enforcing); `init_bsp` computes `kpti_active = kpti_policy && KPTI_WIRED`. Today `KPTI_WIRED = false`, so the reporter honestly shows Meltdown `Vulnerable` (or `Not affected` on `RDCL_NO`). Track A flips `KPTI_WIRED` to `true` and wires the actual CR3-pair enable on the `kpti_policy` path, so `m3ctl mitigations status` (via `SYS_MITIGATIONS_STATUS`) reports `Mitigation: PTI` once active. The `count_global_kernel_leaf_ptes() == 0` `assert_eq!` (a release-build hard assert) stays as the GLOBAL-bit guard — a future `CR4.PGE` optimization that introduced global kernel PTEs would silently defeat isolation, and this fires instead.

### `kernel/src/mm/mod.rs::new_process_page_table` — the PML4 pair

The current function clones the kernel's PML4[1..512] into every process and deep-copies PML4[0]'s PDPT/PD chain so the ELF loader can add user mappings privately. KPTI keeps that as the **kernel** PML4 and builds a second **user** PML4 that maps PML4[0] (user) plus only the minimal entry set (trampoline text + IDT + GDT/TSS + per-CPU entry stack), with kernel `.text`/heap/direct-map absent in the user half. The pair is selected by CR3 on every ring transition.

### `kernel/src/mm/elf.rs` — ASLR randomization

`map_segment` applies a `load_bias` to each `PT_LOAD`; today the interpreter bias is the fixed `INTERP_LOAD_BASE_HINT` and the stack is the fixed `ELF_STACK_TOP`. ASLR draws a per-`execve` random bias for the PIE base, mmap base, and stack top from `kernel_core::csprng::global_fill`, masked to page granularity and a bounded range. The W^X reject in `map_segment` (Phase 75) is unchanged.

### `userspace/crypto-lib/src/argon2.rs` + `verify_password` — the hash migration

A new `no_std` argon2id (RFC 9106) in `crypto-lib`, host-tested against reference vectors. `verify_password` dispatches on prefix: `$argon2id$…` (new) → argon2id verify; `$sha256i$…` / `$sha256$…` (existing) → the current iterated/legacy SHA-256 paths, kept verbatim so seeded images and pre-migration entries still authenticate. The constant-time comparison discipline is preserved. New writes (`passwd`, `adduser`, the `xtask` regenerator) emit only argon2id.

### Secure Boot signing (`xtask` `sign_efi`)

`sign_efi` already shells `sbsign --key … --cert … bootx64.efi`; `cmd_image` wires it under `--sign`. Track D adds no new code path — it exercises the existing one on real firmware (MOK enrollment + Secure-Boot-enabled boot) and records the result, which is the deliverable Phase 10 C.3 / Phase 59 Track J left open.

## How This Builds on Earlier Phases

- **Activates Phase 84 Track A.** Everything Phase 84 host-tested (the policy parser, eIBRS, retpoline, the GLOBAL guard, the reporter, `mitigations-status-smoke`) stays; this phase only flips `KPTI_WIRED` and lands the CR3 trampoline + PML4 pair that the scaffold was built to receive.
- **Reuses the Phase 86a CSPRNG** (`kernel_core::csprng::global_fill`/`global_ready`) — the same DRBG behind `getrandom(2)` — as the entropy source for ASLR bias, stack-canary seeds, and CET shadow-stack tokens.
- **Extends the Phase 75 W^X / Phase 90a PKU posture** rather than replacing it: KPTI + ASLR + canaries/CET are orthogonal layers, all surfaced through the existing `m3ctl mitigations status` reporter.
- **Validates Phase 10 Secure Boot** on the Phase 96/100 bare-metal Dell platform, using the Phase 98 bare-metal validation protocol and capture toolkit.
- **Gated by Phase 99 (SMP robustness).** KPTI's CR3 trampoline runs on every core and the laptop is 8-core (it cannot pin `-smp 1` the way the toolchain gates do), so the SMP block/wake + TLB-shootdown hardening must land first.

## Implementation Outline

1. **Track A** — build the PML4 pair in `new_process_page_table`; add the CR3 switch to `syscall_entry` (kernel CR3 first, scratch register + trampoline stack) and the IRQ/IST symmetry + paranoid NMI/`#DF` save-restore in `interrupts.rs`; flip `KPTI_WIRED`; preserve the GLOBAL-guard assert; add PCID/INVPCID to recover TLB cost; QEMU-boot for functional correctness, then bare-metal-validate boot + Meltdown-PoC reject on the Dell.
2. **Track B** — ASLR bias/mmap/stack randomization in `elf.rs` from the CSPRNG; `-Z stack-protector` + `__stack_chk_guard`/`__stack_chk_fail` in the userspace build; new `cet.rs` (CPUID detect, CR4.CET, `IA32_*_CET`/`IA32_PL3_SSP`, shadow-stack pages, signal-frame SSP); add the always-on `aslr-smoke` (two runs, different bases) and `stack-smash-smoke` (deliberate smash → trap) gates; bare-metal-validate CET on the Dell.
3. **Track C** — `crypto-lib::argon2id` (host tests vs RFC 9106 vectors); the `$argon2id$` prefix + `verify_password` fallback arm + transparent re-hash on legacy-format login; update `passwd`/`adduser`/`generate_seeded_shadow_line`; add the `argon2-smoke` gate (old `$sha256i$` entry still verifies; new entry round-trips; re-hash observed).
4. **Track D** — `cargo xtask image --sign` on the laptop, firmware MOK enrollment, Secure-Boot-enabled boot, record per the bare-metal protocol; flip Phase 10 C.3 and close Phase 59 Track J.
5. Update `m3ctl mitigations status` expectations; extend `mitigations-status-smoke` to assert `kpti(... active=true)` under `mitigations=full` once activated; bump kernel version per the Phase 98 Track C unified-version policy.

## Acceptance Criteria

Per the Phase 98 Track A.5 **bare-metal validation strategy** (`docs/appendix/bare-metal-validation.md`): the HW-only deliverables (KPTI Meltdown-PoC reject, CET shadow stacks, Secure Boot on metal) carry the **`Validated-on-HW (run N, YYYY-MM-DD)`** status with a recorded evidence pointer — never a bare "Complete" — while the CI-able deliverables (ASLR observability, stack-canary trap, argon2id host tests, `mitigations-status-smoke`) carry standard passing-gate evidence. QEMU models none of speculative execution, CET, or Secure-Boot firmware, so those arms are skip-with-reason in CI.

- **KPTI active:** with `mitigations=full` the Dell boots and runs the full smoke surface with the CR3 trampoline live (PML4 pair selected on every kernel↔user transition); `m3ctl mitigations status` reports `kpti(policy=true active=true)` and Meltdown as `Mitigation: PTI`; the `count_global_kernel_leaf_ptes() == 0` guard holds. **Validated-on-HW (run N, date)** — Dell Tiger Lake; evidence: captured serial `[sec] mitigations=… kpti(... active=true)` + `m3ctl` output.
- **Meltdown PoC fails:** the ported public Meltdown reference exploit cannot read kernel memory under `mitigations=full` (it still leaks with KPTI off, proving the PoC is real). **Validated-on-HW (run N, date)**; skip-with-reason under QEMU TCG (no speculation model). Under `mitigations=auto` KPTI is skipped on `RDCL_NO` silicon.
- **ASLR observable:** `aslr-smoke` boots the same PIE twice and asserts the load base / stack top differ across runs (CI-able under QEMU); mappings stay inside the canonical user range and the W^X reject is unaffected.
- **Stack canary + CET:** `stack-smash-smoke` proves a deliberate stack overwrite traps (`__stack_chk_fail`, process killed) rather than returning into corrupted state (CI-able); `objdump` confirms canary prologues/epilogues are emitted in the userspace binaries. CET user shadow stacks are enabled where CPUID advertises CET and a shadow-stack mismatch faults — **Validated-on-HW (run N, date)** on the Dell; clean no-op (no fault, no enable) on non-CET silicon.
- **argon2id:** new `passwd`/`adduser` writes use `$argon2id$…`; `verify_password` still authenticates a pre-migration `$sha256i$` entry and transparently re-hashes it on successful login; `argon2-smoke` and `crypto-lib` host tests (RFC 9106 vectors) pass.
- **Secure Boot on metal:** `cargo xtask image --sign` produces a `sbsign`-signed `bootx64.efi`; with the project cert enrolled as a MOK and Secure Boot **enabled**, the Dell firmware accepts it and m3OS reaches login. **Validated-on-HW (run N, date)**; the Phase 10 C.3 and Phase 59 Track J items are flipped to closed/validated with the recorded run as evidence (prior-audit #14 retired).

## Companion Task List

- [Phase 110 Task List](./tasks/110-real-hardware-security-tasks.md)

## How Real OS Implementations Differ

- **Linux KPTI** is a paired 8 KiB PGD (two adjacent 4 KiB halves selected by `PTI_USER_PGTABLE_BIT`); the user half clones only `cpu_entry_area` via `pti_clone_entry_text`, and `SWITCH_TO_KERNEL_CR3`/`SWITCH_TO_USER_CR3` bracket `entry_SYSCALL_64`. m3OS uses an explicit second PML4 frame and a smaller minimal entry set; PCID recovers the TLB cost on Westmere+ exactly as Linux does. **Redox** (the nearest Rust microkernel) ships its `pti.rs` gated off `default` with the kernel-unmap commented out — m3OS Phase 110 lands strictly ahead of Redox's shipped x86 hardening.
- **Linux/glibc ASLR** randomizes far more (vDSO, brk, the dynamic loader, per-mmap with `mmap_rnd_bits` entropy) and ships full PIE; m3OS randomizes the PIE base, mmap base, and stack with a bounded budget — coarser, but observably non-deterministic.
- **Production password hashing** uses libargon2/`crypt(3)` with tuned `m`/`t`/`p` and per-deployment cost calibration; m3OS ships fixed conservative argon2id parameters and a one-way migration from the legacy SHA-256 format.
- **Real Secure Boot** chains through shim + a vendor/Microsoft-signed first stage and often a TPM-measured boot; m3OS uses a self-owned project key enrolled as a MOK — sufficient to prove the firmware honors the signature, but not a measured-boot/attestation chain.

## Deferred Until Later

- **PCID-less fallback tuning** and per-workload KPTI cost profiling beyond the 30 %-bound check.
- **Kernel-side ASLR (KASLR)** and randomized direct-map base — only userspace ASLR is in scope here.
- **CET indirect-branch tracking (IBT / `endbr64`)** — only shadow stacks are in scope; IBT is a follow-on CFI layer.
- **Measured boot / TPM attestation**, shim integration, and a full PKI beyond the single project MOK (Phase 98 accepted-deferred security backlog).
- **The wider Spectre family** (Retbleed/SRSO/MDS/L1TF/BHI/Downfall) and microarchitectural timing channels — explicitly out of scope, consistent with Phase 84.
- **MAC/SELinux-class policy** and full credential lifecycle (password aging, lockout) — accepted-deferred per Phase 98.
