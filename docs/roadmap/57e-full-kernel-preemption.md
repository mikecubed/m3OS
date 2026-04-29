# Phase 57e — Full Kernel Preemption (PREEMPT_FULL)

**Status:** Planned
**Source Ref:** phase-57e
**Depends on:** Phase 57b (Preemption Foundation) ✅, Phase 57c (Kernel Busy-Wait Audit and Conversion) ✅, Phase 57d (Voluntary Preemption) ✅
**Builds on:** Drops the `from_user` check from the IRQ-return preemption point introduced in 57d.  Once dropped, every kernel-mode IRQ-return becomes a potential preemption point — and the 57b `preempt_count` discipline becomes load-bearing for kernel-mode safety, not just user-mode.
**Primary Components:** `kernel/src/arch/x86_64/interrupts.rs` (replace 57d's early-return in `timer_handler_kernel` / `reschedule_ipi_handler_kernel` with the same preempt check the user handlers run), `kernel/src/task/scheduler.rs` (`preempt_to_scheduler_kernel`, kernel-mode preempt invariants, kernel-mode `preempt_enable` immediate zero-crossing), `kernel/src/arch/x86_64/asm/preempt_entry.S` (`preempt_resume_to_kernel` same-CPL `iretq` resume routine)

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

### Soak gate

A 24-hour soak with `PREEMPT_FULL` enabled, running the standard graphical-stack workload plus a synthetic IPC + futex + notification load.  No deadlocks, no `[WARN] [sched]` lines, no panics.  The soak is the gate.

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

1. **Track A — Audit (kernel preempt invariants).**  Second pass over 57c's catalogue.  Produce `docs/handoffs/57e-kernel-preempt-audit.md`.
2. **Track B — `preempt_disable` wrapping.**  For every "annotate" entry in 57c that requires `preempt_disable` under `PREEMPT_FULL`, add the wrapper.  One PR per subsystem.
3. **Track C — `preempt_resume_to_kernel`.**  Implement the kernel-mode resume routine.  Test in isolation.
4. **Track D — Dispatch reentrancy audit.**  Validate the dispatch path windows.  Add invariant tests.
5. **Track E — Latency benchmarks.**  Land the benchmarks against the 57d baseline.
6. **Track F — Drop the `from_user` check.**  Headline change.  Gated on `cfg(feature = "preempt-full")`.
7. **Track G — 24-hour soak.**  Run the standard workload + synthetic load.  Confirm no regression.
8. **Track H — Default-on flip.**  Flip the feature default.  Remove the flag.

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
- `docs/03-interrupts.md` and `docs/04-tasking.md` are updated to describe `PREEMPT_FULL` semantics.

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
