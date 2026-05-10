# Post-mortem: Phase 61 per-tick CPU accounting slow_req regression

**Incident:** Phase 61 Track E.2 (per-tick CS sampling) and E.4 (per-CoW
page-fault rusage accounting) introduced lock-contention from interrupt
context that produced 50–200 ms tail latency on `vfs_server` IPC
roundtrips. Visible to the user as a 15× slowdown on tab-completion
and Doom WAD loads versus `main`.
**Status:** Resolved 2026-05-09.
**Severity:** Medium-high — kernel did not fault, but interactive
workloads were noticeably degraded; the regression hid behind a
separate AP-core GPF (see `2026-05-09-ap-core-gpf-stack-aliasing` once
written / `docs/handoffs/ap-core-gpf-saved-rsp-stack-corruption.md`)
that crashed the kernel before the slow path manifested.
**Owners:** Kernel (scheduler, task accounting).
**Fix commit:** `87c5c87` fix(61): redo per-task CPU/rusage counters
Linux-style.
**Intermediate commit (superseded):** `0c9310a` fix(61): move per-task
tick/fault counters to lock-free RUSAGE table — fixed the lock but
introduced false sharing; obsoleted by `87c5c87`.
**Diagnostic branch:** `diag/61-disable-per-tick-accounting` (commit
`7ecd02e`) — temporarily disabled the IRQ-context helpers to isolate
the cost; can be deleted now that the root cause is fixed.

## Summary

Phase 61 added two helpers that ran from interrupt context on every
core, every tick:

- `tick_account_current_task` (Track E.2 — `a2427ff`) — called from
  the timer ISR (`timer_handler_user` and `timer_handler_kernel`) on
  every tick to attribute one tick of wall-clock time to either
  `Task::user_ticks` or `Task::system_ticks` based on the interrupted
  CS ring.
- `current_task_record_page_fault` (Track E.4 — `142a497`) — called
  from `page_fault_handler` after a CoW resolution to increment
  `Task::minor_faults`.

Both helpers acquired `try_scheduler_lock()` (the global
`SCHEDULER_INNER` `IrqSafeMutex`) to mutate the `u64` fields on
`Task`. The try-lock itself was added by `7785bb5` after the
original blocking-lock version produced a same-core deadlock.

`try_scheduler_lock` is non-blocking, but the **successful** path
briefly holds `SCHEDULER_INNER` from interrupt context. With four
cores doing this on every tick (BSP at 1 kHz, APs at 100 Hz —
~1300 lock attempts per second), the contention competed with the
IPC dispatcher, which holds the same lock for `pick_next` /
`set_current_task_idx` / `enqueue_to_core` and is the critical path
for every `vfs_server` reply. Tasks waiting to be redispatched
after vfs_server replied to them paid tick-quantized scheduler
latency on every IPC roundtrip; under the Doom WAD-load workload
that surfaced as 50–200 ms vfs_server `slow req` warnings.

The fix moves the four counter fields from `Task` (mutated under
the scheduler lock) to `AtomicU64` fields on `Task` (mutated
lock-free by the CPU currently running the task), accessed from
the IRQ helpers via a per-core cached `current_task_ptr` that
mirrors the existing `current_preempt_count_ptr` pattern. This is
the design Linux uses for `task_struct.utime` /
`task_struct.stime`.

## Impact

- `vfs_server: slow req` count under a Doom WAD-load workload:
  - `main` (no per-tick accounting): 0 warnings observed.
  - HEAD with per-tick (`cefe92a` after the AP-core stack fix
    landed): 242 warnings, avg 70 ms each, ~17 s cumulative wait
    time.
  - With the fix (`87c5c87`): 76 warnings on a 13× larger Doom
    workload — true per-request slow rate dropped to ~4 % from a
    pre-fix 5.7 %.
- Interactive symptoms: shell tab-completion sluggish, Doom WAD
  load took noticeably longer to complete, lights/animations in
  Doom's title screen visibly stuttered before play began.
- No kernel faults. No data corruption. No test failures (the
  regression test `kernel/tests/child_times_e1.rs` passed
  throughout — its assertion only required that counters
  *advanced*, which they did even under the slow path).

## Why this hid for so long

The slow path manifested only after the AP-core GPF
(`docs/handoffs/ap-core-gpf-saved-rsp-stack-corruption.md`) was
fixed in `cefe92a`. Before that commit, the kernel reliably
crashed an AP at ext2-mount time, which prevented userspace from
reaching the steady-state IPC pattern where the scheduler-lock
contention bites. The slow_req warnings *did* fire occasionally
in pre-AP-core-fix logs, but were attributed to "general boot
flakiness" because the kernel was about to crash anyway.

