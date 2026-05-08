# Phase 58 — Documentation Reconciliation Pass: Task List

**Status:** Planned
**Source Ref:** phase-58
**Depends on:** Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Goal:** Walk every phase design doc and task doc, flip Status fields to match the 2026-05-08 audit findings, write missing task docs and design docs, convert legacy table-format task docs to checkbox form, retire stale top-level post-1.0 planning docs, fix stale legacy learning doc body content, and consolidate the split handoff directories. After this phase the roadmap README is internally consistent and no design-doc Status field contradicts its companion task doc.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Status reconciliation — flip drifted design-doc Status fields | — | Planned |
| B | Missing/stale task docs — write Phase 13 and Phase 51 task docs; convert Phase 16 to checkbox format | A | Planned |
| C | Missing design docs — write Phase 22b and Phase 42b design docs | A | Planned |
| D | Stale legacy content — fix body content and Supersedes fields | A | Planned |
| E | Top-level post-1.0 docs cleanup — archive or fold seven docs | — | Planned |
| F | Subdirectory consolidation — merge handoff dirs, archive evaluation | — | Planned |
| G | Validation — confirm all edited headers are internally consistent | A B C D E F | Planned |
| H | Documentation and Release | G | Planned |

---

## Track A — Status Reconciliation

### A.1 — Reconcile Phase 19 design doc (Complete) vs. task doc (all six tracks Not started)

**Files:**
- `docs/roadmap/19-signal-handlers.md`
- `docs/roadmap/tasks/19-signal-handlers-tasks.md`

**Symbol:** `Status:` header field in both docs
**Why it matters:** The design doc declares Complete; the task doc's Track Layout shows all six tracks Not started. Either signals work and the task doc is a documentation failure, or they don't and Phase 19 must be demoted — which would cascade to Phases 21, 29, 30, 43.

**Acceptance:**
- [x] Read `kernel/src/signal/` and the signal-related syscalls in `kernel/src/arch/x86_64/syscall/mod.rs` and identify which of the six tracks (A: mask, B: sigframe, C: sigreturn, D: sigaltstack, E: rt_sigaction, F: validation) have landed code.
- [x] For each track with landed code, flip the corresponding checkboxes to `[x]` with a file+symbol citation. *(Phase 19 task doc is in legacy pipe-table form — table-format conversion deferred to post-1.0 per Track B.3 scope; reconciled by flipping Track Layout statuses and adding a verification section with citations.)*
- [x] For any track with no landed code, convert to `[ ] — Deferred: post-1.0` with an explanation. *(Only `rt_sigpending` (Track A subset) was missing; recorded under "Deferred Until Later" in `docs/roadmap/tasks/19-signal-handlers-tasks.md`.)*
- [x] Design-doc `Status:` field agrees with the resulting task-doc Track Layout. *(Both now Complete.)*
- [x] If Phase 19's Status is demoted from Complete: `Depends on:` lines in the design docs for Phases 21, 29, 30, and 43 are updated to drop the `✅` next to Phase 19, and any acceptance items in those four task docs that explicitly cite Phase 19 features are converted to `[ ] — Blocked: Phase 19 demoted`. If Phase 19 stays Complete, this item is closed with a one-line justification noting no cascade was needed. *(Phase 19 stayed Complete; no cascade needed.)*

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
- [x] Each task doc's items have been walked against the codebase; genuinely-done items are `[x]` with a file+symbol citation. *(Per-track citations live in each task doc's new "Phase 58 reconciliation — verification" section; per-checkbox citations are aggregated to the track level so we don't duplicate the same anchor 30+ times.)*
- [x] Genuinely-not-done items carry `[ ] — Deferred: <owner phase>` and an explanation. *(Track 43c I.1 — regressions intentionally not run on PR — recorded as a documented design deviation in the verification section. No genuine deferrals beyond that; everything else shipped, with deviations noted.)*
- [x] No track in any of the five docs remains universally unchecked without at least one deferral note. *(All checkboxes flipped to `[x]`.)*
- [x] Each design doc's `Status:` field agrees with the resulting task-doc Track Layout. *(43b/43c/46/47 design docs already say Complete; 42b has no design doc — created in this phase as Track C.2.)*

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
- [x] 55a, 55b, 56, 57a, 57d: `Status:` changed to `Complete`.
- [x] 57b: `Status:` changed to `Complete`; stale "pending soak (PR #132)" qualifier removed or replaced with a one-line note pointing to Phase 59 Track G for soak result.
- [x] Phase 55a's "Known Open Bug" section receives a cross-reference noting that 55c R2 claims closure of the VT-d MMIO `CTRL.RST` issue.
- [x] Roadmap README rows for 55a, 55b, 56, 57a, 57b, 57d updated to match.

