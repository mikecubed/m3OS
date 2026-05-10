# Phase 62 — Phase 57a Pi-Lock Closeout: Task List

**Status:** Planned
**Source Ref:** phase-62
**Depends on:** Phase 57a (Scheduler Block/Wake Protocol Rewrite) ✅, Phase 57b (Preemption Foundation) ✅ (post-merge soak tracked as Phase 59 Track G), Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅, Phase 59 (Validation Backlog) — **Track G must complete before Phase 62 Track F.2** (populates `docs/handoffs/57b-soak-gate.md`)
**Goal:** Eliminate the four `TODO(57a-C/D)` sites in `kernel/src/task/scheduler.rs` by routing them through `pi_lock` + `with_block_state`; apply the Option-B (Arc-clone) or Option-C (release-before-block) guard fix to every kernel callsite that holds an `IrqSafeMutex` guard across `block_current_until`; add a regression test that proves the existing Phase 57b D.3 / Phase 57e Bug #9 user-return preempt-count assertion fires when a guard-leak is reintroduced; run the 30-minute soak under the Phase 59 Track G procedure; flip Phase 57a Tracks C/D and update Phase 57b design-doc Status with the Phase 59 soak result.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Inventory four `TODO` sites + every kernel guard-across-block callsite | — | Planned |
| B | Route four `TODO` sites through `pi_lock` + `with_block_state` | A | Planned |
| C | Option-B (Arc-clone) or Option-C (release-before-block) guard fix for every inventory site | A | Planned |
| D | Guard-leak regression test exercising the existing user-return preempt-count assertion | B C | Planned |
| E | Regression suite + 30-minute soak (Phase 59 Track G procedure) | B C D | Planned |
| F | Phase 57a/57b doc updates; Phase 57e post-mortem cross-reference | E | Planned |
| G | Documentation and Release | E F | Planned |

---

## Track A — Inventory

### A.1 — Inventory the four `TODO(57a-C/D)` sites

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** lines 892, 4108, 4120, 4319 (post-Phase 61; re-verify against HEAD with `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` immediately before starting Track B — line numbers drift on every kernel change).
**Why it matters:** The TODO comments are the authoritative list of Phase 57a Tracks C/D incomplete work. Each site has a different surrounding context (queue scan, test scaffolding, dispatch hot path); the fix shape differs per site, so an accurate map is required before writing the fix.

**Acceptance:**
- [ ] `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` rerun against current HEAD; the four lines identified and context documented in `docs/handoffs/62a-pi-lock-inventory.md`.
- [ ] Each of the four sites annotated with its current shape:
  - **Site 1 (~line 892):** scheduler queue-scan defensive cleanup — drops a `Ready` task with `saved_rsp == 0` by setting `state = TaskState::Dead`. Holds `scheduler_lock()` during the scan; `pi_lock` ordering rule (pi_lock outer, SCHEDULER inner) means the helper here must be the locked-helper variant or an inversion-aware path.
  - **Site 2 (~line 4108):** `install_test_task_idx` (`#[cfg(test)]`) — sets a freshly-allocated filler `Task` to `Dead` before pushing into `sched.tasks`. The task is not yet in any queue; no other CPU can observe it.
  - **Site 3 (~line 4120):** `install_test_task_idx` (`#[cfg(test)]`) — sets a freshly-allocated `Task` to `Ready` before in-place overwrite of an existing slot. Same constraint as Site 2 (task not yet observable from another CPU).
  - **Site 4 (~line 4319):** `dispatch` hot path — sets the picked task `state = TaskState::Running` while `scheduler_lock()` is held with IRQs disabled. Hot path: every context switch passes through here.
- [ ] For each site: required `with_block_state` (or scheduler-lock-aware equivalent) call shape identified, including whether the site is reachable with `pi_lock` already held by another CPU.
- [ ] Lock-ordering compliance for each site recorded: pi_lock is the **outer** lock, `SCHEDULER.lock()` is **inner** (per Phase 57a Track A doc-block). Sites 1 and 4 hold `scheduler_lock()`; the fix at those sites cannot acquire `pi_lock` (would invert the order) — instead they must use an alternate primitive (e.g., direct `Task::pi_lock` access via `with_block_state_locked` if such a helper exists, or a deferred state transition that releases SCHEDULER first).

