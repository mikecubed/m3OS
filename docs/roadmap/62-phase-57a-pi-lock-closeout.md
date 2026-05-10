# Phase 62 — Phase 57a Pi-Lock Closeout

**Status:** Planned
**Source Ref:** phase-62
**Depends on:** Phase 57a (Scheduler Block/Wake Protocol Rewrite) ✅, Phase 57b (Preemption Foundation) ✅ (post-merge soak tracked as Phase 59 Track G), Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅, Phase 59 (Validation Backlog) — Track G must complete before Phase 62 Track F.2 (populates `docs/handoffs/57b-soak-gate.md`).
**Builds on:** Completes the Phase 57a Tracks C and D delivery — routing the four `task.state` stores that bypass `pi_lock` through the correct abstraction, and landing the Bug #9 Option-B / Option-C guard fix for every kernel callsite that holds an `IrqSafeMutex` guard across `block_current_until`. Adds a regression test that proves the existing Phase 57b D.3 / Phase 57e Bug #9 user-return preempt-count assertion fires on a deliberate guard leak.
**Primary Components:** `kernel/src/task/scheduler.rs` (four `TODO(57a-C/D)` sites at lines ~892, ~4108, ~4120, ~4319), kernel-wide guard-across-block callsites (initial candidate set: `kernel/src/arch/x86_64/syscall/mod.rs`, `kernel/src/task/{scheduler,wait_queue,mod}.rs`, `kernel/src/ipc/{endpoint,notification,registry}.rs`, `kernel/src/blk/virtio_blk.rs`, `kernel/src/fs/{fat32,ext2,tmpfs}.rs`, `kernel/src/serial.rs`, `kernel/src/lib.rs`), `kernel/src/arch/x86_64/interrupts.rs:96` (existing `assert_preempt_count_zero_on_return_to_user` wrapper), `kernel/src/task/scheduler.rs:2292` (existing `assert_preempt_count_zero_at_user_return` helper), `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` (Bug #9 source), `docs/handoffs/57b-soak-gate.md` (soak result, populated by Phase 59 Track G), `docs/roadmap/57a-scheduler-rewrite.md`, `docs/roadmap/57b-preemption-foundation.md`

## Milestone Goal

The four `// TODO(57a-C/D): route through pi_lock + with_block_state` markers in `kernel/src/task/scheduler.rs` (lines ~892, ~4108, ~4120, ~4319 — re-verify against HEAD before starting) are eliminated: each bare `task.state = ...` store is replaced by a call to the `with_block_state` helper (or a pi_lock-aware locked-helper for sites that already hold `scheduler_lock()`) introduced in Phase 57a Tracks C/D. Every kernel callsite that holds an `IrqSafeMutex` guard across `block_current_until` is converted to Option-B (Arc-clone) or Option-C (release-before-block / copy-out) per the Track A.2 inventory. A new `cargo xtask test` regression test deliberately leaks a guard to prove the existing Phase 57b D.3 / Phase 57e Bug #9 `assert_preempt_count_zero_at_user_return` assertion catches the regression class. Phase 57a's task-doc Tracks C and D are fully checked. Phase 57b's design-doc Status is updated to `Complete` with the soak result from Phase 59 Track G.

## Why This Phase Exists

Phase 57a introduced a `pi_lock` per-task spinlock and a `with_block_state` helper to make block/wake transitions atomic. The four `TODO` markers record the sites where the Phase 57a rewrite reached the time-budget boundary and was not completed. Under PREEMPT_VOLUNTARY (the production model post-57e-deferral), the bare `task.state` stores at these sites are less likely to interact with a preemption window than under PREEMPT_FULL — but they are still genuine integrity gaps. Any future phase that re-examines preemption relies on the abstraction being uniformly applied.

Bug #9 (from `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`) documents the `preempt_count` leak pattern: an `IrqSafeMutex` guard held across `block_current_until` keeps `preempt_count > 0` at the moment the task deschedules. When the task reschedules, `preempt_count` is already elevated; `IrqSafeMutex::drop` then decrements it to zero, triggering a deferred reschedule at an unexpected point. The session-15 fix for `sys_mmap_file_backed` landed an **Option-C** (release-before-block) shape — `lock_page_tables()` is dropped before `kernel_read_fd_at` and re-acquired after. The post-mortem documents additional callsites with the same shape across the syscall, IPC, FS, and block-device layers; Track A.2 enumerates them via a kernel-wide grep.

The `assert_preempt_count_zero_at_user_return` debug-assertion (added by Phase 57b Track D.3 and extended by Phase 57e Bug #9 in `kernel/src/arch/x86_64/interrupts.rs:96` / `kernel/src/task/scheduler.rs:2292`) already fires on every IRQ-/syscall-return to ring 3. Phase 62 does not need to add a new assertion — it relies on the existing one and contributes a deliberate-leak regression test that proves the assertion catches the bug class. Without such a test, a future careless edit could reintroduce a guard-across-block leak and silently pass `cargo xtask test` because no current test exercises the leak path.

## Learning Goals

- Why routing `task.state` mutations through a per-task spinlock (`pi_lock`) is necessary for the block/wake abstraction to be race-free.
- How `IrqSafeMutex` interacts with `preempt_count`: acquiring the lock disables preemption; holding it across a blocking call means the task deschedules with preemption disabled, corrupting the counter for the wakeup path.
- When to choose Option-B (Arc-clone guard, drop before blocking) vs. Option-C (release-before-block / copy-out): Option-B preserves access to a heap-resident object after wake; Option-C is appropriate when only a scalar or no protected data is needed after the block.
- Why the `pi_lock` outer / `SCHEDULER` inner lock-ordering rule constrains the shape of the fix at the dispatch and queue-scan TODO sites — those sites already hold `scheduler_lock()`, so a naive `with_block_state` would invert the order.
- How the existing `assert_preempt_count_zero_at_user_return` assertion (Phase 57b D.3 + Phase 57e Bug #9) catches guard leaks on every IRQ-/syscall-return to ring 3 in debug builds, and why a deliberate-leak regression test is the right addition rather than a new assertion.

## Feature Scope

### Track A — Inventory the Four `TODO(57a-C/D)` Sites and Every Guard-Across-Block Site

Walk `kernel/src/task/scheduler.rs` for `TODO(57a-C/D)` comments. Run a kernel-wide `grep -rn 'block_current_until' kernel/src/` and review every hit for the pattern `guard = LOCK.lock(); ... block_current_until(...)` where the guard is live across the block. Note: the Bug #9 post-mortem's "~25 sites" estimate was made against the syscall layer only — Track A.2 broadens the survey to fs/, blk/, ipc-internal, and task-internal callsites as well. Produce an inventory doc (`docs/handoffs/62a-pi-lock-inventory.md`) listing every site with: file, line, lock type, guard liveness, and recommended fix (Option-B / Option-C / verified-safe).

### Track B — Route the Four `TODO` Sites Through `pi_lock` + `with_block_state`

For each of the four sites (post-Phase 61: lines ~892, ~4108, ~4120, ~4319 — re-verify against HEAD), replace the bare `task.state = ...` store with a pi_lock-aware helper. The four sites have distinct shapes:

- **Site 1 (~line 892, queue-scan defensive cleanup):** drops a `Ready` task with `saved_rsp == 0` to `Dead` while holding `scheduler_lock()`. Lock-ordering issue: pi_lock is outer, SCHEDULER is inner — a naive `with_block_state` here inverts. Use either release-and-reacquire SCHEDULER, or a `pi_lock_locked_under_scheduler` helper whose contract documents the structural-safety argument.
- **Sites 2 and 3 (~lines 4108, 4120, `install_test_task_idx`, `#[cfg(test)]`):** test-only, task not yet visible to other CPUs. Apply `with_block_state` (or direct uncontended pi_lock) for uniformity, not race-fix.
- **Site 4 (~line 4319, dispatch hot path):** sets the picked task to `Running` under `scheduler_lock()` with IRQs disabled — every context switch passes here. Same lock-ordering caveat as Site 1; reuse the resolution chosen there.

### Track C — Option-B / Option-C Guard Fix for Every Inventory Callsite

For each callsite in the Track A.2 inventory where a preempt-affecting guard is held across `block_current_until`:

- **Option-B (Arc-clone):** Arc-wrap the protected data if not already, clone the Arc before the block, drop the guard, block, re-acquire after wake. Used when the protected data is heap-resident and accessed after wake.
- **Option-C (release-before-block / copy-out):** Drop the guard before the block; if a scalar value is needed after wake, copy it out before release. Used when the protected data is small or not accessed after wake. The session-15 `sys_mmap_file_backed` fix is Option-C.

The Track A.2 inventory records the per-site recommendation; Track C applies the fix and tags each conversion with a `// NOTE: Phase 62 Track C — Option-{B,C} ...` comment.

### Track D — Guard-Leak Regression Test Exercising the Existing User-Return Assertion

The existing `assert_preempt_count_zero_at_user_return` (Phase 57b D.3 + Phase 57e Bug #9) already fires on every IRQ-/syscall-return to ring 3 in debug builds. Phase 62 does **not** add a new assertion — it adds a `#[cfg(test)]` regression test that deliberately constructs a guard-across-block leak to confirm the existing assertion fires. The complementary positive case — a regular syscall returning to ring 3 after Track C's fixes does not trip the assertion — is also exercised. The deliberate leak is gated to test code and never reaches production paths.

### Track E — Regression Suite + 30-Minute Soak (Phase 59 Track G Procedure)

Run `cargo xtask test` (full suite) and a 30-minute QEMU soak following the Phase 59 Track G procedure (`docs/handoffs/57b-soak-gate.md`): `cargo xtask run-gui --fresh` for 30 minutes wall-clock with synthetic IPC + futex + notification load on ≥ 4 cores. Pass criteria are the four enumerated in the soak-gate doc. Result is appended to `docs/handoffs/57b-soak-gate.md`'s table and recorded in a new Phase 62 artifact `docs/handoffs/62e-pi-lock-soak.md`. Comparing against the Phase 59 Track G baseline isolates Track B/C regressions.

### Track F — Phase 57a and 57b Doc Updates

Flip Phase 57a task-doc Tracks C and D checkboxes. Confirm Phase 57a's design doc `Status:` reads `Complete` (it currently does at HEAD) and add a one-line note in its Acceptance Criteria section that Phase 62 closed Tracks C and D. Update Phase 57b's design doc: replace the current "(post-merge soak tracked as a Phase 59 Track G item)" qualifier on the `Status:` line with a one-line soak-result note citing the populated `docs/handoffs/57b-soak-gate.md` table row. Add a cross-reference to the Phase 57e post-mortem (`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`) for readers who want the full preemption-model history.

## Important Components and How They Work

### Four `TODO(57a-C/D)` sites in `kernel/src/task/scheduler.rs`

Lines ~892, ~4108, ~4120, ~4319 (post-Phase 61; re-verify with `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` immediately before starting Track B — line numbers drift on every kernel change). These are `task.state = ...` stores that bypass the `pi_lock` + `with_block_state` abstraction that Phase 57a introduced for the main block/wake protocol. Each site was left with a TODO because the Phase 57a rewrite ran out of time budget before reaching these paths. They are not all "edge cases" — Site 4 is the dispatch hot path (every context switch). Site 1 is queue-scan defensive cleanup. Sites 2 and 3 are `#[cfg(test)] install_test_task_idx` test scaffolding. Sites 1 and 4 already hold `scheduler_lock()` when they touch state, which constrains the fix shape because of the pi_lock-outer / SCHEDULER-inner ordering rule.

### `IrqSafeMutex` and `preempt_count`

`IrqSafeMutex` calls `preempt_disable` on `lock()` and `preempt_enable` on `Drop`. `preempt_disable` increments `preempt_count[cpu]`; `preempt_enable` decrements it. If a guard is held across `block_current_until`, `preempt_count` is elevated when the task deschedules. When the task wakes on a (possibly different) CPU, `preempt_count` is already non-zero; the guard's `Drop` brings it to zero, triggering `check_and_yield()`, which may reschedule at a point where the caller assumed no reschedule could occur.

### Option-B Arc-clone vs. Option-C release-before-block

The Phase 57a post-mortem proposes three options for the guard-across-block pattern. Option-A (convert the protected data to a lock-free atomic) requires API redesign. Option-B (Arc-clone before the block, drop guard before block, re-acquire after wake) preserves the existing API while eliminating the lifetime violation; it is appropriate when the caller needs the heap-resident protected data after wake. Option-C (release-before-block / copy-out — drop the guard before the block, copying out any scalar needed after wake) is appropriate when the protected data is small or not needed after wake. The session-15 fix for `sys_mmap_file_backed` is **Option-C**: `lock_page_tables()` is dropped before `kernel_read_fd_at` and re-acquired after, with no Arc-clone involved. Track C applies whichever option the Track A.2 inventory recommends per site.

### `assert_preempt_count_zero_at_user_return` (existing, from Phase 57b D.3 + Phase 57e Bug #9)

The user-mode return path already enforces `preempt_count == 0` on every IRQ-/syscall-return to ring 3:
- `kernel/src/arch/x86_64/interrupts.rs:96` defines `assert_preempt_count_zero_on_return_to_user` — a wrapper gated on `code_segment.rpl() == Ring3` that calls into the scheduler helper.
- `kernel/src/task/scheduler.rs:2292` defines `assert_preempt_count_zero_at_user_return` — `debug_assert!` in debug builds, clamp-to-zero in release builds (Phase 57e Bug #9 behaviour).

The wrapper is called from ~13 IRQ handlers and the syscall-return path. Phase 62 does not need to add a new assertion or a first-return latch — the existing assertion is **stronger** (every-return, not first-return) and is already exercised by the test harness on every syscall. Phase 62 contributes a deliberate-leak regression test that proves the assertion catches a guard-across-block reintroduction; without that test, a future regression could pass `cargo xtask test` because no current test exercises the leak path.

## How This Builds on Earlier Phases

- Completes Phase 57a Tracks C and D, which were the only substantively unclosed tracks in the Phase 57a rewrite.
- Extends Phase 57b's `preempt_count` discipline (F.1 wiring) by adding Option-B / Option-C fixes for the sites that held guards across blocks.
- Reuses the `with_block_state` API from Phase 57a Track B and the `assert_preempt_count_zero_at_user_return` helper from Phase 57b Track D.3 — no new public API is introduced. A `pi_lock_locked_under_scheduler` helper may be added if Track A.1 / B.1 determines that release-and-reacquire SCHEDULER is the wrong shape for sites that already hold the inner lock.
- Closes the Bug #9 residual documented in `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`.
- Depends on Phase 59 Track G to populate `docs/handoffs/57b-soak-gate.md` with the Phase 57b 30-minute soak result; Track F.2 cannot complete until that result is recorded.
- Cross-references Phase 57e's deferred-preemption post-mortem to give readers the full preemption history in one place.

## Implementation Outline

The `pi_lock` + `with_block_state` abstraction embodies the Open/Closed Principle: the four bare `task.state` stores are modifications to the block/wake protocol at sites that were supposed to be closed to direct mutation — routing them through `with_block_state` (or a documented locked-helper variant for sites that hold `scheduler_lock()`) is the correction that makes the abstraction uniformly closed to bypass. For Track C, write a host-side `kernel-core` test that models the Option-B / Option-C pattern for each callsite shape before applying it to the kernel; this TDD pass catches incorrect lock-release ordering at the host level — where iteration is instant — before exercising the QEMU path.

1. Track A.1: inventory the four `TODO` sites — record current line numbers, surrounding lock context (does the site already hold `scheduler_lock()`?), and chosen fix shape per site. Track A.2: kernel-wide grep for `block_current_until`, classify every callsite (live guard / released-before-block / not preempt-affecting), record per-site recommended fix in `docs/handoffs/62a-pi-lock-inventory.md`.
2. Track B.1: route Site 1 (~line 892, queue-scan defensive cleanup) — choose between release-and-reacquire SCHEDULER or a `pi_lock_locked_under_scheduler` helper; document the choice; run `cargo xtask test`.
3. Track B.2: route Sites 2 and 3 (~lines 4108, 4120, `install_test_task_idx`) using the same shape as B.1; run `cargo xtask test`.
4. Track B.3: route Site 4 (~line 4319, dispatch hot path) using the same shape as B.1; run `cargo xtask test`; verify dispatch-path microbench shows no regression.
5. Track C: apply Option-B or Option-C fix to each inventory site per Track A.2's recommendation; run `cargo xtask test` after every batch of ~5 sites; tag each conversion with a `// NOTE:` comment.
6. Track D: add the deliberate-guard-leak regression test that exercises the existing `assert_preempt_count_zero_at_user_return`; verify both the negative case (leak → assertion fires) and the positive case (post-Track-C syscall → assertion does not fire).
7. Track E.1: full `cargo xtask check` + `cargo xtask test` pass. Track E.2: 30-minute soak using the Phase 59 Track G procedure (`cargo xtask run-gui --fresh`, ≥ 4 cores, 30 min, IPC + futex + notification synthetic load); capture log; populate `docs/handoffs/62e-pi-lock-soak.md` and append a row to `docs/handoffs/57b-soak-gate.md`'s table.
8. Track F: flip Phase 57a task-doc Tracks C/D; update Phase 57a design-doc Status to `Complete`; update Phase 57b design-doc Status with the soak result reference; cross-reference the Phase 57e post-mortem.

## Acceptance Criteria

- Zero `// TODO(57a-C/D)` markers remain in `kernel/src/task/scheduler.rs`.
- All four former `TODO` sites use `with_block_state` (or a documented pi_lock-aware locked-helper for sites that already hold `scheduler_lock()`); each site carries a `// NOTE:` comment explaining the chosen lock-order resolution.
- `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` returns no output.
- Every guard-across-block callsite identified in Track A.2 is either converted to Option-B / Option-C or explicitly tagged "verified safe — not preempt-affecting"; the inventory's final count is recorded in `docs/handoffs/62a-pi-lock-inventory.md`.
- A `#[cfg(test)]` regression test deliberately leaks a guard across `block_current_until` and confirms that `assert_preempt_count_zero_at_user_return` panics on the resulting return-to-ring-3 in debug builds; the complementary positive case (post-Track-C syscall) does not trip the assertion.
- No new assertion is introduced; the existing Phase 57b D.3 / Phase 57e Bug #9 every-return assertion remains the gate.
- 30-minute QEMU soak (Track E.2, Phase 59 Track G procedure: `cargo xtask run-gui --fresh`, ≥ 4 cores, 30 min) completes with zero panics and no new `[WARN] [sched]` lines vs. the Phase 59 Track G baseline.
- Phase 57a task-doc Tracks C and D are fully checked.
- Phase 57a design doc `Status:` is `Complete` (it currently is — confirmed at HEAD; this phase's closure is referenced in its Acceptance Criteria section).
- Phase 57b design doc `Status:` is `Complete` (it currently is — confirmed at HEAD); the "post-merge soak tracked as a Phase 59 Track G item" qualifier is replaced with a one-line soak result note pointing at the populated `docs/handoffs/57b-soak-gate.md` table row.

## Companion Task List

- [Phase 62 Task List](./tasks/62-phase-57a-pi-lock-closeout-tasks.md)

## How Real OS Implementations Differ

- Linux's `p->pi_lock` (priority-inheritance lock, despite the name, is a plain raw spinlock in non-RT builds) is held during every `try_to_wake_up` and `finish_task_switch`. m3OS's `pi_lock` follows the same design principle.
- Linux's lockdep subsystem would catch guard-across-block patterns at runtime by tracking lock acquisition order against a known-safe set. m3OS uses the simpler `preempt_count == 0` assertion at the user-mode boundary instead.
- Linux's RT kernel (PREEMPT_RT) converts most spinlocks to `rt_mutex` (sleeping mutex) to make the kernel fully preemptible. m3OS deferred full kernel preemption (Phase 57e) and does not need RT-mutex semantics.

## Deferred Until Later

- Full kernel preemption (PREEMPT_FULL) — deferred indefinitely per Phase 57e post-mortem; this phase does not reopen that scope.
- Lockdep-equivalent lock-order tracking — post-1.0.
- Priority-inheritance semantics for `pi_lock` (the name is borrowed from Linux; m3OS uses it as a plain spinlock) — post-1.0.
- Removing the Phase 57e Bug #9 release-build clamp from `assert_preempt_count_zero_at_user_return` (so release builds also panic on guard leak) — post-1.0; requires a track record of clean soak results to justify removing the safety net.
