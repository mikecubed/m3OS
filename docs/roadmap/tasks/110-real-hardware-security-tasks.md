# Phase 110 — Real-Hardware Security Hardening: Task List

**Status:** Planned
**Source Ref:** phase-110
**Depends on:** Phase 84 (KPTI scaffolding + Spectre mitigations) ✅, Phase 48 (security foundation) ✅, Phase 10 (Secure Boot) ✅, Phase 99 (SMP & Scheduler Robustness — KPTI runs on every core, the laptop is 8-core) ✅, Phase 86a (CSPRNG — ASLR/canary entropy) ✅, Phase 98 (bare-metal validation strategy) ✅
**Goal:** Activate + bare-metal-validate the Phase 84 KPTI CR3-trampoline (Meltdown), add userspace ASLR + stack canaries + CET shadow stacks, migrate password hashing to argon2id with a fallback verify path, and formally validate + record Secure Boot on the Dell Tiger Lake laptop — retiring the stale Phase 59 Track J / prior-audit #14 item. HW-only deliverables follow the `docs/appendix/bare-metal-validation.md` protocol and carry `Validated-on-HW (run N, date)`; CI-able deliverables carry passing-gate evidence.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Activate + bare-metal-validate KPTI (PML4 pair, CR3 trampoline, `KPTI_WIRED`, PCID, Meltdown PoC) | Phase 84 ✅, Phase 99 ✅ | Planned |
| B | Userspace ASLR + stack canaries + CET shadow stacks | Phase 86a ✅, A (shares the mm/exec path) | 🟢 **B.1 (ASLR) + B.2 (canaries) landed + green** (`aslr-smoke` + `stack-smash-smoke`); B.3 (CET) bare-metal-gated (planned) |
| C | argon2id password hashing migration (fallback read path + re-hash) | Phase 48 ✅ | ✅ **Landed** — RFC 9106 argon2id (+ BLAKE2b) host-tested; passwd/adduser/login write argon2id; verify_password fallback + login re-hash; seeded images argon2id; `argon2-smoke` PASS |
| D | Secure Boot on-metal validation + Phase 59 Track J / Phase 10 C.3 closeout | Phase 10 ✅, A (validated boot platform) | Planned |

---

## Track A — Activate + Bare-Metal-Validate KPTI

### A.1 — PML4 pair builder + user-half invariant self-test ✅ (builder + validation; live wiring is A.2)

**File:** `kernel/src/mm/kpti.rs` (new), `kernel/src/mm/mod.rs`, `kernel/src/lib.rs`
**Symbol:** `mm::kpti::self_test`, `mm::kpti::build_selftest_pair`, `mm::mapper_for_frame`
**Why it matters:** Today `new_process_page_table` clones the kernel PML4[1..512] into **every** process PML4, so kernel mappings live in the user CR3 — the exact arrangement Meltdown exploits. KPTI requires a second "user" PML4 carrying only PML4[0] (user pages) plus the minimal entry set, with kernel `.text`/heap/direct-map absent in the user half.

