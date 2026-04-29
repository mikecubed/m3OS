---
status: Complete
source-ref: phase-57a
task-list: docs/roadmap/tasks/57a-scheduler-rewrite-tasks.md (Track A.2, lines 57–66)
date: 2026-04-29
---

# Phase 57a — v1 Scheduler Block/Wake Transition Table

## Why this exists

The v1 block/wake protocol splits a single conceptual "is this task blocked?"
decision across three observable flags — `Task::state`, `Task::switching_out`,
and `Task::wake_after_switch` — plus the per-core `PENDING_SWITCH_OUT[core]`
deferred-enqueue hand-off. Each pair of flags carries its own invariant, and
those invariants hold only under the assumption that the dispatch switch-out
handler runs exactly once per block call, promptly after `switch_context`
stores the task's RSP. When that assumption breaks (cross-core IPC pressure,
SMP reschedule IPI, a scan that fires between block-entry and handler
execution), the latched flag and the task's actual blockedness desynchronise,
producing the lost-wake bug class catalogued in
`docs/handoffs/2026-04-25-scheduler-design-comparison.md` and
`docs/handoff/2026-04-28-graphical-stack-startup.md`.

This table is the regression test contract for the v2 rewrite: the cells that
were already correct must have equivalent v2 cells; the cells annotated as
exhibiting the lost-wake bug are the ones v2 must eliminate.

---

## Notation

- **so** = `Task::switching_out`
- **was** = `Task::wake_after_switch`
- **Blocked\*** = any of `BlockedOnRecv`, `BlockedOnSend`, `BlockedOnReply`,
  `BlockedOnNotif`, `BlockedOnFutex`; all five behave identically in the
  block/wake protocol so they are represented as one set of rows.
- **Events**:
  - `block` — caller invokes `block_current_*` or `yield_now`
  - `wake` — `wake_task(id)` is called (from ISR or task context)
  - `scan_expired` — `scan_expired_wake_deadlines` finds `wake_deadline <= now`
  - `dispatch_switch_out` — dispatch loop consumes `PENDING_SWITCH_OUT[core]`
    after `switch_context` returns to scheduler RSP
- **Lock held during transition**: every cell below names the lock held at the
  moment of the state write.
- **Side effects** names every observable change (state write, flag set/clear,
  ACTIVE_WAKE_DEADLINES delta, run-queue push).

---

## Effective State Space (v1)

The v1 effective state is the three-tuple `(TaskState, so, was)`. The table
below covers the full relevant subset — states that are reachable by normal
scheduler operation. The `(BlockedOn*, so, was)` family is shown once; all
five `Blocked*` variants share the same transitions.

| Row | Current state | so | was | Notes |
|-----|---------------|----|-----|-------|
| 1   | Running       | F  | F   | Task currently executing on CPU |
| 2   | Running       | T  | F   | Post-yield/block lock-release, pre-dispatch-handler; `saved_rsp` not yet published |
| 3   | Blocked\*     | T  | F   | Block entered, `switch_context` in progress, no concurrent wake |
| 4   | Blocked\*     | T  | T   | Wake arrived during switch-out window; deferred enqueue latched |
| 5   | Blocked\*     | F  | F   | Stably blocked; `saved_rsp` published |
| 6   | Ready         | F  | F   | In run queue, awaiting dispatch |
| 7   | Dead          | T  | F   | `mark_current_dead` executed, RSP not yet published |

---

## Transition Table

<!-- Column order: block | wake | scan_expired | dispatch_switch_out -->

### Row 1 — (Running, so=F, was=F)

| Event | Next state | Side effects | Invariant | Bug? |
|-------|-----------|--------------|-----------|------|
| `block` | (Blocked\*, so=T, was=F) | Under `SCHEDULER.lock`: `state ← Blocked*`; `so ← true`; `wake_deadline ← Some(d)` if timed; `ACTIVE_WAKE_DEADLINES++` if new deadline; `set_current_task_idx(None)`. After lock: `PENDING_SWITCH_OUT[core] ← idx`; `switch_context` to scheduler RSP. | Lock is `SCHEDULER.lock`. Task leaves run queue on `set_current_task_idx(None)`. `saved_rsp` must not be read by another core until `dispatch_switch_out` clears `so`. | No |
| `wake` | (Ready, so=F, was=F) | Under `SCHEDULER.lock`: `state ← Ready`; `last_ready_tick ← now`; `wake_deadline.take()` (decrement counter if Some). Return `(enqueue=true, …)`. After lock: `enqueue_to_core(assigned_core, idx)`. | Lock is `SCHEDULER.lock`. Wake to Running is a no-op (match arm falls to `_ => (None, false, …)`); this cell applies only if `wake_task` is called and state is already not `Blocked*`. **Actually**: `wake_task` does nothing for `Running` — see code line 1488. Re-labelled: no-op. | No |
| `scan_expired` | (Running, so=F, was=F) | Under `SCHEDULER.lock` (caller holds it): `wake_deadline` cleared if Some; ACTIVE_WAKE_DEADLINES--. No state change. | Task is not blocked; scan sees non-Blocked state and clears stale deadline (line 2261). | No |
| `dispatch_switch_out` | N/A | Not reachable — `PENDING_SWITCH_OUT[core]` is only set when `so=true`. | — | No |

