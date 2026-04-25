# Scheduler design comparison: m3OS vs. Linux's `do_nanosleep`

**Status:** Research note. Synthesises a deep dive on Linux's
`hrtimer_nanosleep` machinery, an audit of m3OS's
`block_current_unless_woken_until` machinery, and a Redox-side reference
on the PTY/SIGHUP/setsid contract that ion expects. Written to answer:
*Is there a fundamental scheduler issue we need to resolve?*

**Short answer:** Yes — m3OS's blocking primitive splits one conceptual
state transition into three observable flags (`state`, `switching_out`,
`wake_after_switch`) plus a side counter (`ACTIVE_WAKE_DEADLINES`) and a
deferred-wake hand-off (`PENDING_SWITCH_OUT`). Each pair of flags has
its own invariant and the invariants only hold under the assumption
that the dispatch switch-out handler runs exactly once per block call.
Linux solves the same problem with a single state word and ordering;
adopting that pattern would eliminate the entire bug class. There is
**also** a separate, unrelated correctness gap in the PTY/TIOCSCTTY
contract that a Redox-derived audit identified — that one is a single-
line fix and is included in the bundled diff.

---

## The two bugs in one frame

Both Issue 1 (SSH disconnect hang where pid 14 silently stops executing
mid-syscall-loop) and Issue 2 (second `block_current_unless_woken_until`
call silently fails to wake) share a signature:

> A task ends up in some Blocked-or-equivalent state that the
> scheduler never wakes, with no fault, no signal, no panic, no
> `cpu-hog`, no `stale-ready` warning.

The Linux mental model says: this signature *only* happens when the
wake-side store and the block-side state-write race in a window the
hardware doesn't synchronize, *or* when the scheduler tracks "blockedness"
in multiple places and a partial update is observed. Both apply to
m3OS.

---

## Linux's design (concrete)

`do_nanosleep` (`kernel/time/hrtimer.c`) is a tight loop:

```c
do {
    set_current_state(TASK_INTERRUPTIBLE | TASK_FREEZABLE);
    hrtimer_sleeper_start_expires(t, mode);
    if (likely(t->task))
        schedule();
    hrtimer_cancel(&t->timer);
    mode = HRTIMER_MODE_ABS;
} while (t->task && !signal_pending(current));
__set_current_state(TASK_RUNNING);
```

Three properties make this race-free:

1. **`set_current_state` writes `current->__state` with a full memory
   barrier (`smp_store_mb`)**. Pairs with the barrier in
   `try_to_wake_up` before that side reads `p->__state`.
2. **The condition (`t->task`) is read AFTER the state write, BEFORE
   `schedule()`**. If the timer already fired, the callback set
   `t->task = NULL`; the running task observes `NULL`, skips
   `schedule()` entirely. If the timer fires *after* `schedule()` is
   entered, `try_to_wake_up` flips `__state` from `TASK_INTERRUPTIBLE`
   to `TASK_RUNNING` and re-queues. The race window is closed by
   ordering, not retry.
3. **The wake side (`try_to_wake_up`) is a state-match CAS**:
   `if (@state & p->__state) p->__state = TASK_RUNNING`. A wake to a
   task already in `TASK_RUNNING` is silently dropped — there is no
   side-state ("switching_out", "wake_after_switch", reserve flag) the
   waker has to coordinate with. Spurious wakeups become dead-letters
   automatically.

The "sleep again immediately after wake" pattern works because
`hrtimer_sleeper` is **stack-allocated and freshly constructed on every
syscall** (`hrtimer_setup_sleeper_on_stack`). The previous sleeper's
state is on a destroyed stack frame; the new sleeper has `task =
current` set fresh, a fresh timer node, and `__state` is set fresh by
the loop's `set_current_state`. **There is no per-task latched flag
that survives between sleeps.** Every entry to `do_nanosleep` re-
establishes the state machine from scratch.

`current->__state` is mutated by exactly two writers, serialised by
`p->pi_lock` plus the `set_current_state` barrier:

- The running task itself, via `set_current_state` (entering a sleep)
  or `__set_current_state(TASK_RUNNING)` (post-wake reset).
- A waker, via `try_to_wake_up`, holding `p->pi_lock`.

The task never reads its own `__state` to make blocking decisions — it
reads the *condition* (the wait-queue entry, `t->task`, an atomic
flag, etc.) instead.

## m3OS's design (concrete)

