# Phase 62 — Phase 57a Pi-Lock Closeout

**Status:** Planned
**Source Ref:** phase-62
**Depends on:** Phase 57a (Scheduler Block/Wake Protocol Rewrite) ✅, Phase 57b (Preemption Foundation) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅, Phase 59 (Validation Backlog) ✅
**Builds on:** Completes the Phase 57a Tracks C and D delivery — routing the four `task.state` stores that bypass `pi_lock` through the correct abstraction, and landing the Bug #9 Option-B Arc-clone fix for the ~25 callsites that hold `IrqSafeMutex` guards across `block_current_until`. Adds a `preempt_count == 0` invariant assertion at the user-mode return boundary to catch future guard leaks at first offence.
**Primary Components:** `kernel/src/task/scheduler.rs` (four `TODO(57a-C/D)` sites, `preempt_count` assertion), `kernel/src/arch/x86_64/syscall/mod.rs` (~25 callsites with guard-across-block patterns), `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` (Bug #9 source), `docs/handoffs/57b-soak-gate.md` (soak result, populated by Phase 59 Track G), `docs/roadmap/57a-scheduler-rewrite.md`, `docs/roadmap/57b-preemption-foundation.md`

## Milestone Goal

The four `// TODO(57a-C/D): route through pi_lock + with_block_state` markers in `kernel/src/task/scheduler.rs` (lines 829, 3649, 3656, 3855 post-rebase) are eliminated: each bare `task.state = ...` store is replaced by a call to the `with_block_state` helper (or its pi_lock equivalent) introduced in Phase 57a Tracks C/D. The ~25 callsites that hold an `IrqSafeMutex` guard across `block_current_until` are converted to Option-B (Arc-clone of the guard before the block, drop before returning). A `preempt_count == 0` assertion fires on the first user-mode return per CPU if any guard was leaked. Phase 57a's task-doc Tracks C and D are fully checked. Phase 57b's design-doc Status is updated to `Complete` with the soak result from Phase 59 Track G.

## Why This Phase Exists

Phase 57a introduced a `pi_lock` per-task spinlock and a `with_block_state` helper to make block/wake transitions atomic. The four `TODO` markers record the sites where the Phase 57a rewrite reached the time-budget boundary and was not completed. Under PREEMPT_VOLUNTARY (the production model post-57e-deferral), the bare `task.state` stores at these sites are less likely to interact with a preemption window than under PREEMPT_FULL — but they are still genuine integrity gaps. Any future phase that re-examines preemption relies on the abstraction being uniformly applied.

Bug #9 (from `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`) documents the `preempt_count` leak pattern: an `IrqSafeMutex` guard held across `block_current_until` keeps `preempt_count > 0` at the moment the task deschedules. When the task reschedules, `preempt_count` is already elevated; `IrqSafeMutex::drop` then decrements it to zero, triggering a deferred reschedule at an unexpected point. The session-15 fix landed Option-B for one callsite (`sys_mmap_file_backed`). The post-mortem documents ~25 additional callsites with the same shape.

The `preempt_count == 0` assertion at the user-mode return boundary makes this class of bug self-announcing: instead of silently elevating preempt_count until the next subtle timing-dependent failure, the kernel asserts loudly on the first user-mode return after a guard was leaked, catching new regressions at the point of introduction.

## Learning Goals

- Why routing `task.state` mutations through a per-task spinlock (`pi_lock`) is necessary for the block/wake abstraction to be race-free.
- How `IrqSafeMutex` interacts with `preempt_count`: acquiring the lock disables preemption; holding it across a blocking call means the task deschedules with preemption disabled, corrupting the counter for the wakeup path.
- Why Option-B (Arc-clone guard, drop before blocking) is the correct fix: the caller retains the protected data via an Arc reference, releases the lock before blocking, and re-acquires after waking.
- How a `preempt_count == 0` invariant assertion at the user-mode return gate catches guard leaks without any runtime overhead on the hot path (it fires once per CPU per boot, at first user-mode return).

## Feature Scope

### Track A — Inventory the Four `TODO(57a-C/D)` Sites and the ~25 Guard-Across-Block Sites

Walk `kernel/src/task/scheduler.rs` for `TODO(57a-C/D)` comments; walk `kernel/src/arch/x86_64/syscall/mod.rs` (and any other kernel callsites) for the pattern `guard = LOCK.lock(); ... block_current_until(...)` where the guard is live across the block. Produce an inventory doc (`docs/handoffs/62a-pi-lock-inventory.md`) that lists every site, its file, line, and the nature of the fix required.

### Track B — Route the Four `TODO` Sites Through `pi_lock` + `with_block_state`

For each of the four sites at `scheduler.rs:829, 3649, 3656, 3855`, replace the bare `task.state = BLOCKED_STATE` (or equivalent) with the Phase 57a `with_block_state` helper (or direct `pi_lock` acquire + store + release sequence if `with_block_state` doesn't cover the site's shape). Each site must hold the `pi_lock` for the duration of the state transition, matching the invariant established for the other block/wake sites in Phase 57a.

### Track C — Option-B Arc-Clone Fix for the ~25 Callsites

For each callsite in the inventory where an `IrqSafeMutex` guard is held across `block_current_until`:
1. Arc-wrap the protected data if not already behind an `Arc`.
2. Clone the `Arc` before the block.
3. Drop the guard (release the lock + decrement `preempt_count`) before calling `block_current_until`.
4. Re-acquire the lock after waking if the protected data is still needed.

Some callsites may have simpler fixes (copy the needed value out before dropping the guard, rather than re-acquiring). The inventory (Track A) notes the recommended fix shape per site.

### Track D — `preempt_count == 0` Invariant Assertion at User-Mode Return Boundary

In the user-mode return path (the assembly stub in `kernel/src/arch/x86_64/interrupts.rs` or `syscall/mod.rs` that `sysret`s or `iret`s to ring 3), add a one-time-per-CPU assertion: on the very first return to user mode after a core becomes active, assert `preempt_count[cpu] == 0`. After the first return, the assertion is disabled to avoid overhead on the hot path. A `preempt_count != 0` at this point means a guard was leaked through the kernel-to-user transition.

The assertion must be reachable in the `cargo xtask test` harness to be verifiable.

### Track E — Regression Suite Under `preempt-voluntary`

Run `cargo xtask test` (full suite) and a 30-minute QEMU soak under PREEMPT_VOLUNTARY after Tracks B, C, and D land. This is the regression gate for the pi_lock wiring and the Option-B guard fix. Under voluntary preemption, a `preempt_count` imbalance from a missed Arc-clone will eventually surface as a deferred-reschedule at an unexpected point.

### Track F — Phase 57a and 57b Doc Updates

Flip Phase 57a task-doc Tracks C and D checkboxes. Update Phase 57a's design doc `Status:` to reflect that the four `TODO` sites are now closed. Update Phase 57b's design doc: replace the stale "pending soak (PR #132)" qualifier with the soak result from `docs/handoffs/57b-soak-gate.md` (populated by Phase 59 Track G). Add a cross-reference to the Phase 57e post-mortem (`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`) for readers who want the full preemption-model history.

## Important Components and How They Work

### Four `TODO(57a-C/D)` sites in `kernel/src/task/scheduler.rs`

Lines 829, 3649, 3656, 3855 (post-rebase). These are `task.state = ...` stores that bypass the `pi_lock` + `with_block_state` abstraction that Phase 57a introduced for the main block/wake protocol. Each site was left with a TODO because the Phase 57a rewrite ran out of time budget before reaching these edge-case paths (e.g., the reaper path at line 829, and three sites in the deadline-scanner and timeout paths in the 3600s range).

### `IrqSafeMutex` and `preempt_count`

`IrqSafeMutex` calls `preempt_disable` on `lock()` and `preempt_enable` on `Drop`. `preempt_disable` increments `preempt_count[cpu]`; `preempt_enable` decrements it. If a guard is held across `block_current_until`, `preempt_count` is elevated when the task deschedules. When the task wakes on a (possibly different) CPU, `preempt_count` is already non-zero; the guard's `Drop` brings it to zero, triggering `check_and_yield()`, which may reschedule at a point where the caller assumed no reschedule could occur.

### Option-B Arc-clone

The Phase 57a post-mortem proposes two options for the guard-across-block pattern. Option-A (convert the protected data to a lock-free atomic) requires API redesign. Option-B (Arc-clone before the block, drop guard before block, re-acquire after wake) preserves the existing API while eliminating the lifetime violation. Option-B is the correct fix for callsites that hold large, heap-resident objects; callsites that hold only a scalar can use Option-C (copy-out).

### `preempt_count == 0` first-return assertion

The user-mode return path already checks `preempt_count` to decide whether to call `preempt_schedule_irq` before returning (Phase 57b). Extending it to assert `== 0` (with a per-CPU latch that fires only once) adds two instructions on the hot path. After the first return, the latch bit is set and the check is skipped. Any future guard leak will be caught by the QEMU test harness's first user-mode return — not by a silent preemption anomaly hours later.

## How This Builds on Earlier Phases

- Completes Phase 57a Tracks C and D, which were the only substantively unclosed tracks in the Phase 57a rewrite.
- Extends Phase 57b's `preempt_count` discipline (F.1 wiring) by adding the Option-B fix for the sites that held guards across blocks.
- Reuses the `with_block_state` API from Phase 57a Track B — no new API is introduced.
- Closes the Bug #9 residual documented in `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`.
- Cross-references Phase 57e's deferred-preemption post-mortem to give readers the full preemption history in one place.

## Implementation Outline

The `pi_lock` + `with_block_state` abstraction embodies the Open/Closed Principle: the four bare `task.state` stores are modifications to the block/wake protocol at sites that were supposed to be closed to direct mutation — routing them through `with_block_state` is the correction that makes the abstraction uniformly closed to bypass. For Track C, write a host-side `kernel-core` test that models the Option-B Arc-clone pattern for each callsite shape before applying it to the kernel; this TDD pass catches incorrect Arc-clone ordering at the host level — where iteration is instant — before exercising the QEMU path.

1. Track A: inventory all four `TODO` sites and the ~25 guard-across-block sites; produce `docs/handoffs/62a-pi-lock-inventory.md`.
2. Track B: route site at line 829 (reaper path) through `pi_lock` + `with_block_state`; run `cargo xtask test`.
3. Track B: route sites at lines 3649, 3656 (deadline scanner) through `pi_lock` + `with_block_state`; run `cargo xtask test`.
4. Track B: route site at line 3855 (timeout path) through `pi_lock` + `with_block_state`; run `cargo xtask test`.
5. Track C: apply Option-B (or Option-C) fix to each of the ~25 inventory sites; run `cargo xtask test` after every 5 sites.
6. Track D: add `preempt_count == 0` first-return assertion; run `cargo xtask test` to verify the assertion fires and passes.
7. Track E: 30-minute QEMU soak under PREEMPT_VOLUNTARY; capture log.
8. Track F: flip Phase 57a task-doc tracks C/D; update Phase 57a and 57b design docs.

## Acceptance Criteria

- Zero `// TODO(57a-C/D)` markers remain in `kernel/src/task/scheduler.rs`.
- All four former `TODO` sites use `with_block_state` (or equivalent pi_lock + state-store + pi_lock release).
- `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` returns no output.
- All ~25 guard-across-block callsites identified in Track A are converted to Option-B or Option-C.
- The `preempt_count == 0` first-return assertion is present and fires in `cargo xtask test` without tripping (confirming no leaked guards in the test workload).
- 30-minute QEMU soak (Track E) completes without panic.
- Phase 57a task-doc Tracks C and D are fully checked.
- Phase 57b design doc `Status:` is `Complete` with the soak result from `docs/handoffs/57b-soak-gate.md` replacing the stale "pending soak" qualifier.

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
- Extending the `preempt_count == 0` assertion to fire on every user-mode return (not just the first per CPU) — post-1.0 performance analysis required to confirm the overhead is acceptable.
