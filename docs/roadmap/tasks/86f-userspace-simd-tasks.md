# Phase 86f — Userspace SIMD / AES-NI Capstone: Task List

**Status:** In Progress
**Source Ref:** phase-86f
**Depends on:** Phase 86c (HTTPS/TLS — ship correctness on software crypto first), Phase 57e/60 (per-task FPU/XSAVE save/restore) ✅, Phase 85 (Cross-Compiled Toolchains) ✅, Phase 77 (Pre-1.0 Cleanup) ✅
**Goal:** Flip the Rust userspace target from soft-float to an SSE/SSE2 (+AES) hardware-float target so `crypto-lib` and the 86b/86c crypto consumers get hardware AES-NI, while the kernel stays soft-float (no XMM in IRQ handlers); finish the signal-frame FPU save/restore path; verify `_start` RSP/auxv 16-byte alignment; re-validate the entire userspace tree against every gate; and — as the last Phase 86 sub-phase — cut the umbrella learning doc, reconcile the roadmap README, and bump the kernel to `0.86.5`.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the `92-vfs-bulk-io-tasks.md` style.)

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | SSE/AES-enabled Rust userspace target (`x86_64-m3os.json` + `build_userspace_bins`) | — | Planned |
| B | Signal-frame FPU save/restore + `_start` RSP/auxv alignment | A | Planned |
| C | AES-NI backend in `crypto-lib` + full re-validation + smoke gate | A, B | Planned |
| D | Learning doc + roadmap reconcile + version bump (capstone close-out) | A, B, C | Planned |

---

## Track A — SSE/AES-enabled Rust userspace target

### A.1 — Repurpose `x86_64-m3os.json` to hardware-float and point userspace builds at it

**Files:**
- `x86_64-m3os.json` (currently `"features": "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float"`)
- `xtask/src/main.rs` (`build_userspace_bins`, the three userspace `--target x86_64-unknown-none` invocations ≈ lines 1330, 1438, 1484)

**Symbol:** `build_userspace_bins` target selection
**Why it matters:** Enabling SIMD is primarily a build-system change — the per-switch XSAVE cost is already paid (Phase 57e/60) — and the kernel must stay `-sse` so IRQ/exception handlers never emit XMM.

**Acceptance:**
- [ ] `x86_64-m3os.json` `"features"` field is changed to a hardware-float list (`+sse,+sse2`, optionally `+avx,+aes`) with `+soft-float` removed; all other fields (`disable-redzone`, `panic-strategy: abort`, `relocation-model`, data-layout) unchanged.
- [ ] The three `build_userspace_bins` userspace cargo invocations use `--target <repo>/x86_64-m3os.json` (not `x86_64-unknown-none`); the kernel build path (`build_kernel`) still uses the built-in `x86_64-unknown-none`.
- [ ] All userspace crates compile against the new target; `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests + retpoline gate).

### A.2 — Prove userspace emits XMM and the kernel does not

**Files:**
- `xtask/src/main.rs` (a disassembly assertion in the new gate, see C.3)
- `x86_64-m3os.json` (userspace) vs the built-in `x86_64-unknown-none` (kernel)

**Symbol:** `objdump` XMM-instruction check
**Why it matters:** The whole point of the split is hardware SIMD in ring 3 with zero XMM in ring-0 IRQ/retpoline paths; this is the falsifiable proof.

**Acceptance:**
- [ ] `objdump -d` of a representative userspace binary (e.g. a `crypto-lib` consumer) shows `xmm` register use.
- [ ] `objdump -d` of the kernel image shows **no** `xmm`/SSE instructions on its IRQ/exception/retpoline code paths.

---

## Track B — Signal-frame FPU + entry alignment

### B.1 — Complete signal-frame FPU save-on-delivery + restore-on-sigreturn

**File:** `kernel/src/signal.rs` (the `fpstate` slot already exists in the `err/trapno/oldmask/cr2/fpstate/reserved` block)
**Symbol:** signal delivery path + `sigreturn` FPU restore
**Why it matters:** An SSE-using signal handler must not corrupt the interrupted context's XMM; the `fpstate` slot is reserved but not yet populated/restored.

**Acceptance:**
- [ ] On signal delivery, the task's FPU state is saved into the signal frame's `fpstate` slot; on `sigreturn` it is restored.
- [ ] A QEMU test installs an SSE-using signal handler that clobbers XMM, raises the signal mid-computation, and asserts the interrupted context's XMM registers are bit-identical to their pre-signal values after the handler returns.

### B.2 — Verify userspace entry RSP + auxv 16-byte alignment

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `setup_abi_stack_with_envp` (writes `AT_RANDOM`, the auxv via `kernel_core::elf::auxv::build_layout`, then argv/envp)
**Why it matters:** SSE `movaps`/register spills `#GP` on misalignment; musl `_start` realigns, but the kernel-built initial stack + auxv must already land 16-byte aligned for any early SSE-spilling path.