### Row 2 — (Running, so=T, was=F)

| Event | Next state | Side effects | Invariant | Bug? |
|-------|-----------|--------------|-----------|------|
| `block` | Not reachable | A Running task with `so=T` is between `switch_context` and the dispatch handler on its own core; it cannot call `block` concurrently. | — | No |
| `wake` | (Running, so=T, was=F) | Under `SCHEDULER.lock`: `state` is `Running` — falls to `_ => (None, false, …)` (line 1488). No enqueue. | Lock is `SCHEDULER.lock`. Wake to Running/Ready that is mid-switch-out is silently dropped even though `so=T`; the dispatch handler will re-enqueue via `reenqueue_after_yield` if `state==Running`. | No |
| `scan_expired` | (Running, so=T, was=F) | Under `SCHEDULER.lock` (caller holds it): `wake_deadline` cleared if Some. No state change (state is Running). | Scan sees non-Blocked state, clears stale deadline. | No |
| `dispatch_switch_out` | (Ready, so=F, was=F) | Under `SCHEDULER.lock`: `task.saved_rsp ← saved_rsp`; `so ← false`; `wake_after_switch ← false`; `reenqueue_after_yield = (pending==switched && state==Running)` is `true`. `state ← Ready`; `last_ready_tick ← now`. After lock: `enqueue_to_core(assigned_core, sidx)`. | Lock is `SCHEDULER.lock` (acquired inside dispatch handler). `saved_rsp` is written before `so` is cleared (lines 2124, 2137). | No |

### Row 3 — (Blocked\*, so=T, was=F)

| Event | Next state | Side effects | Invariant | Bug? |
|-------|-----------|--------------|-----------|------|
| `block` | Not reachable | Task is already blocked and off-CPU. | — | No |
| `wake` | (Blocked\*, so=T, was=T) — **LOST-WAKE PATH** | Under `SCHEDULER.lock`: `wake_deadline.take()` (ACTIVE_WAKE_DEADLINES--); `wake_after_switch ← true`. Returns `(enqueue=None, woke=true)`. No enqueue. | Lock is `SCHEDULER.lock`. Wake is deferred to `dispatch_switch_out`. If `dispatch_switch_out` does not run promptly, or if a second block call overwrites `was` before the handler observes it, the wake is lost. **See `docs/handoffs/2026-04-25-scheduler-design-comparison.md` §"The specific invariant Linux maintains that m3OS violates" and the re-block scenario.** | **YES** |
| `scan_expired` | (Blocked\*, so=T, was=T) — **LOST-WAKE PATH** | Under `SCHEDULER.lock` (caller holds it): `wake_deadline.take()` (ACTIVE_WAKE_DEADLINES--); `wake_after_switch ← true`; `last_migrated_tick ← now`. No enqueue. | Lock is `SCHEDULER.lock`. Same deferred-enqueue race as `wake` in this row. **See `docs/handoffs/2026-04-25-scheduler-design-comparison.md` §"The specific invariant Linux maintains that m3OS violates"** and `docs/handoff/2026-04-28-graphical-stack-startup.md` §"Hypotheses ranked" (hypothesis 1). | **YES** |
| `dispatch_switch_out` | (Blocked\*, so=F, was=F) | Under `SCHEDULER.lock`: `task.saved_rsp ← saved_rsp`; `so ← false`; `wake_after_switch` is `false` — no enqueue. Task remains Blocked. | Lock is `SCHEDULER.lock`. This is the correct path: block is stable, wake has not arrived yet. | No |

### Row 4 — (Blocked\*, so=T, was=T)

| Event | Next state | Side effects | Invariant | Bug? |
|-------|-----------|--------------|-----------|------|
| `block` | Not reachable | Task is blocked and off-CPU. | — | No |
| `wake` | (Blocked\*, so=T, was=T) | Under `SCHEDULER.lock`: `wake_deadline` already cleared in Row 3 transition; `wake_after_switch` already `true`. No change (idempotent because `was` is already true). However, a second `wake_task` call may arrive and do nothing further — effectively a no-op latch. | Lock is `SCHEDULER.lock`. | No |
| `scan_expired` | (Blocked\*, so=T, was=T) | Under `SCHEDULER.lock` (caller holds it): `wake_deadline` is `None` (cleared in Row 3 transition); scan's `take()` returns `None`; counter unchanged. `was` already `true`. No change. | Lock is `SCHEDULER.lock`. | No |
| `dispatch_switch_out` | (Ready, so=F, was=F) | Under `SCHEDULER.lock`: `task.saved_rsp ← saved_rsp`; `so ← false`; reads `wake_after_switch=true`, `blocked=true` → `state ← Ready`; `last_ready_tick ← now`; `was ← false`. After lock: `enqueue_to_core(assigned_core, sidx)`. | Lock is `SCHEDULER.lock`. This is the *intended* happy path when a wake arrives during the switch-out window. Depends on `dispatch_switch_out` running exactly once and promptly. | No |

