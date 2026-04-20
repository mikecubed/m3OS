# Scheduler Fairness Regression: `net_task` Starved by Userspace CPU-Hogs

**Status:** Unfixed — diagnosed during phase-55b branch-local SSH debugging on 2026-04-20.
**Severity:** High for any workload that depends on timely network RX — TCP
connections hang, ARP requests from the upstream gateway go unanswered, and
the VM appears wedged even though it is not panicked.
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

Under a realistic userspace daemon set (init, syslogd, console_server,
kbd_server, stdin_feeder, fat_server, vfs_server, net_server, crond, sshd,
login), one or more userspace tasks on core 0 run for several hundred
milliseconds without yielding. The scheduler correctly detects this and
emits `[sched] cpu-hog` warnings, and correspondingly emits `[sched]
stale-ready` warnings for the tasks that wanted to run but could not. During
those starvation windows **`net_task` cannot dispatch** even though the
virtio-net RX ISR is firing and `NIC_WOKEN` has been set. The wedge is
**timing-dependent**: on some runs the guest never responds to even the
first ARP request from the QEMU gateway; on others it processes a full TCP
handshake and ~500 bytes of SSH protocol before stalling at the next hot
path. Work-stealing (Phase 52c A.2) exists in the scheduler but is not
moving these tasks off core 0 under this workload.

The fix for this is **not in the network stack.** The TCP lock-hygiene
patch that shipped earlier the same day is a correctness improvement in its
own right, but even a perfectly correct `handle_tcp` cannot help when the
task draining the RX ring never gets CPU.

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

Both outcomes share the same underlying cause (no CPU for `net_task`) —
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

---

## Primary Evidence

### A. The scheduler itself is announcing the unfairness

Two warnings in `kernel/src/task/scheduler.rs` fire reliably in every
repro:

- `cpu-hog` at `scheduler.rs:1784-1791` — emitted when the scheduler
  force-preempts a task that ran longer than the hog threshold, printing
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

### B. The network side is ready; the schedule is not

- Virtio-net IRQ handler: `kernel/src/net/virtio_net.rs:514-537`. The ISR
  reads the ISR-status ack port, sets `NET_IRQ_WOKEN` and `NIC_WOKEN`, and
  calls `wake_task(NET_TASK_ID)`.
- Net task park point: `kernel/src/main.rs:555-581`, specifically line 579
  (`task::scheduler::block_current_unless_woken(&net::NIC_WOKEN)`).

`wake_task` flips the task to `Ready`, but it does not dispatch. The
dispatch decision happens inside `Scheduler::pick_next`
(`kernel/src/task/scheduler.rs:298-331`):

1. Phase 1 — local run-queue scan (`dequeue_local`, line 335).
2. Phase 2 — work-stealing (`try_steal`, line 388).
3. Phase 3 — idle-task fallback.

During the wedge, `net_task` is in the Ready set but does not run, which
implies **both** Phase 1 (local queue) and Phase 2 (stealing) failed to
dispatch it on any of the four configured cores. Per-core `IsrWakeQueue`
draining from `52c` notes is already in place (the wake does happen — the
task is Ready), but the task-selection half is failing.

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
missing is further execution of `net_task`.

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
   MSI-X. The wake flag is being set; the scheduler is not responding to
   it.
5. **Not the IOMMU.** Reproducible without `--iommu`; adding `--iommu`
   does not change the symptom. Phase 55a substrate is not implicated.
6. **Not `RemoteNic`.** The `--device e1000` variant surfaces a separate,
   earlier bug (the ring-3 RX path is not wired through
   `RemoteNic::inject_rx_frame`; see sibling debugging notes), but with
   the default virtio-net NIC there is no RemoteNic in the picture and
   the wedge still reproduces.

---

## Hypothesised Root Causes

In descending order of likelihood given the branch-local evidence.

### H1 — Core-0 affinity pinning combined with non-yielding userspace loops

**Claim.** The tasks accumulating `cpu-hog` warnings are all on core 0,
and the `stale-ready` victims are all on core 0. None of the four cores
under `-smp 4` shows up in the warnings, which is consistent with most or
all userspace tasks being pinned to `affinity_mask = 1 << 0`. Under that
pinning, no amount of work-stealing on cores 1-3 can save a task that has
affinity only for core 0.

**Supporting evidence.**
- `kernel/src/task/scheduler.rs:347` — `dequeue_local` skips a ready task
  whose `affinity_mask & core_bit == 0`.
- `kernel/src/task/scheduler.rs:443` — `try_steal` likewise requires
  `affinity_mask & my_core_bit != 0` before stealing.
- `kernel/src/task/scheduler.rs:1856-1867` — the rebalance path only moves
  tasks whose affinity permits the target core.
- Empirically, under high load on core 0 the run queue on core 0 grows
  (multiple stale-ready entries) while the wedge persists. If the tasks
  were free to run on cores 1-3, the stalls would not last ~hundreds of
  milliseconds.

**Open question.** Where does the core-0 pinning originate? `task::spawn`
defaults to `affinity_mask = !0` for kernel tasks in most paths, but the
fork/execve path may inherit a narrower mask or inject a BSP-preferred
one for initial scheduling. A focused read of `process::fork_ctx` and the
init boot sequence against the affinity call sites at
`scheduler.rs:1919-1996` would settle this in about an hour of work.

