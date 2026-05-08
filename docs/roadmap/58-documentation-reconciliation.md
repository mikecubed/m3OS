# Phase 58 — Documentation Reconciliation Pass

**Status:** Planned
**Source Ref:** phase-58
**Depends on:** Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Builds on:** All phases 1–57e. This phase does not add kernel features; it brings the entire doc corpus into agreement with the audited state of the codebase as of the 2026-05-08 audit.
**Primary Components:** `docs/roadmap/` (all phase design docs and task docs), `docs/roadmap/tasks/` (task-doc corpus), `AGENTS.md`, `docs/06-ipc.md`, `docs/16-network.md`, `docs/22-tty-terminal.md`, `docs/roadmap/README.md`, `docs/evaluation/`, `docs/shell/`, `docs/handoff/`, `docs/handoffs/`

## Milestone Goal

A single PR that walks every phase design doc and task doc in the roadmap, flips Status fields to match the audited reality, supplies missing task docs and design docs, converts legacy table-format task docs to checkbox format, retires stale post-1.0 top-level docs, and consolidates the split `docs/handoff/` / `docs/handoffs/` directories. After this phase, the roadmap README is a reliable source of truth and no design-doc Status field contradicts its companion task doc or the README.

## Why This Phase Exists

The 2026-05-08 audit found sixteen distinct categories of doc-versus-reality drift: design docs marked "Planned" for phases whose code landed months ago, task docs with universally unchecked checkboxes for phases declared Complete, missing task docs, missing design docs, legacy learning docs with stale body content, seven top-level post-1.0 planning docs still referencing a Phase 32 baseline, a split handoff directory, and evaluation docs scoped to v0.47.0. Each discrepancy reduces the README's trustworthiness as a release-gate checklist. Phases 59–62 close functional gaps; this phase is a prerequisite because it establishes which functional gaps are real and which are documentation failures.

## Learning Goals

- How to audit a roadmap corpus for status consistency across design docs, task docs, and a README table.
- How to write a retroactive task doc that faithfully reconstructs completion evidence from source history.
- What makes a checkbox "checked" in a phase task doc — code reference, test reference, or explicit deferral note.

## Feature Scope

### Track A — Status Reconciliation

Flip `Status:` fields in design docs for all phases where the field contradicts the README, AGENTS.md, or downstream dependency chains. The primary targets from the audit are:
- Phases 55a, 55b, 56, 57a, 57d: design docs read "Planned" while all other sources treat them as Complete.
- Phase 57b: drop the stale "pending soak (PR #132)" qualifier (the soak result belongs to Phase 59 Track G and Phase 62 Track F).
- Phase 22b design doc: must be created before its Status can be set.
- Phase 42b design doc: must be created before its Status can be set.
- Phase 35 design doc: add missing `Status:` and `Source Ref:` header fields.

### Track B — Missing Task Docs

Write retroactive task docs for phases that lack them entirely. Phase 13 (Writable FS) has acceptance criteria in its design doc but no task doc — the README explicitly says "Tasks: not yet created." Phase 51 (Service Model Maturity) has a design doc but no task doc and no closure record.

### Track C — Missing Design Docs

Write the two design docs flagged by the audit as absent template violations. Phase 22b (ANSI Parser Enhancement) has only a task doc. Phase 42b (Async Executor) has only a task doc and is referenced by Phase 43b/43c/46/47.

### Track D — Stale Legacy Content

Update body content in legacy learning docs where the text contradicts current implementation:
- `docs/16-network.md`: still describes the network stack as kernel-mode-temporary; Phase 54 migrated UDP policy to userspace.
- `docs/22-tty-terminal.md`: still describes PTY as "skeleton stubs"; Phase 29 implemented the full subsystem.
- `docs/06-ipc.md`: Supersedes field references nonexistent `docs/06-ipc-core.md`.
- `docs/56-display-and-input-architecture.md` (legacy): Status field reads "Planned" while roadmap treats Phase 56 as Complete.

### Track E — Top-Level Post-1.0 Docs Cleanup

Seven top-level docs (`clang-llvm-roadmap.md`, `claude-code-roadmap.md`, `git-roadmap.md`, `github-cli-roadmap.md`, `nodejs-roadmap.md`, `python-roadmap.md`, `rust-crate-acceleration.md`) still reference a Phase 32 baseline, ~20 phases stale. Either fold the relevant content into the appropriate phase appendix or move to an archived subdirectory with a dated note. `rust-crate-acceleration.md` is fully superseded by completed Phases 41–47 and should be retired outright.

### Track F — Subdirectory Consolidation

`docs/handoff/` (singular, 1 file) and `docs/handoffs/` (plural, 21 files) are parallel directories. Move the singular file into the plural directory, update all cross-references, and remove the now-empty singular directory. Additionally: refresh or archive `docs/evaluation/` (scoped to v0.47.0, ~10 phases stale) and `docs/shell/brush-integration-analysis.md` (2026-03-26, ~15 phases stale).

### Track G — Validation

Walk every design-doc header that was changed in Tracks A–F and confirm the result is internally consistent: Status agrees with the task-doc's Track Layout table, Source Ref is present, Depends-on list uses ✅ for all completed predecessors, and the roadmap README row reflects the final status.

## Important Components and How They Work

### `docs/roadmap/` design-doc corpus