### Row 5 — (Blocked\*, so=F, was=F)

| Event | Next state | Side effects | Invariant | Bug? |
|-------|-----------|--------------|-----------|------|
| `block` | Not reachable | Task is blocked and off-CPU. | — | No |
| `wake` | (Ready, so=F, was=F) | Under `SCHEDULER.lock`: `state ← Ready`; `last_ready_tick ← now`; `wake_deadline.take()` (ACTIVE_WAKE_DEADLINES-- if Some). Returns `(enqueue=Some(assigned_core, idx), woke=true)`. After lock: `enqueue_to_core`. | Lock is `SCHEDULER.lock`. This is the normal, correct wake path. | No |
| `scan_expired` | (Ready, so=F, was=F) | Under `SCHEDULER.lock` (caller holds it): `wake_deadline.take()` (ACTIVE_WAKE_DEADLINES--); `state ← Ready`; `last_ready_tick ← now`; `last_migrated_tick ← now`. Pushes `(assigned_core, idx)` to expired array. | Lock is `SCHEDULER.lock`. Normal deadline-expiry wake path. | No |
| `dispatch_switch_out` | N/A | Not reachable — `PENDING_SWITCH_OUT[core]` is only written when `so=true`. | — | No |

### Row 6 — (Ready, so=F, was=F)

| Event | Next state | Side effects | Invariant | Bug? |
|-------|-----------|--------------|-----------|------|
| `block` | Not reachable | Task in run queue is not executing; it cannot call `block`. | — | No |
| `wake` | (Ready, so=F, was=F) | Under `SCHEDULER.lock`: state is `Ready` — falls to `_ => (None, false, …)`. No-op. | Lock is `SCHEDULER.lock`. Spurious wake to ready task is silently dropped. | No |
| `scan_expired` | (Ready, so=F, was=F) | Under `SCHEDULER.lock` (caller holds it): state is `Ready` — non-Blocked check fires, clears stale deadline if any. | Lock is `SCHEDULER.lock`. | No |
| `dispatch_switch_out` | N/A | Not reachable for a Ready task. | — | No |

### Row 7 — (Dead, so=T, was=F)

| Event | Next state | Side effects | Invariant | Bug? |
|-------|-----------|--------------|-----------|------|
| `block` | Not reachable | Dead task is off-CPU. | — | No |
| `wake` | (Dead, so=T, was=F) | Under `SCHEDULER.lock`: state is `Dead` — falls to `_ => (None, false, …)`. No-op. | Lock is `SCHEDULER.lock`. | No |
| `scan_expired` | (Dead, so=T, was=F) | Under `SCHEDULER.lock` (caller holds it): state is `Dead` — non-Blocked check fires, clears stale deadline if any. | Lock is `SCHEDULER.lock`. | No |
| `dispatch_switch_out` | (Dead, so=F, was=F) | Under `SCHEDULER.lock`: `task.saved_rsp ← saved_rsp`; `so ← false`; `was` is `false`, state is `Dead` — `blocked=false` → no enqueue. Task remains Dead awaiting `drain_dead`. | Lock is `SCHEDULER.lock`. | No |

---

## Lost-Wake Bug Summary

The two cells that exhibit the lost-wake bug class are:

| Cell | Row | Event | Bug mechanism |
|------|-----|-------|---------------|
| (Blocked\*, so=T, was=F) + `wake` | 3 | `wake` | Wake is deferred to `wake_after_switch` latch; if `dispatch_switch_out` does not run before the next block call, the latch is stale or consumed by the wrong block cycle. |
| (Blocked\*, so=T, was=F) + `scan_expired` | 3 | `scan_expired` | Same deferred-enqueue path; same race window. |

Both are cited in:
- `docs/handoffs/2026-04-25-scheduler-design-comparison.md` — "The specific invariant Linux maintains that m3OS violates" and the re-block scenario (steps 1–6 in that section).
- `docs/handoff/2026-04-28-graphical-stack-startup.md` — §"Hypotheses ranked" hypothesis 1: "display_server.poll_mouse calls ipc_call(mouse_handle, MOUSE_EVENT_PULL, 0), which blocks display_server in BlockedOnReply. When mouse_server's reply races with display_server's switch-out under the switching_out / wake_after_switch protocol, the wake is lost."

---

## Cell count

v1 table has **28 cells** (7 rows × 4 events).
