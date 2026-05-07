# Phase 57e — Full Kernel Preemption (PREEMPT_FULL)

**Status:** **Deferred (2026-05-07)** — see [post-mortem](../post-mortems/2026-05-07-57e-preempt-full-deferred.md). Phase reduced in scope to "voluntary kernel preemption with cross-core IPI fast-path" (the actual outcome). The timer-driven kernel-mode preemption code path is removed and the `preempt-full` Cargo feature flag is retired. The SMP discipline infrastructure produced during the 57e cycle (`preempt_count` per-task counter, `IrqSafeMutex` F.1 wiring, the wake-bracket race-shape closures in `endpoint.rs` / `wake_child_waiters`, the `sys_waitpid` 1 s deadline backstop) survives because it is preempt-model-independent. **Original goals retained below for historical context.**
**Source Ref:** phase-57e
**Depends on:** Phase 57b (Preemption Foundation) ✅, Phase 57c (Kernel Busy-Wait Audit and Conversion) ✅, Phase 57d (Voluntary Preemption) — functional ✅; gates I.2 (24-hour post-flip soak) and I.3 (`preempt-voluntary` flag removal) must close before 57e starts (see task-list Track 0).
**Builds on:** Drops the `from_user` check from the IRQ-return preemption point introduced in 57d.  Once dropped, every kernel-mode IRQ-return becomes a potential preemption point — and the 57b `preempt_count` discipline becomes load-bearing for kernel-mode safety, not just user-mode.
**Primary Components:** `kernel/src/arch/x86_64/interrupts.rs` (replace 57d's early-return in `timer_handler_kernel` / `reschedule_ipi_handler_kernel` with the same preempt check the user handlers run; extend the existing `global_asm!` block with `preempt_resume_to_kernel` and `dispatch_preempted_and_resume_kernel`), `kernel/src/task/scheduler.rs` (`preempt_to_scheduler_kernel` Rust shim, kernel-mode preempt invariants, kernel-mode `preempt_enable` immediate zero-crossing with IF-enabled gate, FXSAVE→XSAVE migration in `save_fpu_state`/`restore_fpu_state`), `kernel/src/arch/x86_64/cpuid.rs` (XSAVE feature detection), `kernel/src/smp/boot.rs` and `kernel/src/main.rs` (CR4.OSXSAVE + XCR0 wiring on BSP and APs).

## Outcome (2026-05-07 deferral)

Phase 57e is **deferred** after 18 debugging sessions and 13 distinct bugs. Real-hardware testing on the `omarchy` machine confirmed that timer-driven kernel-mode preemption — the headline behaviour the phase set out to add — could not be made lag-free without effectively reverting to voluntary mode's behaviour. The structural reasons are documented in detail in the [post-mortem](../post-mortems/2026-05-07-57e-preempt-full-deferred.md); two summaries:

1. `timer_handler_kernel` calls `signal_reschedule()` unconditionally on every 1 ms tick. Under voluntary the flag is consumed at the user-mode-return boundary; under preempt-full `check_and_preempt_kernel` consumed it on the same tick, preempting every kernel-mode task at every 1 ms boundary regardless of whether any wake event actually fired. For a microkernel with microsecond-scale syscalls, this is unconditional overhead for no benefit.
2. A naive quantum threshold makes the lag worse, not better, because it delays the wakee waiting for the waker to be preempted past its quantum. Cross-core IPI delivery is the right primitive for low-latency wake delivery; timer-driven kernel-mode preemption pessimises it.

Microkernels generally don't need full kernel preemption — the work `CONFIG_PREEMPT` bounds in monolithic kernels (filesystem, drivers, network stack) is already in userspace servers in m3OS. Linux supports `CONFIG_PREEMPT_NONE`, `_VOLUNTARY`, `_PREEMPT`, `_RT`, and `_DYNAMIC` precisely because the cost-benefit varies by workload; most distributions ship `_NONE` or `_VOLUNTARY`. Redox follows the same cooperative-kernel pattern.

**Surviving from the 57e cycle** (preempt-model-independent, retained):
- `preempt_count` per-task counter and per-core retarget infrastructure (Phase 57b C.1 / C.2 / C.3).
- `IrqSafeMutex` F.1 wiring (preempt_disable on lock, preempt_enable on Drop).
- `preempt_enable` deferred-reschedule semantics (`preempt_resched_pending` flag).
- `block_current_until` with absolute tick deadline; `wake_task_v2` with pi_lock + on_cpu cross-core spin-wait; `enqueue_to_core` with cross-core IPI.
- IPC bracket exits in `kernel/src/ipc/endpoint.rs` (Bug #6 / #8.1 / #12 part 4).
- Wake bracket on `wake_child_waiters` (Bug #13, `549584f`).
- 1 s `sys_waitpid` deadline backstop (Bug #13, `9c39291`).
- `init_task` 50 ms reap-loop sleep (Bug #12, `052010a`).
- `stdin_feeder` waitqueue block (Bug #12, `538e650`).

**Removed in the 2026-05-07 cleanup**:
- `check_and_preempt_kernel` and its callers in `timer_handler_kernel` / `reschedule_ipi_handler_kernel`.
- `preempt_to_scheduler_kernel`, `dispatch_preempted_and_resume_kernel`, kernel-mode preempt-frame plumbing.
- `kernel_preempt_watchdog` (Track D.3).
- `preempt-full` Cargo feature flag and every `cfg(feature = "preempt-full")` site.
- `[yield-sample]` instrumentation (was Bug #12 debugging aid, now noise).

**Future work**: if a future workload needs lower kernel-mode latency, the architecturally-honest path is `cond_resched`-style explicit yield points (Linux's `PREEMPT_VOLUNTARY` mechanism, not its `CONFIG_PREEMPT`). The `preempt-voluntary` flag and `preempt_resched_pending` infrastructure are the foundation; both are retained from this cleanup. See post-mortem § Future Work.

---

**Original phase goals (preserved for historical reference; not applicable):**


## Milestone Goal

Kernel-mode code becomes preemptible at any point where `preempt_count == 0`.  Latency improves **per trigger path**, not uniformly:

- **Cross-core reschedule-IPI wakeup** improves to IRQ-handler runtime (~µs) because the receiver core, even if running kernel-mode, is now interrupted and switched.
- **`preempt_enable` zero-crossing** in a kernel-mode preempt-safe context fires the scheduler immediately (~µs) instead of recording the deferred-trigger and waiting for the next user-mode return.
- **Same-core wakeup** still relies on the next timer tick, voluntary yield, or local `preempt_enable` zero-crossing — `PREEMPT_FULL` does not add a self-IPI; this path is benchmarked separately and must not regress.
- **Timer-only preemption** of a kernel-mode CPU loop still fires at the next timer tick (~1 ms) — the same bound as 57d's user-mode-only preemption, now extended to kernel mode.

Every previously-bounded busy-spin in the kernel either remains bounded (preempt-disable wrapped) or becomes preemptible (the holder may be paused mid-spin and the spinner makes no progress until the holder resumes — but the spinner does not block forward progress on its core).

**This is the stretch goal of the 57b/c/d/e programme.**  After 57b/57c/57d, m3OS is at `PREEMPT_VOLUNTARY` parity with Linux's desktop default — a credible plateau, and the realistic 1.0 release target.  57e is the upgrade to `PREEMPT_FULL` (Linux's "low-latency desktop" or "real-time" config), which trades debuggability for latency.  Whether to land 57e depends on m3OS's release goals and the soak data from 57c/57d.

## Why This Phase Exists

After 57c removes kernel-mode CPU-monopoly bugs and 57d adds user-mode preemption, the residual gap is **kernel-mode latency**: a syscall handler that takes 1 ms (e.g., a buddy-allocator coalesce, a TLB shootdown wait, a virtio-blk request submission) blocks every other task on its core for that millisecond.  Most workloads will not notice — 1 ms latency is negligible — but interactive workloads (audio, real-time input) and benchmarks that measure round-trip IPC will.

The fix is to drop the `from_user` check from 57d's preemption point.  Once dropped, the timer IRQ can preempt kernel-mode code, switch to the scheduler, and run another task.  The preempted kernel-mode task resumes via `iretq` from its `preempt_frame` — the same mechanism 57d uses for user-mode tasks.

The phase is **conceptually small** but **carries real risk**:

- Every `preempt_disable` / `preempt_enable` callsite must be correct.  A missed `preempt_disable` around a kernel spinlock means an IRQ that fires while the lock is held will preempt the holder; the runnable task that gets dispatched may try to take the same lock and deadlock the core.
- Every previously-bounded kernel busy-spin must be wrapped in `preempt_disable` (a 57c annotation that becomes load-bearing here).
- The `pick_next` and dispatch paths must be re-audited for re-entrancy: a preemption point can fire during dispatch, so dispatch itself must be `preempt_disable`-wrapped at the right boundaries.

This is why the design notes recommend deferring 57e until 57c/57d have been running clean for at least a release cycle.

## Learning Goals

- How dropping a single conditional in the IRQ handler converts a "user-mode preemption" model to a "full kernel preemption" model — and what invariants must hold across every kernel codepath for the change to be safe.
- Why `preempt_disable` correctness is the gate: every spin-on-condition where the holder may be on a different core must hold the spinner's `preempt_count > 0` so the spinner is not preempted while the holder is also preempted (a livelock).
- How the per-CPU runqueue model (deferred from 57b) becomes more important under `PREEMPT_FULL`: a preempted kernel-mode task may need to migrate cores during dispatch, and global-lock contention becomes a measurable bottleneck.
- Why latency improvements are per-trigger rather than uniform: cross-core reschedule-IPI wakeup and safe `preempt_enable` zero-crossing paths can drop into the microsecond range under `PREEMPT_FULL` because they fire an immediate switch when kernel mode is preemptible; same-core wakeups remain bounded by the next timer tick (no self-IPI exists), and timer-only preemption is naturally tick-bounded.
- How to incrementally validate `PREEMPT_FULL`: enable the flag, run the regression suite, soak for 24 hours, then enable it for a release.

## Learning Outcomes (post-deferral, 2026-05-07)

After 18 debugging sessions and 13 distinct bugs, the implementation reached a plateau where every additional fix either reverted a previous fix or surfaced a new bug elsewhere. The deferral decision (and the path chosen instead) come down to four discoveries that the original Learning Goals did not anticipate. They are the durable lessons from this phase, independent of whether the feature is ever re-attempted.

### What we tried

The full implementation that PR #136 landed (Tracks A through F + J, behind `preempt-full`):

- Timer-driven kernel-mode preemption via `check_and_preempt_kernel`, called from `timer_handler_kernel` and `reschedule_ipi_handler_kernel`.
- Eager-yield zero-crossing branch in `preempt_enable` so a wake setting `reschedule == true` fired a synchronous `yield_now` immediately if `preempt_count` reached zero with `IF == 1`.
- Kernel-mode preempt-frame plumbing (`PreemptTrapFrameKernel`, `preempt_resume_to_kernel`, `dispatch_preempted_and_resume_kernel`) so a preempted kernel-mode task could resume via same-CPL `iretq` from a saved register frame.
- `kernel_preempt_watchdog` (Track D.3) intended to suppress preemption when a tracked scheduler-context lock was held — registry empty by design, since the canonical scheduler-context locks were already `IrqSafeMutex` (which raises `preempt_count`).

After PR #136 landed, debugging Bugs #6–#13 added eight further fixes (the IPC bracket exits in `endpoint.rs`, the `wake_child_waiters` wake bracket, the `sys_waitpid` 1 s deadline backstop, the `init_task` 50 ms reap-loop sleep, the `stdin_feeder` waitqueue block, the eager-yield removal, the init/idle `yield_now`-after-`hlt` fixup, and the 4 ms quantum experiment that was reverted within the same session).

### Why it didn't work — four discoveries

1. **`signal_reschedule()` is called unconditionally on every timer tick.** This was deliberate under voluntary mode (the flag sits until consumed at the user-mode return boundary). Under preempt-full it meant `check_and_preempt_kernel` consumed the flag *on the same tick*, preempting every kernel-mode task at every 1 ms boundary regardless of whether any wake event actually fired. The input pipeline's typically-microsecond syscalls were forced through unnecessary mid-syscall context switches at every quantum — strictly more work than voluntary mode does, with no compensating benefit. This was not a bug in the implementation; it was an architectural mismatch between the way the rest of the kernel uses the flag (rotation hint at the user-mode boundary) and the way preempt-full was reading it (immediate-preempt request).

2. **A naive quantum threshold delays the wakee, not the waker.** The first reaction to discovery #1 was to add a minimum-granularity floor (4 ms, matching Linux CFS `sched_min_granularity_ns`): only preempt a kernel-mode task if it has run for at least the quantum. This made the lag *worse*. A genuine cross-core wake to a kernel-mode task running on a busy core had to wait up to the quantum for the running task to be preempted past it; `mouse_server`'s outbound queue overflowed 4–7 × more events than under no quantum (mdrops jumped from 24-38 to 166 in the test session). The same threshold pattern that bounds kernel-mode hogs in CFS pessimises wake-delivery latency for short syscalls. CFS's design assumption — long syscalls are common, short ones are rare — is inverted in a microkernel.

3. **Microkernels move the work `CONFIG_PREEMPT` was designed to bound.** `CONFIG_PREEMPT` exists in Linux because filesystems, drivers, and network stacks live in kernel mode and routinely take 1 ms+ in syscall handlers (page-table walks, lock acquisition over disk I/O, complex memory reclaim). m3OS already moved that work to userspace servers (`vfs_server`, `display_server`, `audio_server`, the userspace driver-host). Kernel-mode work in m3OS is mostly capability checks, IPC routing, page-fault dispatch, syscall return — microsecond operations that yield naturally. The "1 ms syscall blocks other tasks" problem `CONFIG_PREEMPT` solves is a problem the rest of the m3OS architecture was designed to *not have*. Adding `CONFIG_PREEMPT` was solving a problem we don't have, while paying its overhead on every tick.

4. **QEMU TCG masked the regression for thirteen sessions.** The 24-hour soak passed under TCG for most of Bug #12's life. TCG's serialised vCPU execution is a different concurrency shape than real cores — wake-race interleavings that would have surfaced on hardware did not surface in TCG. Bug #11 and Bug #12 both required real-hardware testing on the `omarchy` test machine to surface. The implication for any future preemption-model phase: real-hardware soak must be a per-track gate, not a post-merge verification.

### Why this was the wrong path

The four discoveries above compose into the architectural conclusion: **timer-driven kernel-mode preemption is a poor fit for a microkernel**. The benefit it provides (bounding latency from long-running kernel paths) is a benefit aimed at monolithic kernels where the long-running paths are unavoidable. m3OS has very few of those paths — and the ones it has (mostly mmap of file-backed regions over disk I/O, fork's page-table copy, exec's binary load) are concrete, narrow, and addressable individually rather than via a global preemption model.

Linux supports `CONFIG_PREEMPT_NONE`, `_VOLUNTARY`, `_PREEMPT`, `_RT`, and `_DYNAMIC` precisely because the cost-benefit varies by workload. Most desktop/server distributions ship `_NONE` (RHEL server) or `_VOLUNTARY` (Ubuntu/Fedora desktop). `CONFIG_PREEMPT` is for low-latency desktop kernels and `PREEMPT_RT` is for hard real-time. As far as we can tell, Redox follows the same cooperative-kernel pattern microkernels typically do. The microkernel argument for *not* having full kernel preemption is that the work that would have benefited has already been moved; the implementation cost remains, but the value does not.

The 18-session implementation cycle was not wasted. It produced real, lasting hardening of the SMP discipline that survives the deferral. But the headline goal — kernel-mode preemption mid-syscall on a 1 ms timer — was always going to be paying for capacity m3OS does not need.

### What we are doing instead

**Today (the 2026-05-07 cleanup state):** voluntary kernel preemption with cross-core IPI fast-path. User-mode tasks are preempted at the 1 ms timer tick (Phase 57d). Kernel-mode tasks run cooperatively — yielding via IPC blocks, deadline-based sleeps, syscall returns. Cross-core wakes deliver via IPI but no longer preempt the receiver mid-syscall (the IPI sets `reschedule = true` and is consumed at the next user-mode return boundary, exactly like voluntary mode). The `preempt-full` Cargo feature flag is retired and every `cfg(feature = "preempt-full")` site has been removed.

The SMP discipline infrastructure is retained:

- `preempt_count` per-task counter and per-core retarget (Phase 57b C.1 / C.2 / C.3).
- `IrqSafeMutex` F.1 wiring (`preempt_disable` on lock, `preempt_enable` on `Drop`).
- `preempt_enable` deferred-reschedule semantics (`preempt_resched_pending` flag, consumed at user-mode return boundary).
- The Phase 57a wake protocol (`block_current_until` with absolute tick deadline; `wake_task_v2` with pi_lock + `on_cpu` cross-core spin-wait; `enqueue_to_core` with cross-core IPI).
- The IPC bracket exits in `kernel/src/ipc/endpoint.rs` and the wake bracket on `wake_child_waiters` (Bug #6 / #8.1 / #12 / #13 race-shape closures).
- The 1 s `sys_waitpid` deadline backstop (Bug #13).
- The `init_task` 50 ms reap-loop sleep (Bug #12) and the `stdin_feeder` waitqueue block (Bug #12).

**If a future workload genuinely needs lower kernel-mode latency** (a hard-real-time audio path, a kernel-hosted graphics driver), the architecturally-honest extension path is **`cond_resched`-style explicit yield points** — Linux's `PREEMPT_VOLUNTARY` mechanism, not its `CONFIG_PREEMPT`:

1. Identify the specific kernel paths that legitimately run uninterrupted for >1 ms (page-fault handling, large-buffer `copy_from_user` / `copy_to_user`, fork's page-table copy, exec's binary load).
2. Insert explicit `task::cond_resched()` calls at safe points inside those paths — points where `preempt_count == 0`, no spin lock is held, and the task is in a state where re-dispatch is safe.
3. The `preempt-voluntary` feature flag and `preempt_resched_pending` infrastructure are the foundation; both are retained from the 57e cleanup.

This approach costs zero unconditional overhead (no per-tick preempt check), bounds latency at the granularity the workload requires, and is the path Linux ships in `PREEMPT_VOLUNTARY` desktops. Under this future model the "preempt-full" name returns as a misleading legacy term — a dedicated phase document for the `cond_resched` work would set its own scope and call it something honest like "PREEMPT_VOLUNTARY+ explicit yield points" or "kernel-path latency bounding".

### The durable lessons

- **Architectural mismatches are easier to spot in retrospect than in design.** The four discoveries above were not visible in the original Phase 57e design doc; they only surfaced under sustained debugging on real hardware. The early "this should work" intuition rested on the assumption that what works for Linux works for m3OS — which is true for many things and wrong for this one.
- **Real-hardware testing must be a per-track gate, not a post-merge verification.** QEMU TCG's deterministic vCPU serialisation hid Bug #12 (and Bug #11) for the entire QEMU-only soak window. The procedural fix is to require a "real-hardware GUI smoke" acceptance gate on every track that touches the scheduler, IPC protocol, or wake protocol.
- **The preempt-discipline work survives the preemption-model decision.** `preempt_count`, `IrqSafeMutex` F.1, the wake brackets, and the deferred-reschedule semantics are all preempt-model-independent. The 18 sessions of bug-finding produced lasting hardening of the SMP discipline even as the headline feature was unwound.
- **Sampled-log instrumentation pays for itself.** The `[yield-sample]` instrumentation pointed at each successive dominant yield source after every fix landed. Per-fix iteration without it would have been blind.

For the full bug-by-bug history, the structural-fix hypotheses that were tried, and the post-mortem action items, see [`docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md`](../post-mortems/2026-05-07-57e-preempt-full-deferred.md). The 18-session debugging log is preserved at [`docs/handoffs/57e-preempt-full-userspace-hangs.md`](../handoffs/57e-preempt-full-userspace-hangs.md) for any future attempt.

## Feature Scope

### Make the kernel handlers preemptible (the headline decision change)

In `kernel/src/arch/x86_64/interrupts.rs::timer_handler_kernel` and `::reschedule_ipi_handler_kernel` (both built in 57d Track B as Rust handlers reached only when `(cs & 3) == 0` at IRQ entry):

```rust
// 57d (PREEMPT_VOLUNTARY): kernel handler returns early without firing preemption.
extern "C" fn timer_handler_kernel(frame: &mut PreemptTrapFrameKernel, captured_kernel_rsp: u64) {
    // Tick / EOI / reschedule-flag work.
    crate::arch::x86_64::apic::lapic_eoi();
    let _ = (frame, captured_kernel_rsp);  // unused: kernel mode is non-preemptible.
}

// 57e (PREEMPT_FULL): kernel handler runs the same preempt check as the user handler.
extern "C" fn timer_handler_kernel(frame: &mut PreemptTrapFrameKernel, captured_kernel_rsp: u64) {
    // Tick / EOI / reschedule-flag work.
    crate::arch::x86_64::apic::lapic_eoi();
    let pc = unsafe { (*crate::smp::per_core().current_preempt_count_ptr.load(Acquire)).load(Relaxed) };
    if pc != 0 { return; }
    if !crate::smp::per_core().reschedule.swap(false, AcqRel) { return; }
    unsafe { preempt_to_scheduler_kernel(frame, captured_kernel_rsp); }
}
```

The decision-side change is structural rather than a single-line drop: the kernel-handler body becomes the same shape as the user handler.  The full set of 57e changes is larger: `preempt_to_scheduler_kernel` Rust shim (Track C.0), `preempt_resume_to_kernel` asm routine (Track C.1) with the same-CPL 3-field `iretq` frame, dispatch-path branch on `cs.rpl()` (Track C.2), per-CPU access audit (Track B.3), kernel-mode `preempt_enable` immediate zero-crossing semantics (Track F.2).  The audit and validation that make this set safe is the bulk of the phase work.

### Kernel-mode preempt invariant audit

A second pass over the 57c audit catalogue, classifying every kernel-mode codepath that may fire preemption:

- **Holds a spinlock?**  `preempt_count > 0`.  Safe — preemption skips.
- **Hardware-bounded spin?**  Wrapped in `preempt_disable`.  Safe — preemption skips during the spin.
- **Cooperative-yield-bounded spin?**  Wrapped in `preempt_disable` if the spin's runtime > 100 µs; else preemption is safe (the holder will resume soon).
- **Calls another preemptible function?**  No `preempt_disable` required — the called function manages its own discipline.
- **Mutates per-CPU data?**  `preempt_disable` required for the duration of the access.

The audit produces `docs/handoffs/57e-kernel-preempt-audit.md`, listing every call path that may now be preempted and the discipline applied.

### Kernel-mode `preempt_resume` variant (different `iretq` frame shape)

When a kernel-mode task is preempted, the save and resume paths are **structurally different** from the user-mode case — not just a selector change.

On x86-64, an `iretq` that **changes privilege level** (ring 0 → ring 3) pops five fields: `rip, cs, rflags, rsp, ss`.  An `iretq` that **stays at the same privilege level** (ring 0 → ring 0) pops only three: `rip, cs, rflags`.  The interrupted code's `rsp` is implicit — it's whatever the kernel stack ends up at after the iretq frame has been popped.

This means:

- **Save side (in 57d's asm entry stub):** when the CPU dispatches an IRQ from ring 0, it pushes only 3 of the 5 iretq fields.  The interrupted task's `rsp` is *not* on the IRQ frame; it is the kernel-stack RSP at the moment of the trap.  The 57d entry stub already saves all 15 GPRs into `PreemptTrapFrame.gprs` — but the `rsp` slot in the trap frame must be populated explicitly with the kernel-stack RSP that *was current at the moment the GPR pushes started* (i.e., before the asm stub's own `push` adjusted it).  The same `PreemptTrapFrame` layout suffices because `rsp` is always at the same offset; the difference is *who writes it*: the CPU for ring-3-interrupted, the asm stub for ring-0-interrupted.
- **Resume side:** `preempt_resume_to_kernel` restores GPRs from `Task::preempt_frame.gprs`, *sets RSP to `preempt_frame.rsp`* (re-aligning the kernel stack to where the interrupted code was running), then pushes only 3 fields (`rip, cs, rflags`) and `iretq`s.  The CPU pops only those 3 fields; RSP stays at the value just set.

57e adds:

- `preempt_resume_to_kernel` (asm) — restores GPRs from `preempt_frame`, sets RSP to `preempt_frame.rsp`, pushes the 3-field iretq frame, and `iretq`s.  Distinct entry from `preempt_resume_to_user`, which pushes the 5-field iretq frame.
- 57d's asm entry stub gains a small adjustment: when `(cs & 3) == 0` (interrupted in ring 0), it captures the pre-stub kernel RSP into `PreemptTrapFrame.rsp` explicitly, since the CPU did not provide it.
- The dispatch path inspects `Task::preempt_frame.cs & 3` to choose between `_user` (rpl == 3) and `_kernel` (rpl == 0) resume routines.

A shared `_preempt_resume_common` macro factors the GPR-restore + segment-load steps that *are* identical between the two variants; only the final iretq frame layout and RSP handling differ.

### Per-CPU dispatch reentrancy

The dispatch path itself becomes a possible preemption point under `PREEMPT_FULL`.  The relevant guards:

- `pick_next` runs with `SCHEDULER.lock` held → `preempt_count > 0` → safe.
- The post-`pick_next` window between releasing the scheduler lock and entering `switch_context` is brief but exists — any preemption here would be benign (the chosen task is already determined; the worst case is the chosen task is preempted before it dispatches, in which case it goes back on the run queue).
- The `switch_context` body has IF=0 between `cli` and `popf`; preemption cannot fire there.
- `preempt_resume_to_*` runs with IF=0 until `iretq`; preemption cannot fire there.

The audit confirms each window.

### Latency benchmarks (per trigger path)

A new in-QEMU test suite (`kernel/tests/preempt_latency.rs`) measures **four distinct trigger paths** because dropping the `from_user` check changes their behaviour by very different amounts:

- **Cross-core reschedule-IPI wakeup.**  Task A on core 0 wakes task B blocked on core 1; the IPI delivers, the IRQ-return preemption check fires.  *Largest expected improvement* — under `PREEMPT_VOLUNTARY` the IPI is ignored if the receiver is in kernel mode; under `PREEMPT_FULL` it preempts immediately.  Target: floor drops measurably below the 57d baseline; aim for IRQ-handler runtime (~10 µs) but acceptance is "improves over 57d baseline by a measured factor".
- **Same-core wakeup.**  Task A on core 0 wakes task B *also on core 0* via futex; A continues running until the next scheduler entry.  *Smallest expected improvement* — `PREEMPT_FULL` does not add a self-IPI; the wake side still relies on the next timer / `preempt_enable` zero-crossing / voluntary yield.  Target: matches the 57d baseline (no regression) plus the `preempt_enable` zero-crossing latency closes faster — but no order-of-magnitude improvement is claimed here.
- **Timer-only preemption.**  A kernel-mode CPU-bound loop is preempted at the next timer tick.  Target: floor at ~1 ms (timer period) — equal to 57d's user-mode bound, but now applies to kernel mode.
- **`preempt_enable` zero-crossing.**  An IRQ sets `reschedule` while a lock is held; the lock is released; the next `preempt_enable` zero-crossing fires the scheduler.  Under 57d this records `preempt_resched_pending` and consumes it at the next user-mode return; under 57e it can fire immediately if the calling context is preempt-safe.  Target: floor drops to lock-release-to-scheduler-entry runtime (~µs).

Acceptance is **per-trigger**: each benchmark is rejected if its measured floor regresses against the 57d baseline.  No single "≥10× drop" claim is made; the cross-core IPI path is the only one where that magnitude is realistic.

### XSAVE migration (FPU state coverage)

The kernel today saves/restores FPU state via `fxsave64`/`fxrstor64` at every dispatch boundary (`kernel/src/task/scheduler.rs:1428–1447, 3644, 3727, 1979`).  FXSAVE only covers x87 + MMX + SSE; it does **not** save AVX YMM upper halves.  Hosted binaries (musl ports, modern Rust crates) emit AVX freely; under 57e's higher switch frequency the resulting silent FP corruption becomes a likely soak failure rather than a rare one.

57e migrates to `xsave64`/`xrstor64` with `XCR0 = x87 | SSE | AVX = 0x7`:

- CPUID-detected at boot; `OSXSAVE` is required (CPUs without it — pre-2011 Sandy Bridge / Bulldozer — are explicitly unsupported).
- `CR4.OSXSAVE` set, `XCR0` written via `xsetbv` on BSP and every AP before any task runs.
- `FxSaveArea` (512 bytes, 16-byte aligned) replaced by `XSaveArea` (832 bytes, 64-byte aligned).
- Save and restore call sites are unchanged — they're already at the dispatch boundary.  Only the type, alignment, and instruction change.
- AVX-512 is deferred (one additional bit in XCR0; bump `XSAVE_AREA_SIZE`; CPUID-conditional sizing).

This work is folded into 57e rather than a separate phase because the same 24-hour soak validates both the kernel-mode preemption headline change and the AVX coverage.  Splitting them would require two soaks and two release-gate cycles for what is, in implementation terms, ~200 lines across five files.

### Soak gate

A 24-hour soak with `PREEMPT_FULL` enabled, running the standard graphical-stack workload plus a synthetic IPC + futex + notification load **and** an AVX-using component (so the XSAVE migration is exercised under load).  No deadlocks, no `[WARN] [sched]` lines, no `[WARN] [preempt]` lines, no panics, no AVX checksum drift.  The soak is the gate.

## Engineering Practice Requirements

- **Test-Driven Development.**  Every track has tests landed before implementation:
  - The kernel-preempt invariant audit produces a checklist; each item has a regression test that exercises the path under `PREEMPT_FULL`.
  - The latency benchmarks land before the headline change so the "before" baseline is captured.
  - The dispatch reentrancy audit produces invariant tests in `kernel-core::preempt_model`.
- **SOLID.**
  - *Single Responsibility.*  `preempt_resume_to_kernel` only restores kernel-mode tasks; `preempt_resume_to_user` only restores user-mode.  No code branches on ring inside a single routine.
  - *Open/Closed.*  Drops a check — a removal, not an addition.  The interface to `preempt_to_scheduler` is unchanged.
  - *Liskov.*  Kernel-mode and user-mode preempted tasks are interchangeable from the scheduler's perspective.
  - *Interface Segregation.*  Same as 57d.
  - *Dependency Inversion.*  Same as 57d.
- **DRY.**  The `_user` and `_kernel` variants of `preempt_resume` share **only the GPR-restore portion** via a `_preempt_resume_common` macro.  The iretq frame layout (5-field privilege-changing for `_user`, 3-field same-CPL for `_kernel`) and the RSP handling (CPU-pushed `rsp` for `_user`, explicit `mov rsp, preempt_frame.rsp` for `_kernel`) are variant-specific and *not* shared.
- **Documented invariants.**
  - The `from_user` check is the *only* difference between 57d and 57e in the preemption decision.  Documented at the IRQ handler.
  - Every kernel busy-spin in `docs/handoffs/57c-busy-wait-audit.md` is annotated with whether it requires `preempt_disable` under `PREEMPT_FULL`.  Reviewers reject changes that add new spins without an annotation.
- **Lock ordering.**  Unchanged from 57d.
- **Migration safety.**  The headline change is gated on `cfg(feature = "preempt-full")`.  Default off.  After the 24-hour soak passes, the default flips to on; the flag is removed in a follow-up commit.
- **Observability.**  The 57d `[TRACE] [preempt]` line gains a `kernel_mode=true|false` field.  A `[WARN] [preempt] kernel-mode preemption with held lock pid=X` watchdog fires if `preempt_count == 0` is observed at the kernel-mode preempt point but the task immediately deadlocks on a known lock.

## Important Components and How They Work

### IRQ-handler preemption check (modified)

The decision-side change (drop the `from_user` check); documented at the IRQ handler.  The full implementation surface area is larger — see Track B.3 (per-CPU access audit), Track C (same-CPL resume + kernel-RSP capture), and the kernel-mode `preempt_enable` zero-crossing immediacy that 57e adds.

### `preempt_resume_to_kernel` (new assembly)

A genuinely different routine from `preempt_resume_to_user`, not just a selector swap.  Restores GPRs from `Task::preempt_frame.gprs`, sets RSP to `preempt_frame.rsp` (placing the stack pointer at the kernel-stack location the interrupted task was using), pushes only the 3-field iretq frame (`rip, cs, rflags`), and `iretq`s.  Same-CPL `iretq` does not pop `rsp`/`ss` — those are not present in the pushed frame.

Shared assembly with `preempt_resume_to_user` is factored into a `_preempt_resume_common` macro that handles the GPR-restore portion; the iretq frame layout and RSP handling are variant-specific.

### Kernel-mode preempt invariant audit (artefact)

`docs/handoffs/57e-kernel-preempt-audit.md`.  A second pass over 57c's audit catalogue, classifying every kernel codepath:

| File | Symbol | Spin pattern | preempt_disable required? | Rationale |
|---|---|---|---|---|
| `kernel/src/smp/ipi.rs` | `wait_icr_idle` | LAPIC ICR poll | yes | spinning on hardware; preemption mid-spin would block the holder's IPI delivery |
| `kernel/src/blk/virtio_blk.rs` | `do_request` | wake on completion | no | converted to block+wake in 57c; preemption at any point is safe |
| ... | ... | ... | ... | ... |

### Latency benchmarks (per-trigger)

A new in-QEMU integration test suite that runs four separate benchmarks (cross-core IPI, same-core, timer, `preempt_enable` zero-crossing) — see "Latency benchmarks (per trigger path)" in the Feature Scope above.  Each benchmark is asserted independently against a per-trigger floor; the cross-core IPI path is the only one expected to drop into the microsecond range, and the rest are required not to regress against the 57d baseline.

### `kernel-core::preempt_model` (extended)

Property tests for the kernel-mode preemption transition:

- A task in kernel mode with `preempt_count == 0` and `reschedule == true` is preempted on the next IRQ.
- A task in kernel mode with `preempt_count > 0` is *not* preempted regardless of `reschedule`.
- A task in user mode is preempted under the same condition (regression for 57d).
- A preempted kernel-mode task resumes via `iretq` to its kernel-mode `rip`.

## How This Builds on Earlier Phases

- **Drops the `from_user` early-return from Phase 57d's IRQ-handler preemption decision.**  In addition, 57e adds: same-CPL `iretq` resume routine + matching kernel-RSP capture (because the CPU pushes a different frame shape for ring-0-interrupted vs ring-3-interrupted), per-CPU access audit (because a kernel-mode preemption can migrate the running task between cores), and kernel-mode `preempt_enable` immediate zero-crossing semantics (replacing 57d's deferred-record path for kernel-mode-safe call sites).  The audit + validation that makes all of this safe is the bulk of the phase work.
- **Reuses Phase 57b's `preempt_count`** discipline — now load-bearing for kernel-mode safety.
- **Reuses Phase 57c's busy-wait audit** as the input to the kernel-preempt invariant audit.  Every "annotate" entry in 57c gains a `preempt_disable` wrapper in 57e.
- **Reuses Phase 57d's `preempt_to_scheduler`** routine — unchanged.  Adds a `_kernel` variant of `preempt_resume`.

## Implementation Outline

0. **Track 0 — Prelude.**  Confirm 57d I.2 (post-flip 24-hour soak) and I.3 (`preempt-voluntary` flag removal) have closed.  Re-baseline latency benchmarks against the post-57d-cleanup `main` so 57e numbers compare apples-to-apples.
1. **Track A — Audit (kernel preempt invariants).**  Second pass over 57c's catalogue.  Produce `docs/handoffs/57e-kernel-preempt-audit.md`.
2. **Track B — `preempt_disable` wrapping.**  Verify already-wrapped sites (B.1); wrap the remaining annotated-but-not-wrapped sites (B.2); per-CPU access audit with the "value escapes the local statement" heuristic (B.3).
3. **Track C — `preempt_resume_to_kernel` + `dispatch_preempted_and_resume_kernel`.**  Add the same-CPL resume routine *and* the dispatch trampoline that updates `*per_sched_rsp_ptr` before iretq (mirrors 57d's user-side `dispatch_preempted_and_resume`).  Without the trampoline the saved-rsp invariant breaks.
4. **Track D — Dispatch reentrancy audit + held-lock watchdog.**  Validate the dispatch path windows.  Add the `[WARN] [preempt] kernel-mode preemption with held lock` watchdog so a missed `preempt_disable` annotation surfaces as a flagged warning rather than a silent deadlock.
5. **Track E — Latency benchmarks.**  Land the per-trigger benchmarks against the 57d baseline.
6. **Track J — XSAVE migration.**  Replace FXSAVE with XSAVE+AVX before Track F so the 24-hour soak validates both changes.  Lands on its own; can be merged ahead of F if convenient.
7. **Track F — Drop the `from_user` check.**  Headline change.  Gated on `cfg(feature = "preempt-full")` (which transitively pulls in `preempt-voluntary` if 57d I.3 has not yet landed).  Includes the kernel-mode `preempt_enable` immediate zero-crossing with IF-enabled gate.
8. **Track G — 24-hour soak.**  Run the standard workload + synthetic load + AVX-using component.  Confirm no regression and no FPU corruption.
9. **Track H — Default-on flip.**  Flip the feature default.  Remove the flag.

## Acceptance Criteria

### Primary (full preemption)

- The `from_user` check is removed from `timer_handler` and `reschedule_ipi_handler`.
- A kernel-mode CPU-bound task is preempted within one timer tick.
- A kernel-mode preempted task resumes via `iretq` to its kernel-mode `rip`.
- No deadlock under any test in the regression suite.  No spinlock callsite is preempted while held.
- `kernel-core::preempt_model` property tests cover kernel-mode preemption; `cargo test -p kernel-core` passes.
- 24-hour soak with the standard graphical-stack workload + synthetic IPC + futex + notification load: no `[WARN] [sched]` lines, no `[WARN] [preempt]` lines, no panics, no deadlocks.

### Secondary (latency wins, per trigger)

- **Cross-core reschedule-IPI wakeup floor** drops measurably below the 57d baseline; benchmark reports a numeric improvement factor (target ≥10×; merge-blocking only if the measured factor is ≤1×).
- **Same-core wakeup floor** does *not* regress against the 57d baseline (no negative-direction movement).  An order-of-magnitude improvement is *not* claimed because `PREEMPT_FULL` does not add a self-IPI; see the per-trigger discussion in the Feature Scope.
- **Timer-only kernel-mode preemption** fires within one timer tick (~1 ms) on a kernel-mode CPU-bound task.
- **`preempt_enable` zero-crossing** fires immediately (within microseconds) when the calling context is preempt-safe.  Under 57d this trigger was deferred to the next user-mode return; 57e removes the deferral for kernel-mode-safe call sites.
- Audio latency (frame-to-output) does not regress; the audio_server's local soak does not report buffer underruns under load.

### Engineering practice

- TDD: every track has tests landed before implementation; PR commit history shows test-first ordering.
- The `preempt-full` feature flag is removable in a follow-up after the 24-hour soak passes.
- `docs/handoffs/57e-kernel-preempt-audit.md` exists and classifies every kernel codepath.
- `docs/03-interrupts.md` and `docs/04-tasking.md` are updated to describe `PREEMPT_FULL` semantics and the XSAVE state-component mask.

### XSAVE migration (Track J)

- CPUID detection helper exists; OSXSAVE absence panics at boot with a clear message.
- `CR4.OSXSAVE` and `XCR0 = 0x7` (x87 + SSE + AVX) are set on BSP and every AP before any task runs.
- `XSaveArea` replaces `FxSaveArea`; size 832 bytes, alignment 64 bytes.
- `xsave64` / `xrstor64` (or `xsaveopt64` if available) replace `fxsave64` / `fxrstor64` at the existing dispatch-boundary call sites.
- AVX context-switch regression test passes (`kernel/tests/xsave_avx.rs`).
- 24-hour soak workload includes an AVX-using component with checksum verification every 100 ms; no drift over 24 hours.

## Companion Task List

- [Phase 57e Task List](./tasks/57e-full-kernel-preemption-tasks.md)

## How Real OS Implementations Differ

- **Linux's `CONFIG_PREEMPT`** is the equivalent model.  Linux gates kernel-mode preemption on `preempt_count == 0` plus `need_resched` plus the IRQ-return-from-kernel point.  m3OS matches this exactly.
- **Linux's `RT_PREEMPT` patchset** replaces sleeping spinlocks (`raw_spinlock_t`) with priority-inheritance mutexes for soft-real-time work.  m3OS does not have priority inheritance and so does not have a parallel `RT_PREEMPT` config.
- **Linux's `cond_resched`** explicit reschedule points inside long kernel loops.  m3OS does not need them because 57c removes the long loops; the rare remaining ones are bounded.
- **seL4** is non-preemptible by design — the kernel runs to completion at every entry.  m3OS aims for `PREEMPT_FULL` parity with Linux as the long-term target, accepting the additional safety requirements.

## Deferred Until Later

- **Per-CPU runqueues with per-CPU locks.**  Increases scalability under `PREEMPT_FULL`; deferred to a later kernel-architecture phase.
- **Priority inheritance.**  `rt_mutex` equivalent.  Deferred.
- **Real-time scheduling policies (SCHED_FIFO, SCHED_RR).**  Deferred.
- **Lockdep equivalent** for runtime lock-ordering and preempt-disable checking.  Deferred (a separate kernel-infrastructure phase).
- **Loom-style formal interleaving search** of preempted kernel codepaths.  Stretch goal.
- **`PREEMPT_RT` parity** — replacing all spinlocks with sleeping mutexes.  Deferred indefinitely; m3OS does not target real-time guarantees.
- **AVX-512 in `XCR0`.**  Track J ships with `XCR0 = 0x7` (x87 + SSE + AVX).  Adding AVX-512 (bit 5) requires bumping `XSAVE_AREA_SIZE`, querying CPUID 0Dh sub-leaf 0 ECX for the runtime size, and verifying QEMU's CPU model advertises AVX-512.  Trivial work; deferred until a hosted binary actually uses AVX-512.
- **XSAVE fallback for pre-2011 CPUs.**  m3OS explicitly drops support for CPUs without OSXSAVE.  A FXSAVE fallback could be added if a contributor needs to run on hardware older than Sandy Bridge / Bulldozer.
- **Memory protection keys (PKRU) save/restore.**  Bit 9 of XCR0; not used by m3OS.  Deferred.
