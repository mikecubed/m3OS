# Phase 86f - Userspace SIMD / AES-NI Capstone

**Status:** Planned
**Source Ref:** phase-86f
**Depends on:** Phase 86c (HTTPS/TLS — ship correctness on software crypto first), Phase 57e/60 (per-task FPU/XSAVE save/restore) ✅, Phase 85 (Cross-Compiled Toolchains) ✅, Phase 77 (Pre-1.0 Cleanup) ✅
**Builds on:** Sub-phase **86f** of the [Phase 86 umbrella](./86-networking-and-github.md). It is the **last** sub-phase: it flips userspace from soft-float to an SSE/AES-enabled Rust target, finishes the signal-frame FPU path, re-validates the whole tree, and — per the Phase 85 precedent where the last sub-phase (85d) cut the family learning doc — owns the umbrella learning doc, the capability inventory cut, and the final kernel version bump.
**Primary Components:** `x86_64-m3os.json` (repurposed to hardware-float), `xtask/src/main.rs` (`build_userspace_bins` target selection), `kernel/src/signal.rs` (signal-frame FPU save/restore), `kernel/src/mm/elf.rs` (`setup_abi_stack_with_envp` RSP/auxv alignment), `userspace/crypto-lib` (`aes` crate AES-NI backend), `kernel/src/arch/x86_64/cpuid.rs` (`enable_xsave_state`, already live), `kernel/src/task/scheduler.rs` (`save_fpu_state`/`restore_fpu_state`, already live), `docs/86-networking-and-github.md` (new learning doc), `docs/research/simd-enablement.md`, `docs/README.md`, `docs/roadmap/README.md`, `kernel/Cargo.toml`, `AGENTS.md`

## Milestone Goal

Userspace gains SSE/SSE2 (and AES-NI) so the Rust crypto consumers — the 86b SSH client, any rustls-adjacent path, and `crypto-lib` — get hardware-accelerated symmetric crypto, while the **kernel stays soft-float** (no XMM in IRQ/exception handlers). A userspace binary disassembles to show `xmm` register use; the kernel image still contains none on its IRQ/retpoline paths; every existing gate (smoke, regression, tui-app, doom-audio, doom-concurrent) passes on the SSE-rebuilt userspace; and the AES-NI throughput win on the SSH/TLS path is measured. This sub-phase also cuts the family learning doc, reconciles the roadmap README rows, and bumps the kernel to `0.86.5` (the Phase 86 umbrella aggregate).

## Why This Phase Exists

m3OS has run **soft-float userspace** since inception — an unusual posture chosen originally to avoid FPU state save/restore on context switches. That rationale is now stale: per `docs/research/simd-enablement.md`, the expensive and error-prone kernel work is **already done and running**. Phase 57e/60 built the per-task XSAVE machinery (x87 + SSE + AVX), so `kernel/src/arch/x86_64/cpuid.rs::enable_xsave_state()` already sets `CR4.OSFXSR` + `CR4.OSXSAVE` + `XCR0 = 0x7` on the BSP and every AP, and `kernel/src/task/scheduler.rs` already calls `save_fpu_state`/`restore_fpu_state` (`xsaveopt64`/`xsave64`) around every `switch_context`. The XMM/YMM register file is saved and restored on **every** context switch today — even though no current code uses those registers. **The per-switch cost is already being paid for a benefit no one collects.**

Enabling SIMD is therefore primarily a **build-system change** (an SSE/AES-enabled Rust *userspace* target), plus finishing the signal-frame FPU path, verifying `_start` stack alignment, and a full re-validation pass. It is explicitly a **performance optimization, not a prerequisite**: TLS (86c) and SSH (86b) already work on software crypto, and userspace SSE2 already functions at all (the Phase 85 C ports — git, Python, clang — are ordinary SSE2 musl binaries built with `CFLAGS=-O2`, no `-mno-sse`). This phase is about the **Rust** userspace target and the **AES-NI** payoff, not basic SSE.

Crucially, enabling SSE does **not** unlock the `ring`/`aws-lc-rs` crypto ecosystem (those fail on their asm/C build and hosted-target assumptions, independent of the SSE flag), so the 86b SSH client decision is unaffected. Sequencing this last — after correctness ships on software crypto — means we accelerate crypto only once there is crypto to accelerate.

