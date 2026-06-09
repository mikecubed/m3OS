# m3OS Phase Completion Audit (01 → 57e)

**Audit date:** 2026-05-07
**Validation pass:** 2026-05-08 — rebased onto `main` after PR #136 (Phase 57e — Deferred) squash-merged. Updates inline below and in `01`, `03`, `04`, `06`, `findings/05`, `findings/06`. The original audit findings about 57e (active branch work, Bug #12/#13, pi_lock TODOs) have been reconciled with the actual disposition: 57e is formally deferred, the SMP discipline hardening produced during the cycle is retained, the timer-driven kernel-mode preempt path is removed, and the five tick-multiplier bugs the audit flagged are fixed. See "What changed since the audit ran" below.
**Supplemental pass:** 2026-05-08 — original audit scoped only to `docs/roadmap/` and `docs/appendix/`; six subdirectories (`debug`, `evaluation`, `handoff`, `handoffs`, `post-mortems`, `research`, `shell`) and the top-level legacy `docs/*.md` corpus (~126 files) were missed. Three subagents closed the gap; results are in `findings/08`, `findings/09`, `findings/10` and synthesised into `08-supplemental-findings.md`. The supplemental pass closes 1 audit residual that was already closed (the `55c-net-remote-rx-test-bug` was fixed in PR #132), surfaces 3 new 🛑 blockers (audio_server doesn't emit PCM, session_manager lifecycle is stubs, Bug #9 preempt_count leak Option-B not landed), and 3 new 🔴 items (`/tmp` sticky-bit, passwd torn-shadow, dynamic linker absent).
**Roadmap follow-on:** 2026-05-08 — the audit's R4 recommendation (cut new remediation phases) was acted on. **Phases 58–76 were drafted** as a structured response: doc reconciliation (58), validation backlog (59), pre-1.0 closeouts (60–68), capability expansion (69–73), and architectural items (74–76). Existing Phases 58–62 (Release 1.0 Gate, Cross-Compiled Toolchains, Networking and GitHub, Node.js, Claude Code) renumbered to 77–81. See the updated `docs/roadmap/README.md` and the new design + task docs.

**Pre-1.0 audit (post-Phase-74):** 2026-05-26 — after Phases 74 (IPC capability grants) and 75 (W^X enforcement) merged, a second source-verified pass was run to identify everything still standing between current main and a real-hardware 1.0. The output is [`74a-pre-1.0-audit.md`](./74a-pre-1.0-audit.md). It is the input to Phase 83 (Release 1.0 Gate) and drove the insertion of Phase 77 (correctness + cheap-security bundle), Phase 78 (USB host foundation), Phase 79 (modern NIC), Phase 80 (Intel HDA), Phase 81 (Wi-Fi reference), Phase 82 (AHCI/SATA, optional), Phase 84 (KPTI/retpoline, post-1.0), and Phase 91 (IPv6/DHCPv6, post-1.0). The May 2026-05-08 Release-1.0 Gate (then numbered Phase 77) shifted to Phase 83; the post-1.0 platform-growth phases shifted from 78–81 to 85–88.
**Branch:** `feat/audit-status`
**Scope:** Roadmap phases 01 through 57e (the latest in-flight phase). Task and design docs, supporting appendices, follow-up trackers, and a code-side stub/TODO scan. No build or test runs were executed.
**Method:** Seven parallel research subagents (Sonnet) read the docs in their assigned ranges and recorded raw findings into `findings/01-*.md` through `findings/07-*.md`. The numbered files in this directory are the synthesised report; the `findings/` files are the underlying evidence and remain in-tree for traceability.

## What changed since the audit ran (2026-05-07 → 2026-05-08)

PR #136 squash-merged into `main` as `ad7d9b2 — Phase 57e — Deferred (full kernel preemption) + SMP discipline hardening`. The audit's 57e findings were validated against the post-merge state:

**Reconciled / closed by the merge:**
- **Phase 57e is formally Deferred** with `docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md` documenting the disposition. README row reads `**Deferred (2026-05-07)**`. The audit's "Bug #12 / Bug #13 active work" framing is superseded — both bugs were resolved as part of the deferral cleanup; their fixes survive as preempt-model-independent SMP discipline hardening (`preempt_count`, `IrqSafeMutex` F.1, wake-bracket race-shape closures, `sys_waitpid` 1 s deadline backstop).
- **All five tick-multiplier `× 10` / `÷ 10` bugs are fixed.** Comments now read `— G.3 fix` at `kernel/src/task/scheduler.rs:3878,4343` and `kernel/src/arch/x86_64/syscall/mod.rs:15763,16024,16436`. Item C8 in `06-pre-1.0-blocker-list.md` is closed.
- **`preempt-full` Cargo flag retired**; `check_and_preempt_kernel` and the kernel-mode preempt-frame plumbing are removed. The audit's "57e XSAVE migration / kernel preempt resume" content is now historical.

**Drift unchanged or worsened:**
- **57a, 57d**: README rows now say `**Complete**` but design-doc Status fields still read `Planned`. The drift this audit flagged (Red Flag #7) widened from {55a, 55b, 56, 57a} to also include 57d.
- **57b**: still says `Complete pending soak (PR #132)` — but PR #132 squash-merged as `f39ca13`. The "pending soak" qualifier is stale.
- **`TODO(57a-C/D): route through pi_lock + with_block_state` markers** still present at 4 sites (line numbers shifted: now `kernel/src/task/scheduler.rs:829, 3649, 3656, 3855`). The pi_lock wiring Phase 57a Tracks C/D were supposed to deliver still has not landed at these callsites.
- All other red flags (Phase 19 status mismatch, Phases 42b/43b/43c/46/47 universally-unchecked, Phase 33 slab migration, Phase 35 `maybe_load_balance`, Phases 30/31/32 deferred validation, Phase 51 In Progress, `fat_server` ENOSYS stub, display-server subscription push, NVMe isolation `todo!()` scaffolds) — unchanged by the merge.

---

## Executive Summary

m3OS is no longer a toy. The kernel is now a real preemptive-multitasking microkernel with ring-3 drivers, a userspace display server, audio, IOMMU, and a working SSH path. The roadmap has captured this growth honestly in many places — the `52d` and `54a` phases exist precisely because earlier phases were over-claimed and the project chose to close those gaps explicitly. That pattern is healthy.

But the audit surfaces a recurring class of problem: **phase status fields lag the evidence under them.** The most common failure mode is a phase declared "Complete" while either (a) its task-doc checkboxes were never flipped, (b) its validation track was deferred wholesale to "manual QEMU test", or (c) the design doc admits the headline deliverable was not actually built. Where this happens, the gap is usually documented in-line — but the README's `Status` column does not reflect it, so a reader scanning the roadmap is told the work is done.

The audit groups the gaps into four buckets, in order of severity:

1. **Critical status mismatches** — places where the README/header says "Complete" but the evidence inside the same doc says otherwise. Phase 19 (Signal Handlers — design doc Complete vs. task doc *Not started across all 6 tracks*) is the worst case. Phases 42b, 43b, 43c, 46, 47 each carry `Status: Complete` with **zero checked task checkboxes** across every track they own. Phase 35's `maybe_load_balance()` — the entire raison d'être of "true SMP" over Phase 25 — has its dispatch hook commented out. Phase 33's slab migration (the headline deliverable of "kernel memory improvements") never happened. Phases 30, 31, 32 ship telnetd, TCC, and `make` with their entire validation tracks left unchecked behind "manual QEMU test" annotations.
2. **Roadmap status drift in the live phases (55–57)** — the design docs for Phases 55a, 55b, 56, 57a, and 57d still say "Planned" while every downstream phase lists them as `✅` completed dependencies and the `AGENTS.md` overview describes their features as operational. (Validation pass 2026-05-08: 57d added to this list after PR #134 merged with no design-doc Status update; 57a and 57d now disagree with their own README rows, which read `Complete`.) Phase 57b is "Complete pending soak (PR #132)" — the qualifier is stale because PR #132 has merged.
3. **Documented shortcuts that have functional implications** — W^X is absent (every userspace code page is writable). The IPC engine has no capability grants, no bulk-page transfers, no timeouts (all "deferred to Phase 7+"). The CSPRNG used by SSH is explicitly not cryptographically secure. SSH pubkey format is incompatible with `authorized_keys`. `O_CLOEXEC` is silently dropped on `open`/`openat`. AMD-Vi has no fault ISR. `fat_server` is a permanent ENOSYS stub. The compositor has no damage tracking. (Five `× 10` / `÷ 10` tick-multiplier bugs were latent at audit time; closed by PR #136.)
4. **Open bug-doc residuals** — `copy_to_user` SMP TLB coherency was never audited closed. The `async-rt` mutex has a latent deadlock if any future code yields while holding the lock. The vendored `sunset-local` `wake_write()` waker-field bug may not have been patched. SSH H9 late-wedge is explicitly deferred to "future work in `sunset-local/`". (The `kernel::net::remote` 3-test bug the original audit listed here as open was already closed by PR #132 — see supplemental pass.)
5. **Stub services declared Complete** (supplemental pass 2026-05-08) — `audio_server` Ac97Backend submits frames for accounting only and **no PCM reaches hardware**, but Phase 57 is marked Complete. `session_manager` `start/stop/restart` return `Ack` unconditionally — the lifecycle orchestration that Phase 57's text-fallback recovery contract depends on does not actually run. Both match `fat_server`'s ENOSYS-stub pattern (Red Flag #14) but in phases declared Complete after that pattern was already a known concern.
6. **Documentation hygiene gap across `docs/`** (supplemental pass 2026-05-08) — `docs/evaluation/` is ~10 phases stale (v0.47.0 baseline). Seven top-level post-1.0 roadmap docs (clang-llvm/claude-code/git/github-cli/nodejs/python/rust-crate-acceleration) all use a `Today (Phase 32)` baseline — 20+ phases stale, fully superseded. `docs/research/post-phase-57 evaluation/` (2026-04-29) is the most accurate runtime state snapshot in the corpus and **directly contradicts** the "Complete" labels on Phase 57.

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
| `08-supplemental-findings.md` | **Supplemental pass 2026-05-08** — covers the missed subdirs (`docs/debug/`, `docs/evaluation/`, `docs/handoff/` (singular, since merged into `docs/handoffs/`), `docs/handoffs/`, `docs/post-mortems/`, `docs/research/`, `docs/shell/`, top-level legacy `docs/*.md`). 3 new 🛑 blockers + 3 new 🔴 items + 1 audit residual closed |
| `74a-pre-1.0-audit.md` | **Pre-1.0 audit pass 2026-05-26** — post-Phase-74 / post-Phase-75 source-verified inventory of every blocker between current main and a real-hardware 1.0. Feeds Phase 83 (Release 1.0 Gate); drove the insertion of Phases 77–82 + 84 + 89. |
| `findings/01-*.md` … `findings/07-*.md` | Underlying raw research from the original audit |
| `findings/08-postmortems-and-handoffs.md` | Supplemental — post-mortems, handoffs, debug |
| `findings/09-evaluation-research-shell.md` | Supplemental — evaluation, research, shell |
| `findings/10-top-level-legacy-docs.md` | Supplemental — top-level `docs/*.md` corpus |

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
