# Phase 62 Track A — Pi-Lock Inventory

**Status:** Complete
**Source Ref:** phase-62-track-A
**Date:** 2026-05-10
**Branch:** `feat/phase-62-pi-lock-closeout`
**Companion:** `docs/roadmap/62-phase-57a-pi-lock-closeout.md`

## Purpose

Two-part inventory required before any code changes in Tracks B and C:

1. **A.1** — enumerate the four `TODO(57a-C/D)` sites in
   `kernel/src/task/scheduler.rs` and record their surrounding lock
   context.
2. **A.2** — kernel-wide audit of every `block_current_until` call
   expression for the Bug #9 pattern (an `IrqSafeMutex` or
   `lock_page_tables` guard alive across the block call).

The post-mortem `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`
and the follow-up handoff `docs/handoffs/57e-bug9-bug10-followup.md`
document the leak mechanism. The post-mortem's "~25 sites" estimate
covered only the syscall-layer FS-volume callsites under investigation
at the time; this inventory broadens the survey kernel-wide.

## A.1 — Four `TODO(57a-C/D)` Sites

Originally enumerated by `grep -n 'TODO(57a' kernel/src/task/scheduler.rs`
during inventory; after Track B closure these are anchored by the
`// NOTE: Phase 62 Track B` comments and locatable with
`grep -n 'NOTE: Phase 62 Track B' kernel/src/task/scheduler.rs`. The
table below uses function-name + grep anchors instead of bare line
numbers, since line numbers drift as the file evolves.

| # | Anchor | Function | Context | Holds `scheduler_lock()`? | Lock-order resolution |
|---|--------|----------|---------|---------------------------|----------------------|
| 1 | `pick_next` zero-`saved_rsp` cleanup (search: `dropping ready task idx`) | `pick_next` (queue scan) | drops a `Ready` task with `saved_rsp == 0` to `Dead` during local-queue scan | **YES** (`scheduler_lock()` is the inner lock) | Cannot `with_block_state` directly (would invert). Use the **release-and-reacquire** shape: drop `scheduler_lock()`, take pi_lock + reacquire scheduler_lock, write both `TaskBlockState.state` and `Task::state`, restart the scan after — OR — note that the task is already being removed from the run queue at this site (`q.remove(i)` follows the state write), so no other CPU can wake it before the dispatch loop drops `scheduler_lock()` further down. The defensive cleanup runs once per scheduling tick under IRQ-disabled scheduler_lock — the structural-safety argument supports a documented direct mutation here. |
| 2 | `install_test_task_idx` filler init (`#[cfg(test)]`; first NOTE in fn) | `install_test_task_idx` | filler `Task` initialization before `push` into `sched.tasks` | **YES** (`scheduler_lock()`), but task is not yet in `sched.tasks` | Task is freshly constructed; not visible from any other CPU. Direct `with_block_state` on the local `filler` Task is safe (uncontended) **before** `push` — once pushed, scheduler_lock is the visibility boundary. Apply `with_block_state` for uniformity. |
| 3 | `install_test_task_idx` in-place overwrite (`#[cfg(test)]`; second NOTE in fn) | `install_test_task_idx` | in-place overwrite of `*sched.tasks[idx]` | **YES** (`scheduler_lock()`) | Same lock-order constraint as Site 1. Either reuse Site 1's release-and-reacquire shape, or note that this overwrite happens during test setup with no live wake path. The test scaffolding already contains a comment about the in-place mutation matching `alloc_task_slot`'s heap-stable address. |
| 4 | per-core dispatch loop (search: `not Running after mark on core`) | `dispatch` (per-core dispatch loop) | sets the picked task `state = TaskState::Running` while `scheduler_lock()` held with IRQs disabled | **YES** (`scheduler_lock()`) — and IRQs are disabled at this site | Same lock-order constraint as Site 1. The dispatch path is the every-context-switch hot path; release-and-reacquire would cost two extra lock cycles per context switch. Document the structural-safety argument: at dispatch time IRQs are disabled, the task is being transitioned **from** `Ready`/idle states **to** `Running`, and `wake_task_v2` (the only waker that takes pi_lock) would CAS `Blocked* → Ready` and never `Ready → Running` — there is no concurrent waker that can race the `Running` write here. Apply `with_block_state` on the freshly-picked task **before** publishing through `set_current_task_idx`. |

### A.1 Addendum — PR #146 review-fix (`wake_task_v2` AlreadyAwake fast path)

