# Phase 86f — Userspace SIMD / AES-NI Capstone: Task List

**Status:** Done ✅ — all four tracks landed; every acceptance item below is checked with its as-built evidence; kernel `0.86.5`.
**Source Ref:** phase-86f
**Depends on:** Phase 86c (HTTPS/TLS — ship correctness on software crypto first), Phase 57e/60 (per-task FPU/XSAVE save/restore) ✅, Phase 85 (Cross-Compiled Toolchains) ✅, Phase 77 (Pre-1.0 Cleanup) ✅
**Goal:** Flip the Rust userspace target from soft-float to an SSE/SSE2 (+AES) hardware-float target so `crypto-lib` and the 86b/86c crypto consumers get hardware AES-NI, while the kernel stays soft-float (no XMM in IRQ handlers); finish the signal-frame FPU save/restore path; verify `_start` RSP/auxv 16-byte alignment; re-validate the entire userspace tree against every gate; and — as the last Phase 86 sub-phase — cut the umbrella learning doc, reconcile the roadmap README, and bump the kernel to `0.86.5`.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the `87-vfs-bulk-io-tasks.md` style.)

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | SSE/AES-enabled Rust userspace target (`x86_64-m3os.json` + `build_userspace_bins`) | — | Landed |
| B | Signal-frame FPU save/restore + `_start` RSP/auxv alignment | A | Landed |
| C | AES-NI backend in `crypto-lib` + full re-validation + smoke gate | A, B | Landed |
| D | Learning doc + roadmap reconcile + version bump (capstone close-out) | A, B, C | Landed |

---

## Track A — SSE/AES-enabled Rust userspace target

### A.1 — Repurpose `x86_64-m3os.json` to hardware-float and point userspace builds at it

**Files:**
- `x86_64-m3os.json` (currently `"features": "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float"`)
- `xtask/src/main.rs` (`build_userspace_bins`, the three userspace `--target x86_64-unknown-none` invocations ≈ lines 1330, 1438, 1484)

**Symbol:** `build_userspace_bins` target selection
**Why it matters:** Enabling SIMD is primarily a build-system change — the per-switch XSAVE cost is already paid (Phase 57e/60) — and the kernel must stay `-sse` so IRQ/exception handlers never emit XMM.

