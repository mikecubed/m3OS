# Enabling SIMD (SSE/AVX) for Userspace — Feasibility Findings

**Status:** Findings (captured 2026-05-29, branch `feat/phase-77-pre-1-0-cleanup`)
**Scope:** What it would take to let userspace use SSE/AVX (incl. hardware AES-NI), given the current SIMD-disabled build.
**Relevant phases:** informs Phase 86 (Networking and GitHub — crypto throughput) and any future "userspace SIMD" perf track.

---

## TL;DR

**The expensive, error-prone kernel work is already done.** Phase 57e/60 built — and the
running kernel already exercises — the full per-task FPU/XSAVE state-preservation machinery
(x87 + SSE + AVX). The XMM/YMM register file is saved and restored on **every** context
switch today, even though no code uses those registers (everything is compiled
`+soft-float`). Enabling SIMD is therefore primarily a **build-system change** (a
SSE-enabled userspace target), not a kernel-architecture project. The per-switch XSAVE cost
is *already being paid*.

SIMD is **not a prerequisite** for HTTPS/TLS — RustCrypto software backends already compile
and run in the SIMD-off target (proven by `sunset`). Enabling it is a **performance**
optimization (hardware AES-NI, faster ChaCha/Poly) plus broader crate compatibility.

## Why SIMD is currently off

- Active build target is the **built-in `x86_64-unknown-none`** (`.cargo/config.toml`):
  SSE off, `+soft-float`, red-zone off, `panic=abort`. (`x86_64-m3os.json` exists but is
  vestigial — it carries the explicit `-mmx,-sse,-sse2,…,-avx,-avx2,+soft-float` list.)
- Kernel and userspace currently share this one target.
- `AGENTS.md` "Target flags — do not remove" lists `-mmx,-sse` with a now-stale rationale
  ("to avoid FPU state save/restore on context switches"). That save/restore now exists.

## What already exists (the surprise)

| Piece | Where |
|---|---|
| Hardware enable per-CPU (BSP + every AP): CR4.OSFXSR (set by bootloader) + CR4.OSXSAVE + `XCR0 = 0x7` (x87+SSE+AVX) | `kernel/src/arch/x86_64/cpuid.rs` — `enable_xsave_state()`, `XSAVE_FEATURE_MASK = 0x7` |
| Per-task save area, slab-cached 1:1 with `Task` | `kernel/src/task/mod.rs` `XSaveArea` (832 B = 512 legacy + 64 hdr + 256 YMM_hi); `kernel/src/mm/slab.rs` `xsave_cache` |
| **Context-switch save/restore** (`xsaveopt64`/`xsave64`) called around `switch_context` | `kernel/src/task/scheduler.rs` — `save_fpu_state`/`restore_fpu_state`, `fpu_states: Vec<SlabBox<XSaveArea>>`, restore at dispatch (≈ lines 2786, 5237) |
| Signal frame reserves an `fpstate` slot | `kernel/src/signal.rs` |
| New-task MXCSR/x87 defaults | `XSaveArea::new()` (legacy-region defaults) |
| Toolchain for a custom target already in use | nightly + `-Zbuild-std=core,compiler_builtins,alloc` in `xtask` `build_userspace` |

## What enabling SIMD would take

1. **A SSE/AVX-enabled userspace target (main task, small).** Repurpose `x86_64-m3os.json`
   to hardware-float + `+sse,+sse2` (optionally `+avx`, `+aes`), and point xtask's userspace
   builds (the `--target x86_64-unknown-none` calls in `build_userspace`, ≈ lines
   1042/1150/1196) at it. Build-std + nightly are already in use, so there is **no new
   toolchain lift** — usually the painful part, already solved.
2. **Keep the kernel soft-float (key decision).** Leave ring 0 on `x86_64-unknown-none`
   (`-sse`). Then IRQ/exception handlers never emit XMM, the existing task-boundary
   save/restore stays sufficient, and no FPU save is needed in interrupt entry. The kernel
   does not need SIMD for the crypto goal — crypto runs in userspace. (Only in-kernel SSE —
   e.g. fast memcpy or in-kernel crypto — would require `kernel_fpu_begin/end`-style guards
   or IRQ-prologue FPU save. Out of scope.)
3. **Finish the signal-frame FPU path.** The `fpstate` slot exists; complete save-into-sigframe
   on delivery + restore on `sigreturn` so an SSE-using signal handler can't corrupt the
   interrupted context's XMM.
4. **Verify userspace entry RSP is 16-byte aligned** (SSE `movaps`/spills require it; musl
   `_start` realigns, but confirm the kernel-built initial stack + auxv lands aligned).
5. **Full re-validation (the real cost).** Every userspace binary recompiles with SSE → re-run
   smoke / regression / tui-app / doom gates to catch alignment/ABI surprises. Blast radius,
   not difficulty.
6. **Update the stale `AGENTS.md` note** (done in the capture commit — points here).

If AVX-512 is ever wanted, bump `XSAVE_FEATURE_MASK`/`XSAVE_AREA_SIZE` and the XCR0 mask
(currently intentionally deferred).

## Crypto framing (why this came up)

- Enabling SIMD does **not** unlock otherwise-impossible TLS — software RustCrypto already works.
- Payoff: **hardware AES-NI** (large TLS throughput win; the `aes` crate's AES-NI backend needs
  `+aes`/`+sse` at compile time) + faster ChaCha20/Poly1305 + baseline-`sse2` crate compatibility.
- It still does **not** make `ring` / `aws-lc-rs` build — those need asm/C builds, independent of SSE.

## Recommendation

Optimization, not a blocker. Sequence it **after** HTTPS lands on software crypto (Phase 86):
ship correctness first, then do an "enable userspace SSE/AES-NI" pass as a focused perf track
with the full re-validation it demands. Doing it first mostly buys faster crypto before there
is crypto to accelerate.

## Open questions for the future track

- Single userspace target for all binaries, or per-binary opt-in? (Single is simpler; revalidate everything.)
- Does any current userspace binary assume the soft-float ABI in a way that breaks under hard-float? (Audit musl/ports interplay.)
- Confirm `cpufeatures` runtime AES-NI detection vs compile-time `+aes` gating in the `aes` crate for the chosen target.
