# Handoff: PR #118 residual issues — SSH disconnect hang + nanosleep starvation

**Status:** Open. Both issues block PR #118 ("Phase 55c: close ring-3 driver
correctness gaps") from being merge-ready.

**Branch:** `feat/phase-55c-ring3-driver-closure` (HEAD: `f877af7`)

**You are an LLM agent picking this up cold.** Read this whole document before
running anything; it tells you what to reproduce, what's already been tried,
and what the acceptance criteria are.

---

## Issue 1 (PR-blocker): SSH disconnect hangs the client

### Symptom

After a successful interactive SSH session, **both `Ctrl-D` and the `exit`
shell built-in cause the OpenSSH client to hang indefinitely** instead of
returning. Affects both NIC drivers (virtio-net default and ring-3 e1000 via
`--device e1000`), so the bug is in the userspace `sshd` (or its kernel
collaborators), not the NIC path.

This is not a new regression from the post-mortem fix that landed in `dcb3401`
— the same hang reproduces on virtio-net, which `dcb3401` does not touch. It
is a residual sshd lifecycle bug that the recent commit
`f8338e9 fix(sshd): detect shell exit from all three async tasks`
attempted but did not finish.

### Reproduction

Two reproduction scripts live in the repo:

- `scripts/ssh_full_session_test.sh <e1000|virtio>` — full pty-driven
  session, ends with Ctrl-D (`\x04`).
- `scripts/ssh_session_exit_test.sh <e1000|virtio> <ctrld|exit>` — same
  shape, picks the disconnect verb explicitly.

Both:

1. `cargo xtask run [--device e1000]` in the background
2. Wait for `sshd: listening on port 22`
3. `ssh -p 2222 root@127.0.0.1`
4. Authenticate (password: `root`)
5. Wait for shell prompt
6. Run `whoami` and `ls /bin` — both return cleanly
7. Send Ctrl-D (or `exit\n`)
8. Expect: ssh client exits within a few seconds with status 0
9. Observed: ssh client hangs; the script kills the QEMU process group
   after 10 s and exits with status 10.

Manual reproduction (no script):

```sh
cd /home/mikecubed/projects/ostest-wt-int-phase55c
cargo xtask run > /tmp/qemu.log 2>&1 &        # virtio-net default
# wait until /tmp/qemu.log shows "sshd: listening on"
ssh -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -p 2222 root@127.0.0.1
# password: root
# at the prompt, type Ctrl-D (or `exit`)
# observe: ssh hangs; on another terminal, ssh -O exit or kill the ssh PID
```

### What the kernel/sshd log shows on a hang

Last lines from `target/.../qemu.log` after sending `exit\n` over a working
session:

```
sshd: channel_relay:chan->pty bytes pid=14 count=1
sshd: channel_relay:skip wait pid=14 count=1
sshd: cleanup:start pid=14
sshd: cleanup:kill shell pid=14 child_pid=15
```

**Critically absent** (these would normally follow inside `cleanup`):

- `sshd: cleanup:waitpid shell` (logged at `userspace/sshd/src/session.rs:1475`
  after `waitpid(pid, &mut status, 0)` returns)
- `sshd: cleanup:close pty_master`
- `sshd: cleanup:close pty_slave`
- `sshd: cleanup:done`

**Reading the source at `userspace/sshd/src/session.rs:1466-1488`:**

```rust
fn cleanup(shell_pid: Option<isize>, pty_master: Option<i32>, pty_slave: Option<i32>) {
    if shell_pid.is_some() || pty_master.is_some() || pty_slave.is_some() {
        log_sshd_step("cleanup:start");
    }
    if let Some(pid) = shell_pid {
        log_sshd_step_u64("cleanup:kill shell", "child_pid", pid as u64);
        syscall_lib::kill(pid as i32, 1); // SIGHUP
        let mut status: i32 = 0;
        waitpid(pid as i32, &mut status, 0);   // ← blocks here forever
        log_sshd_step_u64("cleanup:waitpid shell", "status", status as u64);
    }
    ...
}
```

**Diagnosis.** `cleanup:start` and `cleanup:kill shell` fire, but the next log
line never appears. The blocking `waitpid(pid, &mut status, 0)` on line 1474
is the suspect. The shell (`/bin/ion`) was sent `SIGHUP` and is expected to
exit, but it doesn't — so the blocking `waitpid` never returns and
`close(sock_fd)` (line 261, the path that delivers TCP FIN to the client)
never runs.

### What's already in place that didn't fix it

