# Phase 57a — Scheduler Block/Wake Protocol Rewrite

**Status:** Planned
**Source Ref:** phase-57a
**Depends on:** Phase 4 (Tasking) ✅, Phase 6 (IPC Core) ✅, Phase 35 (True SMP) ✅, Phase 50 (IPC Completion) ✅, Phase 56 (Display and Input Architecture) ✅, Phase 57 (Audio and Local Session) ✅
**Builds on:** Replaces the multi-flag block/wake protocol (`switching_out`, `wake_after_switch`, `PENDING_SWITCH_OUT[core]`) introduced incrementally through Phases 4, 6, 35, and 50. Eliminates the lost-wake bug class catalogued in `docs/handoffs/2026-04-25-scheduler-design-comparison.md` and re-confirmed by the `feat/phase-57-impl` graphical-stack regression in `docs/handoff/2026-04-28-graphical-stack-startup.md`.
**Primary Components:** `kernel/src/task/scheduler.rs` (block/wake primitives, dispatch switch-out handler, deadline scanner), `kernel-core/src/sched_model.rs` (new pure-logic state machine + host tests), `kernel/src/ipc/` (call/recv/reply rendezvous), `kernel/src/arch/x86_64/syscall/mod.rs` (syscall block sites), `kernel/src/main.rs::serial_stdin_feeder_task` (notification-based wait migration), `userspace/audio_server`, `userspace/syslogd` (secondary fixes carried in this phase).

## Milestone Goal

m3OS's task-blocking primitive becomes race-free without latched per-task flags. `task.state` is the single source of truth for whether a task is blocked, wakes are idempotent CAS operations against that state, and a per-task spinlock (mirroring Linux's `p->pi_lock`) holds each block/wake transition atomic. The graphical stack catalogued in the 2026-04-28 handoff boots reliably on real hardware regardless of which AP cores the load balancer chooses, and the SSH disconnect hang catalogued in the 2026-04-25 handoff stops reproducing.

## Why This Phase Exists

Two consecutive debug sessions traced unrelated user-visible failures back to the same scheduler bug class:

- **2026-04-25.** `sshd` cleanup hangs in a `nanosleep` loop after the client disconnects. `block_current_unless_woken_until` enters Blocked state, the wake side fires, but `wake_after_switch` was already latched true from a prior asymmetric scan/wake interaction. The dispatch switch-out handler consumes the flag and enqueues the task before its deadline — the cleanup path treats it as a real wake, re-blocks immediately, and never wakes again.
- **2026-04-28.** `display_server` reaches `BlockedOnReply` waiting for `mouse_server`'s `MOUSE_EVENT_PULL` reply. When `display_server` lands on an AP under cross-core IPC pressure, the wake side's reply hits the `switching_out` window and is deferred to `wake_after_switch`. Under specific timing, the deferred enqueue is lost and `display_server` is stuck Blocked forever. Visible: cursor frozen at (0, 0), no terminal surface, no input.

The 2026-04-28 handoff catalogues eight remediation attempts that all adjusted *placement* of tasks (`parks_scheduler` flag, idle pinning, yield insertion, `least_loaded_core` tweaks). Every attempt failed because the bug is not a placement issue — it is a primitive-level race in the block/wake state machine. The 2026-04-25 doc proposes the fix: collapse the protocol to a single state word with condition recheck (Linux's `try_to_wake_up` pattern), backed by a per-task spinlock (Linux's `p->pi_lock`). This phase implements that proposal.

The phase has two goals, ranked:

1. **Primary — scheduler rewrite.** Eliminate the bug class structurally by rewriting the protocol. Patches that target *symptoms* of the bug have been tried and failed; the protocol itself must change.
2. **Secondary — bug elimination.** Restore the Phase 56/57 graphical stack to a working state and close the related correctness gaps surfaced during the 2026-04-28 investigation (`serial_stdin_feeder` halt-loop parking core 3, `audio_server` exiting without registering `audio.cmd`, the `sys_poll` and `cpu-hog` 10× multiplier bugs, the `syslogd` cpu-hog).

## Learning Goals