### H2 — Userspace daemons doing unbounded tight loops between yields

**Claim.** Init's main reap loop at
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

Neither loop is broken per se — but combined with H1, core-0 affinity
means each such burst starves `net_task` for the full duration of the
burst.

**Supporting evidence.** The recorded `cpu-hog` `ran~480ms` for
`pid=1 name=userspace-init` is consistent with this pattern; so is
`pid=2 name=fork-child` (syslogd) `ran~390ms`. Neither userspace daemon
is expected to run for 100s of ms without yielding to the kernel.

### H3 — ISR wake is set but the wake path is not moving the task out of a park queue on core 0

**Claim.** `block_current_unless_woken` (used by `net_task` at
`main.rs:579`) parks by inserting into a wait structure. If the parked
task lands on core 0's park list and the ISR wakes the task back to
`Ready`, enqueueing into core 0's run queue, but core 0 is already busy
spinning on a cpu-hog, the task will simply sit in core 0's local queue.
Work-stealing should pull it — unless (H1) affinity prevents that, **or**
the stealing path has a subtle live-lock we did not trigger.

**Supporting evidence.** `try_steal` at `scheduler.rs:388` takes the
target core's run-queue lock; if core 0 holds that lock for a non-trivial
fraction of its time under load, steal attempts from cores 1-3 will spin
and retry. Combined with H1 the observed net behaviour is the same
either way, so the two hypotheses are not independently falsifiable with
the current telemetry. A lock-hold trace on the core-0 run queue during
a wedge would distinguish them.

### H4 — Scheduler tick-driven preemption threshold too generous on `-smp 4`

**Claim.** The cpu-hog detection kicks in after ~hundreds of ms. On a
single-socket dev machine emulated at 4 cores this threshold is large
enough that a userspace daemon can monopolise a core for tens of
scheduling decisions before correction. Tuning the hog threshold and/or
introducing periodic voluntary preemption even under Phase 1 local-queue
dispatch would reduce the worst-case starvation window.

**Supporting evidence.** Indirect. Mentioned because the fix, if it ends
up being mostly H1+H2, will also want a guardrail that prevents a single
misbehaving daemon from opening multi-hundred-ms gaps.

---

## Why the Standard Workarounds Do Not Help

| Workaround | Outcome |
|---|---|
| Reduce kernel log volume (`Info` vs `Debug`) | Does not help; wedge still reproduces. |
| Add an explicit `sched_yield` after every `nanosleep(0)` in init | Not tested here, but would at most paper over H2; H1 is the load-bearing part. |
| Give `net_task` an elevated priority | The task is already kernel-privileged and parks on a cheap atomic flag. Priority tweaks do not help a task whose affinity forbids the only idle cores. |
| Add `-smp 1` | Not an option for Phase 25 SMP acceptance, and would mask rather than fix. |

---

## Recommended Investigation Path

Sequenced cheapest-first. Each step produces a falsifiable answer that
bounds the next step's scope.

1. **Audit affinity masks at task spawn.** Instrument `task::spawn`, the
   fork path, and execve to log `affinity_mask` on every transition.
   Confirm whether userspace tasks are pinned to core 0 and, if so, where
   the pin is introduced. Budget: 1 hour. Output: log lines that locate
   the pin.
2. **Set `affinity_mask = !0` on userspace daemons** (or at least on
   init, syslogd, sshd) and rerun the SSH repro. If the wedge disappears,
   H1 is confirmed and the fix is a targeted spawn/exec change. If it
   persists, H3 or H4 is load-bearing.
3. **Record a trace-ring capture during the wedge.** The existing trace
   ring at `kernel/src/trace.rs` already captures Dispatch and SwitchOut
   events; a forced dump via the QEMU monitor's `nmi` or a kernel panic
   poke at the wedge moment gives a ground-truth timeline of which task
   ran on which core for the full wedge window.
4. **Measure `try_steal` success rate.** Add a `SCHED_STEAL_{OK,FAIL}`
   atomic counter; print from the net_task loop every 100 iterations.
   If steals never succeed on cores 1-3 under load, H1 and H3 become
   distinguishable.
5. **Tighten the cpu-hog preempt threshold** (H4 mitigation) once the
   H1/H2 root is nailed down. This is a guardrail, not a fix.

---

## What Should Land Where

| Change | Owning phase |
|---|---|
| Affinity-mask audit + targeted de-pinning of userspace daemons | `52c` (per-core scheduler evolution) |
| Init / syslogd yield-point audit | `52a` (kernel reliability fixes) |
| `task::spawn` / execve affinity-reset policy | `52c` |
| cpu-hog preempt-threshold tuning | `52c` |
| Task debug-name refresh on execve (so `name=fork-child` stops lying) | `52a` |
| Any net-stack follow-up | None — the TCP path is correct after `de6f0d3`. |

There is no reason to carry any part of this fix on the current Phase 55b
branch. The recommendation is to open a Phase 52c follow-up item titled
"scheduler fairness: eliminate core-0 affinity pinning for userspace
daemons" with this appendix as its source reference.

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
- Userspace daemon main loops:
  - `userspace/init/src/main.rs:1995-2040` (reap loop)
  - `userspace/syslogd/src/main.rs:139-205` (poll + drain_kmsg)
  - `userspace/syscall-lib/src/lib.rs:1496` (`nanosleep` takes **seconds**)
