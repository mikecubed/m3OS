# Scheduler-Fairness Investigation — Resume Prompt (post-early-wedge root-cause pivot)

Handoff prompt for resuming the SSH wedge investigation in a fresh
Claude Code session. Copy the code block below into a new session on
this repo.

```
Continue the SSH wedge investigation on branch
feat/phase-55b-ring-3-driver-host. Read docs/appendix/scheduler-fairness-regression.md
first — it contains the full multi-session experiment log, the H1–H9
hypothesis history, seven H9 follow-ups, the early-wedge pivot, and
(most recently) the corrected early-wedge root-cause analysis backed
by QEMU pcap evidence.

Short version of where I left off (commits already pushed):

- fc67213 fix(virtio-net): gate ISR_STATUS read on legacy INTx
- a58d841 feat(sched): add block_current_unless_woken_until primitive
- 4823ec1 docs(appendix): record H9 follow-up #7 + early-wedge pivot
- eb078bc fix(net): passive ARP learning + RFC-793 duplicate-SYN retransmit
- 68ccd91 debug(sshd): add io_task inner-step instrumentation (H9 follow-up #7)
- 2691867 fix(net+sshd): land H6 + H8 + H9-partial fixes
- c5ee209 chore(regression): bump security-floor su-user timeout 30s -> 60s
- 41bb341 fix(vfs_server): gate per-request log to slow-only
- de6f0d3 fix(net/tcp): release TCP_CONNS before sending outbound segments

Real fixes already in place — DO NOT REVERT:

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
- MSI-X ISR_STATUS read gated on USING_LEGACY_INTX (removes a
  transitional-virtio quirk where reading ISR_STATUS in MSI-X mode
  can suppress the next MSI-X edge).
- block_current_unless_woken_until scheduler primitive
  (kernel/src/task/{mod,scheduler}.rs). Fast-path gated by
  ACTIVE_WAKE_DEADLINES counter so unused-case cost is zero. Kept
  for future consumers; net_task uses indefinite block because the
  200-ms defensive poll measurably hurt clean rate.

Active branch-local instrumentation (keep, don't remove):

- kernel-side: [tcp-wake], [sched] sshd fork-child dispatch/switch-out,
  [sched] wake_task[h9] branch tags.
- async-rt: [h9-block-on], [h9-tasks], [h9-spawn], [h9-sources] in
  executor.rs; per-Notify/Mutex wake-source counters; TaskHeader::wake_count.
- sshd session.rs: [h9-io], [h9-pn], [h9-postkey], [h9-iox], [h9-fo],
  [h9-ww] step traces.

===========================================================================
IMPORTANT — the "it's a QEMU issue" conclusion was WRONG.
===========================================================================

An earlier doc iteration claimed the net_wakes=1 early-wedge was caused
by QEMU SLIRP dropping ACKs. A QEMU `filter-dump,netdev=net0` pcap
taken during a wedge PROVED otherwise:

- Client sends SYN — reaches the guest NIC.
- Guest sends SYN-ACK — visible on the wire.
- Client sends ACK — reaches the guest NIC.
- Client sends 43-byte SSH banner — reaches the guest NIC.
- Client retransmits the banner 4 times over ~20 s — all reach the NIC.

Every packet arrives. The three-way handshake completes from the wire's
perspective. QEMU is NOT the problem.

===========================================================================

Real mechanism — SCHEDULER.lock contention in wake_task:

With serial-log tracing added through the tcp-wake path
(net::wake_sockets_for_tcp_slot → wake_socket → WaitQueue::wake_all →
scheduler::wake_task), every wedge reliably lands on:

    [tcp-wake] call#1 sockets_matched=1 waiters=1 ...
    [tcp-wake] call#1 wake_socket h=0 begin
    [wq] wake_all: lock attempt
    [wq] wake_all: lock acquired, len=1
    [wq] wake_all: lock released, waking 1
    [wq] wake_all: waking task id=N      <-- sshd parent task
    <silence>

net_task (on core 2) calls wake_task(sshd_parent) from inside the
tcp-wake path. The call hangs on the first SCHEDULER.lock() acquisition
inside wake_task. Something else is holding SCHEDULER.lock and never
releasing.

Separately, `[sched] stale-ready` warnings fire with 650-940 ms of
staleness on core 0 in these wedges, confirming core 0 is stuck too.

Heavy-logging interference fingerprint: adding per-step log::info!
calls to wake_task, wake_all, the dispatch loop pre-pick_next path,
and the virtio-net ISR raises the ssh clean rate from ~30 % to ~80 %
(10-run sample h9run177–h9run186: 8 clean / 2 early-wedge). Reverting
the extra logs returns the clean rate to ~30 %. That log-sensitivity
is the classic fingerprint of a tight lock-contention race, NOT a
deterministic deadlock. The heavy-logging mitigation is NOT a
real fix — it's a data point about the race.

Reproduce:

  cargo xtask run > run.log 2>&1 &
  # wait for "sshd: listening on port 22" in run.log
  timeout 30 ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o BatchMode=yes -o ConnectTimeout=20 -p 2222 user@127.0.0.1 'exit'

The diagnostic harness /tmp/h9_run_once.sh is reusable — numbered run
IDs, per-run .log/.ssh/.summary files. Clean rate is ~30-40 % baseline,
~80 % with heavy logs.

Outcomes:
- ssh exit=255 + "Permission denied" = clean (auth rejected, no wedge).
- ssh exit=255 + "Connection timed out during banner exchange" = early-wedge.
- ssh exit=124 + "Permanently added" = late-wedge (rare, ~8 %).

Early-wedge sub-variants (from net_wakes column in scheduler log):
- net_wakes=0: SYN never arrives (different bug, likely QEMU-user-mode
  hostfwd race at first-packet; separate from SCHEDULER.lock contention).
- net_wakes=1: SYN arrives, SYN-ACK sent, tcp-wake#1 fires, wake_task
  hangs. This is the SCHEDULER.lock contention case. Dominates.

===========================================================================

Late-wedge (~8 %) is still open — still dominated by early-wedge, so
less urgent. H9 follow-up #7 instrumentation ([h9-iox] / [h9-fo] /
[h9-ww]) is live in userspace/sshd/src/session.rs. 12-run sample
(h9run45–56) caught 0 late-wedges — probably observer effect from
~12-20 extra write() syscalls per io_task iteration.

When a late-wedge IS captured, the three sub-hypotheses to read out
of the fingerprint (from follow-up #6's final mechanism statement):

  (a) iter parks and never wakes. Last line is
      `iter=K step=wait_begin` + `[h9-ww] ... register`, then silence
      (no `ready reg=1`, no `wait_done`). → investigate runner.wake()
      in sunset-local/src/runner.rs and the reactor POLLIN wake
      delivery in async-rt.
  (b) iter suspends inside flush. Last line is `[h9-fo] write_begin
      chunk=N` with no matching `write_done`.
  (c) iter suspends inside runner.lock().await. Last line is any
      `iter=K step=X` immediately before a mutex-await call.
      Contradicted by mutex_handoff=0 across every prior wedge.

===========================================================================

Next investigation step (pick one — both are legitimate):

OPTION A — Fix the early-wedge (DOMINANT, ~60 % of failures):

The SCHEDULER.lock contention race needs an audit. Plausible fixes in
rough order of invasiveness:

1. Wrap SCHEDULER.lock acquisitions in `without_interrupts` so a
   same-core ISR cannot re-enter wake_task while a task holds the
   lock. Same pattern that DRIVER.lock uses in virtio_net.rs. Highest
   probability fix; also the highest-blast-radius change. Audit every
   SCHEDULER.lock site first (62 in scheduler.rs alone — see
   `grep -c 'SCHEDULER.lock()' kernel/src/task/scheduler.rs`).
2. Reduce SCHEDULER.lock acquisition count per dispatch cycle.
   Currently: pre-pick_next scan, pick_next itself, switch-out
   handling — three acquisitions per dispatch per core.
3. Move wake_task's second SCHEDULER.lock (for logging) out of the
   hot path. It's purely diagnostic; could read once into an
   Arc-shared snapshot.
4. Convert wake_task into a two-phase operation: an ISR-safe
   "queue wake request" step (no lock) plus a deferred "apply wakes"
   step in the dispatch loop (already has SCHEDULER.lock).

Validate any fix by running /tmp/h9_run_once.sh 10-15 times WITHOUT
extra logging. Target: clean rate ≥ 80 %. Pre-fix baseline is 30-40 %.

OPTION B — Chase the late-wedge (RARE, ~8 %):

Re-run the 12-run sample with the existing [h9-iox]/[h9-fo]/[h9-ww]
instrumentation. If 0/12 again, trim [h9-ww] per-poll logging and
re-sample. When a late-wedge is captured, the three sub-hypotheses
above will read out directly from the last logged line.

===========================================================================

Don't touch:

- kernel/src/net/tcp.rs — already audited, wake path is structurally
  correct; handle_segment duplicate-SYN arm is new and correct.
- H6/H8/H9 fixes in session.rs / syscall/mod.rs / async-rt — real
  corrections.
- The block_current_unless_woken_until primitive — works correctly
  but don't consume it from net_task (tested, regresses clean rate).
- arp::learn in net/dispatch.rs and USING_LEGACY_INTX in
  virtio_net.rs — RFC-compliant, no regression.
- The existing [h9-iox] / [h9-fo] / [h9-ww] instrumentation — the
  only way to read out the three late-wedge sub-hypotheses.
- vfs_server logging gate — the unconditional version caused the
  security-floor regression.
- sunset-local — the runner state machine is correct (h9-postkey
  proves runner is ready post-hostkeys).

Budget: if 15 runs of Option A produce no clean-rate improvement,
step back and audit the SCHEDULER.lock sites systematically before
making another targeted fix. If 12 runs of Option B catch 0 wedges,
the late-wedge is probably noise-hidden by the early-wedge's high
rate — fix the early-wedge first.
```

