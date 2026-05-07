# Post-mortem: Phase 57e (full kernel-mode preemption) deferred

**Incident:** Phase 57e introduced timer-driven kernel-mode preemption
(`CONFIG_PREEMPT`-equivalent). After 18 sessions of debugging across
13 distinct bugs, the feature could not be made lag-free on real
hardware without effectively reverting to voluntary mode's
behaviour.
**Status:** Deferred 2026-05-07. Phase reduced in scope to "voluntary
kernel preemption with cross-core IPI fast-path"; the timer-driven
kernel-mode preemption code path is removed and the `preempt-full`
feature flag is retired.
**Severity:** Medium — the affected feature was opt-in and not on
the default build path. Real-hardware interactive use under
`preempt-full` was visibly laggier than `preempt-voluntary`; QEMU
TCG hid most of the regression because of its deterministic vCPU
serialisation.
**Owners:** Kernel (scheduler, IRQ subsystem, IPC).
**Resolution commits:** `8b44442`, `549584f`, `9c39291`, `052010a`,
`a1bfe17`, `eb1f13d`, `d5fad05` (followed by feature-flag removal).
**Doc commits:** this file, the Phase 57e roadmap update, the handoff
doc header.
**Related:** `docs/handoffs/57e-preempt-full-userspace-hangs.md` (the
2669-line bug-analysis log — preserved as historical reference and
still load-bearing if Option 3 below is ever attempted).

## Summary

Phase 57e set out to upgrade m3OS from
`PREEMPT_VOLUNTARY`-equivalent kernel preemption (Phase 57d) to
`PREEMPT_FULL`-equivalent (Linux's `CONFIG_PREEMPT`). The headline
behaviour was a `check_and_preempt_kernel` predicate inside the
timer ISR and the reschedule-IPI handler that, when
`preempt_count == 0` and `reschedule == true`, would preempt a
kernel-mode task and dispatch the scheduler.

The implementation surfaced 13 distinct bugs over 18 debugging
sessions:

* Bug #1–5 (boot crashes during early bring-up).
* Bug #6 (eager-yield zero-cross synchronous-yield from
  `preempt_enable` deadlocking the BSP main loop).
* Bug #7 (frame UAF / PML4[256] corruption traced to slab UAF).
* Bug #8.1 (lost-wakeup cascade — missing waker registration in
  `block_current_on_{recv,send,notif}_v2`).
* Bug #8.2 (slow-boot timeout knock-on of #8.1).
* Bug #9 (scheduling-fairness — preempt_count leak around FS-volume
  mutexes held across `block_current_until`).
* Bug #10 (sporadic Doom-launch kernel-mode GPF; one observation,
  not reproduced; not blocking — remains open as a separate bug).
* Bug #11 (real-hardware GUI regression — `init_task` halt loop
  starving BSP-resident services under voluntary mode).
* Bug #12 (real-hardware mouse / keyboard input lag under
  preempt-full; **the bug that ultimately forced the deferral**).
* Bug #13 (waitpid lost-wake under deferred-yield experiment;
  surfaced as a hypothetical race that the doc author traced through
  three plausible structural fixes; we shipped one of them
  defensively).

After landing fixes for Bugs #6–#13, the residual lag on real
hardware was traced (in this post-mortem cycle) to two structural
causes that are not really bugs at all but inherent to the design:

1. **Timer-driven kernel-mode preemption fires unconditionally on
   every 1 ms tick** because `timer_handler_kernel` calls
   `signal_reschedule()` on every tick — `check_and_preempt_kernel`
   then sees `reschedule == true` and preempts whatever kernel-mode
   task is running, regardless of whether there is an actual wake
   event. The input pipeline's typically-microsecond syscalls are
   forced through unnecessary mid-syscall context switches at every
   1 ms boundary, surfacing as user-visible input lag that voluntary
   mode (which has no `check_and_preempt_kernel`) does not produce.
2. **A naive quantum threshold (e.g. 4 ms minimum-granularity)
   makes the lag worse, not better,** because it delays the
   *wakee*: a genuine cross-core wake to a kernel-mode task has to
   wait up to the quantum for the running task to be preempted past
   it. Hardware tests under a 4 ms quantum showed `mouse_server`'s
   outbound queue overflowing 4–7 × more events than under no
   quantum.

Removing timer-driven kernel-mode preemption restored voluntary
mode's interactive responsiveness. With it removed, the
`preempt-full` feature does no work that voluntary doesn't already
do, and the feature flag becomes a misnomer.