- How Linux's `try_to_wake_up` makes wakes idempotent against task state with a single state-match CAS, and why that eliminates lost-wake races without a switching-out handshake.
- Why a single state word plus barrier-ordered condition recheck after the state write is sufficient to close the lost-wake window without latched flags spanning block calls.
- How a per-task spinlock (Linux `p->pi_lock`) reduces contention on a global scheduler lock for the block/wake fast path while preserving lock-ordering invariants for run-queue manipulation.
- Why state-machine bugs in concurrent code surface as "no fault, no signal, no panic, no warning" symptoms, and how property-based testing of state transitions exposes the race windows that integration tests miss.
- How to migrate a kernel scheduling primitive incrementally behind a feature flag, with a transition table as the test contract — and why a flag-based migration is preferable to a big-bang rewrite for primitives this central.

## Feature Scope

### Block primitive rewrite

Collapse the four variants of `block_current_unless_woken*` into a single primitive whose contract is:

1. Acquire per-task spinlock; write `state = Blocked*`; record optional `wake_deadline`; release lock.
2. Recheck the wake condition (a `&AtomicBool`, deadline, or both). If already true, self-revert state under the lock and return.
3. Yield to the scheduler. The dispatcher sees `state ∈ Blocked*` and does not redispatch the task.
4. On resume, recheck the condition once more (defence against spurious wake) and return.

No `switching_out` flag, no `wake_after_switch` flag, no `PENDING_SWITCH_OUT[core]` deferred enqueue. The task never reads its own `state` to make blocking decisions — it reads the *condition* (`woken`, deadline) instead, mirroring Linux's separation of state and condition.

### Wake primitive rewrite

Replace the conditional `wake_task` (which branches on `switching_out`) with a CAS-style wake:

1. Acquire per-task spinlock.
2. If `state ∈ {BlockedOnRecv, BlockedOnSend, BlockedOnReply, BlockedOnNotif, BlockedOnFutex}`, set `state = Ready` and clear `wake_deadline`. Otherwise no-op (silent drop, mirroring Linux semantics).
3. Release per-task lock; acquire scheduler lock; if the target is already in a run queue, no-op (idempotent against scheduler-driven re-runs); else if `target.on_cpu == true`, spin-wait (`smp_cond_load_acquire`-style) until it becomes false (the outgoing context's `saved_rsp` must be durably published before another core can dispatch it); then enqueue on the home core and, if home core ≠ current core, send a reschedule IPI.

A wake to a Running or already-Ready task is a no-op. Spurious wakes become harmless dead-letters automatically. The `on_cpu` spin-wait replaces the v1 `PENDING_SWITCH_OUT[core]` RSP-publication marker (see "Dispatch switch-out handler simplification and `on_cpu` marker" below).

### Per-task spinlock (`pi_lock`) and state ownership model

Each `Task` gains a `Spinlock<TaskBlockState>` named `pi_lock` that guards the *canonical* block state. The v2 design splits what v1 calls `task.state` into two distinct concepts:

- **Canonical block state** (`TaskBlockState.state`, `TaskBlockState.wake_deadline`) — guarded by `pi_lock`. Source of truth for "is this task logically blocked, and on what deadline".
- **Runnable / dispatchable state** (run-queue membership, `Task::on_cpu`) — guarded by `SCHEDULER.lock`. Source of truth for "should the scheduler dispatch this task" and "is the task currently saving context on a CPU".

This split is the SOLID Single-Responsibility decomposition that makes the lock ordering work: the scheduler-side reads (`pick_next`, dispatch, cleanup, the deadline scan) consult run-queue membership and `on_cpu`, never `pi_lock`-protected fields. The wake / block primitives are responsible for keeping the two views consistent.

**Lock-ordering rule (v2).** `pi_lock` is *outer*; `SCHEDULER.lock()` is *inner*. A code path may hold `pi_lock` while acquiring `SCHEDULER.lock`; the reverse — taking `pi_lock` while `SCHEDULER.lock` is already held — is forbidden. A debug assertion fires on violation.

This matches Linux's `p->pi_lock` (outer) → `rq->lock` (inner) pattern, adapted to a single global scheduler lock.

### Wake deadline tracking simplification

`ACTIVE_WAKE_DEADLINES` becomes a debug-only counter; the scan path iterates the task table directly under the scheduler lock and uses the new wake primitive (CAS) to transition expired tasks. No batch-enqueue path that bypasses the CAS, no `wake_after_switch=true` set during the scan.

### Dispatch switch-out handler simplification and `on_cpu` marker

`PENDING_SWITCH_OUT[core]` in v1 is *dual-purpose*: it is both the deferred-wake hand-off (the wake-side concern this phase removes) *and* an RSP-publication marker (the dispatch-side concern: it signals that `switch_context` is still saving the outgoing task's stack pointer; see `kernel/src/task/scheduler.rs:880-881` doc comment, `:954-966` set sites, and the `pick_next` consumers at `:351-352, :371-372, :2098`). The wake-deferral semantics are deleted in v2; the RSP-publication semantic is preserved as a renamed, single-purpose field — `Task::on_cpu: AtomicBool` — that the v2 wake primitive checks before allowing cross-core enqueue.

Flow:

1. **Block-side.** Under `pi_lock`, write `state = Blocked*`. Release `pi_lock`. Set `on_cpu = true` (Release ordering). Call `switch_context` (saves RSP into `Task::saved_rsp`). The arch-level switch-out epilogue clears `on_cpu = false` (Release) *after* `saved_rsp` is durably published.
2. **Wake-side.** Under `pi_lock`, CAS `state` to `Ready`. Release `pi_lock`. Take `SCHEDULER.lock`. If the target task is already in a run queue, no-op. Else if `on_cpu == true`, spin-wait (`smp_cond_load_acquire`-style) on `on_cpu` becoming false. Then enqueue + IPI.

This mirrors Linux's `try_to_wake_up` behaviour with `p->on_cpu`: a wake on a task that has not yet finished publishing its saved RSP must wait for that publication, or the dispatched task will resume from a stale RSP — the same hazard `pick_next` currently guards against with the `!switching_out && saved_rsp != 0` check.

`wake_after_switch` and the wake-deferral semantics of `PENDING_SWITCH_OUT[core]` are deleted entirely. The RSP-publication aspect is replaced by `Task::on_cpu` with a `smp_cond_load_acquire` pattern on the wake side. The v1 `pick_next` guards on `!switching_out && saved_rsp != 0` become guards on `!on_cpu && saved_rsp != 0` — same hazard, single-purpose field.

The dispatch handler itself reduces to timeslice accounting and run-queue manipulation; it no longer mutates state-flags.

### Affected call sites — full migration

- **IPC syscalls.** `sys_ipc_send`, `sys_ipc_recv`, `sys_ipc_call`, `sys_ipc_reply_recv`.
- **Notification syscalls.** `sys_notif_wait`, `sys_notif_wait_timeout`.
- **Futex syscalls.** `sys_futex_wait`, `sys_futex_wait_until`, `sys_futex_wake`.
- **I/O multiplexing syscalls.** `sys_poll`, `sys_select` / `select_inner`, `sys_epoll_wait` (each currently embeds a 100 Hz tick assumption; see Diagnostic infrastructure below).
- **Sleep syscalls.** `sys_nanosleep` ≥ 1 ms branch (the < 1 ms branch retains its TSC busy-spin since the cost of context switch exceeds the sleep).
- **Kernel-internal waits.** `net_task` at `kernel/src/main.rs:648` (calls `block_current_unless_woken(&NIC_WOKEN)` directly), `WaitQueue::sleep` at `kernel/src/task/wait_queue.rs:56` (the generic kernel wait-queue primitive used by other subsystems), and `serial_stdin_feeder_task` at `kernel/src/main.rs:486` (Track H.1 migrates this from `enable_and_hlt` to notification-based wait, which then bottoms out in `block_current_until`). Track A.1 produces the exhaustive inventory; Track F.6 migrates these callers.

### Diagnostic infrastructure

- Periodic `[WARN] [sched]` watchdog dump for any task in `Blocked*` for more than M seconds with no wake_deadline registered.
- Optional state-transition tracepoint gated on `--features sched-trace`, recording every block/wake with caller and timestamp (default off, no overhead).
- **Sweep stale 100 Hz tick-multiplier assumptions.** Multiple sites convert ticks ↔ milliseconds with a `× 10` or `÷ 10` factor that assumes `TICKS_PER_SEC = 100`, but the actual rate is 1000 (1 tick = 1 ms). Every such site is corrected:
  - `kernel/src/task/scheduler.rs:1892` — `stale-ready` log message: `stale_ticks * 10` → `stale_ticks`.
  - `kernel/src/task/scheduler.rs:2191` — `cpu-hog` log message: `ran_ticks * 10` → `ran_ticks`.
  - `kernel/src/arch/x86_64/syscall/mod.rs:14647` — `sys_poll`: `(timeout_i as u64).div_ceil(10)` → `(timeout_i as u64)`.
  - `kernel/src/arch/x86_64/syscall/mod.rs:14894` — `select_inner`: `ms.div_ceil(10)` → `ms`.
  - `kernel/src/arch/x86_64/syscall/mod.rs:15304` — `sys_epoll_wait`: `(timeout_i as u64).div_ceil(10)` → `(timeout_i as u64)`.
  After the sweep, every reported timeout and duration matches wall-clock observation.

### Secondary bug fixes (carried in this phase)

These bugs were discovered during the 2026-04-28 investigation. They are not the scheduler rewrite, but they ride alongside it because they share root cause (the cpu-hog and poll bugs) or block the validation gate (the others):

1. `serial_stdin_feeder_task` notification-based wait migration — eliminates the halt-loop that parks its host core's scheduler.
2. `audio_server` registers a stub `audio.cmd` even when no AC'97 hardware is present — prevents `session_manager` text-fallback on hardware-less boots.
3. `syslogd` cpu-hog investigation and fix — root-cause the ~500 ms uninterrupted-CPU windows.

## Engineering Practice Requirements

This phase is intentionally a teaching opportunity for disciplined work on a concurrent kernel primitive. The following practices are enforced as part of acceptance:

- **Test-Driven Development (TDD).** Track A defines the v2 transition table and host tests in `kernel-core/src/sched_model.rs` *before* any kernel implementation lands. Every later code change has a corresponding test that was added before the implementation; the PR commit history shows test-first ordering. This is mechanically enforced by reviewer attention to the per-track ordering laid out in the companion task list.
- **SOLID.**
  - *Single Responsibility.* `pi_lock` protects state transitions only; the scheduler lock protects run-queue manipulation only; no field is shared between the two responsibilities.
  - *Open/Closed.* New wait kinds (signalfd, eventfd, future epoll variants) plug in via the unified `block_current_until` primitive without scheduler changes.
  - *Liskov.* All callers of the v2 primitive observe identical block/wake semantics, regardless of which deadline / condition shape they pass.
  - *Interface Segregation.* The block primitive does not expose internal state-flag manipulation; callers see a deadline and a condition `&AtomicBool`, nothing else.
  - *Dependency Inversion.* Subsystems depend on `block_current_until` and `wake_task`, not on direct `Task` field access.
- **DRY.** A single block primitive replaces four variants; a single wake primitive replaces three wake paths. Test helpers for state-machine assertions are factored out of `kernel-core::sched_model` and reused across all transition tests.
- **Documented invariants.** Every state transition in the v2 protocol carries a one-line invariant statement in code (doc comment) and in the v2 transition table. Reviewers reject changes that mutate state without a corresponding invariant update.
- **Lock-ordering hierarchy.** Defined in a top-of-file doc block in `scheduler.rs`: `pi_lock` is *outer*, `SCHEDULER.lock()` is *inner* (Linux's `p->pi_lock` → `rq->lock` pattern adapted to a single global scheduler lock). A code path may hold `pi_lock` while acquiring `SCHEDULER.lock`; the reverse is forbidden. A debug assertion catches violations at runtime; host tests confirm the invariant on every transition path.
- **Migration safety.** The `sched-v2` Cargo feature gate keeps v1 and v2 coexisting until every call site is migrated; rollback is a single feature-flip until Track F.5 deletes v1 entirely.
- **Observability.** Every state transition is reachable from a tracepoint when `--features sched-trace` is enabled; a periodic watchdog logs any task stuck in `Blocked*` without a registered waker.

## Important Components and How They Work

### `TaskBlockState` (replaces flag tuple)

A small struct lifted out of `Task` and protected by `pi_lock`:

```rust
struct TaskBlockState {
    state: TaskState,
    wake_deadline: Option<u64>,
}
```

`switching_out: bool` and `wake_after_switch: bool` are deleted. `state` is the single source of truth.

### `pi_lock` (per-task spinlock)

```rust
struct Task {
    // ... existing fields ...
    pi_lock: Spinlock<TaskBlockState>,
}
```

Always acquired for state mutation. Never nested inside `SCHEDULER.lock()`. Replaces the protected-by-global-lock invariant for the block/wake transition with a per-task locking discipline that mirrors Linux's `p->pi_lock`. It is the first per-task lock m3OS introduces; previous phases protected per-task fields with the global scheduler lock.

### Block primitive (`block_current_until`)

Located at `kernel/src/task/scheduler.rs::block_current_until` (renamed from `block_current_unless_woken_inner` and its three siblings; the four variants collapse to one). Signature:

```rust
fn block_current_until(woken: &AtomicBool, deadline_ticks: Option<u64>) -> BlockOutcome;
```

`deadline_ticks` is an *absolute* deadline expressed in scheduler ticks. With `TICKS_PER_SEC = 1000`, one tick equals one millisecond. Callers convert their native units (`Duration`, `timespec`, TSC) to a tick deadline at the boundary; the primitive itself never sees nanoseconds. For `sys_nanosleep`, the conversion is `now_ticks + sleep_ns.div_ceil(1_000_000)`.

The implementation follows Linux's `do_nanosleep` recipe: state write under `pi_lock` → release `pi_lock` → condition recheck → yield (which goes through `SCHEDULER.lock`) → resume recheck. It is the entire fix for the lost-wake bug class.

### Wake primitive (`wake_task`, rewrite)

Located at `kernel/src/task/scheduler.rs::wake_task`. CAS over canonical `state` under `pi_lock`. After releasing `pi_lock`, takes `SCHEDULER.lock` and (idempotency) checks run-queue membership; (RSP-publication safety) spin-waits on `Task::on_cpu` becoming false; then enqueues and optionally sends a reschedule IPI. Returns whether the wake landed (so callers know whether to send the IPI). A wake to a Running or already-Ready task is a no-op — spurious wakes become harmless.

### Dispatch switch-out handler

Located at `kernel/src/task/scheduler.rs::dispatch_switch_out`. Strips the v1 `PENDING_SWITCH_OUT[core]` and `wake_after_switch` clears. Becomes a pure bookkeeping function (timeslice accounting, run-queue manipulation, frame counter accounting). The arch-level switch-out epilogue (separate from this handler) clears `Task::on_cpu = false` after `saved_rsp` is durably published — this is the only state mutation left on the switch-out path.

### `scan_expired_wake_deadlines` (rewrite)

Located at `kernel/src/task/scheduler.rs::scan_expired_wake_deadlines`. Iterates the task table; for any task with `wake_deadline ≤ now` and `state ∈ Blocked*`, calls the new `wake_task`. No batch-enqueue, no `wake_after_switch` set.

### `serial_stdin_feeder_task` (migration)

Located at `kernel/src/main.rs::serial_stdin_feeder_task`. Replaces the `enable_and_hlt` halt-loop with a notification-based wait, mirroring how `net_task` waits on the IRQ wakeup queue at `kernel/src/main.rs:598`. The COM1 RX IRQ delivers a notification to the feeder; the feeder blocks on the notification (using the new primitive) and so its core's scheduler is never parked.

### Pure-logic model in `kernel-core`

Added: `kernel-core/src/sched_model.rs`. A pure-logic mirror of the new block/wake state machine, exercised by host tests. The state machine becomes a `#[derive(Debug, PartialEq)]` enum; transitions are pure functions; tests assert no transition produces a Blocked-with-no-waker configuration. Future scheduler changes extend this model rather than adding new flags directly.

## How This Builds on Earlier Phases

- **Extends Phase 4 (Tasking)** by replacing the block/wake protocol while preserving the `Task` struct shape and the `TaskState` enum's external semantics.
- **Replaces the protocol introduced in Phase 6 (IPC Core)** for synchronous rendezvous (`block_current_unless_woken_inner`) — the function names and signatures change but the IPC syscall *contract* does not.
- **Reuses Phase 35 (True SMP)** cross-core wake delivery (reschedule IPI, `IsrWakeQueue`) but eliminates the `switching_out`/`wake_after_switch` handshake that gated cross-core enqueue.
- **Replaces the `ACTIVE_WAKE_DEADLINES` counter from Phase 50 (IPC Completion)** with a state-derived rescan.
- **Reuses Phase 43c (Regression and Stress)** infrastructure for the multi-core fuzz and long-soak validation gates.
- **Reuses Phase 43b (Kernel Trace Ring)** as the backing for the optional `sched-trace` tracepoint.
- **Closes the Phase 57 graphical-stack regression** that surfaced when display_server, mouse_server, and term were placed across multiple APs under boot fork pressure.

## Implementation Outline

The rewrite is split across nine tracks. Tracks A and B are the foundation (must complete first). Tracks C–F migrate behaviour behind a feature flag and remove v1. Tracks G–H are diagnostics and secondary bug fixes that can run in parallel with C–F. Track I is the final validation gate.

1. **Track A — Audit and Test Scaffolding (TDD foundation).** Build the v1 and v2 transition tables, write host tests in `kernel-core` for every transition, BEFORE any kernel change. Property-based fuzz for state-machine invariants.
2. **Track B — Per-task `pi_lock` infrastructure.** Add `pi_lock` to `Task`, `TaskBlockState` struct, helper functions, lock-ordering doc and `debug_assert!` guards.
3. **Track C — New block primitive (behind `sched-v2` flag).** Implement `block_current_until` alongside the existing four variants; gate per-call-site migration on `cfg(feature = "sched-v2")`.
4. **Track D — New wake primitive.** Implement CAS-style `wake_task`; convert `notify_one`/`notify_all`/`signal_*`/`scan_expired` paths.
5. **Track E — Dispatch handler + `on_cpu` marker + field removal.** Add `Task::on_cpu` and switch-out epilogue clear (replaces the RSP-publication aspect of `PENDING_SWITCH_OUT`); delete `PENDING_SWITCH_OUT`, `wake_after_switch`, `switching_out`. Update `pick_next` guards from `!switching_out && saved_rsp != 0` to `!on_cpu && saved_rsp != 0`.
6. **Track F — Migrate remaining call sites and remove v1.** Convert IPC, notification, futex, I/O multiplexing (poll/select/epoll), nanosleep, and kernel-internal (`net_task`, `WaitQueue::sleep`, etc.) call sites; delete v1 functions and the `sched-v2` gate.
7. **Track G — Diagnostic infrastructure.** Stuck-task watchdog, optional tracepoint, full sweep of 100 Hz tick-multiplier assumptions across scheduler logs and I/O-multiplexing syscalls.
8. **Track H — Secondary bug fixes.** `serial_stdin_feeder` notification migration, `audio_server` stub registration, `syslogd` cpu-hog investigation.
9. **Track I — Validation.** Real-hardware graphical stack regression test; SSH disconnect/reconnect soak; multi-core fuzz; long-soak.

## Acceptance Criteria

### Primary (scheduler rewrite)

- All `block_current_unless_woken*` callers (syscalls *and* kernel-internal callers `net_task`, `WaitQueue::sleep`, etc.) use the v2 primitive. The four legacy variants are deleted.
- `Task` no longer contains `switching_out` or `wake_after_switch` fields. `PENDING_SWITCH_OUT[core]` is deleted.
- Each `Task` has a `pi_lock` field acquired around every canonical-state transition. The v2 lock ordering is `pi_lock` *outer*, `SCHEDULER.lock` *inner*. A debug assertion fails if `pi_lock` is acquired while `SCHEDULER.lock` is already held.
- Each `Task` has an `on_cpu: AtomicBool` field set by the block path before `switch_context` and cleared by the arch-level switch-out epilogue after `saved_rsp` is published. The wake primitive `smp_cond_load_acquire`-spins on `on_cpu == false` before cross-core enqueue, replacing the v1 `PENDING_SWITCH_OUT[core]` RSP-publication marker. `pick_next` guards on `!on_cpu && saved_rsp != 0` (replacing the v1 `!switching_out && saved_rsp != 0` guard).
- `kernel-core::sched_model` host tests cover every transition in the new state machine and pass on `cargo test -p kernel-core`.
- A property-based fuzz test with at least 10,000 random block/wake/scan interleavings produces no lost wakes and no double-wakes.
- The 100 Hz tick-multiplier assumption is removed from every site that currently embeds it: `cpu-hog` log message, `stale-ready` log message, `sys_poll`, `select_inner`, `sys_epoll_wait`. After the sweep, all reported timeouts and durations match wall-clock observation (no 10× discrepancy at any site).
- `cargo xtask check` clean (clippy `-D warnings`, rustfmt).
- `cargo xtask test` passes (all in-QEMU kernel tests).

### Secondary (bug elimination)

- `cargo xtask run-gui --fresh` on the user's test hardware: cursor moves on mouse motion within 1 s of motion start; keyboard input typed in the framebuffer terminal appears within 100 ms; term reaches `TERM_SMOKE:ready`. (Resolves the 2026-04-28 cursor-at-(0,0) symptom.)
- 50 consecutive SSH disconnect/reconnect cycles in one session without a scheduler hang. (Resolves the 2026-04-25 SSH cleanup hang.)
- 30 minutes idle plus 30 minutes synthetic IPC + futex + notification load on 4 cores: no `[WARN] [sched]` stuck-task dumps.
- `sys_poll(2000)` returns after ~2000 ms of wall clock, not 200 ms.
- `cpu-hog` log values match wall-clock observation (no 10× discrepancy).
- `serial_stdin_feeder_task` no longer parks its host core. `kbd_server` runs even when the load balancer places it on the same core as the feeder.
- `audio_server` registers `audio.cmd` (stub if no hardware). `session_manager` does not text-fallback when AC'97 is absent.

### Engineering practice

- TDD: every code change has a corresponding test that was added before the implementation. PR commit history shows test-first ordering.
- SOLID, DRY, documented invariants, lock-ordering, migration safety, observability — see Engineering Practice Requirements above.

## Companion Task List

- [Phase 57a Task List](./tasks/57a-scheduler-rewrite-tasks.md)

## How Real OS Implementations Differ

- **Linux uses RCU plus per-CPU runqueue locks (`rq->lock`) plus per-task `p->pi_lock`** for full scalability across hundreds of cores. m3OS keeps a single global `SCHEDULER.lock()` for run-queue manipulation; the new `pi_lock` is the first per-task lock and is the smallest step toward Linux's discipline. Per-CPU runqueues are deferred to a later phase.
- **Linux's `try_to_wake_up`** does multi-stage lockless tracking (`p->on_rq`, `p->on_cpu`) to handle migration during wake. m3OS's wake primitive holds `pi_lock` for the entire transition — simpler but lower throughput on heavy contention.
- **Linux uses `prepare_to_wait` / `finish_wait` wait-queue helpers** above the raw block primitive so subsystems get a consistent wait-loop wrapper. m3OS keeps the raw `block_current_until` interface and lets each subsystem build its own wait wrapper. A helper layer is deferred.
- **Real-time Linux variants (PREEMPT_RT)** replace `pi_lock` with `rt_mutex` for priority inheritance. m3OS does not yet have priority inheritance; this phase does not introduce it.
- **seL4** uses a similar single-state-word approach but with a fixed-priority scheduler and no migration; m3OS retains its per-core load balancer and so must handle the cross-core case which seL4 sidesteps.

## Deferred Until Later

- Per-CPU runqueues with per-CPU locks (candidate Phase 57b or a later kernel-architecture phase).
- Priority inheritance (`rt_mutex` equivalent).
- Wait-queue helper layer (`prepare_to_wait` / `finish_wait` style) above the raw primitive.
- Loom-style formal interleaving search beyond the property-based fuzz harness — the harness is the floor; loom is a stretch goal in Track A.6.
- Refactoring `userspace-init`'s boot fork burst to be less spammy on the scheduler — orthogonal to this phase, but the boot fork pressure is what surfaces the lost-wake races.
- Migration of the `< 1 ms` branch of `sys_nanosleep` away from TSC busy-spin (the new primitive lets this happen but it is held back as a separate optimisation phase).