## If you want to skim the doc first

The full investigation log is in
`docs/appendix/scheduler-fairness-regression.md`. The most relevant
sections for picking up are:

- The top-of-doc "Status as of …" bullet summary.
- §Early-wedge: root cause is SCHEDULER.lock contention, NOT a QEMU
  issue — the pcap evidence and wake_task hang fingerprint.
- §Early-wedge: block-with-timeout primitive landed — the scheduler
  primitive postmortem including why net_task doesn't use it.
- §H9 follow-up #7: io_task inner-step instrumentation landed +
  12-run sample, 0 late-wedges caught; full clean-run fingerprint
  from iter=5 to iter=8.
- The ~60-run experiment log table at the end of the §H9 section.

## What's already committed

```
git log --oneline origin/feat/phase-55b-ring-3-driver-host -10
# fc67213 fix(virtio-net): gate ISR_STATUS read on legacy INTx
# a58d841 feat(sched): add block_current_unless_woken_until primitive
# 4823ec1 docs(appendix): record H9 follow-up #7 + early-wedge pivot findings
# eb078bc fix(net): passive ARP learning + RFC-793 duplicate-SYN retransmit
# 68ccd91 debug(sshd): add io_task inner-step instrumentation (H9 follow-up #7)
# 41bb341 fix(vfs_server): gate per-request log to slow-only
# c5ee209 chore(regression): bump security-floor su-user timeout 30s -> 60s
# 2691867 fix(net+sshd): land H6 + H8 + H9-partial fixes for SSH late-wedge
# adcb855 docs(appendix): record scheduler fairness regression starving net_task
# de6f0d3 fix(net/tcp): release TCP_CONNS before sending outbound segments
```

Working tree is clean (only `.codex/` untracked, which is gitignored).
