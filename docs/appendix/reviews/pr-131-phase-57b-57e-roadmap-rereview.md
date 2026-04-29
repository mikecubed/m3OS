# PR 131 Phase 57b-57e Roadmap Re-Review

**Review date:** 2026-04-29
**Branch:** `docs/phase-57b-preemption-plan`
**PR:** 131, `docs: split Phase 57b kernel preemption into 57b/57c/57d/57e`
**Prior review:** `docs/appendix/reviews/pr-131-phase-57b-57e-roadmap-review.md`

## Summary

The major blockers from the first review are mostly fixed. The revised docs now:

- Move 57b away from a scheduler-lock-based `preempt_disable()` and introduce stable `Vec<Box<Task>>` storage plus a per-core `current_preempt_count_ptr`.
- Require 57d naked-asm timer/reschedule-IPI entry stubs that save all GPRs before calling Rust.
- Track `preempt_enable()` zero-crossing/deferred-reschedule behavior in 57d.
- Correct 57e's same-CPL kernel `iretq` design and split latency expectations by trigger path.
- Fix 57b's lock audit source-of-truth and 57e's `Planned (Stretch)` status issue.

There is one new correctness issue in the revised 57b pointer lifecycle, plus a few stale-text cleanups. I would fix those before treating this as ready implementation guidance.

## Remaining Findings

### 1. Blocking: `current_preempt_count_ptr` update ordering can decrement the wrong task

**Where:**
- `docs/roadmap/57b-preemption-foundation.md:39-45`
- `docs/roadmap/57b-preemption-foundation.md:121-148`
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:139-149`

The revised 57b plan correctly makes `preempt_disable()` lock-free via `PerCoreData::current_preempt_count_ptr`, but the pointer lifecycle is underspecified and the documented update point is unsafe.

The docs say dispatch updates `current_preempt_count_ptr` between `pick_next` and `switch_context` while `SCHEDULER.lock` is still held. Once `SCHEDULER.lock` is an `IrqSafeMutex`, acquiring that lock increments the count through whatever pointer was current at lock acquisition, and dropping the guard decrements through whatever pointer is current at drop.

If dispatch changes the pointer to the incoming task while the scheduler lock is still held, the later `IrqSafeGuard::Drop` calls `preempt_enable()` through the new pointer and decrements the incoming task's count. That can drive the incoming task negative, while the count incremented at lock acquisition remains on the old pointee.

There is a second lifecycle gap: scheduler-loop code should not keep charging lock acquisitions to the task that just switched out. The docs say the boot dummy is used while running scheduler/idle code, but the task list only requires pointer updates on dispatch to the incoming task or idle. It should also require retargeting to a scheduler dummy immediately after switching out of a task and before the scheduler loop takes any `IrqSafeMutex`.

**Recommended fix:**

Add an explicit pointer lifecycle invariant and test:

- Running task context: pointer targets that task's `preempt_count`.
- Scheduler/idle context: pointer targets `BOOT_PREEMPT_COUNT_DUMMY[core]`.
- Switch-out epilogue: immediately after `switch_context` returns to the scheduler, before any scheduler lock acquisition, retarget to the dummy.
- Switch-in path: retarget from dummy to the incoming task only after all scheduler locks that were acquired under the dummy are dropped, and do it in an interrupt-masked handoff window so an IRQ cannot observe "scheduler is still running, pointer already targets incoming task."
- Regression test: acquire/release `scheduler_lock()` across a dispatch handoff and assert the same pointee is incremented and decremented; incoming task count remains zero until it actually runs.

The key rule is: an `IrqSafeMutex` guard must decrement the same preempt-count pointee it incremented.

### 2. High: 57d asm entry stubs need ABI and IDT-install details

**Where:**
- `docs/roadmap/57d-voluntary-preemption.md:36-106`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:81-109`
- Current code uses `set_handler_fn(timer_handler)` and `set_handler_fn(reschedule_ipi_handler)` in `kernel/src/arch/x86_64/interrupts.rs`.

The revised 57d design correctly replaces Rust `extern "x86-interrupt"` handlers with asm entry stubs. Two practical details should be explicit in the task list:

- The IDT installation cannot keep using the current `set_handler_fn` shape for a Rust `extern "x86-interrupt"` function if the actual entry is a raw assembly symbol. The task should say whether to use the x86_64 crate's raw handler address API, an assembly-compatible wrapper, or another established pattern.
- The asm stub calls `extern "C"` Rust (`timer_handler_with_frame`). The stub must preserve the Rust/C ABI expectations before that call, especially stack alignment, and restore the original interrupt stack on return. An interrupt can arrive at arbitrary instruction boundaries, so this should be tested or at least stated as an invariant.

**Recommended fix:** Extend 57d Track B acceptance with IDT registration and ABI stack-alignment requirements.

### 3. Medium: 57c still has stale wrapper-timing text

**Where:**
- `docs/roadmap/57c-kernel-busy-wait-conversion.md:68-81`
- `docs/roadmap/57c-kernel-busy-wait-conversion.md:180-186`

Most of 57c now correctly says busy-spin `preempt_disable` wrappers are load-bearing for 57e, not 57d. Two stale lines remain:

- The `wait_icr_idle()` bullet still says to wrap when 57b lands so a 57d preemption point does not interrupt the spin.
- "How This Builds on Earlier Phases" still says a 57b/57c integration commit adds wrappers in lockstep with 57b.

**Recommended fix:** Replace both with "57e Track B adds wrappers when `PREEMPT_FULL` makes them load-bearing."

### 4. Medium: 57d design doc duplicates the `Task` state additions section

**Where:**
- `docs/roadmap/57d-voluntary-preemption.md:138-176`

The `Task` state additions section appears twice. The first version includes the useful explanation that `resume_mode` is a discriminant, not a flag; the second is the older text.

**Recommended fix:** Keep the first section and delete the duplicate.

### 5. Medium: 57e has stale "single line" and old helper-name language

**Where:**
- `docs/roadmap/57e-full-kernel-preemption.md:39-56`
- `docs/roadmap/57e-full-kernel-preemption.md:121-130`
- `docs/roadmap/57e-full-kernel-preemption.md:170-175`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:21-29`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:247-258`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:353`

57e's main design now correctly says kernel-mode resume is structurally different and latency is per-trigger. Some older framing remains:

- The pseudocode still uses `current_task_idx_fast()` and `get_task_preempt_count_fast()`, which the revised 57d explicitly removed in favor of `current_preempt_count_ptr`.
- Engineering requirements still say `_user` and `_kernel` variants share an "`iretq` core" and differ by selectors. Later sections correctly say only GPR restore is shared and frame/RSP handling is variant-specific.
- Several lines still describe the headline change as "a single line" or "drops a single check." That is no longer accurate as implementation guidance because 57e also requires same-CPL RSP capture/resume handling, per-CPU access audit, and `preempt_enable` immediate zero-crossing semantics.

**Recommended fix:** Update the stale snippets to match the revised 57d/57e model: `current_preempt_count_ptr`, shared GPR restore only, and "headline decision change" instead of "single-line implementation."

## Prior Findings Status

- **57b `preempt_disable()` recursion through `scheduler_lock()`: addressed conceptually.** The new lock-free pointer design resolves the original recursion, but the pointer lifecycle issue above must be fixed.
- **57d GPR save too late from Rust interrupt handlers: addressed.** The naked-asm entry-stub requirement is the right direction.
- **57e same-CPL `iretq` frame shape: addressed.** The design now distinguishes ring-3 and ring-0 returns.
- **Missing `preempt_enable()` zero-crossing path: addressed.** 57d and 57e both now track it.
- **57e hard 10 us latency target: addressed.** Benchmarks are now per-trigger and more honest.
- **57b lock audit grep incomplete: addressed.** The task list now requires both declaration and acquisition scans plus IRQ-shared classification.
- **57b diagnostic flag conflict: addressed.** The `preempt_logged_nonzero` field is gone.
- **57d duplicate current-task index: addressed.** The docs now reuse `current_preempt_count_ptr`.
- **57c stale helper name: addressed.** No `block_current_unless_woken` references remain.
- **57e `Planned (Stretch)` status: addressed.** Status is now `Planned`, with stretch-goal text in prose.

## Verification Performed

- Re-read revised 57b/57c/57d/57e design docs and task lists.
- Checked for the prior review's key terms and stale references.
- Spot-checked current kernel code paths relevant to the new pointer/asm assumptions: `IrqSafeMutex`, `scheduler_lock`, current `PerCoreData::current_task_idx`, current Rust interrupt handler installation, and current inline `switch_context`.

No build or QEMU test was run; this remains a roadmap/documentation review.
