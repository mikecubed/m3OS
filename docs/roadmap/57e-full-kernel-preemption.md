# Phase 57e — Full Kernel Preemption (PREEMPT_FULL)

**Status:** Planned (Stretch)
**Source Ref:** phase-57e
**Depends on:** Phase 57b (Preemption Foundation) ✅, Phase 57c (Kernel Busy-Wait Audit and Conversion) ✅, Phase 57d (Voluntary Preemption) ✅
**Builds on:** Drops the `from_user` check from the IRQ-return preemption point introduced in 57d.  Once dropped, every kernel-mode IRQ-return becomes a potential preemption point — and the 57b `preempt_count` discipline becomes load-bearing for kernel-mode safety, not just user-mode.
**Primary Components:** `kernel/src/arch/x86_64/interrupts.rs` (drop `from_user` from the preemption check), `kernel/src/task/scheduler.rs` (kernel-mode preempt invariants), `kernel/src/arch/x86_64/asm/switch.S` (kernel-mode `preempt_resume` variant)

## Milestone Goal

Kernel-mode code becomes preemptible at any point where `preempt_count == 0`.  Round-trip latency for IPC and syscall wake-up drops into the microsecond range.  Every previously-bounded busy-spin in the kernel either remains bounded (preempt-disable wrapped) or becomes preemptible (the holder may be paused mid-spin and the spinner makes no progress until the holder resumes — but the spinner does not block forward progress on its core).

This is the **stretch goal**.  After 57b/57c/57d, m3OS is at `PREEMPT_VOLUNTARY` parity with Linux's desktop default — a credible plateau.  57e is the upgrade to `PREEMPT_FULL` (Linux's "low-latency desktop" or "real-time" config), which trades debuggability for latency.  Whether to land 57e depends on m3OS's release goals and the soak data from 57c/57d.

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
- Why latency benchmarks (round-trip IPC, syscall wakeup) drop into the microsecond range only after 57e: in `PREEMPT_VOLUNTARY` the kernel-mode timeslice is the floor; in `PREEMPT_FULL` the floor is the IRQ-handler runtime.
- How to incrementally validate `PREEMPT_FULL`: enable the flag, run the regression suite, soak for 24 hours, then enable it for a release.

## Feature Scope

### Drop the `from_user` check (the headline change)

In `kernel/src/arch/x86_64/interrupts.rs::timer_handler`:

```rust
// 57d:
if from_user
    && let Some(idx) = current_task_idx_fast()
    && get_task_preempt_count_fast(idx) == 0
    && per_core().reschedule.load(Relaxed) { ... }

// 57e:
if let Some(idx) = current_task_idx_fast()
    && get_task_preempt_count_fast(idx) == 0
    && per_core().reschedule.load(Relaxed) { ... }
```

A single line.  The rest of 57e is the audit and validation that makes this single line safe.

### Kernel-mode preempt invariant audit

A second pass over the 57c audit catalogue, classifying every kernel-mode codepath that may fire preemption:

- **Holds a spinlock?**  `preempt_count > 0`.  Safe — preemption skips.
- **Hardware-bounded spin?**  Wrapped in `preempt_disable`.  Safe — preemption skips during the spin.
- **Cooperative-yield-bounded spin?**  Wrapped in `preempt_disable` if the spin's runtime > 100 µs; else preemption is safe (the holder will resume soon).
- **Calls another preemptible function?**  No `preempt_disable` required — the called function manages its own discipline.
- **Mutates per-CPU data?**  `preempt_disable` required for the duration of the access.

The audit produces `docs/handoffs/57e-kernel-preempt-audit.md`, listing every call path that may now be preempted and the discipline applied.

### Kernel-mode `preempt_resume` variant

When a kernel-mode task is preempted, the resume must `iretq` back to ring 0 (not ring 3).  The `preempt_resume_to_user` routine introduced in 57d resumes to ring 3 specifically.  57e adds:

- `preempt_resume_to_kernel` — same routine but with `cs:ss` reflecting kernel selectors.
- The dispatch path inspects `Task::preempt_frame.cs.rpl()` to choose between `_user` and `_kernel` resume routines.

### Per-CPU dispatch reentrancy

The dispatch path itself becomes a possible preemption point under `PREEMPT_FULL`.  The relevant guards:

