# 07 — Recommendations

This document closes the audit with concrete recommendations: status corrections, doc-template fixes, and proposed remediation phases. Each recommendation is named so it can be referenced in PRs or retrospectives. The numbering matches the categories in `06-pre-1.0-blocker-list.md` where applicable.

---

## R1 — Adopt a "doc-truth" rule for status fields

**Problem:** Today, the design-doc Status field, the task-doc Status field, the README row Status field, the AGENTS.md narrative, and the in-code comments often disagree. There is no canonical source of truth.

**Recommendation:** Pick one canonical source and treat the others as derived. The README's per-phase row is the natural choice because it's the most visible and the most discoverable. Establish:

> **The README per-phase row's `Status` column is the canonical project status.** Design-doc and task-doc Status headers must match. AGENTS.md narrative claims must be reconcilable with the README. When they diverge, the divergence is itself a defect tracked as an audit item.

**Effort:** 1 hour to publish the rule + walking the existing divergences (a separate effort, see R2).

---

## R2 — One-shot reconciliation pass on the status fields

**Problem:** As of 2026-05-07, four phases (55a, 55b, 56, 57a) have design-doc status "Planned" while the README and AGENTS.md treat them as Complete. Phase 19 has design-doc Complete vs. task-doc Not started. Phase 51 is In Progress with no task doc. Phase 52 is In Progress with sub-phase work treated as the closure path. Five phases (42b, 43b, 43c, 46, 47) have task docs marked Complete with universally-unchecked checkboxes.

**Recommendation:** Schedule a single reconciliation PR ("audit-status closeout") that touches only Status fields and acceptance-checkbox states. No new functionality, no code changes. The PR's job is to make the roadmap honest about what exists today. The acceptance gate is: every phase's design-doc Status, task-doc Status, and README row agree, and every Complete phase has either checked acceptance items or explicit deferral notes for unchecked ones.

**Effort:** 2–3 days for one engineer.

**Source:** Red flags #1, #2, #6, #7, #8, #9, #10. Items A1–A12 in `06-pre-1.0-blocker-list.md`.

---

## R3 — Establish a "validation track must be checked" gate

**Problem:** Phases 22b, 30, 31, 32 each ship a substantial user-visible feature with the entire validation track left unchecked behind "manual QEMU test" annotations. The phases were closed despite the runtime behaviour never having been verified.

**Recommendation:** Add a project rule:

> A phase cannot be marked Complete if any acceptance criterion in its design doc is mapped to an unchecked task in its task doc. "Manual QEMU test" is acceptable as a verification method, but the resulting checkbox must be checked before the phase closes — meaning someone actually ran the test.

**Effort:** This is a process change, not a code change. To apply it retrospectively, run B1–B5 from the blocker list.

---

## R4 — Cut new remediation phases for the open-but-unowned work