## Learning Goals

- Understand why m3OS could enable userspace SIMD as a build-system change rather than a kernel project: the XSAVE save/restore machinery (Phase 57e/60) was already paying the per-switch cost.
- Learn the principled soft-float-kernel / hard-float-userspace split: keeping ring 0 on `-sse` means IRQ/exception handlers never emit XMM, so the existing task-boundary save/restore stays sufficient and no FPU save is needed in interrupt entry.
- See why an SSE-using signal handler requires a complete signal-frame FPU save-on-delivery / restore-on-sigreturn, and why `movaps`/spills demand a 16-byte-aligned `_start` RSP.
- Understand that the `aes` crate uses `cpufeatures` **runtime** AES-NI autodetection on x86_64 by default (compile-time `+aes` is only a force path), so the real on-m3OS enabler for AES-NI is the userspace target leaving soft-float — permitting XMM/AES-NI codegen at all — and why hardware AES is the throughput payoff.
- Internalize that enabling SSE does **not** expand the Rust crypto-crate field (`ring`/`aws-lc-rs` still don't build) — a common misconception worth stating explicitly.

## Feature Scope

### SSE/AES-enabled Rust userspace target

`x86_64-m3os.json` — currently vestigial, carrying the explicit `-mmx,-sse,-sse2,…,-avx,-avx2,+soft-float` feature list — is repurposed to a hardware-float target with `+sse,+sse2` (optionally `+avx,+aes`) and the soft-float feature dropped. `xtask`'s `build_userspace_bins` (the three `--target x86_64-unknown-none` userspace invocations) is pointed at this repurposed target. The **kernel** stays on the built-in `x86_64-unknown-none` (`-sse`, `+soft-float`) — the two are deliberately decoupled. `-Zbuild-std` + nightly are already in use, so there is no new toolchain lift.

### Signal-frame FPU + entry alignment

The signal frame already reserves an `fpstate` slot (`kernel/src/signal.rs`). This phase completes the path: save the FPU state into the signal frame on delivery and restore it on `sigreturn`, so an SSE-using signal handler cannot corrupt the interrupted context's XMM. Separately, the kernel-built initial userspace stack (`kernel/src/mm/elf.rs::setup_abi_stack_with_envp`) is verified to land the RSP + auxv 16-byte aligned at `_start` — SSE `movaps`/register spills fault on misalignment, and although musl `_start` realigns, the kernel-supplied stack must already be aligned for an early SSE-spilling path.

### AES-NI crypto + full re-validation

`crypto-lib`'s `aes` dependency already autodetects AES-NI at runtime via `cpufeatures`; what unblocks it on m3OS is the userspace target leaving soft-float so XMM/AES-NI codegen is permitted at all (this sub-phase's own change), confirmed in-phase by `objdump`. ChaCha20/Poly1305 stay correct (and get faster). Then **every** userspace binary recompiles with SSE and the full gate suite re-runs: the real cost of this phase is blast radius (alignment/ABI surprises), not difficulty.

### Learning doc, version, and capability cut

As the final sub-phase, 86f cuts the umbrella learning doc `docs/86-networking-and-github.md` (covering the whole 86a–f arc), adds the `docs/README.md` learning row, reconciles the `docs/roadmap/README.md` 86 + 86a–f rows, marks `docs/research/simd-enablement.md` as scheduled/landed in 86f, updates the stale `AGENTS.md` target-flags note, and bumps `kernel/Cargo.toml` to `0.86.5`.

## Important Components and How They Work

### `x86_64-m3os.json` (the userspace target)

Today this file is vestigial — the active build uses the built-in `x86_64-unknown-none` for both kernel and userspace. Repurposing it gives userspace its own target with `features` flipped from `-mmx,-sse,…,+soft-float` to hardware-float `+sse,+sse2` (and optionally `+avx,+aes`). The data-layout, panic-abort, disable-redzone, and relocation-model fields are preserved. Only `build_userspace_bins` consumes it; the kernel build path (`build_kernel`) is untouched.

