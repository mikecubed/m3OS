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

Ranked by likelihood after a second-pass audit (2026-04-17):

1. **`MOUNT_OP_LOCK` held across a blocking IPC — sleep-while-holding-spinlock
   on SMP.** This is the new leading candidate. In `kernel/src/arch/x86_64/
   syscall/mod.rs:478-502`, `open_user_path` acquires `MOUNT_OP_LOCK`
   (a `spin::Mutex<()>`) and then calls `resolve_existing_fs_path`, which
   calls `path_node_nofollow` (same file, line 202), which for ext2 paths
   issues `vfs_service_stat_path` → `endpoint::call_msg` →
   `block_current_on_reply_unless_message`. The caller yields its CPU
   **while still holding `MOUNT_OP_LOCK`**.

   On UP this is invisible. On SMP any other core that tries to acquire
   `MOUNT_OP_LOCK` (other openers, `mount`, `umount`, `link`, `unlink` —
   see `mod.rs:3522, 9123, 10516, 10604`) will busy-spin in
   `spin::Mutex::lock()`. A spinning core:

   - Cannot process its own run queue.
   - Cannot honor its `reschedule` flag.
   - Ignores the reschedule IPI (the handler returns to the same spinning
     instruction).

   If the scheduler lands the blocked-then-woken caller on that same core
   (initial `assigned_core` match, or a load-balance migration), the task
   is Ready + enqueued but **cannot run**. Matches the failure profile:
   random iteration count, serial log quiesces (the spinning core produces
   no events), and the fix explains why a targeted IPI did not resolve
   it — the IPI fired but the receiving core was spinning in ring 0.

   The `drop(_mount_guard)` at `mod.rs:527` only covers the service-routed
   `open` path; it does **not** cover the path through
   `resolve_existing_fs_path`, which is where the blocking IPC actually
   happens. Other users of `MOUNT_OP_LOCK` have the same issue through
   symlink resolution.

2. **Post-switch re-enqueue race** with concurrent wake. Caller is
   mid-`switch_context` when a wake arrives. Wake sees `switching_out=true`,
   sets `wake_after_switch=true`, returns `woke=true`. Post-switch code
   runs on the caller's core, clears `switching_out`, reads
   `wake_after_switch`, decides.

   Second-pass audit note: walked this symbol by symbol.
   `wake_task` (`scheduler.rs:1155-1204`) and the post-switch block
   (`scheduler.rs:1647-1713`) each perform their `switching_out` /
   `wake_after_switch` / state transitions under a single
   `SCHEDULER.lock()` acquisition, so they serialize cleanly — there is
   no in-window interleave where both observe stale values. Downgraded,
   but not fully ruled out: still worth targeted tracing if
   Hypothesis 1 is fixed and the hang persists.

3. **Load balancer interference.** The scheduler's load balancer
   (`kernel/src/task/scheduler.rs`) migrates tasks between cores
   periodically. If it fires on a task that's in the middle of a block /
   wake handshake, the task's `assigned_core` might change out from under
   `enqueue_to_core`.

   Second-pass audit note: the load balancer and work-stealing both
   consult `last_migrated_tick` (100-tick / ~1 s cooldown), and every
   `wake_task` path updates `last_migrated_tick` before returning, so a
   freshly woken task is migration-immune. Unlikely to be the bug on
   its own, but could combine with Hypothesis 1 (balancer moves the
   woken task onto the core that is spinning on `MOUNT_OP_LOCK`).

4. **Reply-cap revocation race.** See `[ipc] reply` handler (syscall 4) in
   `kernel/src/ipc/mod.rs:199-210`. The `caller_id` is extracted from the
   Reply cap BEFORE the cap is removed. If `revoke_reply_caps_for` fires
   in that window, the cap is gone but `endpoint::reply` is still called.
   The subsequent `deliver_message + wake_task` path may run even though
   the caller state has advanced. Known to corrupt `pending_msg` /
   `pending_bulk` — produces visible `u64::MAX` returns, not a hang.