Each file follows the template in `docs/appendix/doc-templates.md`. The top five required header fields (`Status`, `Source Ref`, `Depends on`, `Builds on`, `Primary Components`) are the machine-checkable part of the template; the remaining sections must be present but may be minimal for retroactive docs. Track A work edits only the `Status:` line plus (for 57b) the post-soak qualifier.

### Task-doc checkbox format

The template defines `- [x]` for verified items and `- [ ]` for unverified or deferred items. Phase 16's current task doc uses pipe tables with no checkboxes — converting it means restructuring P16-T001 through P16-T073 into the checkbox format with explicit `[x]` (verified from code history) or `[ ] — Deferred: <owner>` (genuinely not done). Phase 13's task doc must be written from scratch using the design doc's five acceptance criteria.

### `docs/roadmap/README.md` table

The README row template requires: Phase, Theme, Primary Outcome, Status, Source Ref, Milestone link, Tasks link. Every status change in Track A must propagate to the matching README row.

### `AGENTS.md` version string

Currently reads v0.51.0; current kernel version is 0.57.x. This is an explicit acceptance item in Phase 54a Task C.4 (still Planned). Updating it here closes the five-minute item and keeps AGENTS.md consistent with the rest of the doc corpus.

### Phase 55a "Known Open Bug" section

Phase 55a's design doc reports the VT-d MMIO `CTRL.RST` issue as open. Phase 55c's task list claims it was closed in R2. Track A adds a cross-reference closure note to 55a's Known Open Bug section.

## How This Builds on Earlier Phases

- Does not extend any kernel feature — purely the documentation layer.
- Extends the doc-template compliance established in earlier phases by retroactively applying it to the phases (13, 16, 22b, 42b) that shipped before strict template compliance was enforced.
- Provides the stable doc baseline that Phases 59–62 depend on to mark their own source-phase task docs as closed.

## Implementation Outline

1. Run Track A: edit Status fields in the six drifted design docs (55a, 55b, 56, 57a, 57d, 57b) and Phase 35's missing header fields.
2. Write Phase 51 task doc (Track B.1); write Phase 13 task doc (Track B.2).
3. Write Phase 22b design doc (Track C.1); write Phase 42b design doc (Track C.2).
4. Walk Phases 42b, 43b, 43c, 46, 47 task docs: for each unchecked item, either flip to `[x]` with a code/test citation, or convert to `[ ] — Deferred: <phase>` with an explicit owner (Track A.2 closure).
5. Reconcile Phase 19 design doc (Complete) versus task doc (all six tracks "Not started"): read `kernel/src/signal/` and flip appropriate checkboxes or demote design-doc status (Track A.1 closure).
6. Update `AGENTS.md` version string to v0.57.x (Track A.3).
7. Fix `docs/06-ipc.md` Supersedes field; refresh `docs/16-network.md` and `docs/22-tty-terminal.md` body content; fix `docs/56-display-and-input-architecture.md` Status (Track D).
8. Run Track E: archive or fold the seven post-1.0 top-level docs.
9. Run Track F: move `docs/handoff/` file into `docs/handoffs/`, update cross-references, archive `docs/evaluation/` and `docs/shell/brush-integration-analysis.md`.
10. Run Track G: scan all edited headers for consistency with the roadmap README; update README rows.

## Acceptance Criteria

- Every design-doc `Status:` field agrees with the corresponding README row and the majority of downstream dependency markers.
- No task doc is in "not yet created" state for a phase marked Complete in the README.
- Phases 42b, 43b, 43c, 46, 47 task docs have no universally-unchecked tracks without explicit deferral notes.
- Phase 19 design-doc Status and task-doc Track Layout table are internally consistent.
- Phase 13 task doc exists with at minimum the five acceptance criteria from the design doc, each checked or explicitly deferred.
- Phase 16 task doc items are in `[x]`/`[ ]` format.
- Phase 22b design doc exists with all required header fields.
- Phase 42b design doc exists with all required header fields.
- Phase 35 design doc has `Status:` and `Source Ref:` fields.
- `AGENTS.md` version string reads v0.57.x or later.
- `docs/06-ipc.md` Supersedes field references an existing file.
- `docs/handoff/` (singular) directory no longer exists as a separate path.
- `docs/evaluation/` is either refreshed to current phase baseline or contains an explicit archive note with date.
- Seven top-level post-1.0 docs are either archived or folded; none still reference Phase 32 as "today".

## Companion Task List

- [Phase 58 Task List](./tasks/58-documentation-reconciliation-tasks.md)

## How Real OS Implementations Differ

- Large projects (Linux, FreeBSD) have automated tools (scripts, CI) that enforce doc-versus-source consistency. m3OS does not; this phase performs the manual equivalent.
- Release-gated projects require changelog entries for every status change. m3OS uses the git log as its changelog; this phase adds commit-message discipline for doc changes.
- Kernel projects that ship to multiple distributors maintain formal errata lists. m3OS's equivalent is the audit's `06-pre-1.0-blocker-list.md`; this phase closes the documentation tier of that list.

## Deferred Until Later

- Actually running validation tests that would verify unchecked checkboxes — that is Phase 59's scope.
- Closing functional gaps in Phases 33, 35, 57a — those are Phases 60, 61, 62.
- Implementing the `doc-check` CI hook that would prevent future Status drift — post-1.0.
- Writing missing task docs for phases marked Complete that use the no-checkbox table format (Phases 17, 18, 20, 22, 23, 25, 28, 29) but were not flagged as red flags — post-1.0 backlog.