### A.2 — Inventory every kernel guard-across-block callsite

**Files (kernel-wide grep — do not restrict scope):**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/task/scheduler.rs`
- `kernel/src/task/wait_queue.rs`
- `kernel/src/task/mod.rs`
- `kernel/src/ipc/endpoint.rs`
- `kernel/src/ipc/notification.rs`
- `kernel/src/ipc/registry.rs`
- `kernel/src/blk/virtio_blk.rs`
- `kernel/src/fs/fat32.rs`
- `kernel/src/fs/ext2.rs`
- `kernel/src/fs/tmpfs.rs`
- `kernel/src/serial.rs`
- `kernel/src/lib.rs`
- (any additional file surfaced by the kernel-wide grep below)

**Symbol:** pattern `<guard> = <LOCK>.lock(); ... block_current_until(...)` where guard is live across the call
**Why it matters:** Bug #9 source (`docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`) documents the preempt_count leak; Option-B (Arc-clone) and Option-C (release-before-block / copy-out) are the documented fixes. The post-mortem's "~25 callsites" estimate covered only the syscall layer surveyed at that time — fs/blk/ipc-internal callsites must be reviewed in this phase. The real count is determined by the inventory, not assumed.

**Acceptance:**
- [ ] `grep -rn 'block_current_until' kernel/src/` output reviewed (kernel-wide, not the three-file subset). Currently surfaces ~70 hits across 13 files.
- [ ] For each `block_current_until` call: guard liveness at call site checked (is any `IrqSafeMutex`, `Spinlock`, or other `preempt_disable`-incrementing lock guard in scope?).
- [ ] Inventory table in `docs/handoffs/62a-pi-lock-inventory.md` lists every call with: file, line, lock type, guard liveness verdict (live / released-before-block / no-guard), recommended fix (Option-B Arc-clone, Option-C release-before-block, or "verified safe — no IrqSafeMutex").
- [ ] Total count of guard-across-block sites recorded. The post-mortem's ~25 estimate is a lower bound; if the kernel-wide grep surfaces more, the count is updated and noted in the inventory.
- [ ] Sites where the lock is a plain `Mutex` or `Spinlock` (no `preempt_disable`) are explicitly tagged "verified safe — not preempt-affecting" in the inventory rather than converted.

---

## Track B — Pi-Lock TODO Sites

### B.1 — Route `scheduler.rs:~892` (queue-scan defensive cleanup) through pi_lock-aware helper

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** site at line ~892 — inside the per-core ready-queue scan, drops a task with `saved_rsp == 0` by setting `state = TaskState::Dead`. The surrounding context already holds `scheduler_lock()`.
**Why it matters:** A bare `task.state = Dead` here is observable to any concurrent wake path that has already taken `pi_lock`. The transition to `Dead` is terminal; once published, no subsequent wake should resurrect the task. Without `pi_lock`, a wake racing on another CPU can clobber `Dead` back to `Ready`.

**Acceptance:**
- [ ] State store at line ~892 routed through a helper that takes `pi_lock` for the transition. Because the surrounding code holds `scheduler_lock()` (inner lock) and pi_lock is the outer lock, a naive `task.with_block_state(...)` would invert the order. The fix must either (a) release `scheduler_lock()` and re-acquire after the transition, or (b) use an explicit `Task::pi_lock_locked_under_scheduler` helper that documents the inversion is safe at this specific site (queue-scan, IRQ-disabled, no other CPU holds this task's pi_lock by structural invariant).
- [ ] The chosen approach is documented inline at the site with a `// NOTE: Phase 62 Track B.1 — ...` comment explaining the lock-order argument.
- [ ] `// TODO(57a-C/D)` comment removed.
- [ ] `cargo xtask test` passes.