**Problem:** Several substantial functional gaps have no owner phase:
- Slab migration (Phase 33's headline deliverable that never happened)
- `maybe_load_balance` activation (Phase 35 commented-out hook)
- True per-core scheduling (re-deferred by 52d with no successor)
- IPC capability grants / bulk transfers / timeouts (deferred to "Phase 7+", never delivered)
- W^X enforcement
- AMD-Vi fault ISR
- VT-d scalable mode / queued invalidation
- Display server subscription-push wire transmission
- pi_lock wiring at 4 scheduler sites
- 5 tick-multiplier bugs
- `fat_server` ENOSYS stub
- CLOEXEC plumbing gap (Track A of 54a, still Planned)

**Recommendation:** Cut explicit remediation phases for the largest items, following the precedent set by 52a/b/c/d and 54a:

- **Phase 56b — Display Server Closeout.** D-E4 subscription-push, compositor damage tracking, DOOM `sys_fb_acquire` migration, D-A0 modifier-key wire format. ~1 week.
- **Phase 55d — IOMMU Substrate Completion.** AMD-Vi fault ISR (Track E), VT-d scalable mode, VT-d queued invalidation, AMD-Vi multi-BDF domains, the "Known Open Bug" cleanup in 55a's design doc. ~2–3 weeks.
- **Phase 57f — Preemption Hygiene.** pi_lock wiring (Phase 57a Tracks C/D), 5 tick-multiplier fixes, 57b soak result documentation, Track G test activation, deferred `preempt_disable` wrappers from 57c. ~1 week.
- **Phase 58 (Release 1.0 Gate, already planned) — explicit acceptance bar.**
  - Honest status: every Status field reconciled (R2 done).
  - Validation: every previously-deferred manual test run (B-section blockers complete).
  - Decisions made and documented for: slab migration (Phase 33 successor or deferral), `maybe_load_balance` (Phase 35 successor or deferral), `fat_server` (implement or remove), W^X (deferral with timeline), IPC capabilities (deferral with timeline), CSPRNG hardening (deferral with timeline), OpenSSH pubkey format (deferral with timeline).

The above adds three new phases (56b, 55d, 57f) and clarifies the scope of one already-planned phase (58). Together they close the substantive gaps the audit found.

---

## R5 — Stop treating `Phase N+ deferred` as a permanent disposition

**Problem:** Several in-code comments use the form `// Deferred to Phase 6+` or `// Phase 7+ deferred` (notably `kernel/src/mm/user_space.rs` and `kernel/src/ipc/mod.rs`). These deferrals were written when the project was at Phase 5–6 and meant "some later phase". As of Phase 57e, "later phase" has happened many times over and the deferral is just a permanent shortcut hiding behind a phase marker.

**Recommendation:**
- Replace `Phase N+` markers with explicit phase numbers (e.g., `// Deferred to Phase 58 — not part of 1.0`) or with a documented limitation (e.g., `// Limitation: see docs/known-limitations.md#wx`).
- Where a `Phase N+` marker has no current owner, escalate it to a project decision: either commit to a successor phase or accept it as a documented limitation.

**Effort:** 1 day for an audit pass on `// Deferred` comments; per-item decision time varies.

---

## R6 — Treat the `findings/` files in this directory as durable evidence

**Problem:** This audit took ~30 minutes of focused agent work to produce. The findings are detailed and citable. They will rot if they become orphaned.

**Recommendation:**
- Keep `docs/appendix/audit-status/findings/01-*.md` through `findings/07-*.md` in-tree alongside this synthesis. They are the per-phase evidence base.
- When a phase's status is reconciled (per R2), add a one-line note in the corresponding finding file: `Reconciled YYYY-MM-DD by PR #N — phase X now Complete with all checkboxes flipped`.
- When a 1.0 release ships, take a final snapshot of this audit (with current state) and freeze it as `docs/appendix/audit-2026-pre-1.0/`.
- Future audits should follow the same pattern: a research pass, a synthesis pass, durable findings.

---

## R7 — Doc-template enforcement

**Problem:** Three template violations were found:
- Phase 22b has no design doc (only a task doc).
- Phase 42b has no design doc.
- Phase 35's design doc is missing `Status:` and `Source Ref:` header fields.

`docs/appendix/doc-templates.md` defines the templates but the project does not enforce them.

**Recommendation:** Add a CI check (or a `cargo xtask check` step) that validates every `docs/roadmap/NN-*.md` and `docs/roadmap/tasks/NN-*-tasks.md` file against the relevant template — minimally: presence of `Status:`, `Source Ref:`, `Depends on:`, `Builds on:` in design docs; presence of `Status:`, `Source Ref:`, `Depends on:`, `Goal:` in task docs.

**Effort:** 1 day to write the linter.

---

## R8 — Treat AGENTS.md as a tested artifact

**Problem:** AGENTS.md is the most-read project description (read by every Claude Code session, every contributor onboarding). Today it includes:
- Stale version string `v0.51.0` (current ~0.57.0+)
- Narrative claims about ring-3 drivers, IOMMU isolation, the display server, and preemption that disagree with the design-doc Status fields

**Recommendation:**
- Add a `version` field at the top of AGENTS.md and validate it against the kernel version in CI.
- Add a "current capabilities" section in AGENTS.md that explicitly references the README rows it claims to summarise. Anything in the AGENTS.md narrative that isn't backed by a `README.md` row marked Complete should either be reconciled (R2) or hedged.

**Effort:** 1–2 hours.

---

## R9 — Move accepted technical debt out of "Deferred Until Later" into a known-limitations doc

**Problem:** The "Deferred Until Later" section in each phase doc mixes three different kinds of items:
1. Real future-phase work that has an owner phase.
2. Documented shortcuts that the project has accepted as permanent or quasi-permanent (W^X, IPC bulk transfer, OpenSSH pubkey format incompatibility, fat_server stub).
3. Aspirational nice-to-haves the project has no real plans to deliver.

These read identically in the docs but mean very different things.

**Recommendation:** Create `docs/known-limitations.md` capturing items in category 2. Each entry has: the limitation, the reason, the workaround if any, and the conditions under which the project might revisit it. Then prune the "Deferred Until Later" sections of phase docs to category 1 + category 3 only.

**Effort:** 1 day for the initial extraction pass; ongoing.

---

## R10 — Auditor's verdict

**The project is in significantly better shape than this audit's red-flag count would suggest.** Most of the flags are documentation drift, not functional defects. The architectural decisions (microkernel boundary, IOMMU substrate, ring-3 drivers, preemption programme) are sound. The 52a/b/c/d closure pattern is exemplary — the project has demonstrated it can recognise drift and write explicit closure phases. 52d is the model.

**The two deepest concerns are:**

1. **The status-field drift is at a tipping point.** Today a careful reader can reconstruct the actual state by triangulating across docs and code. With one or two more cycles of drift, that becomes hard. R1 + R2 fix this in ~2 days of focused work.

2. **Phase 51 was bypassed and Phase 35's hook was commented out.** Both are cases where a phase was declared Complete with the headline behaviour deferred or never wired in. The pattern recurs (Phase 33's slab migration, Phase 47's universally-unchecked tasks). The pattern is fixable with R3 (the validation-must-be-checked gate) but it requires explicit project policy.

**The project should not declare 1.0 until R2 is complete and at minimum the B-section validation runs from `06-pre-1.0-blocker-list.md` have been run.** The C-section gaps are individual decisions the project should make on the record before 1.0 — for each one, either implement, accept-as-known-limitation (R9), or remove.

The remediation cost is bounded: ~6–10 weeks of focused work, most of it documentation reconciliation rather than new feature development. None of the audit findings imply the project should pivot or rewrite.

---

## Summary table — actions and ownership

| ID | Action | Severity | Effort | Owner |
|---|---|---|---|---|
| R1 | Adopt doc-truth rule | — | 1 hour | Project lead |
| R2 | One-shot status reconciliation PR | 🛑 | 2–3 days | One engineer |
| R3 | Validation-must-be-checked gate | — | Process | Project lead |
| R4 | Cut Phases 55d, 56b, 57f; clarify Phase 58 | 🔴 | 4–5 weeks | Phase owners |
| R5 | Resolve `Phase N+ deferred` markers | 🟠 | 1 day | One engineer |
| R6 | Keep `findings/` in-tree | — | 0 hours | This PR |
| R7 | Add doc-template linter | 🟡 | 1 day | One engineer |
| R8 | Test AGENTS.md version + capability claims | 🟡 | 1–2 hours | One engineer |
| R9 | Extract known-limitations doc | 🟠 | 1 day + ongoing | One engineer |
| R10 | Auditor's verdict — defer 1.0 until R2 + B-section done | 🛑 | (see above) | Project lead |
