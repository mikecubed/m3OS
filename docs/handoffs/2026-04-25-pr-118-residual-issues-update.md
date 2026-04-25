# Handoff Update: PR #118 residual issues (2026-04-25 second pass)

**Status:** Both Issue 1 (SSH disconnect hang) and Issue 2 (`sys_nanosleep`
busy-yield) remain open after a deep investigation pass. The original
handoff at `docs/handoffs/2026-04-25-pr-118-residual-issues.md` is still
accurate but its hypotheses for Issue 1 turn out to be incomplete — the bug
is deeper than the cleanup `waitpid`. This document records what was tried,
what was learned, and what the next agent should attempt.

**Branch:** `feat/phase-55c-ring3-driver-closure`

**This pass landed only one change:** the `cleanup` function in
`userspace/sshd/src/session.rs` now sends `SIGHUP` first, polls `waitpid`
with a 500 ms grace period, and escalates to `SIGKILL` if the shell is
still alive. This is the right design pattern (defense-in-depth so the
cleanup always completes in bounded time) but **does NOT fix the SSH
disconnect hang** — the hang occurs before the cleanup reaches its
escalation step, in some cases even before the cleanup function runs at
all.

The Issue 2 (`sys_nanosleep`) experiment was reverted because it
reproduced the previously-documented "second wake silently fails"
failure mode.

---

## Issue 1: SSH disconnect hang — deeper than the cleanup path

### What the original handoff said

> The blocking `waitpid(pid, &mut status, 0)` on line 1474 is the suspect.
> The shell (`/bin/ion`) was sent `SIGHUP` and is expected to exit, but it
> doesn't — so the blocking `waitpid` never returns and `close(sock_fd)`
> never runs.

This was a reasonable starting hypothesis based on the original log.
It is incomplete.

### What experimental diagnostics actually show

I ran the reproduction script `scripts/ssh_session_exit_test.sh virtio exit`
under five progressively-stripped `cleanup` implementations:

| Variant | Cleanup body | SSH client outcome |
|---|---|---|
| Baseline (HEAD) | `kill(SIGHUP)` then blocking `waitpid` | hang |
| SIGTERM swap | `kill(SIGTERM)` then blocking `waitpid` | hang |
| Grace + SIGKILL escalation (current) | SIGHUP → 500 ms WNOHANG poll → SIGKILL | hang |
| SIGKILL only | `kill(SIGKILL)` then blocking `waitpid` | hang |
| Skip kill, blocking waitpid removed | only logging | hang |
| **Pure NOOP** (`let _ = ...`) | nothing at all | **still hang** |

A NOOP `cleanup` still leaves the SSH client hanging. So the hang is not
in `cleanup` itself.

I then added sentinel writes around `run_session`'s exit path:

```rust
write_str(STDOUT_FILENO, "@A1\n");
let exit_code = state.borrow().exit_code;
write_str(STDOUT_FILENO, "@A2\n");
let shell_pid = state.borrow().shell_pid;
write_str(STDOUT_FILENO, "@A3\n");      // never prints
let pty_master = state.borrow().pty_master;
write_str(STDOUT_FILENO, "@A4\n");
...
```

`@A1` and `@A2` print. `@A3` never prints. The hang is between the first
and second `state.borrow()` calls.

Combining all four borrows into a single block:

```rust
write_str(STDOUT_FILENO, "@A1\n");
let (exit_code, shell_pid, pty_master, pty_slave) = {
    let s = state.borrow();
    (s.exit_code, s.shell_pid, s.pty_master, s.pty_slave)
};
write_str(STDOUT_FILENO, "@A2\n");
cleanup(shell_pid, pty_master, pty_slave);     // first instruction: log_sshd_step()
write_str(STDOUT_FILENO, "@A6\n");
```

`@A1` and `@A2` print but the first `log_sshd_step` inside `cleanup` does
not. So the hang is between `@A2` (a successful write) and `cleanup`'s
first instruction — that is to say, *during a plain function call with no
intervening syscall*.

