# Scheduler-Fairness Investigation — Resume Prompt (post-H9 follow-up #7 + early-wedge pivot)

Handoff prompt for resuming the SSH late-wedge investigation in a fresh
Claude Code session. Copy the code block below into a new session on
this repo.

```
Continue the SSH late-wedge investigation on branch
feat/phase-55b-ring-3-driver-host. Read docs/appendix/scheduler-fairness-regression.md
first — it contains the full 44-run experiment log, the H1–H9
hypothesis history, seven H9 follow-ups, and the current state of
what's been fixed vs. what's open.

Short version of where I left off (3 commits already pushed):

- 2691867 fix(net+sshd): land H6 + H8 + H9-partial fixes for SSH late-wedge
- c5ee209 chore(regression): bump security-floor su-user timeout 30s -> 60s
- 41bb341 fix(vfs_server): gate per-request log to slow-only

Real fixes already in place — DO NOT REVERT:

- H8 fix in kernel/src/arch/x86_64/syscall/mod.rs::sys_poll. Restructured
  the positive-timeout loop to register waiters once on entry, reset the
  per-iteration `woken` flag, and deregister exactly once on exit.
  General correctness fix for every positive-timeout poll() caller.
- H6 fix in userspace/sshd/src/session.rs::io_task. Gates
  set_output_waker on output_buf being drained, suppresses the kHz
  wake-ping-pong between io_task and sunset's Runner::progress().
- H9 partial fix — new async_rt::yield_now() primitive
  (userspace/async-rt/src/yield.rs) plus cooperative yields in
  progress_task's Continue / LoopContinue / Yield arms. Eliminates the
  ~630ms userspace cpu-hog burst observed post-hostkeys.
- vfs_server now logs only slow requests (>= 50ms). The unconditional
  per-request log was adding ~24 syscalls per IPC, regressed
  security-floor.

Branch-local instrumentation that's still in the tree (use it, don't
remove it):

- [tcp-wake] in kernel/src/net/mod.rs — TCP wake counter
- [sched] sshd fork-child dispatch / switch-out logs in
  kernel/src/task/scheduler.rs
- [sched] wake_task[h9] branch-tag log (blocked-to-ready /
  noop-not-blocked / wake-after-switch / missing-task-id)
- [h9-block-on] / [h9-tasks] / [h9-spawn] / [h9-sources] in
  userspace/async-rt/src/executor.rs
- per-Notify and per-Mutex wake-source counters in
  userspace/async-rt/src/sync/{notify.rs,mutex.rs}
- TaskHeader::wake_count in userspace/async-rt/src/task.rs
- [h9-io] / [h9-pn] / [h9-postkey] in userspace/sshd/src/session.rs
  (ContinueProbe arm dumps input_ready / out_empty post-hostkeys)

Wedge rate dropped from ~30% baseline to ~8% post-fix; SSH completes
the handshake in the great majority of runs. The remaining ~8% wedge
fingerprint:

- progress:event hostkeys count=1 → progress_task:continue count=1 →
  progress:event none count=1 → progress_task:yield count=1 →
  progress_task:wait progress_notify count=1
- [h9-postkey] input_ready=1 out_empty=0  (runner state is CORRECT,
  identical to clean runs)
- io_task slot 0 wake_count goes 3 → 4 (one wake post-hostkeys), then
  STAYS at 4
- io_task ran exactly 5 outer-loop iterations during the 30s ssh
  timeout window (with [h9-io] gate at <= 30 || %50, iter=6..30 would
  log if reached — none do)
- Both progress_task and io_task park cleanly. Nothing wakes io_task
  back up to drain pending input or generate fresh output.

The chain back to io_task should be one of:
1. runner.wake() from inside the next progress_task::progress() call
   fires output_waker when is_output_pending() becomes true. If the
   next progress() call doesn't generate fresh output, no fire.
2. The kernel reactor noticing socket POLLIN (incoming TCP from the
   client) and firing the read_waker via the userspace reactor.
   Requires block_on to reach step 4 and call reactor.poll_once(100)
   so the kernel-side waiter is registered.

Reproduce:

  cargo xtask run > run.log 2>&1 &
  # wait for "sshd: listening on port 22" in run.log
  timeout 30 ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o BatchMode=yes -o ConnectTimeout=20 -p 2222 user@127.0.0.1 'exit'

ssh exit=255 + "Connection timed out during banner exchange" = early-wedge.
ssh exit=255 + "Permission denied" = clean (no wedge — auth rejected
because BatchMode has no password).
ssh exit=124 + "Permanently added [127.0.0.1]:2222" = late-wedge.

The late-wedge is rare now (~1 in 8-12 runs). May need 10+ runs to
catch one.

H9 follow-up #7 already landed the inner-step instrumentation in
userspace/sshd/src/session.rs and ran 12 repros (h9run45–h9run56).
Result: 5 clean / 7 early-wedge / **0 late-wedge**. Instrumentation
works — clean-run fingerprint from iter=5 to iter=8 is now fully
documented in §H9 follow-up #7 of scheduler-fairness-regression.md —
but no late-wedge was caught to feed the three sub-hypotheses. The
observer effect from the additional write() syscalls per iteration
probably shifted the distribution toward early-wedge (7/12 = 58 %, top
of prior samples).

Active branch-local instrumentation (all tagged, low-risk to keep):

- `[h9-iox] iter=N step=…` — outer-loop step trace over io_task's
  entire body (top/flush_a_begin/flush_a_done/feed_begin/feed_input/
  feed_done/sw_begin/sw_done/wait_begin/wait_done/flush_b_begin/
  flush_b_done/read/read_input/iter_end).
- `[h9-fo] entry / lock_acquired / write_begin / write_done / exit
  (ok|err) bytes=…` — entry/exit of flush_output_locked for every
  caller.
- `[h9-ww] fd=F events=E register|ready reg=0|1|ready_on_register`
  on WaitWake::poll.

Next investigation step (H9 follow-up #8):

(1) Repeat the 12-run sample with the inner-step instrumentation
already in the tree. At ~8 % baseline × 12 runs = ~60 % catch
probability, so a second batch has a decent chance of a late-wedge.
If still 0/12, the observer effect hypothesis is strongly supported.

(2) If (1) yields 0/12 again, trim the instrumentation to reduce
syscall overhead. Prime candidate: the `[h9-ww]` per-poll log fires
on every WaitWake re-poll, including spurious ones; drop it to
register-only (no `ready` log), or gate it to first-poll-per-iter.
Keep `[h9-iox]` and `[h9-fo]`.

When a late-wedge IS captured, the three sub-hypotheses to read out of
the fingerprint are (unchanged from the follow-up #6 final mechanism
statement):

  (a) iter parks and never wakes. Last line is
      `iter=K step=wait_begin` + `[h9-ww] ... register`, then silence
      (no `ready reg=1`, no `wait_done`). → investigate runner.wake()
      in sunset-local/src/runner.rs, and the reactor POLLIN wake
      delivery in async-rt.
  (b) iter suspends inside flush. Last line is `[h9-fo] write_begin
      chunk=N` with no matching `write_done`. Would falsify follow-up
      #6 Finding 1 (TCP write never returns EAGAIN).
  (c) iter suspends inside runner.lock().await. Last line is any
      `iter=K step=X` immediately before a mutex-await call. Very
      unlikely — mutex_handoff=0 across every prior wedge.

Don't touch:
- kernel/src/net/tcp.rs — already audited, wake path is structurally
  correct.
- The H6/H8/H9 fixes already in session.rs / syscall/mod.rs / async-rt
  — they're real corrections.
- The existing [h9-iox]/[h9-fo]/[h9-ww] instrumentation from
  follow-up #7 — it is the only way to see the three sub-hypotheses.
- vfs_server logging gate — the unconditional version caused a
  separate security-floor regression.
- sunset-local — the runner state machine is correct (h9-postkey
  proves runner is ready post-hostkeys); the wedge is in the wake
  chain into io_task, not in sunset's protocol layer.

Two separate open bugs beyond the late-wedge:

1. Early-wedge — SYN reaches the listener (waiters=1 on tcp-wake#1)
   but no fork-child is ever spawned. Sshd parent (pid=3) doesn't
   accept. Heavily dominates the current wedge distribution (7/12 in
   the follow-up #7 batch). Different bug from the late-wedge; the
   H9 instrumentation doesn't apply (no session child exists).
   Incidental observation from follow-up #7: `[sched] wake_task ...
   name=net ...` count is 0–1 in early-wedges vs 10–12 in clean
   runs — a single-number classifier.
2. Whatever residual mechanism keeps the late-wedge happening at
   ~8% rate after all four fixes. The current data points to
   "io_task doesn't wake back up after its one post-hostkeys
   iteration" — see "Next investigation step" above.

Early-wedge state after pivot work (h9run57–98, 42 runs):
- Correctness fix 1 — passive `arp::learn` in `net/dispatch.rs` —
  landed. RFC-compliant, no regression. Did not change the wedge
  rate; the ARP-miss hypothesis was wrong.
- Correctness fix 2 — duplicate-SYN retransmit arm in
  `net/tcp.rs::handle_segment` — landed. Never fires in observed
  wedges because the client never retransmits SYN. Kept as
  belt-and-suspenders for clients that do retransmit.
- Mechanism narrowed: in `net_wakes=1` wedges (half of wedges), the
  guest sends SYN-ACK successfully, QEMU SLIRP receives it and sends
  back ACK, but **the ACK never reaches guest virtio-net**. No IRQ
  fires for subsequent packets; `net_task` stays blocked; guest's
  TCP stays in SynReceived.
- Two watchdog attempts (ISR-driven and task-context) both failed —
  the former with same-CPU `SCHEDULER.lock` deadlock in the timer
  ISR; the latter by starving boot via busy-yield when the watchdog
  is the only Ready task. The safe shape requires a new
  block-with-timeout scheduler primitive.

Budget: if 12 more runs produce 0 late-wedges, either trim
instrumentation (point 2 above) or pivot to the early-wedge, which
now dominates the sample. The early-wedge fix requires a scheduler
primitive (`block_current_until`) — design that before making another
attempt.
```

