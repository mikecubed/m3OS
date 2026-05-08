# Phase 58 — Documentation Reconciliation Pass: Task List

**Status:** Planned
**Source Ref:** phase-58
**Depends on:** Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Goal:** Walk every phase design doc and task doc, flip Status fields to match the 2026-05-08 audit findings, write missing task docs and design docs, convert legacy table-format task docs to checkbox form, retire stale top-level post-1.0 planning docs, fix stale legacy learning doc body content, and consolidate the split handoff directories. After this phase the roadmap README is internally consistent and no design-doc Status field contradicts its companion task doc.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Status reconciliation — flip drifted design-doc Status fields | — | Planned |
| B | Missing task docs — write Phase 13 and Phase 51 task docs | A | Planned |
| C | Missing design docs — write Phase 22b and Phase 42b design docs | A | Planned |
| D | Stale legacy content — fix body content and Supersedes fields | A | Planned |
| E | Top-level post-1.0 docs cleanup — archive or fold seven docs | — | Planned |
| F | Subdirectory consolidation — merge handoff dirs, archive evaluation | — | Planned |
| G | Validation — confirm all edited headers are internally consistent | A B C D E F | Planned |

---

## Track A — Status Reconciliation

### A.1 — Reconcile Phase 19 design doc (Complete) vs. task doc (all six tracks Not started)

**Files:**
- `docs/roadmap/19-signal-handlers.md`
- `docs/roadmap/tasks/19-signal-handlers-tasks.md`

**Symbol:** `Status:` header field in both docs
**Why it matters:** The design doc declares Complete; the task doc's Track Layout shows all six tracks Not started. Either signals work and the task doc is a documentation failure, or they don't and Phase 19 must be demoted — which would cascade to Phases 21, 29, 30, 43.

**Acceptance:**
- [ ] Read `kernel/src/signal/` and the signal-related syscalls in `kernel/src/arch/x86_64/syscall/mod.rs` and identify which of the six tracks (A: mask, B: sigframe, C: sigreturn, D: sigaltstack, E: rt_sigaction, F: validation) have landed code.
- [ ] For each track with landed code, flip the corresponding checkboxes to `[x]` with a file+symbol citation.
- [ ] For any track with no landed code, convert to `[ ] — Deferred: post-1.0` with an explanation.
- [ ] Design-doc `Status:` field agrees with the resulting task-doc Track Layout.

### A.2 — Flip universally-unchecked task docs for Phases 42b, 43b, 43c, 46, 47

**Files:**
- `docs/roadmap/tasks/42b-async-executor-tasks.md`
- `docs/roadmap/tasks/43b-kernel-trace-ring-tasks.md`
- `docs/roadmap/tasks/43c-regression-stress-ci-tasks.md`
- `docs/roadmap/tasks/46-system-services-tasks.md`
- `docs/roadmap/tasks/47-doom-tasks.md`

**Symbol:** checkbox items throughout all five docs
**Why it matters:** AGENTS.md cites syslogd, crond, the service manager, the kernel trace ring, and DOOM as shipped. The task docs have zero machine-verifiable record of completion.

**Acceptance:**
- [ ] Each task doc's items have been walked against the codebase; genuinely-done items are `[x]` with a file+symbol citation.
- [ ] Genuinely-not-done items carry `[ ] — Deferred: <owner phase>` and an explanation.
- [ ] No track in any of the five docs remains universally unchecked without at least one deferral note.
- [ ] Each design doc's `Status:` field agrees with the resulting task-doc Track Layout.

### A.3 — Flip Status fields for Phases 55a, 55b, 56, 57a, 57b, 57d

**Files:**
- `docs/roadmap/55a-iommu-substrate.md`
- `docs/roadmap/55b-ring-3-driver-host.md`
- `docs/roadmap/56-display-and-input-architecture.md`
- `docs/roadmap/57a-scheduler-rewrite.md`
- `docs/roadmap/57b-preemption-foundation.md`
- `docs/roadmap/57d-voluntary-preemption.md`

**Symbol:** `Status:` header field
**Why it matters:** All six design docs read "Planned" while AGENTS.md, the README, and all downstream dependency chains treat them as Complete. The drift is four-deep for Phase 56 alone.

**Acceptance:**
- [ ] 55a, 55b, 56, 57a, 57d: `Status:` changed to `Complete`.
- [ ] 57b: `Status:` changed to `Complete`; stale "pending soak (PR #132)" qualifier removed or replaced with a one-line note pointing to Phase 59 Track G for soak result.
- [ ] Phase 55a's "Known Open Bug" section receives a cross-reference noting that 55c R2 claims closure of the VT-d MMIO `CTRL.RST` issue.
- [ ] Roadmap README rows for 55a, 55b, 56, 57a, 57b, 57d updated to match.

### A.4 — Add missing header fields to Phase 35 design doc and update AGENTS.md

**Files:**
- `docs/roadmap/35-true-smp-multitasking.md`
- `AGENTS.md`

**Symbol:** `Status:`, `Source Ref:` in Phase 35; version string in AGENTS.md
**Why it matters:** Phase 35's design doc violates the template (missing two required header fields). AGENTS.md version string reads v0.51.0; current kernel is 0.57.x. Updating AGENTS.md closes Phase 54a Task C.4.