## Impact

* `preempt-full` builds on real hardware (`omarchy` test machine)
  exhibited user-visible mouse and keyboard input lag during
  interactive use, while voluntary builds on the same hardware were
  lag-free.
* QEMU TCG soak runs hid the regression because of TCG's
  serialised vCPU execution model — a different concurrency shape
  than real cores. This was the proximate reason the bug only
  surfaced under hardware testing.
* Time cost: 18 debugging sessions, 2669 lines of handoff doc, 13
  filed bugs, multiple revert cycles. Each fix landed surfaced
  another bug; root-cause hypotheses had to be revised three times.

## Timeline (condensed)

* **Phase 57e spec landed** (`docs/roadmap/57e-full-kernel-preemption.md`):
  set the headline goal of `CONFIG_PREEMPT`-equivalent
  kernel-mode preemption.
* **Sessions 1–11** (preempt-full bring-up): Bugs #1–#8.1 found
  and closed — boot crashes, lost-wakeups, slow-boot, frame UAF.
  Tracks A through F mostly clean.
* **Session 12 (24 h soak)**: Bug #9 (scheduling fairness)
  surfaces.
* **Session 13–14**: Bug #9 mechanism identified as
  preempt_count leak around `IrqSafeMutex` guards held across
  `block_current_until`.
* **Session 15**: Bug #9 partial fix lands (`sys_mmap_file_backed`
  releases page-table lock before disk I/O); 2nd-pass attempt
  (`spin::Mutex` swap) reverted after triggering Bug #10
  (sporadic Doom GPF on real hardware).
* **Session 16**: Bug #11 (`init_task` halt loop starving BSP
  under voluntary mode regression) closed; Bug #12 (input lag on
  real hardware) opens.
* **Session 17–18**: IPC bracket shrink, stdin busy-yield fix
  (`538e650`), eager-yield removal attempts. Two attempts
  reverted: Bug #12 part 2 (`cefa7fb`, login crash) and Bug #12
  part 3 (`4c1c552`, surfaced Bug #13). 1 s `sys_waitpid`
  deadline backstop landed (`9c39291`); wake-side
  `preempt_disable` bracket on `wake_child_waiters` landed
  (`549584f`). Eager-yield removal re-applied (`8b44442`); init
  reap-loop busy-yield replaced with 50 ms sleep (`052010a`).
* **Session 19 (this cycle)**: hardware tests confirm lag still
  present after every kernel-side fix landed. 4 ms quantum
  (`17099f6`) makes it strictly worse and is reverted (`5ff8a35`).
  Drop timer-driven kernel-mode preemption entirely (`a1bfe17`)
  — input lag closes but black-screens on hardware because BSP's
  `enable_and_hlt` loop has no path back to scheduler. Idle-task
  `yield_now`-after-hlt fixup lands (`eb1f13d`); regresses
  voluntary mode (1 ms latency on every same-core wake on BSP)
  by adding `hlt` to `init_task`. `init_task` `hlt` removed
  (`d5fad05`) — voluntary lag-free, preempt-full equivalent.
  Decision to defer the phase made.

## Root cause

Two structural drivers, not a single bug:

### Driver 1 — `signal_reschedule()` semantics

`timer_handler_kernel` calls `crate::task::signal_reschedule()`
unconditionally on every 1 ms timer tick (kernel/src/arch/x86_64/interrupts.rs:1665).
That function sets `reschedule = true` on the local core. The
purpose under voluntary is to signal "rotate at the next user-mode
return boundary" — voluntary has no kernel-mode preempt path, so
the flag sits until consumed by `check_and_preempt_user` after the
task transitions to user mode.

Under preempt-full, the same flag is consumed by
`check_and_preempt_kernel` *on the same tick*. Net effect:
**every kernel-mode task is preempted on every 1 ms boundary**,
regardless of whether any wake event actually fired. For our
input pipeline (kbd_server → display_server → term → ion), where
typical syscalls are microseconds, this means an unnecessary
mid-syscall context switch at every quantum — strictly more work
than voluntary mode does, with no compensating benefit.

The `signal_reschedule()` call cannot simply be removed from
`timer_handler_kernel` without breaking the user-mode-return
boundary's flag-consumption invariant.

### Driver 2 — quantum threshold delays the wrong side

A naive fix — "only preempt if the kernel-mode task has run for at
least N ms" — was attempted at N = 4 ms (Linux CFS
`sched_min_granularity_ns` default). It made the lag strictly
worse, not better. Reason: it delays the **wakee** waiting for the
**waker** to be preempted past its quantum. A genuine cross-core
wake to a kernel-mode task running on a busy core had to wait up
to 4 ms for the next preempt point — the input pipeline's
mouse-event queue overflowed 4–7 × more events than under a 1 ms
preempt rate. The same threshold pattern that bounds kernel-mode
hogs in CFS pessimises wake-delivery latency for short syscalls.

