# Scheduler-Fairness Investigation — Resume Prompt (early-wedge fix landed)

Handoff prompt for resuming the SSH wedge investigation in a fresh
Claude Code session. Copy the code block below into a new session on
this repo.

```
Continue the SSH wedge investigation on branch
feat/phase-55b-ring-3-driver-host. Read docs/appendix/scheduler-fairness-regression.md
first — it contains the full multi-session experiment log, the H1–H9
hypothesis history, seven H9 follow-ups, the early-wedge pivot, the
corrected early-wedge root-cause analysis backed by QEMU pcap
evidence, and (most recently) the SCHEDULER.lock IRQ-safety fix that
deterministically closes the early-wedge.

Short version of where the investigation stands:

- Early-wedge (the dominant ~60 % failure mode): **FIXED.** Root cause
  was SCHEDULER.lock being acquired with interrupts on, letting
  virtio-net / virtio-blk ISRs re-enter wake_task on the same CPU
  while a task-context holder spun on the lock. Fix converts
  `SCHEDULER: Mutex<Scheduler>` to `SCHEDULER: IrqSafeMutex<Scheduler>`
  in kernel/src/task/scheduler.rs, wraps enqueue_to_core's run_queue
  acquisition in `without_interrupts`, and folds wake_task's
  redundant second SCHEDULER.lock (and PROCESS_TABLE.lock via
  task_log_label) into the single first critical section. 15-run
  validation: 15/15 clean-auth-rejected (100 %) vs 30-40 % pre-fix
  baseline — no heavy-logging mitigation needed. See §"Early-wedge:
  SCHEDULER.lock IRQ-safety fix (2026-04-21 closeout)" in
  scheduler-fairness-regression.md.
- Late-wedge (~8 % pre-fix, 0/15 in the validation sample): open but
  possibly subsumed. May have been a tail mis-classification of the
  early-wedge. Needs a larger sample (50-100 runs) to confirm
  whether it is real or noise below the validation threshold.

Commits already pushed on this branch (most recent first):

- <new> fix(sched): make SCHEDULER.lock IRQ-safe to close early-wedge
  (the fix itself, with validation numbers)
- fc67213 fix(virtio-net): gate ISR_STATUS read on legacy INTx
- a58d841 feat(sched): add block_current_unless_woken_until primitive
- 4823ec1 docs(appendix): record H9 follow-up #7 + early-wedge pivot
- eb078bc fix(net): passive ARP learning + RFC-793 duplicate-SYN retransmit
- 68ccd91 debug(sshd): add io_task inner-step instrumentation (H9 follow-up #7)
- 2691867 fix(net+sshd): land H6 + H8 + H9-partial fixes
- c5ee209 chore(regression): bump security-floor su-user timeout 30s -> 60s
- 41bb341 fix(vfs_server): gate per-request log to slow-only
- de6f0d3 fix(net/tcp): release TCP_CONNS before sending outbound segments

Real fixes in place — DO NOT REVERT:

- **SCHEDULER.lock IRQ-safety** in kernel/src/task/scheduler.rs
  (`IrqSafeMutex<Scheduler>` + `enqueue_to_core` without_interrupts +
  wake_task single-lock path). This is the early-wedge fix.
- H8 fix in kernel/src/arch/x86_64/syscall/mod.rs::sys_poll. Restructured
  the positive-timeout loop to register waiters once on entry, reset the
  per-iteration `woken` flag, and deregister exactly once on exit.
- H6 fix in userspace/sshd/src/session.rs::io_task. Gates
  set_output_waker on output_buf being drained.
- H9 partial fix — async_rt::yield_now() + cooperative yields in
  progress_task's Continue / LoopContinue / Yield arms.
- vfs_server slow-only request log (security-floor regression fix).
- Passive ARP learning in kernel/src/net/dispatch.rs (RFC-compliant).
- RFC-793 duplicate-SYN retransmit arm in kernel/src/net/tcp.rs.
- MSI-X ISR_STATUS read gated on USING_LEGACY_INTX.
- block_current_unless_woken_until scheduler primitive
  (kernel/src/task/{mod,scheduler}.rs). Unused after the IrqSafeMutex
  fix but kept for future consumers.

Active branch-local instrumentation (keep, don't remove):

- kernel-side: [tcp-wake], [sched] sshd fork-child dispatch/switch-out,
  [sched] wake_task[h9] branch tags.
- async-rt: [h9-block-on], [h9-tasks], [h9-spawn], [h9-sources] in
  executor.rs; per-Notify/Mutex wake-source counters; TaskHeader::wake_count.
- sshd session.rs: [h9-io], [h9-pn], [h9-postkey], [h9-iox], [h9-fo],
  [h9-ww] step traces.

===========================================================================
Reproduction harness
===========================================================================

  /tmp/h9_run_once.sh <run-id>
      → Single-shot boot + ssh attempt. Produces
        /tmp/h9run<id>.{log,ssh,summary}.
  /tmp/h9_batch.sh <count> <prefix>
      → Run /tmp/h9_run_once.sh count times with RUN_ID=<prefix><i>.
        Tallies class= counts at the end.

Outcomes (from the summary file):
- class=clean-auth-rejected → ssh exit=255 + "Permission denied".
  Clean handshake, auth rejected by BatchMode=yes. Expected path.
- class=clean-login → ssh exit=0 (only if a real keyring matches;
  never seen in this harness because BatchMode skips passwords).
- class=early-wedge → ssh exit=255 + "Connection timed out during
  banner exchange". Wedge before or during key exchange.
- class=late-wedge → ssh exit=124 + "Permanently added". Wedge
  after key exchange, during session or post-auth.

===========================================================================

Next investigation step (pick one — both are legitimate):

OPTION A — Late-wedge confirmation (recommended):

Run /tmp/h9_batch.sh 60 latewedgeA (approx 60-90 min). If the result
is still 60/60 clean, the late-wedge was tail mis-classification and
the appendix can be closed as fully resolved. If any late-wedge is
captured, the H9 follow-up #7 instrumentation in session.rs pins the
sub-hypothesis (wake-chain broken / stuck inside flush / stuck inside
runner.lock) directly from the last logged step.

OPTION B — Scheduler architecture cleanup:

With SCHEDULER.lock now IRQ-safe, several lock-holding diagnostic
paths are candidates for simplification:

1. `task_log_label` / `classify_exec_path` at the top of scheduler.rs
   are now unreferenced. Delete or keep gated under a diagnostic
   feature. Currently preserved under the module's
   #![allow(dead_code)] gate.
2. The PROCESS_TABLE.lock calls inside the dispatch loop
   (scheduler.rs lines 1823, 1879, 2017) run in task context so
   they are safe, but they sit inside an IF-off region (SCHEDULER
   is IF-off by the IrqSafeMutex wrapper). A fast-path that avoids
   PROCESS_TABLE.lock when possible — for example, caching the
   address-space pointer in the task itself — would reduce the IF-
   off window on dispatch. Not a correctness fix; a latency
   improvement.
3. `spin::Mutex` is used for many other globals (PROCESS_TABLE,
   TCP_CONNS, etc.). None of them is presently ISR-callable, but
   an audit to confirm that and document the invariant would be
   cheap. The primitive is already on the shelf (`IrqSafeMutex`).

Budget: Option A is a single overnight batch. Option B is a half-
day of reading and refactoring. Option A is more valuable because
it answers "is the late-wedge a real remaining bug?" definitively.

===========================================================================

Don't touch:

- kernel/src/task/scheduler.rs — SCHEDULER.lock IRQ-safety is the
  closing fix for the early-wedge; do not revert the IrqSafeMutex
  wrapper or the enqueue_to_core without_interrupts region.
- kernel/src/net/tcp.rs — already audited, wake path is structurally
  correct; handle_segment duplicate-SYN arm is new and correct.
- H6/H8/H9 fixes in session.rs / syscall/mod.rs / async-rt — real
  corrections.
- The block_current_unless_woken_until primitive — works correctly,
  kept on the shelf for future consumers; net_task does not use it.
- arp::learn in net/dispatch.rs and USING_LEGACY_INTX in
  virtio_net.rs — RFC-compliant, no regression.
- The existing [h9-iox] / [h9-fo] / [h9-ww] instrumentation — the
  only way to read out the three late-wedge sub-hypotheses.
- vfs_server logging gate — the unconditional version caused the
  security-floor regression.
- sunset-local — the runner state machine is correct (h9-postkey
  proves runner is ready post-hostkeys).
```