### B.2 — Route `scheduler.rs:~4108` and `~4120` (`install_test_task_idx` test scaffolding) through `with_block_state`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** sites at lines ~4108 and ~4120 inside `#[cfg(test)] install_test_task_idx`. Both stores happen on freshly-allocated `Task` values **before** they are inserted into `sched.tasks` (line 4108) or in-place-overwrite an existing slot under `scheduler_lock()` (line 4120).
**Why it matters:** Both sites are test-only and currently safe (the task is not yet visible to another CPU). The fix is uniformity, not race-fix: the abstraction should be applied universally so future readers cannot mistake these sites as "the protocol allows direct mutation here."

**Acceptance:**
- [ ] Site at ~4108 (filler-task initialization before push) replaced with `with_block_state` (or direct pi_lock acquire on the freshly-constructed `Task` — which is uncontended since nothing else holds a reference yet).
- [ ] Site at ~4120 (in-place overwrite of `*sched.tasks[idx]`) — note this happens under `scheduler_lock()` (inner). Same lock-ordering caveat as B.1: either release SCHEDULER to acquire pi_lock, or use the locked-helper from B.1 if the structural argument holds.
- [ ] Both `// TODO(57a-C/D)` comments removed.
- [ ] `cargo xtask test` passes after both changes.

### B.3 — Route `scheduler.rs:~4319` (dispatch hot path) through pi_lock-aware helper

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** site at line ~4319 inside the per-core dispatch loop — sets the picked task `state = TaskState::Running` while `scheduler_lock()` is held with IRQs disabled. This is the **every-context-switch** transition.
**Why it matters:** This is the hottest of the four sites. A bare store here is observable to any wake path on another CPU: a wake that takes `pi_lock` and reads `state` to decide whether to enqueue may observe a stale (pre-Running) value. Under PREEMPT_VOLUNTARY this race is narrow but real; under any future PREEMPT_FULL revisit it becomes critical. The transition must hold `pi_lock` for atomicity with the wake-side check.

**Acceptance:**
- [ ] State store at line ~4319 routed through a pi_lock-aware helper. As with B.1 / B.2 the surrounding `scheduler_lock()` is inner; the lock-order resolution chosen for B.1 must be reused here (release-and-reacquire, or the locked-helper inversion).
- [ ] The `debug_assert!(task.state == TaskState::Running, ...)` at line ~4322 still passes after the change (read happens under the same protection as the write).
- [ ] `// TODO(57a-C/D)` comment removed.
- [ ] `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` returns no output.
- [ ] `cargo xtask test` passes.
- [ ] Dispatch-path microbench (or existing `cargo xtask run` smoke) shows no measurable regression vs. pre-change baseline (this is the hot path; even a few extra cycles per context switch matter).

---

## Track C — Guard-Across-Block Fix (Option-B / Option-C)

### C.1 — Apply Option-B or Option-C to every inventory callsite

**Files:** every file surfaced by Track A.2's kernel-wide grep. The candidate set at the start of Phase 62 is:
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/task/scheduler.rs`
- `kernel/src/task/wait_queue.rs`
- `kernel/src/task/mod.rs`
- `kernel/src/ipc/endpoint.rs`
- `kernel/src/ipc/notification.rs`
- `kernel/src/ipc/registry.rs`
- `kernel/src/blk/virtio_blk.rs`
- `kernel/src/fs/fat32.rs`
- `kernel/src/fs/ext2.rs`
- `kernel/src/fs/tmpfs.rs`
- `kernel/src/serial.rs`
- `kernel/src/lib.rs`

**Symbol:** each `block_current_until` callsite with a live guard
**Why it matters:** Holding an `IrqSafeMutex` (or any `preempt_disable`-incrementing) guard across a blocking call corrupts `preempt_count` for the wakeup path. This is the documented Bug #9 general fix. The two acceptable shapes are:
- **Option-B (Arc-clone):** Arc-wrap the protected data, clone the Arc before the block, drop the guard, block, re-acquire after wake. Used when the protected data is a heap-resident object referenced after the block.
- **Option-C (release-before-block / copy-out):** Release the guard before the block; if a scalar value is needed after wake, copy it out before release. Used when the protected data is small or only read once. The session-15 `sys_mmap_file_backed` fix is **Option-C**: it splits the lifecycle so `lock_page_tables()` is dropped before `kernel_read_fd_at` (which reaches `block_current_until`) and re-acquired after. It is **not** an Arc-clone.

**Acceptance:**
- [ ] Every callsite in the A.2 inventory has been converted to Option-B or Option-C per the inventory's per-site recommendation; sites tagged "verified safe — not preempt-affecting" are left unchanged but documented.
- [ ] The `sys_mmap_file_backed` Option-C fix (session-15) is verified intact — no regression introduced by adjacent changes.
- [ ] No new guard-across-`block_current_until` pattern is introduced in this phase. Track C diffs reviewed file-by-file to confirm.
- [ ] Each converted site carries a `// NOTE:` comment naming the option used and the Bug #9 reference, e.g. `// NOTE: Phase 62 Track C — Option-B Arc-clone (Bug #9). Guard dropped before block_current_until to release IrqSafeMutex.`
- [ ] `cargo xtask check` (clippy `-D warnings` + rustfmt + kernel-core host tests) passes after all callsite changes.
- [ ] `cargo xtask test` passes after all callsite changes.

