# Phase 57a — Scheduler Block/Wake Protocol Rewrite: Task List

**Status:** Planned
**Source Ref:** phase-57a
**Depends on:** Phase 4 ✅, Phase 6 ✅, Phase 35 ✅, Phase 50 ✅, Phase 56 ✅, Phase 57 ✅
**Goal:** Rewrite m3OS's task-blocking primitive to a Linux-style single-state-word + condition-recheck protocol with a per-task spinlock. Delete the `switching_out` / `wake_after_switch` / `PENDING_SWITCH_OUT[core]` machinery that produced the lost-wake bug class catalogued in `docs/handoffs/2026-04-25-scheduler-design-comparison.md` and `docs/handoff/2026-04-28-graphical-stack-startup.md`. Restore the Phase 56/57 graphical stack to a working state on real hardware.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit + transition tables + host tests (TDD foundation) | — | Planned |
| B | Per-task `pi_lock` infrastructure | A | Planned |
| C | New block primitive (`block_current_until`) behind `sched-v2` flag | A, B | Planned |
| D | New wake primitive (`wake_task` CAS rewrite) | C | Planned |
| E | Dispatch handler + field removal | D | Planned |
| F | Migrate all call sites and remove v1 + feature gate | C, D, E | Planned |
| G | Diagnostics: stuck-task watchdog, tracepoint, multiplier fixes | A | Planned |
| H | Secondary bug fixes (serial-stdin, audio_server, syslogd) | F | Planned |
| I | Validation gate (real hardware, soak, fuzz) | F, G, H | Planned |

Tracks A and B are the foundation — they must complete before C/D/E/F. Tracks G and H may run in parallel with C–F. Track I is the final gate before merge.

## Engineering Practice Gates (apply to every track)

These gates are enforced by review and CI; tasks that fail them block the phase.

- **TDD.** Every implementation commit must reference a test commit that landed *earlier* in the same PR (or in a prior PR). Tests added in the same commit as implementation are rejected on review.
- **SOLID.** No new flag fields on `Task` for the block/wake transition; new wait kinds plug in through the existing `block_current_until` primitive. State-mutation helpers do not expose `Task` internals to callers.
- **DRY.** No copy-pasted variant of `block_current_until` for new wait shapes; pass an `Option<u64>` deadline and an `&AtomicBool` condition. No copy-pasted variant of `wake_task` for new wake sources.
- **Documented invariants.** Every state transition in v2 has a one-line invariant comment at the transition site and a matching row in the v2 transition table.
- **Lock ordering.** `pi_lock` is innermost; never acquired while `SCHEDULER.lock()` is held. A debug assertion fails the kernel build's tests on violation.
- **Observability.** Every state transition is reachable from the optional `sched-trace` tracepoint; the watchdog logs any task stuck in `Blocked*` without a registered waker.

---

## Track A — Audit and Test Scaffolding (TDD Foundation)

### A.1 — Catalog every block/wake call site