After `@A2`, the kernel produces **zero log entries** for 15 s — no
`[INFO]`, no `[WARN]`, no `cpu-hog`/`stale-ready` warnings. Total
silence until QEMU is killed by the script's trap. The sshd-child
process (pid 14) is neither running (cpu-hog warnings would fire) nor
producing syscalls.

### NEW: deeper diagnosis — likely a kernel scheduler regression

A second pass with finer-grained instrumentation localized the hang
further:

1. **`session_done` is set by io_task at the `n == 0` path**
   (`userspace/sshd/src/session.rs:~467` — the socket-EOF branch). The
   SSH client closes the TCP connection AFTER `exit\n` is forwarded to
   the PTY (most likely because ion's exit fires SSH-level events that
   the OpenSSH client interprets as session close). io_task sees
   `read(sock_fd) == 0`, sets `session_done = true`, signals
   `session_notify`, returns.

2. **The main loop wakes and breaks correctly**. With a single
   sentinel logging `main:break_session_done` right inside the
   `if state.borrow().session_done { break; }` block, the log fires
   reliably.

3. **Right after that log, sshd-child (pid 14) stops executing**. The
   very next syscall — even something as simple as
   `syscall_lib::write(2, b"@PRE_BREAK\n")` placed immediately after
   the break-detection log — never produces output. The kernel log
   goes silent for 15 s, no `cpu-hog`, no `stale-ready`, no
   `[signal] [pX] killed by signal`, no `take_current_group_exit_request`
   firing for pid 14. QEMU is then killed by the test trap.

4. **Replacing the post-loop body with an immediate `close(sock_fd);
   syscall_lib::exit(0);` does not help** — the close syscall and
   the exit syscall both fail to dispatch.

5. With the kernel signal-delivery debug logs promoted to `info!`
   (now permanent in this PR), the only signal targeted at pid 14 is
   SIGCHLD from ion's exit, with `interrupt=false`. SIGCHLD has
   default disposition Ignore for sshd, so `check_pending_signals`
   should dequeue and continue. A guarded "looping?" warning at
   iter > 50 in `check_pending_signals` did not fire either.

6. **Conclusion**: pid 14 ends up in a Blocked or descheduled state
   that the scheduler never wakes. Since no `[signal]`, `cpu-hog`,
   `stale-ready`, fault, or panic event fires, the path is silent —
   suggesting either (a) the task was incorrectly transitioned to a
   Blocked variant by a code path that doesn't log, (b) the task is
   on a wait queue that nothing signals, or (c) a context-switch
   went wrong (e.g., saved RSP corruption pointing into invalid
   memory, where the task doesn't fault but also doesn't make
   progress).

Strong candidates the next agent should investigate:

- **The PENDING_SWITCH_OUT / wake_after_switch interaction** — same
  family of bugs as Issue 2 (post-mortem item 1). After the
  log-syscall return path, the kernel might leave pid 14 with
  `switching_out=true` or `wake_after_switch` mismatched, never
  enqueuing back. A scheduler-state dump of pid 14 at the moment
  of hang (e.g., by adding a periodic [WARN] about
  `pid=14 state=? wake_deadline=?`) would settle this immediately.
- **A spawned task in `runner.lock().await` holding a Mutex
  invariant**. If `progress_task` or `channel_relay_task` is mid-
  borrow on `state` or holding the `Mutex<Runner>`, main's next
  syscall might re-enter the executor in a way that yields to a
  task that never returns. Hard to verify without async-rt
  instrumentation.

### Other plausible mechanisms (kept from prior pass)

The most plausible candidates, in priority order:

1. **`sshd-child` is being terminated by a signal between syscalls.**
   `check_pending_signals` (kernel/src/arch/x86_64/syscall/mod.rs:1724)
   runs after every syscall return. If a signal with default disposition
   `Terminate` was queued on pid 14, it would call `sys_exit` and the
   process would die silently between `@A2`'s write and the next
   instruction. Default-disposition `[p<pid>] killed by signal <n>` log
   is at `log::debug!` level and would not appear in the default
   `INFO`-level kernel log. Look for SIGHUP from PTY close, SIGCHLD
   handlers, or any path that signals pid 14's process group (pgid is
   inherited from sshd-master = pid 3).