`f8338e9 fix(sshd): detect shell exit from all three async tasks` added
`waitpid(pid, &mut status, WNOHANG)` polls inside three async tasks:

- `io_task` — `userspace/sshd/src/session.rs:295-314`
- `progress_task` — `userspace/sshd/src/session.rs:514-528`
- `channel_relay_task` — `userspace/sshd/src/session.rs:945-972`

Each, when it sees the shell has exited, sets `session_done = true`, signals
`session_notify`, and the main loop wakes to call `cleanup` + `close(sock_fd)`.
That fix only applies *after* the shell has exited on its own. The bug we are
chasing is upstream of that: the shell is **not exiting** in response to
either user-side EOF or a SIGHUP from `cleanup`.

### Hypothesis space (in order of likelihood)

1. **`/bin/ion` does not treat SIGHUP as fatal.** Most shells default to
   exit-on-SIGHUP, but `ion` may have ignored or trapped it. Check
   `userspace/ion/src/...` for signal handling. Easy verification: change the
   `cleanup` signal from `SIGHUP (1)` to `SIGTERM (15)` (or `SIGKILL (9)` as a
   blunt instrument) and re-run the test — if disconnect works, the shell's
   SIGHUP handling is the bug.

2. **The PTY layer doesn't translate Ctrl-D (`\x04`) to EOF on read.** In
   POSIX, the canonical-mode line discipline maps `VEOF` (default `\x04`) to
   end-of-input on a read. m3OS's PTY may not do this. Check the pty driver
   (`kernel/src/pty/...` or `userspace/pty/...`, whichever owns line
   discipline) for `VEOF` handling. Verification: `cat | hexdump -C` over the
   SSH session, type Ctrl-D, see whether `cat` exits.

3. **The shell sees EOF on stdin but its read loop doesn't propagate.** If
   `ion`'s REPL catches EOF and prints a prompt instead of exiting, neither
   user-side Ctrl-D nor user-typed `exit` will close the session unless `ion`
   handles `exit` as a built-in. Verification: in a *kernel-direct* shell
   (no SSH), boot, log in via the framebuffer/serial console, run
   `cat`, hit Ctrl-D — does it exit? Does typing `exit` at the prompt do
   anything?

4. **`sshd`'s `exit` path doesn't run because the shell never sees the bytes.**
   The log shows `chan->pty bytes count=1` then immediately `cleanup:start`
   — only one byte forwarded. Could be a `channel_relay` early-exit. Re-read
   `userspace/sshd/src/session.rs` from line 940 forward; specifically check
   what makes `session_done = true` fire after just one chan->pty write.
   Some of the 20 `session_done = true` sites in that file (grep with
   `grep -n "session_done = true" userspace/sshd/src/session.rs`) might be
   over-eager.

5. **The post-cleanup `close(sock_fd)` does send FIN but the client expects
   `SSH_MSG_CHANNEL_CLOSE` first.** Even an unexpected TCP close should
   make the OpenSSH client exit (with non-zero status, but it should *exit*).
   Worth verifying with `tcpdump -i lo port 2222` while the test runs:
   does the guest send a FIN at all? If yes, the client should be exiting and
   the hang is on the client side (less likely). If no, the kernel never
   reaches `close(sock_fd)` (matches hypothesis 1 / 4).

### Investigation plan

1. **Run `tcpdump -i lo -w /tmp/ssh.pcap port 2222` on the host** while the
   reproduction script runs. Inspect afterwards in Wireshark or
   `tshark -r /tmp/ssh.pcap`. The presence/absence of FIN tells you whether
   the kernel reaches `close(sock_fd)`.

2. **Add temporary instrumentation** to `cleanup`:
   ```rust
   log_sshd_step_u64("cleanup:waitpid_about_to_block", "child_pid", pid as u64);
   waitpid(pid as i32, &mut status, 0);
   log_sshd_step_u64("cleanup:waitpid_returned", "status", status as u64);
   ```
   If `waitpid_about_to_block` fires but `waitpid_returned` doesn't, the
   shell isn't dying. Confirms hypothesis 1.

3. **Try `SIGTERM` (15) or `SIGKILL` (9)** in `cleanup` instead of `SIGHUP`
   (1). This is the highest-signal experiment. If switching the signal
   resolves the hang, file the bug under "ion does not exit on SIGHUP" and
   land the SIGTERM change.

4. **Test the kernel-direct shell first.** `cargo xtask run` with no SSH
   client involvement. Log in via serial, run `cat`, hit Ctrl-D, see whether
   `cat` exits. Then type `exit` at the shell — does the shell exit? If the
   serial-console shell can't be exited either, the bug is in
   `ion`/`pty`, and SSH is downstream.