### A.4 — Add missing header fields to Phase 35 design doc

**Files:**
- `docs/roadmap/35-true-smp-multitasking.md`

**Symbol:** `Status:`, `Source Ref:` in Phase 35
**Why it matters:** Phase 35's design doc violates the template (missing two required header fields). Note: the 2026-05-08 audit also flagged `AGENTS.md` as stale at v0.51.0, but the file has since been refreshed to v0.57.0; the only remaining `AGENTS.md` change for this phase is the v0.58.0 bump in H.2.

**Acceptance:**
- [x] `docs/roadmap/35-true-smp-multitasking.md` has `Status:` and `Source Ref: phase-35` as the first two header lines.

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
- `docs/roadmap/52d-*.md` (closure-note edit, file glob — locate the actual Phase 52d design doc)

**Symbol:** (new file, or explicit merge note in 51 design doc; closure-note paragraph in Phase 52d design doc)
**Why it matters:** Phase 51 is marked In Progress with no task doc. Phase 52 and later phases list 51 as a dependency but it was never formally closed. Either write the task doc and run acceptance gates, or add a merge paragraph to the 51 design doc explaining the scope folded into 52d and mark it Complete.

**Acceptance:**
- [ ] Either a task doc `51-service-model-maturity-tasks.md` exists with checkboxes walked, OR the 51 design doc has an explicit closure paragraph stating which acceptance items folded into which later phase.
- [ ] Phase 51 `Status:` field is either `Complete` (if items verified) or `Deferred: see Phase 52d` (if merged).
- [ ] Phase 52 design doc has a companion closure note consistent with the Phase 51 disposition.
- [ ] README row for Phase 51 updated to reflect final status.

### B.3 — Convert Phase 16 task doc from pipe-table to checkbox format

**File:** `docs/roadmap/tasks/16-network-tasks.md`
**Symbol:** P16-T001 through P16-T073 task rows
**Why it matters:** Phase 16's task doc uses pipe tables with no checkboxes (73 rows). The doc-template authority (`docs/appendix/doc-templates.md` § "Template: phase task doc") requires `- [x]`/`- [ ]` checkbox items. Phase 16 was the only table-format task doc flagged by the 2026-05-08 audit as a red flag (Phases 17, 18, 20, 22, 23, 25, 28, 29 also use the legacy format but are deferred to post-1.0 per Track H — see Deferred Until Later in the design doc). Converting Phase 16 here closes the Goal's "convert legacy table-format task docs to checkbox form" commitment.

**Acceptance:**
- [ ] All 73 P16-T### rows are restructured into `- [x]` (verified shipped) or `- [ ] — Deferred: <owner phase>` (not shipped) form.
- [ ] Verified items carry a file+symbol citation (e.g., `kernel/src/net/virtio_net.rs::VirtioNet::receive`).
- [ ] Track Layout table at the top of the file is preserved.
- [ ] Design-doc `Status:` for Phase 16 (currently Complete) agrees with the resulting checkbox state — every Track A–G row in the layout has at least one verified `[x]` item, or the design doc is demoted.

---

## Track C — Missing Design Docs

### C.1 — Write Phase 22b design doc

**File:** `docs/roadmap/22b-ansi-parser-enhancement.md`
**Symbol:** (new file)
**Why it matters:** Phase 22b has only a task doc. The template requires a companion design doc. Without it, the phase's rationale, feature scope, and implementation choices are not recorded.

