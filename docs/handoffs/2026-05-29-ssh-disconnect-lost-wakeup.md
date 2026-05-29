# Handoff: SSH `exit`-disconnect hang — timing-sensitive lost wakeup in sshd teardown

> **UPDATE 2026-05-29 (syslogd RT busy-spin found + FIXED — separate bug):** a
> persistent userspace CPU hog was discovered and fixed. `syslogd` was busy-spinning at
> **100% of a core at RT priority 5** because `/proc/kmsg` was a frozen snapshot that
> `fd_poll_events` reported as *always* `POLLIN`, so its `poll()` never blocked. This was
> the "syslog using a large amount of CPU" seen in htop. It is now reworked into a live
> consuming kmsg stream (the spin is gone: measured **0 CPU jiffies over 8 s**, was a full
> core). **This did NOT fix the `exit` teardown hang** — verification still HUNG on `exit`,
> confirming the teardown hang is a *separate* kernel-side bug. Notably, removing the
> spinner eliminated the chronic `[WARN] [sched] stale-ready` backdrop (0 warnings in the
> post-fix run, vs 25-28 before) — the spin was an *aggravator* of dispatch latency but not
> the teardown hang's root cause. See **"Update: syslogd RT busy-spin (FIXED)"** below.

> **UPDATE 2026-05-29 (trace-ring + agent cycle):** the symptom is broader and the
> diagnosis is substantially refined. See the **"Update: trace-ring + agent cycle"**
> section at the bottom — it supersedes parts of the original analysis below. Short
> version: it is **intermittent dispatch-starvation** (not a single permanent lost
> wakeup). Every-other interactive command in an SSH session stalls indefinitely until
> the next input; `exit` teardown is the same class. The shell (ion) is *correctly*
> blocked on its PTY read (that path is verified safe); the input/wake from sshd's relay
> is delivered with chronic ~100-150 ms per-cycle latency on non-BSP cores, compounding
> to ~100 s. Empirically RULED OUT this cycle: AP-timer coarseness, cross-core
> steal cooldown, TCP retransmit, and the display-server/GUI stack.

