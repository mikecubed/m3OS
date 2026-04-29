---
status: Complete
source-ref: phase-57a
task-list: docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md (Track A.3, lines 68–77)
date: 2026-04-29
---

# Phase 57a — v2 Scheduler Block/Wake Transition Table

## Why this exists

The v1 protocol's lost-wake bug class arises because "is this task blocked?"
is encoded in three separate observable flags rather than a single state word.
The v2 protocol adopts Linux's `do_nanosleep` / `try_to_wake_up` pattern:
`Task::state` is the sole source of truth for block state; a per-task
`pi_lock` serialises all mutations to that state; the block primitive rechecks
the wait condition after the state write but before yielding; and the wake
primitive does a CAS from any `Blocked*` variant to `Ready`. The v1
intermediate-state flags and deferred-enqueue hand-off mechanism are absent
from v2; `Task::state` under `pi_lock` is the sole arbiter.

This table is the spec for Track A.4's `apply_event` function in
`kernel-core`, for Tracks C and D's new primitives, and for Tracks E and F's
field removals and call-site migrations. Every v2 cell must have a
corresponding host test (Track A.5) before any kernel-side implementation
lands (TDD gate from the engineering practice gates).

---

## v2 Protocol Summary

The v2 block primitive (`block_current_until`) follows the four-step Linux
recipe, mirroring `do_nanosleep` (`kernel/time/hrtimer.c`):

1. **State write under `pi_lock`.**
   Acquire `pi_lock`; write `task.state ← Blocked*`; set
   `task.wake_deadline ← deadline_ticks`; release `pi_lock`.
   This pairs with the acquire barrier in the wake side's CAS, closing the
   lost-wake window (Linux `smp_store_mb` / `set_current_state` pattern).

2. **Drop `pi_lock`.**
   The lock is released before the condition recheck so a concurrent waker
   can acquire `pi_lock` and perform its CAS without deadlock.

3. **Condition recheck.**
   Read the `woken` `AtomicBool` (and/or compare `tick_count()` against
   `deadline_ticks`) AFTER the state write. If the condition is already true,
   acquire `pi_lock` and CAS `Blocked* → Running`; clear `wake_deadline`;
   release `pi_lock`; return without yielding. This is the self-revert path
   that closes the race window between state-write and yield (Linux: the
   condition check on `t->task` before `schedule()`).

4. **Yield via `SCHEDULER.lock`.**
   Acquire `SCHEDULER.lock`; remove task from run queue; call
   `switch_context` to the scheduler RSP. On resume, recheck the condition;
   if false (spurious wake), re-enter step 1. The expected path is one trip.

**Wake side** (`wake_task` v2):

1. Acquire `pi_lock`; CAS `state` from any `Blocked*` to `Ready`; clear
   `wake_deadline` (decrement `ACTIVE_WAKE_DEADLINES` if Some); release
   `pi_lock`.
2. If CAS failed (task was not in `Blocked*`): return `AlreadyAwake`.
3. Acquire `SCHEDULER.lock`; if task is already in a run queue (enqueued by
   a concurrent waker), return `Woken` (idempotency guard).
4. If `task.on_cpu == true` (task's `saved_rsp` not yet published by the
   arch-level switch-out epilogue): spin-wait (`smp_cond_load_acquire`-style)
   until `on_cpu` becomes false. This replaces v1's `PENDING_SWITCH_OUT[core]`
   RSP-publication guard (Linux `p->on_cpu` `smp_cond_load_acquire` pattern,
   cited in `try_to_wake_up`, `kernel/sched/core.c`).
5. Enqueue task to its `assigned_core` run queue; if cross-core, send
   reschedule IPI.