5. **`transfer_bulk` / `take_bulk_data` ordering.** In
   `endpoint::reply`, `transfer_bulk` runs AFTER `deliver_message`. On
   SMP the caller could wake on another CPU, read `pending_msg`, and
   call `take_bulk_data` before `transfer_bulk` completes.

   Second-pass audit note: in `endpoint::reply` (`endpoint.rs:543-557`)
   the order is `transfer_bulk` → `deliver_message` → `wake_task`, not
   the reverse. The caller wakes only after both bulk and message are in
   place. Likely not a bug.

### Known regression adjacent to the hang (separate bug)

**`SIGCONT` unconditionally interrupts IPC waits.** `send_signal` at
`kernel/src/process/mod.rs:1261-1272` calls `interrupt_ipc_waits(pid)`
unconditionally inside the `SIGCONT` branch — even when the target
process is not `Stopped`, even when `SIGCONT` is blocked, even when its
disposition is `Ignore`. That path then runs `revoke_reply_caps_for` +
sentinel-`deliver_message` + `wake_task` on every in-flight VFS/UDP
call in the target's tasks. Any `kill(SIGCONT)` broadcast (shell job
control, e.g.) will silently cancel otherwise-successful Phase 54 VFS
requests. Move the `interrupt_ipc_waits(pid)` call inside the
`if proc.state == ProcessState::Stopped` arm. Not the hang, but worth
fixing while this code is open.

## Recommended next steps

In priority order (reordered 2026-04-17 to lead with the new leading
hypothesis):

1. **Release `MOUNT_OP_LOCK` before any path that can issue a blocking
   IPC.** This is the single action most likely to end the hang.
   Concretely, in `kernel/src/arch/x86_64/syscall/mod.rs`:

   - In `open_user_path` (line 478), the `_mount_guard` currently
     covers `resolve_existing_fs_path`, which can call into
     `vfs_service_stat_path`. Restructure so that path resolution runs
     outside the guard, or drop the guard at function entry and re-lock
     only around the actual mount-affecting step (the `open_resolved_path`
     mutating work). The existing `drop(_mount_guard)` at line 527 only
     covers the service-routed branch; add symmetric drops to every
     branch that can reach `path_node_nofollow` with ext2 paths.
   - Audit the other four `MOUNT_OP_LOCK` sites (`mod.rs:3522, 9123,
     10516, 10604`) for the same pattern — symlink resolution via
     `resolve_existing_fs_path` can trigger VFS IPC there too.
   - Longer-term, consider replacing `spin::Mutex<()>` with an IPC-safe
     primitive (e.g. a yielding mutex that checks `reschedule` during
     spin, or an RwLock that readers take non-exclusively for path
     resolution).

   Verification: after the fix, `ls` should run for hundreds of
   iterations without freezing. If it still hangs, move to Step 2.

2. **Build a watchdog** instead of boundary tracing. A low-frequency
   kernel task (1 Hz or so) walks the task table looking for any task
   that's been in `BlockedOnReply` for more than ~5 s with a pending
   message already delivered. When it finds one, dump:
   - The task's `state`, `switching_out`, `wake_after_switch`,
     `assigned_core`, `saved_rsp`, `pending_msg`, `pending_bulk`
   - The target core's run queue contents
   - The last 50 entries of the kernel trace ring
   - Each core's current `reschedule` flag value and whether the core
     has executed a dispatch iteration recently
   - **For the leading hypothesis:** which core (if any) is currently
     blocked inside `spin::Mutex::lock()` and for which address. Easy
     to add as a per-core "spinning_on: Option<usize>" breadcrumb set
     by the Mutex wrapper.

   This catches the hang **at the moment it happens** and gives us the
   full scheduler state, not just the IPC boundary. Stick this behind
   a `M3OS_WATCHDOG=1` env gate so it doesn't run in CI.