**Acceptance:**
- [ ] File `docs/roadmap/22b-ansi-parser-enhancement.md` exists with all 11 template H2 sections populated, not stubbed (Milestone Goal, Why This Phase Exists, Learning Goals, Feature Scope, Important Components and How They Work, How This Builds on Earlier Phases, Implementation Outline, Acceptance Criteria, Companion Task List, How Real OS Implementations Differ, Deferred Until Later — see `docs/appendix/doc-templates.md` § "Template: phase design doc").
- [ ] Required header fields present: `Status`, `Source Ref: phase-22b`, `Depends on`, `Builds on`, `Primary Components`.
- [ ] README row for Phase 22b updated to link the new design doc.

### C.2 — Write Phase 42b design doc

**File:** `docs/roadmap/42b-async-executor.md`
**Symbol:** (new file)
**Why it matters:** Phase 42b has only a task doc. Downstream phases (43b, 43c, 46, 47) depend on 42b's async executor infrastructure; without a design doc the rationale and interface contract are unrecorded.

**Acceptance:**
- [ ] File `docs/roadmap/42b-async-executor.md` exists with all 11 template H2 sections populated, not stubbed (see C.1 acceptance for the section list and template reference).
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

**Disposition (decided up-front to avoid per-file judgement during implementation):**

| File | Disposition | Rationale |
|---|---|---|
| `rust-crate-acceleration.md` | Move to `docs/archived/` with a dated supersession note pointing at Phases 41–47 | Fully superseded; no unique content remains |
| `clang-llvm-roadmap.md` | Move to `docs/archived/` with dated note | Toolchain plan; no shipped phase covers it; defer reactivation to post-1.0 |
| `claude-code-roadmap.md` | Move to `docs/archived/` with dated note | Tooling-meta doc, not implementation-blocking |
| `git-roadmap.md` | Move to `docs/archived/` with dated note | Same as above |
| `github-cli-roadmap.md` | Move to `docs/archived/` with dated note | Same as above |
| `nodejs-roadmap.md` | Move to `docs/archived/` with dated note | Post-1.0 language port plan |
| `python-roadmap.md` | Move to `docs/archived/` with dated note | Post-1.0 language port plan |

The default disposition is "archive with dated note." If during implementation any of the six non-`rust-crate-acceleration.md` docs is discovered to contain unique not-elsewhere-recorded design intent, the implementer may instead fold the salvageable content into the appropriate phase appendix and then archive the husk — but the default action is archive.

**Acceptance:**
- [x] Every file in the table above has been moved to `docs/archived/<original-filename>` (preserving basename).
- [x] Each archived file's first body line is a quoted note in the form `> Archived 2026-05-08 — superseded by <phase or note>. Original content preserved below for historical reference.`
- [x] No file remaining in the top-level `docs/` directory references "Phase 32" as "today" or "current". *(Two `docs/3?-*.md` legacy aligned docs reference Phase 32 by name as a roadmap entry, which is the correct cross-reference form, not a "today" claim.)*
- [x] `git mv` was used (not delete + add) so that history is preserved.

---

## Track F — Subdirectory Consolidation

### F.1 — Merge `docs/handoff/` into `docs/handoffs/`

**Files:**
- `docs/handoff/` (singular — 1 file)
- `docs/handoffs/` (plural — 21 files)

**Symbol:** directory structure and all cross-reference links
**Why it matters:** Two parallel directories exist for the same content type. The singular has 1 file, the plural has 21. Every internal doc that links to `docs/handoff/` gets a broken reference after the merge.

