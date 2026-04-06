# Phase 58 - Release 1.0 Gate

**Status:** Planned
**Source Ref:** phase-58
**Depends on:** Phase 52 (Headless Hardening) ✅, Phase 54 (Hardware Substrate) ✅
**Builds on:** Converts the convergence, hardening, and hardware work into an explicit release promise, while giving the project a disciplined place to decide whether the local-system branch is in scope for 1.0 or deferred to 1.x
**Primary Components:** docs/roadmap/README.md, README.md, docs/README.md, xtask validation flows, release and support-matrix documentation

## Milestone Goal

m3OS defines and validates what "1.0" actually means. The phase produces an explicit support matrix, release gates, non-goals, and documentation commitments for either a headless/reference 1.0 or a broader local-system milestone if the optional graphical branch is ready.

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

### Headless versus local-system decision

Make an explicit decision: either ship a headless/reference 1.0 on the strength of the earlier convergence phases, or include the optional local-system branch only if Phases 55-57 are complete enough to support the broader promise.

### Documentation and versioning discipline

Align the top-level docs, roadmap, learning-doc index, support notes, and version references with the chosen release definition.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Written support matrix with explicit non-goals | 1.0 without a promise is just a label |
| Validation gates tied to the promise | The release must be evidence-backed |
| Explicit headless vs local-system decision | The project must stop blurring those two outcomes |
| Documentation alignment | Release claims and docs must match the same shipped system |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Headless baseline | Phase 52 and Phase 54 are complete enough for a defensible headless/reference release | Pull missing validation or support-boundary work into this phase |
| Optional GUI baseline | If 1.0 is meant to include a local-system milestone, Phases 55-57 are complete enough to justify it | Otherwise explicitly defer the local-system branch to 1.x |
| Release-evidence baseline | The project can name the exact tests, targets, and docs that prove the claim | Add the missing release-gate automation or manual checklist items |
| Versioning baseline | The project agrees that the kernel crate version tracks the roadmap phase number even if the public release language says "1.0" | Add the missing versioning documentation and cross-reference updates |

## Important Components and How They Work

### Support matrix and release contract

The support matrix is the central artifact of the phase. It ties together hardware scope, validated workflows, release non-goals, and the public story the project can defend.

### Validation gate bundle

The validation gate bundle defines which commands and manual checks are required for the selected release promise. It is the operational proof behind the release contract.

### Documentation and version alignment

This phase succeeds only if top-level docs, subsystem docs, roadmap docs, and version references all tell the same story about the shipped system.

## How This Builds on Earlier Phases

- Builds on Phase 52's headless hardening and Phase 54's hardware promise as the minimum 1.0 foundation.
- Optionally includes the local-system milestones from Phases 55-57 if the project chooses the broader release target.
- Creates the stable boundary after which later ecosystem work can clearly be called 1.x growth instead of hidden release debt.

## Implementation Outline

1. Draft the support matrix and release non-goals for the headless/reference 1.0 story.
2. Decide whether the local-system branch is part of the same release or explicitly deferred.
3. Define the final validation gate bundle and evidence trail.
4. Align top-level docs, roadmap docs, and learning-doc indexes with the release promise.
5. Record the versioning policy and release communication posture.
6. Publish the release-gate checklist and the chosen support boundary.

## Learning Documentation Requirement

- Create `docs/58-release-1-0-gate.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the support matrix, validation gate bundle, headless-vs-local-system decision, and how the phase keeps scope honest.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `README.md`, `docs/README.md`, `docs/roadmap/README.md`, release notes, and any support-matrix documentation.
- Update `docs/evaluation/roadmap/README.md`, `docs/evaluation/roadmap/R10-release-1-0-and-beyond.md`, and any evaluation docs that describe release readiness.
- Update validation docs such as `docs/43c-regression-stress-ci.md` if the release gate changes how those results are interpreted.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.58.0`.

## Acceptance Criteria

- A written 1.0 support matrix exists with explicit supported workflows, hardware scope, and non-goals.
- The project has a documented validation bundle for the chosen release claim.
- The docs explicitly state whether 1.0 is headless/reference-only or also includes the local-system branch.
- Top-level docs, roadmap docs, and version references all reflect the same release promise.
- Later work such as toolchains, GitHub integration, Node.js, and Claude Code is explicitly framed as 1.x growth if not part of the chosen release.

## Companion Task List

- Phase 58 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature releases usually have much more automation, hardware lab coverage, packaging, and support staffing than m3OS should assume here.
- The important habit to borrow is disciplined promise-making, not industrial-scale release process.
- A small but honest 1.0 is more valuable than a sprawling roadmap that never becomes a stable release.

## Deferred Until Later

- Broader hardware certification and distribution-style packaging promises
- Large runtime ecosystems as release blockers
- A full desktop claim if the local-system branch is not yet ready
- Advanced CI/lab automation beyond what the support matrix requires
