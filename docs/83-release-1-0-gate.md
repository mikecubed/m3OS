# Release 1.0 Gate

**Aligned Roadmap Phase:** Phase 83
**Status:** Complete
**Source Ref:** phase-83
**Authoritative artifact:** [`docs/release/1.0-release-gate.md`](./release/1.0-release-gate.md)

## Overview

Phase 83 is the phase where m3OS stops *assuming* what "1.0" means and writes it
down. It ships **no runtime code** — its single source change is the
`kernel/Cargo.toml` version bump from `0.82.0` to `0.83.0`. Everything else is a
release **contract**: a document that states exactly what the system promises, on
which targets, with which workflows, and — crucially — with which **evidence** behind
every claim. The lesson of this phase is that release engineering is an
*architectural* discipline, not paperwork: a good 1.0 is defined by the promises it
can keep and the scope it intentionally refuses, and a promise with no reproducible
proof behind it is just marketing.

## What This Doc Covers

- The **support matrix** — why a closed status vocabulary makes a release promise
  falsifiable, and how the matrix becomes the central artifact.
- The **gate bundle and evidence trail** — how every "Supported" claim is tied to a
  named, runnable `cargo xtask` gate, and why the mapping must be *mutually
  exhaustive*.
- The **scope decision** — why 1.0 includes the local-system/graphical branch, and
  why SSH-first headless remains the recommended admin path.
- The **versioning posture** — why the kernel stays at `0.83.0` instead of declaring
  SemVer `1.0.0`.

## Core Implementation

### The support matrix and its closed status vocabulary

The heart of the release gate is a **support matrix**: one row per shipped capability
class (boot+login, SSH, IPv4, storage, NICs, Wi-Fi, USB-HID, the graphical session,
audio, dynamic linking, multi-user). What makes it honest is a **closed status
legend** — every cell must use exactly one of `Supported`, `QEMU-validated`,
`Host-tested-only`, `Bare-metal-validated`, `Experimental`, or `Out-of-scope`, and
nothing else. Free-text like "works" is banned because it is unfalsifiable. This
borrows seL4's discipline, whose verified-platform table draws every cell from a fixed
legend so a QEMU-only proof can never silently masquerade as full hardware support.

The three middle tiers exist precisely to keep **QEMU-blind** hardware honest: the
mt76 Wi-Fi radio (no QEMU model), AHCI hot-plug/BOHC/SSS (no-ops under `ich9-ahci`),
and RTL8125 2.5 G silicon cannot be folded into plain `Supported` — they are
`Host-tested-only` (logic proven on the host) and/or `Bare-metal-validated` (proven
only via VFIO on real hardware).

### The gate bundle and the mutually-exhaustive evidence trail

A matrix is only a *promise*; the **gate bundle** is the *proof*. It enumerates the
exact `cargo xtask` commands that must pass, split into two classes: **env-gated
opt-in** gates (each carrying a `M3OS_*_REGRESSION` trigger quoted verbatim from
`AGENTS.md`) and **always-on** probes (`check`, `smoke-test`, `regression`, and the
screenshot probes). The **PASS-not-SKIP** rule generalizes a pattern the repo already
uses for `tls-smoke`/`dns-smoke`: a gate that *can* SKIP (musl absent, no QEMU device
model) must be run somewhere it actually PASSes before the claim is made, or the
matrix row is downgraded. A SKIP never counts as a PASS.

The **evidence trail** ties the two together with the phase's single most important
invariant: it is **mutually exhaustive**. Every `Supported`/`QEMU-validated` row names
≥ 1 backing gate (forward), and every gate maps back to a matrix row (reverse). A
reviewer can diff the two directions and find zero orphans — the analog of seL4's
`verification-manifest`. If the matrix ever lists a capability with no gate, or a gate
exercises nothing in the matrix, the document will lie on the next change.

### Screenshot-validated graphical evidence

