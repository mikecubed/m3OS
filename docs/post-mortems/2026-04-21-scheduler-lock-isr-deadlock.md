# Post-mortem: SCHEDULER.lock ISR deadlock on same-core wake

**Incident:** Interactive SSH against the guest wedged before banner
exchange in 60 – 70 % of attempts on
`feat/phase-55b-ring-3-driver-host`.
**Status:** Resolved 2026-04-21.
**Severity:** High — blocked the primary inbound-networking acceptance
test on this branch and any interactive workload depending on it.
**Owners:** Kernel (scheduler, virtio drivers).
**Fix commit:** `ac37270` fix(sched): make SCHEDULER.lock IRQ-safe to close early-wedge.
**Doc commits:** `2c331ec`, `fd2c044`.

## Summary

`scheduler::wake_task` acquires `SCHEDULER.lock()`. Two hardware IRQ
handlers —
`kernel/src/net/virtio_net.rs::virtio_net_irq_handler` and
`kernel/src/blk/virtio_blk.rs::virtio_blk_irq_handler` (via
`drain_used_from_irq`) — call `wake_task` synchronously from
interrupt context. `SCHEDULER` was a plain `spin::Mutex<Scheduler>`
that did not mask interrupts on the holder, so a same-core sequence
of "task-context code acquires `SCHEDULER.lock()`; the NIC or block
IRQ fires on that CPU; the ISR's `wake_task` spins on the lock
forever" was a deterministic deadlock. The bug manifested as
timing-dependent SSH wedges because the race depended on where the
interrupt fell relative to the lock acquisition window.

The fix replaces `SCHEDULER` with a thin `IrqSafeMutex` wrapper that
masks interrupts for the duration of every critical section,
eliminating the same-core ISR re-entry class of bugs for this lock.

## Impact

- SSH connection attempts against the guest timed out during banner
  exchange in ~60 % of runs; a separate ~8 % tail mis-classified as
  "late-wedge" (post-KEX timeout) was later confirmed to be slow
  early-wedges narrowly elapsing the 20 s client banner timeout.
- The wedge hung the SSH client, not the VM. Other services
  (`init`, `sh0`, `vfs_server`, shell-local workloads) continued
  running because cores other than the stuck one still dispatched
  tasks.
- Heavy diagnostic logging (per-step `log::info!` in the wake path,
  the virtio-net ISR, and dispatch) accidentally raised the clean
  rate to ~80 % by slowing every path enough to shrink the deadlock
  window. This was a probabilistic mitigation, not a fix.

Nothing else in the system was structurally affected — the lock
order, task-state machine, and IPC paths are unchanged.

## Timeline (condensed)

- **2026-04-19 .. -20.** `feat/phase-55b-ring-3-driver-host` lands a
  TCP deadlock fix (`de6f0d3`) that exposes a previously-shadowed
  SSH wedge. Initial hypothesis: core-0 fairness starvation of
  `net_task`. Spawn-path fairness telemetry does not reproduce it.
- **2026-04-20 .. -21 (days).** Nine hypotheses (H1 – H9) ruled in
  or out with branch-local instrumentation; H6 (sshd wake
  ping-pong), H8 (sys_poll waiter-deregister race), and a partial
  H9 (async-rt cooperative yield) landed as real correctness fixes.
  None of them closed the remaining timing-dependent wedge.
- **2026-04-21 evening.** Pcap evidence via
  `-object filter-dump,netdev=net0` shows every packet of the
  three-way handshake reaches the guest NIC, disproving an earlier
  "QEMU SLIRP drops packets" hypothesis. Serial-log tracing of the
  wake path (`wake_sockets_for_tcp_slot → wake_socket →
  WaitQueue::wake_all → scheduler::wake_task`) pins the wedge to
  the first `SCHEDULER.lock()` acquisition inside `wake_task`.
  A `[sched] stale-ready` warning of 650 – 940 ms on core 0
  confirms core 0 is simultaneously stuck. A failed watchdog
  experiment (timer-ISR driven `wake_task`) hard-deadlocks after
  200 ticks — an unambiguous data point that ISR-context
  `wake_task` blocks on `SCHEDULER.lock` held by the interrupted
  task.