2. **PTY foreground process group is unset.** sshd never calls
   `TIOCSPGRP` on the master after spawning the shell, so the slave's
   `slave_fg_pgid` is 0. When `close_master` (kernel/src/pty.rs:88)
   runs, it sends SIGHUP+SIGCONT to `slave_fg_pgid=0`, which is a no-op.
   This may or may not be relevant to the hang but is definitely a
   correctness bug worth fixing in its own right (it explains why ion
   never receives the natural SIGHUP-on-disconnect that POSIX shells
   rely on).

3. **A hidden `state.borrow_mut()` is held when the main loop tries
   `state.borrow()`.** RefCell would panic, not hang — but if the panic
   handler itself is broken under m3OS (musl dynamic-link mismatch,
   abort() into an aborted state), the process could end up in a state
   that doesn't produce log output. Several `state.borrow_mut()` sites
   are in async-task tail paths; if the borrow extends across an
   `await`, that's UB by Rust rules but may not surface immediately.

4. **The async-rt executor stalls.** Less likely (async-rt is
   single-threaded inside one process and tasks yield cooperatively),
   but worth verifying that `block_on` doesn't have a silent yield path
   that never resumes.

### What is currently committed

The cleanup function (userspace/sshd/src/session.rs:1465-1514) now
implements the SIGHUP→grace→SIGKILL escalation. This is a defensive
improvement that is correct in its own right but does not move the
needle on the disconnect hang. **Do not revert it** when investigating
further — the previous unconditional blocking `waitpid` is strictly
worse.

### Suggested next steps

1. **Bump kernel log level to DEBUG** for the reproduction. The kernel
   has `log::debug!("[p{}] killed by signal {}", pid, signum)` at
   syscall/mod.rs:1738 which would tell us if pid 14 is being signaled
   to death. The current INFO-level log hides this.

2. **Add a TIOCSPGRP call** from sshd's spawn_shell path right after
   the child fork, setting the slave's foreground pgid to the shell's
   pgid (= shell_pid since the shell calls setsid). This will let
   `close_master` actually deliver SIGHUP to ion when sshd closes the
   master, which is the canonical Unix shutdown path.

3. **Add panic instrumentation in sshd**. Wrap the post-loop block in a
   manual sentinel sequence that tries to detect both panic abort and
   silent-kill paths. Even a single `write_str(STDERR_FILENO, "POST_LOOP\n")`
   followed by an `unreachable!()` after `exit()` would tell us whether
   the process is dying or just stalling.

4. **Check the kernel's signal queue when the hang first appears**.
   Add a guarded log inside `check_pending_signals` that always reports
   when pid 14 dequeues *any* signal. That collapses hypothesis (1) to
   a yes/no question.

### Files touched in this pass

- `userspace/sshd/src/session.rs` — `cleanup` function with SIGKILL
  escalation. Compiles cleanly.
- `kernel/src/arch/x86_64/syscall/mod.rs` — promoted two
  `log::debug!` to `log::info!` for `[p<pid>] killed/stopped by
  signal X`. This makes default-disposition Terminate/Stop deliveries
  visible in regression logs without needing a global log-level bump.

### Files NOT touched but considered relevant

- `userspace/sshd/src/session.rs:~762` (after `shell_spawned=true`) —
  TIOCSPGRP fix point.
- `kernel/src/arch/x86_64/syscall/mod.rs:1738` —
  `[p<pid>] killed by signal <n>` debug log.
- `kernel/src/pty.rs:88-117` — `close_master` SIGHUP delivery path.

---

## Issue 2: `sys_nanosleep` busy-yield — second-wake bug reproduces

### What I tried