Because the GUI is in 1.0 scope, and the serial-only smoke harness is **blind to
rendering**, every "the screen shows X" claim is backed by a QMP/PPM framebuffer
**screenshot** probe (`QmpClient::screendump` + `ppm` pixel assertions) or a non-silent
**WAV** capture for audio — never a serial sentinel. A serial `Wait` proves a program
*ran*, not that it *drew* anything. Where no screenshot probe exists yet (the greeter
login screen has none today), the row is `Experimental`, and building the missing probe
is recorded as a follow-up rather than smuggled into this no-new-code phase.

### The scope and versioning decisions

Two decisions are recorded with evidence rather than asserted:

- **Local-system in scope.** The graphical session is a `Supported`, screenshot-validated
  workflow on the strength of Phases 47 (full-screen workload), 56 (display+input),
  57 (audio + `session_manager`), 78 (USB-HID), and 80 (HDA). SSH-first headless stays
  the *recommended admin* path because that is the surface the project hardens most.
- **Phase-tracked `0.83.0`.** "1.0" is quality-bar language, not SemVer `1.0.0`. Per
  SemVer 2.0.0 item 4, `0.y.z` licenses an unstable public API; item 5 says `1.0.0`
  *defines* a stable public API — a promise m3OS cannot make while still adding whole
  subsystems (Wi-Fi and AHCI landed in Phases 81–82). The real `1.0.0` blocker is a
  **frozen public syscall/userspace ABI**, mirroring Redox (which gates 1.0 on a frozen
  `relibc`), SerenityOS (no numbered release), and Haiku (feature-complete cuts shipped
  as beta).

## Key Files

| File | Purpose |
|---|---|
| `docs/release/1.0-release-gate.md` | The authoritative release contract — status legend, support matrix, gate bundle, evidence trail, hardware tiering, non-goals, release checklist |
| `kernel/Cargo.toml` | The one source change: `version = "0.83.0"` (phase-tracked; banner/procfs/uname read `CARGO_PKG_VERSION`) |
| `AGENTS.md` | Source of truth for the `M3OS_*_REGRESSION` env-var triggers the gate bundle references verbatim |
| `xtask/src/qmp.rs`, `xtask/src/ppm.rs` | QMP keystroke injection + PPM screenshot plumbing behind every graphical evidence gate |

## How This Phase Differs From Later Release Work

- This phase introduces the **release contract and gate discipline** — the matrix,
  the evidence trail, the honesty tiering, and the versioning posture.
- A later phase will need to **freeze a public syscall/userspace ABI** before an actual
  SemVer `1.0.0` can be declared (the headline non-goal recorded here).
- Later phases (84 Spectre/KPTI, 85 toolchains, 86 GitHub, 87 Node.js, 88 Claude Code,
  89 IPv6/DHCPv6) are explicitly **1.x growth**, framed against this gate rather than
  allowed to hold the release hostage.
- A dedicated **greeter render probe** (so the greeter row can graduate from
  `Experimental` to `Supported`) is a tracked follow-up, deliberately not built here.

## Related Roadmap Docs

- [Phase 83 design doc](./roadmap/83-release-1-0-gate.md)
- [Phase 83 task list](./roadmap/tasks/83-release-1-0-gate-tasks.md)
- [Authoritative release-gate contract](./release/1.0-release-gate.md)
- [Phase 53 — Headless Hardening](./roadmap/53-headless-hardening.md) (the support boundary + gate bundle this phase extends)
- [Evaluation R10 — Release 1.0 and Beyond](./evaluation/roadmap/R10-release-1-0-and-beyond.md)

## Deferred or Later-Phase Topics

- Freezing a public syscall/userspace ABI for an eventual SemVer `1.0.0`.
- A greeter/login-screen render probe (graphical evidence gap).
- Broader hardware certification, distribution-style packaging, and CI/lab automation
  beyond what the support matrix requires.
- Large runtime ecosystems (Node.js, larger toolchains) as release blockers — they are
  1.x scope.