**Acceptance:**
- [ ] The initial RSP handed to `_start` is 16-byte aligned (SysV ABI), and the auxv table lands aligned; verified by an assertion/test on the computed stack pointer in `setup_abi_stack_with_envp`.
- [ ] An SSE-spilling userspace binary starts and runs to completion with no `#GP`/misalignment fault.

---

## Track C — AES-NI crypto + full re-validation + smoke gate

### C.1 — Enable the AES-NI backend in `crypto-lib`'s `aes` path

**File:** `userspace/crypto-lib` (`Cargo.toml` `aes` + `chacha20poly1305` workspace deps; the `aes` crate's AES-NI vs software backend)
**Symbol:** `aes` crate backend selection (`cpufeatures` runtime AES-NI autodetection; the real gate is the userspace target leaving soft-float)
**Why it matters:** The `aes` crate (0.8.x) autodetects AES-NI at runtime via `cpufeatures` on x86_64 (compile-time `+aes` is only a force path); the real m3OS enabler is the soft-float→hardware-float userspace target, which permits XMM/AES-NI codegen at all — this is what unlocks the throughput payoff for the SSH/TLS symmetric path.

**Acceptance:**
- [ ] `objdump -d` of a rebuilt userspace binary that exercises AES shows `aesenc`/`aesenclast` (hardware AES-NI) rather than the table-driven software S-box, confirming the `aes` crate's `cpufeatures` runtime detection engages once the hardware-float target permits XMM codegen.
- [ ] A microbenchmark (a fixed payload through the `aes`/`aes-gcm` AEAD encrypt path, fixed iteration count) records throughput in MiB/s for the soft-float build vs the `+sse`/`+aes` build; the AES-NI AES path is **≥ 2×** the soft-float AES path (the factor justified in the learning doc), and `objdump -d` of the rebuilt binary shows `aesenc`/`aesenclast` rather than the table-driven software S-box.
- [ ] ChaCha20/Poly1305 round-trip vectors still pass (`crypto-lib` / `crypto-test` host tests unchanged-green); no AEAD correctness regression.

### C.2 — Full re-validation: recompile all userspace with SSE, re-run every gate

**Files:**
- `xtask` gate commands (`smoke-test`, `regression`, `tui-app-smoke`, `doom-audio-smoke`, `doom-concurrent-smoke`)
- `.githooks/pre-push`

**Symbol:** the existing smoke/regression/tui-app/doom gate drivers
**Why it matters:** Every userspace binary recompiles with SSE — the real cost is blast radius (alignment/ABI surprises), not difficulty — so the whole gate suite must pass on the rebuilt tree.

**Acceptance:**
- [ ] `cargo xtask smoke-test` and `regression` PASS on the SSE-rebuilt userspace.
- [ ] `cargo xtask tui-app-smoke`, `doom-audio-smoke`, and `doom-concurrent-smoke` PASS on the SSE-rebuilt userspace.
- [ ] Any ABI/alignment surprise surfaced by the rebuild is fixed (no skipped or quarantined gate); no regression versus the pre-SSE baseline.

### C.3 — `userspace-simd-smoke` gate + pre-push wiring

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`
- `AGENTS.md` (opt-in gate row)

**Symbol:** `cmd_userspace_simd_smoke`
**Why it matters:** Locks in the SIMD/AES-NI win so a later change cannot silently revert userspace to soft-float or break the signal-frame FPU path; mirrors the existing opt-in gate pattern.

**Acceptance:**
- [ ] The gate asserts a userspace binary disassembles with `xmm` use and the kernel image has none on IRQ/retpoline paths (Track A.2), the SSE-spilling binary runs fault-free (Track B.2), and the AES-NI backend is active (Track C.1).
- [ ] The gate is wired as `cargo xtask userspace-simd-smoke` and as an opt-in pre-push regression behind `M3OS_SIMD_REGRESSION=1` in both `AGENTS.md`'s gate table and `.githooks/pre-push`.

---

## Track D — Capstone close-out (learning doc + version)

### D.1 — Create the umbrella learning doc + README rows + reconcile roadmap

**Files:**
- `docs/86-networking-and-github.md` (new, aligned learning-doc template)
- `docs/README.md` (learning row after Phase 85 ≈ line 71)
- `docs/roadmap/README.md` (the 86 umbrella + 86a–f rows)
- `docs/research/simd-enablement.md` (mark scheduled/landed in 86f)
- `AGENTS.md` (the stale `-mmx,-sse … to avoid FPU save/restore` target-flags note ≈ line 191; add a userspace-SIMD capability bullet if warranted)

**Symbol:** the umbrella learning doc; the `AGENTS.md` target-flags note
**Why it matters:** This is the **last** sub-phase, so per the Phase 85 precedent (the family learning doc owned by the last sub-phase, 85d) it owns the umbrella learning doc, the capability cut, and the README reconcile; the umbrella was authored against the a–f split (carrying the Sub-Phase Decomposition table and the six task links as forward-looking `Planned` rows), so 86f reconciles those rows, links, and version lines with what actually landed and cuts the learning doc.

**Acceptance:**
- [ ] `docs/86-networking-and-github.md` is created per the aligned learning-doc template, covering the whole 86a–f arc (CSPRNG/clock/trust foundation, SSH vs HTTPS trust models, the Go runtime, `gh`, and SIMD/AES-NI).
- [ ] `docs/README.md` gains a Phase 86 learning row linking the umbrella doc and all six sub-phase design docs.
- [ ] `docs/roadmap/README.md`'s 86 + 86a–f rows are reconciled (Theme/Outcome/Status/Source Ref/Milestone/Tasks columns consistent with the umbrella decomposition table).
- [ ] `docs/research/simd-enablement.md` is marked scheduled/landed in 86f; the `AGENTS.md` target-flags note is updated to reflect SSE-enabled userspace + soft-float kernel (the stale "to avoid FPU state save/restore" rationale removed), with a userspace-SIMD capability bullet added if it introduces a new capability class.

### D.2 — Bump kernel crate `0.85.3` → `0.86.5`

**File:** `kernel/Cargo.toml` (line 3, currently `version = "0.85.3"`)
**Symbol:** `[package] version = "0.86.5"`
**Why it matters:** 86f is the final Phase 86 sub-phase, so it carries the umbrella aggregate version (`0.86.0` → `0.86.5`), mirroring the Phase 85 sequence where 85d cut `0.85.3`.

**Acceptance:**
- [ ] `kernel/Cargo.toml` reads `version = "0.86.5"` (+ `Cargo.lock` updated); `cargo xtask check` is clean.
- [ ] The boot banner / `uname` report `0.86.5` (`env!("CARGO_PKG_VERSION")` → kernel built as `v0.86.5`).

---

## Documentation Notes

- **This is a build-system + signal-frame + revalidation pass, not a kernel-architecture project.** The expensive XSAVE machinery (`kernel/src/arch/x86_64/cpuid.rs::enable_xsave_state`, `kernel/src/task/scheduler.rs::save_fpu_state`/`restore_fpu_state`) is already live (Phase 57e/60) and the per-switch cost is already paid; this phase only adds the *consumers*. Reference exact symbols, not "the FPU code".
- **The kernel deliberately stays soft-float** (`x86_64-unknown-none`, `-sse`); only userspace (`x86_64-m3os.json`) flips to hardware-float. Do not enable SSE in the kernel build path.
- **Enabling SSE does not unlock `ring`/`aws-lc-rs`** (asm/C build + hosted-target assumptions, independent of the SSE flag), so the 86b SSH client decision is unaffected — state this where the misconception could arise. See sibling `86b-ssh-git-transport.md` / `86c-https-git-transport.md`.
- **Userspace SSE2 already functionally works** — the Phase 85 C ports (git, Python, clang) are ordinary SSE2 musl binaries — so this phase is about the *Rust* userspace target + AES-NI, not basic SSE.
- **Open questions carried from `docs/research/simd-enablement.md`** become notes/follow-ups: single userspace target vs per-binary opt-in (single is shipped here); the soft-float-ABI audit (musl/ports interplay); `cpufeatures` runtime AES-NI detection vs compile-time `+aes` gating in the `aes` crate.
- This sub-phase reads `docs/research/simd-enablement.md` as its authoritative source and the [Phase 86 umbrella](../86-networking-and-github.md) for the shared architecture; the learning-doc ownership follows the Phase 85 → 85d precedent.