Replaced the long-sleep busy-yield in
`kernel/src/arch/x86_64/syscall/mod.rs:3174-3191` with
`block_current_unless_woken_until` (`kernel/src/task/scheduler.rs:1230`),
using a stack-local `AtomicBool::new(false)` so the only wake source is
the scheduler's deadline scan. Compile clean. Tested with
`cargo xtask regression --test serverization-fallback`.

### Result

The regression failed: `service: timed out waiting for 'net_udp' to stop`.
This is the same "second-wake silently fails" symptom the post-mortem
(`docs/post-mortems/2026-04-24-ingress-task-starvation.md`, item 1)
already records from the previous attempt. My naïve port did not
discover anything the post-mortem hadn't.

### Suggested next steps

A subagent's analysis (in this session's transcript) traced the
scheduler interactions in detail. The most plausible mechanism it
identified:

The `wake_after_switch` flag is consumed only by the dispatch loop's
switch-out handler (kernel/src/task/scheduler.rs:2070-2090) for the
*current* task being switched out. If
`scan_expired_wake_deadlines` (~2185) sets `wake_after_switch = true`
for a task that is NOT the one currently being switched out — i.e. the
task already finished its switch-out in an earlier dispatch cycle and
is now sitting Blocked with `switching_out == false` — the flag never
fires and the wake is lost.

That is, the scan's "task is mid-switch-out" path at lines 2185-2188
is correct only when the switch-out handler is about to run for that
specific task on this core. In a re-block scenario (task wakes from
D1, runs, re-blocks for D2 on a fresh `block_current_unless_woken_until`
call), the handler-pending-flag-consume invariant is fragile.

The subagent's primary fix: rewrite the counter-mirroring at
scheduler.rs:1265-1269 to always-decrement-old-then-always-increment-new
to avoid any drift on edge transitions. This may not be the root cause
but is a low-risk hardening worth landing first.

The subagent's alternative fix: clear `switching_out=false` *before*
acquiring the lock in `block_current_unless_woken_inner`, then set
`switching_out=true` only after `set_current_task_idx(None)`. This
would close the race where scan finds `switching_out=true` for a task
that already completed its previous cycle's switch-out.

### What is currently committed

Nothing for Issue 2 — the change is reverted.

### Files touched & reverted

- `kernel/src/arch/x86_64/syscall/mod.rs:3174-3191` — restored to
  busy-yield baseline.

---

## Summary punch list for the next agent

Updated based on the second-pass diagnosis above:

- [ ] **Highest yield**: add a scheduler-state diagnostic that
  periodically logs all tasks' (pid, state, wake_deadline,
  switching_out, wake_after_switch). Kick off the reproduction; when
  sshd hangs, the logs will show pid 14's exact state and any
  conflicting flags. This will distinguish "stuck on wait queue"
  from "incorrectly Blocked" from "saved RSP corrupt".
- [ ] **Second**: instrument the executor (`async-rt` block_on /
  poll_spawned_tasks) to log when a poll begins/ends per task. If
  any spawned task hangs in poll, the executor itself stalls.
- [ ] **Third**: try the always-decrement/always-increment counter
  rewrite at `kernel/src/task/scheduler.rs:1265-1269` for Issue 2.
  Even if it doesn't fix Issue 2 by itself, it's a hardening that
  may also affect the SSH hang if both share the same root cause.
- [ ] Add TIOCSPGRP call in sshd spawn_shell to register the shell as
  the PTY foreground process group. Independent correctness fix.
- [ ] After EITHER issue is resolved, add a regression test wired into
  `cargo xtask regression` so the bug cannot silently regress again.
- [ ] Update `docs/post-mortems/2026-04-24-ingress-task-starvation.md`
  Resolution section to record either (a) Issue 2 closure, or (b) the
  specific mechanism that's blocking it if still open.

## Build status

```
$ cargo xtask check
check passed: clippy clean, formatting correct, kernel-core, passwd, and
driver_runtime host tests pass
```

The pre-push hook should still pass (apart from the documented Issue 2
flakiness, which is unchanged from baseline).
