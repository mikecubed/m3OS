# Phase 83 - Release 1.0 Gate

**Status:** Planned
**Source Ref:** phase-83
**Depends on:** Phase 53 (Headless Hardening) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅, Phase 75 (W^X Enforcement) ✅, Phase 77 (Pre-1.0 Correctness, Cheap Security, and Network Polish) ✅, Phase 78 (USB Host Foundation) ✅, Phase 79 (Modern Intel/Realtek NIC) ✅, Phase 80 (Intel HDA Audio) ✅; capability evidence from Phase 56/57 (display/input + audio + session) ✅, Phase 81 (Wi-Fi mt792x) and Phase 82 (AHCI/SATA) ✅. *(Phase 65 `fat_server` is referenced only as a known-limitation — FAT32 remains an ENOSYS stub — not a release blocker.)*
**Builds on:** Converts the convergence, hardening, and hardware work into an explicit release promise; the local-system/graphical branch is **included** in the 1.0 promise (the GUI stack is now in-tree and screenshot-validated), with SSH-first headless as the recommended admin path
**Primary Components:** docs/roadmap/README.md, README.md, docs/README.md, xtask validation flows, release and support-matrix documentation

## Milestone Goal

m3OS defines and validates what "1.0" actually means. The phase produces an explicit support matrix, release gates, non-goals, and documentation commitments for a 1.0 that **includes the local-system/graphical branch** (greeter → login → compositor → `term`/launcher/bar), screenshot-validated, with SSH-first headless remaining the recommended admin path.

## Why This Phase Exists

Roadmaps often assume the meaning of "1.0" instead of writing it down. That is especially dangerous in a project with both a serious headless/reference story and a tempting future local desktop story. Without an explicit release gate, feature growth can quietly become the definition of success.

This phase exists to force the project to make and document the release decision instead of drifting into it.

## Learning Goals

- Understand why release engineering is an architectural discipline rather than an administrative afterthought.
- Learn how support matrices, validation gates, and non-goals protect a project from uncontrolled scope.
- See how a headless/reference 1.0 and a local-system 1.0 can share groundwork while still being different promises.
- Understand how documentation quality becomes part of the release artifact.

## Feature Scope

### Release contract and support matrix

Define what m3OS supports at 1.0, on which targets, with which workflows, and with which explicit non-goals. This contract should be narrow, defensible, and aligned with the shipped validation path.

### Validation gates and evidence

Tie the supported promise to repeatable validation. The release process should say which smoke, regression, recovery, and hardware checks must pass before the project claims 1.0 readiness.

### Headless and local-system scope (decided: local-system included)

The decision is settled: 1.0 **includes** the local-system/graphical branch on the strength of the now-complete graphical stack — Phase 56 (`display_server` + focus-aware input), Phase 57 (audio + `session_manager`), and Phase 47's full-screen workload, plus Phase 78 USB-HID input and Phase 80 HDA audio. The graphical session (greeter → login → compositor → `term`/launcher/bar) is a **screenshot-validated** supported workflow, while SSH-first headless remains the **recommended admin** path. (The original design predated the GUI stack being planned; this section records the reconciled decision.)

### Documentation and versioning discipline

