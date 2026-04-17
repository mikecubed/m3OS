---
title: Phase 54 follow-up work
status: open
---

# Phase 54: follow-up work

**Status:** Phase 54 deep serverization closed on PR #108. Most items
surfaced during the closure debugging and review cycle have been routed
to their owning phases; the two items below remain as long-term backlog
with no current action. Each entry records its owner and the condition
that would make it worth picking up.

## Routing summary

| Original item | Routed to | Status |
|---|---|---|
| 1. `FdEntry` CLOEXEC / NONBLOCK plumbing | Phase 54a Track A | Planned |
| 2. `arch::x86_64::syscall::*_pub` wrapper relocation | Phase 54a Track B | Planned |
| 3. `/var/run → /run` compatibility symlink | Phase 45 `Deferred Until Later` | Deferred |
| 5. Interrupt-driven virtio_blk completion | Phase 55 Track C.5 | Planned |
| 7. Parent-doc cleanup (`54-remaining-smp-race.md`, `54-review-findings.md`) | Already done in PR #108 | Complete |

The two items below are the remaining long-term backlog — neither blocks
any planned phase and neither has a concrete owner yet.

## 1. Long-term: replace `MOUNT_OP_LOCK` with a yielding primitive

**Site:** `kernel/src/arch/x86_64/syscall/mod.rs:94` —
`static MOUNT_OP_LOCK: spin::Mutex<()>`.

**Current state after PR #108:** The lock is only held around the
mount / umount mutation itself — path resolution runs outside it — so
"sleep while holding spinlock" is no longer reachable. The remaining
concern is that two cores that do race on the lock still busy-spin in
ring 0 until the holder releases.

**Why deferred:** not hot in practice. Mount / umount is rare.

**Long-term options:**

- Replace with a yielding mutex that calls `task::yield_now()` while
  waiting. Works cleanly for kernel-task callers; needs care for
  callers that already hold the scheduler lock.
- Replace with an `RwLock<()>` — readers (path resolution) take
  shared, writers (mount / umount) take exclusive. Lets parallel path
  resolution proceed while mount / umount is quiescent.

**Owner:** no current owner. Pick up as part of a general
"cooperative kernel synchronization" pass if contention ever shows up
in profiling.

## 2. Scheduler diagnostic thresholds — tune with baseline data

**Sites:** `kernel/src/task/scheduler.rs`:

- `[sched] stale-ready` — fires when a Ready task waits ≥ 50 ticks
  (≈ 500 ms) before dispatch.
- `[sched] cpu-hog` — fires when a task held a core ≥ 20 ticks
  (≈ 200 ms) before yielding.

**Why open:** The 200 ms cpu-hog threshold is aggressive — it
surfaces legitimate one-time work during init's service startup and
login. That is acceptable for now (rare, one-shot, identifiable). If
it becomes noise over day-to-day use, raise to 50 ticks (≈ 500 ms) so
only genuine hangs fire.

**Recommended change:** no code change unless the noise becomes a
problem. Then a one-line threshold bump.

**Owner:** whoever next touches the scheduler diagnostics. Revisit
condition: the thresholds fire on >1 % of boots during normal
operation.