- `pick_next` runs with `SCHEDULER.lock` held → `preempt_count > 0` → safe.
- The post-`pick_next` window between releasing the scheduler lock and entering `switch_context` is brief but exists — any preemption here would be benign (the chosen task is already determined; the worst case is the chosen task is preempted before it dispatches, in which case it goes back on the run queue).
- The `switch_context` body has IF=0 between `cli` and `popf`; preemption cannot fire there.
- `preempt_resume_to_*` runs with IF=0 until `iretq`; preemption cannot fire there.

The audit confirms each window.

### Latency benchmarks

A new in-QEMU test (`kernel/tests/preempt_latency.rs`):

- Round-trip IPC: send a request, time the round-trip.  Under `PREEMPT_VOLUNTARY` the floor is ~1 ms (timer tick); under `PREEMPT_FULL` the floor is the IRQ-handler runtime, expected ~10 µs.
- Syscall wakeup: a task blocks on a futex; another task wakes it; time the wakeup.  Same expected drop.
- The benchmarks gate the merge: 57e is accepted only if the latency floor drops by ≥10×.

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
- **DRY.**  The `_user` and `_kernel` variants of `preempt_resume` share an `iretq` core; the difference is the segment selectors.  Factor the shared part into a macro / helper.
- **Documented invariants.**
  - The `from_user` check is the *only* difference between 57d and 57e in the preemption decision.  Documented at the IRQ handler.
  - Every kernel busy-spin in `docs/handoffs/57c-busy-wait-audit.md` is annotated with whether it requires `preempt_disable` under `PREEMPT_FULL`.  Reviewers reject changes that add new spins without an annotation.
- **Lock ordering.**  Unchanged from 57d.
- **Migration safety.**  The headline change is gated on `cfg(feature = "preempt-full")`.  Default off.  After the 24-hour soak passes, the default flips to on; the flag is removed in a follow-up commit.
- **Observability.**  The 57d `[TRACE] [preempt]` line gains a `kernel_mode=true|false` field.  A `[WARN] [preempt] kernel-mode preemption with held lock pid=X` watchdog fires if `preempt_count == 0` is observed at the kernel-mode preempt point but the task immediately deadlocks on a known lock.

## Important Components and How They Work

### IRQ-handler preemption check (modified)

The single line change.  Documented at the IRQ handler.

### `preempt_resume_to_kernel` (new assembly)

Mirrors `preempt_resume_to_user` but pushes kernel-mode `cs:ss` selectors.  Located alongside the user-mode variant in `kernel/src/arch/x86_64/asm/switch.S`.

### Kernel-mode preempt invariant audit (artefact)

`docs/handoffs/57e-kernel-preempt-audit.md`.  A second pass over 57c's audit catalogue, classifying every kernel codepath:

| File | Symbol | Spin pattern | preempt_disable required? | Rationale |
|---|---|---|---|---|
| `kernel/src/smp/ipi.rs` | `wait_icr_idle` | LAPIC ICR poll | yes | spinning on hardware; preemption mid-spin would block the holder's IPI delivery |
| `kernel/src/blk/virtio_blk.rs` | `do_request` | wake on completion | no | converted to block+wake in 57c; preemption at any point is safe |
| ... | ... | ... | ... | ... |

### Latency benchmarks

A new in-QEMU integration test that boots the kernel, runs the standard workload, and measures latency floors.  Asserts the floor is ≥10× lower than the 57d baseline.

### `kernel-core::preempt_model` (extended)

Property tests for the kernel-mode preemption transition:

- A task in kernel mode with `preempt_count == 0` and `reschedule == true` is preempted on the next IRQ.
- A task in kernel mode with `preempt_count > 0` is *not* preempted regardless of `reschedule`.
- A task in user mode is preempted under the same condition (regression for 57d).
- A preempted kernel-mode task resumes via `iretq` to its kernel-mode `rip`.

## How This Builds on Earlier Phases

- **Drops a single check from Phase 57d's IRQ-handler preemption point.**  Everything else 57e adds is audit + validation.
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

### Secondary (latency wins)

- Round-trip IPC latency floor drops from ~1 ms (57d) to ~10 µs (57e), measured by the new latency benchmark.
- Syscall wakeup latency floor drops similarly.
- Audio latency (frame-to-output) drops below the 57d baseline; the audio_server's local soak no longer reports buffer underruns under load.

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