### Why microkernels don't need it

m3OS is a microkernel. The "1 ms syscall blocks other tasks"
problem `CONFIG_PREEMPT` solves in a monolithic kernel does not
really exist in our kernel:

* Filesystems run in `vfs_server` (userspace).
* Block / network drivers run in userspace.
* Heavy compositing runs in `display_server` (userspace).
* Kernel-mode work is mostly capability checks, IPC routing,
  page-fault dispatch, syscall return — microsecond operations
  that yield naturally via `block_current_until` / `switch_context`
  on every IPC block, deadline sleep, or scheduler block.

Linux supports `CONFIG_PREEMPT_NONE`, `_VOLUNTARY`, `_PREEMPT`,
`_RT`, and `_DYNAMIC` precisely because the cost-benefit varies by
workload. Most desktop/server distributions ship `_NONE` or
`_VOLUNTARY`; `_PREEMPT` is for low-latency workstations and
`_RT` for hard real-time. Microkernels typically run cooperatively
inside the kernel and time-slice in userspace — Redox follows
this pattern as far as we can tell. The microkernel argument for
*not* having full kernel preemption is that you've already moved
the work that would have benefited.

## Detection

* **Real-hardware testing (`omarchy` test machine).** QEMU TCG soak
  runs were the validation gate before Session 15; they passed
  10 / 10 throughout most of Bug #12's life because TCG's
  serialised vCPU execution masked the SMP wake-race shapes.
  Real-hardware testing was the only signal that surfaced Bug #12
  and, later, Bug #11 (the voluntary-mode `init_task` halt
  regression). Adding real-hardware smoke as a closure gate is the
  primary process lesson.
* **`[yield-sample]` sampled-log instrumentation** (`7aed426`,
  removed in this cycle) was the diagnostic that pointed at the
  dominant remaining yield sources after each fix landed. Without
  it the per-fix re-test would have been blind.
* **Per-task `preempt-trace` ring** (added in `568e5f6`, retained)
  captured the exact preempt_count cycle that exposed Bug #9.

## Resolution

Three categories of work landed across the 57e effort. We are
keeping the first two and removing the third.

### Keep: SMP discipline infrastructure (still load-bearing)

* `preempt_count` per-task counter and per-core
  `current_preempt_count_ptr` switch-out / switch-in retarget
  (Phase 57b C.1 / C.2 / C.3). Required by the `IrqSafeMutex` F.1
  wiring regardless of preemption model.
* `IrqSafeMutex` F.1 wiring — `preempt_disable` on `lock`,
  `preempt_enable` on `Drop`. Protects against same-core ISR
  re-entry and (under preempt-full) timer-driven kernel-mode
  preemption mid-critical-section. Useful even without preempt-full
  because the SAME-CORE ISR-re-entry case still applies.
* `preempt_enable` deferred-reschedule semantics (`preempt_resched_pending`
  flag, consumed at user-mode return boundary).
* `block_current_until` with absolute tick deadline; `wake_task_v2`
  with pi_lock + `on_cpu` cross-core spin-wait; `enqueue_to_core`
  with cross-core IPI. The wake protocol (Phase 57a Track C/D) is
  preempt-model-independent.

### Keep: defensive race-shape closures

* The `preempt_disable` / `preempt_enable` IPC brackets in
  `kernel/src/ipc/endpoint.rs` (Bug #6 / #8.1, now also #12 part 4
  applies to all bracket exits). Protects against same-core wake
  races on bracket exit.
* The `preempt_disable` / `preempt_enable` wake bracket on
  `wake_child_waiters` in `kernel/src/process/mod.rs` (Bug #13
  Option B, `549584f`).
* The 1 s defensive deadline backstop on `sys_waitpid`'s
  `block_current_until` (`9c39291`).
* The `init_task` reap-loop sleep (50 ms via `nanosleep_for(0,
  50_000_000)` instead of busy-yield, `052010a`).
