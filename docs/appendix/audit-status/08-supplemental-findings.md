# 08 — Supplemental Findings (Subdirectories Missed by the Original Audit)

**Validation pass:** 2026-05-08

The original audit (2026-05-07) scoped only to `docs/roadmap/` and `docs/appendix/`. Six subdirectories and the top-level legacy `docs/*.md` corpus (~126 files) were not read. This supplemental pass closes that gap. Underlying evidence is in `findings/08-postmortems-and-handoffs.md`, `findings/09-evaluation-research-shell.md`, and `findings/10-top-level-legacy-docs.md`.

The supplemental pass changes three things: (1) one audit residual is **already closed** that the audit listed as open; (2) several **new high-severity gaps** surface that the original audit missed; (3) the documentation drift the audit flagged extends further than reported (legacy docs are ~10–20 phases stale).

---

## A. Audit residuals **closed** by the missed subdirs (correct the original audit)

### A1. ✅ `55c-net-remote-rx-test-bug` is closed

**Source:** `docs/handoffs/57b-batch-summary.md` confirms the three RX-path tests were fixed during Phase 57b implementation. The audit's C10 blocker and `findings/05` / `findings/07` listed this as Open — both are now stale. **Action:** strike C10 from `06-pre-1.0-blocker-list.md` (already done in this commit).

### A2. ✅ Five tick-multiplier `× 10` / `÷ 10` bugs are closed

Confirmed independently by the post-merge validation pass. Phase 57a Track G.3 fixed all five. C8 already struck in `06-pre-1.0-blocker-list.md`.

### A3. ⚠️ Phase 57b soak gate document found, but **the soak was never run**

**Source:** `docs/handoffs/57b-soak-gate.md` exists; its result table is empty. The audit's Red Flag #16 ("57b soak result undocumented") is more accurately stated as: *the soak gate doc exists but the soak has not been run; the gate is open, not waiting on documentation*.

---

## B. NEW high-severity findings (escalations)

These are genuine 1.0 blockers the original audit missed.

### B1. 🛑 `audio_server` Ac97Backend submits frames for accounting only — **no PCM reaches hardware**

**Source:** `docs/research/post-phase-57 evaluation/01-phase-57-progress.md` (2026-04-29). The `audio-smoke` xtask gate checks that `audio_server.conf` loads — it does not assert PCM frame consumption.

**Why it matters:** Phase 57 was declared Complete on the strength of "single-client AC'97 audio + session_manager boot sequence". The truth is that the audio path does not actually emit sound — it logs and accounts. This is the 57-equivalent of `fat_server`'s ENOSYS stub: a service that registers, runs, and replies cleanly while doing none of the work it advertises.

**Recommended action:** Either (a) implement real PCM submission against the AC'97 NABM registers and update `audio-smoke` to assert frame consumption, or (b) demote Phase 57's status to "Complete (audio path is accounting-only; PCM emission deferred)" and assign the actual audio path to a new phase.

### B2. 🛑 `session_manager` `start/stop/restart` return `Ack` unconditionally — lifecycle stubs

**Source:** `docs/research/post-phase-57 evaluation/01-phase-57-progress.md`. Also confirmed in `findings/08-postmortems-and-handoffs.md` (`docs/handoffs/`-side: `session_manager` F.4 `stop()` calls are logging-only stubs).

**Why it matters:** Phase 57 acceptance includes a typed `text-fallback` recovery contract orchestrated by `session_manager`. If `stop()` and `restart()` are no-ops that return `Ack`, recovery cannot work — failures are silently absorbed. The `session_manager` is structurally extracted (the service exists and is supervised) but its supervisory behaviour is not implemented.

**Recommended action:** Implement real lifecycle handling (track child pids, signal them, observe SIGCHLD, requeue restart) or document the fallback as advisory-only.

### B3. 🛑 Bug #9 `preempt_count` leak — IrqSafeMutex guard outlives `block_current_until` in ~25 callsites; Option-B fix not landed

**Source:** `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` and `findings/08-postmortems-and-handoffs.md`. The original session-15 fix shipped only for `sys_mmap_file_backed`; the post-mortem documents Option-B (Arc-clone of the guard) as the correct general fix. Option-B is not landed.

**Why it matters:** With Phase 57e deferred, `PREEMPT_VOLUNTARY` is the production model. Under voluntary, the leak is mostly latent (kernel mode is non-preemptible during the leaked count). But: any future move toward `cond_resched`-style explicit yield points (the path the 57e post-mortem proposes for Future Work) re-exposes the leak. This is a structural correctness gap that must close before any further preemption work, even voluntary explicit-yield.