Align the top-level docs, roadmap, learning-doc index, support notes, and version references with the chosen release definition.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Written support matrix with explicit non-goals | 1.0 without a promise is just a label |
| Validation gates tied to the promise | The release must be evidence-backed |
| Recorded scope decision (local-system in scope) | The settled decision — 1.0 includes the graphical branch, SSH-first headless recommended — must be written down, not drifted into |
| Documentation alignment | Release claims and docs must match the same shipped system |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Headless baseline | Phase 53 headless/reference gates (see [Phase 53 § Gate Bundle](./53-headless-hardening.md#gate-bundle)) pass on a post-Phase 53a image, and the Phase 55c ring-3 driver correctness closure is complete for the shipped hardware story | Pull missing validation or support-boundary work into this phase |
| Local-system (GUI) baseline | 1.0 **includes** the local-system branch: Phase 47, 56, 57 (plus Phase 78 USB-HID and Phase 80 HDA) are complete enough to justify it, and each graphical workflow is screenshot-validated (QMP/PPM), not serial-sentinel "proved" | Add the missing screenshot-validated gate for any graphical row claimed `Supported` |
| Release-evidence baseline | The project can name the exact tests, targets, and docs that prove the claim — these are the Phase 53 gate bundle plus the Phase 55 / 55c hardware and ring-3-driver closure gates that back the supported-driver story | Add the missing release-gate automation or manual checklist items |
| Versioning baseline | The project agrees that the kernel crate version tracks the roadmap phase number even if the public release language says "1.0" | Add the missing versioning documentation and cross-reference updates |

## Important Components and How They Work

### Support matrix and release contract

The support matrix is the central artifact of the phase. It starts from the
bounded [Phase 53 support boundary](./53-headless-hardening.md#support-boundary)
— QEMU x86_64 with OVMF, SSH-first remote admin, shipped ports and Rust std
path — and extends it only where the Phase 55 hardware work and Phase 55c
correctness closure add new supported targets or stronger evidence for the
ring-3 driver story. The matrix ties together hardware scope, validated
workflows, release non-goals, and the public story the project can defend.

### Validation gate bundle

The validation gate bundle starts from the
[Phase 53 gate bundle](./53-headless-hardening.md#gate-bundle) (exact `cargo
xtask` commands and manual operator checks) and adds the Phase 55
hardware-specific gates plus the Phase 55c closure evidence for SSH-over-e1000
wake correctness, `--iommu` device-smoke parity, and driver-restart `EAGAIN`
visibility. It defines which commands and manual checks are required for the
selected release promise and serves as the operational proof behind the release
contract.

### Documentation and version alignment

This phase succeeds only if top-level docs, subsystem docs, roadmap docs, and version references all tell the same story about the shipped system.

## How This Builds on Earlier Phases

- Builds on Phase 53's bounded headless/reference baseline — the support boundary, gate bundle, and closure contract are fixed inputs, not re-opened scope.
- Builds on Phase 55's hardware promise and Phase 55c's ring-3 driver correctness closure as additional supported-target evidence beyond the Phase 53 QEMU reference.
- Includes the local-system milestones from Phase 47, 56, and 57 (plus Phase 78 USB-HID and Phase 80 HDA) as a screenshot-validated supported workflow — the settled release-scope decision, not an optional branch.
- Creates the stable boundary after which later ecosystem work can clearly be called 1.x growth instead of hidden release debt.
- Inherits the Phase 53/53a closure rule: Phase 53 gates must have already passed on the post-53a allocator baseline before Phase 83 can close.

## Implementation Outline

1. Draft the support matrix and release non-goals for the headless/reference 1.0 story.
2. Record the settled release-scope decision: the local-system/graphical branch is **included** in 1.0 (screenshot-validated), with SSH-first headless as the recommended admin path.
3. Define the final validation gate bundle and evidence trail.
4. Align top-level docs, roadmap docs, and learning-doc indexes with the release promise.
5. Record the versioning policy and release communication posture.
6. Publish the release-gate checklist and the chosen support boundary.

## Learning Documentation Requirement

- Create `docs/83-release-1-0-gate.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the support matrix, validation gate bundle, headless-vs-local-system decision, and how the phase keeps scope honest.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `README.md`, `docs/README.md`, `docs/roadmap/README.md`, release notes, and any support-matrix documentation.
- Update `docs/evaluation/roadmap/README.md`, `docs/evaluation/roadmap/R10-release-1-0-and-beyond.md`, and any evaluation docs that describe release readiness.
- Update validation docs such as `docs/43c-regression-stress-ci.md` if the release gate changes how those results are interpreted.
- When the phase lands, bump `kernel/Cargo.toml` from `0.82.0` to **`0.83.0`** (the `0.NN.0 = Phase NN` convention). Per the Versioning baseline above, the kernel crate stays phase-tracked; **"1.0" is public quality-bar language, not a SemVer `1.0.0` crate version.** A literal `1.0.0` would assert a stable public ABI the system does not yet freeze (it is still adding whole subsystems — Wi-Fi and AHCI landed in Phases 81–82) and would break the phase→version mapping. The eventual SemVer `1.0.0` blocker (a frozen syscall/userspace ABI, à la Redox isolating its promise in `relibc`) is recorded as a non-goal.

## Acceptance Criteria

- A written 1.0 support matrix exists that starts from the Phase 53 bounded headless/reference baseline, with explicit supported workflows, hardware scope, and non-goals.
- The project has a documented validation bundle that extends the Phase 53 gate bundle with the Phase 55 hardware-specific gates plus the Phase 55c closure evidence for SSH-over-e1000 wake correctness, `--iommu` device-smoke parity, and userspace-visible restart handling.
- The docs explicitly record that 1.0 **includes the local-system/graphical branch** (greeter → login → compositor → `term`/launcher/bar, USB-HID input, HDA/AC'97 audio), screenshot-validated, with SSH-first headless as the recommended admin path.
- Top-level docs, roadmap docs, and version references all reflect the same release promise.
- Later work such as toolchains, GitHub integration, Node.js, and Claude Code is explicitly framed as 1.x growth if not part of the chosen release.
- Phase 53 gates have already passed on the post-53a allocator baseline (the Phase 53/53a closure rule is a prerequisite, not re-evaluated here).

## Companion Task List

- [Phase 83 Task List](./tasks/83-release-1-0-gate-tasks.md)

## How Real OS Implementations Differ

- Mature releases usually have much more automation, hardware lab coverage, packaging, and support staffing than m3OS should assume here.
- The important habit to borrow is disciplined promise-making, not industrial-scale release process.
- A small but honest 1.0 is more valuable than a sprawling roadmap that never becomes a stable release.

## Deferred Until Later

- Broader hardware certification and distribution-style packaging promises
- Large runtime ecosystems as release blockers
- A full general-purpose desktop claim (broad app ecosystem, window-management parity) beyond the screenshot-validated greeter → compositor → `term`/launcher/bar session that 1.0 ships
- Advanced CI/lab automation beyond what the support matrix requires