**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/src/ipc/`
- `kernel/src/arch/x86_64/syscall/mod.rs`
- `kernel/src/main.rs`

**Symbol:** `block_current_unless_woken_inner`, `block_current_unless_woken_until`, `block_current_unless_woken`, any `block_current_unless_woken_with_*` variants, `wake_task`, `scan_expired_wake_deadlines`, all callers
**Why it matters:** Without a complete inventory of call sites, a partial migration leaves a mix of v1 and v2 protocol that produces the same race class. The catalogue is the source of truth for Track F's migration progress.

**Acceptance:**
- [ ] Markdown table at `docs/handoffs/57a-scheduler-rewrite-call-sites.md` listing every callee (function name), every caller (file:line), the kind of block (recv / send / reply / notif / futex / nanosleep), and the wake side responsible for delivering it.
- [ ] Every entry in the table is mapped to a Track F task (F.1, F.2, F.3, or F.4); no orphans.

### A.2 — Build the v1 protocol transition table

**File:** `docs/handoffs/57a-scheduler-rewrite-v1-transitions.md` (artefact)
**Symbol:** —
**Why it matters:** The new protocol must preserve the externally observable behaviour of v1 in the cases that were already correct, and the v1 table is the regression test contract for that preservation.

**Acceptance:**
- [ ] Markdown table with rows = (current state, switching_out, wake_after_switch) tuples, columns = (event: block, wake, scan_expired, dispatch_switch_out), cells = (next state, side effects, invariant).
- [ ] Each cell that exhibits the lost-wake bug is annotated with a citation to the handoff doc that observed it.

### A.3 — Build the v2 protocol transition table

**File:** `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md` (artefact)
**Symbol:** —
**Why it matters:** Defines the new state machine before any kernel code is written. This is the spec.

**Acceptance:**
- [ ] Markdown table with rows = TaskState, columns = (event: block, wake, scan_expired), cells = (next state, side effects, invariant).
- [ ] Every cell has a one-line invariant statement.
- [ ] No cell references `switching_out` or `wake_after_switch`.
- [ ] The v2 table has strictly fewer cells than the v1 table (proof that the protocol shrinks).

### A.4 — Pure-logic state machine model in `kernel-core`

**File:** `kernel-core/src/sched_model.rs` (new)
**Symbol:** `BlockState`, `apply_event`, `Event`
**Why it matters:** Lets the v2 protocol be tested on the host before any kernel code lands, satisfying TDD's red-phase requirement.

**Acceptance:**
- [ ] `BlockState` enum mirrors the v2 `TaskState` blocked variants.
- [ ] `Event` enum: `Block { deadline: Option<u64> }`, `Wake`, `ScanExpired { now: u64 }`, `ConditionTrue`.
- [ ] `apply_event(state, event) -> (new_state, side_effects)` is a pure function with no allocation.
- [ ] Compiles on host (`cargo test -p kernel-core`) and in `no_std` kernel context.
- [ ] One doc comment per state and per transition, explaining the invariant.

### A.5 — Host tests for every v2 transition

**File:** `kernel-core/src/sched_model.rs` (test module)
**Symbol:** `tests::*`
**Why it matters:** TDD red phase — these tests must exist (and most will fail) before any kernel-side implementation begins. The Track A.5 test commit is a prerequisite for any Track C/D/E commit.

**Acceptance:**
- [ ] One test per cell of the v2 transition table; each asserts the new state and side effects.
- [ ] Tests pass against the pure-logic model in A.4.
- [ ] Coverage report shows 100% line and branch coverage of `kernel-core/src/sched_model.rs`.

### A.6 — Property-based fuzz harness

**File:** `kernel-core/tests/sched_property.rs` (new)
**Symbol:** —
**Why it matters:** Hand-written transition tests cover the cases the author thought of; property fuzz catches the rest. The harness is the canonical way to demonstrate "no lost wakes under random interleavings".

**Acceptance:**
- [ ] Property test runs ≥ 10,000 random sequences of `Block` / `Wake` / `ScanExpired` / `ConditionTrue` events.
- [ ] Asserts: every `Block` is eventually followed by a transition out of `Blocked*` if at least one `Wake` or matching `ScanExpired` follows it. (No lost wake.)
- [ ] Asserts: no two consecutive `Wake` events both transition the state. (Idempotent wake.)
- [ ] Asserts: every transition is allowed by the v2 transition table. (No spurious transition.)
- [ ] Hooked into `cargo xtask check` so CI runs it on every build.

### A.7 — Loom-style interleaving harness (stretch goal)

**File:** `kernel-core/tests/sched_loom.rs` (new, optional)
**Symbol:** —
**Why it matters:** Property fuzz catches most races; deterministic interleaving search catches the corner cases. Optional for a first landing; valuable for confidence on the cross-core wake path.

**Acceptance (if included):**
- [ ] Harness exhaustively explores all 2-thread interleavings of (block, wake) with N=4 events; reports any lost-wake configuration.
- [ ] If skipped: documented in the design doc's Deferred section.

---

## Track B — Per-task `pi_lock` Infrastructure

### B.1 — `TaskBlockState` struct

**File:** `kernel/src/task/mod.rs`
**Symbol:** `TaskBlockState`
**Why it matters:** Lifts the protected-by-pi_lock fields into a single struct so the lock guards a clearly defined unit (SOLID — Single Responsibility).

**Acceptance:**
- [ ] `struct TaskBlockState { state: TaskState, wake_deadline: Option<u64> }`.
- [ ] Doc comment on each field: invariant under `pi_lock`.
- [ ] No reader outside `pi_lock` access.

### B.2 — Add `pi_lock` field to `Task`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::pi_lock`
**Why it matters:** The pi_lock is the entire point of the rewrite — protect the state-transition fast path with a per-task lock instead of the global scheduler lock.