**Recommended action:** Land the Option-B fix across the ~25 callsites. Add a `preempt_count` invariant assertion at the user-mode return boundary that fires on first return per CPU after a leaked guard.

### B4. 🛑 Slab caches defined but none integrated — confirms Phase 33 headline gap independently

**Source:** `docs/research/post-phase-57 evaluation/01-phase-57-progress.md`. Independent confirmation that the slab infrastructure landed but no kernel object family was migrated. The original audit flagged this from the design doc's own audit note; the post-Phase-57 evaluation reaches the same conclusion from runtime observation.

**Why it matters:** Already in the original audit (Red Flag #4); now corroborated by a second source. No status change; severity unchanged.

### B5. 🔴 `/tmp` sticky-bit unenforced — local privilege escalation surface

**Source:** `docs/evaluation/security-review.md` (and corroborated in research evaluation set).

**Why it matters:** Without sticky-bit semantics, any user can delete or replace another user's `/tmp` file. The audit's security findings (CSPRNG weakness, OpenSSH pubkey format incompatibility, W^X absence) did not include this.

**Recommended action:** Implement sticky-bit semantics in `unlink`/`rename` checks against the `/tmp` parent directory's `S_ISVTX` bit, or document that m3OS is single-user and sticky-bit is irrelevant.

### B6. 🔴 Auth-file write non-atomicity — `passwd`/`adduser` torn-shadow risk on crash

**Source:** `docs/evaluation/security-review.md`. Existing flow rewrites `/etc/shadow` directly; a crash mid-write produces a torn file with possible partial entries.

**Why it matters:** Phase 48 (Security Foundation) is the trust-floor phase; this is a trust-floor gap not surfaced in that phase's audit.

**Recommended action:** Use a temp-file + atomic rename pattern. ~1 day.

### B7. 🔴 Dynamic linker / shared library loading absent — no owner phase

**Source:** `docs/research/post-phase-57 evaluation/03-real-applications-browser-roadmap.md`. Required for the path to toolkit GUI apps; mentioned only as a "deferred" item in Phase 11's design doc, with no successor phase.

**Why it matters:** Toolkit applications (GTK, Qt, etc.) require dynamic linking. Static linking only goes so far before binary sizes become prohibitive. The audit listed this in Phase 11's deferrals but did not flag the absence of an owner phase as a 1.0 blocker.

**Recommended action:** Decide whether dynamic linking is part of 1.0. If yes, assign a phase. If no, document the limitation and the path forward.

### B8. 🟠 `term` has no published terminfo entry; missing alternate screen buffer, 256-color, truecolor, SIGWINCH, mouse reporting

**Source:** `docs/research/post-phase-57 evaluation/04-tui-and-neovim-roadmap.md`. The graphical terminal emulator added in Phase 57 lacks the features TUI applications (nvim, tmux, htop) require.

**Why it matters:** Phase 57's milestone goal includes "term graphical terminal emulator". Without these features, the terminal is functional for shell sessions but breaks TUI apps. The audit did not surface this; the Phase 57 design doc describes the term composition (PTY + ANSI parser + Phase 56 surfaces + audio bell) but does not list these capabilities as deferrals.

**Recommended action:** Define a Phase 57g (term capabilities) covering terminfo publication, alternate screen, 256-color/truecolor, SIGWINCH propagation, and mouse reporting.

### B9. 🟠 Typed IPC IDLs / code-generated bindings deferred — no owner phase

**Source:** Multiple — confirmed in `docs/research/` and the audit's own Phase 50 deferral list. The audit listed this as a Phase 50 deferral; the supplemental pass confirms no successor phase exists.

**Why it matters:** Typed IDLs are the natural successor to the hand-written request/reply codecs that have grown through Phases 50, 54, 55b, 55c, 56, 57. They are deferred uniformly with no owner.

### B10. 🟠 No interrupt-safe allocation path

**Source:** `docs/evaluation/`-side note. The frame allocator and slab caches use `IrqSafeMutex`, but no path provides ISR-direct allocation. Code paths that allocate from interrupt context risk deadlock.

**Why it matters:** Currently masked because no in-tree code allocates from ISR context, but the constraint is undocumented and any future driver that needs ISR-direct allocation will deadlock.

### B11. 🟠 Phase 56 legacy doc says `Planned`; roadmap says Complete — extends Red Flag #7

**Source:** `findings/10-top-level-legacy-docs.md`. `docs/56-display-and-input-architecture.md` carries `Status: Planned` while `docs/roadmap/56-display-and-input-architecture.md` and the README treat the phase as Complete. Adds Phase 56 legacy doc to the design-doc-vs-README drift list.

### B12. 🟡 `docs/16-network.md` and `docs/22-tty-terminal.md` are content-stale

**Source:** `findings/10`. `16-network.md` claims kernel-mode network implementation is temporary (Phase 54 already migrated it). `22-tty-terminal.md` describes PTY as "skeleton stubs" (Phase 29 fully implemented it). Both docs carry the right Status but stale body content.

### B13. 🟡 `docs/06-ipc.md` references a non-existent file in its `Supersedes` field

**Source:** `findings/10`. Self-referencing or phantom — references `docs/06-ipc-core.md` which does not exist.

### B14. 🟡 Seven post-1.0 roadmap docs at the top level use a `Today (Phase 32)` baseline

**Source:** `findings/10`. Files: `clang-llvm-roadmap.md`, `claude-code-roadmap.md`, `git-roadmap.md`, `github-cli-roadmap.md`, `nodejs-roadmap.md`, `python-roadmap.md`, `rust-crate-acceleration.md`. None has the aligned-template header. All use "Today (Phase 32)" or "Phase 33 in progress" — 20+ phases stale. `rust-crate-acceleration.md` is fully superseded by completed phases 41–47. The official phases 59–62 in `docs/roadmap/README.md` cover the same scope; these top-level docs duplicate and contradict.

**Recommended action:** Either delete the seven top-level docs (their content is superseded) or convert them to references to the matching Phase 59–62 design docs.

### B15. 🟡 `docs/evaluation/README.md` scoped to v0.47.0 (~10 phases stale); `docs/shell/brush-integration-analysis.md` dated 2026-03-26 (~15 phases stale)

**Source:** `findings/09`. The evaluation directory has not been refreshed since the v0.47.0 era. Strategic positioning docs predating the convergence phases (52-54) describe a system that no longer exists.

---

## C. NEW items the supplemental pass flagged that fit existing audit categories

### C1. virtio-input migration plan — no implementation started
`docs/handoffs/2026-05-04-virtio-input-migration.md` lays out the plan; no implementation work referenced. Future-phase candidate.

### C2. RAW_INPUT_ROUTER as a second ISR-unsafe spinlock — fixed
Closed in `c519a60`. Confirms the `IrqSafeMutex` retrofit pattern that landed in PR #132 carried over to this site.

### C3. TIOCSCTTY fix: `slave_fg_pgid` not set — fixed
Audit had no record. `findings/08` records the fix.

### C4. `ipc_wait_service` syscall 0x1115 + new `BlockedOnService` task state
Phase 57a-adjacent change introducing a new scheduler state. Audit had no record. The state was added in the 57a follow-on tracks; verify it has design-doc coverage.

### C5. `TaskSyscallSnapshot` per-task GPR snapshot
Fixes mid-syscall preemption aliasing. Audit had no record. Validates the structural hardening surviving the 57e deferral.

### C6. `sys_nanosleep` second-wake bug — explicitly deferred, still open
Audit had no record.

### C7. `57a-validation-gate.md` I.2 and I.4 fields never filled
Pending user runs. Reflects the audit's recurring "manual QEMU validation deferred" pattern (Phases 30/31/32) extended into 57a.

---

## D. Documentation hygiene — corpus-wide

The supplemental pass surfaces a structural insight: **`docs/` is heterogeneous and not all subdirectories are maintained at the same cadence.** The split is:

| Subdirectory | Cadence | Trustworthiness |
|---|---|---|
| `docs/roadmap/` | Per-phase, gated on PR merge | Status fields lag (audit Red Flag #7) |
| `docs/roadmap/tasks/` | Per-phase, gated on PR merge | Checkbox flips lag (audit Pattern 1) |
| `docs/appendix/` | Ad hoc, kept | Generally accurate |
| `docs/post-mortems/` | Per-incident, kept | Authoritative for closure |
| `docs/handoffs/` (plural) | Per-handoff, kept | Authoritative for in-flight state; some result tables empty |
| `docs/handoff/` (singular) | Per-handoff, 1 file only | Likely deprecated (single file: 2026-04-28-graphical-stack-startup) |
| `docs/debug/` | Per-bug, 1 file only | Cited but not maintained |
| `docs/evaluation/` | Quarterly-ish | ~10 phases stale (v0.47.0 baseline) |
| `docs/research/` | Ad hoc | Recent (`post-phase-57 evaluation/` is 2026-04-29 — most accurate runtime snapshot) |
| `docs/research/post-phase-57 evaluation/` | One-shot | **Most accurate state snapshot in the entire corpus** |
| `docs/*.md` (top-level legacy) | Phase-aligned but rarely refreshed | Header-aligned but body content stales after the phase closes |
| `docs/shell/`, post-1.0 roadmap docs | Ad hoc | ~15-20 phases stale; superseded |

**Recommended cleanup pass (additional R-recommendation, R11):**
1. Treat `docs/research/post-phase-57 evaluation/` as the canonical post-Phase-57 state snapshot until a successor lands.
2. Migrate `docs/handoff/` (singular) into `docs/handoffs/` (plural) and remove the empty directory; or vice-versa.
3. Either retire the top-level legacy docs (their roadmap/ counterparts are authoritative) or refresh them to match the aligned-template baseline.
4. Either retire the seven top-level post-1.0 roadmap docs or fold them into Phase 59–62 design docs.
5. Either refresh `docs/evaluation/` and `docs/shell/` or mark them archived with a date.

---

## E. Net effect on the original audit

| Audit item | Original verdict | Supplemental verdict |
|---|---|---|
| Red Flag #1 (Phase 19 mismatch) | 🛑 | 🛑 unchanged |
| Red Flag #2 (5 phases unchecked task docs) | 🛑 | 🛑 unchanged |
| Red Flag #3 (Phase 35 maybe_load_balance) | 🛑 | 🛑 unchanged |
| Red Flag #4 (Phase 33 slab migration) | 🛑 | 🛑 confirmed by post-Phase-57 evaluation |
| Red Flag #5 (Phases 30/31/32 deferred validation) | 🛑 | 🛑 unchanged |
| Red Flag #6 (Phase 51 In Progress) | 🛑 | 🛑 unchanged |
| Red Flag #7 (5 phases status drift) | 🛑 | 🛑 + Phase 56 legacy doc adds, 57d added in 2026-05-08 pass |
| Red Flag #8 (Phase 13 no task doc) | 🔴 | 🔴 unchanged |
| Red Flag #9 (Phase 16 no checkboxes) | 🔴 | 🔴 unchanged |
| Red Flag #10 (Phase 22b no design doc) | 🔴 | 🔴 unchanged |
| Red Flag #11 (Phase 25 SMP gaps) | 🔴 | 🔴 unchanged |
| Red Flag #12 (Phase 43 SSH tests) | 🔴 | 🔴 unchanged |
| Red Flag #13 (Phase 21 milestone inversion) | 🟠 | 🟠 unchanged |
| Red Flag #14 (`fat_server` ENOSYS) | 🟠 | 🛑 **escalated** — `audio_server` matches the same pattern (B1) |
| Red Flag #15 (Phase 56 subscription push) | 🟠 | 🟠 unchanged |
| Red Flag #16 (57a pi_lock + 57b soak) | 🟠 | 🟠 + soak gate doc found but soak never run; Bug #12/#13 closed |
| Blocker C8 (tick-multiplier bugs) | 🔴 | ✅ closed by PR #136 |
| Blocker C10 (`55c-net-remote-rx-test-bug`) | 🟠 | ✅ closed by PR #132 |
| (NEW) Audio server PCM not emitted | — | 🛑 **new** |
| (NEW) Session manager lifecycle stubs | — | 🛑 **new** |
| (NEW) Bug #9 preempt_count leak Option-B not landed | — | 🛑 **new** |
| (NEW) /tmp sticky-bit unenforced | — | 🔴 **new** |
| (NEW) passwd/adduser torn-shadow risk | — | 🔴 **new** |
| (NEW) Dynamic linker absent — no owner phase | — | 🔴 **new** |
| (NEW) term TUI capabilities missing | — | 🟠 **new** |
| (NEW) Top-level post-1.0 roadmap docs ~20 phases stale | — | 🟡 **new** |

**Net:** the audit has 3 newly-closed items, 6 newly-surfaced high-severity items (3 🛑, 3 🔴), and a corpus-wide doc-hygiene observation that warrants a recommendation (R11). The most material new finding is **B1 (audio_server doesn't emit PCM)** — it matches `fat_server`'s ENOSYS-stub pattern but in a phase whose milestone goal explicitly promised audible output.