**Acceptance:**
- [ ] `docs/roadmap/35-true-smp-multitasking.md` has `Status:` and `Source Ref: phase-35` as the first two header lines.
- [ ] `AGENTS.md` version string is updated to v0.57.x or later.

---

## Track B — Missing Task Docs

### B.1 — Write Phase 13 task doc

**File:** `docs/roadmap/tasks/13-writable-fs-tasks.md`
**Symbol:** (new file)
**Why it matters:** Phase 13 (Writable FS) is the only phase among the 60+ that has a design doc but no task doc. The README explicitly says "Tasks: not yet created". Phase is marked Complete.

**Acceptance:**
- [ ] File `docs/roadmap/tasks/13-writable-fs-tasks.md` exists with all required header fields (`Status`, `Source Ref`, `Depends on`, `Goal`).
- [ ] Track Layout table lists at minimum the tracks implied by the design doc's five acceptance criteria.
- [ ] Each acceptance criterion from the design doc appears as a checkbox item with a file+symbol citation or explicit deferral note.
- [ ] README row for Phase 13 updated to link the new task doc.

### B.2 — Write or close Phase 51 task doc

**Files:**
- `docs/roadmap/51-service-model-maturity.md`
- `docs/roadmap/tasks/51-service-model-maturity-tasks.md` (new or merged)

**Symbol:** (new file, or explicit merge note in 51 design doc)
**Why it matters:** Phase 51 is marked In Progress with no task doc. Phase 52 and later phases list 51 as a dependency but it was never formally closed. Either write the task doc and run acceptance gates, or add a merge paragraph to the 51 design doc explaining the scope folded into 52d and mark it Complete.

**Acceptance:**
- [ ] Either a task doc `51-service-model-maturity-tasks.md` exists with checkboxes walked, OR the 51 design doc has an explicit closure paragraph stating which acceptance items folded into which later phase.
- [ ] Phase 51 `Status:` field is either `Complete` (if items verified) or `Deferred: see Phase 52d` (if merged).
- [ ] Phase 52 design doc has a companion closure note consistent with the Phase 51 disposition.
- [ ] README row for Phase 51 updated to reflect final status.

---

## Track C — Missing Design Docs

### C.1 — Write Phase 22b design doc

**File:** `docs/roadmap/22b-ansi-parser-enhancement.md`
**Symbol:** (new file)
**Why it matters:** Phase 22b has only a task doc. The template requires a companion design doc. Without it, the phase's rationale, feature scope, and implementation choices are not recorded.

**Acceptance:**
- [ ] File `docs/roadmap/22b-ansi-parser-enhancement.md` exists with all 14 template sections populated (not stubbed).
- [ ] Required header fields present: `Status`, `Source Ref: phase-22b`, `Depends on`, `Builds on`, `Primary Components`.
- [ ] README row for Phase 22b updated to link the new design doc.

### C.2 — Write Phase 42b design doc

**File:** `docs/roadmap/42b-async-executor.md`
**Symbol:** (new file)
**Why it matters:** Phase 42b has only a task doc. Downstream phases (43b, 43c, 46, 47) depend on 42b's async executor infrastructure; without a design doc the rationale and interface contract are unrecorded.

**Acceptance:**
- [ ] File `docs/roadmap/42b-async-executor.md` exists with all 14 template sections populated.
- [ ] Required header fields present: `Status`, `Source Ref: phase-42b`, `Depends on`, `Builds on`, `Primary Components`.
- [ ] README row for Phase 42b updated to link the new design doc.

---

## Track D — Stale Legacy Content

### D.1 — Fix `docs/06-ipc.md` Supersedes field

**File:** `docs/06-ipc.md`
**Symbol:** `Supersedes Legacy Doc:` header field
**Why it matters:** The field references `docs/06-ipc-core.md`, which does not exist. Readers following the cross-reference get a 404.

**Acceptance:**
- [ ] `Supersedes Legacy Doc:` field updated to reference the actual existing file (likely `docs/roadmap/06-ipc-core.md` or removed if no predecessor exists).
- [ ] No broken cross-reference remains in the file.

### D.2 — Refresh `docs/16-network.md` body content

**File:** `docs/16-network.md`
**Symbol:** body prose (Phase 54 migration section)
**Why it matters:** Still describes the network stack as kernel-mode-temporary. Phase 54 migrated UDP policy to a userspace `udp_server`; the doc has not been updated.

**Acceptance:**
- [ ] Body content no longer describes the network stack as kernel-mode-temporary.
- [ ] Phase 54 migration to userspace UDP server is acknowledged.
- [ ] `Status:` and `Aligned Roadmap Phase:` header fields are accurate.

### D.3 — Refresh `docs/22-tty-terminal.md` body content

**File:** `docs/22-tty-terminal.md`
**Symbol:** body prose (PTY implementation section)
**Why it matters:** Still describes PTY as "skeleton stubs". Phase 29 implemented the full PTY subsystem including `openpty`, `tcsetattr`, and job-control plumbing.

