# Phase 57e — `preempt-full` Userspace Hangs Handoff

**Status:** Open. Three independent userspace failures observed under `M3OS_KERNEL_FEATURES=preempt-full` after the Bug #1–#5 fixes landed. All reproduce on `feat/phase-57e-full-preemption` at `defb146` (PR #136). The default build (preempt-voluntary) is unaffected.
**Source ref:** Phase 57e (`feat/phase-57e-full-preemption`, PR #136).
**Companion:** `docs/handoffs/57e-preempt-full-boot-crash.md` (Bugs #1–#5, the dispatch reentrancy and per-core syscall snapshot fixes), `docs/handoffs/57e-kernel-preempt-audit.md`, `docs/handoffs/57e-dispatch-reentrancy.md`.

This handoff describes three reproducible userspace correctness failures that the previous handoff classified as "Bug #6 family — performance / quality, NOT correctness." The failure logs in `m3os-bad-term.log`, `m3os-freeze-term.log`, and `m3os.log` show that those warnings are not benign — they coincide with hangs that block forward progress. The classification needs to be revised and the underlying race fixed before Track G's 24 h soak gate can open.

---

## TL;DR

- After Bug #1–#5 fixes, the `preempt-full` kernel boots, executes services, and runs userspace through several fork generations. **It does not stay healthy.**
- Three distinct userspace failures reproduce, each consistent with a **lost wakeup** for an IPC reply or notification — `wake_task_v2` either never ran, or ran without re-queueing the target.
- The kernel emits **`[WARN] [sched] task pid=N … state=BlockedOnReply stuck-since=Wms (no waker registered)`** — this fires on the doom hang and is the cleanest signature.
- The "benign" `[WARN] [sched] dequeue-drop core=N … reason=state-not-ready extra=0x2` warnings are correlated with the hangs, not benign.
- Default build is unaffected on all three reproducers.

## The three reproducers

All three were captured from `cargo xtask run-gui` with `M3OS_KERNEL_FEATURES=preempt-full`. None reproduce under the default `preempt-voluntary` build.

### Reproducer 1 — `fb-takeover doom` never appears in framebuffer (`m3os.log`)

```bash
M3OS_KERNEL_FEATURES=preempt-full cargo xtask run-gui
# Login as root, then at the ion prompt:
fb-takeover doom
```

**Expected:** doom takes over the framebuffer and renders the title screen.
**Observed:** display_server logs `framebuffer yielded for takeover`, fb-takeover spawns doom (pid 24), doom maps the framebuffer, then nothing renders. After ~30 s the watchdog starts logging:

```
[WARN] [sched] task pid=19 name=fork-child state=BlockedOnWait stuck-since=30162ms (no waker registered)   ← ion
[WARN] [sched] task pid=23 name=fork-child state=BlockedOnWait stuck-since=30159ms (no waker registered)   ← fb-takeover
[WARN] [sched] task pid=24 name=fork-child state=BlockedOnReply stuck-since=30234ms (no waker registered)  ← doom
```

ion (pid 19) and fb-takeover (pid 23) `BlockedOnWait` are **downstream** — they wait on doom's exit. The head of the chain is **pid 24 doom in `BlockedOnReply`**: doom called `ipc_call` somewhere, got blocked waiting for a reply, and never resumed. The reply was either never delivered or never woke the task.

Immediately before the hang we also see:

```
[INFO] [framebuffer_mmap] pid=24 mapped 1000 pages @ 0x20003eb000
[WARN] [sched] dequeue-drop core=3 idx=26 pid=24 reason=state-not-ready extra=0x2
```

`extra=0x2` is `TaskState::BlockedOnRecv`. So doom transitioned through Ready → enqueued → BlockedOnRecv between dequeue scans, and *also* later got stuck in `BlockedOnReply`.

Captured log: `m3os.log` (lines 622–918+).

### Reproducer 2 — terminal freeze after tab-completion (`m3os-freeze-term.log`)

```bash
M3OS_KERNEL_FEATURES=preempt-full cargo xtask run-gui
# Login as root. At the ion prompt:
ls
# Type a partial command and hit TAB.
```

**Expected:** ion either completes the command or rings the bell.
**Observed:** the terminal goes silent. `display_server`'s compose loop stops emitting heartbeats:

```
display_server: compose#1980 ok0 writes=0 total=5117 keys=71/19202 ptrs=526/18747 pos=285,190 irq1=77 irq12=1038 mbytes=1737 mpkts=579 mdrops=0
        ← log stops here, no compose#2040 ever appears
```

Notable: `keys=71/19202` shows display_server is *receiving* keyboard events from the kernel scancode buffer (irq1=77) but only forwarded 71 to clients. Right before the freeze we see repeated `dequeue-drop` for vfs_server (pid 11) on multiple cores.

The likely victim is `term` blocked on `ipc_call` to `display_server` (or the reverse), with the reply lost the same way as in Reproducer 1.

Captured log: `m3os-freeze-term.log` (last activity ~line 826).

### Reproducer 3 — garbled prompt at first boot (`m3os-bad-term.log`)

```bash
M3OS_KERNEL_FEATURES=preempt-full cargo xtask run-gui
# Click QEMU window during boot to grab keyboard. Wait for login prompt.
```

**Observed:** instead of the normal ion prompt, term reads bytes `0x5e 0x5b` (`^[`) repeatedly from the PTY:

```
term: pty-read len=2 hex=5e5b
term: pty-read len=2 hex=5e5b
... (30+ identical reads)
term: pty-read len=3 hex=5e5b64       ← finally diverges
```

`^[` is the printable rendering of the Escape key under termios `ECHOCTL`. **This is most likely echo of buffered Escape keystrokes** the user typed during the (longer) preempt-full boot — same root cause class as Reproducer 1 and 2 (slow scheduling forcing more keystrokes to queue), but probably *not* a unique bug.

Captured log: `m3os-bad-term.log`.

**Triage advice:** investigate Reproducers 1 and 2 first; treat Reproducer 3 as a likely consequence of the same scheduling slowdown that precedes the hangs. If 1 and 2 are fixed, retry 3 to confirm.

---

## Why the previous handoff's "benign" classification is wrong

`docs/handoffs/57e-preempt-full-boot-crash.md` § *Residual (Bug #6 family — performance / quality, NOT correctness)* lists:

> `[WARN] [sched] dequeue-drop core=N idx=N pid=N reason=state-not-ready extra=0x2` — run-queue entries for tasks already `BlockedOnRecv (0x2)`. Dispatcher correctly drops; downstream of cross-core wake/block races. **Benign log of state inconsistency.**

The three reproducers above show this is **not** benign. The same warnings precede every hang, and the hangs are **`(no waker registered)`** stuck-since warnings on `BlockedOnReply` / `BlockedOnWait` tasks. The dequeue-drop is the visible tip of a state-machine race that also drops legitimate wakeups.

Track G's 24 h soak gate must remain **closed** until this is fixed.

---

## Hypothesis — H7: lost wakeup in two-step IPC reply path

The reply-side code path in `kernel/src/ipc/endpoint.rs::reply` (line 925) is **two scheduler-lock acquisitions with a non-atomic gap**:

```rust
pub fn reply(server: TaskId, caller: TaskId, reply_msg: Message) {
    transfer_bulk(server, caller);
    scheduler::deliver_message(caller, reply_msg);  // (A) lock, set pending_msg, set waker, unlock
    crate::trace::trace_event(...);
    let _ = crate::task::scheduler::wake_task_v2(caller);  // (B) pi_lock + sched_lock CAS, enqueue
}
```

`deliver_message` (`scheduler.rs:3287`) sets `pending_msg = Some(msg)` and `reply_waker.store(true)` under `scheduler_lock`, then drops the lock.

Under `preempt-full`, the `IrqSafeMutex::Drop` for that scheduler lock calls `preempt_enable()` (`scheduler.rs:1659`), which can fire **`yield_now()` immediately** if `preempt_count` zero-crosses with `reschedule == true` and `IF == 1` (`:1693–1714`):

```rust
if prev == 1 && pc.reschedule.load(Relaxed) {
    #[cfg(feature = "preempt-full")]
    {
        if x86_64::instructions::interrupts::are_enabled() {
            pc.preempt_resched_pending.store(false, Release);
            pc.reschedule.store(false, Release);
            yield_now();          // ← SERVER YIELDS BEFORE wake_task_v2 RUNS
            return;
        }
    }
    pc.preempt_resched_pending.store(true, Release);
}
```

If a wakeup somewhere else set `reschedule = true` while the server held `scheduler_lock` inside `deliver_message`, the server yields between (A) and (B). Now:

- **Caller's `pending_msg` is set, `reply_waker` is `true`, but `state == BlockedOnReply` and the caller is not on any run queue.**
- **`wake_task_v2(caller)` has not run.**

In the happy case, the server is eventually re-dispatched, returns from `yield_now`, finishes `reply` by calling `wake_task_v2`, which transitions the caller to Ready and enqueues it. The caller wakes after a bounded delay.

In the failure case there is a window where `wake_task_v2` is delayed long enough that the caller's `block_current_until` step 3 has *already* observed `woken == false` and proceeded into `switch_context`. The caller is now parked. **If the server's resumption is lost or its `wake_task_v2` returns `AlreadyAwake` for any reason, the caller stays parked.** The `(no waker registered)` watchdog observes exactly that.

`AlreadyAwake` causes:

- The caller's slot was Dead-recycled between `deliver_message` and `wake_task_v2`. (Unlikely for a long-lived task like doom, but possible on heavy churn.)
- The caller's state is no longer `Blocked*` because *something else* already woke it (a signal, a deadline, or — under preempt-full — a stale enqueue from an earlier transition). `wake_task_v2`'s CAS check (`scheduler.rs:3128–3139`) bails on non-`Blocked*` states.
- The caller's `id` no longer matches at the slot identity revalidation in step 2.

Combined with the dequeue-drop warnings (which show entries with state `BlockedOnRecv` being filtered), the picture is consistent with **the caller making one more state transition than the wake side expects**, sliding the state out from under `wake_task_v2`'s CAS exactly when the server's `preempt_enable` interleaved a yield into the middle of the reply.

**Why this is preempt-full-only:** under `preempt-voluntary`, `preempt_enable` never calls `yield_now` — the worst it does is set `preempt_resched_pending` for the next user-mode return boundary. The two-step reply path runs without the server interruption, so `deliver_message` and `wake_task_v2` are effectively atomic from the caller's perspective.

### Companion symptom: dequeue-drop with `BlockedOnRecv`

The `state-not-ready extra=0x2` drops happen when a task has been pushed onto a run queue (state was Ready at enqueue time) but its state has reverted to `BlockedOnRecv` before `dequeue_local` reaches it. Two paths produce this:

1. **Spurious wake.** A wake fires while the task is mid-recv-loop. The task transitions Ready → Running → BlockedOnRecv (server processes the message, replies, loops back to `recv`) before the dispatcher reaches the queue entry. The entry is dropped — no harm done.
2. **Wake against an already-running recv-loop iteration.** The wake CAS *should* fail because state is Running, not Blocked\*. But under preempt-full's interleavings, the state-write ordering is more complex; a CAS may succeed against a transitional state and enqueue a task that's about to re-block.

Either way, the warnings are a fingerprint of the same race window as H7. Confirming H7 should also explain these.

---

## Diagnostic plan (ordered)

### Step 0 — make the trace rings observable on a hang

The existing trace machinery records the events we want, **but the harness doesn't suit a hang-without-panic scenario.** Inventory before instrumenting:

- `feature = "trace"` is already in the default feature set (`kernel/Cargo.toml:16`). The `crate::trace::trace_event(...)` calls in `endpoint.rs`, `notification.rs`, and `scheduler.rs` are already live, recording `RecvBlock` / `RecvWake` / `SendBlock` / `SendWake` / `CallBlock` / `ReplyDeliver` / `MessageDelivered` / `WakeTask` / `RunQueueEnqueue` / `Dispatch` / `SwitchOut` / `YieldNow`. **No new tracepoints need to be added** for the wake/reply path.
- `feature = "sched-trace"` adds a *second* ring (`SchedTrace` entries: pid + old_state + new_state + `#[track_caller]` `file:line`) at six callsites in `scheduler.rs`. Strictly an addition; useful to distinguish *which* code path performed a transition. Worth enabling once Step 1's first capture proves which side of the wake protocol fails.
- **Both rings are 256 entries per core** (`kernel/src/smp/mod.rs:298`, `kernel/src/task/sched_trace.rs:48`). At 1 kHz timer + IPC flux that wraps in tens of milliseconds; by the time the 30 s watchdog fires, the ring has been overwritten thousands of times.
- **Both rings only dump from the panic handler.** A doom hang doesn't panic, so neither ring ever reaches serial today.

To make the rings actually observable on this bug, do both of:

1. **Bump ring capacity.** Change `TraceRing<256>` to `TraceRing<4096>` at `kernel/src/smp/mod.rs:298` and `SCHED_TRACE_RING_SIZE` to 4096 at `kernel/src/task/sched_trace.rs:48`. Memory cost: 4096 × `size_of::<TraceEntry>()` × N_CORES ≈ a few hundred KiB total — negligible for diagnostic builds.
2. **Pick one of these dump triggers** (any one is sufficient):
   - **(a) Self-diagnosing watchdog (preferred).** Wire `crate::trace::dump_trace_rings()` (and `dump_sched_trace_rings()` if enabled) into `watchdog_scan` (`scheduler.rs:4498`) so the first `(no waker registered)` warning also dumps the rings. Cleanest signal-to-noise.
   - **(b) Panic on stuck.** Add `panic!("[sched] stuck pid={pid} state={state:?} stuck-since={stuck_ms}ms")` to the same watchdog branch when `stuck_ms > 60_000`. Reuses the existing panic-time dump path with zero new code. Backs out cleanly once the fix lands.
   - **(c) Debug syscall.** Add a `SYS_DUMP_TRACE_RINGS` syscall callable from a small userspace helper. Most flexible but slowest to wire up.

Once the rings are visible, proceed to Step 1.

### Step 1 — sched-trace capture around `fb-takeover doom`

1. Build with `M3OS_KERNEL_FEATURES="preempt-full,sched-trace"` and run `fb-takeover doom`.
2. After the watchdog fires (and the dump trigger from Step 0 prints the rings), inspect the trace ring for events touching `task_idx == doom_idx`:
   - `WakeTask { task_idx: doom, state_before: BlockedOnReply, … }` — was a wake CAS attempted? Did it succeed?
   - `MessageDelivered { task_idx: doom, ep: … }` — did a server actually call `deliver_message` for doom?
   - `ReplyDeliver { caller_idx: doom, ep: … }` — was the reply sent at all?
   - `RunQueueEnqueue { task_idx: doom, core: … }` — did `wake_task_v2` step 5 run?
   - `Dispatch { task_idx: doom, … }` — did the scheduler ever pick doom after the alleged wake?
   - `YieldNow { task_idx: server, … }` — did the server yield between deliver_message and wake_task_v2?

The traces will localise whether the bug is "server yielded mid-reply and `wake_task_v2` never ran" vs "`wake_task_v2` ran but returned `AlreadyAwake`" vs "`wake_task_v2` enqueued but dispatcher dropped the entry".

### Step 2 — instrument `reply` and `deliver_message` directly

If the trace ring data is ambiguous, add a per-event counter to `endpoint.rs::reply`:

```rust
pub fn reply(server: TaskId, caller: TaskId, reply_msg: Message) {
    transfer_bulk(server, caller);
    scheduler::deliver_message(caller, reply_msg);
    let outcome = crate::task::scheduler::wake_task_v2(caller);
    log::warn!(
        "[ipc-trace] reply: server={} caller={} wake_outcome={:?}",
        server.0, caller.0, outcome
    );
}
```

If `wake_outcome == AlreadyAwake` for the doom hang, H7's "wake_task_v2 returns AlreadyAwake because state slipped" branch is confirmed; otherwise look at whether `wake_task_v2` even ran (maybe the server died mid-reply, or yielded and never resumed).

### Step 3 — verify the preempt_enable-yield_now hypothesis

Add a per-core counter that increments inside `preempt_enable` whenever the `yield_now()` path is taken. Log the counter from the watchdog scan when it fires `(no waker registered)`. If the counter is non-zero and grows during the doom flow, the synchronous yield-on-preempt-enable is exercised; that does not by itself confirm the bug but shows the path is hot.

---

## Proposed fixes (in order of preference)

### F1 — Make `reply` atomic: collapse `deliver_message` + `wake_task_v2` into one critical section

The cleanest fix. The reply must look atomic to the caller's `block_current_until` state machine. Move the wake into `deliver_message` (or a new `deliver_and_wake` helper) so both happen under a single `scheduler_lock` acquisition with no preempt-enable boundary between:

```rust
pub fn deliver_message_and_wake(id: TaskId, msg: Message) -> WakeOutcome {
    // Step 1: deliver under scheduler_lock.
    let needs_wake = {
        let mut sched = scheduler_lock();
        if let Some(idx) = sched.find(id) {
            sched.tasks[idx].pending_msg = Some(msg);
            if let Some(waker) = sched.tasks[idx].reply_waker.as_ref() {
                waker.store(true, Ordering::Release);
            }
            matches!(
                sched.tasks[idx].state,
                TaskState::BlockedOnRecv
                    | TaskState::BlockedOnSend
                    | TaskState::BlockedOnReply
                    | TaskState::BlockedOnNotif
                    | TaskState::BlockedOnFutex
                    | TaskState::BlockedOnWait
                    | TaskState::BlockedOnService
            )
        } else {
            false
        }
    };
    if needs_wake {
        // wake_task_v2 takes pi_lock OUTER, scheduler_lock INNER — different
        // locking order than above (which only takes scheduler_lock).  No
        // deadlock because we drop scheduler_lock before re-entering.
        crate::task::scheduler::wake_task_v2(id)
    } else {
        WakeOutcome::AlreadyAwake
    }
}
```

The two-step nature is preserved (the locks must follow the pi_lock-OUTER discipline) but the **`preempt_enable` zero-crossing between them is suppressed** by holding `preempt_count > 0` across the gap. Easiest implementation: bump `preempt_disable()` at entry to `reply`, drop it after `wake_task_v2`. Look at:

```rust
pub fn reply(server: TaskId, caller: TaskId, reply_msg: Message) {
    crate::task::scheduler::preempt_disable();
    transfer_bulk(server, caller);
    scheduler::deliver_message(caller, reply_msg);
    crate::trace::trace_event(...);
    let _ = crate::task::scheduler::wake_task_v2(caller);
    crate::task::scheduler::preempt_enable();
}
```

Trade-off: extends the kernel-mode-non-preemptible window slightly. Acceptable — the reply path is short and the lock-release-then-reacquire pattern was the leak.

### F2 — Apply the same atomicity discipline to every `deliver_message` + `wake_task_v2` pair

`grep -rn "deliver_message" kernel/src/` shows several call sites in `endpoint.rs` (lines 340–462), `cleanup.rs` (lines 94–106), and a few signal/EINTR delivery paths. Each pair has the same structural risk. F1 should be re-cast as a *helper* that all of them call rather than per-site `preempt_disable` / `preempt_enable`.

Notification wakes (`ipc/notification.rs:544, 752`) already follow the pattern of "set bit then `wake_task_v2`" — verify those are also `preempt_disable`-bracketed if the wake target's state machine has the same vulnerability. (They might be safer because notifications use `BlockedOnNotif` and a different `register_*_waker` slot.)

### F3 — Suppress `yield_now` in `preempt_enable` when the caller is on a "kernel critical path"

Less surgical. Add a per-task or per-core "no synchronous yield" guard around IPC reply / deliver paths so `preempt_enable`'s zero-crossing only sets `preempt_resched_pending` and never calls `yield_now()` from inside those paths. The next IRQ-return or genuine preempt point would still consume the flag.

Trade-off: gives back some of the preempt-full latency wins in IPC paths. If F1 + F2 close the bug, F3 is unnecessary.

### F4 — Make `wake_task_v2` idempotent across a "Blocked → Ready → Blocked → Ready" sequence

Last-resort. If F1/F2 turn out to be incomplete because the dequeue-drop race is a *separate* lost-wake source, audit `wake_task_v2`'s CAS check at `scheduler.rs:3128–3139` and the queue-entry filter at `scheduler.rs:796–806`. Specifically: if a task transitions Blocked → Ready → Blocked → Ready in quick succession, and only one of the wakes enqueues, the task may be Ready-without-queue-entry and only get dispatched on a later wake. Document the invariant explicitly and (if needed) re-enqueue on every successful CAS even when the task is already Ready.

---

## What to *not* do

1. **Don't disable `preempt-full`'s yield-on-preempt_enable path globally.** That defeats the latency goal of 57e and reverts Track F.2. Targeted suppression around IPC reply paths (F3) is the fallback only.
2. **Don't add `without_interrupts` blocks around the IPC reply path.** That masks IRQs across an unbounded wake chain (server replies to caller A, which wakes server B, which …) and breaks the single-IRQ-per-tick LAPIC accounting. Use `preempt_disable` / `preempt_enable` instead.
3. **Don't change the watchdog to suppress `(no waker registered)` logs.** They're the smoking gun. If F1/F2 land, the warnings should disappear naturally.
4. **Don't try to "fix" Reproducer 3 (the `^[` echo) directly.** It's almost certainly an effect of slowed boot under preempt-full; if F1/F2 close the hangs, the boot timing should normalise and the user will have less time to type into the void.

---

## File / line index

Pre-load these for the next session:

- `kernel/src/ipc/endpoint.rs:925-940` — `reply()` (the two-step reply path)
- `kernel/src/ipc/endpoint.rs:340-462` — other `deliver_message` + `wake_task_v2` pairs
- `kernel/src/ipc/cleanup.rs:94-107` — error-path delivery + wake pairs
- `kernel/src/task/scheduler.rs:3287-3315` — `deliver_message` / `try_deliver_message`
- `kernel/src/task/scheduler.rs:3076-3284` — `wake_task_v2` (CAS, on_cpu spin, enqueue)
- `kernel/src/task/scheduler.rs:2416-2632` — `block_current_until` (state writes, condition recheck, yield)
- `kernel/src/task/scheduler.rs:2663-2688` — `block_current_on_reply_v2`
- `kernel/src/task/scheduler.rs:1659-1725` — `preempt_enable` (zero-crossing yield_now under preempt-full)
- `kernel/src/task/scheduler.rs:2034-2080` — `yield_now`
- `kernel/src/task/scheduler.rs:737-852` — `pick_next` / `dequeue_local` (the entry filter that emits `state-not-ready` warnings)
- `kernel/src/arch/x86_64/interrupts.rs:1418-1479` — `check_and_preempt_kernel`
- `kernel/src/ipc/notification.rs:544, 752` — notification wake call sites (sanity check the same pattern)

---

## Captured log files

- `m3os.log` (74 KB) — fb-takeover doom hang, includes the BlockedOnReply watchdog cascade.
- `m3os-freeze-term.log` (58 KB) — terminal freeze after tab-completion.
- `m3os-bad-term.log` (41 KB) — `^[` echo at boot (likely cosmetic).
- `/tmp/m3os-h7-run1.log` … `run6.log` — diagnostic-instrumented runs from the next session (see *Session 2 findings* below).

All three GUI logs were taken at branch tip `defb146` with `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run-gui`.  The headless `run*.log` files reproduce the same hang signature without GUI input — see *Session 2 findings*.

---

## Session 2 findings (headless reproducer + first trace dump)

### Instrumentation now in tree (uncommitted on `feat/phase-57e-full-preemption`)

1. `kernel/src/smp/mod.rs:298` — `TraceRing<256>` → `TraceRing<4096>`.
2. `kernel/src/task/sched_trace.rs:48` — `SCHED_TRACE_RING_SIZE: 256` → `4096`.
3. `kernel-core/src/trace_ring.rs` — added `for_each_recent(max, f)` non-allocating last-N iterator.
4. `kernel/src/trace.rs` — added `dump_trace_rings_recent(max_per_core)`; existing `dump_trace_rings()` calls it with `usize::MAX`.
5. `kernel/src/task/scheduler.rs` — added `TRACE_DUMP_FIRED` / `TRACE_DUMP_PENDING` statics; trigger sites in `watchdog_scan` (StuckNoWaker) and in dispatch's stale-ready logger (≥ 1 s); deferred dump runs from BSP's dispatch loop body **outside any scheduler-context lock**, calling `dump_trace_rings_recent(256)`.

These collectively let the kernel emit a structured trace ring dump on the first watchdog or stale-ready signal, finishing fast enough not to destabilise the rest of the system.  All four host-test gates (`cargo xtask check`) stay green with the patch.

### Headless reproducer (no GUI / no doom needed)

```bash
M3OS_KERNEL_FEATURES=preempt-full cargo xtask run
# Wait ~60 s.  Smoke-runner pid 18 hits BlockedOnWait stuck-since=30000ms.
```

The smoke-runner forks `tcc` for the `tcc-version` and `tcc-compile` smoke steps; under preempt-full one of those forks (pid 19/20/21 depending on run) ends up blocked or busy-looping for the full 30 s, smoke-runner waitpid()s on it, watchdog fires.

### What the trace dump showed (run5 / run6, dump fired ~tick 30 200)

**Per-core summary:**

| Core | Trace tail tick | Behavior |
|---|---|---|
| 0   | ~30 500 | Active, normal IPC mix: `RecvWake` / `CallBlock` / `ReplyDeliver` / `MessageDelivered` between task_idx 13/14/15 (which look like vfs/fat/session IPC partners), interleaved with idle (task_idx 3) yield-loops. |
| 1   | ~30 500 | Similar profile to core 0. |
| 2   | **~366** | **Stuck** — last 256 events span tick 319–366 (~50 ms) and consist almost entirely of alternating `Dispatch { task_idx: 24 } → SwitchOut { task_idx: 24 } → Dispatch { task_idx: 25 } → SwitchOut { task_idx: 25 } → RunQueueEnqueue` cycles **with no `YieldNow` events** and **identical `rsp` / `saved_rsp` for many consecutive cycles**.  No core 2 trace events for ~30 s after tick 366. |
| 3   | ~30 500 | Periodic bursts (~10 ms apart) of `WakeTask { task_idx: 7, state_before: 2 }` etc., then idle yield-loop until next burst. |

The cpu-hog warning at the same time logs `pid=20 name=fork-child exec_path=/bin/ion core=2 ran~30035 ms final_state=BlockedOnReply` — ion ran on core 2 for 30 s without a single yield/preempt-then-resume cycle visible in the trace, then transitioned to `BlockedOnReply` and the watchdog caught it.

### Revised hypothesis — H7' kernel-mode preempt-resume livelock on the assigned core

The Dispatch + SwitchOut **without YieldNow** but **with re-enqueue (state stays Running)** signature matches exactly one path in `scheduler.rs`: `preempt_frame_to_scheduler` (`scheduler.rs:2151`) → `switch_context` → dispatch-loop epilogue.  The task is not blocking — it is being preempted in kernel-mode, switched out, immediately re-dispatched, and preempted again.  Loop frequency is ~200 cycles/ms (~5 µs per cycle), which is far faster than the 1 kHz LAPIC timer can drive on its own.

The IPI path is the prime suspect.  `enqueue_to_core` (`scheduler.rs:1056-1058`) sends a reschedule IPI to the target core whenever a wake comes from a different core:

```rust
if crate::smp::is_per_core_ready() {
    let current = crate::smp::per_core().core_id;
    if current != core_id {
        crate::smp::ipi::send_ipi_to_core(core_id, crate::smp::ipi::IPI_RESCHEDULE);
    }
}
```

If a server on core 0 (e.g. vfs / fat / session) is in a tight `recv → reply` cycle with thousands of wakes per second, every wake of a task on core 2 produces a reschedule IPI.  Each IPI on core 2 trips `reschedule_ipi_handler_kernel` → `check_and_preempt_kernel` → `preempt_to_scheduler_kernel` → `switch_context`.  If the wake rate exceeds the dispatch-cycle rate, core 2 never makes user-mode forward progress — exactly the observed cpu-hog with no preempt-cycle trace.

**This is structurally an IPI livelock, not a lost-wake.**  H7 (the original "lost reply wakeup" theory) is **not confirmed** by the trace data.  The hangs in `m3os-bad-term.log` / `m3os-freeze-term.log` / `m3os.log` are likely the same livelock manifesting as lock-up of the wake target's core; the BlockedOnReply / BlockedOnWait watchdog states are the user-visible *consequence*.

### What was ruled out

- **The synchronous `yield_now` in `preempt_enable`** (`scheduler.rs:1693-1714`).  Replacing it with the deferred-pending path produced the same hang signature in `m3os-h7-run6.log`.  Not the trigger.
- **A panic / triple-fault on core 2** during the 30 s window.  No panic markers in the log; the kernel never reboots in run5 or run6.
- **Trace ring tearing under instrumentation**.  Run3's reboots were diagnosed as side-effects of dumping ~16k entries inside the dispatch hot path before the BSP could re-enable IRQs; the bounded `dump_trace_rings_recent(256)` and deferred-pending pattern fixed it.

### Next-step candidates

1. **IPI rate-limit / coalesce.** Add an `enqueue_to_core` guard: if the target core's `reschedule` flag is already set (and the target is not `hlt`-stalled), skip the IPI.  Hypothesis: this collapses thousands of redundant IPIs per ms into a few, gives the target core room to actually run user code between resched events.  Suspect site: `scheduler.rs:1056-1058`.
2. **Investigate the wake source.** Add a per-source-core counter for `wake_task_v2` calls (or `enqueue_to_core` cross-core sends) per tick.  Confirm that a single source core fires the storm and find the originating IPC path.  Likely candidates: vfs_server (lots of slow `STAT_PATH` requests in run1) or fat_server.
3. **Verify "kernel-mode-preempt of ion" is the right path.** Add a one-line log in `preempt_to_scheduler_kernel` recording the saved RIP for ion's task_idx; correlate with kernel symbols to see which kernel function the preempt is firing inside.  If RIP is in `vfs_server` / IPC code, the wake-storm is a feedback loop with ion's syscall handler.
4. **Re-run with sched-trace.** The `sched-trace` ring records `pid + old_state + new_state + #[track_caller] file:line` for every state transition.  Pinpoints which Rust callsite is doing the rapid Ready/BlockedOnRecv flips associated with the wake storm.

### Files to pre-load (Session 3)

- `kernel/src/task/scheduler.rs:1021-1063` — `enqueue_to_core` (the IPI send site).
- `kernel/src/task/scheduler.rs:2151-2199` — `preempt_frame_to_scheduler` (the preempt-out path consistent with the trace pattern).
- `kernel/src/task/scheduler.rs:3076-3284` — `wake_task_v2` (potential wake-storm source).
- `kernel/src/arch/x86_64/interrupts.rs:1418-1479` — `check_and_preempt_kernel` (the gating check on every IRQ-return).
- `/tmp/m3os-h7-run5.log` and `/tmp/m3os-h7-run6.log` — captured trace dumps.

### IPI-coalesce experiment (run7, reverted)

Tried a one-line edit at `scheduler.rs:1041` to convert the unconditional `data.reschedule.store(true)` + IPI to a coalescing version: send the IPI only when we are the first to flip the target's `reschedule` flag from `false` to `true`.

```rust
let needs_ipi = !data.reschedule.swap(true, Ordering::AcqRel);
if crate::smp::is_per_core_ready() {
    let current = crate::smp::per_core().core_id;
    if current != core_id && needs_ipi {
        crate::smp::ipi::send_ipi_to_core(core_id, crate::smp::ipi::IPI_RESCHEDULE);
    }
}
```

**Result:** symptom changed but did not close.  The cpu-hog / stale-ready / stuck-no-waker warnings disappeared, suggesting the rapid IPI livelock was indeed suppressed.  But the smoke runner now hangs at `SMOKE:tcc-version:BEGIN` with no further output for the rest of the 80 s run — `display_server` keeps composing (so cores are alive) but no userspace progress is made.

**Why:** AP LAPIC timers run at **10 ms period** (`smp/boot.rs:578-585`), not 1 ms.  When the target AP is `hlt`-stalled and the source-core wake skips the IPI because `reschedule` was already true, the AP only resumes on its next local 10 ms tick.  In a heavy IPC chain, this turns each cross-core wake into a 0–10 ms delay; tasks that depend on multiple cross-core IPC round-trips can stack enough delay to look hung.  The coalesce also can lose a wake entirely if the target has consumed its old `reschedule` signal but not yet reached the `pick_next` that would drain the run queue — there is no second IPI to nudge it.

**Reverted in tree.**  The revised fix needs to either:

- **Keep the IPI but avoid trigger storms upstream.**  Find the wake-source that fires thousands of times per ms (likely vfs_server in a `recv → reply` loop driven by stat-storms during tcc-load) and rate-limit at the source, OR fix whatever causes the receiver to not actually drain the queue between wakes.
- **Coalesce smarter.**  Only skip the IPI if the target has *recently* received an IPI within some window, e.g. by tracking `last_ipi_tick` per core; for an AP that's been idle ≥ 10 ms since its last IPI, always send.
- **Fire IPI only after seeing an idle target.**  Track `is_idle_or_running` per core; send IPI only when target is idle (won't pick up the new entry on its current dispatch cycle).

The IPI livelock is real, but the bare coalesce is too aggressive for AP timer cadence.

### Acceptance criteria for closing this revised hypothesis

1. With the IPI rate-limit (or equivalent) in place, `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run` runs for at least 60 s with **zero `cpu-hog ran > 1 s` warnings** for any userspace task, and **zero stuck-no-waker watchdog hits**.
2. The trace ring around the previously-stuck point now shows core 2 making forward progress: `Dispatch` events for ion (or other userspace tasks) interleaved with normal `RecvBlock` / `RecvWake` IPC pairs, instead of the rapid `Dispatch+SwitchOut` cycles.
3. `cargo xtask smoke-test` (default features) still passes.
4. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` reaches `SMOKE:tcc-compile:PASS` (or whatever the next step is) within the standard timeout.

---

## Acceptance criteria for closing this bug

1. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run-gui`, login, `fb-takeover doom` — doom renders and accepts input; the watchdog never fires on doom's pid.
2. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run-gui`, login, type a partial command, hit TAB — ion completes the command (or rings the bell); display_server compose loop continues emitting heartbeats.
3. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` — passes within the standard timeout (no per-step overruns).
4. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run-gui` — over a 10-minute soak, **zero** `[WARN] [sched] dequeue-drop … extra=0x2` warnings *and* zero `(no waker registered)` watchdog warnings.

When (1)–(4) pass, reopen Track G's 24 h soak gate.

---

## Required reading before resuming

- `docs/handoffs/57e-preempt-full-boot-crash.md` § *Resolution* and § *Residual (Bug #6 family)* — bridges from where the previous session left off to this handoff.
- `docs/handoffs/57e-dispatch-reentrancy.md` — dispatch-path reentrancy windows; explains where preemption is meant to be safe and where it is not.
- `docs/handoffs/57a-scheduler-rewrite-v2-transitions.md` — the canonical state-transition protocol that `block_current_until` and `wake_task_v2` are supposed to implement atomically.

Recent commits worth scanning (most recent first):

```
defb146 fix(57e): address PR #136 review — preempt_disable scope, xsave IRQ contract, syscall_user_rsp doc
c3a3b84 docs(57e): record Bug #5 fix and Bug #6-family residual list   ← classified the warnings as benign; this handoff supersedes
4df5378 fix(57e): publish UserReturnState before Cr3::write in execve (Bug #5)
91ca96b fix(57e): route per_core_syscall_arg3 through per-task snapshot
4109101 fix(57e): make user_rsp per-task in TaskSyscallSnapshot (Bug #4)
d0bf15a fix(57e): move user GPR snapshot from per-core to per-task (Bug #3)
```