**Acceptance:**
- [x] `x86_64-m3os.json` `"features"` field is changed to a hardware-float list (`-mmx,+sse,+sse2,+aes`; `+avx` deliberately omitted to bound blast radius) with `+soft-float` removed; all other fields unchanged except `target-pointer-width`/`target-c-int-width` string→integer (forced by current nightly target-spec schema) **and `"os": "m3os"` → `"none"`** — the vestigial `os` value silently flipped `target_os` away from `"none"`, compiling all 23 `cfg(target_os = "none")` device-host syscall wrappers in `driver_runtime` to their host-test fallbacks (ring-3 PCI enumeration returned 0 with no syscall emitted; caught by `e1000-restart-crash` in the full re-validation, C.2).
- [x] The `build_userspace_bins` userspace cargo invocations (per-binary loop + coreutils batch) use `--target <repo>/x86_64-m3os.json` with `-Zjson-target-spec`; the kernel build path (`build_kernel`) still uses the built-in `x86_64-unknown-none`. **Deviation:** `build_ldso` stays on `x86_64-unknown-none` — the dynamic linker must be PIE (`ET_DYN`) and the m3os target is `position-independent-executables: false`; an SSE ldso is not needed (Track B review finding, dynlink-smoke regression fix).
- [x] All userspace crates compile against the new target; `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests + retpoline gate). Userspace keeps `-Zretpoline` via a `[target.x86_64-m3os]` section in `.cargo/config.toml` (verified: retpoline thunk refs present in built userspace bins). Note: the target flip changes userspace ELFs from PIE (`ET_DYN`) to fixed-address `ET_EXEC` — verified supported by the loader (`load_bias = 0` path, same as the Phase 85d static clang binaries).

### A.2 — Prove userspace emits XMM and the kernel does not

**Files:**
- `xtask/src/main.rs` (a disassembly assertion in the new gate, see C.3)
- `x86_64-m3os.json` (userspace) vs the built-in `x86_64-unknown-none` (kernel)

**Symbol:** `objdump` XMM-instruction check
**Why it matters:** The whole point of the split is hardware SIMD in ring 3 with zero XMM in ring-0 IRQ/retpoline paths; this is the falsifiable proof.

**Acceptance:**
- [x] `objdump -d` of a representative userspace binary (e.g. a `crypto-lib` consumer) shows `xmm` register use (`init`: 630 XMM instructions, `crypto-test`: 3962, `sshd`: 5505).
- [x] `objdump -d` of the kernel image shows **no** `xmm`/SSE instructions on its IRQ/exception/retpoline code paths (0 XMM opcodes in the whole kernel image; permanent gate wired in C.3).

---

## Track B — Signal-frame FPU + entry alignment

### B.1 — Complete signal-frame FPU save-on-delivery + restore-on-sigreturn

**File:** `kernel/src/signal.rs` (the `fpstate` slot already exists in the `err/trapno/oldmask/cr2/fpstate/reserved` block)
**Symbol:** signal delivery path + `sigreturn` FPU restore
**Why it matters:** An SSE-using signal handler must not corrupt the interrupted context's XMM; the `fpstate` slot is reserved but not yet populated/restored.

**Acceptance:**
- [x] On signal delivery, the task's live FPU state is xsaved and written into an extended signal frame (560 + 832 = 1392 bytes; `fpstate` pointer at mcontext+184); on `sigreturn` it is restored — through a sanitized XSAVE header (`XSTATE_BV &= 0x7`, `XCOMP_BV = 0`, reserved bytes zeroed, MXCSR masked against a fixed `0xFFBF` — never the user-supplied mask field), closing the ring-0 `xrstor64` #GP DoS a hostile frame could otherwise trigger. `MINSIGSTKSZ` raised 2048 → 4096; alt-stack frame-fit check fails closed (SIGSEGV).
- [x] A QEMU test (`kernel/tests/signal_fpu.rs::xmm_survives_signal_frame_path`) fills XMM with known patterns, traverses the **production** `setup_signal_frame` (FPU bytes included) onto real mapped user pages, clobbers XMM, then traverses the production `restore_sigframe` + restore path and asserts the XMM registers are bit-identical. (A full ring-3 handler round-trip is not expressible in the kernel-test harness — no ring-3 signal-installation machinery; live delivery is exercised by every signal in `smoke-test`, and negative tests cover hostile XSAVE headers / attacker-controlled MXCSR_MASK.)

### B.2 — Verify userspace entry RSP + auxv 16-byte alignment

**File:** `kernel/src/mm/elf.rs`
**Symbol:** `setup_abi_stack_with_envp` (writes `AT_RANDOM`, the auxv via `kernel_core::elf::auxv::build_layout`, then argv/envp)
**Why it matters:** SSE `movaps`/register spills `#GP` on misalignment; musl `_start` realigns, but the kernel-built initial stack + auxv must already land 16-byte aligned for any early SSE-spilling path.

**Acceptance:**
- [x] The initial RSP handed to `_start` is 16-byte aligned (SysV psABI process-entry contract: RSP ≡ 0 mod 16 with argc at RSP), and the auxv table lands aligned; `setup_abi_stack_with_envp` carries a `debug_assert` and `kernel-core` a host test. **Finding:** the pre-86f code landed RSP ≡ 8 mod 16 — a latent SSE `#GP`; additionally all 19 hand-written non-naked `_start` stubs (compiler prologues assuming the called-function convention) were converted to `#[unsafe(naked)]` trampolines matching the `entry_point!` idiom.
- [x] An SSE-spilling userspace binary starts and runs to completion with no `#GP`/misalignment fault (`smoke-test` 25/25 green on the SSE-rebuilt userspace incl. converted `sh0`/`login`/`id`; locked in permanently by the C.3 gate).

---

## Track C — AES-NI crypto + full re-validation + smoke gate

### C.1 — Enable the AES-NI backend in `crypto-lib`'s `aes` path