**Acceptance:**
- [ ] Body content no longer describes PTY as skeleton stubs.
- [ ] Phase 29 full PTY subsystem is described accurately.
- [ ] `Status:` header agrees with Phase 29/22 completion state.

### D.4 — Fix `docs/56-display-and-input-architecture.md` (legacy) Status field

**File:** `docs/56-display-and-input-architecture.md`
**Symbol:** `Status:` header field
**Why it matters:** Reads "Planned" while both the roadmap design doc and AGENTS.md treat Phase 56 as Complete. This is the fourth location of Phase 56 status drift.

**Acceptance:**
- [ ] `Status:` field updated to `Complete`.
- [ ] No other header field in the file contradicts the Phase 56 completion state.

---

## Track E — Top-Level Post-1.0 Docs Cleanup

### E.1 — Archive or fold the seven post-1.0 planning docs

**Files:**
- `docs/clang-llvm-roadmap.md`
- `docs/claude-code-roadmap.md`
- `docs/git-roadmap.md`
- `docs/github-cli-roadmap.md`
- `docs/nodejs-roadmap.md`
- `docs/python-roadmap.md`
- `docs/rust-crate-acceleration.md`

**Symbol:** entire file content
**Why it matters:** All seven still reference "Today (Phase 32)" as their baseline, ~20 phases stale. `rust-crate-acceleration.md` is fully superseded by completed Phases 41–47.

**Acceptance:**
- [ ] `docs/rust-crate-acceleration.md` is either deleted or moved to `docs/archived/` with a dated note explaining supersession by Phases 41–47.
- [ ] The remaining six docs are either (a) updated with a current-phase baseline, (b) moved to `docs/archived/` with a dated note, or (c) folded into the relevant phase appendix.
- [ ] None of the seven files remaining in `docs/` still references "Phase 32" as current.

---

## Track F — Subdirectory Consolidation

### F.1 — Merge `docs/handoff/` into `docs/handoffs/`

**Files:**
- `docs/handoff/` (singular — 1 file)
- `docs/handoffs/` (plural — 21 files)

**Symbol:** directory structure and all cross-reference links
**Why it matters:** Two parallel directories exist for the same content type. The singular has 1 file, the plural has 21. Every internal doc that links to `docs/handoff/` gets a broken reference after the merge.

**Acceptance:**
- [ ] File from `docs/handoff/` moved into `docs/handoffs/`.
- [ ] `docs/handoff/` directory removed.
- [ ] Every cross-reference in the corpus to `docs/handoff/` updated to `docs/handoffs/`.
- [ ] `grep -r 'docs/handoff[^s]'` returns no results in the repo root.

### F.2 — Archive `docs/evaluation/` and `docs/shell/brush-integration-analysis.md`

**Files:**
- `docs/evaluation/` directory
- `docs/shell/brush-integration-analysis.md`

**Symbol:** README or index entry in each
**Why it matters:** `docs/evaluation/README.md` is scoped to v0.47.0 (~10 phases stale). `docs/shell/brush-integration-analysis.md` is dated 2026-03-26 (~15 phases stale). Both mislead readers about the current state.

**Acceptance:**
- [ ] `docs/evaluation/README.md` either updated with a v0.57.x scope note or amended with a prominent "Archived — v0.47.0 era" header.
- [ ] `docs/shell/brush-integration-analysis.md` either updated or amended with a prominent "Archived — 2026-03-26" header and a pointer to the current shell documentation.

---

## Track G — Validation

### G.1 — Confirm all edited design-doc headers are internally consistent

**Files:** all design docs edited in Tracks A–C
**Symbol:** `Status:`, `Source Ref:`, `Depends on:` fields
**Why it matters:** Each edit in Tracks A–C touches a header field that must agree with at minimum three other locations: the task doc's Track Layout, the roadmap README row, and the Depends-on list in downstream phases.

**Acceptance:**
- [ ] For every design doc edited: `Status:` matches the roadmap README row.
- [ ] For every design doc edited: `Source Ref:` is present and follows the `phase-NN` convention.
- [ ] For every design doc edited: every entry in `Depends on:` that references a completed phase carries a `✅`.
- [ ] Roadmap README rows updated for all phases changed in this phase.
- [ ] `cargo xtask check` (clippy + rustfmt) passes — no source files were changed by this phase, but verify no doc changes introduced broken links in any Rust doc-comment.

---

## Documentation Notes

- This phase produces no Rust source changes. All changes are in `docs/` and `AGENTS.md`.
- When flipping checkboxes in Track A.2, prefer citing the specific file and function over a broad "this phase shipped". For example: `[x] Scheduler rewrite — see \`kernel/src/task/scheduler.rs::block_current_until\`` is better than `[x] Done`.
- Deferral notes in Track A.2 must name a specific owner phase or explicitly say "post-1.0" — "deferred" alone is insufficient.
- The Phase 51 disposition (B.2) sets a precedent for how to handle phases where the code shipped but the formal closure record was skipped.
- Track F changes to directory structure require updating cross-references in audit docs under `docs/appendix/audit-status/` as well as design and task docs.