## If you want to skim the doc first

The full investigation log is in
`docs/appendix/scheduler-fairness-regression.md`. The most relevant
sections for picking up are:

- The top-of-doc "Status as of …" bullet summary (covers H6 / H8 /
  H9 / H9-followups #1–#6 + the early-wedge).
- §H9 follow-up #5: post-hostkeys runner state probe — establishes
  that input_ready/out_empty are identical in clean and wedge runs.
- §H9 follow-up #6: cooperative-yield fix tested — describes the
  current best-understanding mechanism statement and why the
  POLLOUT-wake hypothesis from #5 was structurally disproven.
- §H9 follow-up #7: io_task inner-step instrumentation landed +
  12-run sample (h9run45–h9run56), 0 late-wedges caught; includes
  the fully documented clean-run fingerprint from iter=5 to iter=8.
- The 56-run experiment log table at the end of the §H9 section —
  matches run-IDs to outcomes and instrumentation deltas.

## What's already committed and pushed

```
git log --oneline origin/feat/phase-55b-ring-3-driver-host -5
# 41bb341 fix(vfs_server): gate per-request log to slow-only, fixing security-floor flake
# c5ee209 chore(regression): bump security-floor su-user timeout 30s -> 60s
# 2691867 fix(net+sshd): land H6 + H8 + H9-partial fixes for SSH late-wedge
# adcb855 docs(appendix): record scheduler fairness regression starving net_task
# de6f0d3 fix(net/tcp): release TCP_CONNS before sending outbound segments
```

No further commit is needed before resuming — the working tree is
clean (only `.codex/` untracked, which is gitignored).
