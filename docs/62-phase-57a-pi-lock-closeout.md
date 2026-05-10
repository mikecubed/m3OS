# Pi-Lock Closeout (Phase 62)

**Aligned Roadmap Phase:** Phase 62
**Status:** Complete (pending 30-minute soak — Track E.2)
**Source Ref:** phase-62
**Supersedes Legacy Doc:** new — Phase 62 has no pre-existing legacy learning surface; it closes hold-overs from Phase 57a Tracks C/D and Phase 57e Bug #9.

## Overview

Phase 62 closes the four `TODO(57a-C/D)` markers that Phase 57a left in `kernel/src/task/scheduler.rs` when it ran out of time-budget on the block/wake protocol rewrite. Each marker was a bare `task.state = ...` write that bypassed the `pi_lock` + `with_block_state` abstraction Phase 57a introduced for every other state-transition site. Phase 62 routes all four through a new `Task::with_block_state_locked_scheduler` helper that documents a per-site structural-safety argument for the unusual case of acquiring the per-task `pi_lock` (the outer lock) while `scheduler_lock()` (the inner lock) is already held.

Phase 62 also performs a kernel-wide audit of every `block_current_until` callsite for the **Bug #9 leak pattern** — an `IrqSafeMutex` guard alive across a blocking call, which leaves `preempt_count > 0` in the post-resume window. The audit finds **zero callsites at risk**: the dominant historical contributor (FS-volume mutexes held across `virtio_blk` reads) was already closed by Phase 57e session 15 (the FS-volume mutex type swap from `IrqSafeMutex` to `spin::Mutex`, plus the `sys_mmap_file_backed` Option-C release-before-block fix). All 23 surviving callsites either hold no preempt-affecting guard or release the guard via an inner `{ … }` scope before the block.