---

## Track D — Guard-Leak Regression Test

### D.1 — Verify the existing user-return preempt-count assertion catches a guard leak

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs:96` — existing `assert_preempt_count_zero_on_return_to_user` wrapper (Phase 57b D.3 + Phase 57e Bug #9 clamp/assert helper).
- `kernel/src/task/scheduler.rs:2292` — existing `assert_preempt_count_zero_at_user_return()` helper (debug asserts, release clamps).
- `kernel/tests/` — new test binary or existing test extension that deliberately leaks a guard across `block_current_until` to exercise the assertion path.

**Symbol:** existing `assert_preempt_count_zero_at_user_return` (no new assertion code introduced — the assertion was added by Phase 57b D.3 and extended by Phase 57e Bug #9; this track adds a regression test that proves it fires on a deliberate leak).
**Why it matters:** The existing assertion fires on every IRQ-/syscall-return to ring 3 (debug build, gated on `code_segment.rpl() == Ring3`). Phase 62 does not need a new assertion — it needs a positive test that proves the existing one would catch a Track-C regression. Without such a test, a future careless edit could reintroduce a guard-across-block leak and silently pass `cargo xtask test` (because no current test deliberately leaks a guard).

**Acceptance:**
- [ ] A new (or amended) `cargo xtask test` test deliberately constructs a guard-across-`block_current_until` shape behind `#[cfg(test)]` to verify that returning to ring 3 with `preempt_count > 0` triggers the existing `assert_preempt_count_zero_at_user_return` panic in debug builds. The test asserts that the panic was observed (e.g., via a panic-hook capture or by running the misuse path in a child task whose death is expected).
- [ ] The deliberate leak is gated to `#[cfg(test)]` and removed from any production code path before merge.
- [ ] The complementary positive case is also exercised: a regular syscall returning to ring 3 after Track C's fixes does **not** trip the assertion, confirming Track C left no residual leaks.
- [ ] Documentation note added at the top of the new test referencing Phase 57b D.3, Phase 57e Bug #9, and Phase 62 Track D, so a future reader understands why the test exists.
- [ ] No change to the assertion itself (no relaxation, no first-return latch, no scope reduction). Phase 62 strictly leans on the existing every-return discipline.

---

## Track E — Regression and Soak

### E.1 — Full regression pass

**Files:** `xtask/src/main.rs`, all kernel test binaries
**Symbol:** `cargo xtask test`
**Why it matters:** Tracks B and C touch the scheduler's state-transition hot path and every significant blocking syscall. Any missed site or incorrect Option-B conversion produces a `preempt_count` imbalance that eventually surfaces as a hang or spurious reschedule.

**Acceptance:**
- [ ] `cargo xtask test` passes with zero regressions.
- [ ] `cargo xtask check` passes.
- [ ] No new unsafe block without `// SAFETY:` comment.

### E.2 — 30-minute soak (Phase 59 Track G procedure)

