# Handoff: SSH `exit`-disconnect hang — timing-sensitive lost wakeup in sshd teardown

> **UPDATE 2026-05-29 (seventh session — HANG FIXED via async-rt liveness backstop; syslog confirmed non-issue):**
> The infinite hang is **fixed** and verified (5/5 repro runs no longer hang). A decisive
> ktrace capture (armed on the *real* session-child pid, discovered from sshd's own serial
> log lines, with the host **draining** to trigger the bug) finally resolved the
> 5th-vs-6th-session contradiction: the wedged sshd child **cycles the async reactor's
> `poll_once(100)` every 100 ms** (`syscall/mod.rs:18493`) with **all four async tasks parked
> and no fd reporting ready**, so none of its three `waitpid(WNOHANG)` exit-backstops ever
> run → the shell-exit is never noticed → `cleanup()`/`close(sock_fd)`/FIN never happen.
> A second SSH session reliably **unsticks** the first (`cleanup:done` appears only after) —
> the textbook Heisenbug. Root: the async-rt executor was *purely* edge-driven, so a single
> transient missed I/O-readiness edge stalled the session **forever**. **Fix:** an
> idle-liveness backstop in `userspace/async-rt/src/executor.rs` — after 3 consecutive empty
> 100 ms polls it force-re-polls every parked task, so the relay reaches its `waitpid`
> backstop and teardown completes within ~300 ms regardless of the missed edge. The audit
> verified the **kernel mechanics are sound** (PTY refcount, `wake_master`/POLLHUP, IPC
> recv/reply rendezvous, async-rt waker persistence) — this was a userspace edge-driven-stall,
> **not** a kernel lost-wake. **syslog CPU: confirmed a non-issue** on the current build
> (measured `utime=0`, `stime≈0.3%` idle; the `a92486f` fix holds — the earlier observation
> was a stale pre-fix build). **Residual (separate, lower priority):** teardown now always
> disconnects the client, but via a raw transport close → `ssh` reports exit 255 ("closed by
> remote host") instead of a clean exit 0; this teardown-cleanliness gap predates the hang
> (the flow hung before reaching any close) and is tracked below. Also corrected two earlier
> mis-diagnoses: `init`@`syscall/mod.rs:3817` is `nanosleep` (a deadline'd sleep, **not** a
> wedge), and `write(1)` for a daemon is a kernel serial write (**not** an IPC — refuting the
> evidence-#4 "lost write reply" theory). **See the bottom section "seventh session" for the
> full capture, fix, and the residual exit-255 follow-up.**

> **UPDATE 2026-05-29 (sixth session — net-RX theory RETIRED; hang is teardown-side):**
> a net-RX + device-IRQ + blk-completion trace overlay + a clean-disk capture **rule out**
> both the "incoming TCP data stops reaching the relay" theory (net RX is healthy; the `exit`
> *was* delivered — `ion` exited) **and** an apparent "virtio-blk MSI storm" (all 443 blk IRQs
> are *real* completions — it was the control session's own command-exec disk I/O, a capture
> artifact). A tried blk `ISR_STATUS`-under-MSI-X fix did **not** help and was reverted. The
> hang is now localized to the **teardown/reap path** (sshd child never finishes `cleanup` →
> never `close()`s the socket → no FIN), with `userspace-init` blocking at
> `syscall/mod.rs:3817`. syslogd CPU re-verified fixed. **See the bottom section "net-RX trace
> overlay RETIRES the net-RX-delivery theory" for the full evidence + revised next steps
> (arm the sshd-child + ion pids and trace the cleanup block site).**
>
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

**Status:** HANG FIXED (seventh session — async-rt liveness backstop, verified 5/5 no-hang);
`syslogd` RT busy-spin sub-issue CLOSED (fixed `a92486f`, re-verified non-issue this session).
RESIDUAL (separate, lower priority): teardown disconnects the client but via a raw transport
close → `ssh` exit 255 ("closed by remote host") rather than a clean exit 0 — tracked below.
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

## Core-context capture (fifth pass) — it IS a lost wake on BlockedOnRecv

Added two lightweight serial dumps to `ktrace` — `cores` (cmd 7, per-core dispatch state via
`dump_dispatch_state`) and `states` (cmd 8, **every** task's state incl. Blocked, no
preempt-trace flood) — and sampled them repeatedly during a live hang. Result, stable across
samples:

```
idx=28 pid=34 state=BlkRecv core=1   <- sshd session child (teardown actor)
idx=27 pid=35 state=BlkRecv core=1   <- ion (session shell)
```

Both wedged tasks are **BlockedOnRecv**, and the cores are **idle** (core 2/3 running their
idle task). So the freeze is a **lost wake on a plain `recv`** — NOT dispatch starvation. This
**confirms the original evidence point #4** and **invalidates the dispatch-starvation detour**
(the `stale-ready` warnings were transient latency on *other* tasks, not the wedged ones). ⇒
**The planned idle-task-only redispatch fix is inapplicable** (the tasks are Blocked, not Ready).

Refined causal chain:
- **`ion`'s wait is legitimate** — it is correctly blocked reading its PTY slave
  (`block_on_pty_slave_read`, already verified lost-wake-safe), waiting for input that never
  arrives because the relay is stuck.
- **The root is the sshd child (pid 34) stuck in `BlockedOnRecv`** — almost certainly its
  `poll()`/socket-recv multiplex of the TCP socket + PTY master: the `exit\n` bytes
  (client→TCP→sshd→PTY→ion) never get relayed.
- The IPC `recv`/`reply` v2 block primitives (`block_current_on_recv_v2` /
  `block_current_on_reply_v2`, scheduler.rs:3810/3592) both register a waker + recheck
  `pending_msg` before parking — race-free. The TCP receive path sets `wake_slot` on every
  matched segment and calls `wake_sockets_for_tcp_slot` after the lock drops (tcp.rs:644-726)
  — fires correctly. `sys_poll` re-registers + rescans each iteration — lost-wake-safe.
- So every wake path *reads* as correct ⇒ this is a **subtle intermittent race** in the
  net_task → `wake_socket` → poll chain, OR the `on_cpu` deferred-enqueue epilogue
  (`6f57fbc`). It cannot be pinned by inspection; it needs targeted block/wake tracing at the
  exact hang moment. (Consistent with the original "any extra scheduler activity unsticks it"
  Heisenbug — the task is enqueue-able, the wake is *almost* delivered.)

## Immediate next steps (with the tool now in hand)

### Block-site instrumentation result — pinpointed to `poll()`, NO wake attempted

`block_current_until` is now `#[track_caller]` and emits a `BlockCurrent` focus event
carrying the **caller location** (the block site) + state. Re-captured. The freeze sequence
for the sshd session child (pid 23, idx 26) is exact:

```
t=58946 WAKE idx=26 ->core2 (from BlockedOnRecv) → ENQUEUE → DISPATCH   (runs its relay)
t=58946 BLOCK idx=26 core2 state=2 @kernel/src/arch/x86_64/syscall/mod.rs:18493
t=58946 SWITCHOUT idx=26 core2
<silence — no further events for idx 26, ever>
```

Line **18493 is the `block_current_until` call inside `sys_poll`**. So the child is lost-woken
**in `poll()`**. Crucially, **no `WakeTask` is emitted for idx 26 after that BLOCK** — and since
the block committed (state=BlockedOnRecv), any later `wake_task_v2` would have succeeded and
emitted one. ⇒ **No wake was attempted at all.** The poll's fd-waitqueue producers
(`wake_socket` → `wake_all` → `wake_task_v2`) never fired for the child's fd.

By elimination the break is **upstream of the poll wake**: either net_task never processed the
`exit\n` segment (NIC→net_task wake lost), or the data reached the socket but
`wake_sockets_for_tcp_slot`/`wake_socket` wasn't called (or the child was momentarily
deregistered), or the relay's PTY-master write didn't wake the right waiter. The wake chain
(`tcp.rs` sets `wake_slot` on every matched segment → `wake_sockets_for_tcp_slot` →
`wake_socket` → `wake_all`; mod.rs:369) and `sys_poll` re-register/rescan are all correct by
inspection — so it is a real intermittent producer-side miss, not a missing guard.

NB also observed: the **serial-console `ion` (pid 21)** busy-loops a synchronous `call` on
endpoint 4 (~1/ms) the whole time — a separate suspect worth its own look (a prompt read
should block, not spin).

### Wake-side trace — it is NOT a scheduler lost-wake; the relay's poll works

Added a `Wakeup` focus event (recorded unconditionally-when-armed) at the fd-waitqueue
producers — `wake_socket` (kind=0), `wake_master`/`wake_slave` (kind=2/3) — and re-captured.
This **overturns the lost-wake framing of the last four sessions**:

- The sshd child's poll wakes on a **100 ms timeout** (`WAKE(from_state=2)` → `BLOCK
  @poll:18493`, exactly 100 ms apart) and re-scans — so a lost *wake* cannot permanently hang
  it; a 100 ms timeout rescan would find any buffered data. The relay's `poll()` is working.
- `PRODUCER_WAKE socket/pty` events fire plentifully **and then stop** — after which the child
  poll-cycles forever finding nothing ready. ⇒ **The hang is that incoming data STOPS being
  delivered to the relay's socket**, not a missed wakeup.

### Ruled OUT (each by instrumentation/inspection this session)

| Hypothesis | Verdict | Evidence |
|---|---|---|
| Dispatch starvation (Ready-not-dispatched) | ❌ | tasks are `BlockedOnRecv`, cores idle (`ktrace states`) |
| Scheduler/IPC lost-wake on recv/reply | ❌ | `block_current_on_recv_v2`/`reply_v2` register-then-recheck (race-free) |
| `poll()` lost-wake | ❌ | poll wakes every 100 ms (timeout) + rescans; producer wakes fire |
| TCP out-of-order drop (no reassembly) | ❌ | added `[tcp] no-reassembly` drop log — **never fired** during the hang |
| TCP zero-window deadlock | ❌ | `rcv_wnd` is a constant `DEFAULT_WINDOW`; window never closes |

### Refined conclusion + next step

The bug is **incoming TCP data stops reaching sshd's relay socket** mid-session, while the
relay's poll keeps timing out on an empty socket. TCP is neither dropping (OOO log silent) nor
window-blocking it. The remaining suspect is the **net RX path**: the NIC-ISR→`net_task` wake
(`net_task` parks in `BlockedOnRecv`; woken lock-free from the virtio/e1000 ISR like the serial
feeder) or `net_task`'s segment processing. **NEXT:** instrument the NIC RX ISR wake of
`net_task` + `net_task`'s per-segment processing (does the `exit\n` segment reach
`handle_segment` for socket 2?), to see whether net_task stops being woken/scheduled or stops
processing. Only then is the true fix locatable. (NB: the four-session "scheduler lost-wake"
framing is now retired — the relay poll is healthy; the failure is upstream in net RX delivery.)

### Side fix landed this session

`tcp.rs`: an out-of-order/duplicate Established-state segment was dropped **silently with no
ACK**; now it sends a duplicate ACK for `rcv_nxt` (RFC 5681 §3.2) so the peer can fast-
retransmit, plus a rate-limited `[tcp] no-reassembly` drop log. Correct + low-risk, but
confirmed **dormant** in this hang (log count 0) — not the root cause; kept as a latent-
correctness improvement for lossy links.
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

---

# Update: net-RX trace overlay RETIRES the net-RX-delivery theory; hang is teardown-side (2026-05-29, sixth session)

This session built a **net-RX + device-IRQ + blk-completion focus-trace overlay**, captured the
hang on a **clean disk** (reproduced), and the data **retires the "incoming TCP data stops
reaching the relay" framing** of the fourth/fifth-session updates. It also definitively
**rules out a virtio-blk interrupt-storm** (a tempting but wrong lead this session chased and
disproved). Net result: the SSH `exit` hang is **teardown-side**, not an RX-delivery or
device-IRQ problem. The investigation is meaningfully *narrowed*, though not yet fixed.

## syslogd CPU (the htop observation) — CONFIRMED fixed, not spinning

Re-verified live on the current build under active SSH load: `/proc/2/stat`
(syslogd) delta = **utime 0, stime 14 jiffies / 8 s** (cumulative ~19 since boot). The **zero
userspace time** is the tell — a poll-loop busy-spin burns *utime*, and it is flat zero. The
committed fix (`a92486f`, live consuming `/proc/kmsg`) is working; the earlier "syslog uses a
lot of CPU in htop" was a **pre-fix** observation. No further action needed on syslogd.

## The trace overlay (kept as permanent, `trace`-gated tooling)

Reuses the ISR-safe, pid-filter-bypassing `TraceEvent::Wakeup { kind, id }` carrier (records
unconditionally while the focus ring is armed). New `kind` codes + formatter labels:

| kind | label | site | `id` meaning |
|---|---|---|---|
| 4 | `irq-fired` | `interrupts.rs` `dispatch_device_irq` | device IRQ vector (**all** devices — noisy; filter by vector) |
| 5 | `wake-net-task` | `virtio_net.rs` ISR | net_task id about to be woken |
| 6 | `recv-frames` | `virtio_net.rs::recv_frames` | frames pulled off the virtio RX used-ring this poll |
| 7 | `tcp-recv` | `tcp.rs::handle_segment` | in-order payload bytes (high bit set ⇒ OOO/dup drop) |
| 8 | `blk-drain` | `virtio_blk.rs` ISR | `1` = real completion, `0` = spurious blk IRQ |

Capture harness: `/tmp/ssh-repro/netrx_capture.sh` (login-retry to beat the boot-smoke
handshake race; **arm-verified-by-`ktrace len`-growth** — the interactive `arm` is flaky
because ion's *persisted history autosuggestion* corrupts fast-typed input, so retry until
`ktrace len` actually grows). Session A reproduces the hang; session B arms + `ktrace serial`
dumps the focus ring while wedged. **Vector map (from boot):** `0x60`=virtio-blk MSI-X,
`0x61`=virtio-net MSI-X, `0x63`/INTx legacy fallbacks.

## DECISIVE findings (clean-disk capture, hang reproduced)

1. **Net RX is HEALTHY through the freeze.** `recv-frames`, `tcp-recv`, `wake-net-task`, and
   socket-wake (`PRODUCER_WAKE socket`) all fire normally right up to the freeze. The `exit\n`
   **was** delivered — `ion` received it and **exited (zombie)**. ⇒ The fourth/fifth/sixth
   "incoming data stops reaching the relay / net-RX-delivery" conclusion is **RETIRED**: RX is
   not the failure. After the client sends `exit` it stops sending and waits for the server's
   FIN, so *no further RX is even expected* — the bug is the **server never completing
   teardown / sending FIN**.
2. **The "virtio-blk MSI storm on vector 0x60" is a CAPTURE ARTIFACT — not a bug.** The dump's
   silence-tail is dominated by hundreds of `irq-fired id=96` (blk MSI), which *looked* like a
   storm pinning cpu0. But the `blk-drain` discriminator shows **every one is a real completion
   (443× `id=1`, 0× `id=0`)** and the kernel logged **0** `[virtio-blk] completion poll …
   timeout` warnings. The source is **session B's own diagnostic disk I/O** — each `ktrace`/
   `ion`/`PROMPT` command re-execs and reads its binary from the ext2 disk (late pids
   31/34/37/39/42 = the five `ktrace` invocations). The every-device-IRQ marker (`kind 4`)
   amplified normal disk reads into an apparent storm. **Not the hang cause.**
3. **An MSI-X `ISR_STATUS` fix was TRIED and REVERTED.** Hypothesis: virtio-blk reads the legacy
   `VIRTIO_ISR_STATUS` register unconditionally under MSI-X (lines 634/686), whereas virtio-net
   was deliberately fixed *not* to (`USING_LEGACY_INTX` guard, `virtio_net.rs:591`) to avoid a
   QEMU/transitional-virtio edge-delivery quirk. Mirroring that guard for blk **did not change
   the behavior** (still hung, blk IRQs still all real completions) — consistent with the storm
   being benign disk I/O. Reverted. (The blk/net inconsistency is a *latent* tidy-up at most,
   **not** related to this hang.)

## What the capture DID establish about the freeze

At the wedged moment (all four cores idle, daemons `BlkRecv`):
- `userspace-init` (pid 1) **spin-dispatches** its reap/supervise loop ~1/tick, then **blocks**
  `BlockedOnRecv` at `kernel/src/arch/x86_64/syscall/mod.rs:3817`.
- The session's **sshd child + `ion` are `BlockedOnRecv`** (matches the original evidence #4 and
  the fifth-session `ktrace states` capture) — `ion` having already exited as a zombie.
- This is the **teardown** phase, *after* `exit` was delivered. The client hang = the server's
  per-connection sshd child never finishes `cleanup`/reap and never `close()`s `client_fd`
  (→ no FIN), exactly as the original evidence chain (#2/#3) described.

## Theories now RULED OUT (cumulative, across all sessions)

| Theory | Verdict | This session's evidence |
|---|---|---|
| net-RX delivery stops (4th/5th/6th update) | ❌ retired | RX healthy; `exit` delivered (ion exited) |
| virtio-blk MSI storm / lost completion | ❌ | 443 real completions, 0 spurious, 0 blk timeouts; source = session-B disk I/O |
| blk `ISR_STATUS`-under-MSI-X | ❌ | fix applied → no change → reverted |
| scheduler/IPC lost-wake on recv/reply | ❌ (prior) | register-then-recheck primitives are race-free |
| dispatch starvation (Ready-not-dispatched) | ❌ (prior) | wedged tasks are `BlockedOnRecv`, cores idle |
| syslogd RT busy-spin | ✅ fixed (`a92486f`) | 0 utime/8s, verified live |

## Recommended next steps (the teardown path is the remaining suspect)

1. **Arm the wedged actors directly.** Re-run `netrx_capture.sh` but `ktrace arm <sshd-child-pid>
   <ion-pid>` (find them via `ktrace tasks` after session A logs in — the non-listener `/bin/sshd`
   and its `/bin/ion`). With the **deep focus ring** recording *their* block/wake/dispatch +
   `BlockCurrent` call sites during `exit`, the dump will show the exact teardown block site that
   never wakes (vs. the fifth session, which only sampled state, and this session, which armed the
   wrong pids 1+3).
2. **Trace the sshd `cleanup` path in userspace** (`userspace/sshd/src/session.rs`): the original
   `log_sshd_step` trace showed teardown reaches `cleanup:reap shell` then stops. Re-instrument
   with the system now better understood — is the child stuck in `waitpid` on the `ion` zombie, in
   a `poll`, or in a synchronous IPC `write` (the original evidence #4's `write()` block)? The
   answer pins which kernel primitive loses the wake.
3. **Reaping angle:** `userspace-init` blocks at `syscall/mod.rs:3817` (the BlockedOnRecv site).
   Confirm whether the `ion` zombie is reaped by the sshd child or by init, and whether the
   reaper's wake (SIGCHLD/notification) is the lost edge. This is the most likely remaining
   mechanism class for a "BlockedOnRecv, no deadline, never woken" teardown actor.
4. **Note on tooling:** the `kind 4` every-device-IRQ marker is noisy (records all disk/NIC IRQs);
   keep it but **filter by `id` (vector)**. blk IRQs (`id=96`) during a capture are almost always
   the *control session's own* command-exec disk reads — do not mistake them for a fault.

---

# Update: HANG FIXED — async-rt edge-driven-stall, not a kernel lost-wake (2026-05-29, seventh session)

This session **fixed** the infinite hang, **confirmed syslog is a non-issue**, ran an
adversarially-verified kernel audit, and pinned the failure with a decisive ktrace capture.
Net: the bug was a userspace **edge-driven executor stall** in `async-rt`, not a kernel
lost-wake. The kernel mechanics the prior six sessions suspected are all sound.

## syslog CPU (the original side-observation) — CONFIRMED non-issue

Measured live on the current build (SSH in, sample `/proc/<pid>/stat` across an 8 s idle
window): **syslogd `utime=0`, `stime` Δ≈3 jiffies / 10 s ≈ 0.3 % CPU.** Not spinning. The
`a92486f` live-`/proc/kmsg`-cursor fix holds; a 4-track adversarial audit confirmed **no
feedback loop** (the write→ext2→blk→fsync path emits zero dmesg bytes) and **no continuous
idle kernel logging** (`max_level=Info`, timer/scheduler hot paths log nothing). The user's
"large CPU in htop" was a **stale pre-fix build**. NB: the sixth-session "verification" that
dismissed `stime` because `utime=0` was wrong reasoning (a `poll→read→poll` spin burns
*stime*, not utime) — but the measurement re-done here shows it genuinely isn't spinning.

## Two earlier mis-diagnoses corrected

- **`init` blocking at `syscall/mod.rs:3817` is NOT a wedge.** Line 3817 is `nanosleep`'s
  `block_current_until(BlockedOnRecv, Some(deadline))` — init's normal supervise-loop sleep,
  which wakes on its deadline. (Sixth-session update mislabeled it as the reap-block site.)
- **`write(STDOUT_FILENO)` for sshd is NOT a synchronous IPC.** A daemon inherits init's fd 1 =
  `FdBackend::Stdout` → `syscall/mod.rs:6397` writes to serial + framebuffer console (kernel-side,
  no IPC, no block). This **refutes original evidence #4** ("blocked on a lost `write()` reply").

## Adversarial kernel audit — mechanics are SOUND (each finding independently refuted/verified)

| Suspected kernel bug | Verdict |
|---|---|
| PTY `slave_refcount` off-by-one across fork/exit | ❌ refuted — `fork` bumps refs via `add_fd_refs`, exit decrements; `POLLHUP` at `slave_refcount==0 && slave_opened` is correct |
| `close_slave`→`wake_master` lost | ❌ — fires correctly; and the 100 ms re-poll re-reads `fd_poll_events` anyway |
| async-rt reactor registration gap (waker lost between iterations) | ❌ refuted — wakers are a persistent `Arc<TaskHeader>`; `interests` persist (no deregister) |
| `wake_all`→`wake_task_v2` SMP ordering | ⚠️ doc-gap only — `pi_lock` CMPXCHG already provides the barrier; bounded µs latency, not a 100 s hang |
| watchdog skips `BlockedOnRecv`-no-deadline | intentional (servers block in recv by design); re-waking would be a symptom-mask with broad blast radius |

## DECISIVE capture (hang reproduced; armed on the *real* pid; host draining)

Harness: boot → SSH session A logs in → discover the session-child pid from sshd's **own serial
log** (`run_session:start pid=N`) — robust, since kernel task names are all `"fork-child"` →
`ktrace arm N` → send `exit` → **keep draining** session A's pty for 14 s (required to trigger
the bug) → SSH session B dumps `ktrace serial`/`states`. Result:

- `ktrace states` during the hang: `idx=25 pid=21 state=BlkRecv core=2` — session A's child wedged.
- Focus ring: `idx=25` **cycles `BLOCK @syscall/mod.rs:18493` (the reactor's `poll_once(100)`)
  every exactly 100 ms**, waking on timeout (`from_state=2`), re-scanning, re-blocking — forever.
  So it is **NOT a lost wake** (poll cycles fine); it is that **no registered fd ever reports
  ready**, so the executor's `poll_once(100)` always times out with `run_queue` empty.
- The sshd teardown log shows `cleanup:start … cleanup:done pid=21` appearing **only after**
  session B connects (long after the `states` snapshot) — i.e. **session B's activity unstuck
  session A.** Classic Heisenbug.

### Why "no fd ready" ⇒ permanent stall (the actual mechanism)

The sshd session child is a single process running an `async-rt` executor with four tasks
(root `async_session`, `io_task`, `progress_task`, `channel_relay`). Shell-exit detection is
the relay's `waitpid(WNOHANG)` backstop (`session.rs:1010`) plus two others — but **a task only
runs when its waker fires.** When `poll_once(100)` times out with no fd ready, **no task is
woken, so none of the three `waitpid` backstops execute.** ion's exit *should* make the PTY
master report `POLLHUP`, but the captured behavior shows the relay's poll set never surfaces a
ready fd during the hang (a transient missed edge — the exact lost edge is bounded to a window
the 100 ms re-poll should have caught but, intermittently, doesn't). Because the executor was
**purely edge-driven**, that single missed edge stalls the whole session **forever**.

## The fix (verified) — idle-liveness backstop in the executor

`userspace/async-rt/src/executor.rs` `block_on`: track consecutive empty 100 ms reactor polls;
after `LIVENESS_STALL_TICKS = 3` (~300 ms of zero progress) call new `Executor::requeue_all()`
— mark every live task woken + queued and wake the root — so each task **re-polls from its
await point**. The relay's `WaitWake` future returns `Ready` once `registered`, so the relay
reaches its `waitpid(WNOHANG)` backstop, reaps the zombie shell, sets `session_done`, and
teardown (`cleanup` → `close(sock_fd)` → FIN) completes within ~300 ms — converting a permanent
hang into bounded recovery **regardless of which edge was missed**. `TaskHeader::mark_woken()`
added in `task.rs`. Reset on any progress, so normal event-driven operation is unchanged; only
a genuine ≥300 ms stall triggers the backstop (then ~3 Hz re-poll until it clears).

Also: fixed a **pre-existing** broken async-rt host test (`test_spawn_during_poll_is_not_dropped`
used `ran` after move — it never compiled, and `cargo xtask check` doesn't run async-rt tests so
it slipped through) and added a regression test `test_idle_liveness_backstop_recovers_lost_wake`
that models a spawned task with a lost readiness edge and asserts the backstop recovers it.

### Verification

- `cargo test -p async-rt` — **67/67 pass** (incl. the new regression test).
- `cargo xtask check` — clippy `-D warnings` clean, fmt clean, all host tests pass.
- `scripts/ssh_session_exit_test.sh virtio exit` ×5 — **0/5 HUNG** (was ~100 % hang before).
  The client now always disconnects (no `rc=10`). Expectation: the same backstop also cures the
  "every-other interactive command stalls until next input" responsiveness symptom (same class).

## RESIDUAL (separate, lower priority) — teardown is not a *clean* SSH logout (ssh exit 255)

Post-fix the client always disconnects, but `ssh` reports **exit 255 / "Connection closed by
remote host"** (test `rc=8`), not a clean exit 0. This is a **teardown-protocol** gap, not a
hang: the server completes teardown and `close(sock_fd)`s the raw socket **without** an orderly
SSH close. It predates the hang (the flow hung before reaching any close, so the clean close was
never exercised). A tried best-effort fix — `runner.channel_done(handle)` → `progress()` →
flush before `close(sock_fd)` in `async_session` teardown — **did not change the 255** and was
**reverted** (it ventured into sunset/TCP-close territory with no observable benefit). Likely
cause: kernel TCP `close()` emits **RST** when unread data sits in the socket receive buffer
(client's `exit\n`/trailing bytes), or a missing `SSH_MSG_DISCONNECT`. **Next step for a clean
exit 0:** confirm RST-vs-FIN on `close()` with pending RX (kernel `net/tcp` close path) and/or
drain the socket + send `SSH_MSG_DISCONNECT` before close. Low user impact (terminal returns
immediately; 255 is cosmetic), so deprioritized below the now-fixed hang.

## Tooling added this session (host-side, under /tmp during the investigation)

- syslog CPU sampler (boot → SSH → sample `/proc/<pid>/stat` idle delta; slow-typed to defeat
  ion autosuggestion, time-drained reads).
- ktrace hang-capture harness (pid discovery from sshd serial log + host-draining + two-session
  arm/dump) — the pattern that finally produced a clean armed-on-the-right-pid capture.
- `verify_fix.sh` — runs `ssh_session_exit_test.sh virtio exit` N× and tallies clean/hung.