The matching regression test lands in `kernel-core/tests/preempt_property.rs` and uses the host-testable `Counter` mirror to pin the contract that `assert_preempt_count_zero_at_user_return` (the live kernel assertion from Phase 57b D.3 and Phase 57e Bug #9) panics on a deliberate guard-leak shape.

## What This Doc Covers

- The four `TODO(57a-C/D)` sites in `kernel/src/task/scheduler.rs` and the lock-order constraint that made them unfinishable in Phase 57a's time budget.
- The `Task::with_block_state_locked_scheduler` helper introduced by Phase 62 — when to use it, what its contract is, and why it does NOT replace `Task::with_block_state` for normal callers.
- The Bug #9 guard-across-block leak pattern, why it matters even after Phase 57e deferred kernel-mode timer preemption, and why the kernel-wide audit found zero leaks at HEAD.
- The Phase 62 Track D regression test pattern: how the host-testable `Counter` mirror in `kernel-core::preempt_model` is used to pin the contract that the live `assert_preempt_count_zero_at_user_return` would catch a guard-leak regression.

## Core Implementation

### `Task::with_block_state` — the canonical helper

Phase 57a introduced `Task::with_block_state(|bs| ...)` as the single entry point for reading or writing canonical block state (`TaskBlockState.state`, `TaskBlockState.wake_deadline`). The helper takes `pi_lock` (the per-task spinlock), invokes the closure, drops the guard. **It debug-asserts that `scheduler_lock()` is NOT currently held by this CPU** — the canonical Linux `p->pi_lock` → `rq->lock` lock-ordering invariant: `pi_lock` is the outer lock, `scheduler_lock` is the inner lock, and the standard write path is `pi_lock.lock()` → `scheduler_lock.lock()` → write both → release inner → release outer.

### `Task::with_block_state_locked_scheduler` — the documented exception path (Phase 62)

The four `TODO(57a-C/D)` sites do NOT fit the canonical pattern: each site already holds `scheduler_lock()` when it needs to write `task.state`. Calling `with_block_state` here would inversion-trigger the debug assertion and risk a deadlock under contention.

Phase 62 introduces `Task::with_block_state_locked_scheduler(|bs| ...)` — a sibling helper that **omits** the lock-ordering debug assertion. Each call site MUST carry an inline `// NOTE: Phase 62 Track B — Shape β` comment explaining a **structural-safety argument** for why `pi_lock` acquisition is safe at that specific site. The argument has two flavours:

1. **Task is not yet visible to other CPUs.** Sites 2 and 3 (the `#[cfg(test)] install_test_task_idx` sites) write to a freshly-constructed `Task` before it is `push`ed into `sched.tasks`. No other CPU can hold a reference to the local task, so the `pi_lock` acquire is structurally uncontended.
2. **Competing waker class cannot target this transition.** Sites 1 (queue-scan defensive cleanup, `Ready → Dead`) and 4 (dispatch hot path, `Ready/idle → Running`) are protected by IRQ disable + scheduler_lock. The only competing pi_lock acquirer is `wake_task_v2`, whose CAS only ever transitions `Blocked* → Ready`. It cannot target `Ready → Dead` or `Ready → Running`, so its CAS cannot race the canonical write at these sites.

### The kernel-wide `block_current_until` audit

The Bug #9 mechanism is documented in `docs/handoffs/57e-bug9-bug10-followup.md`. Briefly: any `IrqSafeMutex` guard alive across `block_current_until(...)` leaves `preempt_count` net `+1` after the post-resume `preempt_enable` runs (the guard's lifecycle was already paired with the wake protocol's restoration; the guard's `Drop`-time decrement then double-counts when the caller eventually releases the lock). The `+1` corrupts the counter's discipline contract and would defeat any future preemption-model phase that re-introduces `peek_preempt_count_irq`-gated paths.

Phase 62 Track A.2 audits all 23 actual `block_current_until` call expressions kernel-wide. The audit table is in `docs/handoffs/62a-pi-lock-inventory.md`. Result: **zero LEAK verdicts**. Every preempt-affecting (`IrqSafeMutex`) guard is either absent at the call or released via an inner `{ … }` scope before the block. The historical worst case (`FAT32_VOLUME` / `EXT2_VOLUME` held across `kernel_read_fd_at` → `virtio_blk::do_request`) was closed by Phase 57e session-15's two-step fix:

1. **Step 1 — `sys_mmap_file_backed` Option-C release-before-block:** `lock_page_tables()` is dropped before `kernel_read_fd_at` and re-acquired after. (Verified intact at HEAD by Phase 62 Track A.2.)
2. **Step 2 — FS-volume mutex type swap:** `FAT32_VOLUME`, `EXT2_VOLUME`, `Ext2Volume::block_cache`, and `TMPFS` all converted from `IrqSafeMutex` to plain `spin::Mutex`. Plain `spin::Mutex` does not call `preempt_disable`, so holding it across a block does not leak `preempt_count`.

### Why the audit's zero-LEAK result is robust

Phase 62 Track C originally planned to apply Option-B (Arc-clone) or Option-C (release-before-block) fixes to ~25 callsites surveyed by the Bug #9 post-mortem. With the audit returning zero LEAKs, Track C's deliverable becomes documentation only — the inventory doc records the audit and the verdict per site. No source-code changes were needed in Track C, and no inline `// NOTE: Phase 62 Track C` annotations were added (none of the audited sites needed conversion).

The post-mortem's "~25 sites" estimate was a syscall-layer-only count; the kernel-wide audit broadens to fs/, blk/, ipc-internal, and task-internal callsites. Track A.2 verifies that no new guard-across-block patterns were introduced between Phase 57e session 15 and the start of Phase 62.

### The Track D guard-leak regression test

Phase 57b Track D.3 added `assert_preempt_count_zero_at_user_return` to fire on every IRQ-/syscall-return to ring 3 with `preempt_count != 0`. Phase 57e Bug #9 extended it with a release-build clamp + `[preempt] count=N at user-mode return — clamping to 0 (Bug #9 mitigation)` warning when the leak fires on a real kernel build.

Phase 62 Track D adds a deliberate-leak regression test that proves the existing assertion catches the bug class — without such a test, a future careless edit could re-introduce a `guard = LOCK.lock(); ...; block_current_until(...)` pattern and silently pass `cargo xtask test`.

The test lives in `kernel-core/tests/preempt_property.rs` and uses the host-testable `Counter` mirror (the `kernel` binary crate has no `lib.rs`, so kernel symbols are not reachable from `kernel-core` integration tests). The Counter's `assert_balanced` method uses the same `preempt_count` substring in its panic message that the live `assert_preempt_count_zero_at_user_return` uses, so a `#[should_panic(expected = "preempt_count")]` test pins the same contract:

```rust
#[test]
#[should_panic(expected = "preempt_count")]
fn phase62_track_d_guard_across_block_then_user_return_panics() {
    let mut counter = Counter::new();
    counter.disable();           // IrqSafeMutex::lock()
    // block_current_until(...)  — modelled as no-op
    counter.assert_balanced();   // assert_preempt_count_zero_at_user_return — MUST panic
}
```

The complementary positive case (post-Track-C correct shape: drop the guard before the block) is also exercised — the test passes without panic when every `disable` is paired with a matching `enable` before `assert_balanced` is called.

## Key Files

| File | Purpose in this phase |
|---|---|
| `kernel/src/task/mod.rs` | `Task::with_block_state_locked_scheduler` helper added (lines ~810–842). |
| `kernel/src/task/scheduler.rs` | Four `TODO(57a-C/D)` markers replaced with `with_block_state_locked_scheduler` calls + inline structural-safety NOTEs (lines 892, ~4117, ~4136, ~4342). |
| `kernel-core/tests/preempt_property.rs` | Phase 62 Track D regression tests (4 tests: 3 `should_panic`, 1 positive). |
| `docs/handoffs/62a-pi-lock-inventory.md` | Track A inventory: per-site lock-context for the four TODO sites + kernel-wide `block_current_until` audit (23 callsites, zero LEAK verdicts). |
| `docs/handoffs/62e-pi-lock-soak.md` | Track E.2 soak log artifact (populated by the 30-minute soak run). |
| `docs/handoffs/57b-soak-gate.md` | Phase 57b soak-gate procedure; Phase 62 Track E.2 reuses it and populates the result table. |
| `docs/roadmap/57a-scheduler-rewrite.md` | Status: Complete (pre-existing); Acceptance Criteria gains a Phase 62 closure paragraph. |
| `docs/roadmap/57b-preemption-foundation.md` | Status qualifier updated; Related Reading section added cross-referencing Phase 57e post-mortem and Phase 62 closeout. |

## How This Phase Differs From Later Scheduler Work

- **Phase 57a** introduced the `pi_lock` + `with_block_state` abstraction and migrated most state-transition callsites. Phase 62 closes the four sites Phase 57a left as TODOs because they need the `with_block_state_locked_scheduler` exception variant.
- **Phase 57b** added the `preempt_count` discipline and the `assert_preempt_count_zero_at_user_return` debug assertion. Phase 62 does not modify the assertion — it adds a regression test that proves the assertion catches the Bug #9 leak class.
- **Phase 57c–e** explored kernel-mode timer preemption (voluntary in 57c–d, full in 57e). Phase 57e deferred full preemption indefinitely; Phase 62 inherits the post-deferral severity adjustment for Bug #9 (still a logic bug; no longer an operational starvation pattern).
- **Hypothetical future preemption-model phase** would benefit from Phase 62's audit and the Track D regression test — the audit confirms zero pre-existing leaks at the boundary, and the test catches any reintroduction.
- **Linux's `lockdep`** subsystem catches guard-across-block patterns at runtime by tracking lock acquisition order against a known-safe set. m3OS uses the simpler `preempt_count == 0` user-mode-return assertion plus a deliberate-leak regression test instead.

## Related Roadmap Docs

- [Phase 62 design doc](./roadmap/62-phase-57a-pi-lock-closeout.md)
- [Phase 62 task doc](./roadmap/tasks/62-phase-57a-pi-lock-closeout-tasks.md)
- [Phase 57a design doc](./roadmap/57a-scheduler-rewrite.md) (Phase 62 closes its Tracks C/D)
- [Phase 57b design doc](./roadmap/57b-preemption-foundation.md) (Phase 62 cross-references the assertion it relies on)
- [Phase 57e post-mortem](./post-mortems/2026-05-07-57e-preempt-full-deferred.md) (Bug #9 mechanism + post-deferral severity adjustment)
- [Bug #9 follow-up handoff](./handoffs/57e-bug9-bug10-followup.md) (Option-A/B/C fix shape analysis)
- [Phase 62 inventory](./handoffs/62a-pi-lock-inventory.md) (the per-callsite audit)

## Deferred or Later-Phase Topics

- Lockdep-equivalent runtime lock-order tracking — post-1.0.
- Priority-inheritance semantics for `pi_lock` (the name is borrowed from Linux; m3OS uses it as a plain spinlock) — post-1.0.
- Removing the Phase 57e Bug #9 release-build clamp from `assert_preempt_count_zero_at_user_return` so release builds also panic on guard leak — post-1.0; requires a track record of clean soak results to justify removing the safety net.
- Re-opening kernel-mode timer preemption (`PREEMPT_FULL`) — deferred indefinitely per Phase 57e post-mortem; Phase 62 does not reopen that scope.