**As-built (A.1):** the reusable **user-half builder** (private-sub-table entry-set mapper — never cloning a kernel `PML4[i]` slot) plus a **boot-time self-test** landed with `KPTI_WIRED` still `false`, so the live CR3 is untouched and every existing gate is unaffected. The self-test builds a real user PML4 (a synthetic user page + a representative entry set: the PerCoreData page(s), the `syscall_entry` text page, a fresh entry stack), walks it back, and feeds the observations to `kernel_core::kpti::check_user_half_invariant` — emitting `KPTI_SELFTEST:PASS`. Gate `kpti-selftest-smoke` (`M3OS_KPTI_REGRESSION=1`) asserts it. **Load-bearing subtlety:** m3OS never uses `swapgs` (`GS_BASE` = `PerCoreData` in both rings), so the PerCoreData page(s) MUST be in the entry set — the entry asm reads `gs:[…]` before the CR3 switch. Entry-set pages are mapped at their existing kernel VAs through fresh private sub-tables. **Deferred to A.3 (originally slated A.2):** wiring the builder into `new_process_page_table` so `AddressSpace` tracks the pair per-process (only meaningful once the CR3 switch consumes it — building an inert second PML4 for every process would add fork/exec overhead for no benefit while `KPTI_WIRED` is false; and the full entry set the user half must carry — GDT/TSS/IDT/entry stacks — is only fixed by A.3's IRQ symmetry work).

**Acceptance:**
- [ ] `new_process_page_table` returns a kernel/user PML4 **pair** (kernel = full map; user = PML4[0] + minimal entry set), each tracked on the process `AddressSpace`. *(Builder landed in `mm::kpti`; per-process wiring lands with A.3, where the full entry set — GDT/TSS/IDT/entry stacks — is first known.)*
- [x] The user PML4 maps exactly the minimal entry set (trampoline text, IDT, GDT/TSS, per-CPU entry stack) and **no** kernel `.text`/heap/direct-map entries (verified by a walk asserting no kernel upper-half secret leaf in the user half). *(Self-test proves it on a representative pair; the real switch's full entry set — GDT/TSS/IDT — is added in A.2.)*
- [x] Host/`kernel-core` test (or a boot-time self-check) asserts the user half contains no kernel upper-half leaf PTE. *(`mm::kpti::self_test` → `KPTI_SELFTEST:PASS`, gate `kpti-selftest-smoke`; the `kernel_core::kpti` invariant model is host-tested.)*

### A.2 — Syscall-entry CR3 trampoline ✅

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `syscall_entry_kpti`, `syscall_entry`, `.text.kpti_entry`, `lstar_target`
**Why it matters:** On `SYSCALL` the CPU is still on the user CR3; the first instructions must switch to the kernel CR3 using only a scratch register and a trampoline stack mapped in the user PML4, before any kernel-stack or global access, and switch back before `sysretq`. This is the load-bearing ~200 LOC of asm.

**As-built (A.2):** a **separate** `syscall_entry_kpti` stub (option (b) from the handoff — production's LSTAR keeps pointing at the non-KPTI `syscall_entry` until `kpti_active`): saves user RSP, spills `rax` to `gs:[kpti_scratch]`, loads `gs:[kpti_kernel_cr3]` → `mov cr3`, restores `rax`, and joins the **shared** body at `.Lsyscall_entry_common` (single body = no dual-maintenance drift in the most triple-fault-prone asm in the kernel). SYSCALL auto-pushes nothing, so no trampoline stack is needed on this path — pre-switch instructions touch **only** `gs:[…]` (PerCoreData, mapped in both halves; m3OS is `swapgs`-free) and registers. The **sysret tail** re-derives the posture from `gs:[kpti_user_cr3]` (non-zero ⇔ KPTI-dispatched): spill `rax` → `test` → conditional `mov cr3` → restore — reading the per-core slot (not a saved flag) keeps `execve`'s mid-syscall pair retarget correct, and costs the non-KPTI path two gs-moves + one never-taken branch. The whole entry surface lives in a **page-aligned `.text.kpti_entry` section** bounded by `kpti_entry_text_start/_end` (`.balign 4096` both ends, so no neighbouring kernel text shares a mapped page); `mm::kpti` now maps that full range as the entry-set text and the boot self-test asserts both stubs lie inside it (layout regression = boot `KPTI_SELFTEST:FAIL reason=entry-text-layout`, not a live #PF loop at A.4). LSTAR selection: `lstar_target()` picks the stub from `mitigations::state().kpti_active` — APs (`mitigations::init_ap` runs before `syscall::init_ap`) and the S3-resume re-`init()` self-select; the BSP's early `init()` predates the policy decision and gets the explicit LSTAR re-install in A.4.

**Acceptance:**
- [x] `syscall_entry` switches CR3 to the kernel PML4 first (scratch register + trampoline stack only) and restores the user PML4 before `sysretq`. *(As `syscall_entry_kpti`; the SYSCALL path needs no trampoline stack — nothing is auto-pushed.)*
- [x] The existing `SFMASK` flag-masking and argument-register contract are preserved across the rewrite (all syscall smoke gates still pass under QEMU). *(SFMASK writes untouched; prologue clobbers only `rax` (spilled/restored via `kpti_scratch`); `smoke-test` + `regression` + `kpti-selftest-smoke` green.)*
- [x] No kernel global / kernel-stack access occurs before the CR3 switch (audited; trampoline stack lives in the user PML4's minimal set). *(Audited: pre-switch = `gs:[user_rsp]`, `gs:[kpti_scratch]`, `gs:[kpti_kernel_cr3]` — all PerCoreData, in the entry set.)*

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

### B.1 — ASLR: randomize PIE base / mmap base / stack top ✅

**File:** `kernel/src/mm/elf.rs` (`aslr_offset_bytes`, `map_user_stack`, the `ET_DYN` `load_bias`), `kernel/src/arch/x86_64/syscall/mod.rs` (mmap base at exec).
**Why it matters:** Every stack sat at the fixed `ELF_STACK_TOP`, mmaps at the fixed `ANON_MMAP_BASE`, and PIEs at a fixed bias — fully predictable.

**Acceptance:**
- [x] Per-`execve` page-aligned CSPRNG offsets jitter the **stack top** (≤ 1 MiB, within the eager stack mapping — the mapped extent, guard page, and demand-page window are unchanged; only the initial RSP moves), the **`ET_DYN` load bias** (≤ 2 MiB above `USER_VADDR_MIN` — the interpreter + PIE binaries), and the **anonymous mmap base** (≤ 16 MiB, seeded into `mmap_next` at exec). Native `ET_EXEC` binaries keep fixed link addresses (the target links non-PIE), so only their stack + mmap are randomized. Bounds keep every anchor inside the canonical range; the Phase 75 W^X reject is untouched.
- [x] `aslr-smoke` (CI-able, `M3OS_ASLR_REGRESSION=1`): execs `/bin/aslr-probe` 5× and asserts the printed stack address is **not all identical** (observed 5/5 distinct). Uses `global_fill`, falling back to `global_fill_insecure` when the DRBG lacks credited bits (QEMU TCG has no RDRAND) — still per-exec-varying; credited-random on real hardware. Never blocks on entropy (the DRBG is seeded at boot before any exec).

### B.2 — Stack canaries (`-Z stack-protector` + runtime symbols) ✅

**Files:** `.cargo/config.toml` (`[target.x86_64-m3os]` rustflag), new `userspace/syscall-lib/src/stack_protector.rs` (`__stack_chk_guard`, `__stack_chk_fail`, `seed_guard`), `userspace/syscall-lib/src/start.rs` (seed call).

**Acceptance:**
- [x] `-Z stack-protector=strong` set for the userspace target (in `.cargo/config.toml`, not the JSON — scoped so the ring-0 kernel never carries canaries). Confirmed via `objdump`: protected functions load the global `__stack_chk_guard` (RIP-relative, **not** `%fs:0x28`) and `call __stack_chk_fail`.
- [x] `__stack_chk_guard` is a fixed non-zero sentinel (canaries functional from the first instruction), **re-seeded per process from the CSPRNG** by `seed_guard`, called from the divergent `start::run_main*` trampoline before `main` (so the reseed can't trip its own caller's epilogue check). `__stack_chk_fail` prints `*** stack smashing detected` and `exit(134)`.
- [x] `stack-smash-smoke` (CI-able, `M3OS_ASLR_REGRESSION=1`): `/bin/stack-smash` overflows a 16-byte buffer past its canary and is aborted via `__stack_chk_fail` (the `STACK_SMASH:after-NOT-CAUGHT` line never prints). **PASS.**

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

### C.1 — argon2id implementation + host tests ✅

**Landed as:** `userspace/syscall-lib/src/argon2.rs` + `userspace/syscall-lib/src/blake2b.rs`, re-exported by `userspace/crypto-lib/src/argon2.rs`.
**Symbol:** `argon2id_hash`, `argon2id_verify`, `argon2id_raw` (+ `Blake2b`)
**Deviation (intentional):** the charter located this in `crypto-lib`, but `crypto-lib` depends on `syscall-lib` (for `getrandom`), so `verify_password` (in `syscall-lib`) can't call *up* into `crypto-lib` without a dependency cycle. The impl lives in `syscall-lib` (no new deps — BLAKE2b is dependency-free) and `crypto-lib` re-exports it, giving the charter's `crypto_lib::argon2::*` API and running the RFC vector in the merge gate.

**Acceptance:**
- [x] `argon2id_hash`/`argon2id_verify` implement RFC 9106 argon2id; `DEFAULT_PARAMS` = 4 MiB / t=3 / p=1 (conservative, ~6 ms native).
- [x] Host tests pass against the RFC 9106 §5.3 reference vector (`crypto-lib`'s `argon2id_rfc9106_vector`, in `cargo xtask check`) + the RFC 7693 BLAKE2b Appendix A vectors.
- [x] Verify uses constant-time comparison (`ct_eq`, no early-out).

### C.2 — `$argon2id$` shadow prefix + `verify_password` fallback arm ✅

**File:** `userspace/syscall-lib/src/sha256.rs` (dispatch) + `userspace/syscall-lib/src/argon2.rs` (format).
**Symbol:** `verify_password`, `argon2::verify_shadow_field`

**Acceptance:**
- [x] `verify_password` accepts `$argon2id$v=19$m=…,t=…,p=…$<hex_salt>$<hex_hash>` via the local `argon2::verify_shadow_field` (alloc-gated — argon2id needs a heap matrix; parses cost params from the stored entry). Hex (not PHC base64) matches the existing shadow convention.
- [x] The pre-existing `$sha256i$10000$…` and legacy `$sha256$…` arms are unchanged and still verify (the fallback read path; host tests assert all three).
- [x] A malformed `$argon2id$` field returns `false` (fail-closed) — `verify_shadow_field_fails_closed_on_malformed` covers wrong version, missing param, non-hex, over-cap memory, wrong prefix.

### C.3 — New writes use argon2id + transparent re-hash on legacy login ✅

**Files:** `userspace/passwd/src/main.rs`, `userspace/adduser/src/main.rs` (write path), `userspace/login/src/main.rs` (re-hash + set-initial-password).
**Deviations (intentional):** (a) the passwd-lib `HASH_FORMAT_PREFIX`/`build_hash_field` (the `$sha256i$` builders) are **left in place** as the legacy-format reference; the binaries now call `argon2::build_shadow_field` directly instead. (b) the re-hash lives in **`login`** (which runs as root) via login's existing non-atomic shadow-write path — **`su` does not re-hash** because when `su` requires a password the caller is not yet root and cannot write `/etc/shadow` (`su` only gains the alloc-gated argon2id *verify* arm).

**Acceptance:**
- [x] New `passwd`/`adduser` writes use argon2id exclusively (`build_shadow_field` + `DEFAULT_PARAMS`); all four auth binaries carry a `BrkAllocator` (`needs_alloc`).
- [x] A successful **login** against a `$sha256i$`/`$sha256$` entry re-hashes the shadow line to argon2id in place (best-effort; sentinel `[security] rehashed login password to argon2id`; `argon2-smoke` asserts the on-disk `$argon2id$` result).
- [x] Existing `passwd`/`shadow` host tests pass (`rewrite_shadow_file`/atomic-write state machines untouched).

### C.4 — Update the seeded-shadow regenerator + argon2-smoke ✅

**Files:** `xtask/src/main.rs` (`generate_seeded_shadow_line`, `legacy_sha256i_shadow_field`, `cmd_argon2_smoke`).

**Acceptance:**
- [x] `generate_seeded_shadow_line` emits `$argon2id$…` through the **same** `crypto_lib::argon2::build_shadow_field` the in-guest binaries use — byte-for-byte parity by construction, not just a matching host test.
- [x] `argon2-smoke` (CI-able, `M3OS_ARGON2_REGRESSION=1`): logs in as the argon2id-seeded root, plants a `$sha256i$` legacy user, drives a fresh `login` that fallback-verifies + re-hashes it, and confirms the on-disk `$argon2id$` result. **PASS (30 steps, ~36 s locally.)** The always-on `security-floor` regression step also now asserts `$argon2id$` in `/etc/shadow`.
- [x] `M3OS_ARGON2_REGRESSION` row added to the `AGENTS.md` gate table.

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