**Acceptance:**
- [ ] `Task::pi_lock: Spinlock<TaskBlockState>` field, initialised at task construction with the task's initial `TaskState`.
- [ ] All existing `task.state = ...` writes are migrated under a TODO comment to be replaced in Tracks C/D.
- [ ] Existing `cargo xtask test` passes (no semantic change yet — the lock is acquired but transitions still go through v1 code).

### B.3 — Lock-ordering documentation and debug assertion

**Files:**
- `kernel/src/task/scheduler.rs` (top-of-file doc comment)
- `kernel/src/task/mod.rs::pi_lock`

**Symbol:** `Task::pi_lock`, `SCHEDULER`
**Why it matters:** Lock-ordering bugs are hard to detect at runtime; a debug assertion catches the most common mistake (acquiring pi_lock while holding SCHEDULER.lock).

**Acceptance:**
- [ ] Top-of-file doc block in `scheduler.rs` defines the lock hierarchy: `pi_lock` (innermost) → `SCHEDULER.lock()` (outer). Acquiring inner while holding outer is a panic in debug builds.
- [ ] Debug assertion in `pi_lock.lock()` checks `!SCHEDULER.is_locked_by_current_cpu()` (helper added if necessary; may use a per-CPU `holds_scheduler_lock: AtomicBool`).
- [ ] Host test in `kernel-core` (or a kernel test) exercising the lock-ordering invariant via a controlled sequence; the test panics on violation in debug builds.

### B.4 — Helper API on `Task`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::with_block_state`
**Why it matters:** Centralises the lock-acquire/transition/release pattern so individual call sites do not duplicate the lock dance (DRY).

**Acceptance:**
- [ ] Method `fn with_block_state<R>(&self, f: impl FnOnce(&mut TaskBlockState) -> R) -> R` acquires `pi_lock`, calls `f`, releases, returns the result.
- [ ] Used by every reader/writer of `TaskBlockState` in subsequent tracks.

---

## Track C — New Block Primitive Behind `sched-v2` Flag

### C.1 — Add `sched-v2` Cargo feature

**File:** `kernel/Cargo.toml`
**Symbol:** `[features]` table
**Why it matters:** Lets the v2 primitive coexist with v1 during incremental migration. Migration safety: a single feature-flip rolls back to v1 if a regression appears mid-track.

**Acceptance:**
- [ ] `sched-v2` feature defined; default off.
- [ ] `cargo xtask check` passes with feature both on and off.
- [ ] `cargo xtask test` passes with feature both on and off (v1 still active when off, v2 plumbed but unused when on until C.4 and beyond migrate call sites).

### C.2 — Implement `block_current_until`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `block_current_until`
**Why it matters:** This is the core new primitive — Linux's pattern verbatim, the entire fix for the lost-wake bug class.

