---
status: COMPLETE — Phase 99 Track A.1 + A.5 deliverable. Every `block_current_until`
  caller audited against the Phase 57a single-state-word v2 protocol. 29 sites: 28
  conform, 1 non-conformant (`ipc/notification.rs` `wait()`) found and FIXED in this
  phase (commit on `feat/phase-99-smp-scheduler-robustness`). Futex REQUEUE/CMP_REQUEUE
  path (A.5) confirmed conformant.
date: 2026-06-28
phase: phase-99
component: kernel/task (scheduler v2 block/wake), kernel/ipc (notification, registry),
  kernel/arch/x86_64/syscall (futex + every blocking syscall), kernel/blk (virtio_blk)
related:
  - docs/handoffs/2026-04-25-scheduler-design-comparison.md   # recommended the single-state-word model
  - docs/roadmap/tasks/99-smp-scheduler-robustness-tasks.md   # Track A.1 / A.2 / A.5
---

# Phase 99 — Blocking Call-Site Conformance Audit (Track A.1 + A.5)

## Scope & method

The Phase 57a single-state-word v2 block/wake model is **already present**:
`block_current_until` (`kernel/src/task/scheduler.rs:3580`) follows the four-step Linux
recipe, `wake_task_v2` (`scheduler.rs:4538`) is the CAS wake, and the v1 flags
(`switching_out`/`wake_after_switch`/`PENDING_SWITCH_OUT`) are deleted. Track A.1 does
not re-introduce it — it **audits every wait site for uniform conformance**, because the
lost-wake recurrences (Phase 89/90b, the 2026-06-14 cross-core lost-wake, Phase 95) were
per-site patches and "the model is only as correct as its least-conformant wait site."

**The conformance invariant.** The `woken` flag passed to `block_current_until` must be
set to `true` by the wake side **before or concurrent with** `wake_task_v2`, so step-3's
`woken.load(Acquire)` recheck can self-revert (`Blocked*→Running`, return without
yielding) when a wake lands in the window between the state write and the yield. A
**fresh** flag per block call (or an edge-reset static) is required; a latched per-site
flag that survives a prior block call, or a **dummy** flag the wake side never sets, both
defeat step-3.

Every `block_current_until` caller in `kernel/src/**` was enumerated and classified.

## Result: 29 sites, 28 conformant, 1 non-conformant (now fixed)

### Conformant sites (28)

| Site | File:line | Blocked kind | Flag shape | Why conformant |
|---|---|---|---|---|
| W1–W6 | `scheduler.rs:3893/3961/4142/4197/4227/4268` | Reply/Recv/Notif/Send | fresh `Arc<AtomicBool>` registered via `register_reply_waker` | `deliver_message`/`complete_send` set the reply_waker flag before `wake_task_v2`; register-before-`pending_msg`-check closes the TOCTOU window |
| 2 | `ipc/registry.rs:157` | BlockedOnService | `Arc` in `ServiceWaiter` | `wake_registered_waiters` sets `woken` before push; register before the 2nd REGISTRY check |
| 3 | `task/wait_queue.rs:75` | BlockedOnRecv | fresh `Arc` in `WaitEntry` | `wake_one`/`wake_all` set `woken` before `wake_task_v2` |
| 4 | `blk/virtio_blk.rs:581` | BlockedOnRecv | struct-field, **reset in `register()`** | IRQ `wake_all` sets flag; reset clears stale state |
| 5 | `blk/virtio_blk.rs:968` | BlockedOnRecv | static `REQ_WOKEN`, **reset before submit** (one in flight) | IRQ sets flag + `wake_task_v2`; `while !REQ_WOKEN` loop |
| 6 | `lib.rs:1061` | BlockedOnRecv | static `STDIN_FEEDER_WOKEN`, **edge-reset at loop top** | COM1 RX ISR sets flag |
| 7 | `lib.rs:1174` | BlockedOnRecv | static `NIC_WOKEN`, **edge-reset at loop top** | NIC ISR / RemoteNic sets flag |
| 8 | `syscall/mod.rs:4303` | BlockedOnRecv | stack dummy, **deadline-only** | sole waker is the deadline scanner; no concurrent flag-waker → no lost-wake |
| 9 | `syscall/mod.rs:5804` | BlockedOnWait | `Arc`, **reset per iteration** | `wake_child_waiters` sets flag; 1 s deadline backstop |
| 10–11 | `syscall/mod.rs:6415/6461` | BlockedOnRecv | fresh `Arc`, register-before-check | PTY master/slave `WaitQueue::wake_all` |
| 12–16 | `syscall/mod.rs:6538/6591/6664/6884/7850` | Recv/Send | `Arc`, **reset per iteration** | eventfd/timerfd/stdin/pipe register-before-recheck |
| 17 | `syscall/mod.rs:18696` | BlockedOnRecv | `Arc` fresh **inside loop body** | flock; post-register `try_acquire` recheck |
| 18 | `syscall/mod.rs:19886` | BlockedOnFutex | `Arc` in `FutexWaiter` | `FUTEX_WAKE`/`CMP_REQUEUE` set `woken` before `wake_task_v2` (see A.5) |
| 19–20 | `syscall/mod.rs:23073/23629` | BlockedOnRecv | `Arc` fresh inside loop | AF_UNIX sendmsg/recvmsg |
| 21–23 | `syscall/mod.rs:24408/24663/25422` | BlockedOnRecv | `Arc`, reset/fresh per block | poll/select/epoll_wait, post-register readiness recheck |

