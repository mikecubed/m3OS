# Phase 110 — Real-Hardware Security Hardening: Task List

**Status:** Planned
**Source Ref:** phase-110
**Depends on:** Phase 84 (KPTI scaffolding + Spectre mitigations) ✅, Phase 48 (security foundation) ✅, Phase 10 (Secure Boot) ✅, Phase 99 (SMP & Scheduler Robustness — KPTI runs on every core, the laptop is 8-core) ✅, Phase 86a (CSPRNG — ASLR/canary entropy) ✅, Phase 98 (bare-metal validation strategy) ✅
**Goal:** Activate + bare-metal-validate the Phase 84 KPTI CR3-trampoline (Meltdown), add userspace ASLR + stack canaries + CET shadow stacks, migrate password hashing to argon2id with a fallback verify path, and formally validate + record Secure Boot on the Dell Tiger Lake laptop — retiring the stale Phase 59 Track J / prior-audit #14 item. HW-only deliverables follow the `docs/appendix/bare-metal-validation.md` protocol and carry `Validated-on-HW (run N, date)`; CI-able deliverables carry passing-gate evidence.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Activate + bare-metal-validate KPTI (PML4 pair, CR3 trampoline, `KPTI_WIRED`, PCID, Meltdown PoC) | Phase 84 ✅, Phase 99 ✅ | Planned |
| B | Userspace ASLR + stack canaries + CET shadow stacks | Phase 86a ✅, A (shares the mm/exec path) | Planned |
| C | argon2id password hashing migration (fallback read path + re-hash) | Phase 48 ✅ | Planned |
| D | Secure Boot on-metal validation + Phase 59 Track J / Phase 10 C.3 closeout | Phase 10 ✅, A (validated boot platform) | Planned |

---

## Track A — Activate + Bare-Metal-Validate KPTI

### A.1 — PML4 pair in `new_process_page_table`

**File:** `kernel/src/mm/mod.rs`
**Symbol:** `new_process_page_table`
**Why it matters:** Today this clones the kernel PML4[1..512] into **every** process PML4, so kernel mappings live in the user CR3 — the exact arrangement Meltdown exploits. KPTI requires a second "user" PML4 carrying only PML4[0] (user pages) plus the minimal entry set, with kernel `.text`/heap/direct-map absent in the user half.

**Acceptance:**
- [ ] `new_process_page_table` returns a kernel/user PML4 **pair** (kernel = full map; user = PML4[0] + minimal entry set), each tracked on the process `AddressSpace`.
- [ ] The user PML4 maps exactly the minimal entry set (trampoline text, IDT, GDT/TSS, per-CPU entry stack) and **no** kernel `.text`/heap/direct-map entries (verified by a walk asserting PML4[256..512] empty in the user half).
- [ ] Host/`kernel-core` test (or a boot-time self-check) asserts the user half contains no kernel upper-half leaf PTE.

### A.2 — Syscall-entry CR3 trampoline

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `syscall_entry` (entry/exit asm)
**Why it matters:** On `SYSCALL` the CPU is still on the user CR3; the first instructions must switch to the kernel CR3 using only a scratch register and a trampoline stack mapped in the user PML4, before any kernel-stack or global access, and switch back before `sysretq`. This is the load-bearing ~200 LOC of asm.