The kernel-side test suite (`kernel/tests/*.rs`) didn't catch the
regression either: every existing SMP test runs short, single-task
workloads where scheduler-lock contention is below threshold. A
new IPC roundtrip latency test (`kernel/tests/ipc_roundtrip_latency.rs`,
landed in the diagnostic branch) attempts to model multi-task IPC
contention but at 200 iterations the tail latency is still 100×
below what the user-visible Doom workload produces. Reproducing
the regression in an automated test would require a much heavier,
contention-generating workload than any existing test runs.

## Timeline

| Date (2026) | Event |
|---|---|
| 04-XX | Track E.2 lands (`a2427ff`) — adds `tick_account_current_task` from timer ISR using blocking `scheduler_lock()`. |
| 04-XX | Track E.4 lands (`142a497`) — adds `current_task_record_page_fault` from page-fault handler, same lock pattern. |
| 05-09 00:17 | Same-core deadlock GPF observed under fork-bomb load. `7785bb5` switches both helpers to `try_scheduler_lock()`. Believed-resolved at this point. |
| 05-09 02:34 | AP-core stack-aliasing GPF observed during user testing. Multiple failed mitigation attempts (saved_rsp bounds checks). All reverted. Handoff doc filed. |
| 05-09 07:13 | AP-core GPF resolved by isolating kernel stacks in static `.bss` pool (`cefe92a`). Boot now reaches steady-state userspace. |
| 05-09 07:30 | First user observation of `vfs_server: slow req` at scale: 242 warnings during Doom workload. |
| 05-09 (afternoon) | Diagnostic A/B branch `diag/61-disable-per-tick-accounting` (`7ecd02e`) confirms Track E.2/E.4 are the cause: 242 → 16 slow_reqs (15× reduction). |
| 05-09 (later) | First refactor (`0c9310a`) moves counters to a global `[TaskRusage; 256]` static table to drop the lock entirely. Lock cost gone, but adjacent 32-byte slots share 64-byte cache lines → false sharing across cores when concurrent tasks ran on different cores. User reported "slow again". |
| 05-09 (later still) | Linux-style refactor (`87c5c87`) — counters back on `Task` as `AtomicU64`, accessed via per-core cached `current_task_ptr`. Each Task lives in its own `SlabBox` larger than a cache line; only the CPU currently running the task writes its counter, so no false sharing and no cross-CPU coherence traffic. Slow-rate drops to ~4 % under heavy Doom workload — within virtio-blk's intrinsic per-request latency tail. |

## Root causes

The regression had two distinct root causes that compounded:

1. **Phase 61's design assumed `try_scheduler_lock` was free.** It
   isn't. The lock is the IPC critical-path lock, held briefly by
   every dispatch. Adding 4 IRQ-context contenders per second
   (BSP) is enough to perturb tick-quantized scheduler latency by
   a few ticks per dispatch, which compounds across every vfs_server
   roundtrip. The lesson: any lock taken on the IPC dispatch path
   cannot tolerate IRQ-context contenders.
2. **The first refactor traded lock contention for cache contention.**
   Moving counters to a static `[TaskRusage; 256]` table where
   adjacent 32-byte slots share 64-byte cache lines created classic
   false sharing — every per-tick write on core N invalidated the
   cache line on core M whose task happened to land in an adjacent
   slot. The lesson: when relocating per-task data out of the task
   struct, either pad to a cache-line boundary or keep it inline
   with a struct that is already larger than a cache line.

## Resolution

`87c5c87` — `Task::user_ticks`, `system_ticks`, `minor_faults`,
`major_faults` are `AtomicU64` fields directly on `Task`.
`AtomicU64` is `#[repr(transparent)]` over `u64`, same size and
alignment, so `EXPECTED_TASK_PREEMPT_FRAME_OFFSET` (the Phase 57d
assembly contract) is preserved without padding.

The IRQ helpers access these fields lock-free via a new per-core
`current_task_ptr: AtomicPtr<Task>` in `PerCoreData`, set by the
dispatcher at every `set_current_task_idx(Some(idx))` site and
cleared at every `set_current_task_idx(None)` site. The helper
`current_task_ptr() -> Option<&'static Task>` reads the cached
pointer (single atomic load) and dereferences. The only write
path is the local CPU's `fetch_add` on its own task's counter,
which produces no cross-CPU coherence traffic and no false
sharing because each `Task` is in its own `SlabBox` larger than a
cache line.

This mirrors Linux's `task_struct.utime` / `stime` model: every
distro since 2.6 has shipped this design, run on every CPU at
250–1000 Hz, and nobody has noticed.

Verification:

- `cargo xtask check` clean (clippy + rustfmt + kernel-core,
  passwd, driver_runtime host tests).
- `cargo xtask test` — all 12 tests pass, including
  `child_times_e1` (the Track E.1/E.2/E.4 regression test).
- `cargo xtask run --fresh` boots cleanly to userspace with zero
  GPF / DOUBLE FAULT / panic lines.
- User-side A/B confirms `vfs_server: slow req` per-request rate
  is ~4 % under heavy Doom workload — comparable to the diag-branch
  rate (which had per-tick disabled entirely) within the noise of
  small-sample comparisons.

## What is *not* fixed

The remaining ~4 % slow-request tail under heavy Doom workload is
not in the kernel scheduler or accounting paths. It is the
intrinsic per-request latency of QEMU's virtio-blk under sustained
burst-read load — Linux on the same QEMU virtio-blk shows similar
tails. Hitting `slow req` warnings during multi-MB WAD loads is
expected with the current virtio-blk request-path design.

This is documented as a separate concern, not a Phase 61
regression.

## Recommendations for further performance work

In rough order of expected impact-per-effort, lowest-effort first:

### 1. Investigate virtio-blk request batching and queue depth

The remaining 4 % slow-request tail under heavy Doom workload
suggests virtio-blk is the next bottleneck. Concrete experiments:

- Measure the current effective queue depth in
  `kernel/src/blk/virtio_blk.rs` — how many requests does
  `vfs_server` keep in flight at once, vs the device's
  advertised queue size (typically 256)?
- Coalesce adjacent reads: if `vfs_server` is issuing 4 KiB reads
  for a contiguous WAD chunk, batching into a single 64 KiB
  `READ` request through a virtio-blk indirect descriptor would
  reduce round-trips and the per-request fixed overhead.
- Add IRQ coalescing on the device side (`VIRTIO_BLK_F_FLUSH`
  / interrupt batching) so several completions trigger one ISR
  instead of one ISR per completion.

Expected impact: high (this is currently the dominant
user-visible latency under sustained read workloads).

### 2. Replace `rdmsr` in `per_core()` with a `gs:[0]` self-pointer load

`crate::smp::per_core()` (`kernel/src/smp/mod.rs:499`) reads
`IA32_GS_BASE` via `rdmsr` on every call. `rdmsr` is a
serializing instruction (~50 cycles on real hardware,
significantly slower under QEMU TCG). The kernel calls
`per_core()` from many hot paths including the timer ISR and the
new IRQ-context CPU-time helper. `PerCoreData` already has a
`self_ptr` field at offset 0 specifically for this — switch
`per_core()` to `mov rax, gs:[0]`.

Expected impact: small per-call (single-digit nanoseconds saved
per `per_core()` call) but multiplies across the kernel — every
syscall, every IRQ, every dispatch.

### 3. Audit other helpers that take the global scheduler lock from
###    interrupt context

The Phase 61 lesson generalises: any code path that runs from IRQ
context and acquires `SCHEDULER_INNER` is a hidden contention
source on the IPC dispatch path, even with `try_lock`. Audit
`kernel/src/task/scheduler.rs` for `try_scheduler_lock()` callers
and confirm each one is genuinely on a slow path. Likely
candidates:

- `kernel/src/task/watchdog.rs` (every BSP scheduler iteration —
  currently fine because BSP-only, but worth confirming).
- `wake_task_v2` ISR-context callers (already fast-pathed via
  `IsrWakeQueue`, but the SCHEDULER mirror is the slow leg).

Expected impact: medium — depends on what the audit finds.

### 4. Convert the dispatcher's `Vec<SlabBox<Task>>` access pattern
###    to lock-free for the read-side

Most reads of `tasks[idx]` from helpers like `current_task_times`,
`task_times_for_pid`, `rusage_counters_for_pid` take the global
scheduler lock just to dereference one pointer-stable address.
Since the addresses are stable for the task's lifetime (Phase 60
slab work), a parallel `[AtomicPtr<Task>; MAX_TASKS]` mirror —
populated in `alloc_task_slot`, cleared in `drain_dead` — would
let read-only helpers skip the lock entirely. Same shape as the
new `current_task_ptr` cache, generalised.

Expected impact: small (these helpers aren't on the IPC critical
path) but reduces global-lock pressure and makes future
refactors easier.

### 5. Add an automated multi-task IPC contention regression test

`kernel/tests/ipc_roundtrip_latency.rs` (in the diagnostic branch)
models 2-task IPC roundtrip latency and didn't catch this
regression. A 16-task variant — 8 client/server pairs all
hammering vfs_server-style endpoints concurrently while the timer
runs at full rate — would have caught the per-tick lock
contention before user-side observation. Land it as part of the
Phase 61 closeout test surface.