**Lock ordering** (from B.3, enforced by debug assertion):
`pi_lock` is *outer*, `SCHEDULER.lock` is *inner` (Linux's `p->pi_lock` →
`rq->lock` pattern). A code path may hold `pi_lock` while acquiring
`SCHEDULER.lock`; the reverse is forbidden and panics in debug builds.

**`Task::on_cpu`** (introduced in Track E.1) replaces the RSP-publication
aspect of v1's `PENDING_SWITCH_OUT[core]`:
- Block side: set `on_cpu ← true` after releasing `pi_lock`, before
  `switch_context`.
- Arch-level switch epilogue: set `on_cpu ← false` after `saved_rsp` is
  durably written.
- Wake side: spin-wait on `on_cpu == false` under `SCHEDULER.lock` before
  enqueue.

**Linux citations:**
- `kernel/time/hrtimer.c::do_nanosleep` — four-step pattern, `t->task`
  recheck before `schedule()`.
- `kernel/sched/core.c::try_to_wake_up` — CAS `p->__state` from sleeping to
  running; `p->on_cpu` `smp_cond_load_acquire` spin-wait before enqueue.
- `kernel/sched/core.c::set_current_state` — `smp_store_mb` barrier on state
  write, pairs with `try_to_wake_up`'s barrier on state read.

---

## Notation

- **`pi_lock`** = per-task spinlock protecting `TaskBlockState` (`state`,
  `wake_deadline`).
- **`SCHEDULER.lock`** = global IrqSafeMutex protecting run-queue membership
  and `Task::on_cpu`.
- **Events**:
  - `block` — caller invokes `block_current_until(woken, deadline)`
  - `wake` — `wake_task(id)` is called (from ISR or task context)
  - `scan_expired` — `scan_expired_wake_deadlines` finds `wake_deadline <= now`
- **Cells**: `(next state, side effects, invariant, locks held during transition)`

---

## Transition Table

Rows = `TaskState` variants (using exact Rust variant names from
`kernel/src/task/mod.rs`). Columns = v2 events.

---

### Ready

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Ready (no-op early return) | None — caller checks condition before entering `block_current_until`; if condition is already met, function returns without state write. | A task in `Ready` cannot call `block_current_until` because it is not currently executing; this cell is reached only if the primitive is called with an already-true condition from a just-dispatched task before any state write. | Neither |
| `wake` | Ready (no-op) | Under `pi_lock`: CAS `Blocked* → Ready` fails (state is `Ready`, not `Blocked*`). Returns `AlreadyAwake`. No `SCHEDULER.lock` acquired. | A wake to a `Ready` task is idempotent. Spurious wakes are silent no-ops. | `pi_lock` only (briefly, CAS fails, released) |
| `scan_expired` | Ready (no-op) | Under `SCHEDULER.lock` (caller holds it): state is `Ready` — non-Blocked, stale deadline cleared if any. | Scan to a non-Blocked task is a no-op; stale deadline (from an aborted block) is cleaned up. | `SCHEDULER.lock` (held by caller) |

---

### Running

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Blocked\* (via four-step protocol), then eventually `Ready` on wake | Under `pi_lock`: `state ← Blocked*`; `wake_deadline ← Some(d)` if timed; `ACTIVE_WAKE_DEADLINES++` if new deadline. Release `pi_lock`. Recheck condition. If true: re-acquire `pi_lock`; CAS `Blocked* → Running`; clear `wake_deadline`; release; return (self-revert, no yield). If false: acquire `SCHEDULER.lock`; yield via `switch_context`. | A `Running` task transitioning to `Blocked*` must write state under `pi_lock` before dropping the lock. The condition recheck after the state write closes the lost-wake window. | `pi_lock` for state write; then `SCHEDULER.lock` for yield (never both simultaneously during the write) |
| `wake` | Running (no-op) | Under `pi_lock`: CAS `Blocked* → Ready` fails (state is `Running`). Returns `AlreadyAwake`. | A wake to a `Running` task is idempotent. The task will recheck its condition in the `block_current_until` loop on resume. | `pi_lock` only (CAS fails, released) |
| `scan_expired` | Running (no-op) | Under `SCHEDULER.lock` (caller holds it): state is `Running` — non-Blocked, stale deadline cleared if any. | Scan to a `Running` task is a no-op. | `SCHEDULER.lock` (held by caller) |

---

### BlockedOnRecv

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Not reachable | A `BlockedOnRecv` task is off-CPU and cannot invoke `block_current_until`. | — | — |
| `wake` | Ready | Under `pi_lock`: CAS `BlockedOnRecv → Ready`; `wake_deadline.take()` (ACTIVE_WAKE_DEADLINES-- if Some); release `pi_lock`. Acquire `SCHEDULER.lock`; spin-wait if `on_cpu==true`; `enqueue_to_core(assigned_core, idx)`; send reschedule IPI if cross-core. Returns `Woken`. | CAS is the single state mutation; no intermediate flag. A concurrent `scan_expired` that observes `Ready` after this CAS is a no-op (non-Blocked). | `pi_lock` for CAS; then `SCHEDULER.lock` for enqueue |
| `scan_expired` | Ready | Under `SCHEDULER.lock` (caller holds it): state matches `Blocked*`; `wake_deadline ≤ now`; `wake_deadline.take()` (ACTIVE_WAKE_DEADLINES--); `state ← Ready`; `last_ready_tick ← now`. Add to expired array for enqueue after lock release. | Deadline expiry is equivalent to a wake; CAS is done under `SCHEDULER.lock` which the caller already holds (no `pi_lock` needed for scan because scan runs inside `SCHEDULER.lock` and state reads are consistent there). | `SCHEDULER.lock` (held by caller) |

---

### BlockedOnSend

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Not reachable | A `BlockedOnSend` task is off-CPU. | — | — |
| `wake` | Ready | Under `pi_lock`: CAS `BlockedOnSend → Ready`; `wake_deadline.take()` (ACTIVE_WAKE_DEADLINES-- if Some); release `pi_lock`. Acquire `SCHEDULER.lock`; spin-wait if `on_cpu==true`; `enqueue_to_core`; IPI if cross-core. Returns `Woken`. | Same CAS invariant as `BlockedOnRecv`. | `pi_lock` for CAS; then `SCHEDULER.lock` for enqueue |
| `scan_expired` | Ready | Under `SCHEDULER.lock` (caller holds it): `wake_deadline ≤ now`; `wake_deadline.take()`; `state ← Ready`; `last_ready_tick ← now`. Enqueued after lock release. | Deadline expiry is idempotent with respect to a concurrent `wake` CAS (one of them wins; the other sees `Ready` or no deadline). | `SCHEDULER.lock` (held by caller) |

---

### BlockedOnReply

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Not reachable | A `BlockedOnReply` task is off-CPU. | — | — |
| `wake` | Ready | Under `pi_lock`: CAS `BlockedOnReply → Ready`; `wake_deadline.take()`; release `pi_lock`. Acquire `SCHEDULER.lock`; spin-wait if `on_cpu==true`; `enqueue_to_core`; IPI if cross-core. Returns `Woken`. | The `BlockedOnReply` → `Ready` CAS is the fix for the display_server / mouse_server race documented in `docs/handoff/2026-04-28-graphical-stack-startup.md` §"Hypotheses ranked" hypothesis 1. No v1 deferred-enqueue flag path. | `pi_lock` for CAS; then `SCHEDULER.lock` for enqueue |
| `scan_expired` | Ready | Under `SCHEDULER.lock` (caller holds it): `wake_deadline ≤ now`; `wake_deadline.take()`; `state ← Ready`; enqueued after lock release. | Deadline expiry on a reply-blocked task (rare; only if a reply-wait has a timeout) is handled identically to other Blocked variants. | `SCHEDULER.lock` (held by caller) |

---

### BlockedOnNotif

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Not reachable | A `BlockedOnNotif` task is off-CPU. | — | — |
| `wake` | Ready | Under `pi_lock`: CAS `BlockedOnNotif → Ready`; `wake_deadline.take()`; release `pi_lock`. Acquire `SCHEDULER.lock`; spin-wait if `on_cpu==true`; `enqueue_to_core`; IPI if cross-core. Returns `Woken`. | Notification wake uses the same CAS primitive; no special path. | `pi_lock` for CAS; then `SCHEDULER.lock` for enqueue |
| `scan_expired` | Ready | Under `SCHEDULER.lock` (caller holds it): `wake_deadline ≤ now`; `wake_deadline.take()`; `state ← Ready`; enqueued after lock release. | Notification-blocked task with a timeout expires identically to other Blocked variants. | `SCHEDULER.lock` (held by caller) |

---

### BlockedOnFutex

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Not reachable | A `BlockedOnFutex` task is off-CPU. | — | — |
| `wake` | Ready | Under `pi_lock`: CAS `BlockedOnFutex → Ready`; `wake_deadline.take()`; release `pi_lock`. Acquire `SCHEDULER.lock`; spin-wait if `on_cpu==true`; `enqueue_to_core`; IPI if cross-core. Returns `Woken`. | Futex wake uses the same CAS primitive. | `pi_lock` for CAS; then `SCHEDULER.lock` for enqueue |
| `scan_expired` | Ready | Under `SCHEDULER.lock` (caller holds it): `wake_deadline ≤ now`; `wake_deadline.take()`; `state ← Ready`; enqueued after lock release. | Futex with timeout expires identically to other Blocked variants. | `SCHEDULER.lock` (held by caller) |

---

### Dead

| Event | Next state | Side effects | Invariant | Locks held |
|-------|-----------|--------------|-----------|------------|
| `block` | Not reachable | A `Dead` task is off-CPU and must not re-enter any block primitive. | — | — |
| `wake` | Dead (no-op) | Under `pi_lock`: CAS `Blocked* → Ready` fails (state is `Dead`). Returns `AlreadyAwake`. | A wake to a `Dead` task is a silent no-op; the reaper (`drain_dead`) handles cleanup independently of the scheduler wake path. | `pi_lock` only (CAS fails, released) |
| `scan_expired` | Dead (no-op) | Under `SCHEDULER.lock` (caller holds it): state is `Dead` — non-Blocked, stale deadline cleared if any. | Stale deadline on a dead task is cleaned up without any state change. | `SCHEDULER.lock` (held by caller) |

---

## Cell Count and Lost-Wake Surface

v2 table has **24 cells** (8 rows × 3 events).
v1 table had **28 cells** (7 rows × 4 events).

**Δ = 28 − 24 = 4 cells removed** (the `dispatch_switch_out` column is
eliminated entirely, along with its two lost-wake cells in Row 3).

The four eliminated cells correspond to:
- `(Running, so=T, was=F)` + `dispatch_switch_out` (Row 2, v1)
- `(Blocked*, so=T, was=F)` + `dispatch_switch_out` (Row 3, v1) — **lost-wake path**
- `(Blocked*, so=T, was=T)` + `dispatch_switch_out` (Row 4, v1) — deferred-enqueue happy path
- `(Dead, so=T, was=F)` + `dispatch_switch_out` (Row 7, v1)

The two lost-wake cells (Row 3, `wake` and `scan_expired` in v1) remain as
cells in v2 (`BlockedOn*` + `wake` and `BlockedOn*` + `scan_expired`), but
they no longer exhibit the bug: v2's CAS under `pi_lock` is atomic with the
state write, so there is no v1-style intermediate flag for a concurrent
waker to observe. The `on_cpu` spin-wait in step 4 of the wake side handles
the RSP-publication window without a deferred-enqueue hand-off.