- **2026-04-21 late.** Fix landed as `IrqSafeMutex<Scheduler>` plus
  `interrupts::without_interrupts(…)` around the per-core
  `run_queue.lock()` in `enqueue_to_core`, plus folding
  `wake_task`'s redundant second `SCHEDULER.lock()` (which
  re-entered `PROCESS_TABLE.lock` via `task_log_label`) into the
  first critical section. 15-run validation came back 15/15 clean
  immediately, and a follow-up 60-run confirmation batch came back
  60/60 clean — conclusively rejecting the late-wedge as a separate
  bug (probability of a clean 60-run streak at the pre-fix ~8 %
  late-wedge rate is ~0.7 %).

## Root cause

Three concurrent conditions were required to deadlock:

1. A task-context caller on core *X* holds `SCHEDULER.lock()`.
2. Interrupts are enabled on core *X* while the lock is held.
3. An ISR that calls `wake_task` (virtio-net or virtio-blk) fires
   on core *X* during the lock-hold window.

When all three align, the ISR's `wake_task` spins on
`SCHEDULER.lock()` forever because the interrupted holder cannot
make progress while the ISR runs, and the ISR cannot release a lock
it does not hold. The timer ISR alone does not trigger this —
`signal_reschedule()` is ISR-safe and uses only atomic operations —
but the virtio drivers bypass the ISR-safe wake pattern.

The same ISR-safety pattern was already established elsewhere in the
kernel:

- `virtio_net.rs` and `virtio_blk.rs` wrap their non-ISR
  `DRIVER.lock()` callers in
  `interrupts::without_interrupts(…)` so an ISR cannot re-enter
  `DRIVER.lock()` while a task holds it.
- `mm/frame_allocator.rs` exposes a `with_frame_alloc_irq_safe`
  helper that disables interrupts around
  `FRAME_ALLOCATOR.0.lock()`.
- `notification::signal_irq` explicitly documents "do NOT call
  `wake_task()` — that acquires `SCHEDULER.lock()` which is not
  safe from ISR context" and uses the per-core lock-free
  `IsrWakeQueue` instead.

`SCHEDULER.lock()` was the outlier: widely used, ISR-callable via
`wake_task`, but never wrapped in `without_interrupts`. The 62
call sites all assumed "interrupts stay on during the critical
section" without checking whether the critical section was
safe to be pre-empted by an ISR that would take the same lock.

Secondary contributors that widened the race window but were not
root causes:

- `wake_task` took `SCHEDULER.lock()` a second time just to
  snapshot task fields for a diagnostic log line. Each additional
  acquisition is another opportunity to be interrupted.
- The same path called `task_log_label`, which took
  `PROCESS_TABLE.lock()`. The same class of same-core ISR
  re-entry hazard applies to any global mutex held along the wake
  path. With the old design, a fix had to touch every such lock;
  with the new design, only the ISR-callable lock needs to be
  IRQ-safe.

## Detection

The bug was found by instrumenting the wake path at progressively
finer granularity until the hang fingerprint localised to the
first `SCHEDULER.lock()` acquisition inside `wake_task`. The
`[sched] stale-ready` warning, already present as a fairness
diagnostic, consistently showed ~1 s of staleness on core 0 at
the moment `net_task` on core 2 hit its `wake_task` hang. A failed
watchdog experiment that called `wake_task` from the timer ISR
produced an unambiguous same-core deadlock on tick counter
advancement within 200 ticks, confirming the ISR / task-context
lock collision mechanism.

## Resolution

`kernel/src/task/scheduler.rs`:

1. New private `IrqSafeMutex<T>` wrapper.

   ```rust
   pub(crate) struct IrqSafeMutex<T: ?Sized> {
       inner: Mutex<T>,
   }

   pub(crate) struct IrqSafeGuard<'a, T: ?Sized + 'a> {
       guard: spin::MutexGuard<'a, T>,
       _restore: InterruptRestore,
   }

   struct InterruptRestore {
       was_enabled: bool,
   }

   impl<T: ?Sized> IrqSafeMutex<T> {
       pub(crate) fn lock(&self) -> IrqSafeGuard<'_, T> {
           let was_enabled = interrupts::are_enabled();
           if was_enabled { interrupts::disable(); }
           let guard = self.inner.lock();
           IrqSafeGuard { guard, _restore: InterruptRestore { was_enabled } }
       }
   }
   ```

   Field declaration order is load-bearing. `guard` (the inner
   `spin::MutexGuard`) is declared first and drops first, releasing
   the spinlock. `_restore` drops second and re-enables interrupts
   only if they were enabled at lock time. An ISR arriving in the
   drop-window sees the lock already released; it cannot observe an
   "unlocked + IF still off" state with a stale `was_enabled`.

2. `pub(super) static SCHEDULER: Mutex<Scheduler>` →
   `pub(super) static SCHEDULER: IrqSafeMutex<Scheduler>`. All 62
   existing `SCHEDULER.lock()` call sites retain their textual form
   and automatically inherit interrupt masking.

3. `enqueue_to_core`'s body is wrapped in
   `interrupts::without_interrupts(…)`. The per-core
   `data.run_queue.lock()` is a separate `spin::Mutex`, and the
   brief IF-on window between dropping `SCHEDULER.lock` and
   acquiring `run_queue.lock` was reachable by an ISR that would
   then re-enter `run_queue.lock` via `wake_task →
   enqueue_to_core`. Wrapping the enqueue body closes that window.

4. `wake_task` takes `SCHEDULER.lock` exactly once. The redundant
   second acquisition (plus the re-entry into
   `PROCESS_TABLE.lock` via `task_log_label`) was folded into the
   first critical section. A new `label_from_name_only` helper
   replaces the PROCESS-TABLE-based pid lookup with a static match
   on `task.name` when `pid == 0`.

5. `kernel/src/task/mod.rs::try_lock_scheduler` changed signature
   from `Option<spin::MutexGuard<'static, scheduler::Scheduler>>`
   to `Option<scheduler::IrqSafeGuard<'static,
   scheduler::Scheduler>>` (used by `panic_diag`). The helper keeps
   its non-blocking semantics — `try_lock` returns `None` if the
   inner spinlock is held.

## Validation

No heavy-logging mitigation in either batch. `/tmp/h9_run_once.sh`
does a clean `cargo xtask run`, waits for
`sshd: listening on port 22`, then fires one
`ssh -o BatchMode=yes -p 2222 user@127.0.0.1 exit`.

| Batch | Size | Outcome | Clean rate |
|---|---|---|---|
| Pre-fix baseline | ~50 historical runs | dominant early-wedge | 30 – 40 % |
| Heavy-logging mitigation | 10 (h9run177–h9run186) | 8 clean / 2 early-wedge | ~80 % |
| Post-fix Phase 1 | 15 (irqfixA1 – irqfixA15) | 15 clean-auth-rejected | **100 %** |
| Post-fix Phase 2 | 60 (latewedgeA1 – latewedgeA60) | 60 clean-auth-rejected | **100 %** |

Per-run serial-log fingerprints across the 75 post-fix runs were
consistent: clean boot, 15 `[tcp-wake]` events per run (listener +
session socket + teardown), a single `[tcp] connection established
(passive)`, `sshd: accepted client fd=4 count=1`, `sshd: session
child pid=14 sock_fd=4`, and 6 – 7 early-boot `[sched] stale-ready`
entries (all at `ready_at_tick ≤ 2714`; none during or after the
SSH handshake).

At the pre-fix ~8 % "late-wedge" rate, the probability of a clean
60-run streak is ~0.7 %. The late-wedge hypothesis (distinct real
bug) is rejected; the observed pre-fix late-wedges were tail
mis-classifications of slow early-wedges that narrowly elapsed the
20 s client banner timeout.

## Lessons learned