`block_current_unless_woken_inner` (`kernel/src/task/scheduler.rs:1237`)
runs under `SCHEDULER.lock`, sets four pieces of state:

```rust
sched.tasks[idx].state = BlockedOnRecv;
sched.tasks[idx].switching_out = true;
// counter += if old.is_none() && new.is_some() { 1 } else if ...
sched.tasks[idx].wake_deadline = wake_deadline;
```

drops the lock, publishes `idx` to `PENDING_SWITCH_OUT[my_core]`, and
calls `switch_context`. The wake side has three paths:

- `wake_task` (line 1377): if `state ∈ {BlockedOn*}` and
  `switching_out == false`, sets `state = Ready` and returns idx for
  enqueueing. If `switching_out == true`, instead sets
  `wake_after_switch = true` and returns "no enqueue" — relying on the
  dispatch switch-out handler to consume the flag.
- `scan_expired_wake_deadlines` (line 2156): same fork — if the task is
  switching_out at scan time, sets `wake_after_switch = true`; otherwise
  transitions to Ready and pushes to the enqueue array.
- The dispatch switch-out handler (line 2013): for the task whose idx
  was published to `PENDING_SWITCH_OUT[core]`, reads
  `wake_after_switch`, then clears it, then conditionally enqueues if
  the task is blocked.

Compared to Linux:

- **Three observable states** instead of one (`Ready`, `Blocked +
  switching_out=true`, `Blocked + switching_out=false`), plus a
  side counter (`ACTIVE_WAKE_DEADLINES`) that mirrors deadline
  Some-ness.
- **Two latched per-task flags** (`switching_out`, `wake_after_switch`)
  whose lifetime is "from this block call until the next dispatch
  cycle picks up `PENDING_SWITCH_OUT[core]`". On re-block, the second
  flag should be false (the dispatch handler clears it at line 2082)
  but the invariant relies on the handler having actually run.
- **The waker has to choose between two actions** (direct Ready +
  enqueue, or set `wake_after_switch`) based on a flag (`switching_out`)
  that is *meaningful* only because the task itself just wrote it
  before yielding. This is the dance Linux specifically eliminated by
  making the wake idempotent against state.

## The specific invariant Linux maintains that m3OS violates

> Linux: the task's "should I block?" decision and the "I am blocked"
> state are written in a single barrier-ordered operation, and the
> wake observes them atomically — there is no intermediate "switching
> out" state visible to the waker that requires the waker to defer
> enqueue to a later phase.

