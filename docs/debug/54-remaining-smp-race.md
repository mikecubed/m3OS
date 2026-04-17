# Phase 54: remaining SMP scheduling race — investigation handoff

**Status:** Open. Narrow intermittent hang after the IPI fix on
`fix/54-smp-reschedule-ipi` landed.

**Branch:** `debug/54-remaining-smp-race` (based on `fix/54-smp-reschedule-ipi`)

## The problem

After Phase 54 broadened VFS routing (commit 3944b9b), the kernel forwards
`path_node_nofollow`, `path_metadata`, `sys_linux_fstatat`, and
`sys_linux_getdents64` through `vfs_server` via IPC. Cross-core IPC volume
increased ~10-100× for common syscalls.

Initially this caused:

1. **Deterministic hang during login** on `STAT /etc/services.d` (init's
   service-manager scan).
2. **Intermittent hangs** during subsequent interactive commands (often
   after 7-10 successful `ls` invocations).

### Known cause (fixed on `fix/54-smp-reschedule-ipi`)

The scheduler's `enqueue_to_core` set the target core's reschedule flag
but did not send an IPI. `hlt` on an idle core only wakes on an interrupt,
so tasks enqueued onto halted remote cores waited up to a timer tick
(~10 ms on APs) — or stranded entirely if the flag check raced with a new
halt cycle.

**Fix:** `enqueue_to_core` now sends `IPI_RESCHEDULE` (vector 0xFE) when
the target core differs from the caller's. Confirmed working by
trace-level reproduction.

### Remaining symptom

Even with the IPI fix in place, interactive use still hangs occasionally:

- Login completes reliably.
- Shell prompt appears.
- `ls` runs successfully multiple times.
- After N successful commands (≈7–15), the terminal freezes: no output,
  no keyboard response. Serial log stops advancing.

The hang is **random** — same commands, same paths, different iteration
count on different boots. This rules out deterministic data-dependent
bugs. It points at a narrow SMP timing race.

## What we know from the traces

The following instrumentation was added then removed (dead code cleaned up
on `fix/54-smp-reschedule-ipi`; reintroduce from this branch's history
only if needed):

- `log::info!` at every `vfs_service_*` entry and exit (kernel side)
- `syscall_lib::write_str` at every `vfs_server` request dispatch
- `log::warn!` in `endpoint::reply` when `wake_task` returned false
- `log::warn!` in the post-switch re-enqueue path when
  `wake_after_switch && !blocked` (the "LOST-WAKE" case)
- `log::warn!` in `enqueue_to_core` when the target core lookup dropped
  the enqueue

### Observed patterns at hang time

1. Kernel logs `[vfs-kern] STAT task=N path=…` (call start)
2. `vfs_server` logs `[vfs-srv] got STAT d0=…` (receive)
3. `vfs_server` logs `[vfs-srv] reply STAT label=0x0 d0=…` (reply)
4. `endpoint::reply` delivers + `wake_task(caller) → true`
5. **No** `[vfs-kern] STAT … reply.label=…` — kernel never returns from
   `endpoint::call_msg`.

No `wake=NO` / `LOST-WAKE` / `DROPPED` warnings ever fired. From the
scheduler's own accounting the task was successfully transitioned
Blocked→Ready and enqueued. But it never actually resumed.

### Another observation — signal sentinel clobbering replies

One trace showed `[vfs-srv] reply STAT label=0x0` (success) but the kernel
logged `reply.label=0xffffffffffffffff` (`u64::MAX`). That's `send_signal`
→ `interrupt_ipc_waits` → `deliver_message(task, u64::MAX)` overwriting a
legitimate reply with the EINTR sentinel because the signal happened to
race with the real reply.

This was intermittently clobbering valid replies. It's a correctness
bug, but *not* the cause of the hang (it produces a visible error, not a
deadlock).

A `try_deliver_message` helper (only writes when `pending_msg` is `None`)
was prototyped but not merged — it belongs on this investigation branch.
See **Recommended next steps** below.

## Things that worked

- **IPI on cross-core enqueue** (`fix/54-smp-reschedule-ipi`). Fixed login
  hang and most interactive hangs. Root cause of the initial deterministic
  failure mode.

## Things that did NOT help

- `revoke_reply_caps_for` in `interrupt_ipc_waits` (9b6017c). Intended to
  drop stale reply caps when a caller is pulled out of IPC by a signal.
  Has a race (server can extract `caller_id` before revocation takes
  effect, then still calls `endpoint::reply`) so does not fully solve the
  stale-reply concern. Did not change hang behavior one way or the other.
  Still in place on `fix/54-smp-reschedule-ipi` — consider whether to keep.

- `signal_interrupts_ipc_wait` narrowing (22aa577). Restricts IPC-wait
  cancellation to signals that would actually do something. Correct
  behavior change but not related to the hang. Still in place.

- VFS handle refcounting via `vfs_handle_open_count` +
  `cleanup_vfs_handle_if_unused` (20af452). Correct fix for dup/fork
  lifecycle. Unrelated to this hang. Still in place.

- High-frequency tracing (`log::info!` on every wake/enqueue/post-switch).
  Generated too much output to use interactively; also drowned the
  race in noise. Removed.

## Hypotheses still on the table

Ranked by likelihood:

1. **Post-switch re-enqueue race** with concurrent wake. Caller is
   mid-`switch_context` when a wake arrives. Wake sees `switching_out=true`,
   sets `wake_after_switch=true`, returns `woke=true`. Post-switch code
   runs on the caller's core, clears `switching_out`, reads
   `wake_after_switch`, decides. If `wake_task` on another core observes
   `switching_out=false` simultaneously (different CPU cache views?) and
   takes the "normal enqueue" path while post-switch also enqueues,
   the task might get double-enqueued or the state transitions might
   interleave in a way that leaves the task not actually on a run queue.
   Needs a targeted trace of this specific sequence.