### The already-live XSAVE substrate (`cpuid.rs` / `scheduler.rs`)

`enable_xsave_state()` (with `XSAVE_FEATURE_MASK = 0x7`) and `save_fpu_state`/`restore_fpu_state` are **not** new work — they run today. This phase consumes them: once userspace emits XMM, the existing per-switch `xsaveopt64`/`xsave64` around `switch_context` is exactly what preserves that state. The phase's job is to make the consumers exist, not to build the substrate. If AVX-512 is ever wanted later, `XSAVE_FEATURE_MASK`/`XSAVE_AREA_SIZE` and the XCR0 mask would be bumped — explicitly deferred.

### Signal-frame FPU path (`kernel/src/signal.rs`)

The signal frame layout already reserves an `fpstate` slot (the `err/trapno/oldmask/cr2/fpstate/reserved` block). The delivery path must populate it from the task's saved FPU area, and the `sigreturn` path must restore it, so a signal handler that touches XMM does not clobber the interrupted context's vector registers.

### Initial-stack alignment (`kernel/src/mm/elf.rs`)

`setup_abi_stack_with_envp` builds the initial stack: it writes 16 bytes of `AT_RANDOM` data, the auxv layout (via `kernel_core::elf::auxv::build_layout`), then the argv/envp pointer table. SSE requires the SysV-ABI guarantee that RSP is 16-byte aligned at function entry; this phase verifies the kernel-built RSP + auxv land aligned at `_start`.

### `crypto-lib` AES backend (`userspace/crypto-lib`)

`crypto-lib` depends on the `aes` and `chacha20poly1305` crates (workspace deps). The `aes` crate (0.8.x) autodetects AES-NI at runtime via `cpufeatures` on x86_64; compile-time `+aes` is only a force path. The blocker is not the crate flag but the soft-float userspace target, which forbids XMM codegen entirely — so the crate's runtime detection has no hardware path to select. On the hardware-float target the AES-NI backend becomes the throughput payoff for the SSH/TLS symmetric path.

## How This Builds on Earlier Phases

- Consumes **Phase 57e/60**'s FPU/XSAVE machinery (`enable_xsave_state`, `save_fpu_state`/`restore_fpu_state`) — already running — which is precisely why 86f is a build-system change rather than a kernel project.
- Sequenced after **Phase 86c** (HTTPS/TLS): correctness ships on software crypto first, then this phase accelerates it. The 86c TLS suite (ChaCha20-Poly1305-preferred) and the 86b SSH client are the AES-NI/ChaCha consumers.
- Extends the **Phase 85** posture: the C ports (git/Python/clang) are already SSE2 musl binaries, so this phase only adds the Rust userspace target + AES-NI on top of an already-SSE-capable C userspace.
- Reuses **Phase 77**'s networking/DNS/`connect` groundwork transitively (the crypto it accelerates rides those paths) without reopening it.

## Implementation Outline

1. Repurpose `x86_64-m3os.json` to hardware-float `+sse,+sse2` (optionally `+avx,+aes`); point `build_userspace_bins`'s three userspace `--target` invocations at it; keep the kernel on `x86_64-unknown-none`.
2. Complete the signal-frame FPU save-on-delivery + restore-on-sigreturn in `kernel/src/signal.rs`; add a QEMU test that an SSE-using handler cannot corrupt the interrupted XMM.
3. Verify `setup_abi_stack_with_envp` lands RSP + auxv 16-byte aligned at `_start`; prove an SSE-spilling binary takes no `#GP`/misalignment fault.
4. Enable/confirm the `aes` crate's AES-NI backend in `crypto-lib`; measure the SSH/TLS crypto path before/after.
5. Recompile **all** userspace with SSE and re-run smoke + regression + tui-app + doom-audio + doom-concurrent; fix any ABI/alignment surprise.
6. Cut `docs/86-networking-and-github.md`, the `docs/README.md` row, the `docs/roadmap/README.md` 86/86a–f rows, the `simd-enablement.md` status, the `AGENTS.md` target-flags note; bump `kernel/Cargo.toml` to `0.86.5`.

## Acceptance Criteria