* The `stdin_feeder` waitqueue block instead of busy-yield
  (`538e650`).

These are independent of the preemption model. Each closes a real
race shape that surfaced under preempt-full's eager-yield but is
latently present under any concurrent-wake scenario; keeping them
hardens the kernel against analogous races in future paths.

### Remove: timer-driven kernel-mode preemption

* `check_and_preempt_kernel` (the IRQ-side preempt gate).
* `timer_handler_kernel`'s `cfg(preempt-full)` call to it.
* `reschedule_ipi_handler_kernel`'s same call (was the cross-core
  IPI delivery fast path; now redundant because we no longer
  preempt kernel-mode at all).
* `dispatch_preempted_and_resume_kernel` and the kernel-mode
  preempt-frame return path.
* `preempt_to_scheduler_kernel` (kernel-mode preempt entry).
* `kernel_preempt_watchdog` (Track D.3 held-lock watchdog) — only
  consumer was `check_and_preempt_kernel`.
* The `preempt-full` Cargo feature flag, `preempt-full = ["preempt-voluntary"]`
  in `kernel/Cargo.toml`, and every `cfg(feature = "preempt-full")`
  block in the kernel tree.
* The kernel-mode `PreemptTrapFrameKernel` IRQ-entry stub paths
  that build a kernel-mode preempt frame (the matching user-mode
  paths stay).

### Keep, but unused for now: voluntary's contract

The `preempt-voluntary` feature flag, `cfg(feature =
"preempt-voluntary")` blocks, the `preempt_resched_pending` flag,
and the `assert_preempt_count_zero_at_user_return` invariant
remain. This is now the only preemption model — but the cfg-gate
documents the contract and gives a clean place to add
`cond_resched`-style explicit yield points later (see Future Work
below) without re-introducing a feature flag.

## Validation

* `cargo xtask check` green on `feat/phase-57e-full-preemption` at
  HEAD post-cleanup.
* Voluntary-mode boot on real hardware (`omarchy`) confirmed
  lag-free interactive use (mouse responsive, keyboard echo
  immediate, no `BlockedOnWait stuck-since=` warnings, no `outbound
  queue full` overflows from `display_server` / `mouse_server`).
* `preempt-full`-mode boot — N/A after this commit; the feature
  flag is removed.
* QEMU smoke / soak / regression: green at every step of the
  cleanup.

## Lessons learned

### Architectural

1. **Microkernels don't need full kernel preemption.** The
   benefit `CONFIG_PREEMPT` provides — bounding latency from
   long-running kernel paths — is targeted at monolithic kernels
   where filesystems, drivers, and network stacks live in kernel
   mode. m3OS already moved that work to userspace. Adding
   timer-driven kernel preemption was solving a problem we don't
   have, while paying its overhead on every tick.
2. **Preemption rate ≠ wake-delivery latency.** A naive intuition
   ("preempting more often delivers wakes faster") collapses under
   the asymmetric preempt-the-waker / wait-for-the-wakee model
   that timer-driven kernel preemption forces. Cross-core IPI
   delivery is the right primitive for low-latency wake delivery;
   timer-driven kernel preemption pessimises it.
3. **The preempt-discipline work survives the preemption-model
   change.** `preempt_count`, `IrqSafeMutex` F.1 wiring, the wake
   brackets, and the deferred-reschedule semantics are all
   preempt-model-independent. The 18-session bug saga produced
   real, lasting hardening of the SMP discipline — even though
   the headline feature is being deferred.

### Process

4. **Real-hardware testing has to be a gate, not an
   afterthought.** QEMU TCG's deterministic vCPU serialisation
   masked Bug #12 (and would have masked Bug #11) for the entire
   QEMU-only soak window. Hardware soak should have been a
   per-Track gate from Phase 57e's start, not a post-track
   verification.
5. **Sampled-log instrumentation pays for itself.** The
   `[yield-sample]` instrumentation (`7aed426`) was the diagnostic
   that pointed at each successive dominant yield source after
   every fix landed. Per-fix iteration without it would have been
   blind. Same lesson applies to the per-task `preempt-trace`
   ring.
