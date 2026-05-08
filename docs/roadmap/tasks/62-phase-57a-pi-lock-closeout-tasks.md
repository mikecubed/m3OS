# Phase 62 — Phase 57a Pi-Lock Closeout: Task List

**Status:** Planned
**Source Ref:** phase-62
**Depends on:** Phase 57a (Scheduler Block/Wake Protocol Rewrite) ✅, Phase 57b (Preemption Foundation) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅, Phase 59 (Validation Backlog) ✅
**Goal:** Eliminate the four `TODO(57a-C/D)` sites in `kernel/src/task/scheduler.rs` by routing them through `pi_lock` + `with_block_state`; apply the Option-B Arc-clone fix to ~25 callsites holding `IrqSafeMutex` guards across `block_current_until`; add a `preempt_count == 0` first-return assertion; run a 30-minute PREEMPT_VOLUNTARY soak; flip Phase 57a Tracks C/D and update Phase 57b design-doc Status with the Phase 59 soak result.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Inventory four `TODO` sites + ~25 guard-across-block sites | — | Planned |
| B | Route four `TODO` sites through `pi_lock` + `with_block_state` | A | Planned |
| C | Option-B Arc-clone fix for ~25 guard-across-block callsites | A | Planned |
| D | `preempt_count == 0` first-return assertion | B C | Planned |
| E | Regression suite + 30-minute PREEMPT_VOLUNTARY soak | B C D | Planned |
| F | Phase 57a/57b doc updates; Phase 57e post-mortem cross-reference | E | Planned |

---

## Track A — Inventory

### A.1 — Inventory the four `TODO(57a-C/D)` sites

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** lines 829, 3649, 3656, 3855 (post-rebase; verify line numbers against current HEAD)
**Why it matters:** The TODO comments are the authoritative list of Phase 57a Tracks C/D incomplete work. Verifying exact line numbers and understanding the code context at each site is necessary before writing the fix.

**Acceptance:**
- [ ] `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` output reviewed; all four lines identified and their context documented in `docs/handoffs/62a-pi-lock-inventory.md`.
- [ ] For each site: current `task.state = ...` store described; required `with_block_state` call shape identified.
- [ ] If any site's line number has shifted from the audit values (829, 3649, 3656, 3855), the correct current line is recorded.

### A.2 — Inventory the ~25 guard-across-block callsites

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/task/scheduler.rs`
- `kernel/src/ipc/endpoint.rs` (potential additional sites)

**Symbol:** pattern `<guard> = <LOCK>.lock(); ... block_current_until(...)` where guard is live across the call
**Why it matters:** Bug #9 source (`docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`) documents the preempt_count leak; Option-B (Arc-clone) is the documented general fix. The ~25 estimate comes from the post-mortem; the exact count must be verified.

**Acceptance:**
- [ ] `grep -n 'block_current_until' kernel/src/arch/x86_64/syscall/mod.rs kernel/src/task/scheduler.rs kernel/src/ipc/endpoint.rs` output reviewed.
- [ ] For each `block_current_until` call: guard liveness at call site checked (is any `IrqSafeMutex` guard in scope?).
- [ ] Inventory table in `docs/handoffs/62a-pi-lock-inventory.md` lists each site with: file, line, lock type, recommended fix (Option-B Arc-clone or Option-C copy-out).
- [ ] Total count of guard-across-block sites recorded; if materially different from ~25, the discrepancy is noted.

---

## Track B — Pi-Lock TODO Sites

### B.1 — Route `scheduler.rs:829` (reaper path) through `pi_lock` + `with_block_state`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** site at line ~829 (reaper / task-exit path `task.state = ...` store)
**Why it matters:** The reaper path sets a task's state to Dead/Zombie outside the `pi_lock` hold. If a wake races with the reaper, the wake may observe an inconsistent state.

**Acceptance:**
- [ ] `task.state = ...` store at line ~829 replaced by `with_block_state` (or explicit `pi_lock.lock(); task.state = ...; drop(pi_lock_guard)`) with the `pi_lock` held for the full transition.
- [ ] `// TODO(57a-C/D)` comment removed.
- [ ] `cargo xtask test` passes.