**Acceptance:**
- [ ] Signature: `fn block_current_until(woken: &AtomicBool, deadline: Option<u64>) -> BlockOutcome`.
- [ ] Body follows the four-step Linux recipe: state write under `pi_lock` → condition recheck → yield → resume recheck.
- [ ] No reference to `switching_out`, `wake_after_switch`, or `PENDING_SWITCH_OUT`.
- [ ] Doc comment cites the Linux `do_nanosleep` source line and the 2026-04-25 handoff for context.
- [ ] Matching host tests in `kernel-core::sched_model` exist and pass.

### C.3 — Self-revert path

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `block_current_until` (the early-return branch in step 2)
**Why it matters:** Without the self-revert, a wake that arrives between the state write and the yield leaves the task Blocked but Ready-on-condition. This is the precise window the lost-wake bug exploits in v1.

**Acceptance:**
- [ ] If the condition recheck observes the wake before the yield, the function takes `pi_lock`, transitions `state: Blocked* → Running`, clears `wake_deadline`, releases, and returns without yielding.
- [ ] Host test exercising the recheck-true-before-yield case; asserts no yield is observed.
- [ ] Property test (A.6) covers this path.

### C.4 — Migrate IPC call site (single example)

**File:** `kernel/src/ipc/call.rs` (or whichever file invokes `block_current_unless_woken_inner` for `BlockedOnReply`)
**Symbol:** `sys_ipc_call::wait_for_reply` (or equivalent)
**Why it matters:** A first concrete user of the new primitive validates the integration and exposes any API mismatch before mass migration.

**Acceptance:**
- [ ] Under `cfg(feature = "sched-v2")`, the IPC call path uses `block_current_until` instead of `block_current_unless_woken_inner`.
- [ ] Existing IPC integration tests pass with the feature on.
- [ ] No code path under `sched-v2` references `switching_out` or `wake_after_switch`.

---

## Track D — New Wake Primitive

### D.1 — Implement CAS-style `wake_task`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `wake_task` (rewrite, behind `sched-v2`)
**Why it matters:** The wake side must be idempotent against state — a wake to a Running or Ready task is a silent no-op, mirroring Linux semantics.

**Acceptance:**
- [ ] Signature: `fn wake_task(idx: TaskIdx) -> WakeOutcome`.
- [ ] Body: take `pi_lock`; CAS `state` from any `Blocked*` to `Ready`; clear `wake_deadline`; release `pi_lock`; acquire `SCHEDULER.lock`; enqueue.
- [ ] Returns `WakeOutcome::Woken` if the CAS succeeded, `WakeOutcome::AlreadyAwake` otherwise.
- [ ] No reference to `switching_out` or `wake_after_switch`.
- [ ] Doc comment cites Linux's `try_to_wake_up` and the 2026-04-25 handoff.

### D.2 — Cross-core IPI on wake

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `wake_task` (and `send_reschedule_ipi` callsite)
**Why it matters:** A wake to a task on a different core must trigger a reschedule on that core; otherwise the task waits until that core's next timer tick (10 ms latency at 1 kHz, but more importantly it can be missed entirely if the home core is in `enable_and_hlt` and only timer IRQ is delivered).

**Acceptance:**
- [ ] After enqueue, if the woken task's home core is not `current_core_id()`, send a reschedule IPI.
- [ ] Existing IPI mechanism in `kernel/src/smp/mod.rs` reused; no new IPI vector introduced.
- [ ] Host test (model) covering: `Wake` followed by `ScanExpired` on the same task on a different core does not double-enqueue.

### D.3 — Notification path migration

**File:** `kernel/src/ipc/notification.rs` (or whichever file owns `notify_one`/`notify_all`)
**Symbol:** `notify_one`, `notify_all`, `signal_*`
**Why it matters:** All wake paths must use the new CAS primitive; a single missed migration leaves a path that can stomp the new state machine.

**Acceptance:**
- [ ] Under `cfg(feature = "sched-v2")`, all notification wake paths call the new `wake_task`.
- [ ] Notification integration tests pass.

### D.4 — `scan_expired_wake_deadlines` migration

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `scan_expired_wake_deadlines`
**Why it matters:** The deadline scan is one of the wake paths most prone to the lost-wake race in v1. It must use the CAS primitive in v2.

