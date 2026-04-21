# Scheduler Fairness Regression: SSH Session Spin / Core-0 Fairness Investigation

**Status:** Unfixed, partially narrowed — branch-local investigation updated on 2026-04-21.
**Severity:** High for interactive SSH and related userspace workloads. The VM
can hang at multiple points in the SSH path: before key exchange, during key
exchange, before the password prompt, after login, or on the first typed input.
**Discovered:** Debugging inbound SSH on `feat/phase-55b-ring-3-driver-host`
after resolving a separate TCP deadlock in `kernel/src/net/tcp.rs`
(`fix(net/tcp): release TCP_CONNS before sending outbound segments`,
commit `de6f0d3`).
**Exposed by:** A clean-boot `cargo xtask run` followed by a single inbound
TCP connection attempt on port 2222 (`ssh` or `nc`) against the default
virtio-net NIC. The same symptom reproduced with `--device e1000` once RX
delivery was separately validated.
**Related phase docs:**
[`52c-kernel-architecture-evolution`](../roadmap/52c-kernel-architecture-evolution.md)
already plans per-core scheduler evolution and work-stealing hardening; this
appendix is the concrete data point that belongs on its acceptance list.

---

## Summary

The original working theory for this appendix was "core-0 fairness starvation
keeps `net_task` from running." The investigation moved past that.

What is now established:

- The failure is **timing-dependent** and can appear at multiple phases of one
  SSH attempt: before host-key identity, during key exchange, before password
  auth, after password auth, after shell spawn, or immediately after the first
  typed input.
- The network wake path is **not** the leading suspect anymore in the virtio
  repros. `net_task` has wide affinity, successfully wakes, and continues to
  run on core 2 in many failing runs.
- A real bug was found and fixed in `userspace/async-rt`: tasks spawned while
  the executor was draining the run queue could be dropped before their first
  poll. This explained one concrete failure mode where `progress_task` logged
  `spawn relay` but `channel_relay_task` never started.
- Even after that fix, the VM can still hang with one or both of the SSH
  session process and shell child hot-yielding on core 0. The remaining bug is
  therefore **not just** the dropped-spawn bug.

The current evidence points to a broader SSH/session/PTY fairness problem on
core 0, with at least one confirmed userspace-runtime bug already fixed and at
least one timing-sensitive bug still present. The appendix should now be read
as an investigation log for that broader issue, not as proof that `net_task`
starvation is the root cause.

**Status as of 2026-04-21 (late evening):**

- H6 (wake-ping-pong): real bug, minimal patch applied to
  `userspace/sshd/src/session.rs`. Suppresses the kHz ping-pong but does
  not close the wedge on its own.
- H8 (missing socket-readability wake): real bug, **root cause found and
  fixed** in `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_poll`). The
  positive-timeout branch of `sys_poll` was deregistering all waiters
  before `yield_now()`, so wakes arriving during the yield window hit
  empty `WaitQueue`s and were silently lost. Fix restructures the loop
  to register waiters once, reset the `woken` flag per iteration, and
  deregister only at exit. Confirmed via `[tcp-wake]` instrumentation:
  post-fix, the first TCP segment arrives at `call#1` showing
  `waiters=1` (correctly registered) instead of the pre-fix `waiters=0`.