**File:** `userspace/crypto-lib` (`Cargo.toml` `aes` + `chacha20poly1305` workspace deps; the `aes` crate's AES-NI vs software backend)
**Symbol:** `aes` crate backend selection (`cpufeatures` runtime AES-NI autodetection; the real gate is the userspace target leaving soft-float)
**Why it matters:** The `aes` crate (0.8.x) autodetects AES-NI at runtime via `cpufeatures` on x86_64 (compile-time `+aes` is only a force path); the real m3OS enabler is the soft-float→hardware-float userspace target, which permits XMM/AES-NI codegen at all — this is what unlocks the throughput payoff for the SSH/TLS symmetric path.

**Acceptance:**
- [x] `objdump -d` of a rebuilt userspace binary that exercises AES shows `aesenc`/`aesenclast` (hardware AES-NI) rather than the table-driven software S-box (`crypto-test`: 208 `aesenc`, 16 `aesenclast`, 13 `aeskeygenassist`), confirming the `aes` crate's `cpufeatures` runtime detection engages once the hardware-float target permits XMM codegen.
- [x] A microbenchmark (1 MiB payload × 32 iterations through the AES-256-CTR path — crypto-lib has no `aes-gcm` dep; AES-CTR is its real AES consumer) records throughput in MiB/s: hardware AES-NI **5459 MiB/s** vs forced-soft (`--cfg aes_force_soft`, fixsliced backend) **203 MiB/s** ≈ **27×**, far above the ≥ 2× criterion (factor to be justified in the learning doc, Track D). An in-OS `crypto-test --bench` mode prints `BENCH:<cipher>:<MiB/s>` sentinels via `CLOCK_MONOTONIC` for the C.3 harness (QEMU runs TCG by default, so the host A/B is the authoritative comparison).
- [x] ChaCha20/Poly1305 round-trip vectors still pass (31 crypto-lib host tests green under both hardware and forced-soft backends); NIST SP 800-38A §F.5.5 CTR-AES256 conformance vectors added; no AEAD correctness regression.

### C.2 — Full re-validation: recompile all userspace with SSE, re-run every gate

**Files:**
- `xtask` gate commands (`smoke-test`, `regression`, `tui-app-smoke`, `doom-audio-smoke`, `doom-concurrent-smoke`)
- `.githooks/pre-push`

**Symbol:** the existing smoke/regression/tui-app/doom gate drivers
**Why it matters:** Every userspace binary recompiles with SSE — the real cost is blast radius (alignment/ABI surprises), not difficulty — so the whole gate suite must pass on the rebuilt tree.

**Acceptance:**
- [x] `cargo xtask smoke-test` and `regression` (11/11 arms, incl. `e1000-restart-crash`) PASS on the SSE-rebuilt userspace.
- [x] `cargo xtask tui-app-smoke` (60 steps), `doom-audio-smoke`, and `doom-concurrent-smoke` (two concurrent DOOMs) PASS on the SSE-rebuilt userspace.
- [x] Every ABI/alignment surprise surfaced by the rebuild was fixed (no skipped or quarantined gate; no regression vs the pre-SSE baseline): (1) entry RSP ≡ 8 → ≡ 0 mod 16 + 19 naked-trampoline `_start` conversions (Track B); (2) `ld-musl` kept on `x86_64-unknown-none` (must stay PIE/`ET_DYN`); (3) `x86_64-m3os.json` `"os"` restored to `"none"` — the vestigial `"m3os"` value flipped `target_os` and compiled `driver_runtime`'s 23 `cfg(target_os = "none")` device-host syscall wrappers to host-test fallbacks, blinding every ring-3 PCI driver (caught by `e1000-restart-crash`, root-caused via instrumented driver + kernel + disassembly, bisected against a green `main` baseline).

### C.3 — `userspace-simd-smoke` gate + pre-push wiring

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`
- `AGENTS.md` (opt-in gate row)

**Symbol:** `cmd_userspace_simd_smoke`
**Why it matters:** Locks in the SIMD/AES-NI win so a later change cannot silently revert userspace to soft-float or break the signal-frame FPU path; mirrors the existing opt-in gate pattern.

**Acceptance:**
- [x] The gate asserts a userspace binary disassembles with `xmm` use (`crypto-test`: >0 instruction-line matches) and the kernel image has none (0; instruction-line matcher immune to symbol-name false positives) plus `aesenc`/`aesenclast` present (Track A.2 + C.1 static proof), and boots QEMU to run `/bin/crypto-test` ("all tests PASSED" — the SSE+AES binary runs fault-free on the 16-aligned entry stack, Track B.2) and `/bin/crypto-test --bench` (`BENCH:aes-ctr:` — the AES path executes in-OS, Track C.1). Negative-sanity proven (an inverted assertion fails the gate). Note: QEMU TCG's `-cpu qemu64` does not advertise AES, and compile-time `+aes` makes `cpufeatures` short-circuit, so `+aes` was added to the shared TCG `-cpu` flags (KVM `-cpu host` path untouched) — on real hardware this sets an AES-NI hardware floor for userspace.
- [x] The gate is wired as `cargo xtask userspace-simd-smoke` (`--timeout`/`--display` like siblings) and as an opt-in pre-push regression behind `M3OS_SIMD_REGRESSION=1` in both `AGENTS.md`'s gate table and `.githooks/pre-push`.

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
- [x] `docs/86-networking-and-github.md` is created per the aligned learning-doc template (matched to the `docs/85-cross-compiled-toolchains.md` structure), covering the whole 86a–f arc (CSPRNG/clock/trust foundation, SSH vs HTTPS trust models, the Go runtime, `gh`, and SIMD/AES-NI incl. the measured 27× and the `ring`/`aws-lc-rs`-not-unlocked caveat); reviewer-verified factually accurate against the landed code with all links resolving.
- [x] `docs/README.md` gains a Phase 86 learning row linking the umbrella doc and all six sub-phase design docs.
- [x] `docs/roadmap/README.md`'s 86 + 86a–f rows are reconciled (column format verified consistent; 86/86f marked Complete at kernel `0.86.5`).
- [x] `docs/research/simd-enablement.md` is marked landed in 86f (with pointers to the design + learning docs); the `AGENTS.md` target-flags note reflects SSE-enabled userspace + soft-float kernel (stale rationale removed), and the existing CPU-hardening capability bullet was extended in place (no new capability class added, per the file's maintenance policy).

### D.2 — Bump kernel crate `0.85.3` → `0.86.5`

**File:** `kernel/Cargo.toml` (line 3, currently `version = "0.85.3"`)
**Symbol:** `[package] version = "0.86.5"`
**Why it matters:** 86f is the final Phase 86 sub-phase, so it carries the umbrella aggregate version (`0.86.0` → `0.86.5`), mirroring the Phase 85 sequence where 85d cut `0.85.3`.

**Acceptance:**
- [x] `kernel/Cargo.toml` reads `version = "0.86.5"` (+ `Cargo.lock` updated); `cargo xtask check` is clean.
- [x] The boot banner / `uname` report `0.86.5` (`env!("CARGO_PKG_VERSION")` → kernel compiled as `v0.86.5` in every gate build; the banner string is a compile-time constant of that version).

---

## Documentation Notes

- **This is a build-system + signal-frame + revalidation pass, not a kernel-architecture project.** The expensive XSAVE machinery (`kernel/src/arch/x86_64/cpuid.rs::enable_xsave_state`, `kernel/src/task/scheduler.rs::save_fpu_state`/`restore_fpu_state`) is already live (Phase 57e/60) and the per-switch cost is already paid; this phase only adds the *consumers*. Reference exact symbols, not "the FPU code".
- **The kernel deliberately stays soft-float** (`x86_64-unknown-none`, `-sse`); only userspace (`x86_64-m3os.json`) flips to hardware-float. Do not enable SSE in the kernel build path.
- **Enabling SSE does not unlock `ring`/`aws-lc-rs`** (asm/C build + hosted-target assumptions, independent of the SSE flag), so the 86b SSH client decision is unaffected — state this where the misconception could arise. See sibling `86b-ssh-git-transport.md` / `86c-https-git-transport.md`.
- **Userspace SSE2 already functionally works** — the Phase 85 C ports (git, Python, clang) are ordinary SSE2 musl binaries — so this phase is about the *Rust* userspace target + AES-NI, not basic SSE.
- **Open questions carried from `docs/research/simd-enablement.md`** become notes/follow-ups: single userspace target vs per-binary opt-in (single is shipped here); the soft-float-ABI audit (musl/ports interplay); `cpufeatures` runtime AES-NI detection vs compile-time `+aes` gating in the `aes` crate.
- This sub-phase reads `docs/research/simd-enablement.md` as its authoritative source and the [Phase 86 umbrella](../86-networking-and-github.md) for the shared architecture; the learning-doc ownership follows the Phase 85 → 85d precedent.
