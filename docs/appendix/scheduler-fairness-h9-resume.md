# Scheduler-Fairness Investigation — H9 Resume Prompt

Handoff prompt for resuming the SSH scheduler-fairness investigation in a
fresh Claude Code session. Copy the code block below into a new session on
this repo.

```
Continue the SSH scheduler-fairness investigation on branch
feat/phase-55b-ring-3-driver-host. Read docs/appendix/scheduler-fairness-regression.md
first — it contains the full 14-run experiment log, the H1–H9 hypothesis
history, and the current state of what's been fixed vs. what's open.

Short version of where I left off:

- H6 patch applied in userspace/sshd/src/session.rs:328-351 (gate
  set_output_waker on non-empty output_buf). Suppresses the kHz
  ping-pong between io_task and sunset's Runner::progress(). Real
  improvement, does not close the wedge.
- H8 fix applied in kernel/src/arch/x86_64/syscall/mod.rs sys_poll.
  Restructured to register waiters once, reset `woken` per iteration,
  deregister at exit. Previously the positive-timeout branch
  deregistered before yield_now, losing any wake that arrived during
  the yield window. General correctness fix for every positive-timeout
  poll() caller.
- [tcp-wake] instrumentation in kernel/src/net/mod.rs
  wake_sockets_for_tcp_slot: logs call count, tcp_idx, matched sockets,
  and waiter count on every wake. Confirmed H8 (pre-fix waiters=0) and
  confirmed the H8 fix (post-fix waiters=1 on the first SYN).
- The late-wedge *still reproduces* even with both H6 and H8 fixed.
  See §H9 in the doc. In fix7.log the wake is delivered correctly
  (call#9 waiters=1) but pid=14 does not make SSH-protocol progress
  afterward. The scheduler log shows it at cycles=1 state=Running
  reenqueue_after_yield=true, then silent. Open question: is pid=14
  being dispatched many times silently (below the cycles%1000==0 log
  gate) or not dispatched at all?
- The early-wedge is a separate bug — SYN never reaches handle_tcp
  (0 [tcp-wake] calls, no [tcp] connection established). Not addressed
  by H6/H8.

Reproduce:

  cargo xtask run > run.log 2>&1 &
  # wait for "sshd: listening on port 22" in run.log
  timeout 30 ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o BatchMode=yes -o ConnectTimeout=20 -p 2222 user@127.0.0.1 'exit'

ssh exit=255 + "Connection timed out during banner exchange" = early-wedge.
ssh exit=255 + "Permission denied" = clean (no wedge — auth rejected
because BatchMode has no password).
ssh exit=124 + "Permanently added [127.0.0.1]:2222" = late-wedge.

Next investigation step (H9):

1. Add a per-task dispatch counter to kernel/src/task/scheduler.rs that
   increments on EVERY dispatch (not just sshd-fork-child), and log it
   at every cycles%100==0 or similar — the current 1000 gate is hiding
   slow dispatch. Purpose: determine whether pid=14 is being dispatched
   silently or not at all after its first 290ms burst.
2. Add a block_on iteration counter in userspace/async-rt/src/executor.rs.
   Log every 10k iterations with run_queue.len() and
   root_header.is_woken(). Purpose: distinguish "pid=14 is spinning in
   its executor" from "pid=14 is not running at all."
3. Add a log to scheduler::wake_task showing the branch taken
   (Blocked→Ready, no-op on Running, missing task id). The wake from
   wake_sockets_for_tcp_slot may be hitting the "already Running" no-op
   branch and the scheduler isn't giving pid=14 a fair share on core 0.

Don't touch:
- kernel/src/net/tcp.rs — already audited, wake path is structurally
  correct (see doc §H6 / §H8 appendix code references).
- The existing H6 and H8 fixes in session.rs and syscall/mod.rs — keep
  them; they are real corrections even if they don't close the remaining
  wedge.

Budget: if you can't isolate H9 in ~5 runs of instrumentation + analysis,
stop and report. The early-wedge is also open and may be a better use
of time if H9 stays murky.
```

## If you want to commit the current state first

Files to stage:

```
docs/appendix/scheduler-fairness-regression.md
docs/appendix/scheduler-fairness-h9-resume.md
kernel/src/arch/x86_64/syscall/mod.rs
kernel/src/net/mod.rs
userspace/sshd/src/session.rs
# plus the pre-existing branch-local instrumentation already modified:
kernel/src/main.rs
kernel/src/net/virtio_net.rs
kernel/src/process/mod.rs
kernel/src/task/mod.rs
kernel/src/task/scheduler.rs
userspace/async-rt/src/executor.rs
userspace/async-rt/src/task.rs
userspace/sshd/src/main.rs
userspace/vfs_server/src/main.rs
```

Do NOT stage: `baseline*.log fix*.log h8run*.log patched*.log ssh*.log output.prev.txt .codex .claude/scheduled_tasks.lock` — runtime artifacts. Add `*.log` and `output*.txt` to `.gitignore` if not already present.

Suggested commit message:

```
fix(kernel): sys_poll no longer loses wakes during yield + SSH late-wedge diagnosis

H8 root cause — sys_poll's positive-timeout branch was deregistering all
waiters before yield_now(), so TCP wakes arriving during the yield window
hit empty WaitQueues and were silently lost. Fix restructures the loop
to register waiters once on entry, reset the per-iteration woken flag at
the top of each pass, and deregister once on exit. General correctness
fix for every positive-timeout poll() caller, not just sshd.

H6 patch — gate io_task's set_output_waker on non-empty output_buf in
userspace/sshd/src/session.rs. Suppresses the kHz wake-ping-pong between
sunset's Runner::progress() and sshd's io_task. Semantic improvement
that does not close the late-wedge on its own.

Instrumentation — [tcp-wake] log in wake_sockets_for_tcp_slot shows how
many waiters are present when a TCP segment delivery fires the wake.
Kept throttle-friendly for ongoing H9 investigation.

Full 14-run evidence table, H6/H8/H9 hypothesis history, and remaining
investigation items captured in docs/appendix/scheduler-fairness-regression.md.
```