Expected impact: high (regression prevention) at moderate
implementation cost (~1 day).

### 6. Defer to a future phase: cache-line-aware data layout audit

The first refactor's false-sharing surprise suggests a kernel-wide
audit would be valuable: identify static tables of small structs
(`[T; N]` where `size_of::<T>() < 64`) and confirm whether
adjacent slots are written by different CPUs. Candidates:

- `[AtomicU8; MAX_TASKS]` (TCB_BOUND_NOTIF) — if both written
  during normal IPC routing.
- `[AtomicI32; MAX_CORES]` (SCHED_PREEMPT_COUNT_DUMMY) — already
  per-CPU but adjacent slots could share lines.

The fix in each case is `#[repr(align(64))]` padding around hot
fields. Low-cost change, hard to measure without a load test.

Expected impact: small (these are already mostly cold paths) but
forecloses a class of future bugs.

### 7. Consider per-CPU CPU-time accumulators with read-time aggregation

Long-term, the truly Linux-equivalent design for very high
tick-rate accounting is: each CPU accumulates `(user_ticks,
system_ticks)` deltas for "the task currently running on me" into
a per-CPU buffer. At task ctxsw, the buffer is flushed into the
outgoing task's counters. At read time, getrusage iterates
per-CPU buffers + the task's own counters to get a snapshot.

This is what Linux's `kernel/sched/cputime.c` actually does for
`account_user_time` / `account_system_time` on
`CONFIG_VIRT_CPU_ACCOUNTING_GEN`. The current
`AtomicU64.fetch_add` pattern is what Linux's
`CONFIG_TICK_CPU_ACCOUNTING` does — fine for typical workloads,
but switches to the per-CPU buffer model for very-high-frequency
or NO_HZ_FULL configurations.

We are nowhere near needing this. Filing it as future-future work
in case tick rates ever climb to 10+ kHz or NO_HZ_FULL becomes a
target.

## Lessons

1. **`try_lock` from interrupt context is not free.** A
   non-blocking try-lock that *succeeds* still holds the lock
   for the critical section's duration, which competes with
   every other holder. On a hot lock (the IPC dispatch lock),
   the contention compounds.
2. **Static parallel tables of small structs need cache-line
   alignment.** Adjacent 32-byte entries in a 64-byte-line
   architecture share lines and cause false sharing under
   concurrent writes. Either pad each entry to 64 bytes, or
   keep the data inline with a larger struct.
3. **Linux's design is usually right for a reason.** The
   `task_struct.utime` model has been the kernel's CPU-time
   accounting design since the 2.6 era. The "lock-free atomic on
   per-task counter, written only by the running CPU, read
   unsynchronized from anywhere" pattern is fundamental, not a
   detail. Both of our wrong-turn refactors started from "let's
   try something different from Linux" — neither survived
   contact with the workload.
4. **Bug visibility depends on what's running.** This regression
   was present in the kernel for the entire Phase 61 development
   window but was masked by an unrelated AP-core GPF that
   crashed the kernel before steady-state IPC traffic could
   surface the slow path. After the GPF fix, the latency was
   immediately user-visible. Bugs in performance properties of
   shared resources only manifest under realistic concurrent
   workloads — automated short tests are insufficient.

## Related artefacts

- `87c5c87` — fix(61): redo per-task CPU/rusage counters Linux-style.
  Final fix.
- `0c9310a` — fix(61): move per-task tick/fault counters to lock-free
  RUSAGE table. Superseded by `87c5c87`.
- `cefe92a` — fix(mm): isolate kernel stacks in static `.bss` pool.
  AP-core GPF fix that unblocked the discovery of this regression.
- `7785bb5` — fix(61): tick_account / page_fault / ctxsw helpers use
  try_scheduler_lock. The same-core-deadlock fix that left the
  contention path in place.
- `7ecd02e` (branch `diag/61-disable-per-tick-accounting`) —
  diagnostic branch confirming the cause. Can be deleted now.
- `kernel/tests/ipc_roundtrip_latency.rs` (in diag branch) — IPC
  latency test that did not catch this; recommendation 5 above
  proposes a heavier variant.
- `docs/handoffs/61g-smp-soak.md` — Phase 61 Track G manual soak
  handoff; the slow_req warnings are documented there.
- `docs/handoffs/ap-core-gpf-saved-rsp-stack-corruption.md` — the
  AP-core GPF that hid this regression.
- `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` —
  prior `IrqSafeMutex` migration that established the
  IRQ-safety pattern; Track E.2's original blocking
  `scheduler_lock` from the timer ISR was a regression of that
  invariant, fixed in `7785bb5`.