### B.2 — Route `scheduler.rs:3649` and `3656` (deadline scanner) through `pi_lock` + `with_block_state`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** sites at lines ~3649 and ~3656 (deadline scanner / timeout expiry)
**Why it matters:** The deadline scanner transitions blocked tasks to Runnable when their deadline expires. Without `pi_lock`, a concurrent wake from an IPC reply can race with the deadline transition.

**Acceptance:**
- [ ] Both `task.state` stores at ~3649 and ~3656 routed through `pi_lock` + `with_block_state`.
- [ ] `// TODO(57a-C/D)` comments removed from both sites.
- [ ] `cargo xtask test` passes after both changes.

### B.3 — Route `scheduler.rs:3855` (timeout path) through `pi_lock` + `with_block_state`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** site at line ~3855 (timeout / `ETIMEOUT` transition)
**Why it matters:** The timeout path sets task state to Runnable (with an ETIMEOUT result) on deadline expiry. Same race shape as B.2 but in the timeout-specific code path.

**Acceptance:**
- [ ] `task.state` store at ~3855 routed through `pi_lock` + `with_block_state`.
- [ ] `// TODO(57a-C/D)` comment removed.
- [ ] `grep -n 'TODO(57a' kernel/src/task/scheduler.rs` returns no output.
- [ ] `cargo xtask test` passes.

---

## Track C — Option-B Guard-Across-Block Fix

### C.1 — Apply Option-B (or Option-C) to all inventory callsites

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/task/scheduler.rs`
- `kernel/src/ipc/endpoint.rs` (if additional sites found in A.2)

**Symbol:** each `block_current_until` callsite with a live guard
**Why it matters:** Holding an `IrqSafeMutex` guard across a blocking call corrupts `preempt_count` for the wakeup path. This is the documented Bug #9 general fix.

**Acceptance:**
- [ ] Every callsite in the A.2 inventory has been converted: guard dropped before `block_current_until`, data accessed via Arc clone (Option-B) or copied out (Option-C) as appropriate.
- [ ] `session-15` fix for `sys_mmap_file_backed` is confirmed already Option-B compliant (no change needed, just verified).
- [ ] No new `IrqSafeMutex` guard is introduced across a `block_current_until` call in this phase.
- [ ] `cargo xtask check` (clippy -D warnings) passes after all callsite changes.
- [ ] `cargo xtask test` passes after all callsite changes.

---

## Track D — `preempt_count == 0` First-Return Assertion

### D.1 — Add first-return assertion at user-mode return boundary

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs` (or `syscall/mod.rs`) — user-mode return path
- `kernel/src/task/scheduler.rs` — `preempt_count` accessor

**Symbol:** `assert_preempt_count_zero_on_first_return` (new function or inline block)
**Why it matters:** Makes guard leaks self-announcing. Instead of a silent preempt_count imbalance leading to a subtle future race, the kernel asserts on the first user-mode return after a leaked guard, catching regressions at introduction time.

**Acceptance:**
- [ ] A per-CPU `first_user_return_checked: bool` flag (or `AtomicBool`) is initialized to `false` at AP/BSP init.
- [ ] The user-mode return path checks: `if !first_user_return_checked { assert_eq!(preempt_count(), 0); first_user_return_checked = true; }`.
- [ ] A `cargo xtask test` test that calls a syscall and returns to user mode confirms the assertion does not trip (i.e., all Option-B fixes in Track C are correct).
- [ ] Assertion presence confirmed in the hot path with a `#[cfg(debug_assertions)]` guard so release builds omit it (or unconditional if the overhead is acceptable — document the choice).

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

### E.2 — 30-minute PREEMPT_VOLUNTARY soak