- H9 (narrowed 2026-04-21 night): the late-wedge after H6+H8 reproduces
  in two flavors that share the same root mechanism. Branch-local H9
  instrumentation (per-cycle log gate tightening, `wake_task[h9]`
  branch tagging, and a `block_on` iteration counter in async-rt)
  confirmed both via 4 fresh runs:
  - **Variant A (h9run3):** TCP wake fires with `waiters=1`, but
    `wake_task[h9]` reports `branch=noop-not-blocked
    prior_state=Running`. The kernel-side wake is no-op because pid=14
    sits in `Running` (the yield/reenqueue path leaves the task in
    `TaskState::Running`, not `Blocked*`). pid=14 *is* still dispatched
    after this — `cycles` advances to 2, 3, ... — but makes no further
    sshd-protocol progress.
  - **Variant B (h9run4):** All post-listener TCP wakes show
    `waiters=0`. `block_on` log shows `iter=1..6` with
    `run_queue.len() == 1`, then stops. The executor never reaches
    step 4 (the only path that calls the blocking
    `reactor.poll_once(100)` and registers waiters), because step 4 is
    gated on `run_queue.is_empty()`.
  Both flavors collapse to the same root: **at least one always-runnable
  spawned task in the session-child executor keeps the run queue
  non-empty, so `block_on` never reaches its blocking-poll step, no
  sys_poll waiter ever gets registered on the session socket, and TCP
  arrivals either land on an empty WaitQueue or on a Running task.**
  This is structurally H7's amplifier with a different upstream waker
  (no longer sunset's `Runner::wake()` after the H6 fix). Next step is
  to find the remaining persistent waker source inside
  `userspace/sshd/src/session.rs` (likely the `progress_task` Notify
  loop or io_task's WaitWake re-arm pattern). Investigation budget
  exhausted at 4 runs.
- H9 follow-up #2 (2 more runs, h9run8/h9run9): **wake source
  attributed to `Notify::signal`** (`mutex_handoff=0` throughout). Both
  `progress_notify` and `session_notify` are hot. The signals come
  from io_task on every successful `runner.input()` call, including
  the `Ok(0)` path. The wake loop is sustained by client TCP
  retransmissions during the wedge: each retransmitted segment fires
  the socket waitqueue → io_task wakes → io_task feeds runner →
  io_task signals → progress_task wakes → `progress()` returns
  `Event::None` → repeat. **Conclusion: H9 is no longer a kernel /
  async-rt fairness issue.** It is an **SSH protocol-layer issue**:
  sunset's `Runner` does not advance KEX after the application provides
  host keys, so no further server-side response is ever generated.
  Future work belongs in `sunset-local/`. The kernel-side and async-rt
  instrumentation in this branch is correct and useful as ongoing
  diagnostics, but a fix for the late-wedge will not come from changing
  it.
- Separate "early-wedge" variant: the SYN never reaches `handle_tcp`
  (0 `[tcp-wake]` calls for the entire run, no `[tcp] connection
  established`). Different bug, very likely a missed virtio-net IRQ at
  first-packet time or a QEMU user-mode hostfwd race. Not addressed by
  the H6 / H8 fixes.
- H9 follow-up #7 (2026-04-21, 12 runs h9run45–h9run56): io_task
  inner-step instrumentation landed (`[h9-iox]`, `[h9-fo]`, `[h9-ww]`
  in `userspace/sshd/src/session.rs`). **0 late-wedges captured** in
  12 runs; instrumentation validated against the clean-run path
  (full iter=5–8 trace documented). Probable observer effect — the
  extra per-iteration `write()` syscalls may have pushed the
  distribution toward early-wedge (7/12 = 58 %, top of prior
  samples). Instrumentation stays in-tree ready for the next
  sampling pass. The three sub-hypotheses (wake-chain broken /
  stuck inside flush / stuck inside runner.lock) remain
  uncaptured — any late-wedge captured with this instrumentation
  would pin exactly one of them.

H6's patch is still worth landing as a semantic improvement. The H8
fix is a correctness fix for every `sys_poll` caller with a positive
timeout, not just sshd. H9 (via follow-up #8 sampling) and the
early-wedge are the remaining diagnosis items.

---

## How to Reproduce

```bash
git checkout feat/phase-55b-ring-3-driver-host
cargo xtask run                     # default virtio-net NIC
# in another shell, once the boot log shows "sshd: listening on port 22":
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o BatchMode=yes -o ConnectTimeout=15 -p 2222 user@127.0.0.1
```

Either outcome reproduces the bug:

1. **Full silent wedge.** `ssh` times out during banner exchange. The serial
   log shows **no** `[tcp]` or `[arp]` lines at all. A `filter-dump` pcap
   on `netdev=net0` shows QEMU repeatedly ARPing `who-has 10.0.2.15 tell
   10.0.2.2` every ~5s with **zero replies from the guest**.
2. **Partial-progress wedge.** `ssh` gets further — the serial log shows
   `[tcp] SYN-ACK sent (passive open)`, `[tcp] connection established
   (passive)`, `sshd: generating new host key`, and a successful
   `fork() → child pid N` / `[pipe] created pipe_id=0`. Pcap shows a
   clean three-way handshake plus ~500 bytes of SSH payload sent from the
   guest. Then the serial log freezes and the peer retransmits unanswered
   for minutes before giving up.

Both outcomes appear to share the same scheduler/fairness failure mode —
they differ only in how far the guest progressed before the starvation
window opened.

### Observation instrumentation (one-off, not committed)

The branch-local diagnostics that produced the evidence in this doc:

| Change | Purpose |
|---|---|
| `kernel/src/serial.rs:134` — bump `LevelFilter::Info` → `Debug` | Unmask `[tcp] SYN-ACK sent (passive open)`, `[arp] sent reply to …`, `[pipe] created pipe_id=…`, and per-syscall `[open]/[execve]` debug lines. |
| `xtask/src/main.rs:~1680` — append `-object filter-dump,id=netdump0,netdev=net0,file=…/net0.pcap` | Wireshark-readable capture of every frame on the QEMU user-mode NIC. Proves whether a SYN ever reached the guest and whether the guest ever replied. |

Neither change is required to hit the wedge. They are what made the
diagnosis tractable. Re-apply locally if you need to re-run the
investigation.

### Follow-up instrumentation now landed locally

To close the earlier "did the wake reach `net_task`?" gap and then follow the
bug deeper into SSH/session handling, the current workspace now includes
low-overhead scheduler and userspace telemetry:

| Change | Purpose |
|---|---|
| `kernel/src/net/virtio_net.rs:465-490, 548-560` | Counts IRQ-side wake attempts, successful `wake_task` transitions, failed/no-op wake attempts, and IRQs that fired before `NET_TASK_ID` was registered. |
| `kernel/src/task/scheduler.rs:881-912, 1248-1297` | Exposes task debug snapshots (`pid`, `name`, `state`, `assigned_core`, `affinity_mask`, `last_ready_tick`, `last_migrated_tick`) and logs labeled wake transitions for `net`, `sshd`, `vfs_server`, and related tasks. |
| `kernel/src/main.rs:562-635` | Logs the `net_task` startup snapshot plus per-change wake summaries from task context, so ISR-side counters can be inspected without logging from the interrupt path. |
| `kernel/src/arch/x86_64/syscall/mod.rs` | Logs selective `fork()` / `execve()` events for the interactive path. |
| `kernel/src/process/mod.rs`, `kernel/src/task/scheduler.rs` | Tags actual `sshd` fork-children, logs sparse fork-child dispatch/switch summaries, and logs `fork_child_trampoline` only for the tagged task. |
| `userspace/vfs_server/src/main.rs` | Logs VFS request start/end and elapsed time. This proved VFS often continued making progress even while SSH was hung. |
| `userspace/sshd/src/main.rs`, `userspace/sshd/src/session.rs` | Logs parent accept/fork flow, child session startup, sparse `runner.progress()` event variants, shell spawn milestones, relay startup, and sparse PTY/channel relay counters. |
| `userspace/async-rt/src/executor.rs`, `userspace/async-rt/src/task.rs` | Fixes duplicate queueing and, later, preserves tasks spawned while the run queue is being drained so they are not lost before first poll. |

Expected log signatures in a fresh repro:

- `[net] registered net_task id=...`
- `[net] task snapshot: id=... pid=... state=... assigned_core=... affinity=...`
- `[sched] wake_task: id=... name=net prev_state=... -> Ready ...`
- `[net] wake-summary#N: ... attempts=... successes=... failures=... missing_task_id=...`
- `sshd: accepted client fd=...`
- `sshd: host key ready`
- `sshd: session child pid=...`
- `sshd: progress:event ...`
- `sshd: channel_relay:start ...`

### Reading the scheduler fork-child log fields

The `sshd fork-child switch-out` / `sshd fork-child dispatch` log lines are the
backbone of this investigation. Their fields are subtle enough to be misread:

- `cycles=N` — this is the **dispatch counter** (`debug_sshd_fork_child_cycles`,
  `kernel/src/task/scheduler.rs:1924`), not CPU cycles. It's incremented once
  per switch-out and the log fires only when `cycles == 1` or
  `cycles % 1000 == 0`. One log line therefore represents 1,000 actual
  dispatch/yield cycles.
- `reenqueue_after_yield=true` combined with `state=Running` means the task
  voluntarily ended up in `PENDING_REENQUEUE[core_id]` via the yield path
  (`scheduler.rs:1918`). This is not a timer-tick preemption — it is either a
  `sys_sched_yield`-equivalent or a syscall (e.g. `poll(timeout=0)`) whose exit
  path put the task back on the ready queue without blocking it.
- `wake_after_switch=false` — no separate wake was pending when the task left
  the CPU. Combined with `reenqueue_after_yield=true`, the meaning is "the task
  gave up the CPU while still runnable and still wants to run."
- `final_state=BlockedOnRecv` in a `cpu-hog` line means the task ran for ≥ 20
  ticks and finally blocked on IPC. This is compatible with a later burst of
  `switch-out … state=Running reenqueue_after_yield=true` lines for the same
  task once the reply arrives and it runs again.

---

## Primary Evidence

### Current status snapshot

As of the latest branch-local runs on 2026-04-21:

- The strongest early-path trace reaches:
  - `sshd: session child pid=...`
  - `sshd: run_session:start`
  - `progress:event progressed`
  - `progress:event none`
  - `progress:event hostkeys`
  - then `cpu-hog` on the SSH child before any auth event.
- A later-path trace reaches:
  - `password auth ok`
  - `open_session`
  - `session_pty`
  - `session_shell`
  - shell child spawn into `/bin/ion`
  - and then both the SSH session parent and the shell child can hot-yield on
    core 0 for tens of thousands of cycles.
- In one important intermediate repro, `progress_task:spawn relay` appeared
  but `channel_relay:start` did not. That led to the `async-rt` spawned-task
  preservation fix described in §F3 below.
- Even after that fix, hangs still occur at multiple earlier and later points.
  So the executor fix was real but not sufficient.

### A. The scheduler itself is announcing the unfairness

Two warnings in `kernel/src/task/scheduler.rs` fire reliably in every
repro:

- `cpu-hog` at `scheduler.rs:1784-1791` — emitted when a task returns to the
  scheduler after running for at least 20 ticks (~200 ms), printing
  `ran~{ms}` and `final_state`.
- `stale-ready` at `scheduler.rs:1513-1522` — emitted when a task has been
  in `Ready` for longer than the stale threshold without being picked.

Observed lines (Info-level serial output from a plain `cargo xtask run`
followed by one `ssh` attempt):

```
[WARN] [sched] cpu-hog: pid=1 name=userspace-init core=0 ran~480ms final_state=Running
```

and in a Debug-level repro after boot continued a bit further:

```
[WARN] [sched] cpu-hog:    pid=2 name=fork-child core=0 ran~390ms final_state=Running
[WARN] [sched] stale-ready: pid=3 name=fork-child core=0 stale~650ms (ready_at_tick=102)
[WARN] [sched] stale-ready: pid=4 name=fork-child core=0 stale~620ms (ready_at_tick=105)
[WARN] [sched] stale-ready: pid=6 name=fork-child core=0 stale~580ms (ready_at_tick=109)
```

Identity of the involved PIDs comes from init's `started '<name>' pid=N`
banners earlier in the boot log: pid 1 is userspace-init, pid 2 is
`syslogd`, pid 3 is `sshd`, pid 4 is `crond`, pid 6 is `kbd_server`, etc.
All the hogs and all the stale-ready victims are on **core 0**.

> The `name=fork-child` label on the warning is itself a pre-existing
> minor bug: `execve` does not overwrite the task's debug name, so every
> forked-then-execve'd task keeps displaying the fork-child string even
> after it has become syslogd / sshd / whatever. The warning is still
> identifying the right task — just by the wrong label.

### B. The wake path exists, but `net_task` starvation is no longer the lead diagnosis

- Virtio-net IRQ handler: `kernel/src/net/virtio_net.rs:514-537`. The ISR
  reads the ISR-status ack port, sets `NET_IRQ_WOKEN` and `NIC_WOKEN`, and
  calls `wake_task(NET_TASK_ID)`.
- Net task park point: `kernel/src/main.rs:555-581`, specifically line 579
  (`task::scheduler::block_current_unless_woken(&net::NIC_WOKEN)`).
- Wake implementation: `kernel/src/task/scheduler.rs:1173-1222`. If the task
  is in a blocked state and `wake_task` finds it, the scheduler flips it back
  to `Ready`, refreshes `last_ready_tick`, and enqueues it on its
  `assigned_core`.

The dispatch decision still happens inside `Scheduler::pick_next`
(`kernel/src/task/scheduler.rs:298-331`):

1. Phase 1 — local run-queue scan (`dequeue_local`, line 335).
2. Phase 2 — work-stealing (`try_steal`, line 388).
3. Phase 3 — idle-task fallback.

The early version of this appendix stopped here and treated delayed
`net_task` dispatch as the likely root cause. The later instrumentation
changed that assessment:

- `NET_TASK_ID` registration succeeds (`missing_task_id=0`).
- `net_task` typically shows `affinity=0xffffffffffffffff`.
- In many failing virtio repros it is assigned to core 2, not core 0.
- Wake summaries continue changing while SSH is already hung.

So while delayed network work may still contribute in some runs, the current
workspace evidence does **not** support "lost net wakeups" as the primary
remaining diagnosis.

Corroborating pcap: the upstream QEMU user-mode gateway keeps sending `ARP
Request who-has 10.0.2.15 tell 10.0.2.2` once every ~5 s and the guest
sends **nothing** in reply while the wedge is active. Inbound frames are
definitely arriving at the NIC (QEMU does not dump the same frame twice),
so the ISR is being taken but its woken task is not being picked.

### C. Packet-level confirmation that the kernel is otherwise healthy
     when it *does* run

From a Debug-level run where the wedge opened *later* in the SSH session:

```
17:15:16.xxxxxx  10.0.2.2.47814  > 10.0.2.15.22  Flags [S]          (SYN)
17:15:16.xxxxxx  10.0.2.15.22    > 10.0.2.2.47814 Flags [S.]          (SYN-ACK)
17:15:16.xxxxxx  10.0.2.2.47814  > 10.0.2.15.22  Flags [.]           (ACK, 0 bytes)
17:15:16.xxxxxx  10.0.2.2.47814  > 10.0.2.15.22  Flags [P.] len 48    (SSH banner)
17:15:16.xxxxxx  10.0.2.15.22    > 10.0.2.2.47814 Flags [P.] len 208  (server KEX init)
...
17:15:16.xxxxxx  (guest goes silent permanently)
17:15:17.xxxxxx  host retransmits seq 1644:1688 (44 bytes)            [1]
17:15:20.xxxxxx  host retransmits seq 1644:1688                       [2]
17:15:26.xxxxxx  host retransmits seq 1644:1688                       [3]
17:15:36.xxxxxx  host sends FIN                                        (unacked)
17:15:38–17:16:02 additional FIN-PSH retransmits (unacked)
```

The guest correctly sent 531 bytes across multiple segments, then stopped.
This is important because it rules out several plausible alternative
hypotheses (see §"Rule-outs" below): the virtio-net TX ring works, the TCP
state machine works, the ARP cache works, the kernel heap works. What goes
missing is timely network progress after that point.

---

## What This Is Not

These were considered and ruled out by the debugging session:

1. **Not the TCP `TCP_CONNS` deadlock.** That bug (fixed in `de6f0d3`)
   produced a *permanent* wedge on the very first inbound SYN, with the
   last log line always `[tcp] SYN-ACK sent (passive open)` and **zero**
   guest→host SSH bytes on the wire. The fairness regression in this doc
   still wedges, but the log line is different, the pcap shows hundreds
   of payload bytes going out, and repro is timing-dependent.
2. **Not the Debug log volume.** Flipping `serial.rs:134` back to
   `LevelFilter::Info` reduces the number of log lines syslogd has to
   process but does not eliminate the wedge — the Info-level repro still
   hangs before even the first `[tcp] SYN-ACK sent` line would have
   appeared, and pcap still shows unanswered ARP.
3. **Not virtio-net TX.** The "partial-progress" repro proves the TX path
   works at least until the guest sends its KEX init. A fully wedged TX
   ring could not get the KEX init onto the wire at all.
4. **Not the NIC IRQ.** QEMU's user-mode stack does not silently drop the
   ARP requests it logs into pcap — those frames reach the virtio-net
   device. The ISR at `virtio_net.rs:514` is registered as usual via
   MSI-X. The wake flag is being set; what remains unproven is whether the
   wake reached `net_task` successfully and, if it did, why dispatch still
   lagged.
5. **Not the IOMMU.** Reproducible without `--iommu`; adding `--iommu`
   does not change the symptom. Phase 55a substrate is not implicated.
6. **Not `RemoteNic`.** The `--device e1000` variant surfaces a separate,
   earlier bug (the ring-3 RX path is not wired through
   `RemoteNic::inject_rx_frame`; see sibling debugging notes), but with
   the default virtio-net NIC there is no RemoteNic in the picture and
   the wedge still reproduces.

---

## Audited Findings And Remaining Hypotheses

The code audit below separates what is now confirmed from what remains
hypothesis.

### F1 — No audited evidence of explicit core-0 affinity pinning

The earlier version of this appendix treated "userspace tasks are pinned to
core 0 by `affinity_mask = 1 << 0`" as the leading explanation. A code audit
does **not** support that claim:

- `Task::new` defaults to `affinity_mask = u64::MAX` in
  `kernel/src/task/mod.rs:241-255`.
- `task::spawn` places new kernel tasks on the least-loaded core, not
  unconditionally on core 0 (`kernel/src/task/scheduler.rs:579-592`).
- `spawn_fork_task` places a fresh fork child on the **current core** and sets
  `fork_ctx`, but it does **not** narrow the child's affinity mask
  (`kernel/src/task/scheduler.rs:612-647`).
- `sys_sched_setaffinity` is the only audited path here that narrows
  `affinity_mask`, and this appendix contains no evidence that it was called
  on the affected tasks (`kernel/src/task/scheduler.rs:1922-1984`).

What the logs **do** show is clustering on core 0: the reported hogs and
stale-ready victims are all there. At present the safer interpretation is
"core-0 locality or imbalance" rather than "proven affinity pinning."

### H1 — Core-0 locality combined with long-running userspace loops is still plausible

Init's main reap loop at
`userspace/init/src/main.rs:1995-2040` does a `waitpid(WNOHANG)`,
`check_control_commands()` every 3rd iteration, `write_status_file()`
every 10th, and a `nanosleep(1)` (1 **second**, per
`userspace/syscall-lib/src/lib.rs:1496`) only when no child was reaped.
When children exit in bursts (e.g., the nvme_driver and e1000_driver
"cleanly exit because hardware absent" at boot) the `ret > 0` branch runs
many iterations back-to-back with no sleep, doing small file-I/O syscalls
each loop.

Syslogd at `userspace/syslogd/src/main.rs:139-184` calls
`poll(POLL_TIMEOUT_MS)`, drains all pending datagrams in an inner loop,
and unconditionally calls `drain_kmsg` which reads `/proc/kmsg` in a
tight loop until EOF. With a bursty kernel log (boot, service startup,
first SYN-ACK) the drain can run for many iterations without returning
to poll.

Neither loop is necessarily buggy on its own, but both are credible sources
for the observed `cpu-hog` warnings. Combined with the observed concentration
of work on core 0, they remain plausible contributors to the wedge.

**Supporting evidence.** The recorded `cpu-hog` `ran~480ms` for
`pid=1 name=userspace-init` is consistent with this pattern; so is
`pid=2 name=fork-child` (syslogd) `ran~390ms`. Neither userspace daemon
is expected to run for 100s of ms without yielding to the kernel.

**Refinement from the 2026-04-21 repro.** During the post-shell wedge, the
core-0 cluster is actually **three** tasks, not two:

- pid=9 `vfs_server` (fork-child of init), assigned_core=0, state=Running.
- pid=14 sshd session child (execve'd `/bin/sshd`), assigned_core=0.
- pid=15 shell child (execve'd `/bin/ion`), assigned_core=0.

All three were placed on core 0 via fork/clone's current-core placement rule:
init is on core 0, so vfs_server landed there; sshd's listener is on core 0,
so the session child landed there; the session child forked the shell, which
also landed there. Even with `affinity_mask=0xffffffffffffffff`, these three
workers end up pinned-by-ancestry to the same core.

In the final observed wedge, pid=14 and pid=15 ping-pong yields at roughly
1.5 kHz while vfs_server continues to service IPC requests in the background.
ion's `cpu-hog` at line 5412 of the latest `output.txt` ends with
`final_state=BlockedOnRecv` — ion itself does block correctly on IPC. The
remaining spinner is pid=14 (the sshd session's async-rt executor).

### H2 — `assigned_core` locality, fork-child placement, and migration cooldown may matter more than affinity

Even without explicit affinity pinning, several audited scheduler rules keep
work near its current core:

- fork/clone children start on the spawning core and are explicitly exempt from
  stealing while `fork_ctx.is_some()` (`kernel/src/task/scheduler.rs:428-433`,
  `612-647`);
- both stealing and periodic load balancing skip tasks whose
  `last_migrated_tick` is within `MIGRATE_COOLDOWN = 100` ticks (~1 s)
  (`kernel/src/task/scheduler.rs:34-45`, `435-438`, `1811`, `1856-1866`);
- `wake_task` re-enqueues onto the task's current `assigned_core`, so a task
  that repeatedly sleeps and wakes can keep feeding the same local queue
  (`kernel/src/task/scheduler.rs:1196-1219`).

This does not prove the root cause, but it is a code-backed explanation for
why core-0 imbalance could persist even when `affinity_mask` remains wide.

### H3 — The wake path still needs direct instrumentation at the wedge moment

The remaining gap is straightforward: the current evidence does not capture
whether, during the wedge, `NET_TASK_ID` was already registered, whether
`wake_task(TaskId(raw))` returned `true`, and whether `net_task` became
`Ready` but then sat in a run queue. Trace or logging at those points would
turn this from an inference into a direct observation.

### F3 — A real `async-rt` spawned-task bug was found and fixed

`userspace/async-rt/src/executor.rs` previously drained `run_queue` into a
temporary queue inside `poll_spawned_tasks()`, polled that batch, and then
replaced `self.run_queue` with the drained queue. If a task called `spawn()`
while it was being polled, the new child task was pushed into
`self.run_queue` and then discarded when the old queue was written back.

That bug matched one concrete SSH repro exactly:

- `sshd: progress_task:spawn relay ...`
- but no `sshd: channel_relay:start ...`

The current workspace now preserves tasks spawned during the drain pass and
includes a regression test. This was a real root cause for one session-path
failure mode, but not the whole investigation because hangs still occur after
the fix.

### H6 — async-rt ↔ sunset wake-ping-pong (patch applied, not the user-visible bug)

**Status update (2026-04-21 evening):** A minimal branch-local patch was
applied to `userspace/sshd/src/session.rs` gating `set_output_waker` on
non-empty `output_buf()`. The patch demonstrably suppresses the ping-pong
— patched late-wedge runs show pid=14 at `cycles=1` after its first 280ms
burst and silent thereafter, versus the unpatched `output.prev.txt` which
showed pid=14 reaching `cycles=150,000+`. However, the wedge still
reproduces at a comparable rate with the patch applied (see §"Experiment
log — 9 runs" below), so H6 is a real but non-primary bug. The patch is
worth keeping as a semantic improvement; the remaining wedge is now tracked
as H8.

A code audit of the sshd session's async-rt wiring identifies a mutual wake
cycle that matches the previously observed symptom (pid=14 yielding rapidly
on core 0 without any visible SSH-layer progress):

The mechanism:

- `sunset-local/src/runner.rs:367` — `Runner::progress()` calls `self.wake()`
  **unconditionally** before returning. `wake()` in turn fires both
  `input_waker` and `output_waker` whenever `is_input_ready()` /
  `is_output_ready()` are true (`sunset-local/src/runner.rs:697-709`).
- `userspace/sshd/src/session.rs:342` — `io_task` re-arms
  `guard.set_output_waker(&waker)` on **every** loop iteration, using the
  current task's waker.
- `userspace/sshd/src/session.rs:135-152` — `WaitWake::poll` returns `Ready`
  on its second poll regardless of whether the registered event actually
  fired (`self.registered` short-circuits the check). Any wake from any
  source — reactor, Notify, sunset's input/output waker, mutex handoff —
  ends the wait.
- `userspace/sshd/src/session.rs:767` — `ProgressAction::LoopContinue`
  (returned for `Event::Progressed` and `Event::PollAgain`) re-enters
  `runner.progress()` with no yield point in between.

Putting those together, on a quiescent session:

1. progress_task enters `runner.progress()` → sunset calls `wake()` →
   `output_waker` (= io_task's waker) fires.
2. io_task's `WaitWake` flips to `Ready` on its next poll (because
   `self.registered == true`). io_task runs, re-arms `set_output_waker`
   with its waker again, parks on a fresh `WaitWake`.
3. progress_task's next iteration (or the very same one, if sunset returned
   `Progressed` / `PollAgain`) calls `progress()` again → `wake()` again →
   io_task wakes again.

This loop does not require any actual SSH forward progress. Even when the
socket is idle and the output buffer is empty, `runner.wake()` still runs
along the normal return path, and io_task still re-arms the waker every
iteration. The async-rt executor never reaches its blocking
`reactor.poll_once(100)` branch because its run queue never empties (see
H7), so the whole thing stays hot.

Consistent observables:

- `sshd: progress:event progressed pid=14 count=1` appears but no subsequent
  `count=1000` milestone fires, because progress_task spins fast enough that
  the logger's `is_multiple_of(1000)` gate rarely hits a boundary that
  matches the sparse log interval.
- The scheduler log tail shows pid=14 alternating with pid=15 at ~1.5 kHz,
  matching a cooperative two-task yield loop.
- The fault is independent of the network path: both virtio-net and e1000
  substrates reproduce, because the loop is driven by sunset's in-process
  wake, not by packet arrival.

Cheapest falsifiable check: guard `set_output_waker` behind a non-empty
output buffer (`if !guard.output_buf().is_empty()`) and/or replace
`WaitWake`'s `self.registered` short-circuit with a re-check against
`fd_has_events`. If the wedge disappears with that change, H6 is the
remaining bug.

**Result of the waker-gating experiment.** The first variant of that check
was applied and did **not** close the wedge — see §"Experiment log — 9
runs" for the full data. pid=14 now blocks correctly at `cycles=1` instead
of ping-ponging, but stays blocked for the entire ssh timeout window. H6
is therefore demoted to "real, patched, but not sufficient." The wedge
that survives the patch is now tracked as H8.

### H7 — The async-rt executor never blocks while the run queue is non-empty

`userspace/async-rt/src/executor.rs:207-239` — `block_on`'s main loop calls
`reactor.poll_once(100)` only when
`executor.run_queue.is_empty() && !root_header.is_woken()`. Step 3 of the
same loop always runs `reactor.poll_once(0)` (a non-blocking poll syscall).
So any condition that keeps at least one spawned task woken keeps the
executor off the blocking path entirely.

This is what the kernel observes as pid=14's rapid
`reenqueue_after_yield=true` pattern: each iteration of `block_on` performs
at least one `poll` syscall, whose exit path re-enqueues the task without
blocking it. The executor is doing exactly what it was designed to do; the
pathology is that H6 keeps the queue permanently non-empty.

H7 is not a bug on its own — a busy run queue should run — but it is the
amplifier that turns any "task re-wakes itself even when idle" condition
into a visible scheduler-fairness symptom. The mitigation shares a fix with
H6: remove the self-wake source and the executor will reach step 4 and
block in the kernel as designed.

### H8 — Missing socket-readability wake from kernel TCP to userspace poll (new, leading as of 2026-04-21 evening)

With H6's ping-pong suppressed, the late-wedge now shows a cleaner shape:
pid=14 runs a single ~280 ms burst (the SSH handshake synchronous work),
yields once at `cycles=1` with `state=Running reenqueue_after_yield=true`,
and is then **never dispatched again** for the entire ssh timeout window.
Meanwhile `net_task` on core 2 continues to service IRQs — its
`wake-summary` counter increments from #5 to #7 during the window where
pid=14 is frozen. The packets are arriving; they just aren't propagating
into a wake for pid=14's poll waiter.

Observable in `patched2.log`:

```
[INFO] [sched] sshd fork-child switch-out: pid=14 task_idx=19 core=0 … state=Running wake_after_switch=false reenqueue_after_yield=true cycles=1
[WARN] [sched] cpu-hog: pid=14 name=fork-child exec_path=/bin/sshd core=0 ran~280ms final_state=Running
[INFO] [sched] wake_task: id=7 pid=0 name=net … ready_at=3362 migrated_at=3362
[INFO] [net] wake-summary#6: … attempts=10 successes=5 failures=5
[INFO] [net] wake-summary#7: … attempts=11 successes=5 failures=6
qemu-system-x86_64: terminating on signal 15
```

Every `sshd: io_task:*`, `progress_task:*`, and `progress:event` counter
in the session child is at `count=1` throughout the frozen window,
confirming that pid=14's async-rt event loop really has stopped iterating
(not just logging below the `N % 1000` threshold).

Expected mechanism — the gap is somewhere in:

- `kernel/src/net/tcp.rs` — where TCP segment arrival should mark the
  receiving socket as POLLIN-ready and wake any tasks waiting in
  `poll()` / equivalent on that socket.
- `kernel/src/syscall` (or wherever `sys_poll`/`sys_read` handle socket
  FDs) — where userspace `poll()` should register the calling task to
  be woken when socket state changes.

Falsifiable by adding sparse instrumentation to the TCP receive path that
logs (a) whether the incoming segment was attributed to the listening or
established socket matching sock_fd, (b) how many tasks were registered
as waiters on that socket at delivery time, and (c) whether `wake_task`
was called for each waiter. If the wake count is 0 while the packet is
delivered, H8 is confirmed. If the wake happens but pid=14 doesn't run,
the bug is in the scheduler / userspace reactor path, not the TCP stack.

#### H8 root cause and fix (2026-04-21 late evening)

Instrumentation added to `wake_sockets_for_tcp_slot` in
`kernel/src/net/mod.rs` and to socket-poll waiter registration in
`kernel/src/arch/x86_64/syscall/mod.rs` confirmed H8 directly. The root
cause is in `sys_poll`'s positive-timeout path (`syscall/mod.rs:14609`
in the pre-fix code):

```rust
// Pre-fix
if deadline_tick.is_some() {
    for i in 0..nfds {
        if let Some(entry) = &entries[i] {
            fd_deregister_waiter(entry, task_id);
        }
    }
    crate::task::yield_now();
}
```

For every positive-timeout `poll()` call — including the sshd reactor's
`reactor.poll_once(100)` — the code:

1. Scanned FDs for readiness.
2. If not ready, created a `woken` `AtomicBool`, registered waiters on
   every FD's `WaitQueue` with that flag.
3. Re-scanned to close the TOCTOU window (did an event arrive between
   steps 1 and 2?).
4. If still not ready and the timeout was positive, **deregistered all
   waiters** and called `crate::task::yield_now()`.
5. On next scheduler dispatch, returned to step 1.

Step 4 is the bug. Between `deregister` and the next iteration's
`register`, the task has no representation on any `WaitQueue`. If a TCP
segment arrives in that window, `wake_sockets_for_tcp_slot` →
`SOCKET_WAITQUEUES[h].wake_all()` iterates an empty queue. The wake
is silently lost; the task just spins in its poll loop until the
deadline expires, or until it happens to be mid-registration when a
packet arrives.

Observable before the fix: `[tcp-wake] call#N` consistently showing
`waiters=0` despite heavy registration traffic (`[sock-poll-reg]`
showing 65,000+ registrations for pid=14's session on
`socket_handle=1`).

The fix restructures the loop to:

- Allocate a single `woken` `Arc<AtomicBool>` before the loop.
- Register waiters exactly once, up front.
- Reset `woken` at the top of each iteration.
- For positive timeout: `yield_now()` with waiters still registered.
- For indefinite timeout: `block_current_unless_woken(&woken)`.
- Deregister all waiters exactly once, after the loop exits.

Observable after the fix: `[tcp-wake] call#1` shows `waiters=1` for the
SYN (sshd parent's listener poll is registered). Subsequent packets that
arrive while pid=14 is in its own `poll()` show `waiters=1` for the
session socket. The wake-delivery mechanism is now correct.

This is also a general correctness fix for every caller of
`sys_poll()` with a positive timeout, not just sshd.

### H9 — After H8 fix, pid=14 still not making progress after wake (new, leading as of 2026-04-21 late evening)

Even with H8 fixed, the late-wedge still reproduces at a non-trivial
rate. The fingerprint is now subtly different. Example from
`fix7.log`:

- `[tcp-wake] call#1 ... waiters=1` — SYN arrives, sshd parent's listener
  wake fires correctly.
- `sshd: accepted client fd=4 count=1` — sshd parent accepts.
- `sshd: session child pid=14 sock_fd=4` — forked.
- `progress:event progressed` / `progress:event hostkeys` — pid=14's
  async-rt event loop runs for ~290 ms of synchronous SSH handshake work.
- `sshd fork-child switch-out: pid=14 ... state=Running
  reenqueue_after_yield=true cycles=1` — pid=14 yields.
- `cpu-hog: pid=14 ... ran~290ms final_state=Running` — 290 ms hog.
- `[tcp-wake] call#9 ... waiters=1` — subsequent TCP packet arrives,
  pid=14's async-rt is correctly registered on the socket wait queue.

And then the session still hangs for the remaining ssh timeout window.
No further `sshd:` log lines from pid=14. No further `fork-child
dispatch` or `switch-out` for pid=14. The `wake_all` on the socket's
wait queue successfully iterates a registered entry, sets its `woken`
flag, and calls `scheduler::wake_task(pid=14's task_id)`.

That call has known no-op semantics when the task is already
`Ready`/`Running`. And the scheduler log shows pid=14 was
`state=Running reenqueue_after_yield=true` at its last switch-out, so
it IS in the ready queue. The open question is why it isn't being
dispatched.

Candidate next-step investigations:

- Add a dispatch-rate counter per task (not per switch-out like the
  existing debug counter) so we can see whether pid=14 is dispatched
  zero times post-wake versus dispatched many times silently.
- Instrument async-rt's `block_on` loop to log each iteration number
  every N iterations; confirm whether pid=14's executor is spinning
  (many iterations silently) or actually stopped.
- Instrument `scheduler::wake_task` to show what branch it took
  (Blocked→Ready transition, no-op on Running, missing task id).
- Consider whether the core-0 three-task cluster (vfs_server pid=9,
  sshd session pid=14, plus whatever else) produces a scheduling
  starvation on pid=14 specifically.

Relationship to H6 and H8:

- H6 explained the kHz ping-pong that `output.prev.txt` captured.
  H6's patch removed that noise.
- H8 explained the `waiters=0` observation during the wedge. H8's fix
  removed that noise.
- H9 is what's left: a wedge whose fingerprint is "pid=14 correctly
  woken, but downstream the task just does not run."

#### H9 instrumentation pass (2026-04-21 night, 4 runs)

Three branch-local diagnostic additions, all compatible with the prior
H6/H8 fixes:

| Change | Purpose |
|---|---|
| `kernel/src/task/scheduler.rs` — tighten the `sshd fork-child dispatch` and `switch-out` log gate from `cycles == 0/1 \|\| cycles % 1000` to `cycles <= 20 \|\| cycles % 100` | Reveal silent dispatch cycles between the previous 1000-step samples — distinguishes "pid=14 not dispatched" from "pid=14 dispatched at low cycle count." |
| `kernel/src/task/scheduler.rs` — `wake_task` now records the branch outcome (`blocked-to-ready`, `wake-after-switch`, `noop-not-blocked`, `missing-task-id`) and logs it for sshd-fork-child tasks as `[sched] wake_task[h9]` | Distinguish a real Blocked→Ready transition from a no-op wake against an already-Running task. Pre-fix the only logged path was the successful transition, so the no-op branch was invisible. |
| `userspace/async-rt/src/executor.rs` — per-`block_on` iteration counter logged as `[h9-block-on] iter=N pid=P run_queue=L root_woken=B` at iter ≤ 10 and every 200th iteration thereafter | Distinguish "executor spinning hot but never blocking" from "executor parked indefinitely on a non-socket primitive." Output goes to STDOUT (the kernel serial console, not the SSH socket) via `syscall_lib::write_*`. |

Two late-wedge runs after the instrumentation landed:

**h9run3 — late-wedge, 8 tcp-wake calls.** Smoking gun: when call#8
arrives with `waiters=1`, `wake_task[h9]` fires and reports
`branch=noop-not-blocked prior_state=Running`. The kernel-side wake of
pid=14 is a no-op because pid=14's `TaskState` is `Running` at that
exact moment (it is mid-`yield_now()` — see "Reading the scheduler
fork-child log fields" above for why `state=Running
reenqueue_after_yield=true` is the visible signature). The wake
sequence after the cpu-hog burst is:

```
[INFO] [tcp-wake] call#8 tcp_idx=0 sockets_matched=1 waiters=1
[INFO] [sched] wake_task[h9]: id=22 pid=14 name=fork-child branch=noop-not-blocked prior_state=Running
[INFO] [sched] sshd fork-child switch-out: pid=14 ... state=Running reenqueue_after_yield=true cycles=1
[WARN] [sched] cpu-hog: pid=14 ... ran~280ms final_state=Running
[INFO] [sched] sshd fork-child dispatch: pid=14 ... cycles=1
[INFO] [sched] sshd fork-child switch-out: pid=14 ... state=Running reenqueue_after_yield=true cycles=2
[INFO] [sched] sshd fork-child dispatch: pid=14 ... cycles=2
[INFO] [sched] sshd fork-child switch-out: pid=14 ... state=Running reenqueue_after_yield=true cycles=3
```

After the wake hits noop, pid=14 continues to be dispatched (cycles 1, 2,
3). It is **not** the case that pid=14 stops running entirely — but it
also makes **no further sshd-protocol progress**: no new `sshd:
progress:event …` lines, no auth, no shell.

**h9run4 — late-wedge, 8 tcp-wake calls, all post-listener with waiters=0.**
Different fingerprint:

```
[h9-block-on] iter=1 pid=14 run_queue=0 root_woken=1
[h9-block-on] iter=2 pid=14 run_queue=2 root_woken=0
[h9-block-on] iter=3 pid=14 run_queue=1 root_woken=0
[h9-block-on] iter=4 pid=14 run_queue=1 root_woken=0
[h9-block-on] iter=5 pid=14 run_queue=1 root_woken=0
[h9-block-on] iter=6 pid=14 run_queue=1 root_woken=0
... (no further [h9-block-on] iter logs for ~30 s)
```

At iter=2..6 the executor's `run_queue.len() == 1` and
`root_header.is_woken() == false`. With `run_queue` non-empty, the
`block_on` loop's step 4 — the **only** path that calls
`reactor.poll_once(100)` (the blocking, waiter-registering variant) —
is **skipped**. The executor only ever calls `poll_once(0)`, which does
not register any waiter. Hence every subsequent `[tcp-wake]` call shows
`waiters=0` for the session socket.

Then the executor logs stop at iter=6 and pid=14 only completes
`cycles=2` over the remaining ~30 s. The executor is parked on something
that is *not* the socket wait queue — almost certainly a userspace
`Notify` or futex inside the always-runnable spawned task that keeps
`run_queue.len() == 1`.

**Refined H9 diagnosis.** The original H9 framing — "pid=14 woken
correctly but does not dispatch" — was wrong on two counts:

1. The kernel-side `wake_task` for pid=14 is *not* a successful
   Blocked→Ready transition. It is a `noop-not-blocked` because pid=14
   sits in `TaskState::Running` (the yield/reenqueue path leaves the
   state field as `Running`, only re-enqueueing on the local ready
   queue). The wake is a true no-op — `wake_all()` on the WaitQueue still
   sets the per-waiter `AtomicBool`, but the redundant scheduler call
   does nothing.
2. pid=14 *is* dispatched after the wake (cycles 2, 3, …). It is not
   "frozen." It is "running but not making protocol progress."

The two failure modes seen in the four-run sample share the same
underlying problem: **the executor's `block_on` step 4 is gated by
`run_queue.is_empty()`, and at least one always-runnable spawned task
keeps the queue non-empty. The blocking poll path is therefore
unreachable, no waiters get registered, and TCP wakes either land on an
empty WaitQueue (run4) or hit the no-op `wake_task` branch (run3) —
either way the executor never consumes the readiness in a way that
unblocks the SSH protocol layer.**

This is structurally a re-statement of H7 ("the executor never blocks
while the run queue is non-empty"), now confirmed at the per-iteration
level. H6's `set_output_waker` gate removed *one* source of permanent
wakeups (sunset's mutual ping-pong), but at least one other persistent
waker source remains. Candidates worth instrumenting next:

- `progress_task`'s `Notify::notified()` await — does the Notify get
  re-armed every iteration even when there is nothing to progress?
- `io_task`'s loop after the H6 gate — when `output_buf` is empty, does
  io_task park on the WaitWake or does it fall through to another
  always-runnable code path?
- The shared `Mutex<Runner>` — is the runner-mutex's wake path
  re-queueing one of the awaiters even when no work has arrived?

**Budget exhausted.** Per the H9 resume budget (5 runs of
instrumentation + analysis), the investigation halts here with the
above narrowing. The mechanism is now bounded enough that the next
person can target the userspace runtime directly without re-running the
broad H6/H8 hunt.

#### H9 follow-up: per-task wake-count attribution (3 more runs)

After the budget-exhaustion note above the user requested one more
narrowing pass. Two additional instrumentation hooks landed in
`userspace/async-rt`:

| Change | Purpose |
|---|---|
| `userspace/async-rt/src/task.rs` — added `wake_count: AtomicU64` to `TaskHeader`, incremented inside the Arc-waker `wake_fn` and `wake_by_ref_fn` | Per-task running total of how many times each spawned future has been woken since spawn. |
| `userspace/async-rt/src/executor.rs` — `Executor::insert` now emits `[h9-spawn] pid=P slot=S`, and `block_on` dumps `[h9-tasks] iter=N pid=P slot=S woken=B queued=B wake_count=W` for every occupied slab slot at the same gate as `[h9-block-on]` | Identify which spawned task is keeping `run_queue.len() ≥ 1` and at what cadence. |

Three more runs (h9run5–h9run7). h9run7 reproduced the late-wedge with
the new instrumentation enabled and produced the cleanest data set so
far. Spawn order in `userspace/sshd/src/session.rs:197-211` maps
`slot=0 → io_task` and `slot=1 → progress_task`.

```
[h9-spawn] pid=14 slot=0          (io_task)
[h9-spawn] pid=14 slot=1          (progress_task)
[h9-block-on] iter=1 pid=14 run_queue=0 root_woken=1
[h9-block-on] iter=2 pid=14 run_queue=2 root_woken=0
[h9-tasks]    iter=2 slot=0 woken=1 queued=1 wake_count=0
[h9-tasks]    iter=2 slot=1 woken=1 queued=1 wake_count=0
[h9-block-on] iter=3 pid=14 run_queue=1 root_woken=0
[h9-tasks]    iter=3 slot=0 woken=1 queued=1 wake_count=3   ← +3 wakes during slot 1's poll
[h9-tasks]    iter=3 slot=1 woken=0 queued=0 wake_count=0
[h9-block-on] iter=4 pid=14 run_queue=1 root_woken=0
[h9-tasks]    iter=4 slot=0 woken=0 queued=0 wake_count=3
[h9-tasks]    iter=4 slot=1 woken=1 queued=1 wake_count=1   ← +1 wake during slot 0's poll
[h9-block-on] iter=5 pid=14 run_queue=1 root_woken=0
[h9-tasks]    iter=5 slot=0 woken=1 queued=1 wake_count=4   ← +1
[h9-tasks]    iter=5 slot=1 woken=0 queued=0 wake_count=1
[h9-block-on] iter=6 pid=14 run_queue=1 root_woken=0
[h9-tasks]    iter=6 slot=0 woken=0 queued=0 wake_count=4
[h9-tasks]    iter=6 slot=1 woken=1 queued=1 wake_count=2   ← +1
... (executor logs stop at iter=6 — same wedge pattern as h9run4)
```

**Reading.** From iter=3 onward, slot 0 (`io_task`) and slot 1
(`progress_task`) wake each other on alternating polls. Each iteration:
exactly one slot has `woken=1 queued=1` and the other has
`woken=0 queued=0`. `run_queue.len() == 1` persistently. Therefore
`block_on`'s step 4 — gated on `run_queue.is_empty() &&
!root_header.is_woken()` — is unreachable and the executor never calls
the blocking `reactor.poll_once(100)`. No socket waiter is ever
registered for the session, which is why subsequent `[tcp-wake]` calls
land on `waiters=0` (h9run4 fingerprint) or hit the `noop-not-blocked`
branch when pid=14 happens to be `Running` (h9run3 / h9run7 fingerprint).

The +3 jump on slot 0 at iter=3 is consistent with three independent
waker registrations being fired by a single `progress_task` poll: most
likely (a) `runner.set_input_waker(&waker)` and `runner.set_output_waker(&waker)`
both store io_task's waker, then sunset's `Runner::progress()` call
inside progress_task fires `wake()` which signals **both** registered
wakers; combined with (b) the runner-mutex handoff, three `wake_fn`
invocations land on slot 0's header before slot 1 even returns.

**Suspect call sites in `userspace/sshd/src/session.rs`** (in the
order they are most likely to be the residual driver):

1. `session.rs:344, 349` — `guard.set_input_waker(&waker)` and
   `guard.set_output_waker(&waker)` register io_task's waker into the
   shared `Runner`. Sunset's `Runner::progress()`
   (`sunset-local/src/runner.rs:367`) calls `self.wake()`
   unconditionally before returning, which fires whichever wakers are
   registered when the predicates hold. This is the H6 mechanism — the
   gate at line 348 (`if !output_pending`) limits it but does not
   eliminate it because `set_input_waker` is still called every
   iteration whenever `pending_len > 0` (line 340).
2. `session.rs:308-318, 393-401` — `progress_notify.signal()` and
   `session_notify.signal()` are signalled by io_task on every
   non-empty `runner.input()`. Each signal wakes the corresponding
   `Notify::wait()` future. progress_task's `Notify::wait` is at
   `session.rs:785`.
3. The `runner` Mutex itself (`SharedRunner = Rc<Mutex<...>>`) — every
   contended `lock().await` registers the loser's waker, then fires it
   when the holder drops the guard.

**Next step (not yet attempted, would consume more budget than this
session has).** Add per-call-site wake-source tagging: pass an
identifier through the waker so we can distinguish "wake from
set_input_waker" from "wake from progress_notify.signal" from "wake
from runner Mutex handoff." That requires changing the waker
abstraction, so it is non-trivial. A cheaper interim experiment: gate
`set_input_waker` (line 344) on the same condition that already gates
`set_output_waker` — only arm when input is in a state where progress
would have something to do. If that closes the wedge, suspect (1) is
the residual; if not, move to suspect (2).

**Investigation status as of this update.** The H9 mechanism is now
attributable to a specific pair of spawned futures and a small set of
call sites. The next person should not need to re-run the broad
H6/H8/H9 hunt to land a fix.

#### H9 follow-up #2: wake-source attribution (2 more runs)

To answer "is the residual driver `Notify::signal` or the runner
Mutex's guard-drop handoff?" two more diagnostic hooks landed:

| Change | Purpose |
|---|---|
| `userspace/async-rt/src/sync/notify.rs` — global `NOTIFY_SIGNAL_FIRED` / `NOTIFY_SIGNAL_PENDING` atomics, and per-instance `Notify::debug_fired()` / `debug_pending()` getters | Count `Notify::signal()` calls that woke a stored waker vs. those that landed on an empty Notify (no wake fired). |
| `userspace/async-rt/src/sync/mutex.rs` — global `MUTEX_HANDOFF_WAKES` / `MUTEX_DROP_NO_WAITER` atomics on `MutexGuard::drop` | Count guard drops that handed off the lock to a queued waiter vs. drops that found an empty queue. |
| `userspace/async-rt/src/executor.rs` — `[h9-sources] iter=N pid=P notify_fired=A notify_pending=B mutex_handoff=C mutex_drop_idle=D` log at the same gate as `[h9-block-on]` | Per-`block_on`-iteration totals so per-task wake counts can be attributed to specific async-rt primitives. |
| `userspace/sshd/src/session.rs` — sparse `[h9-pn] iter=N progress_fired=… progress_pending=… session_fired=… session_pending=…` dump in `progress_task` | Per-instance counts for the two Notifys actually used by sshd (`progress_notify` and `session_notify`). |

h9run8 + h9run9 (both late-wedge) produced consistent data:

```
[h9-sources] iter=3 pid=14 notify_fired=1  notify_pending=3 mutex_handoff=0 mutex_drop_idle=21
[h9-sources] iter=4 pid=14 notify_fired=3  notify_pending=9 mutex_handoff=0 mutex_drop_idle=35
[h9-sources] iter=5 pid=14 notify_fired=3  notify_pending=9 mutex_handoff=0 mutex_drop_idle=41
[h9-sources] iter=6 pid=14 notify_fired=5  notify_pending=9 mutex_handoff=0 mutex_drop_idle=47
[h9-pn]      iter=4 progress_fired=1 progress_pending=5 session_fired=2 session_pending=4
[h9-pn]      iter=5 progress_fired=1 progress_pending=5 session_fired=2 session_pending=4
```

(`[h9-pn] iter=N` here is `progress_task`'s outer-loop iteration, not
`block_on`'s; it stops advancing once `progress_task` parks on
`progress_notify.wait().await` and gets re-woken only by io_task's
post-input signal.)

**Conclusions:**

1. **`mutex_handoff=0` throughout the wedge.** The async-rt `Mutex`
   guard-drop wake path is **not** firing — `MutexLockFuture` always
   takes the fast path because (in this single-threaded executor) the
   lock is always free when contended-for. `mutex_drop_idle` grows
   steadily (47 drops by exec iter=6) which just confirms the same
   thing from the drop-with-no-waiter side.

   Hypothesis (3) from the previous section — the runner Mutex itself
   driving the ping-pong — is **falsified**. Cross it off the suspect
   list.

2. **`notify_fired` grows monotonically with per-task `wake_count`.**
   At exec iter=6 the global Notify-fired total is 5; per-task
   `wake_count` totals are slot 0 (`io_task`) = 4, slot 1
   (`progress_task`) = 2, sum = 6 (= 5 Notify fires + 1 initial spawn
   wake). The +1 from spawn is expected. The remaining 5 are all
   accounted for by Notify signals. **`Notify::signal` is the residual
   wake source, accounting for ≈100 % of the post-handshake wakes.**

3. **Both `progress_notify` and `session_notify` contribute.** In
   h9run9 by progress_task iter=5: `progress_notify` fired 1× (pending
   5×), `session_notify` fired 2× (pending 4×). Both Notifys are
   hot. The 6 progress + 6 session pending events are dominated by
   io_task's post-input signal pair (`session.rs:308-309, 316-317,
   393-394, 400-401`).

4. **The deeper cause is not in the executor / wake plumbing.**
   io_task signals progress_notify on **every** successful
   `runner.input()` call, including `Ok(0)` (input buffer full or no
   net progress). progress_task wakes from those signals, calls
   `runner.progress()`, gets `Event::None`, parks on
   `progress_notify.wait().await`, then wakes again on the next
   io_task input signal. This loop is sustained by **client TCP
   retransmissions** during the wedge: every retransmitted segment
   arrives at the kernel, fires the socket waitqueue, wakes io_task,
   io_task feeds it to runner (which already saw the same bytes), and
   runner returns no new event because there is no new data.

   The actual wedge then reduces to: **after `progress:event hostkeys`
   + the `hostkeys.hostkeys(&[&host_sign_key])` handler runs,
   `runner.progress()` does not produce further protocol-advancing
   events**, so no SSH server response is ever generated and the
   client times out. The repeated wakes are a downstream symptom, not
   the cause.

**Pivot.** H9 is no longer a kernel scheduler / async-rt fairness
issue. It is an **SSH protocol-layer issue**: sunset's `Runner` does
not advance KEX after the application provides host keys, in this
particular session-child execution path. Future investigation should
target the sunset Runner state machine post-`Hostkeys` event:

- `sunset-local/src/runner.rs` — `progress()` after `ServEvent::Hostkeys`
  returns. What state is `conn` in? Does
  `conn.handle_payload()` get called against the next inbound packet?
- `sunset-local/src/conn.rs` — server-side KEX state. Is there a
  pending response that's not being queued to `traf_out`?
- Compare against a working SSH session (e.g., on the host with
  upstream sunset against a real OpenSSH server) to see what
  intermediate `Event::Progressed` events should fire between
  `Hostkeys` and the eventual `FirstAuth`.

The kernel-side and async-rt instrumentation in this branch is correct
and useful as ongoing diagnostics, but a fix for the late-wedge will
not come from changing it. The branch-local H6 + H8 fixes remain
worthwhile on their own merits (they each fix a real bug). The H9
"residual waker" investigation should now hand off to whoever owns
`sunset-local/`.

##### Postscript: which `Notify` to defang first if pivoting back to async-rt

If a follow-up wants to reduce the noise from a different angle (e.g.,
to make the wedge easier to reason about even before the
sunset-side fix), the data points to a small, targeted change:
gate the `progress_notify.signal()` calls at
`session.rs:308, 316, 393, 400` on `runner.input()` returning `Ok(c)
with c > 0` only — drop the signals when `Ok(0)` is returned. That
removes the spurious wakes triggered by io_task feeding bytes the
runner refuses to consume, without breaking the wake when actual
progress happens. It will not by itself close the wedge (the underlying
sunset state machine still has to produce events), but it will stop the
cosmetic ping-pong that has been the focus of H6 / H7 / H9 so far.

#### H9 follow-up #3: runner output-buffer + event-cadence visibility (7 more runs)

To validate the SSH-protocol pivot two more diagnostic hooks landed:

| Change | Purpose |
|---|---|
| `userspace/sshd/src/session.rs` — `[h9-io] iter=N out_buf_len=L pending_len=P` log at the top of `io_task`'s loop, gated `iter ≤ 5 ‖ % 50` | See whether sunset's `output_buf` is ever non-empty (i.e., whether the server is actually queueing response packets) and whether io_task's `pending` buffer is filling up because `runner.input()` rejects bytes. |
| `userspace/sshd/src/session.rs` — tightened `log_sshd_loop_counter` gate from `count == 1 ‖ count % 1000` to `count ≤ 5 ‖ count % 50` | See the post-hostkeys event cadence (every event variant has its own counter; the old gate hid all subsequent fires). |

Five fresh runs (h9run10–h9run16) — late-wedge captured in h9run10, late-wedge in h9run9
(separate analysis), early-wedge in h9run11/12/13/15, clean in h9run14/16. Two key
observations:

**Observation 1 (clean h9run14/h9run16): the post-hostkeys flow that
`progress_task` should produce.**

```
sshd: progress:event hostkeys count=1
sshd: progress_task:continue count=1
sshd: progress:event progressed count=3        ← post-hostkeys progress
sshd: progress_task:loop_continue count=3
sshd: progress:event none count=4              ← yields cleanly
sshd: progress_task:yield count=4
sshd: progress_task:wait progress_notify count=4
sshd: progress:event none count=5
sshd: progress_task:yield count=5
sshd: progress_task:wait progress_notify count=5
sshd: progress:event first_auth count=1        ← KEX completes, auth begins
sshd: progress_task:continue count=2
```

**Observation 2 (late-wedge h9run10): post-hostkeys is silent.**

```
sshd: progress:event hostkeys count=1
sshd: progress_task:continue count=1
... (no further sshd: progress events for the whole ssh timeout window)
```

With the tightened `≤ 5 ‖ % 50` gate, any of `progress:event progressed
count=2/3/4/5`, `progress:event none count=2/3/4/5`, or any of the
post-hostkeys auth events (`first_auth`, `password_auth`, `pubkey_auth`,
`open_session`, `session_pty`, `session_shell`, `session_exec`,
`session_subsystem`, `session_env`, `defunct`, `poll_again`) **would
have logged** if `progress()` returned them. None of them appear in the
wedge log.

So in the wedge state, **`progress_task` is not making it back to the
log site for any known event after the hostkeys handler returns**.
Either:

- `progress()` returns an `Event` variant the match doesn't recognise
  and falls through to `_ => ProgressAction::Continue` (no log line),
  with the loop body re-entering immediately;
- `progress()` returns `Err`, the handler hits `ProgressAction::Fatal`
  (no log line), `progress_task` returns, `io_task` continues alone
  (but the executor still iterates and `[h9-block-on] iter=7+` should
  appear; it does not);
- the loop is suspended somewhere inside `flush_output_locked.await`,
  `runner.lock().await`, or `progress()` itself in a way that doesn't
  yield to the kernel (consistent with the `cpu-hog ran~630ms
  final_state=Running` observation — 630 ms of pure userspace burn with
  zero scheduler yields).

**`mutex_handoff=0` rules out runner-mutex contention as the suspend
point.** `flush_output_locked.await` with `out_buf_len == 0` is a
no-op. The most consistent reading is that `progress()` is returning a
non-event-bearing result repeatedly (either an unrecognised variant or
`Err`) and the loop is spinning at userspace speed without ever
suspending.

**Confirming evidence from `[h9-io]`:** in every wedge run we observe
`out_buf_len = 0` at every io_task iteration logged. The runner is
**not** generating any output to send. Combined with `pending_len`
growing (43 → 48 in h9run10) — io_task has buffered raw socket bytes
that `runner.input()` refuses to consume because `traf_in` is already
in `InPayload`/`ReadComplete`/`InChannelData` state. Once `progress()`
emits the corresponding `Event` and the application handler calls
`done_payload` (the `Hostkeys` handler does this via
`resume_servhostkeys`), `traf_in` should return to `Idle` and the
pending bytes should drain on the next io_task pass — but we never see
that drain because io_task doesn't reach its log gate before the wedge
sets in.

**Concrete next experiment** (not run here): drop `log_sshd_loop_counter`'s
gate to `≤ 50 ‖ % 50`, drop the `[h9-io]` gate to `≤ 50 ‖ % 50`, and
add an unconditional log at the entry of `progress_task`'s match block
naming the matched arm — even the `_ => Continue` and the `Err` paths.
That single change should make it impossible for `progress_task` to
spin invisibly. If the result shows hundreds of `progress:event
progressed count=N` lines post-hostkeys with `pending_len > 0`
unchanged, the bug is in sunset's `conn.progress()` returning
`Progressed` indefinitely without consuming traf_in. If it shows the
unlogged `_ => Continue` path firing, the bug is an unhandled event
variant in session.rs's match. Either way the next run after that
change will name the residual driver definitively.

Until then the H9 conclusion stands as stated: kernel-side and
async-rt instrumentation is correct; the wedge lives in the SSH
protocol-layer interaction between sunset's `Runner` state machine and
sshd's `progress_task` event loop.

#### H9 follow-up #4: cooperative-yield fix (partial — 5 more runs)

Two changes landed:

| Change | Purpose |
|---|---|
| `userspace/async-rt/src/yield.rs` (new) and `userspace/async-rt/src/lib.rs` | New `yield_now()` future: returns `Pending` then `Ready(())` after a self-wake. Lets a poll-bound future cooperatively yield to the executor without any I/O. |
| `userspace/sshd/src/session.rs` — `progress_task` `LoopContinue` and `Continue` arms now `yield_now().await` before continuing the loop | Force a cooperative yield between back-to-back `progress()` calls so io_task gets polled in between. Without this yield, the loop body had no async suspension point and ran entirely in userspace (the `cpu-hog ran~630ms` observation). |

The widened `log_sshd_loop_counter` gate (`≤ 50 ‖ % 50`) and the
`[h9-arm]` log lines were **reverted** before testing the fix to avoid
the observer-effect noise that was itself perturbing the timing.

5 more runs (h9run22 + h9run23 + h9run26–h9run27 — h9run24/25 lost to
the script subshell aborting after the first ssh exit). Result:

| Run | Code state | Outcome | ssh signal | fork-child cycles | block_on iter |
|---|---|---|---|---|---|
| h9run22 | + `yield_now` in `LoopContinue` only | clean | Permission denied | 21 | (n/a) |
| h9run23 | same | **late-wedge** | host key + timeout | 200+ | 7 (parked at iter=7) |
| h9run26 | + `yield_now` in `Continue` arm too | **early-wedge** | banner timeout | 0 | 0 |
| h9run27 | same | **late-wedge** | host key + timeout | 44 | 9 (parked at iter=9) |

**Reading.** The yield_now fix demonstrably changed the failure shape:

- Pre-fix late-wedge (h9run3/7/9/10): `block_on` iter ≤ 6, kernel
  cycles ≤ 3 over a 30 s window, executor in a 630 ms userspace burst.
- Post-fix late-wedge (h9run23/27): `block_on` iter reaches 7–9 then
  parks at step 4 (`run_queue=0, root_woken=0`), kernel cycles climb
  to 200+ as `sys_poll(100)`'s positive-timeout `yield_now()` ticks
  the deadline. The executor reaches its blocking step properly. The
  ~630 ms cpu-hog burst is gone.

So the `yield_now` patch in `progress_task` is a real, partial fix:
it eliminates the userspace spin and lets `block_on` reach its
blocking step. **But the wedge itself persists** — `progress_task`
parks correctly on `progress_notify.wait()` after `Event::None`, and
`io_task` parks correctly on `WaitWake`, and yet neither one wakes
back up to drain the 48-byte `pending` buffer that was stashed during
the early handshake.

Two new candidate failure modes consistent with the data:

1. **`runner.wake()` post-hostkeys does not fire `input_waker` despite
   `is_input_ready()` becoming true.** sunset's `wake()` consumes
   wakers via `take()` and only re-fires them if the next `progress()`
   call's `is_input_ready()`/`is_output_pending()` predicates return
   true. If `progress()` is *not* called again post-Hostkeys (because
   `progress_task` parks on `progress_notify.wait()` waiting for a
   signal that never comes), `wake()` is never called, `input_waker`
   never fires, and io_task stays parked.
2. **The post-hostkeys `progress()` call IS made but generates no
   `input_waker` fire** — possibly a state where `is_input_ready()`
   returns false even though `traf_in.state` is back to `Idle` (e.g.
   `conn.initial_sent()` returning false at this specific point).

Cheapest next experiment: instrument `runner.wake()` in
`sunset-local/src/runner.rs:699-721` to log every fire (with which
side) and every "no waker" trace. Pair with a log in
`runner.is_input_ready()` post-hostkeys to confirm whether the
predicate flips as expected. This is a small amount of logging in the
*hot path* — should be guarded behind an `#[cfg(feature = "trace_h9")]`
or similar to keep it from regressing the existing tracing volume.

**Status.** The `yield_now` fix is genuinely useful and worth landing
as a `progress_task` correctness improvement on its own (eliminates
the cpu-hog), but does not close the late-wedge. The remaining wedge
is a missing wake along the path

```
hostkeys handled → Continue → next progress() call → wake() → input_waker fires
```

with the failing link unknown. Investigation should pick up in
`sunset-local/src/runner.rs::wake()` and `is_input_ready()`.

#### H9 follow-up #5: post-hostkeys runner state probe (12 more runs)

Two changes landed:

| Change | Purpose |
|---|---|
| `userspace/sshd/src/session.rs` — new `ProgressAction::ContinueProbe` variant. Hostkeys arm now returns `ContinueProbe`. The action handler logs `[h9-postkey] input_ready=B out_empty=B` after re-acquiring the runner lock outside the match. | See sunset's input/output readiness immediately after the application's hostkeys handler returned. |
| `userspace/sshd/src/session.rs` — `[h9-io]` gate widened to `≤ 30 ‖ % 50` | See io_task's iteration cadence in the post-hostkeys window without losing the wedge to log overhead. |

12 fresh runs (h9run28–h9run39). Wedge breakdown:

- 6 clean (h9run29, 30, 32, 35, 37, 39) — `input_ready=1, out_empty=0`
- 1 late-wedge (h9run31) — `input_ready=1, out_empty=0` ⟵ **same as clean**
- 5 early-wedge (28, 33, 34, 36, 38)

The yield_now patch dropped the late-wedge rate from ~30 % baseline to
~8 %, but it still happens.

**Critical new finding from h9run31 (the only late-wedge in this
batch):** the runner's post-hostkeys state is **correct** —
`input_ready=1, out_empty=0`, identical to a clean run. So the wedge
is **not** a state-machine bug in sunset. The output is queued, the
input is acceptable. Yet io_task fails to drain it.

Reading the [h9-tasks] / [h9-io] data:

- io_task (slot 0) post-handshake: `wake_count` goes from 3 (block_on
  iter=4) to 4 (iter=7) — exactly **one** wake.
- progress_task (slot 1): `wake_count` 2 → 5 — three wakes.
- `[h9-io]` log stops at io_task iter=5. With the gate at `≤ 30 ‖ % 50`
  any iter from 6 through 30 would log. **None do.** io_task ran
  exactly 5 outer-loop iterations during the entire ssh-timeout window.
- `flush_output_locked.await` calls `write_all_nonblocking.await`,
  which calls `async_fd.writable().await` if the underlying `write()`
  returns `EAGAIN`. **io_task is suspended inside that
  `writable().await` waiting for the session socket's `POLLOUT` to
  fire** — and the wake never arrives.

Why does the socket not become writable? In the clean run the same
~500-byte burst (`KexDHReply` + `NewKeys` + `ExtInfo`) flushes fine.
In the wedge the kernel-side TCP send path or its waitqueue/wake
mechanism isn't surfacing `POLLOUT` to `sys_poll`. Candidate causes:

- TCP send buffer full because the client never ACKs (chicken-and-egg
  with the server never sending).
- A missing wake on the `POLLOUT` waitqueue when the buffer drains
  symmetrical to the H8 read-side bug.
- An EAGAIN-then-no-wake race in `write_all_nonblocking` similar to
  the pre-H8 sys_poll bug.

**Pivot back to kernel.** The wedge is no longer in `progress_task`
spinning, and no longer in sunset's state machine. It is in the
**socket POLLOUT wake path** — kernel side, in the TCP send-buffer →
`sys_poll`/`AsyncFd::writable` chain.

**Cheapest next experiment:** apply the H8-style "register waiters
once" audit to the kernel's `POLLOUT` registration path
(`kernel/src/arch/x86_64/syscall/mod.rs:fd_register_waiter`'s
write-side equivalent), and add `[tcp-wake-out]` logging at the
TCP send-buffer-drain wake site (analogous to `[tcp-wake]` for
receive). One run with that instrumentation should show whether
`POLLOUT` waiters are being registered and woken correctly when the
TCP send buffer drains under inbound ACKs.

**Status.** Three real fixes landed in the working tree as a result
of this multi-day investigation:

1. **H8** — `sys_poll` positive-timeout register-once correctness fix
   (`kernel/src/arch/x86_64/syscall/mod.rs`).
2. **H6** — gate `set_output_waker` on non-empty output buffer
   (`userspace/sshd/src/session.rs`).
3. **H9 partial** — `async_rt::yield_now()` primitive plus
   cooperative yields in `progress_task::Continue` and
   `progress_task::LoopContinue` paths to eliminate the
   ~630 ms userspace cpu-hog burst.

The remaining wedge is timing-sensitive (~8 % rate post-fix vs ~30 %
baseline) and now sits in the socket `POLLOUT` wake path. The
kernel/userspace plumbing is correct enough that a clean SSH session
completes the handshake in the great majority of runs.

#### H9 follow-up #6: kernel POLLOUT semantics audit (no extra runs)

A code audit of the kernel-side wake / poll plumbing (no new runs in
this session) clarified the residual wedge mechanism enough to write
down the next fix without further bisection:

**Finding 1 — TCP `tcp::send` is unconditionally accepting.**
`kernel/src/net/tcp.rs:314-325` and the per-conn `tcp_send` helper at
line 156 queue inbound writes via `queue_segment` with **no
backpressure**: no send-window check, no per-conn buffer-space test,
no return value reflecting "no space." Combined with the syscall
wrapper at `kernel/src/arch/x86_64/syscall/mod.rs:13538-13546`
which always returns the capped byte count for the TCP path,
**`write(socket_fd, …)` for an established TCP socket never returns
EAGAIN**.

**Finding 2 — TCP `POLLOUT` is unconditional once Connected.**
`kernel/src/arch/x86_64/syscall/mod.rs:14237-14248`:

```rust
let writable = match s.protocol {
    crate::net::SocketProtocol::Tcp => {
        s.tcp_slot.is_some()
            && matches!(s.state, crate::net::SocketState::Connected)
    }
    _ => true,
};
```

There is no notion of "send buffer full → not writable." `POLLOUT` is
true forever once the socket is `Connected`. `AsyncFd::writable().await`
correspondingly returns `Ready` immediately on every call.

**Implication.** The path `write_all_nonblocking → write → EAGAIN →
async_fd.writable().await → spin` proposed in §"H9 follow-up #5" as
the suspected wedge mechanism is **not** the actual mechanism —
EAGAIN never fires, so `async_fd.writable().await` is never even
called from io_task in the wedge state. io_task is stuck somewhere
else.

**Finding 3 — `progress_notify` shows zero successful signal-fires
during the wedge.** The h9run31 `[h9-pn]` series:

```
[h9-pn] iter=1 progress_fired=0 progress_pending=2 ...
[h9-pn] iter=5 progress_fired=0 progress_pending=8 ...
```

Eight `progress_notify.signal()` calls happened during the wedge. **All
eight landed on a Notify with no stored waker** (so the corresponding
`Notify::wait()` calls all returned `Ready` immediately by consuming
the pre-set `signalled` bit). progress_task's `Yield` arm path is

```rust
ProgressAction::Yield => {
    log_sshd_loop_counter("progress_task:yield", ...);
    log_sshd_loop_counter("progress_task:wait progress_notify", ...);
    progress_notify.wait().await;       // returns Ready immediately!
    continue;
}
```

So progress_task's `Yield` path is **also** spinning — every
`progress_notify.wait().await` is a no-op because the previous
io_task signal pre-set the bit, and progress_task never actually
parks. It loops `progress() → Event::None → wait (no-op) →
progress() → ...`.

This is **not** caught by the `yield_now` patch in `LoopContinue` /
`Continue` because the `Yield` arm doesn't go through those paths.

**Concrete next fix candidate.** Add `yield_now().await` after the
`progress_notify.wait().await` in the `Yield` arm of
`userspace/sshd/src/session.rs:progress_task` (around line ~793). This
forces a cooperative yield even when the Notify wait was a no-op. It
is the symmetric companion to the `LoopContinue` / `Continue` yields
already in place for the H9 partial fix.

**Why we did not run it in this session.** Earlier follow-up #4 showed
that adding I/O syscalls (any kind, including the gate-tightening
log statements) to the wedge-prone code path materially changes the
repro rate via observer effect. The right discipline is: (a) revert
all in-flight diagnostic logging, (b) add only the
`yield_now` after `wait()`, (c) take a fresh 10-run sample, (d)
compare wedge rate to the post-yield_now baseline (~8 %). That work
exceeds the time-box of this session and belongs in a clean follow-up
patch.

**Status (final for this session).** Three real fixes landed (H6,
H8, H9 partial). Wedge rate dropped from ~30 % to ~8 %. The remaining
8 % is now traced to a missing `yield_now` after the `Yield`-arm
`progress_notify.wait()`, with code-audit evidence (Finding 3) but no
fresh repro showing the patched behaviour. The H8-style POLLOUT
register-once audit is **no longer** the leading hypothesis — the
TCP send path is unconditional, EAGAIN never fires, so the POLLOUT
wake path is structurally irrelevant to this wedge.

#### H9 follow-up #6: Yield-arm yield_now fix tested (5 more runs)

The fix from Finding 3 was applied: `yield_now().await` after
`progress_notify.wait().await` in `progress_task`'s Yield arm.

5-run sanity sample (h9run40–h9run44):

| Run | Outcome |
|---|---|
| h9run40 | clean (Permission denied) |
| h9run41 | early-wedge (no fork-child) |
| h9run42 | **late-wedge** (host key + timeout) |
| h9run43 | clean |
| h9run44 | early-wedge |

Late-wedge: 1/5 (~20 % — within noise of the ~8 % post-yield_now
baseline; the 5-run sample is too small to distinguish movement from
noise). The Yield-arm yield_now did **not** close the wedge.

So Finding 3 explained one mechanism that *can* spin without yielding,
but it isn't the only mechanism (or the dominant one) keeping io_task
from making post-handshake progress.

**Final mechanism statement (best understanding as of this session):**

In late-wedge runs, after the application's hostkeys handler returns
and progress_task continues, sunset's `Runner` has output queued
(`out_empty=0`) and accepts input (`input_ready=1`) — both
**identical to clean runs**. io_task is woken once after hostkeys
(slot 0 wake_count goes 3 → 4). io_task runs one outer iteration,
flushes the queued output, drains pending input, arms wakers, and
suspends on `WaitWake`. **Nothing wakes it again.**

The wake-chain back to io_task should be one of:
1. `runner.wake()` from inside the next `progress_task::progress()`
   call — fires `output_waker` when `is_output_pending()` becomes
   true. If the next progress() call doesn't generate fresh output,
   no fire.
2. The kernel reactor noticing socket `POLLIN` (incoming TCP segment
   from the client) and firing the read_waker via the userspace
   reactor. Requires `block_on` to reach step 4 and call
   `reactor.poll_once(100)` so the kernel-side waiter is registered.

In clean runs, branch (2) fires when the client responds with
`NewKeys` after seeing the server's `KexDHReply`. In wedge runs, the
client probably *did* respond (we see `tcp-wake#10 waiters=1` in
h9run31), and the kernel wake correctly delivered to the registered
waiter — but io_task's *one* post-wake outer iteration did not produce
visible forward progress (no further `progress:event ...` lines, no
further `[h9-io] iter=` lines).

The tightest possible next experiment is **finer-grained io_task
inner-step instrumentation**: log every transition through
`flush_output_locked` entry/exit, every `runner.input()` return, and
every `should_wait` outcome, all without rate-limiting. That would
show definitively where the post-wake io_task iteration completes (or
doesn't). It is bounded — likely 1–2 runs to capture a wedge with
that visibility — but should be done by someone fresh with a clean
budget rather than this multi-day session.

**This investigation closes here.** Three real correctness fixes
landed. Wedge rate is now low enough that SSH completes in the
majority of runs. The remaining wedge is well-characterised: it lives
in io_task's post-wake outer-iteration completion, which is reachable
with a single targeted instrumentation patch on a fresh
investigation.

### Early-wedge: root cause is SCHEDULER.lock contention, NOT a QEMU issue (2026-04-21, 4th session)

Previously this doc concluded the `net_wakes=1` early-wedge was a
QEMU-side packet-delivery failure. **That conclusion was wrong.** A
fresh pcap via QEMU `filter-dump,netdev=net0` proved:

- Client sends SYN to guest — arrives at the guest NIC (confirmed
  via `[tcp-wake] call#1` firing inside handle_tcp).
- **Guest sends SYN-ACK successfully** — visible on the wire.
- Client sends ACK — visible on the wire, arrives at guest NIC.
- Client sends 43-byte SSH banner — visible on the wire, arrives at
  guest NIC.
- Client retransmits the banner 4 times over ~20 s — all visible on
  the wire.

Every packet the client sends reaches the guest's NIC. The three-way
handshake completes from the wire's perspective. This rules out any
QEMU-side drop.

**Actual mechanism.** With serial-log tracing added to
`wake_sockets_for_tcp_slot` → `wake_socket` → `WaitQueue::wake_all` →
`scheduler::wake_task`, the wedge reliably lands on a single line:

```
[tcp-wake] call#1 tcp_idx=0 sockets_matched=1 waiters=1 ...
[tcp-wake] call#1 wake_socket h=0 begin
[wq] wake_all: lock attempt
[wq] wake_all: lock acquired, len=1
[wq] wake_all: lock released, waking 1
[wq] wake_all: waking task id=11    <-- net_task is here
<silence — `woke task id=11` never logs>
```

`net_task` (running on core 2) calls `wake_task(sshd_parent_task_id)`
from inside the tcp-wake path. The call hangs at
`SCHEDULER.lock()` — the first spin::Mutex acquisition inside
`wake_task`'s body. **Something else is holding `SCHEDULER.lock` and
not releasing.**

**Heavy-logging interference confirms the race.** With per-step
logging added to `wake_task`, `wake_all`, the dispatch loop's
pre-`pick_next` path, and `virtio_net_irq_handler`, the ssh clean
rate rose from ~30 % to ~80 % in a 10-run sample (h9run177–h9run186:
8 clean / 2 early-wedge). Reverting the extra logs returns the
clean rate to ~30 %. That log-sensitivity is the classic fingerprint
of a tight lock-contention race, not a deterministic deadlock.

**The real fix requires a SCHEDULER.lock audit** — likely some
combination of:

1. Wrapping `SCHEDULER.lock` acquisitions in `without_interrupts`
   so a same-core ISR cannot re-enter `wake_task` while a task
   holds the lock.
2. Reducing the number of `SCHEDULER.lock` acquisitions per
   dispatch cycle (we take it at least three times per iteration:
   pre-`pick_next` scan, `pick_next` itself, and switch-out
   handling).
3. Splitting hot scheduler state (run queues already have their own
   lock; task state transitions might benefit from per-task atomics).

That work is out of scope for this session. The mechanism is now
pinpointed and the heavy-logging workaround is documented as a
correctness-by-obscurity data point.

**Small side fix landed this session**
(`kernel/src/net/virtio_net.rs`): gated `ISR_STATUS` register reads
in `virtio_net_irq_handler` on a new `USING_LEGACY_INTX` flag.
Legacy INTx needs the read to clear the shared-IRQ latch per
virtio 0.9.5 §2.1.2.4; MSI-X delivery is per-vector and doesn't need
it. Skipping the read in MSI-X mode removes a potential
QEMU / transitional-virtio interaction where reading ISR_STATUS
while MSI-X is enabled can suppress the next MSI-X edge. Did not
measurably change the wedge rate on its own (10 post-fix runs,
3 clean / 7 early-wedge — within noise of baseline) but is correct
in principle and closes a spec-compliance gap.

### Early-wedge: block-with-timeout primitive landed (2026-04-21, late)

Continued the early-wedge pivot by landing the "block-with-timeout"
scheduler primitive called for in the prior session's failed-watchdog
postmortems. Three changes:

1. **New field `Task::wake_deadline: Option<u64>`** in
   `kernel/src/task/mod.rs` — optional absolute-tick deadline at which
   a `Blocked*` task should be force-woken.
2. **New primitive `scheduler::block_current_unless_woken_until(flag, deadline)`**
   in `kernel/src/task/scheduler.rs` — same signature as the existing
   `block_current_unless_woken` but records `wake_deadline`. Refactored
   the original into a shared `block_current_unless_woken_inner` helper.
3. **Scheduler dispatch expiry scan** in the main per-core dispatch
   loop. Before `pick_next`, scans for `Blocked*` tasks whose
   `wake_deadline <= tick_count()`, transitions them to `Ready`, and
   enqueues them via `enqueue_to_core` (which sends the cross-core
   reschedule IPI — critical so halted APs wake on remote expiry).
   Gated by a `static AtomicU32 ACTIVE_WAKE_DEADLINES` counter so the
   scan is O(1) — a no-op load — when no task has a deadline set
   (i.e. always, under the current defaults).

**Lock-hazard postmortem.** The scan runs with `SCHEDULER.lock` held,
then RELEASES the lock before calling `enqueue_to_core`. `enqueue_to_core`
sends the IPI via a direct LAPIC register write and takes
`data.run_queue.lock()` internally; calling it with `SCHEDULER.lock`
still held would break the documented lock order (SCHEDULER → run_queue
is OK; run_queue → SCHEDULER via the IPI handler is not). The first
attempt at this primitive pushed directly into `data.run_queue` without
the IPI — halted APs stayed asleep until their own LAPIC timer fired
(observed as net_task stalling post-SYN).

**Measurements.** 25 runs with the primitive actively consumed by
`net_task` (200-ms defensive poll, h9run120–144) produced 6 clean /
19 early-wedge ≈ 24 %. 10 runs with the primitive landed but NOT
consumed (h9run145–154, net_task back on indefinite block) produced
3 clean / 7 early-wedge = 30 %. Pre-primitive baseline
(h9run89–98, ARP + dup-SYN fixes only) was 4 clean / 6 early-wedge
= 40 %. Sample sizes are small but the trend is:

| Configuration | Clean rate | Notes |
|---|---|---|
| Pre-primitive (baseline) | 40 % | h9run89–98 |
| Primitive consumed (200-ms poll) | 24 % | h9run120–144 — measurably worse |
| Primitive landed, not consumed | 30 % | h9run145–154 — approximately baseline |

The 200-ms defensive poll consistently made the ssh clean rate worse,
not better — either a side-effect of waking net_task when there is no
RX work (~50 Hz wake rate on a system designed for event-driven
scheduling) or because the timed-wake's post-deadline enqueue conflicts
with the scheduler's other wake paths in a way that's hard to capture
in a small sample.

**Conclusion for this session.** The primitive is the right shape for
future consumers with known-unreliable wake sources, and it's kept in
tree with the fast-path counter so the unused case has no cost.
`net_task` itself stays on `block_current_unless_woken` — indefinite
block — because the virtio-net ISR is the authoritative wake source
and a defensive poll layered on top of it is net-negative.

**Still unfixed.** The two early-wedge sub-variants documented in
§Early-wedge: `net_wakes=0` ("SYN never reaches virtio-net") and
`net_wakes=1` ("SYN arrives, SYN-ACK sent, ACK never arrives"). Both
are upstream of guest RX dispatch — the former is a QEMU-SLIRP
first-packet race, the latter an asymmetric ACK-delivery failure that
no guest-side fix can repair without changing the transport substrate.

### Early-wedge: partial fix + mechanism narrowed (2026-04-21)

Separate pivot from H9, working on the early-wedge that now dominates
the failure distribution (58 % of the h9run45–56 sample, 40–60 % of
post-fix samples). Two small correctness fixes landed in the net
stack, plus a failed watchdog experiment that narrowed the remaining
unfixable mechanism to "guest virtio-net RX loses subsequent packets
after the first SYN."

**Fix 1 — Passive ARP learning** (`kernel/src/net/arp.rs` +
`kernel/src/net/dispatch.rs`). The `process_rx_frames` dispatcher
now populates the ARP cache with `(sender_ip, sender_mac)` for every
inbound IPv4 frame. Intended to prevent the classic "first outbound
packet drops because ARP cache is empty" scenario — `ipv4::send`
silently drops packets when `arp::resolve` misses. RFC-compliant
(many real stacks do this as ARP snoop); safe in all m3OS routing
contexts because `ipv4::send` only uses ARP entries for the next-hop
IP, so any extra cached entry sits unused.

Did NOT measurably change the wedge rate: post-fix h9run89–98 →
4 clean / 6 early-wedge (40 %) vs pre-fix h9run45–56 → 5 clean /
7 early-wedge (42 %). The ARP-miss hypothesis turned out wrong —
passive learning never had anything to "fix" because outbound SYN-ACKs
in the early-wedge path had a populated cache to begin with.
(The diagnostic `[ipv4] arp miss` log added during investigation
produced `arp_miss=0` across every clean and every wedge run.)

**Fix 2 — RFC-793 duplicate-SYN retransmit** (`kernel/src/net/tcp.rs`).
Added a `TcpState::SynReceived if has_syn && !has_ack` arm that
re-queues SYN-ACK on duplicate-SYN arrival. Previously this case hit
the `_ => {}` default and was silently dropped. RFC 793 3.4 is
ambiguous between re-send and RST here; re-send is the recovery path
we want.

Did NOT trigger in any observed wedge: `syn_ack_requeued=0` across all
10 post-fix runs. In the `net_wakes=1` sub-variant (client sends SYN,
guest sends SYN-ACK, no further activity), the client never
retransmits SYN — QEMU SLIRP's internal TCP treats the first SYN-ACK
as sufficient and waits for the banner. The duplicate-SYN arm is
kept as correct belt-and-suspenders behaviour for clients that *do*
retransmit.

**Failed experiment — periodic net_task watchdog.** Two attempts:

1. ISR-driven wake in `timer_handler` (every 20 ticks / ~20 ms at
   1 kHz). Calls `wake_task(net_task_id)` from the timer ISR. **Hard
   deadlock** — same-CPU ISR re-entrance when the interrupted task
   held `SCHEDULER.lock`. Manifested as "tick counter freezes after
   ~200 ticks, system silent until QEMU killed" in h9run68. Reverted.

2. Task-context watchdog spawned alongside `net_task`, using
   `yield_now` in a tick-bounded loop. Avoids the ISR lock hazard.
   **Broke boot** — the yield-loop hogs its core when it's the only
   Ready task (yield returns essentially instantly when there's
   nothing else to schedule), starving boot progress. Observed as
   "sshd never listens within 90 s" in h9run84–88. Reverted.

The safe shape — "block-with-timeout" — requires a new scheduler
primitive that tracks per-task wake deadlines and force-wakes expired
blocked tasks from the timer handler *without* taking `SCHEDULER.lock`
from ISR context. That's a real but non-trivial scheduler change,
out of scope for this session.

**Mechanism statement for the remaining early-wedge.** Across every
`net_wakes=1` wedge run (about half of wedges; the other half are
`net_wakes=0`, "SYN never reaches virtio-net"):

1. Client sends SYN → virtio-net RX IRQ → net_task wakes → `handle_tcp`
   → SYN-ACK queued → `ipv4::send` → `arp::resolve` hits (cache
   populated from the inbound SYN's source MAC via Fix 1) → SYN-ACK
   goes out via `send_frame`.
2. `[tcp-wake] call#1 waiters=1` fires — listener waiter registered.
3. QEMU SLIRP receives SYN-ACK, marks connection established on its
   internal side, sends ACK to guest.
4. **The ACK never reaches virtio-net RX.** No subsequent IRQ fires.
   `net_wakes` stays at 1. Guest's TCP remains in `SynReceived`.
5. `sshd`'s poll on the listener sees no POLLIN (listener is still
   not `Established`), continues its 1 s yield-loop silently until
   ssh client times out at 20 s.

Whether the ACK is never sent by SLIRP (SLIRP-side stall), lost in
transit between SLIRP and virtio-net (QEMU bug), or delivered to
virtio-net without an IRQ (virtio-net driver bug) is unresolved. A
working periodic RX drain watchdog — once the scheduler primitive
exists — would recover from cases 2 and 3 by draining the RX queue
defensively regardless of IRQ state.

**Incidental finding.** `[sched] wake_task ... name=net ...` count
in the scheduler log is a single-number classifier for outcome:
clean runs see 10–12 wakes; `net_wakes=1` early-wedges are exactly
that; `net_wakes=0` early-wedges are the "SYN never arrives" variant.
Useful for batch analysis of large run samples.

#### H9 follow-up #7: io_task inner-step instrumentation (12 more runs, no late-wedge caught)

Fresh-session pass. Three branch-local instrumentation additions
landed in `userspace/sshd/src/session.rs`:

| Change | Purpose |
|---|---|
| `[h9-iox] iter=N step=…` — unratelimited step trace over every outer iteration of `io_task`: `top` → `flush_a_begin` → `flush_a_done` → `feed_begin pending=X` → per-call `feed_input ret=0 ‖ ret_c=N ‖ ret=err` → `feed_done pending=X` → `sw_begin` → `sw_done input_ready=B output_pending=B should_wait=B` → `wait_begin` → `wait_done` → `flush_b_begin` → `flush_b_done` → per-read `read n=N`, `read_input ret_c=N` → `iter_end`. | Definitively locates the step at which an io_task iteration suspends in a late-wedge. |
| `[h9-fo] entry ‖ lock_acquired ‖ write_begin chunk=N ‖ write_done chunk=N ‖ exit ok bytes=T ‖ exit err bytes=T` on `flush_output_locked`. | Distinguishes "hung before output lock," "hung inside `write_all_nonblocking`," and "exited normally." |
| `[h9-ww] fd=F events=E register ‖ ready reg=0 ‖ ready reg=1 ‖ ready_on_register` on `WaitWake::poll`. | Distinguishes "Ready without registering" (fd already has events), "register now" (first-poll path), and "Ready after register" (wake arrived). |

12 fresh runs (h9run45–h9run56). Outcomes:

- **5 clean** (45, 46, 49, 52, 54) — `Permission denied` after full
  handshake.
- **7 early-wedge** (47, 48, 50, 51, 53, 55, 56) — `Connection timed
  out during banner exchange`; no session child spawned; `[h9-iox]`
  logs absent.
- **0 late-wedge.**

The io_task post-wake iteration hypothesis therefore **remains
untested** after this pass. The ~8 % baseline late-wedge rate × 12
runs gave ~60 % expected catch; we got 0. Two possibilities:

1. **Observer effect.** The additional logging (roughly 12–20 new
   `write()` syscalls per io_task outer iteration, plus flush and
   WaitWake transitions) materially changes timing enough to suppress
   the late-wedge branch. The early-wedge rate of 7/12 (~58 %) is at
   the high end of prior samples (~40–50 % post-H9-fix), which is
   weakly consistent with logging overhead pushing the failure mode
   earlier.
2. **Sample noise.** 12 runs at 8 % ≈ 1 ± 1; 0 is within noise.

#### Clean-run fingerprint (h9run45, iter=5 through iter=8)

The new instrumentation gives a complete picture of post-handshake
io_task behavior in a CLEAN run. This is the reference that a
late-wedge must be compared against:

```
iter=5 step=top
iter=5 step=flush_a_begin
[h9-fo] entry fd=4 ... exit ok bytes=0
iter=5 step=flush_a_done
iter=5 step=feed_begin pending=48
iter=5 step=feed_input ret_c=48           ← runner accepts 48 B (KEX)
iter=5 step=feed_done pending=0
iter=5 step=sw_begin
iter=5 step=sw_done input_ready=0 output_pending=0 should_wait=1
iter=5 step=wait_begin
[h9-ww] fd=4 events=1 register            ← WaitWake registers
  (while parked: progress_task runs →
   progress:event hostkeys,
   flush_output_locked fires with write_begin chunk=208 / write_done)
[h9-ww] fd=4 events=1 ready reg=1         ← wake arrives, Ready
iter=5 step=wait_done
iter=5 step=flush_b_begin
[h9-fo] entry ... exit ok bytes=0
iter=5 step=flush_b_done
iter=5 step=read n=60                      ← client responded
iter=5 step=read_input ret_c=16
iter=5 step=read_input ret=0              ← partial consume
iter=5 step=iter_end
iter=6 step=top
iter=6 step=flush_a_begin
 ...
```

Key clean-path signatures to match against any late-wedge capture:

- Each outer iteration reaches `iter_end` cleanly.
- `WaitWake` alternates between `register` and `ready reg=1` — fd=4,
  events=POLLIN only.
- `flush_output_locked` exits with `bytes=0` when called by io_task
  directly; the 208 B KEX reply and 44 B / 52 B auth responses are
  flushed by **progress_task** (also through `flush_output_locked`,
  visible as `[h9-fo] write_begin chunk=N`).
- `feed_input` returns `ret_c=N` (consumed) on iterations where the
  runner is ready, `ret=0` otherwise; `pending` correspondingly
  drops or stays constant across iterations.

#### Expected late-wedge fingerprint (to be captured)

Given the hypothesis in §H9 follow-up #6's "Final mechanism
statement," the late-wedge should fingerprint as one of:

- **(a) iter parks and never wakes.** Last `[h9-iox]` line is
  `iter=K step=wait_begin` followed by `[h9-ww] fd=4 events=1
  register`, then silence. Missing subsequent `[h9-ww] … ready
  reg=1` and missing `iter=K step=wait_done`. ⇒ wake chain into
  io_task is broken; next step is to audit `runner.wake()` in
  `sunset-local/src/runner.rs` and the reactor's POLLIN delivery
  from `sys_poll` to the userspace executor.
- **(b) iter suspends inside flush.** Last line is `[h9-fo] write_begin
  chunk=N` without matching `write_done`. ⇒ `write_all_nonblocking`
  hangs on `async_fd.writable().await` despite §H9 follow-up #6
  Finding 1 asserting `write()` never returns EAGAIN for TCP. A
  capture here would falsify that finding.
- **(c) iter suspends inside `runner.lock().await`.** Last line is an
  `iter=K step=X` that is immediately before an `await guard =
  runner.lock().await` call, without the follow-up log. ⇒ some other
  task holds the runner mutex indefinitely. Contradicted by
  `mutex_handoff=0` across all prior wedges, but a concrete capture
  would settle it.

Next-session action: repeat the 12-run sample. At 8 % baseline,
expected catch is ~60 % within 12 runs; if 0 wedges caught again,
trim the instrumentation (especially the `[h9-ww]` per-poll log)
and re-sample — the per-poll log may be the biggest observer-effect
contributor since WaitWake is re-polled on every spurious wake.

#### Incidental observation: net_task wake count separates early-wedge from clean

A useful side signal from the 12-run sample is that the kernel
`[sched] wake_task: ... name=net ...` count cleanly separates the
outcomes:

- Clean runs: 10–12 net_task wakes (full TCP handshake).
- Early-wedge runs: 0–1 net_task wakes (SYN arrives, then nothing).

This is a single-number classifier for outcome that doesn't require
parsing session-child logs. Useful as a sanity check when sampling.

### Experiment log — 14 runs (baseline, H6, H8 diagnosis, H8 fix)

Captured 2026-04-21 on `feat/phase-55b-ring-3-driver-host`. Each run:
fresh `cargo xtask run`, one `ssh -o BatchMode=yes -o ConnectTimeout=20 -p 2222`
against 127.0.0.1, QEMU killed ~30–60 s later.

| Run | Code state | Outcome | ssh signal | [tcp-wake] calls | Waiters on first call |
|---|---|---|---|---|---|
| B1 | unpatched | **early-wedge** | banner timeout | (no instr) | — |
| B2 | unpatched | clean | Permission denied | — | — |
| B3 | unpatched | clean | Permission denied | — | — |
| B4 | unpatched | **late-wedge** | host key + timeout | — | — |
| P1 | H6 waker-gate | clean | Permission denied | — | — |
| P2 | H6 waker-gate | **late-wedge** | host key + timeout | — | — |
| P3 | H6 waker-gate | **early-wedge** | banner timeout | — | — |
| P4 | H6 waker-gate | **early-wedge** | banner timeout | — | — |
| P5 | H6 waker-gate | **early-wedge** | banner timeout | — | — |
| H8-diag-r2 | H6 + tcp-wake instr | **late-wedge** | host key + timeout | 15 | **0** ← H8 confirmed |
| H8-diag-r3 | H6 + tcp-wake + sock-poll-reg instr | clean | Permission denied | 15 | 1 then 0 |
| fix1 | H6 + minimal H8 fix (keep waiters across yield) | **early-wedge** | banner timeout | 1 | 110,580 (bloat) |
| fix2 | H6 + minimal H8 fix | clean | Permission denied | 15 | 92,884 (bloat) |
| fix3 | H6 + minimal H8 fix | **early-wedge** | banner timeout | 0 | — (no TCP segment) |
| fix4 | H6 + **proper H8 fix** (register once) | clean | Permission denied | 15 | **1** |
| fix5 | H6 + proper H8 fix | **early-wedge** | banner timeout | 0 | — |
| fix6 | H6 + proper H8 fix | **early-wedge** | banner timeout | 0 | — |
| fix7 | H6 + proper H8 fix | **late-wedge** | host key + timeout | 9 | 1 (call#1) → 1 (call#9) |
| fix8 | H6 + proper H8 fix | **early-wedge** | banner timeout | 1 | 1 (late) |
| h9run1 | + H9 instr (cycle gate, wake_task[h9], block_on iter) | **early-wedge** | banner timeout | 1 | 1 (listener) — no fork-child spawned |
| h9run2 | + H9 instr | **early-wedge** | banner timeout | 1 | 1 (listener) — no fork-child spawned |
| h9run3 | + H9 instr | **late-wedge** | host key + timeout | 8 | 1 (call#1, call#8) — `wake_task[h9] noop-not-blocked prior_state=Running` on call#8 |
| h9run4 | + H9 instr (block_on gate tightened to ≤10/200) | **late-wedge** | host key + timeout | 8 | 1 (call#1 only); calls #2–8 all `waiters=0`. Executor logs `iter=1..6` then stops; `cycles=1, 2` over ~30 s |
| h9run5 | + per-task wake-count + spawn log | clean | Permission denied | — | slot 0 wc=5, slot 1 wc=3 over iter=1..10; executor reaches blocking step, settles |
| h9run6 | + per-task wake-count | **early-wedge** | banner timeout | 1 | 1 (listener) — no fork-child spawned |
| h9run7 | + per-task wake-count | **late-wedge** | host key + timeout | 8 | 1 (call#1, call#8); slot 0 (`io_task`) and slot 1 (`progress_task`) ping-pong wakes 1-each per iter from iter=4 onward — `run_queue.len()=1` persistently |
| h9run8 | + global Notify/Mutex wake-source counters | **late-wedge** | host key + timeout | 8 | `mutex_handoff=0`, `notify_fired=5` over iter=1..6 — Notify is the wake source, Mutex is not |
| h9run9 | + per-instance `Notify::debug_fired/pending` | **late-wedge** | host key + timeout | 8 | progress_notify fired 1× / pending 5×; session_notify fired 2× / pending 4× — both Notifys hot, dominated by io_task's post-input signal pair |
| h9run10 | + `[h9-io] out_buf_len/pending_len` log in io_task | **late-wedge** | host key + timeout | 8 | `out_buf_len = 0` always; `pending_len` grows 43 → 48 — runner generates no output, refuses input |
| h9run11 | + tightened `log_sshd_loop_counter` gate to `≤ 5 ‖ % 50` | **early-wedge** | banner timeout | 1 | listener wake only |
| h9run12 | same | **early-wedge** | banner timeout | 0 | SYN never reaches handle_tcp |
| h9run13 | same | **early-wedge** | banner timeout | 0 | SYN never reaches handle_tcp |
| h9run14 | same + 8 s settle delay before ssh | clean | Permission denied | — | Full event flow visible: progressed → none → ... → hostkeys → progressed → none → none → first_auth |
| h9run15 | same | **early-wedge** | banner timeout | 0 | SYN never reaches handle_tcp |
| h9run16 | same + 60 s settle delay | clean | Permission denied | — | Same clean flow as h9run14 |
| h9run17 | + `[h9-arm]` unknown_event_continue / err_fatal logs | **early-wedge** | banner timeout | 0 | (n/a) |
| h9run18 | same + 60 s settle | clean | Permission denied | — | h9-arm count=0 — neither dead-end arm fires |
| h9run19 | same | clean | Permission denied | — | h9-arm count=0 |
| h9run20 | same | clean | Permission denied | — | h9-arm count=0 |
| h9run21 | same | clean | Permission denied | — | h9-arm count=0 |
| h9run22 | + `yield_now` in `LoopContinue` only | clean | Permission denied | 21 | n/a |
| h9run23 | same | **late-wedge** | host key + timeout | 200+ | 7 (parked) — yield_now changed shape but didn't close wedge |
| h9run26 | + `yield_now` in `Continue` arm too | **early-wedge** | banner timeout | 0 | 0 |
| h9run27 | same | **late-wedge** | host key + timeout | 44 | 9 (parked) |
| h9run28–39 | + `[h9-postkey] input_ready out_empty` probe via new `ContinueProbe` action | 6 clean / 1 late-wedge / 5 early-wedge | mixed | various | h9run31 (only late-wedge): `input_ready=1 out_empty=0` — runner state correct, io_task ran only 5 outer iters total → suspended inside `flush_output_locked → write_all_nonblocking → async_fd.writable().await` waiting for socket POLLOUT (later disproven — see follow-up #6) |
| h9run40–44 | + `yield_now` after `progress_notify.wait()` in `Yield` arm | 2 clean / 1 late-wedge / 2 early-wedge | mixed | various | Yield-arm yield didn't close the wedge — late-wedge still 1/5 (within noise of ~8 % baseline) |
| h9run45–56 | + `[h9-iox]` step trace / `[h9-fo]` flush entry-exit / `[h9-ww]` WaitWake register-vs-Ready in `session.rs` | 5 clean / 7 early-wedge / 0 late-wedge | mixed | various | 12 runs, **no late-wedge captured**. Clean-run fingerprint now fully documented (iter=5–8, see §H9 follow-up #7). Observer effect probably shifted distribution toward early-wedge (7/12 = 58 %, top of prior samples). Instrumentation is ready for the next sampling pass. |
| h9run57–66 | + passive `arp::learn` in dispatch; `[ipv4] arp miss` diagnostic log | 5 clean / 5 early-wedge / 0 late-wedge | mixed | various | `arp_miss=0` in every run — ARP cache was always populated when SYN-ACK was attempted. The "first inbound packet drops outbound reply" hypothesis was wrong; Fix 1 is correct but no-op for this wedge. |
| h9run67 | + duplicate-SYN retransmit arm in tcp.rs | early-wedge | host key + timeout | n/a | SYN-ACK queued and sent; no duplicate SYN ever arrived (client doesn't retransmit), so the new arm didn't trigger. Confirmed: client TCP treats initial SYN-ACK as sufficient and waits for banner — the wedge is later than the TCP handshake. |
| h9run68–73 | + timer-handler watchdog (every 20 ticks call `wake_task(net_task_id)` from ISR) | 0 clean / 6 early-wedge | banner timeout | 0 | **Deadlock**: timer ISR → wake_task → `SCHEDULER.lock` → spin-wait on same-CPU task already holding the lock. Manifested as tick counter freezing after 200 ticks. Reverted. |
| h9run84–88 | + task-context watchdog (`net_watchdog_task` with `yield_now` tick-bounded wait) | boot hang | "sshd never listened in 90 s" | n/a | Watchdog yield-loop hogs its core when it is the only Ready task — starves boot. Reverted. |
| h9run89–98 | post-cleanup (Fixes 1+2 only; no watchdog) | 4 clean / 6 early-wedge / 0 late-wedge | mixed | various | Correctness fixes keep but don't measurably change wedge rate (40 % clean vs 42 % pre-fix). Remaining early-wedge mechanism: guest virtio-net RX loses subsequent packets after the first SYN. Requires block-with-timeout scheduler primitive to fix safely. |

Summary across conditions:

- Baseline (4 runs): 2 wedges (1 early, 1 late) — 50 %.
- H6 only (5 runs): 4 wedges (3 early, 1 late) — 80 %.
- H6 + minimal H8 (3 runs): 2 wedges (2 early) — 67 %; waiters bloat to
  100k+ because registrations aren't cleaned between iterations.
- H6 + proper H8 fix (5 runs, fix4–fix8): 4 wedges (3 early, 1 late) —
  80 %; waiters=1 on key events, bloat fixed.

Net effect of the H8 fix on wedge rate is not distinguishable from noise
on these small samples. But the qualitative wake behavior is demonstrably
fixed: H8-diag-r2's pre-fix `waiters=0` becomes fix4's `waiters=1` on the
exact same code path. That's what correctness actually looks like.

What the numbers are telling us, if we zoom out from wedge counts to
mechanism:

- **Early-wedge** dominates the wedge total today (7 of 9 total wedges
  across the 14 runs). The fingerprint is "0 `[tcp-wake]` calls or just
  1 near the end of the ssh timeout window." That's an entirely different
  failure mode than H6 / H8 — the SYN never reaches `handle_tcp` at all.
  Not addressed by this investigation.
- **Late-wedge** is the bug the doc's original evidence (2026-04-20
  `output.prev.txt`) captured. With H6 + H8 both fixed, it still
  reproduces (fix7) with `waiters=1` at the right moment but pid=14
  not making subsequent progress. That's now H9.

Open questions for future sampling:

- Is the early-wedge rate actually elevated by something in our recent
  changes, or is 7/14 just the ambient rate on this machine today?
- In fix7's late-wedge, is pid=14 dispatched many times silently (below
  the `cycles == 1 || cycles % 1000 == 0` log gate) or not dispatched at
  all? Needs a different per-dispatch counter.

### H5 — Remaining bug is likely in SSH/session/PTY progress, not just net RX

H5 is the umbrella hypothesis; H6 is the concrete mechanism currently
considered most likely. The umbrella remains useful because it captures the
observation that the hang can open at multiple SSH-layer phases:

- early-path spin after `progress:event hostkeys`, before auth
- later-path spin after `password auth ok`, `open_session`, `session_pty`,
  and `session_shell`
- possible PTY/channel backpressure or zero-progress relay loops after the
  shell is live

Evidence supporting that:

- `/bin/ion` is observed starting and touching history files
- `vfs_server` frequently keeps making forward progress during the hang
- `net_task` often continues to wake and run on another core
- the hot-yielding tasks are the SSH session process and shell child

If H6's fix lands and the wedge moves or mutates rather than disappearing,
H5 stays live and the next step is channel-relay instrumentation. If the
wedge disappears outright, H5 was really just H6 wearing different clothes
at different phases.

### H4 — The scheduler's anti-starvation guardrails are still coarse

The `cpu-hog` warning triggers only after a task has already run for
20 ticks (~200 ms), and migration cooldown is 100 ticks (~1 s). Those are
reasonable diagnostics, but coarse enough that a bad burst on one core can
still create long visible stalls before any corrective path helps.

---

## Why the Standard Workarounds Do Not Help

| Workaround | Outcome |
|---|---|
| Reduce kernel log volume (`Info` vs `Debug`) | Does not help; wedge still reproduces. |
| Add an explicit `sched_yield` after every `nanosleep(0)` in init | Not tested here. Might reduce one source of long runs, but does not explain the whole wedge by itself. |
| Give `net_task` an elevated priority | Possibly helpful, but not enough as a root-cause fix if the real problem is wake failure or persistent core-0 locality. |
| Add `-smp 1` | Not an option for Phase 25 SMP acceptance, and would mask rather than fix. |

---

## Recommended Investigation Path

Sequenced cheapest-first. Each step produces a falsifiable answer that
bounds the next step's scope. Steps 1-2 are now the primary path (H8);
step 3 is retained for completeness. H6 work (steps 4-5) is already done
and its fix is in the working tree.

1. **Instrument the kernel TCP receive path (H8).** In
   `kernel/src/net/tcp.rs` (or equivalent), add a sparse counter and log
   at segment-delivery time: which socket received the segment, whether
   it's the listening or an established connection, how many waiters were
   woken. One run is enough to confirm whether the wake never fires or
   whether it fires but pid=14 doesn't respond. This is the single
   highest-value next experiment.
2. **Cross-check `sys_poll` / socket-FD wake registration.** If step 1
   shows no waiters registered at delivery time, the bug is on the poll
   side: userspace's reactor registers POLLIN interest but the kernel
   doesn't record the task as a waker candidate on the socket's readable
   state. Audit the poll syscall path for socket FDs.
3. **Reconfirm the early-wedge variant separately.** It occurs in both
   unpatched (B1) and patched (P3, P4, P5) runs; the sshd session child
   never forks. Likely candidates: a missed virtio-net IRQ when no task
   is currently parked on net_task, or a listening-socket accept race.
   Needs its own instrumentation in the listener wake path.
4. **Remove the `WaitWake::registered` short-circuit (H6 companion).**
   In `userspace/sshd/src/session.rs:135-152`, replace the `|| self.registered`
   branch with an explicit re-check against `fd_has_events(self.fd, self.events)`
   so the future only returns `Ready` when the registered event has
   actually arrived. With H6's waker-gating already applied, this is the
   remaining piece of the H6 cleanup — worth doing independently even
   though it does not close the H8 wedge.
5. **Instrument `Runner::progress()` event distribution.** One log line
   per call in `sunset-local/src/runner.rs:289` with `disp.event` variant
   and whether input/output wakers were fired. Still useful once H8 is
   closed, as a general runtime-health signal.
6. **Stabilize the remaining logging footprint.** The investigation itself
   exposed observer effects: high-volume scheduler logs materially changed the
   repro shape. Keep only sparse SSH/session/runtime breadcrumbs and avoid
   reintroducing broad `wake_task no-transition` or global fork logs.
5. **Record a trace-ring capture during the wedge.** The existing trace
   ring at `kernel/src/trace.rs` already captures Dispatch and SwitchOut
   events; a forced dump via the QEMU monitor's `nmi` or a kernel panic
   poke at the wedge moment gives a ground-truth timeline of which task
   ran on which core for the full wedge window. Useful if H6's fix does
   not close the hang.
6. **Move the sshd session child off core 0 as a control experiment.**
   Call `sys_sched_setaffinity` to mask core 0 off for the session child
   right after the accept-fork. If the hang moves with the task, the
   remaining bug is not core-0-specific. If the hang disappears entirely,
   core-0 clustering still contributes independently of H6.
7. **Measure core-0 locality rather than affinity first.** Add one-off logs for
   `assigned_core`, `affinity_mask`, and `last_migrated_tick` on init, syslogd,
   sshd, `vfs_server`, and `net_task` at spawn/wake time. This distinguishes
   clustering from actual affinity restriction.
8. **Measure `try_steal` success rate.** Add a `SCHED_STEAL_{OK,FAIL}`
   atomic counter; print from the net_task loop every 100 iterations.
   If steals never succeed on cores 1-3 under load, H2 and H4 become
   distinguishable.
9. **Audit the long-running userspace loops** in init and syslogd once the wake
   path is ruled in or out. This is the cheapest likely reducer of
   `cpu-hog` events, but should follow the direct scheduler/wake telemetry.
10. **Tighten the hog / migration guardrails** only after the above. This is a
    mitigation layer, not the first thing to tune.

---

## What Should Land Where

| Change | Owning phase |
|---|---|
| **H8 fix — restructure `sys_poll` to register waiters once and deregister at exit** (`kernel/src/arch/x86_64/syscall/mod.rs`, `sys_poll`) | **Applied** in working tree. Phase 55b branch-local kernel correctness fix — this is a general fix for every positive-timeout `poll()` caller, not just sshd. Worth landing on its own merits regardless of H9's resolution. |
| H8 instrumentation — `[tcp-wake]` wake-counter log in `wake_sockets_for_tcp_slot` (`kernel/src/net/mod.rs`) | In working tree; useful as an ongoing diagnostic for H9 and for the early-wedge investigation. Keep throttled. |
| H9 investigation — why pid=14 stops making progress after a correctly-delivered socket wake | Phase 55b or 52c; needs per-dispatch counter + async-rt block_on iteration counter to disambiguate "not dispatched" vs "dispatched but silent." |
| Early-wedge investigation — SYN never reaches `handle_tcp` (`kernel/src/net/virtio_net.rs`, listener wake path, QEMU hostfwd) | Phase 52c; entirely separate bug from H6 / H8 / H9. |
| H6 fix — gate `set_output_waker` on non-empty output buffer (`userspace/sshd/src/session.rs`) | **Applied** in working tree (`session.rs:328-351`). Phase 55b branch-local SSH/session fix; still worth landing as a semantic improvement even though it does not close the H8/H9 wedge. |
| H6 fix — remove `WaitWake::registered` short-circuit, or replace with `AsyncFd::readable/writable` (`userspace/sshd/src/session.rs`) | Phase 55b branch-local SSH/session fix; companion to the landed gate. |
| Optional upstream hardening — note / guard `Runner::progress()`'s unconditional `self.wake()` so multi-task executors are not whipsawed (`sunset-local/src/runner.rs`) | Out-of-phase: sunset vendor fork; low priority now that H6 is demoted. |
| Wake-path instrumentation and trace capture | `52c` (per-core scheduler evolution) |
| Core-locality / migration audit (`assigned_core`, cooldown, steal rate) | `52c` |
| Fork-child placement policy review (three-way core-0 cluster of vfs_server + sshd session child + shell) | `52c` |
| Init / syslogd yield-point audit | `52a` (kernel reliability fixes) |
| Any future affinity-policy fix, if telemetry proves one exists | `52c` |
| Hog-threshold / migration-cooldown tuning | `52c` |
| Task debug-name refresh on execve (so `name=fork-child` stops lying) | `52a` |
| `async-rt` spawned-task preservation fix | Phase 55b branch-local userspace runtime fix; likely worth upstreaming independently of the remaining bug |
| Any net-stack follow-up | None — the TCP path is correct after `de6f0d3`, and net wakeups are no longer the leading diagnosis |

After the 2026-04-21 experiment the investigation has split into three
distinct deliverables:

1. **Branch-local on Phase 55b:** the H6 fix (waker gating is already
   applied; the `WaitWake::registered` cleanup is still pending). Ship
   this as a runtime hygiene improvement even though it does not close
   the hang. With the gate in place the executor correctly blocks when
   idle; without it the executor busy-loops.
2. **Kernel-side investigation (likely Phase 55b for a direct fix, 52c
   for any fairness tuning that falls out):** H8 and the early-wedge
   bug. These are the two remaining user-visible failures. H8 is a
   missing wake from TCP receive → userspace poll; the early-wedge is a
   missed listener wake where sshd never accepts. Both need kernel
   instrumentation first, then a targeted fix.
3. **Phase 52c follow-up:** the broader scheduler-fairness telemetry
   item framed in neutral terms, for example "capture wake/dispatch
   telemetry for core-0 starvation under SSH load." This remains
   relevant independently of H8/early-wedge because the
   fork-child-to-current-core placement still clusters long-lived
   userspace workers onto whichever core init happens to run on.

---

## Appendix: Full evidence log pointers

- Commit that fixes the unrelated TCP lock-hold: `de6f0d3 fix(net/tcp):
  release TCP_CONNS before sending outbound segments`.
- Scheduler warning emitters:
  - `kernel/src/task/scheduler.rs:1513-1522` (stale-ready)
  - `kernel/src/task/scheduler.rs:1783-1791` (cpu-hog)
- Task selection:
  - `kernel/src/task/scheduler.rs:298-331` (`pick_next`)
  - `kernel/src/task/scheduler.rs:335-385` (`dequeue_local`)
  - `kernel/src/task/scheduler.rs:388-475` (`try_steal`)
  - `kernel/src/task/scheduler.rs:1840-1900` (rebalance / migration)
- Network wake path:
  - `kernel/src/main.rs:555-581` (`net_task`)
  - `kernel/src/net/virtio_net.rs:514-537` (ISR)
  - `kernel/src/net/virtio_net.rs:463-474` (`NET_TASK_ID`)
  - `kernel/src/net/virtio_net.rs:465-490` (wake counters)
  - `kernel/src/task/scheduler.rs:881-912` (task debug snapshots)
  - `kernel/src/task/scheduler.rs:1248-1297` (`wake_task` net-task logs)
- Userspace daemon main loops:
  - `userspace/init/src/main.rs:1995-2040` (reap loop)
  - `userspace/syslogd/src/main.rs:139-205` (poll + drain_kmsg)
  - `userspace/syscall-lib/src/lib.rs:1496` (`nanosleep` takes **seconds**)
- sshd session async runtime (H6 code paths):
  - `userspace/sshd/src/session.rs:133-153` (`WaitWake` — `self.registered`
    short-circuit returns `Ready` on any wake, not only on the registered
    event)
  - `userspace/sshd/src/session.rs:268-424` (`io_task` — re-arms
    `set_output_waker` on every iteration at line 342; awaits on line 353)
  - `userspace/sshd/src/session.rs:447-814` (`progress_task` —
    `ProgressAction::LoopContinue` path at line 767 re-enters
    `runner.progress()` with no yield point)
  - `userspace/async-rt/src/executor.rs:207-239` (`block_on` — only reaches
    its blocking `poll_once(100)` branch when the run queue is empty AND
    the root future is not woken)
  - `userspace/async-rt/src/reactor.rs:77-132` (`Reactor::poll_once` —
    build-pollfds / call-poll / fire-wakers per call)
- sunset runner wake source (H6 upstream origin):
  - `sunset-local/src/runner.rs:289-374` (`Runner::progress` —
    unconditional `self.wake()` at line 367)
  - `sunset-local/src/runner.rs:697-730` (`Runner::wake` — fires
    `input_waker` and `output_waker` whenever the corresponding
    readiness predicate is true)