5. **Once the shell exits cleanly via Ctrl-D and `exit` on the local
   console**, retest SSH. If SSH still hangs, the residual bug is in the
   `cleanup` order: probably `close(sock_fd)` should run *before*
   `waitpid(pid, 0)` (or `waitpid` should be `WNOHANG` after a brief grace
   period).

### Acceptance criteria

1. `scripts/ssh_session_exit_test.sh virtio exit` exits 0 deterministically
   (5/5 runs).
2. `scripts/ssh_session_exit_test.sh virtio ctrld` exits 0 deterministically.
3. `scripts/ssh_session_exit_test.sh e1000 exit` exits 0 deterministically.
4. `scripts/ssh_session_exit_test.sh e1000 ctrld` exits 0 deterministically.
5. The "ssh client exited with status 0" line appears in
   `/tmp/m3os-ssh-*-*/ssh-session.log`.
6. Add a regression test (probably `ssh-disconnect`) to the
   `cargo xtask regression` suite that exercises full SSH login + `exit`
   over virtio-net so the disconnect bug can never silently regress again.
7. Update `docs/post-mortems/2026-04-24-ingress-task-starvation.md` (the
   "Resolution" section) noting the SSH disconnect issue is now fixed.

### Useful greps and entry points

```sh
# All sshd session-done assignments — start audit here
grep -n "session_done = true" userspace/sshd/src/session.rs

# Cleanup function
sed -n '1465,1490p' userspace/sshd/src/session.rs

# Main loop's exit path that calls cleanup
sed -n '240,265p' userspace/sshd/src/session.rs

# SIGHUP signal sites
grep -rn "SIGHUP\|kill.*1)\|signal::SIGHUP" userspace/ kernel/ kernel-core/

# ion shell signal handling
find userspace/ion -name '*.rs' | xargs grep -ln "SIGHUP\|signal_action\|sigaction" 2>/dev/null
```

---

## Issue 2 (separate but related): `serverization-fallback` flakiness from
## `sys_nanosleep` busy-yield

### Status

Pre-existing. ~40% pass rate on the base commit (`9881e8f`), unchanged by the
fix in `dcb3401`. This is **item 1** from the existing post-mortem at
`docs/post-mortems/2026-04-24-ingress-task-starvation.md` ("what the real fix
would look like"). Items 2 and 3 from that post-mortem are now done; item 1
is what's left.

The new pre-push hook (`f877af7`) re-runs failing tests in isolation, so
this is no longer push-blocking — but it remains a real bug.

### Root cause (per the post-mortem)

`sys_nanosleep`'s long-sleep branch is:

```rust
} else {
    // Long sleep (≥ 5 ms): yield-based sleep.
    crate::task::yield_now();
    if has_pending_signal() { return NEG_EINTR; }
    let sleep_tsc = sleep_us.saturating_mul(tsc_per_ms) / 1_000;
    let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };
    while unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start_tsc) < sleep_tsc {
        crate::task::yield_now();
        if has_pending_signal() { return NEG_EINTR; }
    }
}
```

Located at `kernel/src/arch/x86_64/syscall/mod.rs:3174-3191`.

The `while rdtsc < deadline: yield_now()` busy-yield consumes 100% of PID 1's
CPU slice. Init's `stop_service` polling loop (`userspace/init/src/main.rs`,
the SIGTERM/waitpid/nanosleep cycle) makes too little forward progress
between sleeps to consistently process `/run/init.cmd` within the test's 30s
budget.

### What was tried, and why it isn't landed

The post-mortem records an attempt to replace the busy-yield with
`block_current_unless_woken_until` (the primitive already exists in
`kernel/src/task/scheduler.rs:1230`). Result: PID 1 woke from the *first*
deadline, but a *follow-up* 1-second sleep silently failed to wake. The
follow-up needs to audit `scan_expired_wake_deadlines` and the dispatch
post-switch path for PID-1-specific behavior — initial block-and-wake works,
subsequent cycles on the same task don't.

### Investigation plan

1. **Reproduce the second-wake failure deterministically.** Wire
   `block_current_unless_woken_until` into `sys_nanosleep`'s long branch
   and run `cargo xtask regression --test serverization-fallback`. Capture
   the QEMU log; look for the second `nanosleep` call from PID 1
   (the SIGTERM-to-SIGKILL gap in `init::stop_service`) and see whether the
   task transitions Ready → Running after the deadline expires.