**File:** `docs/handoffs/62e-pi-lock-soak.md` (new log artifact)
**Symbol:** QEMU 2-core instance, PREEMPT_VOLUNTARY build
**Why it matters:** Under voluntary preemption, `preempt_count` imbalances from leaked guards produce deferred reschedules that may not surface in short test runs. A 30-minute soak with IPC-heavy workload exercises the race surfaces.

**Acceptance:**
- [ ] `cargo xtask run` with PREEMPT_VOLUNTARY, 2 QEMU cores, running display_server + sshd + sh0 for 30 minutes.
- [ ] Zero kernel panics, WARNINGs, or `preempt_count` assertion trips in serial log.
- [ ] `docs/handoffs/62e-pi-lock-soak.md` populated: QEMU command, duration, observed events, pass/fail verdict.

---

## Track F — Phase 57a and 57b Doc Updates

### F.1 — Flip Phase 57a task-doc Tracks C and D

**File:** `docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md`
**Symbol:** Track C and Track D checkbox items
**Why it matters:** Phase 57a Tracks C (pi_lock routing) and D (with_block_state uniformity) are the source items this phase closes.

**Acceptance:**
- [ ] Track C checkboxes: all four `TODO(57a-C/D)` site items flipped to `[x]` with `scheduler.rs` line citations.
- [ ] Track D checkboxes: Option-B guard fix items flipped to `[x]` with `syscall/mod.rs` and other file citations.
- [ ] Phase 57a design doc `Status:` updated from `Planned` to `Complete` (this is the Track A.3 item from Phase 58 that is now provably closeable after this phase lands).

### F.2 — Update Phase 57b design doc with soak result

**File:** `docs/roadmap/57b-preemption-foundation.md`
**Symbol:** `Status:` field; soak qualifier text
**Why it matters:** Phase 57b's "pending soak (PR #132)" qualifier is stale (PR merged). The soak result was populated by Phase 59 Track G into `docs/handoffs/57b-soak-gate.md`.

**Acceptance:**
- [ ] `Status:` changed to `Complete`.
- [ ] "pending soak (PR #132)" qualifier replaced with one-line soak result note: "30-minute soak completed YYYY-MM-DD — see docs/handoffs/57b-soak-gate.md (pass/fail verdict)."
- [ ] Phase 57e post-mortem (`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`) cross-referenced in Phase 57b's design doc for readers who want the full preemption-model history.

### F.3 — Verify Phase 57a design-doc `Status:` is now `Complete`

**File:** `docs/roadmap/57a-scheduler-rewrite.md`
**Symbol:** `Status:` field
**Why it matters:** Phase 58 Track A.3 flips the Status field; this task confirms Phase 62's track closure is reflected.

**Acceptance:**
- [ ] `Status:` reads `Complete` (changed by Phase 58 Track A.3, confirmed here).
- [ ] A one-line note in the "Acceptance Criteria" section references Phase 62 as the phase that closed Tracks C/D.

---

## Documentation Notes

- The `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` file is the primary design reference for Option-B. Re-read it before starting Track C to understand which callsite shapes map to Option-B vs. Option-C.
- When applying Option-B, always document the pattern in a `// NOTE:` comment adjacent to the Arc clone: `// NOTE: Arc-clone before block — Phase 62 Track C, Bug #9 Option-B. Guard dropped before block_current_until to release IrqSafeMutex and decrement preempt_count.`
- Track A.2 may reveal sites where the guard-across-block is safe because the lock is not an `IrqSafeMutex` (e.g., a plain `Mutex` that does not affect `preempt_count`). Document those as "verified safe — not an IrqSafeMutex" in the inventory rather than converting them.
- The Phase 59 Track G soak result must be complete before Track F.2 can be finished. Phase 62 depends on Phase 59 for this reason.
- The `preempt_count == 0` assertion (Track D) is a debug-mode feature in this phase. If `cargo xtask test` is always run in debug mode, it will always be exercised. Add a comment explaining this assumption.