- All userspace crates compile against the `+sse,+sse2` `x86_64-m3os.json` target; the kernel still builds soft-float `x86_64-unknown-none`; `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests + retpoline gate).
- A userspace binary disassembles to show `xmm` register use; `objdump` of the kernel image shows **no** `xmm`/SSE instructions on its IRQ/exception/retpoline paths.
- An SSE-using signal handler runs and, on return via `sigreturn`, the interrupted context's XMM registers are bit-identical to their pre-signal values (QEMU test asserts this).
- An SSE-spilling userspace binary starts and runs with no `#GP`/misalignment fault; the initial RSP + auxv are 16-byte aligned at `_start`.
- A microbenchmark (a fixed payload through the `aes`/`aes-gcm` AEAD encrypt path, fixed iteration count) records throughput in MiB/s for the soft-float build vs the `+sse`/`+aes` build; the AES-NI AES path is **≥ 2×** the soft-float AES path (the chosen factor justified in the learning doc), and `objdump -d` of the rebuilt binary shows `aesenc`/`aesenclast` rather than the table-driven software S-box. ChaCha20/Poly1305 round-trip vectors still pass.
- `cargo xtask smoke-test`, `regression`, `tui-app-smoke`, `doom-audio-smoke`, and `doom-concurrent-smoke` all PASS on the SSE-rebuilt userspace with no regressions.
- A `userspace-simd-smoke` gate PASSes (disassembly + signal-frame + alignment + AES-NI assertions) and is wired as `cargo xtask userspace-simd-smoke` + an opt-in `M3OS_SIMD_REGRESSION=1` pre-push row.
- `docs/86-networking-and-github.md` exists per the learning-doc template (covering CSPRNG/clock/trust, SSH vs HTTPS, the Go runtime, `gh`, and SIMD/AES-NI) and is linked from `docs/README.md`; `docs/roadmap/README.md` 86 + 86a–f rows are reconciled; `docs/research/simd-enablement.md` is marked scheduled/landed in 86f.
- `kernel/Cargo.toml` reads `0.86.5`; the boot banner / `uname` report `0.86.5`; the `AGENTS.md` target-flags note reflects the SSE-enabled-userspace / soft-float-kernel split.

## Companion Task List

- [Phase 86f Task List](./tasks/86f-userspace-simd-tasks.md)

## How Real OS Implementations Differ

- Most OSes ship **hard-float userspace by default** with lazy or eager FPU save/restore; m3OS uniquely ran soft-float userspace until now and deliberately keeps the **kernel** soft-float (no XMM in IRQ handlers) — an unusual but principled split that keeps interrupt-entry FPU save unnecessary.
- Linux/BSD enable a broad SIMD spectrum (AVX2/AVX-512) with dynamic dispatch and runtime feature detection throughout libc and crypto libraries; 86f enables SSE/SSE2 (+AES) only, with **AVX-512 deferred** (bump `XSAVE_FEATURE_MASK`/`XCR0`).
- General-purpose systems pair hard-float with the full `ring`/`aws-lc-rs`/OpenSSL-asm crypto stack; on m3OS, enabling SSE does **not** make `ring`/`aws-lc-rs` build (they need asm/C builds + hosted-target assumptions, independent of the SSE flag) — so 86f does not expand the Rust crypto-crate field, a misconception worth stating.

## Deferred Until Later

- AVX-512 (and the corresponding `XSAVE_FEATURE_MASK`/`XSAVE_AREA_SIZE` + XCR0-mask bump).
- In-kernel SIMD (fast SSE memcpy or in-kernel crypto), which would require `kernel_fpu_begin`/`kernel_fpu_end`-style guards or IRQ-prologue FPU save — the kernel deliberately stays soft-float.
- Unlocking `ring`/`aws-lc-rs` (blocked by asm/C build + hosted-target assumptions, not by the SSE flag) — out of scope.
- Per-binary SIMD opt-in (this phase ships a single SSE-enabled userspace target; a per-binary scheme is an open question carried from `simd-enablement.md`).
- A full soft-float-ABI audit beyond what the re-validation surfaces (the musl/ports interplay edge cases noted in `simd-enablement.md`).