**Acceptance:**
- [ ] `syscall_entry` switches CR3 to the kernel PML4 first (scratch register + trampoline stack only) and restores the user PML4 before `sysretq`.
- [ ] The existing `SFMASK` flag-masking and argument-register contract are preserved across the rewrite (all syscall smoke gates still pass under QEMU).
- [ ] No kernel global / kernel-stack access occurs before the CR3 switch (audited; trampoline stack lives in the user PML4's minimal set).

### A.3 — IRQ / IST CR3 symmetry

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** the IRQ entry/exit stubs + NMI/`#DF` IST handlers
**Why it matters:** Hardware interrupts arriving in user mode are on the user CR3 and must do the same switch; NMI/`#DF`/IST vectors can interrupt **either** address space, so they must save-and-restore the entry CR3 (paranoid path).

**Acceptance:**
- [ ] Every maskable IRQ entry switches to the kernel CR3 and restores the entry CR3 on exit.
- [ ] NMI and `#DF` (IST) handlers save the entry CR3 on entry and restore it on exit (correct whether they interrupted ring 0 or ring 3).
- [ ] `kstack-overflow-smoke` and the existing fault-recovery gates still pass (the `#DF` path is unaffected functionally).

### A.4 — Flip `KPTI_WIRED` + activate on the policy path

**File:** `kernel/src/mitigations.rs`
**Symbol:** `KPTI_WIRED`, `MitigationState.kpti_active`, `init_bsp`
**Why it matters:** `kpti_active = kpti_policy && KPTI_WIRED` and `KPTI_WIRED = false` today, so the reporter honestly shows Meltdown `Vulnerable`. Activation must flip the flag **and** perform the real enable on the `kpti_policy` path so `kpti_active` reflects enforcement.

**Acceptance:**
- [ ] `KPTI_WIRED = true`; `init_bsp` performs the CR3-pair enable when `kpti_policy` is set and reports `kpti_active = true` (and `false` under `mitigations=off` or `auto` + `RDCL_NO`).
- [ ] The `count_global_kernel_leaf_ptes() == 0` `assert_eq!` guard (Track A.4 of Phase 84) is preserved and still holds (no GLOBAL kernel leaf survives the CR3 switch).
- [ ] `m3ctl mitigations status` reports `kpti(policy=… active=true)` and Meltdown as `Mitigation: PTI`; `mitigations-status-smoke` is extended to assert `active=true` under `mitigations=full`.

### A.5 — PCID / INVPCID TLB-cost recovery

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`, `kernel/src/mm/mod.rs`, `kernel/src/smp/tlb.rs`
**Symbol:** the CR3-write sites + `restore_kernel_cr3` + the SMP shootdown path
**Why it matters:** A naive KPTI flushes the whole TLB on every CR3 switch (~30 % syscall overhead); PCID-tagged CR3 loads avoid the flush (~5 %). The SMP shootdown must flush **both** the kernel and user PCID of the target ASID.

**Acceptance:**
- [ ] CR3 loads on the trampoline carry distinct kernel/user PCIDs (gated on `CPUID` PCID + `INVPCID` support; plain full-flush fallback when absent).
- [ ] The SMP TLB-shootdown path invalidates both PCIDs of the target ASID (no stale cross-core translation).
- [ ] Under QEMU + `mitigations=full`, the smoke suite is at most 30 % slower than `mitigations=off` **when PCID is active** (the Phase 84 bound).

### A.6 — Bare-metal KPTI boot + Meltdown-PoC validation

**Files:**
- new: `kernel/initrd/meltdown-poc` (or `userspace/meltdown-poc`) — the ported public reference exploit
- `scripts/security-validate.md` (new — generalize `scripts/ure-vfio-validate.md`, results appendix)

**Symbol:** the Meltdown PoC binary + the recorded run
**Why it matters:** QEMU TCG models no speculation, so isolation correctness can only be proven on real silicon — the canonical Phase 98 bare-metal case. The PoC must leak with KPTI off (proving it is real) and fail with KPTI on.

**Acceptance:**
- [ ] The 8-core Dell boots and runs the full smoke surface with `mitigations=full` and the CR3 trampoline live; captured serial shows `[sec] mitigations=… kpti(policy=true active=true)`. **Validated-on-HW (run N, YYYY-MM-DD)** — Dell Tiger Lake; evidence: serial capture + `m3ctl mitigations status` output in `scripts/security-validate.md`.
- [ ] The Meltdown PoC reads kernel memory with KPTI **off** and fails to read it with KPTI **on**. **Validated-on-HW (run N, date)**; skip-with-reason under QEMU TCG (documented in the gate).
- [ ] Under `mitigations=auto`, KPTI is skipped on `RDCL_NO` silicon (confirmed against the reporter output for the test CPU).

---

## Track B — Userspace ASLR + Stack Canaries + CET Shadow Stacks

### B.1 — ASLR: randomize PIE base / mmap base / stack top

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `map_segment` / `load_bias`, `INTERP_LOAD_BASE_HINT`, `ELF_STACK_TOP`, the mmap base allocator
**Why it matters:** Every PIE loads at the fixed `INTERP_LOAD_BASE_HINT = 0x4000_0000` bias and the stack at the fixed `ELF_STACK_TOP` today, so addresses are fully predictable. ASLR draws a per-`execve` random bias from `kernel_core::csprng::global_fill` (Phase 86a).

**Acceptance:**
- [ ] PIE/`ET_DYN` load bias, mmap base, and stack top are each offset by a CSPRNG-drawn, page-aligned random value within a bounded budget.
- [ ] Randomized mappings stay inside the canonical user range, never overlap, and the Phase 75 W^X reject in `map_segment` is unchanged.
- [ ] `aslr-smoke` (new, CI-able under QEMU) boots the same PIE twice and asserts the observed load base / stack top differ across runs.
- [ ] When the CSPRNG is not yet `global_ready()`, load falls back to the fixed bias (boot never blocks on entropy).

### B.2 — Stack canaries (`-Z stack-protector` + runtime symbols)

**Files:**
- `x86_64-m3os.json`
- `xtask/src/main.rs` (`build_userspace` flags)
- new: `userspace/syscall-lib/src/stack_protector.rs` (`__stack_chk_guard`, `__stack_chk_fail`)

**Symbol:** `__stack_chk_guard`, `__stack_chk_fail`
**Why it matters:** The userspace target compiles with no stack protector, so a stack overwrite returns into corrupted control flow undetected. Canaries are compiler-emitted CFI; m3OS must supply the guard symbol (seeded from the CSPRNG at process start) and a `__stack_chk_fail` that aborts.

**Acceptance:**
- [ ] `-Z stack-protector=strong` (or `all`) is set for the userspace build; `objdump -d` of a representative binary shows canary load/compare prologue+epilogue sequences.
- [ ] `__stack_chk_guard` is seeded from `getrandom`/CSPRNG at process start; `__stack_chk_fail` terminates the process (no return into corrupted state).
- [ ] `stack-smash-smoke` (new, CI-able under QEMU): a binary that deliberately overwrites its canary is killed via `__stack_chk_fail` rather than completing.

### B.3 — CET shadow stacks

**Files:**
- new: `kernel/src/arch/x86_64/cet.rs`
- `kernel/src/arch/x86_64/cpuid.rs` (CET feature probe), `kernel/src/signal.rs` (SSP save/restore)

**Symbol:** `probe_cet` / `enable_user_cet` (mirroring `probe_smep_smap`/`probe_pku`), `IA32_U_CET`, `IA32_PL3_SSP`, CR4.CET
**Why it matters:** Tiger Lake supports CET; shadow stacks are a hardware control-flow-integrity layer that catches ROP/return-address overwrite even where a canary is bypassed. Must be a clean no-op on silicon without CET (QEMU TCG, older parts).

**Acceptance:**
- [ ] `probe_cet` detects CET via CPUID (guarded by the leaf-7 max-leaf check, like `probe_smep_smap`); `enable_user_cet` sets CR4.CET + `IA32_U_CET` shadow-stack-enable and allocates a per-task shadow-stack page only when supported.
- [ ] A shadow-stack mismatch (forged return address) faults; the signal-frame path saves/restores `IA32_PL3_SSP` so signal delivery does not corrupt the shadow stack.
- [ ] On non-CET silicon (QEMU TCG), `enable_user_cet` is a no-op: no CR4.CET write, no fault, normal boot. **Validated-on-HW (run N, date)** for the active path on the Dell; CI proves the no-op path.
- [ ] `m3ctl mitigations status` reports the CET posture (enabled / not-supported).

---

## Track C — argon2id Password Hashing

### C.1 — argon2id implementation + host tests

**File:** new: `userspace/crypto-lib/src/argon2.rs` (+ `userspace/crypto-lib/src/lib.rs` export)
**Symbol:** `argon2id_hash`, `argon2id_verify`
**Why it matters:** Iterated SHA-256 has no memory-hardness; a stolen `/etc/shadow` is cheap to crack on GPU/ASIC. argon2id (RFC 9106) is the modern memory-hard, side-channel-resistant answer. Must be `no_std` and host-testable.

**Acceptance:**
- [ ] `argon2id_hash`/`argon2id_verify` implement RFC 9106 argon2id with fixed conservative `m`/`t`/`p` parameters.
- [ ] Host tests in `crypto-lib` pass against the RFC 9106 reference test vectors (added to `cargo xtask check`).
- [ ] Verify uses constant-time comparison (no early-out on mismatch).

### C.2 — `$argon2id$` shadow prefix + `verify_password` fallback arm

**File:** `userspace/syscall-lib/src/sha256.rs`
**Symbol:** `verify_password`
**Why it matters:** `verify_password` currently dispatches on `$sha256i$` / legacy `$sha256$`. An `$argon2id$` arm must be added **ahead** of those, while the existing arms stay verbatim so seeded images and every pre-migration entry keep authenticating (the fallback read path).

**Acceptance:**
- [ ] `verify_password` accepts `$argon2id$v=19$m=…,t=…,p=…$<salt>$<hash>` via `crypto-lib::argon2id_verify`.
- [ ] A pre-existing `$sha256i$10000$…` entry and a legacy `$sha256$…` entry still verify (the existing arms are unchanged; host tests assert all three formats).
- [ ] A malformed `$argon2id$` field returns `false` (fail-closed), not a panic.

### C.3 — New writes use argon2id + transparent re-hash on legacy login

**Files:**
- `userspace/passwd/src/lib.rs` (`HASH_FORMAT_PREFIX`, `build_hash_field`)
- `userspace/passwd/src/main.rs`, `userspace/adduser` (write path)
- `userspace/lib/shadow/src/lib.rs` (atomic write — reused unchanged)

**Symbol:** `HASH_FORMAT_PREFIX`, `build_hash_field`, the login re-hash hook
**Why it matters:** `passwd`/`adduser` must emit the new format, and a successful login against an old-format entry should upgrade it in place so the population migrates without an admin sweep — using the existing atomic `shadow_write_atomic` so a torn write never corrupts `/etc/shadow`.

**Acceptance:**
- [ ] `HASH_FORMAT_PREFIX` / `build_hash_field` emit `$argon2id$…`; new `passwd` and `adduser` writes use argon2id exclusively.
- [ ] On a successful login/`su` against a `$sha256i$`/`$sha256$` entry, the shadow line is re-hashed to argon2id via `shadow::shadow_write_atomic` (atomic; original preserved on any write failure).
- [ ] Existing `passwd`/`shadow` host tests pass (the rewrite/atomic-write state machines are unchanged).

### C.4 — Update the seeded-shadow regenerator + argon2-smoke

**Files:**
- `xtask/src/main.rs` (`generate_seeded_shadow_line`)
- new: `cmd_argon2_smoke` in `xtask/src/main.rs`

**Symbol:** `generate_seeded_shadow_line`, `cmd_argon2_smoke`
**Why it matters:** The host-side regenerator must match the in-guest format byte-for-byte (it currently mirrors `$sha256i$`), or seeded images and the running OS diverge. The gate proves the migration end-to-end.

**Acceptance:**
- [ ] `generate_seeded_shadow_line` emits `$argon2id$…` matching `crypto-lib::argon2id_hash` byte-for-byte (host test asserts parity).
- [ ] `argon2-smoke` (new, CI-able): boots, logs in against an argon2id-seeded user, logs in against a `$sha256i$`-seeded user, and asserts the legacy user's shadow line was re-hashed to `$argon2id$` after login.
- [ ] `M3OS_ARGON2_REGRESSION` row added to the `AGENTS.md` gate table (per the Phase 98 Track D slimmed format).

---

## Track D — Secure Boot On-Metal Validation + Phase 59 Closeout

### D.1 — Sign + enroll + Secure-Boot-enabled boot on the Dell

**Files:**
- `xtask/src/main.rs` (`sign_efi`, `cmd_image` `--sign`) — exercised, not changed
- `scripts/security-validate.md` (Secure Boot results appendix)

**Symbol:** `sign_efi`, `cmd_image`, the recorded MOK-enrollment + boot run
**Why it matters:** Phase 10 shipped the signing path but it was never run on real firmware (Phase 59 Track J / prior-audit #14). This validates the chain end-to-end: project key → `sbsign` → MOK enrollment → Secure-Boot-enabled boot.

**Acceptance:**
- [ ] `cargo xtask image --sign --key <k> --cert <c>` produces a `sbsign`-signed `efi/boot/bootx64.efi` (verified with `sbverify` against the project cert).
- [ ] The project certificate is enrolled as a MOK in the Dell firmware; with Secure Boot **enabled** the firmware accepts the signed binary and m3OS reaches login. **Validated-on-HW (run N, YYYY-MM-DD)** — Dell Tiger Lake; evidence: photo of the firmware Secure-Boot-enabled state + captured boot-to-login serial in `scripts/security-validate.md`.
- [ ] A negative check is recorded: an **unsigned** image is **rejected** by the firmware with Secure Boot enabled (proving the check is live, not bypassed).

### D.2 — Close Phase 59 Track J + Phase 10 C.3 + retire prior-audit #14

**Files:**
- `docs/roadmap/59-validation-backlog.md` (Track J)
- `docs/roadmap/10-secure-boot.md` (C.3 deferral note)
- `docs/roadmap/README.md` (status rows)

**Symbol:** the Phase 59 Track J entry, the Phase 10 C.3 item
**Why it matters:** The whole point of the Phase 98 audit was to retire stale "deferred-on-hardware" items once hardware exists; leaving them open after a recorded validation re-creates the claim-vs-validated drift.

**Acceptance:**
- [ ] Phase 59 Track J is flipped from deferred to closed, citing the D.1 recorded run as evidence.
- [ ] Phase 10 C.3 (real-hardware Secure Boot) is marked `Validated-on-HW (run N, date)` with the same evidence pointer.
- [ ] The README status rows for Phase 10 / 59 / 110 reflect the validated state; prior-audit #14 is noted retired in the Phase 98 verdict matrix (`docs/appendix/audit-status/`).

---

## Documentation Notes

- This phase **activates** Phase 84 Track A; everything Phase 84 host-tested (policy parser, eIBRS, retpoline, the GLOBAL guard, `mitigations-status-smoke`) is unchanged — record that the only logical change to the mitigations substrate is flipping `KPTI_WIRED` and landing the CR3-pair enable on the existing `kpti_policy` path.
- KPTI Meltdown-reject, CET shadow stacks, and Secure Boot are **HW-only** — they carry `Validated-on-HW (run N, date)` per `docs/appendix/bare-metal-validation.md`, never a bare "Complete". ASLR observability, the stack-canary trap, argon2id host tests, and `mitigations-status-smoke` are CI-able and carry standard passing-gate evidence.
- The argon2id migration is one-way with a fallback **read** path: `verify_password` keeps the `$sha256i$`/`$sha256$` arms verbatim so old entries authenticate; only new writes and post-login re-hashes produce `$argon2id$`. Keep the host-side `generate_seeded_shadow_line` in lockstep with `crypto-lib::argon2id_hash` (the same drift hazard Phase 66 flagged for `$sha256i$`).
- `scripts/security-validate.md` is the new per-phase runbook (generalized from `scripts/ure-vfio-validate.md`); keep it driver-agnostic so future security HW validations reuse it.
- Prefer exact files/symbols over directories as these land; update this list's checkboxes and the `Validated-on-HW (run N, date)` strings as each track completes.