- **ISR-callable globals must be IRQ-safe.** Any lock reachable
  from an interrupt handler must mask interrupts on its
  task-context holders, or the ISR path must avoid the lock
  entirely (e.g. via a per-core lock-free queue, as
  `notification::signal_irq` does). The kernel already had two of
  these patterns (`DRIVER.lock` via `without_interrupts`,
  `FRAME_ALLOCATOR.lock` via `with_frame_alloc_irq_safe`) but
  `SCHEDULER.lock` slipped through.
- **Matching patterns locally does not catch this class of bug.**
  `virtio_net_irq_handler` correctly wraps its own
  `DRIVER.lock`-taking callers in `without_interrupts`, but
  nothing warned that it was *also* calling `wake_task` from ISR
  context. The property "this ISR touches a global lock that
  another path holds with IF on" is a transitive property that
  only a whole-kernel lock-graph audit surfaces.
- **Log sensitivity is a fingerprint, not a fix.** "Adding
  `log::info!` makes the bug go away" is the classic fingerprint
  of a tight concurrency race, not a correctness story. The
  temptation to ship the logging as the fix was real; the 15/60
  post-fix validations confirmed the correct mechanism was
  identified.
- **Watchdog experiments are cheap oracles.** The failed
  timer-ISR-driven `wake_task` watchdog deadlocked within 200
  ticks — cheap, unambiguous confirmation of the ISR / task-
  context lock collision mechanism. Similar "deliberately
  contend this lock from ISR context and see whether it
  deadlocks" experiments are a good default for any
  ISR-reachable lock.
- **`pcap` on the QEMU netdev resolves external / internal
  confusion quickly.** Before the pcap, an earlier doc iteration
  blamed QEMU SLIRP for "dropping ACKs." The pcap showed every
  packet arriving, refocusing the investigation on the guest
  and saving an unknown amount of wasted scope.

## Action items

- [x] **Fix landed:** `IrqSafeMutex` wrapper + `SCHEDULER.lock`
  conversion + `enqueue_to_core` without_interrupts +
  `wake_task` single-lock path. Commit `ac37270`.
- [x] **Validation:** 15-run (Phase 1) and 60-run (Phase 2)
  clean-rate batches at 100 %. Commits `2c331ec`, `fd2c044`.
- [ ] **Global lock audit.** Inventory every `spin::Mutex` /
  `spin::RwLock` static in the kernel and classify each as
  ISR-callable or not. Any that reaches `wake_task` or any
  lock an ISR holds transitively should be converted to
  `IrqSafeMutex` or prove the no-ISR invariant in a doc comment.
  `PROCESS_TABLE`, `TCP_CONNS`, socket tables, pipe tables are
  the first candidates.
- [ ] **Remove branch-local debug instrumentation** added
  during the investigation (the `[h9-*]` traces in
  `userspace/sshd/src/session.rs` and
  `userspace/async-rt/src/executor.rs`, the `wake_task[h9]`
  and `sshd fork-child` logs, the `NET_WAKE_*` counters, and
  the `debug_sshd_fork_child` Task fields). The
  `IrqSafeMutex` primitive is the durable artifact; the
  diagnostics can go.
- [ ] **Regression harness.** `/tmp/h9_run_once.sh` and
  `/tmp/h9_batch.sh` live outside the tree. Promote both into
  `tests/` or `scripts/` (as a named regression so the class of
  bug is continuously guarded against) and wire into
  `cargo xtask test` when time permits.
- [ ] **Diagnostic comment on every ISR handler.** Each
  `extern "x86-interrupt" fn …_handler` should carry a short
  "ISR-safe invariant" comment enumerating what it's allowed to
  call. `virtio_net_irq_handler` and `virtio_blk_irq_handler`
  had no such invariant; adding it would have caught the call
  to `wake_task` during review.

## Related

- `kernel/src/task/scheduler.rs` — the fix. See
  `IrqSafeMutex` definition and the `wake_task` refactor.
- `docs/appendix/scheduler-fairness-regression.md` — full
  multi-session investigation log. Preserved as historical record
  of the nine-hypothesis search; this post-mortem is the
  authoritative short summary.
- `docs/appendix/scheduler-fairness-h9-resume.md` — closeout
  note / reproduction harness pointer. Kept as a short
  "where-to-look" index for a future regression of the same
  class.