### Non-conformant site (1) — `ipc/notification.rs` `wait()` — FOUND & FIXED

`notification::wait()` blocks via `block_current_until(BlockedOnNotif, &dummy, None)` with
a **dummy stack-local `AtomicBool::new(false)`** the wake side never sets. The intentional
design (documented in `wait()`) is that the outer `loop` re-drains `PENDING[idx]` on each
wake, so the flag value "doesn't matter" — but that reasoning misses one window.

**The lost-wake (verified against source).** The two wake paths are asymmetric:

- **`signal_irq()` (ISR, every driver IRQ)** does *not* take the `WAITERS` slot — it only
  clears the lock-free `ISR_WAITERS` mirror and pushes to the per-core `IsrWakeQueue`. So if
  its `wake_task_v2` (run from the dispatch-loop drain) CAS-fails because the task is still
  `Running` in the gap before the `BlockedOnNotif` commit, the BSP's
  `drain_pending_waiters()` (`scheduler.rs:5219`, every dispatch iteration) re-finds
  `WAITERS[idx]=Some` + `PENDING!=0` and **rescues** the task. *Already protected.*
- **`signal()` (task-context `notify_signal`)** *does* `waiters[idx].take()` → `None`. If
  its `wake_task_v2` CAS-fails (task still `Running`), the task then commits `BlockedOnNotif`
  with the dummy flag → step-3 cannot self-revert → it parks with `PENDING!=0` but a `None`
  slot, so `drain_pending_waiters()` finds nothing to wake. **Permanently stuck.**

**Fix (landed this phase).** Make `signal()` consistent with `signal_irq()`: when
`wake_task_v2` returns `AlreadyAwake` (the CAS-failed / lost-wake case), **re-register** the
`WAITERS` slot (the `PENDING` bits we set are still pending) so the same proven
`drain_pending_waiters()` safety net wakes the task once it commits. An `is_none()` guard
avoids clobbering a slot the task re-took by looping. This was preferred over rewriting the
`WAITERS` element type to carry an `Arc<AtomicBool>` (the textbook v2 wiring) because that
type change ripples through the shared `recv_msg_with_notif` registration path
(`register_recv_waiter`/`unregister_recv_waiter`) and the ISR mirror — higher blast radius
for an equivalent guarantee. The re-register reuses the *existing* rescue mechanism rather
than introducing a parallel one. `wait()`'s dummy-flag rationale comment was updated to
document the closure.

## Track A.5 — Futex REQUEUE / CMP_REQUEUE conformance (confirmed)

`FUTEX_REQUEUE`/`FUTEX_CMP_REQUEUE` (`syscall/mod.rs:19991–20080`):

- **Wake path** (up to `nr_wake`): `w.woken.store(true, Release)` is set **before**
  `wake_task_v2(tid)` — canonical. A CAS-fail still self-reverts via the set flag.
- **Requeue path** (remaining `nr_requeue`): waiters keep their original `Arc<AtomicBool>`
  (still `false`), stay in `BlockedOnFutex`, and move from `FUTEX_TABLE[key]` to `[key2]`.
  They are woken only by a future `FUTEX_WAKE` on `uaddr2` via the same set-flag-then-CAS
  protocol. No flag is carried over from a prior block; the `FUTEX_WAIT` dequeue scans all
  keys, so a requeued waiter is removed correctly after resume.
- **`CMP_REQUEUE` atomicity**: the `*uaddr == val3` check runs under `FUTEX_TABLE.lock()`;
  the `val3` page is pre-faulted before lock acquisition (musl `pthread_cond_*` requirement).

Phase 89's `FUTEX_REQUEUE` and Phase 90b's per-address-space futex keys + cross-thread
PKU read-recovery are thereby **subsumed** by this audited model — they are not independent
special cases.

## Conclusion

The single-state-word model is now **uniformly** applied: 28 sites already conformed, the
one gap (`notification::wait` task-context `signal` lost-wake) is closed, and the futex
requeue path is confirmed. The four prior lost-wake fixes are subsumed by this audit. The
SMP-8 `smp-smoke` gate (Track A.4) is the standing stress validation of the consolidated
model.