**File:** `docs/handoffs/62e-pi-lock-soak.md` (new log artifact); cross-references `docs/handoffs/57b-soak-gate.md` for procedure.
**Symbol:** QEMU 4-core GUI instance, default kernel build (PREEMPT_VOLUNTARY is the production model post-57e-deferral); same workload shape as the Phase 57b soak gate.
**Why it matters:** Under voluntary preemption, `preempt_count` imbalances from leaked guards produce deferred reschedules that may not surface in short test runs. The Phase 57b soak gate established the canonical procedure (4 cores, GUI, IPC + futex + notification synthetic load, 30 minutes). Phase 62 reuses that exact procedure so the result is directly comparable to the Phase 59 Track G baseline; any new panic or `[WARN] [sched]` line vs. that baseline is a Track-B/C regression.

**Acceptance:**
- [ ] `cargo xtask run-gui --fresh` running for 30 minutes wall-clock with synthetic IPC + futex + notification load on ≥ 4 QEMU cores (per `docs/handoffs/57b-soak-gate.md` procedure).
- [ ] Serial log evaluated against the four pass criteria from the soak-gate doc: zero `preempt_count != 0 at user-mode return` panics; no new `[WARN] [sched]` lines vs. the Phase 59 Track G baseline; no deadlocks; clean shutdown.
- [ ] `docs/handoffs/62e-pi-lock-soak.md` populated: QEMU command (including any non-default flags), start/end timestamps, panic count (= 0 expected), pass/fail verdict, log-artifact reference.
- [ ] If the soak fails: kernel panic captured, root-caused to a specific Track B / Track C site, fix applied and soak re-run to a pass before Track F starts.

---

## Track F — Phase 57a and 57b Doc Updates

### F.1 — Flip Phase 57a task-doc Tracks C and D

**File:** `docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md`
**Symbol:** Track C and Track D checkbox items
**Why it matters:** Phase 57a Tracks C (pi_lock routing) and D (with_block_state uniformity) are the source items this phase closes. The Phase 57a design-doc Status is already `Complete` at HEAD; this task only flips the per-track checkboxes that document the actual code closure.

**Acceptance:**
- [ ] Track C checkboxes: all four `TODO(57a-C/D)` site items flipped to `[x]` with `scheduler.rs` line citations.
- [ ] Track D checkboxes: Option-B / Option-C guard fix items flipped to `[x]` with file + line citations across the kernel-wide inventory.

### F.2 — Update Phase 57b design doc with soak result

**File:** `docs/roadmap/57b-preemption-foundation.md`
**Symbol:** `Status:` field — currently reads `Complete (post-merge soak tracked as a Phase 59 Track G item)`.
**Why it matters:** The Status qualifier records that the soak was tracked elsewhere. Once Phase 59 Track G has populated `docs/handoffs/57b-soak-gate.md` and Phase 62 Track E.2 has confirmed no regression vs. that baseline, the qualifier can be replaced with a concrete pass-result reference.

**Acceptance:**
- [ ] `Status:` line stays `Complete`; the parenthetical qualifier `(post-merge soak tracked as a Phase 59 Track G item)` is replaced with `(30-minute soak completed YYYY-MM-DD per docs/handoffs/57b-soak-gate.md — pass)`. The date and verdict come from the soak-gate table row populated by Phase 59 Track G; if Phase 62 re-ran the soak, cite the later row.
- [ ] Phase 57e post-mortem (`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`) cross-referenced in Phase 57b's design doc body (e.g., in "How This Builds on Earlier Phases" or a new "Related Reading" stub) for readers who want the full preemption-model history.

### F.3 — Add Phase 62 closure note to Phase 57a design doc

**File:** `docs/roadmap/57a-scheduler-rewrite.md`
**Symbol:** Acceptance Criteria section
**Why it matters:** Phase 57a's Status is already `Complete` at HEAD, but its Acceptance Criteria section does not yet credit Phase 62 with closing Tracks C/D. A reader auditing the phase needs to be pointed at the closure.

**Acceptance:**
- [ ] `Status:` confirmed `Complete` at HEAD (no change required if already so).
- [ ] A one-line note added to the Phase 57a Acceptance Criteria section: "Tracks C (pi_lock routing) and D (Option-B/C guard fix) closed by Phase 62 — see `docs/roadmap/62-phase-57a-pi-lock-closeout.md`."