2. **Load balancer interference.** The scheduler's load balancer
   (`kernel/src/task/scheduler.rs`) migrates tasks between cores
   periodically. If it fires on a task that's in the middle of a block /
   wake handshake, the task's `assigned_core` might change out from under
   `enqueue_to_core`. Worth reading the load balancer code with this in
   mind.

3. **Reply-cap revocation race.** See `[ipc] reply` handler (syscall 4) in
   `kernel/src/ipc/mod.rs`. The `caller_id` is extracted from the Reply
   cap BEFORE the cap is removed. If `revoke_reply_caps_for` fires in
   that window, the cap is gone but `endpoint::reply` is still called.
   The subsequent `deliver_message + wake_task` path may run even though
   the caller state has advanced. Not known to cause a hang but could
   corrupt pending-slot invariants.

4. **`transfer_bulk` / `take_bulk_data` ordering.** In
   `endpoint::reply`, `transfer_bulk` runs AFTER `deliver_message`. On
   SMP the caller could wake on another CPU, read `pending_msg`, and
   call `take_bulk_data` before `transfer_bulk` completes. Check
   whether this is actually possible given `wake_task` ordering.

## Recommended next steps

In priority order:

1. **Build a watchdog** instead of boundary tracing. A low-frequency
   kernel task (1 Hz or so) walks the task table looking for any task
   that's been in `BlockedOnReply` for more than ~5 s with a pending
   message already delivered. When it finds one, dump:
   - The task's `state`, `switching_out`, `wake_after_switch`,
     `assigned_core`, `saved_rsp`, `pending_msg`, `pending_bulk`
   - The target core's run queue contents
   - The last 50 entries of the kernel trace ring
   - Each core's current `reschedule` flag value and whether the core
     has executed a dispatch iteration recently

   This catches the hang **at the moment it happens** and gives us the
   full scheduler state, not just the IPC boundary. Stick this behind
   a `M3OS_WATCHDOG=1` env gate so it doesn't run in CI.

2. **Re-land `try_deliver_message`** in `interrupt_ipc_waits`. The
   signal-clobbers-real-reply bug was visible in the trace
   (`reply.label=0xffff…` despite server replying `label=0x0`). The fix
   is small and safe:

   ```rust
   // in kernel/src/task/scheduler.rs
   pub fn try_deliver_message(id: TaskId, msg: Message) -> bool {
       let mut sched = SCHEDULER.lock();
       if let Some(idx) = sched.find(id)
           && sched.tasks[idx].pending_msg.is_none()
       {
           sched.tasks[idx].pending_msg = Some(msg);
           return true;
       }
       false
   }
   ```

   Then `interrupt_ipc_waits` uses it instead of `deliver_message`.
   This is correctness-only; if it doesn't help the hang, leave it
   in for the signal path.

3. **Audit the post-switch re-enqueue** in
   `kernel/src/task/scheduler.rs:1626-1696`. Walk through the
   `switching_out → wake_after_switch → post-switch re-enqueue`
   sequence with an SMP lens. Particularly: if a wake comes in AFTER
   post-switch clears `switching_out` but BEFORE the enqueue decision,
   does the task end up on a run queue?

4. **Fix the `sys_ipc_reply` race.** Change the sequence in
   `kernel/src/ipc/mod.rs` for syscall number 4 so `remove_task_cap`
   runs FIRST (atomic check-and-remove), and only then extract the
   `caller_id` and call `endpoint::reply`. If the remove fails, the
   cap was revoked — don't deliver the reply.

## Repro steps

```bash
git switch debug/54-remaining-smp-race
cargo xtask clean
cargo xtask run
```

Log in as `root` with the default password, then run `ls` repeatedly.
The terminal will freeze after a variable number of iterations (often
7-15). There is no output once the hang starts.

## Related files

- `kernel/src/task/scheduler.rs` — IPI fix (landed), wake/enqueue paths,
  post-switch re-enqueue, `revoke_reply_caps_for`.
- `kernel/src/process/mod.rs` — `send_signal`,
  `signal_interrupts_ipc_wait`, `interrupt_ipc_waits`,
  `vfs_handle_open_count`.
- `kernel/src/ipc/endpoint.rs` — `call_msg`, `reply`, `cancel_task_wait`.
- `kernel/src/ipc/mod.rs` — `sys_ipc_reply` syscall 4 dispatch.
- `kernel/src/arch/x86_64/syscall/mod.rs` — `vfs_service_open/stat/read/
  close/list_dir` entry points that exercise the IPC path at high
  volume.
- `userspace/vfs_server/src/main.rs` — server loop that receives and
  replies to VFS requests.

## Reverted ideas

- **Reverting 3944b9b** or narrowing the VFS routing back to `/etc/*`
  only. Rejected by the user — Phase 54's goal is userspace migration,
  not narrowing it. Keep broad routing.

- **Widespread `[vfs-kern]` / `[vfs-srv]` INFO tracing.** Too noisy for
  interactive use. Leave as a diagnostic tool to enable case-by-case
  when needed; do not land in main.

## Hand-off author

Debugging session on 2026-04-17. Bisect of Phase 54 closure identified
commit `3944b9b` as the first bad commit. IPI fix landed on
`fix/54-smp-reschedule-ipi` as commit `e752418`.