**Acceptance:**
- [ ] Under `cfg(feature = "sched-v2")`, the scan iterates the task table; for each task with `wake_deadline ≤ now` and `state ∈ Blocked*`, calls `wake_task`.
- [ ] No batch-enqueue array; no `wake_after_switch` set.
- [ ] Host test in `kernel-core::sched_model` covering scan-expires-during-block-window race.

---

## Track E — Dispatch Handler and Field Removal

### E.1 — Delete `PENDING_SWITCH_OUT[core]`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `PENDING_SWITCH_OUT`, dispatch switch-out handler (around `kernel/src/task/scheduler.rs:2013-2129` per the handoff)
**Why it matters:** This array and its consumer in the dispatch handler are the v1 deferred-enqueue mechanism. They are dead under v2.

**Acceptance:**
- [ ] Static `PENDING_SWITCH_OUT` removed.
- [ ] Dispatch switch-out handler no longer reads `PENDING_SWITCH_OUT[core]`.
- [ ] At this point `cfg(feature = "sched-v2")` is implicit — all migrating call sites have flipped (Track F.1–F.4 must be complete before E.1 lands; the gate on E.1 is "no remaining v1 callers exist").

### E.2 — Delete `switching_out` field from `Task`

**File:** `kernel/src/task/mod.rs` and all readers
**Symbol:** `Task::switching_out`
**Why it matters:** The flag is the v1 hand-off; it has no role in v2.

**Acceptance:**
- [ ] Field deleted from `Task`.
- [ ] All readers (in scheduler.rs, ipc/, syscall/) removed.
- [ ] `cargo xtask check` clean.

### E.3 — Delete `wake_after_switch` field from `Task`

**File:** `kernel/src/task/mod.rs` and all readers
**Symbol:** `Task::wake_after_switch`
**Why it matters:** The flag is the v1 latched-wake; it has no role in v2 and was the source of the race.

**Acceptance:**
- [ ] Field deleted from `Task`.
- [ ] All readers removed.

### E.4 — Simplify dispatch switch-out handler

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `dispatch_switch_out` (or whatever the post-switch_context handler is named in current source)
**Why it matters:** With the v1 fields gone, the handler reduces to bookkeeping (timeslice accounting, run-queue manipulation, frame counter accounting).

**Acceptance:**
- [ ] Handler body has no `wake_after_switch` consumption, no `switching_out` clear.
- [ ] Existing in-QEMU scheduler tests pass.
- [ ] Doc comment on the handler now reads as a pure bookkeeping function (no state-machine commentary).

---

## Track F — Migrate Remaining Call Sites and Remove Feature Gate

### F.1 — Migrate IPC syscalls (recv, send, reply_recv)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`, `kernel/src/ipc/`
**Symbol:** `sys_ipc_send`, `sys_ipc_recv`, `sys_ipc_reply_recv`
**Why it matters:** These are the highest-traffic block sites; correct migration validates the v2 primitive under realistic load.

**Acceptance:**
- [ ] All three syscalls block via `block_current_until`.
- [ ] IPC banner-exchange test (`cargo xtask test`) passes.
- [ ] No reference to v1 functions in any IPC code path.

### F.2 — Migrate notification syscalls

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_notif_wait`, `sys_notif_wait_timeout`
**Why it matters:** Notification waiters are the second-highest-traffic block site.

**Acceptance:**
- [ ] Both syscalls block via `block_current_until`.
- [ ] Notification integration tests pass.

### F.3 — Migrate futex syscalls

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbol:** `sys_futex_wait`, `sys_futex_wait_until`, `sys_futex_wake`
**Why it matters:** Futex is the lowest-level userspace synchronization primitive; its correctness is the foundation for libstd-equivalent locking.

**Acceptance:**
- [ ] Wait-side syscalls block via `block_current_until`.
- [ ] Wake-side calls `wake_task` directly (no intermediate v1 path).
- [ ] Futex test suite passes.

### F.4 — Migrate `sys_nanosleep` (≥ 1 ms branch)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs:3162-3232`
**Symbol:** `sys_nanosleep`
**Why it matters:** The current `< 5 ms` busy-spin branch saturates a core when a userspace daemon does `nanosleep(0, 1_000_000)`. Migrating sleeps ≥ 1 ms to `block_current_until` with a deadline is the obvious win.