---

## Track G — Documentation and Release

### G.1 — Create the aligned legacy learning doc

**File:** `docs/62-phase-57a-pi-lock-closeout.md`
**Symbol:** new file
**Why it matters:** The doc-template "aligned legacy learning doc" form gives a learner-friendly companion to the design + task docs. Every shipped phase has one (or has a deliberate exception). This file is created from the template in `docs/appendix/doc-templates.md` § "Template: aligned legacy learning doc".

**Acceptance:**
- [ ] `docs/62-phase-57a-pi-lock-closeout.md` exists, follows the template (Aligned Roadmap Phase, Status, Source Ref, Supersedes Legacy Doc / new — all present)
- [ ] Overview paragraph is learner-friendly and explains the phase outcome in plain language
- [ ] "What This Doc Covers" lists 3+ concrete topics
- [ ] "Core Implementation" is written for a learner who has not read the design or task doc
- [ ] "Key Files" table cites the actual files this phase touches
- [ ] "How This Phase Differs From Later Scheduler Work" (or analogous heading specific to this phase) is filled in
- [ ] "Related Roadmap Docs" links the design and task docs

### G.2 — Bump kernel version to 0.62.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md` (any version annotations)

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]` section
**Why it matters:** Phase closure is signalled by a kernel version bump per project convention. Each new phase moves the project from `0.<previous>.x` to `0.<NN>.0`. The `AGENTS.md` "Kernel v0.X.Y" reference must move with it (per audit Red Flag — `AGENTS.md` was found stale at `v0.51.0` during the 2026-05-08 audit).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.62.0"`
- [ ] `Cargo.lock` regenerated (`cargo generate-lockfile` or similar)
- [ ] `AGENTS.md` "Kernel v0.62.0" reference updated
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag suggestion: `v0.62.0` (tag at phase merge, not at task-checkbox tick)

---

## Documentation Notes

- The `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` file is the primary design reference for Option-B and Option-C. Re-read it before starting Track C to understand which callsite shapes map to which option.
- When applying Option-B (Arc-clone), document the pattern adjacent to the clone: `// NOTE: Phase 62 Track C — Option-B Arc-clone (Bug #9). Guard dropped before block_current_until to release IrqSafeMutex.` When applying Option-C (release-before-block), document the pattern at the explicit `drop(guard)`: `// NOTE: Phase 62 Track C — Option-C release-before-block (Bug #9). Guard dropped before block_current_until; data needed after wake was copied out above.`
- Track A.2 may reveal sites where the guard-across-block is safe because the lock is not preempt-affecting (e.g., a plain `Mutex` or `Spinlock` that does not call `preempt_disable`). Tag those "verified safe — not preempt-affecting" in the inventory rather than converting them.
- The Phase 59 Track G soak result must be complete before Track F.2 can be finished. Phase 62 depends on Phase 59 for this reason. As of this doc's writing, Phase 59 status in `docs/roadmap/README.md` is `Planned`; check the README and `docs/handoffs/57b-soak-gate.md` for the current state before starting Phase 62 work.
- Track D leans on the existing user-return preempt-count assertion (added by Phase 57b D.3 and extended by Phase 57e Bug #9). It is a `debug_assert!` inside `assert_preempt_count_zero_at_user_return` (release builds clamp instead of panic). If `cargo xtask test` runs in debug mode, the assertion is always exercised on every IRQ-/syscall-return to ring 3.
- Sites at lines ~892 and ~4319 (Track B.1 / B.3) hold `scheduler_lock()` (the inner lock) when they touch `task.state`. The pi_lock-outer / SCHEDULER-inner ordering rule established in Phase 57a Track A means a naive `task.with_block_state(...)` call here would invert the order. The implementer must choose between (a) release-and-reacquire SCHEDULER, or (b) a `Task::pi_lock_locked_under_scheduler` helper whose contract documents why the inversion is safe at queue-scan / dispatch sites (IRQs disabled, no other CPU can hold this task's pi_lock while it is being dispatched). The choice must be documented in the inventory (Track A) before Track B starts coding.
