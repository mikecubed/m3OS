# Phase 57e — `preempt-full` Userspace Hangs Handoff

**Status (branch tip `2e1bbc4`, end of Session 8):**

- **Bug #6 family** (preempt_enable zero-cross synchronous yield, three variants) — **closed** in Session 3 (commits `695f800`, `38d35ea`, `d83ecc7`, `3e3107c`).
- **Bug #7** (frame UAF / PML4[256] corruption — the residual that Sessions 4–7 chased as "slab UAF") — **closed** in Session 8 (commits `d8db950`, `22cd711`). Validated: 0 kernel page faults and 0 `[free_pt] !!!` defensive warnings across a 5-iteration `cargo xtask smoke-test` loop under `preempt-full,sched-trace`.
- **Bug #8** (the remaining `cargo xtask smoke-test` intermittency under `preempt-full`) — **open**. After Bug #7 closed, the smoke-test still passes only ~1/5 of the time; failures are now exclusively a `BlockedOnReply` watchdog cascade (same shape as the Sessions 1–3 lost-wakeup family) or a `prompt-ready` slow-boot timeout. **Track G's 24 h soak gate stays closed** until Bug #8 is fixed.

**Source ref:** Phase 57e (`feat/phase-57e-full-preemption`, PR #136).
**Companion:** `docs/handoffs/57e-preempt-full-boot-crash.md` (Bugs #1–#5, the dispatch reentrancy and per-core syscall snapshot fixes), `docs/handoffs/57e-kernel-preempt-audit.md`, `docs/handoffs/57e-dispatch-reentrancy.md`.

---

## Quick-start for the next session — Bug #8

### Reproducer

```bash
M3OS_KERNEL_FEATURES="preempt-full,sched-trace" \
  M3OS_SMOKE_SERIAL_DUMP=/tmp/m3os-smoke-full.log \
  cargo xtask smoke-test
```

Expect the smoke-test to pass on attempt 2 or 3 about 1 time in 5; otherwise it fails or terminates with one of the two patterns below in `/tmp/m3os-smoke-full.log` (the file holds the last failed attempt's serial because `M3OS_SMOKE_SERIAL_DUMP` writes only on error returns).

### Two failure shapes (Bug #8)

1. **`BlockedOnReply` cascade.** `pid=18 fork-child state=BlockedOnWait` (smoke-runner waitpid'ing the tcc forkchild) plus `pid=21 fork-child state=BlockedOnReply` (the tcc subprocess waiting for an IPC reply that never arrives). Stuck-since values run for 100+ s. This is the same shape as the original Sessions 1–3 lost-wakeup family — Sessions 3–4 attributed it to the slab UAF zombieing a non-BSP core, but with Bug #7 closed the cascade still reproduces, so a separate Bug #6 variant survives the F2-partial helper coverage. See *Session 8 — root cause identified and fixed* below for the validation table.
2. **`prompt-ready` slow boot.** `syslogd: prompt-ready gate timed out` and `sshd: prompt-ready gate timed out` after `display_server: compose#1500` or so. The boot is healthy (display_server keeps composing) but userspace services don't reach their readiness gate within the smoke-test timeout. May be the same wakeup issue surfacing earlier in boot, or a separate scheduling-cadence effect.

### Files to pre-load for Bug #8

- `kernel/src/ipc/endpoint.rs:340..850` — multi-step send/recv paths NOT covered by the F2-partial `deliver_message_and_wake` helper (which only covers 11 simple sites in `endpoint.rs` and `cleanup.rs`).
- `kernel/src/task/scheduler.rs:1271-1283` — `save_user_return_state`, the function that read PROCESS_TABLE during yield_now and caused Bug #7. Now that we know yield_now under preempt-full is the main offender, audit every other site that mutates process state for a similar "PROCESS_TABLE updated then synchronous yield reads stale snapshot" race.
- `kernel/src/task/scheduler.rs:1041-1060` — `enqueue_to_core` (the IPI send site that Sessions 2–3 explored as the IPI livelock). The bare-coalesce experiment was reverted; a smarter coalesce or wake-source rate limit may be the right move.
- `kernel/src/arch/x86_64/syscall/mod.rs:4263-4475` — `sys_execve` (with the Bug #7 fix at line ~4413). The same race shape may still exist in fork's CoW-clone path.
- `/tmp/m3os-smoke-full.log` — capture from the most recent failing attempt of the reproducer above.

### Diagnostic infrastructure already in tree

The next session inherits a strong toolchain. Don't rebuild any of these:

| Diagnostic | What it does | Source |
|---|---|---|
| `[mm] addr-space layout` boot log | One-shot at `mm::init` showing `KERNEL_PML4_PHYS`, bootloader phys-offset (with PML4 idx), heap range (with PML4 idx), and a collide flag. | `kernel/src/mm/mod.rs` (commit `8567dbc`) |
| Page-fault PT-walk diagnostic (`[pf-diag]`) | Walks the four-level page table for the fault address from BOTH active CR3 PML4 and `KERNEL_PML4_PHYS`, printing each level's flags + addr. Wired into both ring-0 and ring-3 not-present fault paths. | `kernel/src/arch/x86_64/interrupts.rs` (commit `78417ee`) |
| Per-frame allocate/free trace ring (`[frame-trace]`) | Global 16 384-entry ring keyed by physical frame address, recording every alloc/free with `#[track_caller]` location. Dumped on kernel page fault, filtered to the active CR3 frame's lifecycle. | `kernel/src/mm/frame_trace.rs`, wired in `frame_allocator.rs` (commit `b23a24c`) |
| `[free_pt] !!!` defensive sanity check | WARN log if `free_process_page_table` is called with `cr3_phys == active CR3` (the Bug #7 race signature). Silent under correct operation. | `kernel/src/mm/mod.rs` (commits `b23a24c`, `22cd711`) |
| `M3OS_SMOKE_SERIAL_DUMP=<path>` env var | `xtask smoke-test` writes full serial history to the path on every error return; trim threshold bumped from 192 KiB to 24 MiB so the deferred trace-ring dump fits. | `xtask/src/main.rs` (commit `d5da120`) |

### Approach hints

If you start with the cascade (Bug #8.1), the cleanest first step is to add `track_caller` chains through the 11 simple `deliver_message_and_wake` sites and grep for any other `deliver_message + wake_task_v2` pair the helper missed. The kernel scheduler trace ring (already enabled when `M3OS_KERNEL_FEATURES` includes `sched-trace`) records `WakeTask`, `RunQueueEnqueue`, `Dispatch` per core; on a stuck-no-waker watchdog hit the deferred trace dump fires automatically.

If you start with the slow-boot pattern (Bug #8.2), it might just be the same lost-wakeup but on a different IPC pair (e.g. the syslogd readiness handshake to init), so the same fix may close both.

### Acceptance criteria for closing Bug #8

1. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` passes on attempt 1 (no retries) for **10 runs in a row**.
2. Zero `(no waker registered)` watchdog warnings across those 10 runs.
3. `cargo xtask check` stays green.

When (1)–(3) pass, reopen Track G's 24 h soak gate (still gated on the `run-gui` doom + TAB criteria from the Sessions 1 reproducers, which were never re-verified after the Bug #7 fix).

This handoff describes three reproducible userspace correctness failures that the previous handoff classified as "Bug #6 family — performance / quality, NOT correctness." The failure logs in `m3os-bad-term.log`, `m3os-freeze-term.log`, and `m3os.log` show that those warnings are not benign — they coincide with hangs that block forward progress. The classification needs to be revised and the underlying race fixed before Track G's 24 h soak gate can open.

---

## Resolution summary (as of `95d231a`)

Two distinct bugs were tangled in the original three reproducers:

1. **Bug #6 family — `preempt_enable` zero-crossing synchronous yield (closed).** Three preempt-full-only call sites where `SchedulerGuard::drop` zero-crossed `preempt_count` with `reschedule == true` and called `yield_now` synchronously, monopolising a core or losing an IPC wake. All three closed in this session — see *Session 3 — resolution* and *Session 3 follow-on fixes*.
   - `init_task`'s `loop { task::yield_now() }` — replaced with `enable_and_hlt` (commit `695f800`).
   - BSP `idle_task` and AP `ap_idle_task`'s post-hlt `yield_now` — `cfg(not(preempt-full))`-gated (commit `38d35ea`).
   - `ipc::endpoint::reply()` deliver+wake gap and 11 other simple `deliver_message + wake_task_v2` pairs — bracketed via `preempt_disable` directly or via the new `scheduler::deliver_message_and_wake` helper (commits `d83ecc7`, `3e3107c`).

2. **Slab/MM page fault — open.** Surfaces under `preempt-full` only, manifests as a kernel page fault inside `kernel_core::slab::SlabCache::allocate` while a non-BSP core is in early userspace setup (right after a process's first `execve`). `hlt_loop`'s only the faulting core, leaving the system in a partial-deadlock zombie state where surviving cores eventually cascade into `BlockedOnReply` because they depend on services running on the dead core. See *Session 4 — slab UAF identified as the residual* for the diagnosis.

`cargo xtask run` under `preempt-full` reaches `SMOKE:PASS` reliably. `cargo xtask smoke-test` (heavier disk + retry-driven) is intermittent — sometimes passes on attempt 1 or 2, sometimes hits the slab fault on every attempt. The retry mechanism recovers when the fault doesn't fire deterministically.

**Track G's 24 h soak gate stays closed** until the slab fault is identified and fixed.

## Original TL;DR (from session 1, kept for context)

- After Bug #1–#5 fixes, the `preempt-full` kernel boots, executes services, and runs userspace through several fork generations. **It does not stay healthy.**
- Three distinct userspace failures reproduce, each consistent with a **lost wakeup** for an IPC reply or notification — `wake_task_v2` either never ran, or ran without re-queueing the target.
- The kernel emits **`[WARN] [sched] task pid=N … state=BlockedOnReply stuck-since=Wms (no waker registered)`** — this fires on the doom hang and is the cleanest signature.
- The "benign" `[WARN] [sched] dequeue-drop core=N … reason=state-not-ready extra=0x2` warnings are correlated with the hangs, not benign.
- Default build is unaffected on all three reproducers.

(After all the work documented below, points 1, 2 are addressed: the kernel does stay healthy across the smoke-test core path. The "lost wakeup" framing was *partially* correct — `reply()`'s gap was real, but the `init_task` busy-yield livelock is the dominant variant. Point 4's "dequeue-drop is correlated with hangs" was wrong; with the busy-yield closed, those warnings are again benign log noise.)

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

---

## Session 3 — resolution

### What the sched-trace capture (run8) revealed

Captured under `M3OS_KERNEL_FEATURES="preempt-full,sched-trace"` at `2b1c3f5`. The watchdog trips at the headless smoke step `SMOKE:tcc-compile:BEGIN` exactly as in earlier runs. The new sched-trace ring localised the stuck core's hot path:

- **Per timer tick on the stuck core (10 ms cadence):** five sched-trace entries — one kernel-mode `emit_preempt_trace` at `interrupts.rs:1339` plus two complete `retarget_preempt_count_to_dummy` / `retarget_preempt_count_to_task` pairs. Two full dispatch handoffs per timer tick.
- **`pid` field of the SwitchIn pivot at `scheduler.rs:1418`:** the same `task_idx` every cycle, which corresponds to the kernel's `init_task` (the bootstrap task spawned at `kernel/src/main.rs:249`).
- **`pid` field of the kernel-mode preempt event:** the truncated saved RIP, constant at `0x7A4E62`. `addr2line` resolves this to `init_task+0x410`. Disassembly of that offset shows the inlined body of `preempt_enable`'s zero-crossing branch (`scheduler.rs:1693-1714`) — `lock decl (preempt_count)`, conditional read of `pc.reschedule`, conditional `call yield_now`.

### Why H7 and the original H7' were both incomplete

- **H7 (lost wakeup):** ruled out by the trace. There was no missing `WakeTask` event — the receiver was simply never selected, because the core was dispatching `init_task` instead.
- **H7' (IPI livelock):** directionally right — cross-core wakes do keep setting `reschedule = true` on the stuck core, but the *effect* lives at the `preempt_enable` zero-cross inside `init_task`'s busy-yield, not at the IPI handler. The bare-coalesce IPI experiment (run7) suppressed the trigger but introduced new wake delays because of the AP 10 ms timer.

### Mechanism (final)

1. `task::spawn(init_task, "init")` placed on whichever core `least_loaded_core` picks at boot, or migrated there later by load balance. Often an AP, not the BSP.
2. `init_task` finishes service setup and falls into `loop { task::yield_now(); }` (`kernel/src/main.rs:325-327`).
3. Each iteration: `yield_now → scheduler_lock → SchedulerGuard::drop → preempt_enable`.
4. Cross-core IPC traffic (vfs / fat / session servers) keeps setting `reschedule = true` on `init_task`'s core via `enqueue_to_core` (`scheduler.rs:1041`).
5. `preempt_enable`'s zero-crossing observes `reschedule == true` with IF=1 and synchronously calls `yield_now` again (`scheduler.rs:1707`).
6. Scheduler picks `init_task` as the only ready task on this core (real userspace tasks on the same core are now `BlockedOnReply` waiting for the wakers that `init_task` is starving).
7. Loop forever. The core makes zero userspace progress for 30+ seconds; the watchdog catches it as `(no waker registered)` on whichever userspace task happened to land on the same core.

The `cpu-hog: pid=20 ion ran~30035 ms` warning was a red herring — `ran_ticks = now - start_tick`, and `start_tick` was set when ion was first dispatched 30 s earlier. By the time the warning fired, ion had long since been preempted off the core; `init_task` had taken over.

### Fix that landed (`695f800`)

One-line change in `kernel/src/main.rs:325-336`:

```rust
log::info!("[init] service set started — yielding");
loop {
    x86_64::instructions::interrupts::enable_and_hlt();
}
```

`init_task` has no work after service setup, so halting it removes it as a wake target on whichever core it ends up. This does not address the underlying tendency for `preempt_enable` zero-crossing to fire synchronous yields when `reschedule` is set — that is still latent for any future kernel task with the same shape — but `init_task` was the only such task in the kernel today.

### Verification

- **Headless** `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run` (run9, post-fix):
  - `SMOKE:auth:PASS`, `SMOKE:tcc-version:PASS`, `SMOKE:tcc-compile:PASS`, `SMOKE:hello:PASS`, `SMOKE:storage:PASS`, `SMOKE:net:PASS`, `SMOKE:log:PASS`, `SMOKE:PASS`.
  - `display_server` compose loop reaches `compose#11160` at the 240 s timeout, no heartbeat stalls.
  - **0** `cpu-hog` warnings; **0** `(no waker registered)` watchdog hits; **0** stale-ready warnings ≥ 1 s.
  - 12 residual `dequeue-drop … extra=0x2 (BlockedOnRecv)` warnings — back to genuinely benign now that no core is monopolised.
- **Acceptance criteria 1, 2, 4** (GUI doom + GUI TAB + 10 min soak under `run-gui`) — **not yet verified**. Suggested next step before reopening Track G's 24 h soak gate.

### Residual concerns

1. **Why `init_task` ends up on an AP at all.** `task::spawn` selects via `least_loaded_core`. At boot, all APs are empty so the choice is arbitrary. Pinning `init_task` to BSP would have masked this bug too, but the halt-yield fix is more robust because it survives later load-balance migration.
2. **Other busy-yield kernel tasks.** `grep -n "loop { *task::yield_now()" kernel/src/` should be empty after this fix; if a future task is added with that shape, the same livelock is possible.
3. **The `preempt_enable` synchronous-yield path is still the most aggressive option under `preempt-full`.** Consider whether F3 from the original *Proposed fixes* — suppress synchronous yield inside IPC reply paths — should land as defence in depth, even though it is no longer required to close this bug.

### Follow-on fixes that landed in this session

After the `init_task` halt fix passed `cargo xtask run` but **`cargo xtask smoke-test`** (heavier disk, retry-driven) still hung at `tcc-compile` with `pid=2 syslogd state=BlockedOnReply stuck-since=134973ms`, two more fixes were needed:

#### `38d35ea` — cfg-gate idle yield_now to preempt-voluntary

`idle_task` (BSP, `kernel/src/main.rs`) and `ap_idle_task` (APs, `kernel/src/smp/boot.rs`) both followed every `enable_and_hlt` with a `task::yield_now()`. The same Bug #6 mechanism applied: under `preempt-full`, the post-hlt yield's `SchedulerGuard::drop` zero-crossed `preempt_count` with `reschedule` set, synchronously calling `yield_now` again. Scheduler picked idle as the only ready task on the core, redispatched it, livelock.

The yield is required under `preempt-voluntary` (where the IPI handler only sets `reschedule` without dispatching) so it was cfg-gated, not removed.

#### `d83ecc7` — bracket `reply()` with preempt_disable / preempt_enable (F1)

`ipc::endpoint::reply` is `deliver_message(caller, msg)` followed by `wake_task_v2(caller)`. `deliver_message`'s `SchedulerGuard::drop` zero-crosses between (A) and (B). Under `preempt-full` the synchronous yield branch fires, server yields before `wake_task_v2` runs. If wake is then delayed (server starved or its CAS bails), the caller is parked in `BlockedOnReply` with `pending_msg` set but no run-queue entry — the original H7 lost-wakeup signature.

F1 fix: bracket the deliver+wake pair with `preempt_disable` / `preempt_enable` so `deliver_message`'s drop sees `preempt_count > 0`, no zero-cross, no synchronous yield. The pair is now atomic from the caller's state machine's perspective.

### Status of the original acceptance criteria (snapshot at end of Session 3)

| Criterion | Status |
| --- | --- |
| (1) `run-gui` + login + `fb-takeover doom` renders | **not verified** — needs interactive run-gui |
| (2) `run-gui` + login + TAB completion → ion completes | **not verified** — needs interactive run-gui |
| (3) `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` passes within standard timeout | ⚠️ passed *once* on attempt 3 of 3 in 67 s; subsequent runs were intermittent — see *Session 4* for the actual root cause |
| (4) `run-gui` 10-minute soak with zero `(no waker registered)` warnings | **not verified** — needs interactive run-gui |

### Initial hypothesis for the smoke-test intermittency: another deliver+wake pair

> **Note:** This subsection captures the working theory at the end of Session 3. Session 4 (below) replaces it: the intermittent hang is a slab/MM page fault, not another `deliver_message + wake_task_v2` race. The F2 partial described next still landed and is correct on its own merits, but it does **not** close the residual.

`cargo xtask smoke-test` retry attempts 1 and 2 hang at `display_server: starting (Phase 56 — C.1+C.2)` with no further serial output until the trace-ring dump fires. Attempt 3 boots cleanly and runs to completion. The pattern is **timing-sensitive**: same kernel, same disk, different outcome.

Likely cause (turned out to be wrong): another `deliver_message + wake_task_v2` pair (F2 in the original *Proposed fixes*) hits the same race that F1 just closed for `reply()`. Candidate sites from `grep -rn deliver_message kernel/src/ipc/`:

- `kernel/src/ipc/endpoint.rs:340, 360, 380, 461, 479, 492, 568, 582, 596, 740, 850, 857` (sender-side delivery in send/recv paths)
- `kernel/src/ipc/cleanup.rs:94, 99, 105` (error-path delivery on task death)

Each pair should either be wrapped with `preempt_disable` / `preempt_enable` (per-site) or refactored to call a `deliver_and_wake_atomic` helper. The right next step (we thought) was to introduce that helper, since one place to bracket means no risk of forgetting a pair on a future addition. F2 partial below landed exactly that.

#### `3e3107c` — F2 partial — `deliver_message_and_wake` helper + 11 simple sites refactored

A `pub fn deliver_message_and_wake(id, msg) -> WakeOutcome` helper was added to `kernel/src/task/scheduler.rs` next to `deliver_message`. It brackets `deliver_message` + `wake_task_v2` with `preempt_disable` / `preempt_enable`. 11 simple call sites were refactored to use it:

- `kernel/src/ipc/cleanup.rs`: 3 sites (stranded senders, stranded receivers, reply waiters).
- `kernel/src/ipc/endpoint.rs`: 8 EINTR-style error paths (cap-table-full, transfer-cap-failed, endpoint-closed).

#### Smoke-test under preempt-full is genuinely intermittent

After F1 + idle + F2-partial, `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` is *non-deterministic*. The same kernel binary fails or passes across different invocations:

| Run | F2 partial? | Outcome |
| --- | --- | --- |
| postfix3 | no | ✅ passed on attempt 3/3 in 67 s |
| postfix4 | yes | ❌ failed all three attempts (pid=18 BlockedOnWait) |
| baseline (F2 stashed) | no | ❌ failed all three attempts |
| stf2 (sched-trace + F2) | yes | ❌ failed all three attempts (pid=18 BlockedOnWait, pid=21 BlockedOnReply) |

So **F2 did not regress anything** — the same intermittency reproduces with F2 stashed. The remaining failure mode is `pid=21` (a tcc-compile subprocess) `BlockedOnReply` for 100+ seconds while `display_server` and `term` continue making progress, suggesting:

- The wake target's wake_task_v2 either never ran, ran with `AlreadyAwake`, or ran but the dispatch is being suppressed somehow.
- The scheduler is healthy enough that other tasks make progress; this is NOT a livelock anymore.
- This is the H7 lost-wakeup pattern surviving F1 — F1 only covers the `reply()` path; other servers may use a different path that still has the gap.

#### Why smoke-test diagnosis is hard

`cargo xtask smoke-test` discards the QEMU serial pipe between retry attempts. Only the last ~100 lines of "serial output" are surfaced in the failure report, which is too short to capture the deferred trace-ring dump. The dump fires once per boot but is overwritten or truncated before the smoke-test wrapper reports it.

To make further progress, one of the following is needed:

1. **A smoke-test variant that captures full serial output** to a file across all attempts (e.g., a flag to write `/tmp/m3os-smoke-attempt-N.log` per attempt before the QEMU process is killed). ✅ landed in Session 4 as the `M3OS_SMOKE_SERIAL_DUMP` env-var (commit `d5da120`).
2. **A more aggressive trigger for the trace dump** that fires sooner (e.g., on the first stuck-since > 5 s rather than > 30 s) so the dump lands before the wrapper times out the attempt.
3. **A smaller in-kernel reproducer** that triggers the same stuck-on-reply pattern in `cargo xtask test` (with `--display` for a visible QEMU window) so we can attach lldb / step through.

#### Status of acceptance criteria as of `3e3107c`

| Criterion | Status |
| --- | --- |
| (1) `run-gui` + `fb-takeover doom` renders | not verified |
| (2) `run-gui` + TAB completion | not verified |
| (3) `cargo xtask smoke-test` passes deterministically | ❌ — intermittent |
| (4) `run-gui` 10-min soak | not verified |

`cargo xtask run` (no smoke wrapper, no retry) under preempt-full **does** reach `SMOKE:PASS` reliably across the runs we tested. The bug is severe enough to surface under the heavier disk + tighter timing of `cargo xtask smoke-test`, mild enough to not surface under `cargo xtask run`.

Track G's 24 h soak gate **must** stay closed until smoke-test is deterministic.

---

## Session 4 — slab UAF identified as the residual

After F1 + idle + F2-partial, smoke-test was *intermittent* — sometimes attempt 1 passes, sometimes attempt 2, sometimes all three fail. Diagnosing this required a way to capture full kernel serial output past the wrapper's 80-line tail; the kernel's deferred trace-ring dump (which fires on stuck-no-waker watchdog hits) was always being truncated.

### `M3OS_SMOKE_SERIAL_DUMP` diagnostic flag

`xtask` was extended with `M3OS_SMOKE_SERIAL_DUMP=<path>` (commit landed alongside this section). When set, `run_smoke_script` writes the full `serial_history` to that path on every error return, and the per-iteration trim threshold is bumped from 256 KiB / 192 KiB to 32 MiB / 24 MiB so the trace-ring dump (≈ 1 MiB per fire under 4096-entry rings × 4 cores) does not evict the boot log it followed.

Usage:

```bash
M3OS_KERNEL_FEATURES="preempt-full,sched-trace" \
  M3OS_SMOKE_SERIAL_DUMP=/tmp/m3os-smoke-full.log \
  cargo xtask smoke-test
```

### What the full capture revealed

A `cargo xtask smoke-test` run that **passed on attempt 2** but failed on attempt 1 had this in `/tmp/m3os-smoke-full.log` (attempt 1's serial):

```
init: started 'syslogd' pid=2
[INFO] [proc] execve: pid=2 path=/bin/syslogd
syslogd: starting
[int] kernel page fault: addr=Ok(VirtAddr(0xffff8000005119f8)) err=PageFaultErrorCode(0x0)
InterruptStackFrame { instruction_pointer: VirtAddr(0x100008798b3), code_segment: SegmentSelector { index: 1, rpl: Ring0 }, … }
[int] KERNEL page fault — CR3=0x000000003f261000
=== CRASH DIAGNOSTICS ===
…
--- Current Task ---
  task_idx=9 on core 1
  TaskId=10 state=Running saved_rsp=…
  pid=2 assigned_core=1 priority=20
…
```

`addr2line` resolves RIP `0x8798b3` to **`<kernel_core::slab::SlabCache>::allocate`**. The faulting access is at offset 0x80 from `R13=0xffff800000511978`, which points into the bootstrap heap at `0xffff800000000000`. `err=0x0` is "page not present, read access from kernel mode" — so the slab cache is dereferencing a pointer to a frame that is unmapped.

The kernel page-fault handler at `kernel/src/arch/x86_64/interrupts.rs:1067-1079` calls `dump_crash_context`, dumps the trace ring, then `crate::hlt_loop()`. **`hlt_loop` halts only the faulting core**; other cores keep running. So one core dies, three continue, and any task whose wake depends on a service running on the dead core stalls in `BlockedOnReply` — explaining the cascade we saw in earlier session logs.

### Implications

1. **The Bug #6 fixes that landed are correct and necessary.** They close three real preempt-full-specific livelock / lost-wake variants. With these fixes, *if* the slab fault doesn't fire, smoke-test passes within standard timing.
2. **The slab fault is a separate bug** — almost certainly a use-after-free or double-free, surfaced by `preempt-full`'s tighter scheduling cadence and reproduced on-and-off across all of our smoke-test runs. It is most likely **not** in `kernel_core::slab` itself but in a caller that frees a slab object too eagerly.
3. **The single-core hlt_loop on kernel page fault is dangerous in itself.** A faulting core should at minimum kill the running task and reclaim its slot, or drop to a recovery scheduler that quiesces other cores; otherwise the system becomes a partial-deadlock zombie. That is a separate Phase 57 hardening item.

### Where to investigate the slab UAF (Session 5)

The disassembly at `0x8798b3` confirms the faulting instruction is an inlined `Vec::partition_point` binary search:

```
8798a9: add %rsi,%r15           ; binary-search advance
8798ac: mov %r15,%r8
8798af: shl $0x7,%r8            ; r8 = r15 * 128 (= sizeof SpanMeta)
8798b3: cmp %rdx,(%r13,%r8)     ; compare [r13 + r8], rdx ← FAULT
```

`R13 = 0xffff800000511978` is the heap base of `self.spans` (`Vec<SpanMeta>`). At the fault, `R15 == 1` and `R8 == 128`, meaning `self.spans.len() == 2` and the search probed `&spans[1]` — a 128-byte structure whose memory is in the SAME 4 KiB page as `&spans[0]` (`0x511978` and `0x5119f8` are both in page `0x511000`). The page-not-present error means **page `0x511000` of the bootstrap heap is not mapped**, even though `R13` (which is supposed to be a valid heap pointer) lives there.

That points away from a slab-internal bug and toward one of:

1. **Heap allocator returning an address whose page is not yet mapped.** Bootstrap heap should be eagerly mapped at `0xffff800000000000` for 8 MiB = ends at `0xffff800000800000`. Fault is at `0x5119f8`, well within that range, so the heap *should* cover it. Check the bootstrap heap mapping path and whether all 8 MiB is actually mapped, or only enough for what was used at boot — and whether growth past initial usage requires explicit mapping.
2. **Vec growing past the mapped portion of the bootstrap heap.** The slab cache's `self.spans` grew from len=1 to len=2 (when `create_span` ran during `allocate`), forcing `Vec` to reallocate to a larger backing buffer. If that reallocation lands on an address whose page isn't mapped, the next access faults.
3. **TLB / page-table sync bug.** Less likely on a fresh address — TLB miss should walk page tables, find an entry, and map it. Page-not-present means the entry isn't there at all.

Cargo.toml's `kernel-core` exports `SlabCache::allocate`. The kernel-side caller chain is in `kernel/src/mm/heap.rs` or similar; that wrapper provides the `page_alloc` callback. Trace the page-allocation callback to see whether it ever returns an address that the kernel hasn't mapped.

#### What is NOT this bug

- The 8 MiB bootstrap heap is the small heap used during early init. The runtime kernel heap is different and uses `BuddyAllocator` or similar. Verify which heap the slab cache is using at the time of the fault.
- The fault is **not** under `preempt-voluntary` (doc TL;DR confirms). So either this only triggers under preempt-full's specific allocation timing, or preempt-voluntary serializes things enough to hide it. Both cases imply a race or ordering issue.

### Acceptance criteria for closing this issue

1. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` reaches `SMOKE:PASS` deterministically on attempt 1, ten runs in a row.
2. The slab UAF root cause is identified and fixed (or reverted-by-causing-commit if a regression).
3. Kernel page fault handler is hardened to either kill the faulting task and continue (single-task fault) or quiesce all cores and panic (kernel state corruption fault), instead of `hlt_loop`'ing only the faulting core.

---

## Session 5 — required reading and reproducer

### Reproducer

```bash
M3OS_KERNEL_FEATURES="preempt-full,sched-trace" \
  M3OS_SMOKE_SERIAL_DUMP=/tmp/m3os-smoke-full.log \
  cargo xtask smoke-test
```

When the slab fault fires (intermittent — sometimes attempt 1, sometimes attempt 2 or 3), `/tmp/m3os-smoke-full.log` contains the full kernel serial including the deferred trace-ring dump. Look for `[int] kernel page fault: addr=Ok(VirtAddr(0xffff8000…))`. If smoke-test passes deterministically, the fault did not fire on this run — try again or increase the iteration count to find a failing seed.

### Files to pre-load

- `kernel-core/src/slab.rs:269-278` — `SlabCache::allocate` (the function the page-fault RIP resolves to).
- `kernel-core/src/slab.rs:366-397` — `create_span` (where the inlined `partition_point` lives — the actual faulting binary search).
- `kernel/src/mm/heap.rs:728-768` — `init_heap` (the eager mapper of the 8 MiB bootstrap heap).
- `kernel/src/mm/heap.rs:815-886` — `grow_heap` (uses `get_mapper()` which uses CR3 — see below).
- `kernel/src/mm/paging.rs:33-60` — `active_level_4_table` and `get_mapper` (CR3-derived mapper; the suspicious bit).
- `kernel/src/mm/mod.rs:258-359` — `new_process_page_table` (kernel-half PML4 sharing for new processes).
- `kernel/src/mm/mod.rs:183-238` — `mm::init` (capture order: KERNEL_PML4_PHYS, then heap, then buddy, then refcounts, then slab).
- `kernel/src/main.rs:41-45` — `BootloaderConfig` with `Mapping::Dynamic` for `physical_memory` (a candidate for the address-space conflict hypothesis below).
- `kernel/src/arch/x86_64/interrupts.rs:1067-1080` — kernel page-fault handler that `hlt_loop`'s only the faulting core.

### Hypotheses to investigate

In rough order of cheapness to confirm:

1. **`get_mapper()` uses CR3, not KERNEL_PML4_PHYS.** `init_heap`'s mapper IS for the kernel PML4 (because `paging::init` is called at boot before any process exists). But `grow_heap` calls `get_mapper()` which calls `active_level_4_table()` which reads CR3. If `grow_heap` runs while a process CR3 is loaded, it adds the new heap mapping to the *process's* page-table walk — and other processes that don't share that PDPT entry would see the page as not-present. **Verify:** when does grow_heap fire (boot? per-allocation?) and what CR3 is loaded at that time?
2. **Address-space conflict.** `HEAP_START = 0xffff_8000_0000_0000` is fixed; `physical_memory_offset` is `Mapping::Dynamic` from the bootloader. If the bootloader picks the same base address for the phys-mapping range, the heap and phys-mapping conflict in PML4[256]. **Verify:** log `physical_memory_offset` at boot and confirm it does *not* overlap `[HEAP_START, HEAP_START + HEAP_MAX_SIZE)`.
3. **PML4[256] not actually shared after `new_process_page_table`.** The shallow copy at `kernel/src/mm/mod.rs:292-294` copies PML4 entries 1..512. If PML4[256] points to a PDPT that itself was DEEP-copied somewhere, mappings added after process creation wouldn't propagate. **Verify:** is PML4[256]'s PDPT ever deep-copied? Search for any `.clone()` or new-frame-allocate within the kernel half.
4. **Frame-allocator returns a frame that's still mapped elsewhere.** Less likely but worth ruling out — verify `frame_allocator::allocate_frame` doesn't return a frame already used as a page-table or heap page.

### Quick-confirm command

`addr2line -e target/x86_64-unknown-none/release/kernel -f -C 0x8798b3` should resolve to `<kernel_core::slab::SlabCache>::allocate`. If a future kernel build moves the inline boundary, re-disassemble the function around that RIP to find the new offset of the `cmp %rdx,(%r13,%r8,1)` instruction.

---

## Session 5 — Hypotheses 2 & 3 ruled out, working theory shifts to frame UAF (Hyp #4)

### Diag patch landed (`8567dbc`)

`mm::init` now logs the kernel address-space layout at boot:

```rust
log::info!(
    "[mm] addr-space layout: KERNEL_PML4_PHYS={:#x} phys_offset={:#x} (PML4[{}]) heap={:#x}..{:#x} (PML4[{}]) collide={}",
    ...
);
```

Run output (`M3OS_KERNEL_FEATURES=preempt-full,sched-trace M3OS_SMOKE_SERIAL_DUMP=/tmp/m3os-smoke-full.log cargo xtask smoke-test`):

```
[INFO] [mm] addr-space layout: KERNEL_PML4_PHYS=0x101000 phys_offset=0x28000000000 (PML4[5]) heap=0xffff800000000000..0xffff800004000000 (PML4[256]) collide=false
```

### Hypothesis #2 — eliminated

The bootloader's `Mapping::Dynamic` phys_offset placed phys-memory at PML4[5] (`0x28000000000`). The heap is at PML4[256] (`0xffff800000000000`). They share no PML4 entry; they cannot conflict at any sub-table level.

### Hypothesis #3 — audited, looks correct

`new_process_page_table` (`kernel/src/mm/mod.rs:292-294`) shallow-copies PML4[1..512] from `KERNEL_PML4_PHYS` (not from CR3 — see comment at 271-274 explicitly noting this). PML4[256] is therefore copied as a single entry value, meaning every process inherits a pointer to the **same kernel-side PDPT physical frame** for the heap. Any subsequent modification under that PDPT (PD entries, PT entries, leaf 4 KiB pages) is visible to every process via shared sub-tables.

PML4[256] is never deep-copied anywhere — the only deep-copy in `new_process_page_table` is for PML4[0] (the user-space lower half, conditional on the kernel having a present PDPT there). No `.clone()` on heap-region tables exists anywhere we found in `kernel/src/mm/`.

So Hypothesis #3 also does not explain the fault.

### Hypothesis #1 — structurally real but does not match this fault address

`get_mapper()` (`kernel/src/mm/paging.rs:54-60`) does read CR3, and `grow_heap` does call `get_mapper()`, so a heap growth made while a process CR3 is loaded would walk that process's PML4. **However:** the fault address `0xffff8000005119f8` is at heap offset `0x5119f8` ≈ 5.07 MiB, well within `HEAP_INITIAL_SIZE = 8 MiB` (`heap.rs:42`). That range is mapped eagerly by `init_heap` at boot, *before* any process page-table exists. `grow_heap` never had to fire for this address.

Hyp #1 is still a real latent bug for the > 8 MiB heap region — worth fixing for defence-in-depth — but it does not explain this fault.

### New observation: a *different* fault on the same run

The smoke-test run that produced the layout log above also captured a **userspace** instruction-fetch fault, not a kernel slab fault:

```
init: started 'sshd' pid=3
[int] userspace page fault: pid=3 addr=Ok(VirtAddr(0x439e00)) err=PageFaultErrorCode(USER_MODE | INSTRUCTION_FETCH) rip=0x439e00 — process killed
[WARN] [sched] cpu-hog: pid=3 name=fork-child exec_path=/bin/sshd core=1 ran~1577 ms final_state=Dead
```

`0x439e00` is just past `USER_VADDR_MIN = 0x400000`, deep inside `/bin/sshd`'s text segment. The page-fault error code is `USER_MODE | INSTRUCTION_FETCH` with the present bit clear → **the page that backs sshd's text at `0x439000` is not present in pid=3's page table**, even though sshd had been running successfully for ~1.58 s.

Cascade after sshd is killed: `pid=18 BlockedOnWait` (smoke-runner waitpid'ing the test forkchild) and `pid=21 BlockedOnReply` (the tcc subprocess — likely waiting on vfs/fat which depends on the same wakers). These are downstream consequences, not the trigger.

### Why this matters

We now have **two structurally identical faults** that fire under preempt-full only:

| Fault | Where | What |
| --- | --- | --- |
| Slab fault (Session 4) | `kernel_core::slab::SlabCache::allocate` at heap offset `0x5119f8` | kernel data page not present, ring 0 read |
| Userspace IF fault (this session) | sshd at `rip=0x439e00` | userspace code page not present, ring 3 instruction fetch |

Both: "page that should be mapped is not present." Both: intermittent. Both: preempt-full only. Both: on a non-BSP core (slab fault was core 1; sshd ran on core 1 here).

The unifying explanation is **frame UAF / stale page-table linkage** — at some point a frame that was backing one page gets either:

1. Returned to the frame allocator while still referenced by a live PTE, and later reallocated (the new owner may overwrite it; the old owner now reads garbage or — if the PTE's present bit is then cleared by some other path — sees a not-present fault), OR
2. The PTE pointing at a still-live frame gets cleared by an unrelated TLB-shootdown or page-table-edit code path, leaving the address space inconsistent.

(2) is more consistent with the symptoms because both observed faults are *not-present* faults, not "wrong contents" faults — i.e. the PTE itself is gone, not that the frame contains stale data.

### Hypothesis #4 elevated — frame UAF / spurious PTE clear

Working theory for Session 6: a TLB-shootdown / page-table-update / mm cleanup code path under preempt-full is racing against a CR3-load-or-frame-free, with the result that a leaf PTE under the active CR3 gets cleared while the address is still in use.

Likely suspects to read carefully:

- `kernel/src/smp/tlb.rs` — `tlb_shootdown_range` (and any IPI handlers that do PTE writes from interrupt context).
- `kernel/src/mm/free_process_page_table` (`kernel/src/mm/mod.rs:373-…`) — walks process PML4 freeing user-only pages; could it free pages that are also mapped under a kernel-half PDPT? (Less likely, since kernel half is shallow-copied, but the *freeing logic* is a known source of UAF in similar kernels.)
- `kernel/src/mm/heap.rs:705-720` and the `bootstrap_dealloc` / size-class deactivation paths — does any deallocation accidentally `unmap` a heap page rather than just `free` the underlying buffer?
- The frame-allocator buddy / refcount paths (`kernel/src/mm/frame_allocator.rs`) — does `free_frame` ever fire on a frame that's still page-table-linked? Add an assertion at frame-free time that any PTE pointing at the frame is already cleared.

### Hardening item still pending

`hlt_loop`-on-faulting-core remains dangerous regardless of root cause. The kernel page-fault handler should at least quiesce all cores and panic on a kernel-half not-present fault, so the symptom is one observable crash instead of a partial-deadlock zombie. Filed as a sub-task for Session 6.

### Updated acceptance criteria

(unchanged from Session 4) plus:

5. The frame allocator carries a debug-build invariant: every frame returned by `allocate_frame` is *not* present in any active PTE in any active CR3 at the moment of allocation.
6. Every `free_frame` call site is audited; a frame is not freed until all PTEs referencing it have been cleared and TLB-shot-down.

### Files to pre-load (Session 6)

- `kernel/src/mm/frame_allocator.rs` — buddy allocator, refcounts, `allocate_frame` / `free_frame`.
- `kernel/src/smp/tlb.rs` — `tlb_shootdown_range`, IPI handler, page-table-edit synchronization.
- `kernel/src/mm/mod.rs:373-…` — `free_process_page_table` (the freeing walk).
- `kernel/src/arch/x86_64/interrupts.rs:1067-1100` — kernel page-fault handler (single-core hlt → all-core quiesce hardening).
- `/tmp/m3os-smoke-full.log` from a run that hits *either* fault variant — both are now data points for the same bug.

---

## Session 6 — PML4-level corruption confirmed; frame allocator UAF is the root cause

### PT-walk diagnostic landed (`78417ee`)

`dump_pte_walk_diagnostics(vaddr)` is now wired into both the ring-0 kernel page-fault path and the ring-3 not-present page-fault path. It walks the four-level page table from BOTH the active CR3's PML4 and `KERNEL_PML4_PHYS`, printing each level's flags + addr. Builds clean (`cargo xtask check` passes).

### First fault captured under instrumentation (smoke-test iteration 1, attempt 1)

```
[INFO] [proc] fork: parent_pid=17 parent_exec=/bin/term child_pid=20
[INFO] [proc] execve: pid=20 path=/bin/ion
[int] kernel page fault: addr=Ok(VirtAddr(0xffff80000051ca18)) err=PageFaultErrorCode(CAUSED_BY_WRITE)
InterruptStackFrame { instruction_pointer: VirtAddr(0x1000087add9), ... }
[int] KERNEL page fault — CR3=0x00000000017eb000
[pf-diag] vaddr=0xffff80000051ca18 idx=[p4=256 p3=0 p2=2 p1=284] active_cr3=0x17eb000 kernel_pml4=0x101000
[pf-diag] active: PML4[256] flags=PageTableFlags(BIT_11 | BIT_52 | BIT_53 | BIT_54 | BIT_55 | BIT_56 | BIT_57 | BIT_59 | BIT_60) addr=0x64ec9f4294000
[pf-diag] kernel: PML4[256] flags=PageTableFlags(PRESENT | WRITABLE | ACCESSED) addr=0x3feda000
[pf-diag] kernel: PDPT[0] flags=PageTableFlags(PRESENT | WRITABLE | ACCESSED) addr=0x3fed9000
[pf-diag] kernel: PD  [2] flags=PageTableFlags(PRESENT | WRITABLE | ACCESSED) addr=0x3fad6000
[pf-diag] kernel: PT  [284] flags=PageTableFlags(PRESENT | WRITABLE | ACCESSED | DIRTY) addr=0x3f9ba000
```

**Reading the divergence:**

- `active_cr3 = 0x17eb000` is pid=20 ion's freshly-created PML4 (just had its first `execve`).
- `active PML4[256]` is **corrupted** — `PRESENT` bit is **clear**, the address `0x64ec9f4294000` is impossible (≈ 6.9 PB), and the high bits are random-looking OS-available bits (BIT_52..57, 59, 60) plus BIT_11 set in the low.
- `kernel PML4[256]` is correct (`PRESENT | WRITABLE | ACCESSED`, addr `0x3feda000`).
- The kernel was about to write to its own heap at `0xffff80000051ca18`. The walk needed PML4[256] → PDPT[0] → PD[2] → PT[284]. From the kernel PML4 the walk works fine. From the active PML4 it dies at level 4 because the entry is non-PRESENT.

### Why this rules out Hyp #1 / #2 / #3 conclusively

`new_process_page_table` (`mm/mod.rs:280-294`) explicitly sets `new_pml4[256]` from the kernel's PML4:

```rust
for i in 1usize..512 {
    new_pml4[i] = cur_pml4[i].clone();
}
```

The PML4 frame is also zeroed first (line 268). So immediately after `new_process_page_table` returns, the new PML4's `[256]` entry **must** equal the kernel's, byte for byte. The corruption we observe happens *after* `new_process_page_table` returns and *before* the kernel's first heap mutation in execve.

Phys-offset is at PML4[5] (Session 5 layout log), so it cannot collide. The kernel PML4 is intact (the comparison walk on the same fault confirms it). PT/PD-level corruption can be ruled out for this fault — the PML4 entry itself is wrong, so no sub-table walk happens.

### Working diagnosis — frame allocator UAF (Hyp #4 confirmed)

The corruption pattern is consistent with **another kernel data structure being written into frame `0x17eb000`** at byte offset `0x800` (= entry index 256 × 8 bytes). That is, the same physical frame that became pid=20's PML4 is *also* being used as the backing memory for some other kernel object (a Vec, slab span, kernel stack, DMA buffer, IPC message page, etc.).

Two ways this happens:

1. **Double-allocate.** `allocate_frame` returned the same physical frame to two callers without an intervening `free_frame`. The frame allocator's free-list / per-CPU-cache / refcount logic has a race window where the same frame is enqueued twice (or never properly dequeued).
2. **Free-while-mapped.** The frame was freed by some path while a stale reference (Vec, slab span pointer, kernel stack pointer) still held it. `new_process_page_table`'s `allocate_frame` returns the freed frame, the freshly-zeroed PML4 is constructed, and the original holder later overwrites it.

Both shapes match the BIT_11 + BIT_52..60 garbage pattern (it looks like 8 bytes of arbitrary kernel data, not a recognizable structure).

### Why this is `preempt-full`-only

Under `preempt-voluntary`, kernel code paths are not preempted between consecutive `allocate_frame` calls or between `free_frame` and a subsequent re-use. The race window is closed by single-threaded execution within each call site. Under `preempt-full`, kernel code can be preempted *anywhere*, and the per-CPU page cache can hand the same frame to two different allocation contexts that interleave.

The pattern matches the prior Session 4 slab fault and Session 5 sshd userspace fault — all three are downstream consequences of the same UAF; the visible fault address depends on which page tables happen to be torn.

### Next step (Session 7)

Add a debug-only allocate/free trace to `frame_allocator::allocate_frame` and `frame_allocator::free_frame`. Each call records `(timestamp, core_id, frame_phys, op, caller_rip)` into a small per-core ring (e.g. 1024 entries). On a kernel page fault, dump the rings filtered to the offending physical frame (the active CR3 phys for PML4 corruption, the leaf PT-frame for sub-table corruption).

Specifically wanted output:

```
[frame-trace] 0x17eb000 last operations:
  [tick=N] core=K op=ALLOC  rip=<caller>  ← who got it
  [tick=N-x] core=K op=FREE   rip=<caller>  ← who freed it (UAF means alloc-after-this is the bug)
  ...
```

If we see two ALLOCs without an intervening FREE, that's the double-allocate bug. If we see a FREE followed by an ALLOC and the FREE-er didn't actually relinquish the pointer, that's the free-while-mapped bug.

Pre-load files:

- `kernel/src/mm/frame_allocator.rs:557-628` — `allocate_frame` per-CPU cache + buddy fallback.
- `kernel/src/mm/frame_allocator.rs:722-762` — `free_frame` refcount-then-cache logic.
- `kernel/src/mm/frame_allocator.rs:225-290` — `release_last_reference`, refcount inc/dec.
- `kernel/src/mm/frame_allocator.rs:340-450` — buddy allocator internals (`free_to_pool`, `allocate`).

### Hardening still pending

The kernel page-fault handler still ends with `crate::hlt_loop()` which halts only the faulting core. Other cores keep running, hit dependencies on the dead core, and cascade into `BlockedOnReply`. Replacing `hlt_loop()` with `panic!` would be slightly better but the existing `panic_handler` also calls `hlt_loop` — true hardening requires sending an NMI/IPI to all cores and stopping cleanly. Filed as a separate hardening sub-task.

### Updated acceptance criteria (Session 6 revision)

(unchanged from Session 5) plus:

7. The frame allocator carries a per-frame allocate/free trace ring, and a kernel page-fault dump prints the recent history of the offending frame.
8. Root-cause UAF/double-allocate site is identified, fixed, and validated by 10 deterministic smoke-test passes under `preempt-full`.

---

## Session 7 — frame-trace ring landed; lifecycle traced; root caller still TBD

### What landed (`b23a24c`)

1. `kernel/src/mm/frame_trace.rs` — global 16 384-entry ring keyed by physical frame address. Records every `allocate_frame` / `allocate_contiguous` / `free_frame` / `free_frame_direct` / `free_contiguous` with the immediate caller's `&'static Location` via `#[track_caller]`. Wired into the kernel page-fault handler — on a ring-0 fault the active CR3 frame's recent history is dumped.
2. `kernel/src/arch/x86_64/syscall/mod.rs` (execve) — speculative Bug #7 fix: capture `old_cr3_phys` via `Cr3::read` BEFORE `set_current_user_return` so a kernel-mode preempt between the urs publish and the read cannot make `Cr3::read` return new_cr3.
3. `kernel/src/mm/mod.rs` (free_process_page_table) — `[free_pt] cr3_phys=… caller=…` log on every entry so the next captured fault identifies which path passed the just-allocated PML4 as cr3_phys.

### What the first captured fault revealed

The frame-trace dump for the corrupted PML4 frame (`0x1673000` in Session 6, `0x1674000` in iteration 3 here — they shift each run) shows a deterministic six-event lifecycle:

```
[tick=N+0]  ALLOC  caller=kernel/src/mm/elf.rs:400        ← initial ELF user page (some pid)
[tick=N+~70] FREE  caller=kernel/src/arch/x86_64/interrupts.rs:322  ← CoW handler core 1 (refcount 2→1)
[tick=N+~70] FREE  caller=kernel/src/arch/x86_64/interrupts.rs:322  ← CoW handler core 3 (refcount 1→0)
[tick=N+~70] ALLOC caller=kernel/src/mm/mod.rs:288        ← new_process_page_table (new PML4)
[tick=N+~73] FREE  caller=kernel/src/mm/mod.rs:498        ← free_process_page_table cr3_phys (!!!)
[tick=N+~74] ALLOC caller=kernel/src/mm/slab.rs:303       ← reused as a slab span
```

The mm/mod.rs:498 free is the smoking gun: the just-allocated new PML4 is being passed to `free_process_page_table` as `cr3_phys` only ~3 ticks after creation. CR3 is still pointing at this frame when the kernel next mutates its heap, walks PML4[256], and faults on the now-corrupted entry that was overwritten by the slab cache.

### The Bug #7 fix did not close the residual

`b23a24c` moved `Cr3::read()` in execve to BEFORE `set_current_user_return` so the captured `old_cr3_phys` cannot be the new_cr3 (eliminating the obvious "preempt between urs publish and Cr3::read makes the dispatcher restore Cr3 = new_cr3" race). The same six-event lifecycle still reproduces under preempt-full smoke-test, so the actual race is elsewhere.

Three viable hypotheses remain:

1. **Frame allocator double-allocate.** Two allocate_frame calls on different cores returning the same physical address. The per-CPU page cache + fetch_add on the cache head could have a race window under preempt-full where two pops return the same slot. Verify by reading the per-CPU cache pop path with concurrency in mind.
2. **`free_process_page_table` called on a frame that's also live elsewhere.** A process Y dies and `free_process_page_table` walks Y's page table, finds the victim frame as a *user leaf* in Y's PT, and frees it — even though Y's PT was supposed to be updated by the CoW handler when Y wrote to that page. This would imply the CoW handler's per-process PTE update on Y's side did NOT actually take effect, OR Y's PT inherited a stale copy. Verify by examining `cow_clone_user_pages` and `resolve_cow_fault` for any path that fails to update the OTHER CoW partner's PT.
3. **`execve` calling `free_process_page_table` with `new_cr3_phys` instead of `old_cr3_phys`.** Despite the fix, some path within execve's flow ends up with `old_cr3_phys == new_cr3_phys`. Possible routes: a yield-style call between PROCESS_TABLE update (line 4332) and Cr3::read that runs `save_user_return_state` with the now-updated PROCESS_TABLE → urs.cr3_phys = new_cr3 → preempt → dispatcher restores Cr3 = new_cr3 → execve resumes → Cr3::read returns new_cr3 → old_cr3_phys = new_cr3.

`save_user_return_state` is called by `yield_now` (`scheduler.rs:2062`) and `block_current_until` (`scheduler.rs:2533`). It is **not** called by `preempt_frame_to_scheduler` (`scheduler.rs:2151`), which only refreshes `urs.fs_base`. So a pure IRQ-driven preempt does NOT refresh urs.cr3_phys. The race in Hyp #3 would require a yield/block between PROCESS_TABLE update and Cr3::read.

### Next-session investigation (Session 8)

1. **Make `free_process_page_table` `#[track_caller]`** so the trace ring records the **outer** caller (currently records mm/mod.rs:498 which is uninformative). Until that lands, the `[free_pt]` log line is the only signal — but it didn't fire on a faulting iteration during Session 7's runs (the [free_pt] log emits at every entry; on a fault the dump shows the surrounding history). Re-run with multiple iterations to capture both signals together.
2. **Audit `cow_clone_user_pages` for refcount/flag setup.** Specifically: when fork CoW-clones, does it correctly increment refcount for shared pages? Does it correctly mark BOTH parent and child PTEs with BIT_9 + non-WRITABLE? If the child's PT inherits a stale entry (mapped to old frame, BIT_9 marker missing), the CoW handler in the child wouldn't update it — but then the parent's free would leave an orphaned mapping in the child.
3. **Audit execve between line 4332 (PROCESS_TABLE update) and line 4413 (Cr3::read)** for any explicit or implicit yield. Specifically, `reset_current_task_fpu_state`, the `log::info!` calls, and any `Vec::push`/heap mutation that could trigger `try_grow_on_oom_for_layout` → `grow_heap` → ... → yield.
4. **Add a `#[track_caller]` chain through `new_process_page_table`** so the frame-trace records who called it. Currently mm/mod.rs:288 is uninformative.

### Files to pre-load (Session 8)

- `kernel/src/mm/mod.rs:282-381` — `new_process_page_table` (allocate_frame call).
- `kernel/src/mm/mod.rs:396-500` — `free_process_page_table` (already track_caller, but inner call sites are not).
- `kernel/src/arch/x86_64/syscall/mod.rs:3858-3960` — `sys_fork` and `cow_clone_user_pages`.
- `kernel/src/arch/x86_64/interrupts.rs:233-326` — `resolve_cow_fault`.
- `kernel/src/arch/x86_64/syscall/mod.rs:4263-4475` — `sys_execve` (with my Bug #7 fix at 4413).

### Status of acceptance criteria as of `b23a24c`

| Criterion | Status |
| --- | --- |
| (1) `run-gui` + `fb-takeover doom` renders | not verified |
| (2) `run-gui` + TAB completion | not verified |
| (3) `cargo xtask smoke-test` passes deterministically | ❌ — still intermittent; about 1 in 3 attempts hits the kernel page fault |
| (4) `run-gui` 10-min soak | not verified |
| (7) per-frame trace ring + page-fault dump | ✅ landed |
| (8) UAF/double-allocate site identified, fixed | ❌ — lifecycle traced but root caller TBD |

---

## Session 8 — root cause identified and fixed

### The defensive log fired (`b23a24c` + new sanity check)

After making `free_process_page_table` `#[track_caller]` and adding a sanity check that warns when `cr3_phys` equals the active CR3 at free time, a captured fault produced this output:

```
[INFO] [free_pt] cr3_phys=0x6d0000 caller=kernel/src/arch/x86_64/syscall/mod.rs:4466
[WARN] [free_pt] !!! cr3_phys=0x6d0000 EQUALS active CR3 — caller=kernel/src/arch/x86_64/syscall/mod.rs:4466
```

`syscall/mod.rs:4466` is execve's `free_process_page_table(old_cr3_phys)` call. The fact that `cr3_phys` (= `old_cr3_phys`) equals the active CR3 (= `new_cr3` after `Cr3::write(new_cr3)`) deterministically proves Hypothesis #3: `old_cr3_phys == new_cr3_phys`.

### Root cause

The race chain in execve:

1. `proc.addr_space = Some(new_addr_space.clone())` updates PROCESS_TABLE (line 4332).
2. `reset_current_task_fpu_state()` takes `scheduler_lock` (line 4373). On drop, `SchedulerGuard::drop` calls `preempt_enable`.
3. Under `preempt-full`, `preempt_enable`'s zero-cross can synchronously fire `yield_now` if `reschedule == true` and `IF == 1`.
4. `yield_now`'s `save_user_return_state` reads `current_user_return_addr_space_snapshot(pid)` — which now returns the **new** CR3 because PROCESS_TABLE was updated in step 1 — and writes `urs.cr3_phys = new_cr3`.
5. Dispatcher restore loads `Cr3 = urs.cr3_phys = new_cr3`.
6. execve resumes; the previous Bug #7 fix's `Cr3::read()` returns `new_cr3`.
7. `old_cr3_phys = new_cr3`.
8. `free_process_page_table(old_cr3_phys)` frees the just-allocated new PML4 while CR3 still points at it.
9. The freed frame is reused by a slab span (or another new PML4); the new owner overwrites the kernel-half PML4 entries.
10. The next kernel heap mutation walks PML4[256] of the zombie CR3 and faults on garbage.

This is the exact `0xffff80000051ca18` / corrupted PML4[256] symptom Sessions 4–7 chased.

### Fix (`d8db950`)

Derive `old_cr3_phys` from the **already-captured `_old_addr_space` Arc** rather than from `Cr3::read`:

```rust
let old_cr3_phys = _old_addr_space
    .as_ref()
    .map(|addr_space| addr_space.pml4_phys().as_u64())
    .unwrap_or(0);
```

The `_old_addr_space` Arc is taken at line 4314 — **before** PROCESS_TABLE is mutated at line 4332. Its `pml4_phys()` field is immutable, so even if `save_user_return_state` later overwrites `urs.cr3_phys`, the captured Arc still represents the actual pre-execve CR3.

The `Cr3::read()` is gone entirely from this code path — it was the broken source of truth.

Also added a `if old_cr3_phys != 0` guard so the very first execv'd task (which has no prior `addr_space`) does not call `free_process_page_table(0)`.

### Why the previous Session 7 attempt failed

The earlier "Bug #7 fix" moved `Cr3::read` to before `set_current_user_return`. That closed the *direct* "preempt between urs publish and Cr3::read makes dispatcher restore Cr3 = new_cr3" race — but it did not close the *upstream* race where `reset_current_task_fpu_state`'s `scheduler_lock` drop fires `preempt_enable` → `yield_now` → `save_user_return_state` reading the already-mutated PROCESS_TABLE. The dispatcher then restores `Cr3 = new_cr3` *before* execve's `Cr3::read` runs, and the outcome is the same.

The lesson: under `preempt-full`, **anything that reads PROCESS_TABLE.addr_space.pml4_phys() AFTER the table has been mutated is suspect**. Use the captured-before-mutation Arc instead.

### Validation observations (5-iteration loop after `22cd711`)

| iter | outcome | kernel faults | free_pt !!! |
| --- | --- | --- | --- |
| 1 | Terminated (BlockedOnReply cascade, pid=21 stuck 171 s) | 0 | 0 |
| 2 | PASSED on attempt 2 (10 s) | 0 | 0 |
| 3 | Terminated (BlockedOnReply cascade) | 0 | 0 |
| 4 | FAILED (prompt-ready gate timeout — slow boot) | 0 | 0 |
| 5 | FAILED (prompt-ready gate timeout — slow boot) | 0 | 0 |

After landing `d8db950`, the 5-iteration loop confirms:

1. **Zero kernel page faults** across all attempts of all iterations. The `0xffff80000051ca18` / corrupted PML4[256] symptom is gone.
2. **Zero `[free_pt] !!! cr3_phys EQUALS active CR3` warnings** — the defensive sanity check stays silent, confirming the active-CR3 race is closed.
3. **Smoke-test is still intermittent (1/5 pass-rate)**, but the failure mode has shifted: runs that fail now either hang on the `BlockedOnReply` watchdog cascade (iters 1, 3 — same shape as Sessions 1–3 lost-wakeup) or time out the prompt-ready gate during slow boot (iters 4, 5).
4. **Successful runs pass cleanly and quickly** (iter 2: 10 s on attempt 2).

The `BlockedOnReply` cascade is **the same shape as the original Session 1–3 lost-wakeup symptom** (Bug #6 family), not a regression of the just-fixed Bug #7 slab UAF. Sessions 3–4 originally attributed the cascade to the slab UAF zombieing a non-BSP core (which created a partial-deadlock zombie when other cores depended on the dead core). With the UAF closed, the cascade should be gone — but it isn't, so a separate Bug #6 variant is still open.

### Residual — separate Bug #6 variant under preempt-full smoke-test

The smoke-test specifically (not bare `cargo xtask run`) intermittently hits the `BlockedOnReply` cascade. The likely candidates:

1. **A fourth Bug #6 deliver+wake site that the F2 partial helper missed.** The Session 3 helper `deliver_message_and_wake` covers 11 simple sites in `endpoint.rs` and `cleanup.rs`, but `endpoint.rs:340..850` has more complex multi-step send/recv paths that may not be wrapped.
2. **An IPI-coalesce-style livelock** that survives the Session 3 `init_task` halt fix (the bare-coalesce experiment at `scheduler.rs:1041` was reverted, but the underlying wake-storm pattern may still surface under heavy fork/exec load).
3. **A slow-boot effect** unrelated to wakeups — e.g. preempt-full's tighter scheduling cadence elongates each tcc-compile step enough that the smoke-runner's 30 s watchdog fires before tcc completes.

This residual is **independent of the Session 8 fix** and was already present in earlier sessions when the slab UAF was the dominant symptom. Filing as Bug #8 for a separate session.

### Acceptance criteria status

| Criterion | Status |
| --- | --- |
| (1) `run-gui` + `fb-takeover doom` renders | not verified |
| (2) `run-gui` + TAB completion | not verified |
| (3) `cargo xtask smoke-test` passes deterministically | ❌ — 1/5 pass-rate; failures are Bug #8 (cascade + slow-boot), NOT the closed Bug #7 |
| (4) `run-gui` 10-min soak | not verified |
| (7) per-frame trace ring + page-fault dump | ✅ landed |
| (8) Bug #7 frame UAF identified and fixed | ✅ (`d8db950`); 0 recurrences across 5-iter validation |

---

## Session 9 — scheduler-side fix attempt failed; root cause is virtio-blk latency, not IPI livelock

### TL;DR

Diagnosed the captured `BlockedOnReply` failure mode as an **IPI-driven preempt-resume livelock** on an AP core (Session 2's H7' shape) and tried four variants of a two-pronged fix (source-side IPI coalesce with idle-target exception + target-side preempt filter). All four variants failed validation: best result was 3/5 pass-rate (no improvement over the 1/5 baseline once timing variance is accounted for), worst was 0/10 with a fundamental boot regression.

The diagnosis was wrong. The "livelock" trace pattern (`Dispatch → SwitchOut → RunQueueEnqueue` cycles with `saved_rsp` ping-ponging by 16 bytes) is consistent with **normal heavy IPC on a slow disk**, not a kernel preempt loop. The actual bottleneck is **virtio-blk completion latency**: 340 ms for a single READ, with `[WARN] [virtio-blk] completion poll + queue notify after request timeout` firing repeatedly. Cumulative slowness across tcc-compile's many file reads exceeds the smoke-test's 180 s budget. **All scheduler changes have been reverted** — branch tip after this session matches its state before Session 9 started (`2e1bbc4` plus the handoff updates landed at the tail).

Track G's 24 h soak gate stays closed.

### What was tried (and reverted)

Working hypothesis at session start: target-side preempt filter would catch the case where `pick_next` would re-dispatch the currently-running task, and source-side IPI coalesce would refine Session 2's reverted bare-coalesce by handling the AP-hlt wake hole (always send IPI when target is in `enable_and_hlt`).

| Variant | What changed | Pass-rate | Failure mode |
|---|---|---|---|
| v1 — naive | Source-side CAS coalesce + idle exception via new `is_idle: AtomicBool`; target-side filter "skip preempt if local run queue empty" on all cores | 0/10 | Boot hangs after `init: started 'sshd' pid=3`. `init`'s 5 ms `nanosleep_for(...)` between service spawns relies on the scheduler dispatch loop running `drive_expired_wake_deadlines()`. The "skip if queue empty" filter blocked the dispatch loop from iterating, so deadlines never expired. |
| v2 — deadline-aware | Same as v1 plus a global `ACTIVE_WAKE_DEADLINES > 0` gate that disables the filter when any task has a pending deadline | 3/5 (some on attempt 2 or 3) | Cascade still surfaces. virtio-blk's polling timeouts likely keep `ACTIVE_WAKE_DEADLINES > 0` continuously, defeating the filter for most of boot. |
| v3 — AP-only | Filter applies only on non-BSP cores; BSP always preempts so its dispatch loop keeps housekeeping (deadline scan, drain_dead, watchdog, load_balance) running | 1/5 | New `[WARN] [sched] stale-ready: pid=N name=fork-child core=2 stale~50 ms` warnings (Ready tasks not dispatched for 50–80 ms) and `cpu-hog: pid=0 name=idle ran~30350 ms final_state=Running` on a stuck AP. The source-side coalesce was racing against the AP filter and losing wakes. |
| v4 — filter only | Removed source-side coalesce entirely; kept AP-only target-side filter | 1/5 | Same shape as baseline. Filter wasn't catching the actual livelock pattern (queue is non-empty when ion is the highest-priority task; `pick_next` would re-pick it regardless). |

All four variants have been reverted (`git checkout kernel/src/arch/x86_64/interrupts.rs kernel/src/task/scheduler.rs kernel/src/main.rs kernel/src/smp/boot.rs kernel/src/smp/mod.rs` — tree clean as of session end).

### Why the diagnosis was wrong

Re-reading the original Bug #8 trace dump (`/tmp/m3os-smoke-bug8-1.log`) with fresh eyes plus the new soak captures:

1. **`task_idx=20 == /bin/ion`, not the smoke-runner's tcc-compile child.** Confirmed by the cpu-hog log line in the original Session 2 capture: `pid=20 name=fork-child exec_path=/bin/ion`. ion is the userspace shell that term spawns at session boot — it's reading shell init scripts via vfs_server, which does heavy IPC.
2. **The `saved_rsp` ping-pong by 16 bytes is consistent with normal short userspace work between IPC blocks**, not a tight kernel-mode preempt-resume loop. Each Dispatch is followed by a small amount of userspace before the next IRQ. The lack of `RecvBlock`/`CallBlock` events in the 256-entry trace ring just means those events were OVERWRITTEN by newer Dispatch/SwitchOut events — at high IPC frequency, the 4096-entry ring still wraps in tens of ms.
3. **Even if it WERE a kernel-mode preempt loop, the "skip preempt when queue empty" filter doesn't catch it.** The original livelock had OTHER tasks on the queue (woken by cross-core IPI from vfs/drivers); they just had lower priority than the running ion. `pick_next` would still pick ion, regardless of queue contents.
4. **The `(no waker registered)` watchdog message is misleading for `BlockedOnWait`.** A `BlockedOnWait` task's "waker" is the child's exit. If the child hasn't exited (because it's slowly chugging through disk I/O), no waker is registered — that's expected, not a bug.

The four failed variants together rule out the IPI-livelock framing. The actual bottleneck is somewhere else.

### Where the data points: virtio-blk completion latency

The reproducible slow signal across both Bug #8 failure shapes is virtio-blk:

```
[WARN] [virtio-blk] completion poll + queue notify after request timeout owner_pid=21 type=1 sector=2072 completed=false
[WARN] [virtio-blk] completion poll + queue notify after slot-wait timeout owner_pid=21 type=1 sector=2072 completed=false
… (6 repetitions per request)
vfs_server: slow req#43 READ elapsed_us=340165
```

340 ms for one READ. Tcc-compile reads ~hundreds of files (headers, source, intermediate output); if each round-trip is hundreds of ms under preempt-full instead of the ~ms expected under preempt-voluntary, the 180 s smoke-test budget is exhausted before tcc finishes. That explains both:

- **Cascade failure mode (Bug #8.1):** parent (smoke-runner pid=18) waits via `waitpid` on child (tcc); child waits on vfs_server; vfs_server waits on virtio-blk; virtio-blk timeout-and-retries each completion. Cumulative latency exceeds the test step's deadline.
- **Slow-boot failure mode (Bug #8.2):** `prompt-ready gate timed out` because syslogd / sshd's readiness handshake reads files via the same slow virtio-blk path.

Without scheduler intervention, `cargo xtask run` (no smoke-runner timeout, no retry harness) under preempt-full reaches `SMOKE:PASS` reliably (handoff Session 3 confirmed). The 180 s smoke-test step boundary is what makes Bug #8 visible.

### Recommended next step — targeted profile, then fix

**Don't patch blind again.** Profile a single virtio-blk request end-to-end under both kernel modes, with timestamps at:

1. Request submitted to virtq (driver task).
2. MMIO write to notify the device (driver task).
3. Completion IRQ fires (LAPIC IRR set on which core?).
4. IRQ handler reads completion ring and signals waker.
5. Waker delivers to driver/vfs_server task; `wake_task_v2` → dispatch.
6. vfs_server reads ring, replies to caller via IPC.

The 340 ms outlier has to be in *one* of those steps. Knowing which one tells you the fix:

- **(3) IRQ delivery delayed** → MSI-X routing race or LAPIC IRR collapsing under preempt-full's higher IRQ traffic. Fix in IRQ delivery path / MSI-X vector strategy.
- **(4 → 5) wake delivery delayed** → the wake fires but reaches the dispatcher with extra latency (cross-core sync, pi_lock contention, dispatch-loop iteration delay). Fix in `wake_task_v2` or virtio-blk's IRQ-context wake handoff.
- **(6) vfs_server slow** → ring contention or extra preempts inside the server. Fix in vfs_server's hot path or scheduling priority.

Minimal instrumentation patch (suggested shape — not yet written):

```rust
// In the virtio-blk request submission + completion path, gated by a feature
// flag or env var so it's off in production:

#[cfg(feature = "virtblk-trace")]
{
    let now = crate::arch::x86_64::interrupts::tick_count();
    log::info!(
        "[virtblk-trace] req={} stage=submit owner_pid={} sector={} t={}",
        req_id, owner_pid, sector, now,
    );
}
// … and similar at: notify, irq-fire, irq-handler-completion, wake-fire,
// dispatch, vfs-ack, vfs-reply.
```

Capture under `M3OS_KERNEL_FEATURES="preempt-full,virtblk-trace"` and `M3OS_KERNEL_FEATURES="preempt-voluntary,virtblk-trace"`, run a single `cargo xtask smoke-test` to first failure (or full pass), grep the timestamps, find the slow step, fix.

### Other options if profiling is out of scope

In rough order of decreasing rigor:

1. **Band-aid: relax timeouts.** Bump the 180 s smoke-test step timeout *and* virtio-blk's per-request retry deadline (whatever drives the "completion poll + queue notify after request timeout" warning). Closes the test (probably) but hides the regression. Not a real fix; just unblocks the soak gate.
2. **Lower preempt-full's IRQ-handler overhead.** The `signal_reschedule` + `lapic_eoi` + `check_and_preempt_kernel` path runs on every IRQ. Under preempt-full this path is structurally heavier than preempt-voluntary's IRQ-return path. If virtio-blk's MSI-X completions land at high frequency (each disk read = 1 IRQ, tcc-compile = hundreds of reads), per-IRQ overhead amplifies. Audit `kernel/src/arch/x86_64/interrupts.rs:1535-1596` (`check_and_preempt_kernel`) for hot-path optimisations.
3. **Step back and reframe.** The Bug #6 closure relied on the assumption that `(no waker registered)` warnings ALWAYS meant a lost-wake. Sessions 1–8 chased that hypothesis through six dead ends before Bug #7 closed the slab UAF. Consider that what's left is genuinely "preempt-full IS slower per IPC round-trip, and the smoke-test budget needs to reflect that" — measure the slowdown ratio, decide if it's acceptable, either accept it (adjust budgets and proceed to Track G) or schedule real perf work.

### What NOT to do (lessons from Session 9)

1. **Don't patch the scheduler based on a single trace ring's pattern.** The 256/4096-entry rings can wrap thousands of times during a 30 s hang; the surviving entries show the LAST tens of ms, which may not represent the steady state. Use sched-trace + `M3OS_SMOKE_SERIAL_DUMP` together, and triangulate with cpu-hog / stale-ready warnings.
2. **Don't try "skip preempt when queue empty" without checking deadline pressure.** `init`'s `nanosleep_for(...)` between service spawns relies on the dispatch loop iterating; any filter that prevents preempts also prevents the dispatch loop from running, which prevents `drive_expired_wake_deadlines` from running.
3. **Don't combine source-side IPI coalesce with target-side filter without designing the interaction.** They race: if the source coalesces (skips IPI because `reschedule` was already true) but the target's previous reschedule was just consumed by a filter-skip, the wake is delivered as a queue entry but never gets a preempt to dispatch it. Stale-ready tasks ensue.
4. **Don't trust task_idx-to-pid mapping by inspection.** Use sched-trace (which logs pid + state transitions with `#[track_caller]` file:line) to confirm WHICH userspace task is showing the "livelock" pattern. The session-9 confusion was caused by assuming `task_idx=20` was the smoke-runner's tcc child when it was actually `/bin/ion`.

### Files touched and reverted (for posterity)

```
kernel/src/arch/x86_64/interrupts.rs   (filter calls in check_and_preempt_user/kernel)
kernel/src/task/scheduler.rs           (should_skip_preempt_irq helper, enqueue_to_core coalesce)
kernel/src/main.rs                     (idle_task is_idle gating)
kernel/src/smp/boot.rs                 (ap_idle_task is_idle gating)
kernel/src/smp/mod.rs                  (PerCoreData::is_idle field)
```

All reverted via `git checkout` — branch tip clean, `cargo xtask check` green.

### Acceptance criteria status (unchanged from Session 8)

| Criterion | Status |
| --- | --- |
| (1) `run-gui` + `fb-takeover doom` renders | not verified |
| (2) `run-gui` + TAB completion | not verified |
| (3) `cargo xtask smoke-test` passes deterministically | ❌ — 1/5 pass-rate; failures attributed to virtio-blk completion latency under preempt-full, NOT scheduler livelock |
| (4) `run-gui` 10-min soak | not verified |

### Files to pre-load for next session (Session 10)

- `kernel/src/drivers/virtio_blk` (or the actual path — see `cargo xtask check` output for crate layout) — completion-wait path, request-timeout retry logic.
- `kernel/src/ipc/endpoint.rs` — vfs_server's reply path.
- `userspace/vfs_server/src/main.rs` — vfs_server's READ handler.
- `/tmp/m3os-smoke-bug8-1.log` — the original failure capture (includes both failure shapes plus the "completion poll + queue notify" warnings).
- `xtask/src/main.rs:3282..3540` — `run_smoke_script` and `M3OS_SMOKE_SERIAL_DUMP` plumbing.

### Quick-start reproducer (unchanged)

```bash
M3OS_KERNEL_FEATURES="preempt-full,sched-trace" \
  M3OS_SMOKE_SERIAL_DUMP=/tmp/m3os-smoke-full.log \
  cargo xtask smoke-test
```

Pass rate is ~1 in 5 attempts. On failure, `/tmp/m3os-smoke-full.log` contains the full kernel serial including any deferred trace-ring dump.