**Acceptance:**
- [x] File from `docs/handoff/` moved into `docs/handoffs/`.
- [x] `docs/handoff/` directory removed.
- [x] Every cross-reference in the corpus to `docs/handoff/` updated to `docs/handoffs/`.
- [x] `grep -r 'docs/handoff[^s]'` returns no results in the repo root. *(Aside from this Phase 58 task doc and design doc themselves, which describe the migration action by name. Track G's final-validation grep excludes those self-referential narrative paths.)*

### F.2 — Archive `docs/evaluation/` and `docs/shell/brush-integration-analysis.md`

**Files:**
- `docs/evaluation/` directory
- `docs/shell/brush-integration-analysis.md`

**Symbol:** README or index entry in each
**Why it matters:** `docs/evaluation/README.md` is scoped to v0.47.0 (~10 phases stale). `docs/shell/brush-integration-analysis.md` is dated 2026-03-26 (~15 phases stale). Both mislead readers about the current state.

**Acceptance (archive-only — neither doc is on the active path so refresh is not justified):**
- [x] `docs/evaluation/README.md` has a prominent first-body-line header: `> Archived 2026-05-08 — scoped to v0.47.0; not maintained against later phases. Refer to current phase docs for up-to-date evaluation criteria.`
- [x] `docs/shell/brush-integration-analysis.md` has a prominent first-body-line header pointing to current shell + PTY docs (`docs/22-tty-terminal.md` and `docs/roadmap/29-pty-subsystem.md` — note: the task spec referenced `29-pty-and-jobs.md`, but the actual filename is `29-pty-subsystem.md`).
- [x] Neither file's body content is otherwise modified — the goal is to flag staleness, not rewrite v0.47-era analysis.

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
- [ ] `grep -rn 'docs/handoff[^s]' docs/ AGENTS.md` returns no matches (validates F.1 completed cleanly).
- [ ] `grep -rn 'Phase 32' docs/*.md` returns no matches against top-level docs (validates E.1 completed cleanly — does not include `docs/roadmap/` or `docs/archived/`).
- [ ] `cargo xtask check` (clippy + rustfmt) passes — no source files were changed by this phase, but verify no doc changes introduced broken links in any Rust doc-comment.

---

---

## Track H — Documentation and Release

### H.1 — Create the aligned legacy learning doc

**File:** `docs/58-documentation-reconciliation.md`
**Symbol:** new file
**Why it matters:** The doc-template "aligned legacy learning doc" form gives a learner-friendly companion to the design + task docs. Every shipped phase has one (or has a deliberate exception). This file is created from the template in `docs/appendix/doc-templates.md` § "Template: aligned legacy learning doc".

**Acceptance:**
- [ ] `docs/58-documentation-reconciliation.md` exists, follows the template (Aligned Roadmap Phase, Status, Source Ref, Supersedes Legacy Doc / new — all present)
- [ ] Overview paragraph is learner-friendly and explains the phase outcome in plain language
- [ ] "What This Doc Covers" lists 3+ concrete topics
- [ ] "Core Implementation" is written for a learner who has not read the design or task doc
- [ ] "Key Files" table cites the actual files this phase touches
- [ ] "How This Phase Differs From Later Documentation Work" (or analogous heading specific to this phase) is filled in
- [ ] "Related Roadmap Docs" links the design and task docs

### H.2 — Bump kernel version to 0.58.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md` (any version annotations)

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]` section
**Why it matters:** Phase closure is signalled by a kernel version bump per project convention. Each new phase moves the project from `0.<previous>.x` to `0.<NN>.0`. The `AGENTS.md` "Kernel v0.X.Y" reference must move with it (the 2026-05-08 audit Red Flag noted `AGENTS.md` stale at `v0.51.0`; it has since been refreshed to `v0.57.0`, so this phase moves it directly to `v0.58.0`).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.58.0"`
- [ ] `Cargo.lock` regenerated (`cargo generate-lockfile` or similar)
- [ ] `AGENTS.md` "Kernel v0.58.0" reference updated
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag suggestion: `v0.58.0` (tag at phase merge, not at task-checkbox tick)

---

## Documentation Notes

- This phase produces no Rust source changes. All changes are in `docs/` and `AGENTS.md`.
- When flipping checkboxes in Track A.2, prefer citing the specific file and function over a broad "this phase shipped". For example: `[x] Scheduler rewrite — see \`kernel/src/task/scheduler.rs::block_current_until\`` is better than `[x] Done`.
- Deferral notes in Track A.2 must name a specific owner phase or explicitly say "post-1.0" — "deferred" alone is insufficient.
- The Phase 51 disposition (B.2) sets a precedent for how to handle phases where the code shipped but the formal closure record was skipped.
- Track F changes to directory structure require updating cross-references in audit docs under `docs/appendix/audit-status/` as well as design and task docs.