**Acceptance:**
- [ ] Sleeps ≥ 1 ms use `block_current_until(deadline = now + sleep_ns)`.
- [ ] Sleeps < 1 ms retain the TSC busy-spin (cost of context switch is higher than the sleep).
- [ ] `cargo xtask run-gui --fresh` shows `userspace/syslogd` and `userspace/display_server` no longer cpu-hogging at 100% on their cores during idle.

### F.5 — Delete v1 functions and `sched-v2` feature gate

**Files:**
- `kernel/src/task/scheduler.rs`
- `kernel/Cargo.toml`

**Symbol:** `block_current_unless_woken_inner`, `block_current_unless_woken_until`, `block_current_unless_woken`, `block_current_unless_woken_with_recv`
**Why it matters:** Cleanup. The v1 protocol is the bug; leaving it as dead code invites accidental re-use.

**Acceptance:**
- [ ] All four v1 functions deleted.
- [ ] `sched-v2` feature deleted from `Cargo.toml`.
- [ ] `cargo xtask check` clean.
- [ ] `git grep` shows no references to `switching_out`, `wake_after_switch`, or `PENDING_SWITCH_OUT` anywhere in the repository.

---

## Track G — Diagnostic Infrastructure

### G.1 — Stuck-task watchdog

**File:** `kernel/src/task/scheduler.rs` (new function `watchdog_scan`)
**Symbol:** `watchdog_scan`
**Why it matters:** The 2026-04-25 doc identifies a periodic state dump as "the missing tool that would have shortened this PR's debugging by hours." A watchdog makes future Blocked-forever bugs immediately visible.

**Acceptance:**
- [ ] Every N seconds (default 10), iterate task table; for any task in `Blocked*` for more than M seconds (default 30) with no wake_deadline registered (i.e. nothing will eventually wake it), log `[WARN] [sched] task pid=X state=Y stuck-since=Zms`.
- [ ] Configurable via `kernel/src/log/mod.rs` log filter.
- [ ] Integration test: a deliberately Blocked task with no waker triggers the warning within M+N seconds.

### G.2 — Optional state-transition tracepoint

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** trace points inside `block_current_until` and `wake_task`
**Why it matters:** When a future bug requires deep diagnosis, a record of every state transition with caller and timestamp is invaluable. Reuses Phase 43b's per-core trace ring.

**Acceptance:**
- [ ] Under `cfg(feature = "sched-trace")`, every state transition emits a structured trace-ring entry (pid, old state, new state, caller via `core::panic::Location` or similar, tick).
- [ ] Default off (no overhead in default build).
- [ ] Manual smoke: enable feature, reproduce a wake, dump the trace ring, see the transition entries.

### G.3 — Fix `cpu-hog` log 10× multiplier bug

**File:** `kernel/src/task/scheduler.rs:2191`
**Symbol:** the `ran_ticks * 10` expression in the cpu-hog log message
**Why it matters:** All `[WARN] [sched] cpu-hog: pid=X ran~Yms` values are 10× the truth because the formula assumes 100 Hz timer but `TICKS_PER_SEC = 1000`. Active misinformation; misled the 2026-04-28 investigation.

**Acceptance:**
- [ ] `ran_ticks * 10` replaced with `ran_ticks` (since `TICKS_PER_SEC = 1000` makes 1 tick = 1 ms).
- [ ] One regression test demonstrating the corrected value.

### G.4 — Fix `sys_poll` 10× multiplier bug