## If you want to skim the doc first

The full investigation log is in
`docs/appendix/scheduler-fairness-regression.md`. The most relevant
sections for picking up are:

- The top-of-doc "Status as of …" bullet summary (includes the
  2026-04-21 closeout entry).
- §"Early-wedge: SCHEDULER.lock IRQ-safety fix (2026-04-21 closeout)"
  — root cause, fix design, and 15-run validation results.
- §"Early-wedge: root cause is SCHEDULER.lock contention, NOT a QEMU
  issue" — the pcap evidence and wake_task hang fingerprint that
  pointed at the fix.
- §"Early-wedge: block-with-timeout primitive landed" — scheduler
  primitive kept on the shelf (unused after the IrqSafeMutex fix).
- §"H9 follow-up #7: io_task inner-step instrumentation landed" —
  the late-wedge instrumentation that stays in-tree ready for a
  50-100-run confirmation pass.

## What's already committed

```
git log --oneline origin/feat/phase-55b-ring-3-driver-host -10
# <new> fix(sched): make SCHEDULER.lock IRQ-safe to close early-wedge
# fc67213 fix(virtio-net): gate ISR_STATUS read on legacy INTx
# a58d841 feat(sched): add block_current_unless_woken_until primitive
# 4823ec1 docs(appendix): record H9 follow-up #7 + early-wedge pivot findings
# eb078bc fix(net): passive ARP learning + RFC-793 duplicate-SYN retransmit
# 68ccd91 debug(sshd): add io_task inner-step instrumentation (H9 follow-up #7)
# 41bb341 fix(vfs_server): gate per-request log to slow-only
# c5ee209 chore(regression): bump security-floor su-user timeout 30s -> 60s
# 2691867 fix(net+sshd): land H6 + H8 + H9-partial fixes for SSH late-wedge
# adcb855 docs(appendix): record scheduler fairness regression starving net_task
```

Working tree is clean (only `.codex/` untracked, which is gitignored).