6. **Three structural-fix hypotheses with no proof = trial and
   error.** The Bug #13 fix had three plausible structural fixes
   (deadline backstop, wake-side preempt bracket, notification
   object). The doc author wrote "almost certainly one of"
   without proving which. We landed (a) and (b) defensively as
   belt-and-suspenders. In hindsight, identifying the *exact*
   race before fixing it would have saved another cycle. The
   trade-off is real — sometimes you ship the safety net and move
   on — but flagging the uncertainty in the doc was the right call.

## Action items

* [x] Update `docs/roadmap/57e-full-kernel-preemption.md` to
  **Status: Deferred**, link to this post-mortem, and explicitly
  note that the SMP discipline infrastructure (Phase 57b
  preempt_count + IrqSafeMutex F.1 wiring) survives.
* [x] Update `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md`
  similarly. Mark unfinished tracks as Deferred. Retain Tracks G
  (Bug #9 FS-mutex fairness) and D.3 partial value as separate
  follow-ups.
* [x] Add a header to `docs/handoffs/57e-preempt-full-userspace-hangs.md`
  pointing to this post-mortem and noting the outcome. Do not
  trim the body — the bug-analysis is goldmine for any future
  Option 3 (cond_resched) attempt.
* [x] Remove the `[yield-sample]` instrumentation
  (`task::yield_now` sampled-log block in `kernel/src/task/scheduler.rs`).
  It was a 57e debugging aid and the busy-yield idle pattern
  amplifies it into thousands of log lines per session.
* [x] Remove the `preempt-full` Cargo feature flag and every
  `cfg(feature = "preempt-full")` site in the kernel. Remove the
  dead code those gates wrap (`check_and_preempt_kernel`,
  `kernel_preempt_watchdog`, `preempt_to_scheduler_kernel`,
  `dispatch_preempted_and_resume_kernel`,
  `PreemptTrapFrameKernel`, the kernel-mode IRQ entry stubs).
* [ ] Make real-hardware smoke a per-track gate in future phases.
  Concrete proposal: every track that touches the scheduler, IPC
  protocol, or wake protocol must include a "real-hardware GUI
  smoke" acceptance gate before track closure. (Track for
  inclusion in the phase-doc template.)
* [ ] Track G (Bug #9 FS-mutex fairness — Option B Arc-clone
  refactor) remains open as a separate follow-up. Independent of
  57e disposition.
* [ ] Bug #10 (sporadic Doom-launch kernel-mode GPF, one
  observation, not reproduced) remains open. Independent of 57e
  disposition.

## Future work

If a future m3OS workload genuinely needs lower kernel-mode
latency — for example, a hard-real-time audio path or a kernel-
hosted graphics driver — the right next step is **`cond_resched`-style
explicit yield points** (Linux's `PREEMPT_VOLUNTARY` mechanism, not
its `CONFIG_PREEMPT`):

1. Identify the specific kernel paths that legitimately run
   uninterrupted for >1 ms (page fault handling, large-buffer
   `copy_from_user` / `copy_to_user`, fork's page-table copy,
   exec's binary load).
2. Insert `task::cond_resched()` calls at safe points inside those
   paths — points where preempt_count == 0, no spin lock is held,
   and the task is in a state where re-dispatch is safe.
3. The `preempt-voluntary` feature flag and the
   `preempt_resched_pending` infrastructure are the foundation for
   this approach; both are retained from the 57e cleanup.

This is the architecturally-honest extension path. It costs zero
unconditional overhead (no per-tick preempt check), bounds latency
at the granularity the workload requires, and is the path Linux
ships in `PREEMPT_VOLUNTARY` desktops. Under this model the
"preempt-full" name returns as a misleading legacy term — a
dedicated phase document for the cond_resched work would set its
own scope.

## Related

* `docs/roadmap/57e-full-kernel-preemption.md` — phase design
  doc, marked Deferred in the same commit cycle as this
  post-mortem.
* `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md` —
  task list, marked Deferred where applicable.
* `docs/handoffs/57e-preempt-full-userspace-hangs.md` — 18-session
  bug-analysis log, retained as historical reference.
* `docs/handoffs/57e-preempt-full-boot-crash.md` — Bugs #1–#5
  log.
* `docs/handoffs/57e-kernel-preempt-audit.md` — preempt-discipline
  callsite audit.
* `docs/handoffs/57e-dispatch-reentrancy.md` — dispatch path
  re-entrancy audit.
* `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md` —
  pre-57e scheduler lock fix (the IrqSafeMutex foundation).
* `docs/post-mortems/2026-04-24-ingress-task-starvation.md` —
  pre-57e PID 1 starvation fix.