3. **Re-land `try_deliver_message`** in `interrupt_ipc_waits`. The
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

4. **Fix the `sys_ipc_reply` race.** Change the sequence in
   `kernel/src/ipc/mod.rs` for syscall number 4 (and 5, 16 which share
   the pattern) so `remove_task_cap` runs FIRST (atomic check-and-remove)
   and returns the `Capability::Reply(caller_id)` it removed. Only then
   call `endpoint::reply`. If the remove fails, the cap was revoked —
   don't deliver the reply.

5. **Fix the `SIGCONT` IPC-wait regression.** Move
   `interrupt_ipc_waits(pid)` inside the `if proc.state ==
   ProcessState::Stopped` arm at `kernel/src/process/mod.rs:1261-1272`.
   Leaves the `pending_signals` clear path intact but stops cancelling
   in-flight IPC on `SIGCONT` to non-stopped processes.

6. **Audit the post-switch re-enqueue** (only if 1–5 do not resolve the
   hang) in `kernel/src/task/scheduler.rs:1647-1713`. Walk through the
   `switching_out → wake_after_switch → post-switch re-enqueue`
   sequence with an SMP lens. Particularly: if a wake comes in AFTER
   post-switch clears `switching_out` but BEFORE the enqueue decision,
   does the task end up on a run queue? (Second-pass audit says yes,
   but a targeted trace would confirm.)

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

- `kernel/src/arch/x86_64/syscall/mod.rs` — **leading hypothesis:**
  `MOUNT_OP_LOCK` (line 94), its acquire sites
  (lines 479, 3522, 9123, 10516, 10604), the early `drop(_mount_guard)`
  at line 527 (routed path only), `path_node_nofollow` (line 202) that
  issues blocking IPC, and the `vfs_service_open/stat/read/close/list_dir`
  entry points that exercise the IPC path at high volume.
- `kernel/src/task/scheduler.rs` — IPI fix (landed), wake/enqueue paths,
  post-switch re-enqueue (line 1647-1713), `revoke_reply_caps_for`
  (line 1222-1228), `blocked_ipc_task_ids_for_pid` (line 1351-1365).
- `kernel/src/process/mod.rs` — `send_signal` (line 1251), `SIGCONT`
  regression (line 1261-1272), `signal_interrupts_ipc_wait`,
  `interrupt_ipc_waits`, `vfs_handle_open_count`.
- `kernel/src/ipc/endpoint.rs` — `call_msg`, `reply`, `cancel_task_wait`.
- `kernel/src/ipc/mod.rs` — `sys_ipc_reply` syscall 4 dispatch
  (line 199-210) and parallel patterns at syscall 5 (line 212-234) and
  syscall 16 (line 287-313).
- `userspace/vfs_server/src/main.rs` — server loop that receives and
  replies to VFS requests. Note: uses separate `ipc_reply` +
  `ipc_recv_msg` calls (line 503-506), not atomic `reply_recv_msg`.
- `kernel/src/blk/virtio_blk.rs` — `read_sectors` (line 413) spin-polls
  the virtqueue while holding `DRIVER` — another kernel spin-while-in-
  kernel site, though not on the hang path.

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

Second-pass audit on 2026-04-17 (same day) re-read the scheduler, IPC,
IPI, and signal paths end-to-end against the post-IPI failure profile.
Outcome: downgraded the post-switch race (Hypothesis 2) and elevated
**`MOUNT_OP_LOCK` held across blocking IPC** (new Hypothesis 1) as the
leading candidate. Also documented three separate Phase 54 correctness
bugs adjacent to the hang: the signal sentinel clobber, the
`sys_ipc_reply` cap-removal race, and the `SIGCONT` IPC-wait regression.
None of the three explain the hang on their own, but all three are
small, safe fixes worth landing while the code is open.