2. **The wake-deadline counter is the prime suspect.** Read
   `kernel/src/task/scheduler.rs:1237-1284` (`block_current_unless_woken_inner`)
   carefully. The `ACTIVE_WAKE_DEADLINES` counter is *supposed* to be a
   fast-path gate for `scan_expired_wake_deadlines`; check whether the
   transitions during a second `block_current_unless_woken_until` on the
   same task correctly increment the counter when the previous deadline
   was already cleared. The `take()` calls in
   `scan_expired_wake_deadlines` (lines 2177, 2182) and `wake_task` (line
   1415) both decrement; verify all three paths agree.

3. **Audit `Task::wake_after_switch` interaction with re-blocks.** When a
   task wakes via `wake_task` while `switching_out` is true, it sets
   `wake_after_switch`. The dispatch loop's switch-out handler at
   `kernel/src/task/scheduler.rs:2028-2093` consumes that flag. Race
   between the deadline scan, the switch-out handler, and the next
   re-block on the same task is plausible.

4. **Per-core affinity sanity check.** The post-mortem notes PID 1 and the
   ingress task landed on the same core 0. With the ingress task gone,
   PID 1 should be alone on core 0 most of the time, but the cpu-hog
   warnings in the latest test logs (`pid=1 ran~430ms`) suggest PID 1 is
   still busy-yielding for hundreds of milliseconds at a stretch. That's
   the busy-yield itself; switching to `block_current_unless_woken_until`
   should drop the task off the run queue entirely.

### Acceptance criteria

1. `cargo xtask regression --test serverization-fallback` passes 10/10 in
   isolated runs.
2. `cargo xtask regression --test serverization-fallback` passes 10/10 in
   suite runs (with the new pre-push hook's isolation retry, this is the
   suite-level expectation; without the retry it would be 8-9/10 due to
   QEMU contention).
3. `kbd_server` short sleeps and `net_task`'s indefinite block continue to
   work — these are the "primitive works for them" cases the post-mortem
   already validates.
4. The post-mortem at `docs/post-mortems/2026-04-24-ingress-task-starvation.md`
   gets a Resolution-update closing item 1.

---

## Repository orientation for this work

### Files most likely to be edited

| File | What |
|---|---|
| `userspace/sshd/src/session.rs` | sshd session loop + cleanup (issue 1) |
| `userspace/ion/src/...` | shell signal/EOF handling (issue 1, hypothesis 1+3) |
| `kernel/src/pty/...` or `userspace/pty/` | line discipline / VEOF (issue 1, hypothesis 2) |
| `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_nanosleep`) | long-sleep branch (issue 2) |
| `kernel/src/task/scheduler.rs` | `block_current_unless_woken_until` + `scan_expired_wake_deadlines` (issue 2) |

### Recent context this PR is built on

```
f877af7 build(hooks): retry regression-suite flakes in isolation before failing
dcb3401 fix(kernel,ipc,e1000): restore ring-3 NIC RX via net_task drain
9881e8f docs: post-mortem for ingress-task starvation of PID 1
d16a5a2 fix(kernel,e1000): stop spawning blocked ingress task to unblock PID 1
b224d9f fix(init,e1000): cache disabled state, soften reap-loop sleep, retry post-restart sends
81db7b3 fix(init): unblock login under regression-harness --snapshot disk
f8338e9 fix(sshd): detect shell exit from all three async tasks    ← related to issue 1
6aa27da fix(e1000): batch RX publish to bound driver block time
bd61176 fix(e1000): defer remote NIC rx dispatch
d88d49b fix(e1000): restore ring-3 SSH traffic
```

### Build / test cadence

```sh
cargo xtask check          # clippy + fmt + host tests, ~30 s
cargo xtask test           # kernel tests in QEMU, ~3 min
cargo xtask regression     # full QEMU-driven smoke, ~5 min (flake-resilient via the new hook)
cargo xtask regression --test <name>     # isolated single test
cargo xtask run            # boot the OS interactively (default virtio-net)
cargo xtask run --device e1000           # boot with ring-3 e1000
```

### Don't do

- Don't `git push --no-verify` to bypass the regression gate. The new
  pre-push hook (`f877af7`) handles documented harness flakiness via
  isolated retry; if a test fails *both* in suite and isolation, that's a
  real failure.
- Don't disable the ingress endpoint or revert `dcb3401` to "fix"
  serverization-fallback flakiness. The flakiness is the still-open
  nanosleep issue, independent of the ingress restoration.
- Don't add `cargo xtask regression --test` calls to other places without
  thinking — they're slow.
