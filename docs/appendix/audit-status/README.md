# m3OS Phase Completion Audit (01 → 57e)

**Audit date:** 2026-05-07
**Branch:** `feat/audit-status`
**Scope:** Roadmap phases 01 through 57e (the latest in-flight phase). Task and design docs, supporting appendices, follow-up trackers, and a code-side stub/TODO scan. No build or test runs were executed.
**Method:** Seven parallel research subagents (Sonnet) read the docs in their assigned ranges and recorded raw findings into `findings/01-*.md` through `findings/07-*.md`. The numbered files in this directory are the synthesised report; the `findings/` files are the underlying evidence and remain in-tree for traceability.

---

## Executive Summary

m3OS is no longer a toy. The kernel is now a real preemptive-multitasking microkernel with ring-3 drivers, a userspace display server, audio, IOMMU, and a working SSH path. The roadmap has captured this growth honestly in many places — the `52d` and `54a` phases exist precisely because earlier phases were over-claimed and the project chose to close those gaps explicitly. That pattern is healthy.

But the audit surfaces a recurring class of problem: **phase status fields lag the evidence under them.** The most common failure mode is a phase declared "Complete" while either (a) its task-doc checkboxes were never flipped, (b) its validation track was deferred wholesale to "manual QEMU test", or (c) the design doc admits the headline deliverable was not actually built. Where this happens, the gap is usually documented in-line — but the README's `Status` column does not reflect it, so a reader scanning the roadmap is told the work is done.

The audit groups the gaps into four buckets, in order of severity:

1. **Critical status mismatches** — places where the README/header says "Complete" but the evidence inside the same doc says otherwise. Phase 19 (Signal Handlers — design doc Complete vs. task doc *Not started across all 6 tracks*) is the worst case. Phases 42b, 43b, 43c, 46, 47 each carry `Status: Complete` with **zero checked task checkboxes** across every track they own. Phase 35's `maybe_load_balance()` — the entire raison d'être of "true SMP" over Phase 25 — has its dispatch hook commented out. Phase 33's slab migration (the headline deliverable of "kernel memory improvements") never happened. Phases 30, 31, 32 ship telnetd, TCC, and `make` with their entire validation tracks left unchecked behind "manual QEMU test" annotations.
2. **Roadmap status drift in the live phases (55–57)** — the design docs for Phases 55a, 55b, 56, and 57a all still say "Planned" while every downstream phase lists them as `✅` completed dependencies and the `AGENTS.md` overview describes their features as operational. Phase 57b is "Complete pending soak" with no soak artifact recorded.
3. **Documented shortcuts that have functional implications** — W^X is absent (every userspace code page is writable). The IPC engine has no capability grants, no bulk-page transfers, no timeouts (all "deferred to Phase 7+"). The CSPRNG used by SSH is explicitly not cryptographically secure. SSH pubkey format is incompatible with `authorized_keys`. `O_CLOEXEC` is silently dropped on `open`/`openat`. AMD-Vi has no fault ISR. `fat_server` is a permanent ENOSYS stub. The compositor has no damage tracking. Five `× 10` / `÷ 10` tick-multiplier bugs sit in scheduler dispatch and `sys_poll`/`select`/`epoll_wait` from a 100 Hz → 1000 Hz tick rate change.
4. **Open bug-doc residuals** — `copy_to_user` SMP TLB coherency was never audited closed. The `async-rt` mutex has a latent deadlock if any future code yields while holding the lock. The vendored `sunset-local` `wake_write()` waker-field bug may not have been patched. SSH H9 late-wedge is explicitly deferred to "future work in `sunset-local/`". The `kernel::net::remote` test suite has three failing tests masked by an unrelated CI failure that will surface as soon as the masking failure is fixed.

The audit is **not** an indictment of the project's direction — the architectural decisions (microkernel boundary, ring-3 drivers, IOMMU substrate, preemption programme) are sound and the remediation phases (`52d`, `54a`) demonstrate a culture of revisiting and closing gaps. What this audit recommends is making the status drift explicit so the path to a credible 1.0 is not blocked by uncertainty about what is and isn't actually done.

---

## How to read this audit

| File | What it contains |
|---|---|
| `README.md` (this file) | Executive summary + navigation |
| `01-completion-truth-matrix.md` | Phase-by-phase honest status: declared vs. actual, with severity tags |
| `02-deferred-and-shortcuts.md` | Catalogue of every deferred item and documented shortcut, organised by phase |
| `03-red-flags-and-status-mismatches.md` | Places where the status claim contradicts evidence in the same doc |
| `04-code-side-evidence.md` | TODO/FIXME/stub markers, ignored tests, unsafe-block density, phase-tagged deferrals in code |
| `05-cross-cutting-bugs-and-followups.md` | Open bug-doc residuals and follow-up trackers |
| `06-pre-1.0-blocker-list.md` | Prioritised list of actionable items between current state and a credible 1.0 |
| `07-recommendations.md` | Proposed status corrections, doc-template fixes, and remediation phase candidates |
| `findings/01-*.md` … `findings/07-*.md` | Underlying raw research — kept in-tree for traceability |

The synthesised docs cite the `findings/*.md` files for the verbatim quotes and per-phase detail; if you want to verify a specific claim, follow the citation.

---

## Top-line numbers

- **62 design docs** scanned (phases 01–62 + 22b, 42b, 43a/b/c, 52a–d, 53a, 54a, 55a/b/c, 57a/b/c/d/e)
- **63 task docs** scanned (one per phase except where missing — see below)
- **Phases marked `Complete` in README:** 51 of 62
- **Phases marked `Complete` with universally-unchecked task docs:** 5 (42b, 43b, 43c, 46, 47)
- **Phases marked `Complete` with entire validation tracks deferred:** 4 (22b, 30, 31, 32)
- **Phases declared `Planned` in their design doc but treated as `✅` by downstream phases:** 4 (55a, 55b, 56, 57a)
- **Phase status mismatches between design doc and task doc:** 1 (Phase 19 — design `Complete`, task `Not started`)
- **Missing task docs:** 2 (Phase 13 — README explicitly notes "not yet created"; Phase 51 — no task doc exists)
- **Missing design docs:** 2 (Phase 22b — task doc only; Phase 42b — task doc only)
- **Phase task docs using a no-checkbox table format (no `[x]`/`[ ]`):** 9 (16, 17, 18, 20, 22, 23, 25, 28, 29)
- **Code-side TODO markers:** 22 substantive (kernel: 8, userspace: 9, kernel-core: 5)
- **`#[ignore]`d tests:** 26 named bodies (14 for preemption/XSAVE, 4 for Phase 55c isolation, 3 for Phase 56 G-track, 3 for QEMU-only driver-restart, 2 for xsave context-switch)
- **`unsafe { }` blocks in kernel:** 526 across 44 files (~59% have adjacent `// SAFETY:` comments)
- **`todo!()` macros in production paths:** 4 (all in Phase 55c isolation tests, all `#[ignore]`d — no `todo!()` macros outside `#[ignore]`d test bodies)

---

## Reading priority

If you are short on time, read in this order:

1. **`03-red-flags-and-status-mismatches.md`** — the items where the audit disagrees with the README
2. **`06-pre-1.0-blocker-list.md`** — the actionable list
3. **`07-recommendations.md`** — what to do about it

The rest of the documents are reference material.