**File:** `kernel/src/arch/x86_64/syscall/mod.rs:14647`
**Symbol:** `sys_poll` (the `(timeout_i as u64).div_ceil(10)` expression)
**Why it matters:** `poll(2000)` currently times out at 200 ms because the conversion divides by 10 assuming 100 Hz. Every `poll`-using userspace daemon observes 1/10 the configured timeout — a silent correctness bug.

**Acceptance:**
- [ ] `(timeout_i as u64).div_ceil(10)` replaced with `(timeout_i as u64)`.
- [ ] Userspace test: `poll(fd, 2000)` returns after ~2000 ms ± 50 ms.

---

## Track H — Secondary Bug Fixes

### H.1 — Migrate `serial_stdin_feeder_task` to notification-based wait

**File:** `kernel/src/main.rs:486`
**Symbol:** `serial_stdin_feeder_task`
**Why it matters:** The current `enable_and_hlt` halt-loop parks the feeder's host core's scheduler indefinitely between IRQs. Migrating to a notification-based wait (mirroring `net_task` at `kernel/src/main.rs:598`) lets the scheduler dispatch other tasks on that core. This is the bug behind the `kbd_server` dead-on-core-3 baseline at `3729a69`.

**Acceptance:**
- [ ] Feeder blocks on a `Notification` signalled from the COM1 RX IRQ handler via the existing `IsrWakeQueue` infrastructure.
- [ ] No `enable_and_hlt` in user-mode loop.
- [ ] `kbd_server` (which previously parked behind the feeder when both landed on AP3) reaches its main loop on the same boot.
- [ ] Manual smoke: `cargo xtask run-gui --fresh` with feeder-on-AP3 placement still reproducible (e.g. by stress test) — kbd input now works.

### H.2 — Register stub `audio.cmd` in `audio_server` with no AC'97

**File:** `userspace/audio_server/src/main.rs:67`
**Symbol:** `main` (the early-exit branch when no `0x8086:0x2415` device is found)
**Why it matters:** `session_manager` treats `audio.cmd` registration as a required boot step. Without the stub, hardware-less boots always text-fallback even though the rest of the graphical stack works (Phase 57 acceptance regression).

**Acceptance:**
- [ ] When no AC'97 hardware is detected, `audio_server` registers `audio.cmd` and serves a no-op `play` reply (silent — discards PCM data).
- [ ] `session_manager` does not text-fallback in `cargo xtask run-gui` (which lacks AC'97 in QEMU's default config).
- [ ] Phase 57 audio integration test still passes when AC'97 *is* present (the stub branch is gated on hardware-absence).

### H.3 — Investigate and fix `syslogd` cpu-hog

**File:** `userspace/syslogd/src/main.rs:141-216`
**Symbol:** `main_loop`, `drain_kmsg`
**Why it matters:** `syslogd` cpu-hogs core 1 for ~500 ms at a stretch even though it uses `poll`. Either the `sys_poll` 10× bug (fixed in G.4) is the root cause, or `drain_kmsg` is doing very long uninterrupted work.

**Acceptance:**
- [ ] Root cause identified and documented in the PR description.
- [ ] After fix, `syslogd` consumes < 5% CPU during idle (1 minute observation, no incoming kmsg).
- [ ] If the root cause is `drain_kmsg` work, fix splits the drain into smaller chunks with `yield_now` between chunks.

---

## Track I — Validation Gate

### I.1 — Real-hardware graphical stack regression test

**File:** procedural; results go in PR description and a permanent record at `docs/handoffs/57a-validation-gate.md`
**Symbol:** —
**Why it matters:** The 2026-04-28 cursor-at-(0,0) regression is the user-facing acceptance test. It must not reproduce.

**Acceptance:**
- [ ] On the user's test hardware, `cargo xtask run-gui --fresh`: cursor moves on mouse motion within 1 s of motion start; keyboard input typed in the framebuffer terminal appears within 100 ms; term reaches `TERM_SMOKE:ready`; no `[WARN] [sched]` stuck-task lines.
- [ ] Repeated 5 times, 5 successes (placement varies between boots; the test must succeed regardless of which AP cores serve display_server / mouse_server / kbd_server).