m3OS's `wake_after_switch` is exactly that intermediate state. It is
maintained by *trust in the dispatch loop's promptness*, not by lock
ordering. Empirically (PR #118): when something else perturbs the
dispatch ordering on the same core (heavy logging, an SMP reschedule
IPI, a scan that fires between block and the handler), the latched
flag and the actual state can desynchronise.

The audit (subagent report retained in this turn's transcript) is most
confident in this re-block scenario:

1. Task T blocks on D1. `switching_out=true`, `wake_after_switch=false`,
   `wake_deadline=Some(D1)`.
2. Switch-out handler reads `wake_after_switch=false`, clears
   `switching_out=false`, does not enqueue.
3. Some time later D1 expires; `scan_expired` finds T with
   `switching_out=false`, sets `state=Ready`, pushes for enqueue.
4. T runs, returns from block, runs userspace, calls
   `block_current_unless_woken_until(D2)` again.
5. Inside the lock: `state=BlockedOnRecv`, `switching_out=true`,
   `wake_deadline=Some(D2)`. **`wake_after_switch` is whatever it was
   left at by some earlier path** — the new block does NOT clear it
   in current code.
6. If a wake arrives during the `switching_out` window for D2 and
   `wake_after_switch` is *already* true (left over from a prior
   asymmetric scan/wake interaction), the handler clears it and
   enqueues T immediately — **before** D2 actually expired. T runs as
   though awoken, but its waker condition (deadline expiry) is false.
   Some callers loop and re-block; others return as if the deadline
   fired.

The reverse scenario (a stale `wake_after_switch=true` causing an
*orphan* wake that never fires because the handler already consumed
it for an earlier block) is also reachable in principle. We have not
empirically nailed which of the two manifests as the SSH hang we
observed; both are eliminated by the same fix.

## Why my "obvious" fix didn't work

I tried adding `task.wake_after_switch = false` at the start of every
`block_current_*` variant (and `yield_now`). Result: the boot path
broke (banner-exchange timeouts, `stale-ready` warnings on PIDs 4,
14, 15). The fix conflicts with the existing protocol because the
**dispatch handler depends on `wake_after_switch=true` being set by a
*prior* block's wake to know whether to enqueue the *current* block's
task**. By clearing the flag at every block entry, I deleted the
hand-off between the prior wake and the next dispatch.

This is itself diagnostic: the m3OS protocol *requires* the latched
flag to span block calls. That is precisely the property Linux
designed out, and is precisely why a small change breaks more than it
fixes. **A correct fix is a wholesale rewrite of the block/wake
protocol, not a flag-clear patch.** See "Recommendation" below.

## What was committed in this pass

- **`kernel/src/arch/x86_64/syscall/mod.rs`**:
  - Two `log::debug!` → `log::info!` promotions for
    `[signal] [pX] killed/stopped by signal Y` so default-disposition
    Terminate/Stop deliveries are visible without a global log-level
    bump (useful for pid-14-killed-silently diagnosis).
  - In the `TIOCSCTTY` ioctl handler, also set `pair.slave_fg_pgid =
    proc.pgid` (Linux 4.6+ behaviour). Without this,
    `kernel/src/pty.rs::close_master`'s SIGHUP-to-fg-pgrp delivery is
    a no-op (sends to pgid 0) and the upstream `ion` shell never
    receives SIGHUP on PTY-master close. **This is a real correctness
    fix even though it does not, by itself, resolve the disconnect
    hang** (the cleanup never reaches `close_master` because of the
    deeper scheduler issue).
- **`kernel/src/pty.rs`**: new `set_slave_fg_pgid(id, pgid)` helper
  exposed for the TIOCSCTTY path.
- **`userspace/sshd/src/session.rs`**: `cleanup` function uses
  SIGHUP → 500 ms grace (WNOHANG poll) → SIGKILL escalation →
  blocking waitpid. Defensive against ion catching SIGHUP; bounded
  cleanup time. Does not, by itself, fix the disconnect hang either —
  cleanup hangs in the WNOHANG/nanosleep grace loop because of the
  same scheduler issue.

What was tried and reverted:

- `task.wake_after_switch = false` at the start of every
  `block_current_*` (broke yield_now / general scheduling).
- Replacing `sys_nanosleep`'s long-sleep busy-yield with
  `block_current_unless_woken_until` (introduced widespread
  `stale-ready` warnings; same underlying scheduler issue).

## Recommendation

**Adopt Linux's "single state word + condition recheck after state
write" pattern in m3OS.** The minimum viable form:

```rust
// New invariant: `task.state` is the single source of truth for
// "is this task blocked". `wake_after_switch` and `switching_out` are
// deleted. Wakes use a CAS on state.

fn block_current_unless_woken_until(woken: &AtomicBool, deadline: u64) {
    loop {
        let idx = current_task_idx();
        // 1. Atomic state write WITH FULL BARRIER, under SCHEDULER lock.
        {
            let mut sched = SCHEDULER.lock();
            sched.tasks[idx].state = TaskState::BlockedOnRecv;
            sched.tasks[idx].wake_deadline = Some(deadline);
            // counter ↑ if transitioning None→Some
        }

        // 2. Re-check condition AFTER the state write. If the wake
        //    already happened, the waker has set `woken=true` (or for
        //    deadline-only blocks, deadline is now in the past). Self-
        //    revert state and return without yielding.
        if woken.load(Acquire) || tick_count() >= deadline {
            let mut sched = SCHEDULER.lock();
            if sched.tasks[idx].state == TaskState::BlockedOnRecv {
                sched.tasks[idx].state = TaskState::Running;
                if sched.tasks[idx].wake_deadline.take().is_some() {
                    ACTIVE_WAKE_DEADLINES.fetch_sub(1, Relaxed);
                }
            }
            return;
        }

        // 3. Yield. The scheduler does NOT need to know "this is a
        //    blocking yield" — the state field already says Blocked,
        //    so dispatch won't pick this task again until a waker
        //    sets it to Ready.
        yield_to_scheduler();

        // 4. Loop only if spuriously woken (rare). The expected path
        //    is one trip through the loop.
        if woken.load(Acquire) || tick_count() >= deadline { return; }
    }
}

fn wake_task(idx: usize) {
    let mut sched = SCHEDULER.lock();
    let task = &mut sched.tasks[idx];
    // CAS: only wake if currently Blocked. A wake to a Running or
    // already-Ready task is silently dropped (Linux semantics).
    let blocked = matches!(task.state, BlockedOnRecv | BlockedOnSend |
                                       BlockedOnReply | BlockedOnNotif |
                                       BlockedOnFutex);
    if blocked {
        if task.wake_deadline.take().is_some() {
            ACTIVE_WAKE_DEADLINES.fetch_sub(1, Relaxed);
        }
        task.state = TaskState::Ready;
        // enqueue under same lock
    }
}
```

Key changes:

1. **Delete `switching_out` and `wake_after_switch`.** No flag
   handshake between the block path and the dispatch loop. The state
   field is the single source of truth.
2. **The condition recheck in step 2 closes the lost-wakeup window.**
   If a wake arrived between `unlock` and the recheck, the waker
   already wrote `state=Ready` (under our lock); our recheck sees the
   `woken` flag (set by the same waker before its CAS) and we self-
   revert without yielding. If the wake arrives *after* the recheck
   but *before* the yield-context-switch completes, the wake's
   state-write is observed by the dispatch loop's `pick_next` (which
   reads `state == Ready` to pick).
3. **The waker's CAS makes spurious wakes safe.** A wake to a task
   that has already been woken by a different path is a no-op, not a
   double-enqueue.
4. **Per-call `wake_deadline` registration is fresh every block.**
   No latched flags carry over, mirroring Linux's stack-allocated
   `hrtimer_sleeper`.

This is non-trivial work. It touches every IPC syscall, every
notification waiter, every futex, and the dispatch-loop switch-out
handler. The recommendation is to do it as a dedicated phase, with
the audit's transition table (in the subagent report retained in
this turn's transcript) as the test matrix.

A smaller intermediate step worth trying first: **acquire a per-task
spinlock around the block/wake transition**, mirroring Linux's
`p->pi_lock`. The current `SCHEDULER.lock()` is global; a per-task
lock plus state-CAS gives most of the benefit without the global
contention.

## What the next agent should do

1. **Land the bundled diff as-is** (TIOCSCTTY + signal-log promotion +
   cleanup hardening + `set_slave_fg_pgid` helper). All four are
   correctness improvements that hold even if the scheduler rewrite
   never lands. The TIOCSCTTY fix in particular makes m3OS conformant
   to the Linux/Redox PTY contract that `ion` was written against.
2. **Treat the scheduler rewrite as a separate phase.** Use the
   transition table the m3OS audit subagent produced (in this turn's
   transcript) as the spec for the new state machine. Aim for the
   Linux pattern: single state word, condition recheck, CAS wakes.
3. **Add a periodic scheduler-state diagnostic** (`[WARN] [sched]
   tasks: pid=X state=Y wake_deadline=Z switching_out=W
   wake_after_switch=V` for every task every N seconds) so that any
   future "task stuck in Blocked forever" symptom produces direct
   evidence at the moment of hang. This is the missing tool that
   would have shortened this PR's debugging by hours.
4. **Investigate ion's PTY canonical-mode behaviour separately.** The
   Redox subagent flagged that ion's liner uses `into_raw_mode()` to
   disable ICANON; verify m3OS's `TCSETS` actually clears ICANON on
   the slave side. If raw mode isn't reaching the kernel PTY driver,
   `Ctrl-D` doesn't propagate through canonical-mode line discipline
   and ion's `^D` exit path is broken even on a correct scheduler.

## Sources

- Linux: `kernel/time/hrtimer.c::hrtimer_nanosleep`, `do_nanosleep`,
  `hrtimer_wakeup`; `kernel/sched/core.c::set_current_state`,
  `try_to_wake_up`; `include/linux/sched.h` task-state flags.
- Redox: `redox-os/ion/src/binary/{readln,signals}.rs` for ion's
  termination and signal handling; `redox-os/kernel` PTY scheme for
  the close-master-sends-SIGHUP-to-fg-pgrp contract that ion expects.
- m3OS audit: `kernel/src/task/scheduler.rs:1237-1284` (block_inner),
  `:1377-1437` (wake_task), `:2013-2129` (dispatch switch-out
  handler), `:2156-2199` (scan_expired).
- m3OS PTY: `kernel/src/pty.rs:88-117` (close_master),
  `kernel/src/arch/x86_64/syscall/mod.rs:9661-9690` (TIOCSCTTY).