**Status:** OPEN (teardown hang) — `syslogd` RT busy-spin sub-issue is CLOSED (fixed);
the `exit` teardown hang is confirmed independent of it and remains open.
**Date:** 2026-05-29
**Relates to:** Phase 77 Track A "SSH-disconnect hang" (the *residual* explicitly noted in
commit `6f57fbc` — "the SSH client `exit` disconnect still intermittently wedges the guest
in a deeper teardown SMP race"). The `on_cpu`-spin class fixed in `6f57fbc` is **not** this bug.
**Not caused by:** `f699fe4` (PR #201 review fixes: TCP RST/SYN-ACK + procfs) — pre-existing.

## Symptom

From an interactive SSH session into the guest, typing `exit` (logout) hangs the **SSH
client** forever. The rest of the guest stays alive (a second SSH session can log in and
run `ps`). So the client hangs because no TCP FIN is ever delivered to it.

## Reproduction (100% on the failing flow)

```
# host→guest SSH is forwarded: qemu hostfwd tcp::2222-:22
scripts/ssh_session_exit_test.sh virtio exit     # (fix its hardcoded cd path to this repo)
```

The harness boots the guest, SSHes in as `root` (password `root`), waits for the `#`
prompt, sends `exit\n`, and waits for the ssh client to exit. On the bug it prints
`FAIL: ssh client hung after exit` (exit code 10). Default build is 4-core SMP
(`M3OS_SMP` unset → 4); the race needs SMP.

**Important:** the hang only reproduces when the host **keeps draining the pty** while it
waits (as `ssh_session_exit_test.sh` does). A variant that sends `exit` and then merely
`sleep`s (no pty reads) lets cleanup complete normally — the bug is that timing-sensitive.

## Evidence chain

1. **`ps` during the hang (second SSH session):**
   ```
   21  S (sleeping)  /bin/sshd    <- per-connection session child: BLOCKED, failing to reap
   22  Z (zombie)    /bin/ion     <- the login shell: exited cleanly, awaiting reap
   ```
   So the shell `exit` worked; the sshd session child is stuck and never reaps it.

2. **sshd is single-threaded per connection** (`userspace/sshd/src/main.rs`: accept → fork
   per connection). The session child owns `client_fd`; it only `close()`s it (→ FIN) **after**
   `run_session()` returns. The shell-fork child closes all fds except the PTY slave
   (`session.rs` ~792), so it does not pin the socket. Therefore: client hang = `run_session`
   never returns = teardown wedged.

3. **sshd `log_sshd_step` trace** (`userspace/sshd/src/session.rs` `cleanup`): teardown
   reaches `cleanup:start` → `cleanup:close pty_master` → `cleanup:reap shell` and then
   **stops** — no `cleanup:escalate SIGKILL` (fires after only ~2 s), no `cleanup:waitpid
   shell`, no `cleanup:done`. So the child wedges in the reap phase of `cleanup`, before it
   can even SIGKILL/close.

4. **Block state = `BlockedOnRecv` with NO `wake_deadline`** (a plain IPC receive, no timeout).
   Determined by the stuck-task watchdog (`kernel/src/task/scheduler.rs` `watchdog_scan` +
   `kernel-core/src/watchdog_policy.rs`): with the threshold temporarily lowered to 3 s, the
   non-skipped states (`BlockedOnReply/Send/Wait/Futex/Notif`) never fired for the sshd child —
   only `BlockedOnRecv`-no-deadline is skipped by the watchdog, and that is the only state
   consistent with the silence. This rules **out** `nanosleep` (which sets a deadline →
   would be `StuckDeadlineExpired`) and `BlockedOnWait` (waitpid). The instrumented per-iter
   trace showed the child blocks at the **`write()`** for the next log line, i.e. a
   synchronous IPC `write`→server receive whose reply/wake is lost.

5. **Heisenbug.** The bug *disappears* under perturbation:
   - Heavy diagnostic logging (watchdog dumping every idle server every scan, ~12k lines)
     → `cleanup` completes (`cleanup:done`).
   - Raising the watchdog scan frequency (interval 10000→1000) → completes.
   - Host not draining the pty → completes.
   This is the signature of a genuine lost-wakeup race: extra scheduler/serial activity
   delivers (or works around) the missed wake.

6. **Aggravating backdrop:** during teardown the kernel logs recurring
   `[WARN] [sched] stale-ready: ... serial-stdin core=N stale~100 ms` — a core's dispatch
   is intermittently ~100 ms behind. `vfs_server` also logs slow (~70 ms) `STAT_PATH` around
   the `ion`→`/bin/PROMPT` fork at logout. The system is sluggish exactly at teardown.

## Ruled out

- **Not** the `on_cpu` defer-to-epilogue handoff added in `6f57fbc`. Read both halves:
  `wake_task_v2_with` (scheduler.rs ~4304-4348) defers enqueue when `on_cpu==true`; the
  dispatch epilogue (scheduler.rs ~5331-5444) **does** re-enqueue both `Running` (yield) and
  `Ready` (deferred-wake) tasks under `SCHEDULER.lock`. The handoff is correct.
- **Not** dispatch starvation of the sshd child — it is `Blocked` (S), never observed `Ready`
  (no `stale-ready` warning for its pid). The waker simply never fires (or fires into a
  missed-wakeup window).
- **Not** `f699fe4` (this branch's PR #201 review commit).

## Working hypothesis (unconfirmed)

sshd's session child, during the verbose teardown, performs a synchronous `write()` to its
stdout that routes through an IPC to a server (console/log path; `console_server` pid and a
`console` kernel task are present). The child blocks `BlockedOnRecv` waiting for the
reply/credit, and the wake is lost in a missed-wakeup window — most likely in the IPC
endpoint reply→wake rendezvous under SMP teardown contention, OR a lost reschedule to a
halted target core. Any later unrelated scheduler activity would have delivered it (hence
the Heisenbug).

## Recommended next steps

1. **Trace-ring capture, not logging.** The kernel already records `WakeTask`/`SwitchOut`
   into per-core trace rings and can dump them on demand (`TRACE_DUMP_PENDING` /
   `dump_trace_rings_recent`, scheduler.rs ~4816). Wire the *existing* stuck-task watchdog
   (at the real 30 s threshold, or a modest 5 s) to fire `TRACE_DUMP_PENDING` when it catches
   the sshd child, then read the ring for the last `WakeTask` targeting that task — this shows
   whether a wake fired and was lost vs. never fired. Rings are low-perturbation (unlike
   serial logging, which masks the bug).
2. **Identify the endpoint.** Add the blocked task's awaited endpoint id / IPC peer to the
   watchdog dump (kernel-side, one line) so the specific server (console/syslog?) is known.
   Confirm what `fd 1` is for a daemon (init's spawn fd wiring) and whether `write(1)` is a
   synchronous IPC.
3. **Audit the IPC reply→wake rendezvous** for a missed-wakeup window (the send/block vs.
   reply/wake ordering for `BlockedOnRecv`), analogous to the `wake_task_v2_if` TOCTTOU fix
   already in the tree.
4. Consider handing the kernel lost-wake root-cause to a focused second-opinion pass; verify
   any fix against `ssh_session_exit_test.sh virtio exit` run many times (it is ~100% on the
   draining flow, so a clean streak is meaningful).

## Verification harness

`scripts/ssh_session_exit_test.sh <virtio|e1000> <exit|ctrld|exit-ctrld>` — exit 0 = clean
logout, exit 10 = client hung. Repro session artifacts (qemu.log + ssh-session.log) were
captured under `/tmp/m3os-ssh-*` during this investigation. A `ps`-during-hang two-session
inspector pattern (login A → `exit` → login B → `ps`/`/proc`) confirmed the
zombie-shell + blocked-parent state.

## Workaround

`M3OS_SMP=1 cargo xtask run` (single core) should avoid the SMP race, but single-core boot
is slow enough that the test harness could not confirm it cleanly — treat as **untested**
(single-core also failed to reach the SSH password prompt within 60 s — a separate
single-core slowness, see Update below).

---

# Update: trace-ring + agent cycle (2026-05-29, second session)

A deeper pass (two multi-agent workflows + ~15 build/run cycles + targeted kernel
instrumentation) substantially refined the diagnosis and **ruled out several mechanisms**,
but did **not** yet land a working fix. This section supersedes the "Working hypothesis"
above where they conflict.

## Refined symptom — it is NOT teardown-specific

The SSH session is **intermittently sluggish during normal use**, not only at logout. A
responsiveness probe (login, then time five `echo TOKEN` round-trips, then `exit`) shows
the round-trips **alternate**: fast (~0.01 s) / stalled (≥30 s) / fast / stalled / fast.
The "≥30 s" is just the probe's read-timeout before it sends the *next* command — which is
what finally unsticks the shell. So a stalled command stalls **indefinitely until the next
input arrives**. The `exit` teardown is the same class: the relay/teardown crawls for
~100 s (measured: ~100 000 BSP ticks between "exit delivered" and `cleanup:start`).

## What the trace established

- **The shell (ion) is correctly blocked, not buggy.** During a stall ion sits in
  `BlockedOnRecv` (no deadline) on its PTY-slave read. `block_on_pty_slave_read`
  (syscall/mod.rs:5689) is **lost-wake-safe**: it `register()`s on `PTY_SLAVE_WQ` *before*
  the buffer check, and the read loop re-checks the buffer under `PTY_TABLE.lock()` each
  iteration — so a wake landing in any window cannot lose data. Verified by code reading.
- **Therefore the input is not being delivered promptly** — i.e. **sshd's relay executor
  is dispatch-starved**, not ion. (sshd reads the TCP socket, decrypts, writes the PTY
  master; if sshd doesn't run, ion correctly waits.)
- **Chronic dispatch latency on non-BSP cores.** Kernel logs show repeated
  `[WARN] [sched] stale-ready: ... core=3 stale~100-150 ms` (25 of 28 on core 3),
  for `serial-stdin`, the session `ion` (pid 22), and others. A Ready task on a non-BSP
  core waits ~100-150 ms to be dispatched, then runs briefly. Over the teardown's many
  wake/dispatch cycles this compounds to ~100 s.
- **No preempt-count leak, no cpu-hog warnings** — so it is not Phase-57e Bug #9 and not a
  task holding the CPU > 200 ms in one stretch.

## Four-agent consensus root cause (leading, UNREFUTED)

Default features are `["trace", "preempt-voluntary"]`. Under `preempt-voluntary`, a
**reschedule IPI (and the AP LAPIC timer) delivered to a core running in KERNEL mode only
sets the reschedule flag and returns** — `reschedule_ipi_handler_kernel` (interrupts.rs:2091)
and `timer_handler_kernel` (interrupts.rs:1632) **deliberately do not preempt** kernel-mode
tasks (Phase-57e removed `check_and_preempt_kernel`/`preempt_to_scheduler_kernel`; only
comments remain). The reschedule flag is consumed into a dispatch ONLY at a ring-3 return
(`check_and_preempt_user`) or the running task's next cooperative yield/block. So a task
that becomes Ready and is enqueued to a non-BSP core that is busy in kernel mode (or idle)
is not dispatched until that core voluntarily yields. AP timer = 10 ms (vs BSP 1 ms);
`MIGRATE_COOLDOWN` = 100 ms gates cross-core steal of a freshly-enqueued task. The
Heisenbug fits: extra serial logging / scheduler churn creates extra yield points that
drain the pending reschedule flag and dispatch the waiting task.

## Empirically RULED OUT this cycle (each: build + measure, still HUNG > 50-60 s)

| Candidate | Change tested | Result |
|---|---|---|
| AP-timer coarseness | AP LAPIC period 10 ms → 1 ms (`smp/boot.rs:654` `tpm*10`→`tpm*1`) | **still hung** |
| Cross-core steal gating | `MIGRATE_COOLDOWN` 100 → 2 (`scheduler.rs:5455`) | **still hung** |
| TCP retransmit stall | inspected logs | no `[tcp] retransmit`/reset at all — **not TCP** |
| display-server / GUI hog | `M3OS_DISABLE_DISPLAY_SERVER=1` | **still hung** |
| single-core (workaround) | `M3OS_SMP=1` | inconclusive — couldn't reach login (separate single-core slowness) |

So the dominant per-cycle ~100-150 ms latency is **not** the idle-AP timer floor, **not**
the steal cooldown, **not** TCP, **not** the GUI. That leaves the consensus
kernel-mode-no-preempt mechanism as the leading unrefuted cause.

## Why the trace ring couldn't pin the exact wake (tooling limitation)

The per-core trace ring is `TraceRing<128>` (smp/mod.rs:323) — only 128 entries/core — and
core 0's idle task (`task_idx 3`) spams `YieldNow` (caller `lib.rs:438`) so fast that a dump
spans only ~1 ms and is swamped by idle-loop churn. The watchdog one-shot dump trigger also
fired on the wrong task (a benign idle shell crossing the threshold first). To use the ring
here it must be **much deeper (e.g. `Box<TraceRing<2048>>`) and/or the idle-loop `YieldNow`
recording suppressed**, or use a **dedicated per-target-pid ring** that only records
block/wake/dispatch for the task(s) under study.

## Recommended next steps (revised)

1. **Dedicated deep per-task trace.** Add a `Box<TraceRing<N>>` (N≈2048) or a dedicated
   side ring recording, for a target pid (sshd session child pid 21 + its `ion`),
   every block/wake/dispatch/switch with tick + the *assigned core's currently-running
   task*. Dump via a second SSH session (guest stays alive) using `sys_ktrace`
   (syscall 0x1002, `syscall_lib::ktrace`) — write a tiny `ktrace` userspace tool
   (4-place wiring per AGENTS.md). This proves whether sshd's wake fires-but-waits-for-a-
   busy-core-to-yield vs. is-never-enqueued.
2. **Evaluate a low-risk preempt fix:** make `reschedule_ipi_handler_kernel` force a
   redispatch **only when the interrupted ring-0 context is the per-core IDLE task**
   (safe — idle has no state to preserve), rather than full kernel preemption. This is the
   minimal reversal of the Phase-57e deferral and avoids the input-lag regression that
   motivated removing per-tick kernel preemption. (Full kernel preemption is gated behind a
   documented 24h soak — see `kernel/Cargo.toml` `[features]` note — so do not re-enable it
   wholesale without that process.)
3. **Confirm the relay side in userspace:** add targeted per-echo logging to sshd's relay
   (`userspace/sshd/src/session.rs`) to confirm sshd reads the socket bytes and writes the
   PTY promptly vs. its executor stalling — locates kernel-dispatch vs. sshd-logic.

## Verification / repro harnesses (in `/tmp/ssh-repro/`, this session)

- `measure.sh` — boots guest, logs in, sends `exit`, prints `TEARDOWN_SECONDS` or `HUNG`.
- `probe.sh` — logs in, times five `echo` round-trips (responsiveness), then times `exit`.
- `inspect.sh` — two-session: session A hangs on `exit`, session B runs `ps` / reads `/proc`.
- Underlying: `scripts/ssh_session_exit_test.sh <virtio|e1000> <exit|ctrld|exit-ctrld>`
  (exit 0 = clean logout within 15 s, exit 10 = hung). ~100 % on the draining flow.
  (Fixed a stale hard-coded `cd` to a nonexistent worktree → now uses `$REPO_ROOT`.)

---

# Update: syslogd RT busy-spin (FIXED, 2026-05-29 third session)

A separate, definite bug found while investigating "syslogd uses a lot of CPU in htop."
**Fixed in this branch.** It is NOT the `exit` teardown hang (that still reproduces), but
it *was* aggravating system-wide dispatch latency.

## Root cause (adversarially verified — 4 independent code-reading agents)

`syslogd` busy-spun at **100 % of one core at real-time priority 5** (`nice(-15)`,
root not clamped). Mechanism — a textbook level-triggered-poll-on-EOF pathology:

1. `/proc/kmsg` opened as `FdBackend::Proc` with a **frozen one-time `snapshot`** of the
   kernel log, taken at `open()` (`syscall/mod.rs` open path).
2. `fd_poll_events` reported every `Proc` fd as **unconditionally `POLLIN`** — even when the
   snapshot was fully consumed (`syscall/mod.rs` poll arm).
3. Proc `read` returned `0` (EOF) instantly once drained (no block / no EAGAIN).
4. ⇒ syslogd's `poll([sock, kmsg], 2000)` returned immediately every iteration (kmsg always
   "ready"), `drain_kmsg` read to EOF instantly, and the loop re-polled with **no sleep
   anywhere** (the 2 s timeout and the 32-msg `nanosleep` were both never reached).

Secondary functional gap in the old model: the snapshot was frozen at open, so **kernel
messages logged after syslogd started were never delivered** to `kern.log`.

## Fix — `/proc/kmsg` is now a live, consuming stream

- `kernel-core/src/log_ring.rs`: `LogRing` gained a monotonic `total` byte counter plus
  `total_written()`, `oldest_seq()`, and `read_from(cursor, out) -> (n, new_cursor)`
  (fast-forwards a cursor that fell behind the resident window). Host unit tests added.
- `kernel/src/serial.rs`: `dmesg_oldest_seq()`, `dmesg_total_written()`,
  `dmesg_read_from()` accessors over `DMESG_RING`.
- `kernel/src/process/mod.rs`: removed the `snapshot` field from `FdBackend::Proc`
  (`Proc { path }`); for `/proc/kmsg`, `FdEntry::offset` now holds an absolute dmesg
  byte-sequence cursor (not a file offset).
- `kernel/src/arch/x86_64/syscall/mod.rs`: `open` seeds the kmsg cursor at `oldest_seq`;
  `read` streams bytes newer than the cursor; `fd_poll_events` reports kmsg `POLLIN` only
  while `cursor < total_written` (else `0`, so `poll()` blocks for its timeout).
- `kernel/src/fs/procfs.rs`: removed orphaned `render_kmsg_bytes`.

**No userspace change** — syslogd's existing poll loop is correct once the kernel reports
readiness honestly. Idle syslogd now blocks on its 2 s poll deadline; new kernel messages
reach `kern.log` within ≤2 s (no immediate wake added — the wait-queue doc forbids waking
from the arbitrary-context log path, and 2 s log latency is fine; this also keeps the
hot logging path zero-cost and ISR-safe).

## Why no wait-queue wake from the log path

`WaitQueue::wake_*` is task-context-only (`kernel/src/task/wait_queue.rs` doc). The dmesg
ring is pushed from `_kernel_print`, which can run in arbitrary/IRQ context, so waking a
WQ there would violate that contract. `fd_register_waiter` therefore leaves kmsg
non-registered; correctness comes from poll's deadline + the socket waiter syslogd already
has in its poll set.

## Verification (post-fix, `cargo xtask run`, default 4-core SMP)

- `cargo xtask check` — clippy `-D warnings` clean, fmt clean, all host tests pass
  (incl. 4 new `log_ring` cursor tests).
- syslogd `/proc/2/stat` (utime+stime) delta = **0 jiffies over ~8 s** (cumulative 6
  since boot). Pre-fix this was a full core (~800/8 s). **Spin eliminated.**
- `/var/log/kern.log` = **76 676 bytes** (populated; > 64 KiB ring ⇒ post-snapshot
  streaming is occurring, not just the frozen replay).
- **0** `[WARN] [sched] stale-ready` warnings in the run (vs 25-28 before) — the chronic
  dispatch-latency backdrop is gone.
- `scripts/ssh_session_exit_test.sh`-style `exit` still **HUNG** ⇒ teardown hang is
  independent of this fix (consistent with the four-agent claim that the RT spinner is not
  the teardown root cause).

## Bearing on the still-open `exit` teardown hang

The spinner removal closes the "chronic ~100-150 ms stale-ready latency" backdrop but the
`exit`-disconnect teardown still wedges. So the remaining work is unchanged: pursue the
kernel teardown lost-wake / kernel-mode-no-preempt path (recommended next steps above —
the dedicated deep per-task trace, or the idle-task-only redispatch in
`reschedule_ipi_handler_kernel`). Verify any future fix with the now-portable
`scripts/ssh_session_exit_test.sh virtio exit`.

---

# Update: `ktrace` deep per-task trace tool BUILT + first capture (2026-05-29, fourth session)

The deep per-task trace tool (recommended step 1) is **built and working**, and its first
capture **pins the freeze endpoint** the earlier cycles could not.

## The tool (`ktrace`)

A deep, **pid-filtered** focus trace ring, decoupled from the shallow per-core
`TraceRing<128>` rings so it is not swamped by idle churn. Controlled via the extended
`sys_ktrace` (syscall `0x1002`) and a new `userspace/ktrace` binary.

- Kernel: `kernel/src/trace.rs::focus` — a heap `Vec`-backed ring (`FOCUS_CAP=4096`) that
  records **only events whose subject task index is a target** (≤8 targets). Filtering uses
  standalone atomics so `record()` only takes the ring lock for kept events (rare); it uses
  `try_lock` and never nests another lock, so it is safe from scheduler-locked / IRQ-ish
  contexts. Entries are annotated from an **arm-time** `idx→(pid,name)` snapshot (dump-time
  lookup mislabels them — task slots are reused during teardown).
- ABI (`sys_ktrace(cmd,a,b,c)`): `1 ARM(pids)`, `2 DISARM`, `3 READ_FOCUS(offset)` (paged
  text), `4 FOCUS_LEN`, `5 TASKS`, **`6 DUMP_SERIAL`**. The serial dump is the load-bearing
  path: it prints the ring to the **serial console**, which works even while the userspace
  I/O path is wedged by the very hang under study (the userspace `read_focus` path is *not*
  reliable mid-hang).
- Usage: `ktrace arm <pid>...` then trigger the hang, then from a second SSH session
  `ktrace serial` and read the dump out of the QEMU serial log.

## First capture — what it shows (DECISIVE)

Armed on the session's sshd-child (pid 21, idx 25) + its `ion` (pid 22, idx 26) + the sshd
listener (pid 3), then sent `exit`, waited 12 s, dumped from a second session. The focus
ring (target-only) is clean and legible. Over the 4.4 s window from arm to the freeze:

- **`ion` (pid 22) is in a tight synchronous IPC `call` loop on endpoint 4** — 1784×
  `CALL_BLOCK ep=4` → `REPLY_DELIVER`, ~2.5 ms/cycle. It uses a **direct call/reply
  rendezvous hand-off** (no `Dispatch`/`Wake`/`Enqueue` events for idx 26 at all — the reply
  resumes the caller without going through the run queue).
- **The hang's freeze point is exact:** the *last* recorded event is
  `t=47971 REPLY_DELIVER caller_idx=26 pid=22` — ion's call got its reply (so ion became
  runnable) and then **ion never ran again** (no next `CALL_BLOCK`, ever). The dump ran ~12 s
  later with zero newer entries. So **the reply was delivered but the caller was never
  resumed** — the textbook "wake/reply fires but the task is never dispatched" endpoint.
- sshd child (pid 21) was being woken/dispatched normally (44 wakes / 132 dispatches) until
  t=47881, then it too goes silent (~90 ticks before ion).

So the *last thing that happens* to ion is a reply delivery; it then never runs again.

## Follow-up audit — it is dispatch starvation, NOT a lost IPC wake

Two further pieces, gathered after the capture, **redirect the conclusion** away from an IPC
rendezvous bug:

1. **The IPC `call`/`reply` rendezvous is race-free.** `endpoint::reply` (endpoint.rs:1208)
   does `deliver_message(caller)` then `wake_task_v2(caller)` under a `preempt_disable`
   bracket. `deliver_message` (scheduler.rs:4462) sets `pending_msg` **and** stores `true`
   into the caller's `reply_waker` flag, under `SCHEDULER.lock`. The caller's
   `block_current_on_reply_v2` (scheduler.rs:3592) registers that same flag, re-checks
   `has_pending_message` after registering, and blocks on the flag — all under the same lock.
   Every interleaving is covered: deliver-before-block self-reverts via the flag;
   block-before-deliver gets a real `wake_task_v2`. No missed-wakeup window here.
2. **The kernel's own `stale-ready` warnings fire at the exact freeze tick.** With pid
   filtering quiet, the serial log shows, at `ready_at_tick≈47972` (ion's last event was
   47971): `stale-ready pid=9 core=3 stale~50ms`, `pid=18 core=3 ~147ms`, `pid=15 core=3
   ~182ms`, `pid=0(net) core=1 ~238ms`. I.e. **multiple tasks become Ready on core 3 / core 1
   at the freeze and sit un-dispatched for tens-to-hundreds of ms.**

⇒ The freeze is **dispatch starvation**: ion's reply makes it Ready, but its core does not
re-dispatch it (and other Ready tasks pile up on cores 3/1). This is the *same class* as the
"chronic ~100-150 ms stale-ready latency" backdrop and matches the four-agent
**kernel-mode-no-preempt under `preempt-voluntary`** consensus: a core busy in ring 0 (or
halted with a missed reschedule) doesn't pick up a freshly-Ready task until it voluntarily
yields. The IPC reply path is exonerated.

## Immediate next steps (with the tool now in hand)

1. **Capture the stalled core's running context.** The focus filter is target-only, so it
   shows the *victims* (ion, the stale-ready tasks) but not **what occupies core 3 / core 1
   in ring 0 while they wait**. Add a small focus mode that, in addition to the targets, also
   records `Dispatch`/`SwitchOut` for the core(s) named in a `stale-ready` warning (or a
   one-shot "dump the running task of every core" op). That names the task holding the core
   in kernel mode — the actual culprit.
2. **Evaluate the idle-task-only redispatch fix** (recommended step 2 from the trace-ring
   section): make `reschedule_ipi_handler_kernel` (interrupts.rs:2091) force a redispatch
   only when the interrupted ring-0 context is the per-core **idle** task. If the stalled
   cores are idle/halted with a missed reschedule, this fixes it with minimal risk; if they
   are busy in a real kernel-mode task, a bounded kernel-preempt is needed (gated by the
   documented soak). The `stale-ready` tasks (pid 9/15/18) suggest core 3 is *not* idle
   (those would be picked up instantly by an idle core), so lean toward the busy-in-kernel
   case — confirm with step 1.
3. **Why is `ion` busy-looping ep4 at ~2.5 ms/cycle?** A separate, lower-priority oddity: a
   shell at a prompt should block on its read, not spin a synchronous `call`. Likely the
   `exit\n` never reaches ion (input-delivery starvation, same root cause), so it re-polls.
   Identify ep4 (arm its server pid) and confirm.