### I.2 — SSH disconnect/reconnect soak

**File:** procedural; can be scripted via `userspace/sshd` test fixture or external client
**Symbol:** —
**Why it matters:** The 2026-04-25 SSH cleanup hang is the second user-facing acceptance test.

**Acceptance:**
- [ ] 50 consecutive SSH disconnect/reconnect cycles in one session with no scheduler hang and no `[WARN] [sched]` stuck-task lines.

### I.3 — Multi-core in-QEMU fuzz

**File:** new `kernel/tests/sched_fuzz.rs` (in-QEMU integration test)
**Symbol:** —
**Why it matters:** Property-based host tests exercise the model; a real in-QEMU fuzz test on 4 cores exercises the actual primitive under cross-core IPC pressure.

**Acceptance:**
- [ ] 4-core QEMU run, 5 minutes, alternating bursts of IPC + futex + notification across cores. No hang, no panic, no stuck-task warning, no `cpu-hog` warning whose corrected `ran` value exceeds 200 ms.

### I.4 — Long-soak (idle + load)

**File:** procedural; documented run on the test hardware
**Symbol:** —
**Why it matters:** Lost-wake bugs are by definition timing-dependent. A 60-minute soak gives confidence that the bug is not just shifted to a longer window.

**Acceptance:**
- [ ] 30 minutes idle + 30 minutes synthetic load, 4 cores, no hang, no panic, no stuck-task warning, no `cpu-hog` warning whose corrected `ran` value exceeds 200 ms.

### I.5 — Documentation update

**Files:**
- `docs/roadmap/README.md` (add Phase 57a row, update mermaid graph and gantt)
- `docs/04-tasking.md` and/or `docs/06-ipc.md` (update narrative to describe the v2 protocol; remove references to `switching_out`/`wake_after_switch`)
- `kernel/Cargo.toml` (bump version to `0.57.1`)
- `kernel/src/main.rs` (banner version)
- Top-of-file doc in `kernel/src/task/scheduler.rs` (lock-ordering hierarchy from B.3, plus a v2 protocol overview citing this phase)

**Symbol:** —
**Why it matters:** The phase is not done until the documentation describes the new protocol and the kernel version reflects the change. Future agents must be able to read `docs/04-tasking.md` and learn the v2 protocol, not the v1 one.

**Acceptance:**
- [ ] All listed files updated; `cargo xtask check` clean after version bump.
- [ ] No stale reference to v1 protocol terms in `docs/`.
- [ ] Phase 57a row added to `docs/roadmap/README.md`'s milestone summary table with status `Complete`.

---

## Documentation Notes

- This phase replaces the block/wake protocol introduced incrementally across Phases 4, 6, 35, and 50. The relevant phase docs are not edited (they remain a snapshot of the original design); instead, `docs/04-tasking.md` and `docs/06-ipc.md` are updated to describe the v2 protocol as the current state, with a footnote pointing to Phase 57a as the rewrite.
- Adds `kernel-core/src/sched_model.rs` as the first host-testable scheduler model. Future scheduler changes should extend this model rather than add new flags directly.
- The transition tables produced in Track A (v1 and v2) live at `docs/handoffs/57a-scheduler-rewrite-v1-transitions.md` and `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md`. They serve as both the test contract and the durable reference for any future work in this area.
- The two source handoff docs (`docs/handoffs/2026-04-25-scheduler-design-comparison.md` and `docs/handoff/2026-04-28-graphical-stack-startup.md`) are the input specification for this phase; they are not modified.
- Engineering practice gates (top of this file) are enforced by review and CI; tasks that fail them block the phase from closing.
- Lock ordering (recapped from B.3): `pi_lock` is innermost; `SCHEDULER.lock()` is outer. Acquiring inner while holding outer is a panic in debug builds.
