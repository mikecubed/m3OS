# Scheduler-Fairness Investigation — Closeout Note

**Status as of 2026-04-21: resolved.** The early-wedge was fixed by
converting `SCHEDULER.lock()` to an IRQ-safe mutex. A 60-run
confirmation batch came back 60/60 clean, which also rules out the
late-wedge as a separate bug (it was tail mis-classification of slow
early-wedges that elapsed the banner timeout). This file used to host
the "pick up and continue" prompt for a fresh session; it now serves
as a short closeout note. The full story lives in
`docs/appendix/scheduler-fairness-regression.md`.

## TL;DR for a future reader

1. **Root cause.** `virtio_net::virtio_net_irq_handler` and
   `virtio_blk::virtio_blk_irq_handler` synchronously call
   `wake_task` from ISR context. `wake_task` acquires
   `scheduler::SCHEDULER.lock()`. Until the fix, that lock was a
   plain `spin::Mutex<Scheduler>` — if a task-context holder of the
   lock on core X was interrupted by the virtio-net or virtio-blk
   IRQ on the same core, the ISR's `wake_task` spun forever on the
   lock. Heavy-logging mitigation closed the race probabilistically
   (slowed every path enough to shrink the deadlock window); it was
   never a fix.
2. **Fix.** `kernel/src/task/scheduler.rs`:
   - New `IrqSafeMutex<T>` wrapper. `lock()` saves IF state, masks
     interrupts on the current CPU, and acquires the inner
     `spin::Mutex`; guard drop releases the inner guard *before*
     restoring IF so an ISR cannot reach the freed slot with stale
     interrupt state.
   - `SCHEDULER: Mutex<Scheduler>` → `SCHEDULER:
     IrqSafeMutex<Scheduler>`. All 62 existing `SCHEDULER.lock()`
     call sites kept their textual form.
   - `enqueue_to_core` wraps its per-core `run_queue.lock()` body in
     `interrupts::without_interrupts(…)` to cover the brief IF-on
     window between dropping `SCHEDULER.lock` and acquiring the run
     queue lock.
   - `wake_task` takes `SCHEDULER.lock` exactly once. The previous
     implementation re-acquired it (and entered `PROCESS_TABLE.lock`
     via `task_log_label`) solely to snapshot log fields; those are
     now captured inside the first critical section, and a new
     `label_from_name_only` helper replaces the PROCESS-TABLE-based
     pid lookup.
3. **Validation.** No heavy-logging mitigation in either pass.
   - `/tmp/h9_batch.sh 15 irqfixA` → 15/15 clean-auth-rejected.
   - `/tmp/h9_batch.sh 60 latewedgeA` → 60/60 clean-auth-rejected,
     0 early-wedges, 0 late-wedges. At the pre-fix ~8 % late-wedge
     rate, the probability of a clean 60-run streak by chance is
     ~0.7 %; late-wedge is conclusively subsumed.
4. **Commits.**
   - `ac37270` fix(sched): make SCHEDULER.lock IRQ-safe to close early-wedge
   - `2c331ec` docs(appendix): record SCHEDULER.lock IRQ-safety fix + 15/15 validation
   - `<this commit>` docs(appendix): mark appendix resolved after 60-run confirmation

## Reproduction harness (kept for regression testing)

```
/tmp/h9_run_once.sh <run-id>
    → Single-shot boot + ssh attempt. Produces
      /tmp/h9run<id>.{log,ssh,summary}.
/tmp/h9_batch.sh <count> <prefix>
    → Run /tmp/h9_run_once.sh count times with RUN_ID=<prefix><i>.
      Tallies class= counts at the end.
```

Classification from the `summary` file:

- `class=clean-auth-rejected` → ssh exit=255 + "Permission denied".
  Clean handshake, auth rejected by `BatchMode=yes`. Expected path.
- `class=clean-login` → ssh exit=0. Only if a real keyring matches;
  never seen in this harness because `BatchMode` skips passwords.
- `class=early-wedge` → ssh exit=255 + "Connection timed out during
  banner exchange". The pre-fix dominant failure.
- `class=late-wedge` → ssh exit=124 + "Permanently added". A
  slow-tail artefact in pre-fix samples; no longer observed.

## Cleanup follow-ups (optional, not blocking)

With `SCHEDULER.lock` IRQ-safe and the wake path using a single lock
acquisition, several diagnostic helpers are now unused:

1. `task_log_label` and `classify_exec_path` at the top of
   `kernel/src/task/scheduler.rs` are no longer referenced. They sit
   under the module's `#![allow(dead_code)]` gate. Delete in a
   cleanup pass, or keep gated behind a future diagnostic feature.
2. `block_current_unless_woken_until` (added in `a58d841`) is
   correct but unused — the IrqSafeMutex fix closes the early-wedge
   without needing a timeout-bounded block. Kept in-tree for any
   future consumer that actually needs it (e.g. a real
   `nanosleep(2)`).
3. `spin::Mutex` is used for many other kernel globals
   (PROCESS_TABLE, TCP_CONNS, several per-subsystem locks). None of
   them is presently ISR-callable, but an audit to confirm and
   document the invariant would be cheap now that `IrqSafeMutex` is
   on the shelf as the canonical ISR-safe primitive.

## Don't-touch list (still valid)

- `kernel/src/task/scheduler.rs` — the IrqSafeMutex wrapper and
  `enqueue_to_core` without_interrupts region are the closing fix.
- `kernel/src/net/tcp.rs` — wake path structurally correct; RFC-793
  duplicate-SYN retransmit arm is new and correct.
- H6/H8/H9 fixes in `userspace/sshd/src/session.rs`,
  `kernel/src/arch/x86_64/syscall/mod.rs::sys_poll`, and
  `userspace/async-rt` — real corrections.
- `arp::learn` in `kernel/src/net/dispatch.rs` and
  `USING_LEGACY_INTX` in `kernel/src/net/virtio_net.rs` —
  RFC-compliant, no regression.
- The `[h9-iox] / [h9-fo] / [h9-ww]` instrumentation in
  `userspace/sshd/src/session.rs` — kept for future regression
  diagnosis.
- `vfs_server` slow-only logging gate — unconditional version
  caused the security-floor regression.
- `sunset-local` — runner state machine is correct.

## Pointer into the regression doc

The full investigation log is in
`docs/appendix/scheduler-fairness-regression.md`. For the closeout
story specifically:

- Top-of-doc "Status" block and the last entry under "Status as of
  2026-04-21" cover the one-paragraph summary.
- §"Early-wedge: SCHEDULER.lock IRQ-safety fix (2026-04-21
  closeout)" is the full technical narrative plus the 15-run and
  60-run validation tables.
- §"Early-wedge: root cause is SCHEDULER.lock contention, NOT a
  QEMU issue" has the pcap evidence and wake_task hang fingerprint
  that pointed at the fix.