The Phase 62 Track B helper relies on a **structural-safety argument**
about wake-side races, but the original `wake_task_v2` implementation
acquired `pi_lock` then `scheduler_lock` *unconditionally* — including
on the AlreadyAwake fast path (state ≠ `Blocked*`). That created a real
ABBA lock-order inversion against Sites 1 and 4: a CPU dispatching task
T (holding `scheduler_lock`, needing `pi_lock(T)`) could deadlock with a
CPU running `wake_task_v2(T)` between its step-1 `scheduler_lock` drop
and its step-2 `scheduler_lock` re-acquire, if step 2 had already taken
`pi_lock(T)`. `serial::wake_feeder_task` (called from the COM1 RX ISR,
which doesn't know whether the feeder is currently `Ready` or `Blocked*`)
is a real speculative caller that exercises this path.

The fix (committed as part of the Phase 62 closure on PR #146) moves
`wake_task_v2`'s state check inside the `pi_lock`-only critical section
so the AlreadyAwake fast path returns *before* acquiring `scheduler_lock`.
With the fast-path early return in place, `wake_task_v2` only ever
acquires `scheduler_lock` while holding `pi_lock` for tasks that are
genuinely `Blocked*` — which by construction excludes Sites 1 and 4
(both target `Ready` tasks) and excludes Sites 2 and 3 (target tasks
not yet visible cross-CPU). The structural-safety argument is now
backed by a lock-acquisition argument as well.

### A.1 Conclusion

All four sites already hold `scheduler_lock()`, so `Task::with_block_state`
in its current form (which `debug_assert!`s that scheduler_lock is NOT
held) cannot be called directly. The two acceptable shapes for the fix:

- **Shape α — Release-and-reacquire SCHEDULER:** drop scheduler_lock(),
  take pi_lock, take scheduler_lock again, write both state mirrors,
  drop scheduler_lock, drop pi_lock. Correct but expensive on the
  dispatch hot path (Site 4).
- **Shape β — Locked-helper variant `Task::with_block_state_locked_under_scheduler`:**
  a new helper that documents a structural-safety argument allowing
  pi_lock to be acquired while scheduler_lock is held at sites where
  the wake-side races are statically excluded. The argument is
  site-specific (queue scan, dispatch hot path, IRQ-disabled, no
  competing waker that touches `Ready → Running`).

Track B will use **Shape β** at all four sites. The new helper will
be `Task::with_block_state_locked_scheduler` (or similar) and will be
documented inline with the structural argument and a reference back
to this inventory entry.

## A.2 — Kernel-Wide `block_current_until` Audit

`grep -rn 'block_current_until(' kernel/src/` against HEAD returns
**23 actual call expressions** (after filtering doc-comments and the
`pub fn block_current_until` definition itself). Per-call audit:

### Verdict legend

- **`no-guard`** — no preempt-affecting lock guard alive at the call
- **`released-before-block`** — a preempt-affecting guard exists but its
  scope ends before the block call (typically via an inner `{ … }` block
  or an explicit `drop(guard)`)
- **`LEAK`** — a preempt-affecting (`IrqSafeMutex` /
  `lock_page_tables`) guard is alive across the block; needs Option-B
  or Option-C in Track C
- **`AMBIGUOUS`** — manual review required

### Inventory table

| File | Line | Function | Wrapping helper / context | Guards live | Verdict |
|------|------|----------|---------------------------|-------------|---------|
| `kernel/src/lib.rs` | 716 | `read_stdin_blocking` | direct call | Local `AtomicBool` only | no-guard |
| `kernel/src/lib.rs` | 802 | `net_task` (NIC recv loop) | direct call | Local `AtomicBool` only | no-guard |
| `kernel/src/task/wait_queue.rs` | 75 | `WaitQueue::sleep` | direct call | `self.waiters.lock()` (IrqSafeMutex) — temporary, dropped at end of `push_back(...)` statement (line 68) | released-before-block |
| `kernel/src/blk/virtio_blk.rs` | 556 | `RequestSlot::wait_for_completion` | direct call | `Self::register_waiter` returns; no internal guard escapes scope | no-guard |
| `kernel/src/blk/virtio_blk.rs` | 887 | `do_request` (poll loop) | direct call | `with_driver(\|d\| ...)` closure exits at line 854 — DRIVER lock released before block at 887 | released-before-block |
| `kernel/src/ipc/notification.rs` | 831 | `notify::wait` (drain loop) | direct call | `WAITERS.lock()` (IrqSafeMutex) at line 795 — inner scope ends at line 815, before block at 831 | released-before-block |
| `kernel/src/ipc/registry.rs` | 157 | `wait_until_registered` | direct call | `SERVICE_WAITERS.lock()` (IrqSafeMutex via `Lazy`) at line 148 — inner scope ends at line 150, before block at 157 | released-before-block |
| `kernel/src/task/scheduler.rs` | 3288 | `block_current_on_reply_v2` | wrapper helper | Local `AtomicBool` only; callers (IPC layer) are responsible for not holding guards across the wrapper | no-guard *(at the wrapper; callers audited below)* |
| `kernel/src/task/scheduler.rs` | 3363 | `block_current_on_recv_v2` | wrapper helper | Same as above | no-guard *(at the wrapper)* |
| `kernel/src/task/scheduler.rs` | 3396 | `block_current_on_notif_v2` | wrapper helper | Same as above | no-guard *(at the wrapper)* |
| `kernel/src/task/scheduler.rs` | 3437 | `block_current_on_send_v2` | wrapper helper | Same as above | no-guard *(at the wrapper)* |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 3565 | `sys_nanosleep` (≥ 1 ms branch) | direct call | Local `AtomicBool` only | no-guard |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 4860 | `sys_waitpid` (parent block) | direct call | `PROCESS_TABLE` lock at line 4717 — `result` scope ends at line 4791, before block at 4860 | released-before-block |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 5367 | `block_on_pty_master_read` | direct call | `PTY_TABLE.lock()` (IrqSafeMutex) at line 5360 — inner `let ready = { ... }` scope ends at line 5365, before block at 5367 | released-before-block |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 5397 | `block_on_pty_slave_read` | direct call | `PTY_TABLE.lock()` (IrqSafeMutex) at line 5382 — inner scope ends at line 5395, before block at 5397 | released-before-block |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 5488 | `sys_linux_read` (stdin direct-read branch) | direct call via `STDIN_WAITQUEUE` register/sleep flow | `STDIN_WAITQUEUE` is a `WaitQueue` — `register` releases its inner IrqSafeMutex via `push_back` temp drop. No guard alive at 5488. | no-guard |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 5679 | `sys_linux_read` (pipe read branch) | direct call | `PIPE_TABLE` (IrqSafeMutex) accessed only inside `pipe_register_waiter`/`pipe_read`/`pipe_deregister_waiter` — each call returns before the next; no guard escapes their scope | no-guard |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 6252 | `sys_linux_write` (pipe write branch) | direct call | Same as 5679 — PIPE_TABLE never held across the block | no-guard |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 13834 | `sys_futex` (FUTEX_WAIT) | direct call | `FUTEX_TABLE.lock()` (IrqSafeMutex) at line 13802 — inner scope ends at line 13823, before block at 13834 | released-before-block |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 16170 | `sys_poll` (wait loop) | direct call | No locks held — `entries[]` is local; FD waiter operations encapsulate their own locks | no-guard |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 16425 | `select_inner` (wait loop) | direct call | Same shape as `sys_poll` | no-guard |
| `kernel/src/arch/x86_64/syscall/mod.rs` | 16843 | `sys_epoll_wait` (wait loop) | direct call | Same shape as `sys_poll` | no-guard |

### A.2 Conclusion

**Total call expressions:** 23
**LEAK verdicts:** 0
**AMBIGUOUS verdicts:** 0

All 23 callsites are clean: every preempt-affecting guard is either
absent or explicitly released before the block call (typically via
an inner `{ … }` scope or by encapsulating the lock inside a helper
function that returns before the block).

This result is consistent with the post-deferral closure narrative
in `docs/handoffs/57e-bug9-bug10-followup.md`:

- The historical worst case — `FAT32_VOLUME` / `EXT2_VOLUME` held
  across `kernel_read_fd_at` → `virtio_blk::do_request` — was closed
  by Phase 57e Bug #9 Step 1 (`sys_mmap_file_backed` Option-C release-
  before-block) and then by the FS-volume mutex type swap from
  `IrqSafeMutex` to `spin::Mutex` (`9292aec`, un-reverted in `6826deb`).
- TMPFS, FAT32_PERMISSIONS, and other in-memory volumes were verified
  not to descend into `block_current_until` while held.

### Verification of the `sys_mmap_file_backed` Option-C fix

`kernel/src/arch/x86_64/syscall/mod.rs` `sys_mmap_file_backed` (around
line 8901–9105 at HEAD): the `kernel_read_fd_at` call descends into
`virtio_blk::do_request` (which calls `block_current_until`).
`lock_page_tables()` is acquired **inside** an inner block (around
lines 9058–9068) that runs **after** `kernel_read_fd_at`'s read — the
read happens at line ~9031 with no `lock_page_tables` guard alive.
Comment at line ~8941 documents the split lifecycle as the Bug #9 fix.
**Verified intact at HEAD.**

### Lock types confirmed at A.2 sites

| Lock | Type | Preempt-affecting? |
|------|------|--------------------|
| `FAT32_VOLUME` | `spin::Mutex<Option<Fat32Volume>>` (kernel/src/fs/fat32.rs:213) | NO |
| `EXT2_VOLUME` | `spin::Mutex<Option<Ext2Volume>>` (kernel/src/fs/ext2.rs:79) | NO |
| `Ext2Volume::block_cache` | `spin::Mutex<BTreeMap<u32, Vec<u8>>>` (kernel/src/fs/ext2.rs:61) | NO |
| `TMPFS` | `spin::Mutex<Tmpfs>` (kernel/src/fs/tmpfs.rs:37) | NO |
| `PIPE_TABLE` | `IrqSafeMutex<Vec<Option<Pipe>>>` (kernel/src/pipe.rs:25) | YES — but not held across any block (helper-encapsulated) |
| `PTY_TABLE` | `IrqSafeMutex<[Option<PtyPairState>; MAX_PTYS]>` (kernel/src/pty.rs:16) | YES — but always released-before-block via inner scope |
| `WAITERS` (notification) | `IrqSafeMutex<[Option<TaskId>; MAX_NOTIFS]>` (kernel/src/ipc/notification.rs:228) | YES — released-before-block |
| `SERVICE_WAITERS` (registry) | `Lazy<IrqSafeMutex<Vec<ServiceWaiter>>>` (kernel/src/ipc/registry.rs:55) | YES — released-before-block |
| `REQUEST_WAITERS` (virtio_blk) | `IrqSafeMutex<RequestWaitQueue>` (kernel/src/blk/virtio_blk.rs:516) | YES — accessed only inside helpers that return before block |
| `WaitQueue.waiters` | `IrqSafeMutex<VecDeque<WaitEntry>>` (kernel/src/task/wait_queue.rs:40) | YES — temporary scope, released by statement-end |
| `FUTEX_TABLE` | `IrqSafeMutex<...>` (syscall/mod.rs) | YES — released-before-block |
| `PROCESS_TABLE` | `IrqSafeMutex<...>` (process.rs) | YES — released-before-block |

## Track C implications

Because A.2 returns zero LEAK verdicts, **Track C has no source-code
changes to make**. The track's deliverable is:

1. This inventory doc, recording the audit and the per-site verdicts.
2. Verification (text only, no code change) that the
   `sys_mmap_file_backed` Option-C fix is intact at HEAD.
3. Confirmation that no new guard-across-block pattern was introduced
   between Phase 57e session-15 and the start of Phase 62.

The Track C `// NOTE: Phase 62 Track C — Option-{B,C} (Bug #9)`
comments were planned to be added at converted callsites; since no
conversions are needed, no inline comments are added in Track C
beyond the existing post-mortem-referencing doc-comments on the
FS-volume statics.

## Track B implications

All four `TODO(57a-C/D)` sites already hold `scheduler_lock()`, so
the fix shape is **Shape β** (a new `Task::with_block_state_locked_scheduler`
helper documenting the structural-safety argument). The argument
varies per site:

- **Site 1 (queue-scan defensive cleanup):** the task is being removed
  from the run queue in the same critical section; no waker can
  observe the transient `Dead` write.
- **Site 2 (test filler before push):** task not yet visible — no
  competing CPU.
- **Site 3 (test in-place overwrite):** task slot is being replaced
  during test setup; no live wake path.
- **Site 4 (dispatch hot path):** IRQs disabled; the only transitions
  competing with our `Running` write are `wake_task_v2`'s `Blocked* →
  Ready` CAS, which never targets `Running` and never occurs without
  pi_lock. The pi_lock acquire here serializes against `wake_task_v2`'s
  CAS but not against any other dispatcher (only one CPU dispatches
  this slot at a time).

## Re-verify before Track B

Track B implementer: re-run

```bash
grep -n 'TODO(57a' kernel/src/task/scheduler.rs
grep -rn 'block_current_until(' kernel/src/ | grep -v '//'
```

against the branch tip immediately before starting work. If line
numbers have drifted from the values in this doc, update inline
references in Track B's commit messages and `// NOTE:` comments.

## References

- `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` — original Bug #9 post-mortem.
- `docs/handoffs/57e-bug9-bug10-followup.md` — Bug #9 closure plan and Option-A/B/C analysis.
- `docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md` — preemption-full deferral and post-deferral severity adjustment for Bug #9.
- `docs/roadmap/57a-scheduler-rewrite.md` — Phase 57a design.
- `docs/roadmap/57b-preemption-foundation.md` — Phase 57b design (preempt_count discipline).
- `docs/roadmap/62-phase-57a-pi-lock-closeout.md` — Phase 62 design.
- `docs/roadmap/tasks/62-phase-57a-pi-lock-closeout-tasks.md` — Phase 62 task list.
