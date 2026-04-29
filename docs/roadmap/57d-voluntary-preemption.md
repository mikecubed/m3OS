# Phase 57d — Voluntary Preemption (PREEMPT_VOLUNTARY)

**Status:** Planned
**Source Ref:** phase-57d
**Depends on:** Phase 3 (Interrupts) ✅, Phase 4 (Tasking) ✅, Phase 25 (SMP) ✅, Phase 35 (True SMP) ✅, Phase 57a (Scheduler Block/Wake Protocol Rewrite) ✅, Phase 57b (Preemption Foundation) ✅
**Builds on:** Activates the 57b foundation: the timer IRQ handler is extended to fire `preempt_to_scheduler` whenever the interrupted code is in user mode, `preempt_count == 0`, and the per-core `reschedule` flag is set.  The 57b `PreemptFrame` save area becomes live; the 57b `preempt_count` discipline becomes load-bearing.
**Primary Components:** `kernel/src/arch/x86_64/interrupts.rs` (timer + reschedule-IPI handlers), `kernel/src/arch/x86_64/asm/switch.S` (`preempt_to_scheduler` routine, full register save/restore via `iretq`), `kernel/src/task/scheduler.rs` (`preempt_to_scheduler` Rust entry, `dispatch` integration, run-queue placement of the preempted task)

## Milestone Goal

A user-mode task that monopolises its core via a tight CPU loop is now preempted within one timer tick.  The scheduler reaches the next runnable task on that core within ~1 ms of preemption-eligibility (timer tick at 1 kHz).  Kernel-mode code remains non-preemptible — a syscall that busy-waits is still a CPU monopoly, but Phase 57c has made those rare and bounded.

This is the first subphase that **changes user-visible behaviour**.  After 57d, the residual graphical-stack regression catalogued in `docs/handoffs/57a-validation-gate.md` is fixed by both kernel-mode (57c) and user-mode (57d) starvation paths simultaneously.

## Why This Phase Exists

After 57b lands the `preempt_count` infrastructure and 57c removes the kernel-mode CPU-monopoly bugs, the remaining gap is user-mode CPU-bound tasks: a userspace daemon in a tight loop, a runaway test program, or simply a busy compute task during a graphical session.  These tasks block every other task scheduled on the same core because the timer IRQ handler does not preempt them — it only sets the per-core `reschedule` flag, which the task never consults.

The `PREEMPT_VOLUNTARY` model (Linux's desktop default) is the right next step: interrupts may preempt running user-mode code, but kernel-mode code remains non-preemptible.  This is a conservative, high-value increment:

- **High value.**  Every user-mode CPU-bound task becomes interruptible.  The scheduler regains the ability to make forward progress on every core regardless of which task is running.
- **Conservative.**  Kernel-mode preemption (57e) is a much harder problem — every kernel spinlock callsite must be `preempt_disable`-correct, and any missed `preempt_disable` causes a deadlock the moment the missing wrapper is exercised.  57d sidesteps that risk by gating preemption on `from_user`.

The phase is small in lines-of-code but high in ceremony: every register the IRQ handler save / restores must match `iretq`'s expectations bit-for-bit, the scheduler-RSP swap must be atomic with respect to the IRQ, and the preempted task must end up on the run queue in a state the v2 scheduler accepts (`Ready`, with `on_cpu` cleared).

## Learning Goals

- How an `iretq`-driven preemption point differs from a `ret`-driven cooperative yield: the full register set is live, the switch happens mid-instruction, and the resume must use `iretq` (with the full frame) not `ret` (with only callee-saved + RFLAGS).
- Why the timer IRQ handler must EOI the LAPIC *before* swapping RSP to the scheduler stack: a deferred EOI on the new stack would leave the LAPIC starved.
- Why the preemption check must read `from_user`, `preempt_count`, and `reschedule` *atomically with respect to the interrupted code* — and why those reads happen in the IRQ handler (not the scheduler) where IF=0 keeps them coherent.
- How the scheduler's run-queue accepts a preempted task: the task is enqueued at the tail of its home core's queue with `state = Ready`, `on_cpu = false`, and `preempt_frame` populated — the dispatch path picks it up like any cooperative yield.
- Why kernel-mode preemption is deferred to 57e: the audit + `preempt_disable` discipline needed for safety is large enough to deserve its own phase, and the 57c work makes 57d-only correctness a viable plateau.

## Feature Scope

### IRQ-return preemption check

In `kernel/src/arch/x86_64/interrupts.rs::timer_handler` and `::reschedule_ipi_handler`, after the existing tick + EOI work:

```rust
extern "x86-interrupt" fn timer_handler(mut stack_frame: InterruptStackFrame) {
    // ... existing tick + reschedule-flag work ...
    super::apic::lapic_eoi();

    // NEW (57d): preemption check.
    let from_user = stack_frame.code_segment.rpl() == PrivilegeLevel::Ring3;
    if from_user
        && let Some(idx) = current_task_idx()
        && SCHEDULER.tasks[idx].preempt_count.load(Relaxed) == 0
        && per_core().reschedule.load(Relaxed)
    {
        // Switch to scheduler RSP; scheduler will pick next task.
        unsafe { preempt_to_scheduler(&stack_frame, idx) };
        // Returns via iretq to whatever the scheduler picked next.
    }
}
```

Same logic mirrored in `reschedule_ipi_handler` (which is the cross-core wake delivery path).

### `preempt_to_scheduler` routine

A new arch-specific routine that:

1. Saves the full `iretq` frame from the IRQ stack (which already contains `rip`, `cs`, `rflags`, `rsp`, `ss`) plus all GPRs into `Task::preempt_frame`.
2. Stores the preempted-task state: `task.state = Ready`, `task.on_cpu = false`, run-queue insertion at home-core tail.
3. Switches RSP to the scheduler RSP for this core.
4. Calls into the scheduler's `pick_next` and dispatches the chosen task.
5. The dispatched task resumes via the existing cooperative `switch_context` path — *unless* it too was preempted, in which case the scheduler reads its `preempt_frame` and uses an `iretq`-restore path.

The dispatch path gains a third arm:

- **Cooperative resume** (existing): saved by `switch_context`, restored by `switch_context` via `ret`.
- **Preempted resume** (new): saved by `preempt_to_scheduler` to `preempt_frame`, restored by a new `preempt_resume_to_user` routine via `iretq`.
- **Initial dispatch** (existing): a freshly-spawned task starts via the cooperative path.

### `Task` state additions

`Task` gains a discriminant identifying the resume mode:

```rust
enum ResumeMode {
    Cooperative,  // restore via switch_context (saved_rsp), ret
    Preempted,    // restore via preempt_resume_to_user (preempt_frame), iretq
    Initial,      // freshly spawned; init_stack layout
}

struct Task {
    // ... existing fields ...
    resume_mode: AtomicU8,  // ResumeMode encoded
    preempt_frame: PreemptFrame,  // 57b — load-bearing in 57d
}
```

The dispatch path reads `resume_mode` and selects the restore routine.  This is a small additive change.

### Scheduler integration

The scheduler's `pick_next` and `dispatch` routines must accept a preempted task:

- `pick_next` already returns the next runnable task; no change needed (the preempted task is enqueued before `pick_next` runs).
- `dispatch` reads `resume_mode` and selects between `switch_context` (cooperative) and `preempt_resume_to_user` (preempted).  Both routines end up at user mode; the difference is the register-restore mechanism.

### Per-CPU `current_task_idx` fast path

The IRQ handler reads `current_task_idx` and `preempt_count` on every timer tick.  In 57b the read goes through the scheduler lock; in 57d this becomes a hot path and must be lock-free.

- Add a per-CPU `AtomicI32 current_task_idx_fast` (Relaxed) updated by the dispatch path on every context switch.  The IRQ handler reads it directly.
- The lock-acquired version remains for non-hot paths; the fast version is read-only and may be momentarily stale (which is acceptable because a stale read in the IRQ handler simply skips preemption this tick — the next tick will catch it).

### Stress test

A new in-QEMU integration test (`kernel/tests/preempt_user_loop.rs`):

- Spawn a userspace task that runs a tight CPU loop with no syscall.
- Run other tasks on the same core.
- Verify they make forward progress within 100 ms.
- Verify the CPU-bound task accumulates approximately the expected runtime fraction.

## Engineering Practice Requirements

- **Test-Driven Development.**  Every behaviour change has a regression test landed before the implementation:
  - The `preempt_to_scheduler` routine has a model test in `kernel-core/src/preempt_model.rs` (extended from 57b) covering the state transitions: a Running task becomes Ready with `preempt_frame` populated.
  - The IRQ-return check has an in-QEMU integration test that fires the timer IRQ on a stub task with `from_user=true`, `preempt_count=0`, `reschedule=true` and asserts the scheduler is reached.
  - The user-loop stress test runs in CI.
- **SOLID.**
  - *Single Responsibility.*  `preempt_to_scheduler` saves and switches; the scheduler picks; `preempt_resume_to_user` restores.  Each routine has one job.
  - *Open/Closed.*  New IRQ sources that want to fire preemption (e.g., a future IPC reschedule-IPI variant) plug in via `signal_reschedule()` + the existing IRQ-return check; no scheduler changes required.
  - *Liskov.*  A preempted task and a cooperatively-yielded task are indistinguishable to `pick_next` — both are `Ready` in the run queue.
  - *Interface Segregation.*  The IRQ handler sees `preempt_to_scheduler(&stack_frame, idx)`; it does not see `Task::preempt_frame`.
  - *Dependency Inversion.*  The IRQ handler depends on the `preempt_count` and `reschedule` atomics, not on `Task` internals.
- **DRY.**  A single `preempt_to_scheduler` for both timer and reschedule-IPI paths.  A single `preempt_resume_to_user` for restore.  No per-IRQ variant.
- **Documented invariants.**
  - **`from_user` check.**  Preemption only fires when the interrupted code was in ring 3.  Documented at the IRQ handler.
  - **`preempt_count == 0`.**  Required precondition.  A non-zero count indicates a held lock or explicit preempt-disable; preemption silently skips.
  - **`reschedule` flag.**  Set by the timer or by `signal_reschedule_all()`.  Cleared by the scheduler dispatch path.
  - **`from_user → preempt_count == 0` always.**  In ring 3, no kernel locks are held; `preempt_count` is always 0.  A `debug_assert!` confirms this in the IRQ handler.
- **Lock ordering.**  The IRQ handler reads three atomics with `Relaxed` ordering — no locks acquired in the IRQ context.  The scheduler dispatch (reached via `preempt_to_scheduler`) takes `SCHEDULER.lock` (inner) but never `pi_lock` (outer) — preserving the 57a hierarchy.
- **Migration safety.**  The IRQ-return check is gated on a `cfg(feature = "preempt-voluntary")` flag during initial roll-out.  Default off; opt-in for testing.  Final landing flips the default to on; the flag is removed in a follow-up cleanup commit.
- **Observability.**  Every preemption emits a `[TRACE] [preempt]` line under `--features sched-trace` (extends 57a's tracepoint).  The watchdog continues to fire on stuck tasks; a stuck-task warning that includes "preempted=N" frames helps diagnose preempt-discipline bugs.

## Important Components and How They Work

### `preempt_to_scheduler` (assembly + Rust)

Located at `kernel/src/arch/x86_64/asm/switch.S` (Rust shim in `kernel/src/task/scheduler.rs`).  Called from the IRQ handler with the `InterruptStackFrame` reference and the current task index.

```asm
preempt_to_scheduler:
    // rdi = pointer to Task::preempt_frame
    // rsi = scheduler RSP for this core
    //
    // Save GPRs into preempt_frame (rax..r15 except rsp).
    // Save iretq fields from the IRQ stack (rip, cs, rflags, rsp, ss) into preempt_frame.
    // Set Task::on_cpu = false (Release).  Set Task::resume_mode = Preempted.
    // Switch RSP to scheduler RSP.
    // Jump to scheduler dispatch entry point.
```

The Rust shim handles run-queue insertion of the preempted task and the `pick_next` call.

### `preempt_resume_to_user` (assembly)

Located at `kernel/src/arch/x86_64/asm/switch.S`.  Called by the dispatch path when the chosen task's `resume_mode == Preempted`.

```asm
preempt_resume_to_user:
    // rdi = pointer to Task::preempt_frame
    //
    // Restore GPRs.  Push iretq frame (ss, rsp, rflags, cs, rip) onto current stack.
    // iretq.
```

### IRQ-handler preemption check

Located at `kernel/src/arch/x86_64/interrupts.rs::timer_handler` and `::reschedule_ipi_handler`.  After the existing EOI:

```rust
let from_user = stack_frame.code_segment.rpl() == PrivilegeLevel::Ring3;
if from_user
    && let Some(idx) = current_task_idx_fast()
{
    let pc = get_task_preempt_count_fast(idx);
    if pc == 0 && per_core().reschedule.load(Relaxed) {
        per_core().reschedule.store(false, Relaxed);
        unsafe { preempt_to_scheduler(&stack_frame, idx); }
        // does not return
    }
}
```

`current_task_idx_fast` reads the per-CPU `current_task_idx_fast` atomic (Relaxed).  `get_task_preempt_count_fast` is a similarly relaxed read on `Task::preempt_count`.  Neither acquires the scheduler lock.

### `Task::resume_mode`

Tracks how the task was last suspended.  Read on dispatch, written by the suspending path.  Transitions:

- `Initial → Cooperative` on first dispatch (the dispatch path runs `init_stack`-laid-out code via `switch_context`).
- `Cooperative → Cooperative` on every cooperative yield.
- `Running → Preempted` on a preemption.
- `Preempted → Cooperative` on the first cooperative yield after resume (the task voluntarily entered kernel mode and yielded; subsequent dispatch is cooperative until a new preemption).

### `kernel-core::preempt_model` (extended)

The 57b model gains:

- A `preempt(state) -> state` transition: from `Running` with `preempt_count == 0` to `Ready` with `preempt_frame` populated.
- Property tests for the IRQ-return check: random sequences of (timer tick, syscall enter, syscall exit, lock acquire, lock release) must preserve the invariant that preemption only fires when `preempt_count == 0` and `from_user == true`.

## How This Builds on Earlier Phases

- **Activates Phase 57b's `preempt_count`** as the live gate for preemption.  Before 57d the count is incremented and decremented but never inspected; 57d makes it load-bearing.
- **Activates Phase 57b's `PreemptFrame`** as the live save-area.  Before 57d the frame exists but is never written; 57d makes it load-bearing.
- **Reuses Phase 57a's `wake_task_v2`** as the post-preemption wake source — a preempted task's home core may differ from where it was preempted, so the run-queue insertion may need a reschedule IPI.
- **Reuses Phase 35 (True SMP)** per-core `reschedule` flag and `signal_reschedule()` helper.  The flag's set sites are unchanged; the consumer is new.
- **Reuses Phase 43c (Regression and Stress)** infrastructure for the user-loop stress test and the soak gate.

## Implementation Outline

1. **Track A — TDD foundation.**  Extend `kernel-core::preempt_model` with the preemption transition and property tests.  Write the IRQ-return check test stubs in `kernel/tests/`.
2. **Track B — `preempt_to_scheduler` and `preempt_resume_to_user`.**  Implement the assembly + Rust shim.  Verify the model.
3. **Track C — Dispatch integration.**  Add `Task::resume_mode`; route the dispatch path to either `switch_context` or `preempt_resume_to_user` based on the mode.
4. **Track D — Per-CPU fast path.**  Add `current_task_idx_fast` and `preempt_count_fast` atomic reads.  Update on every dispatch.
5. **Track E — IRQ-return check.**  Wire the check into `timer_handler` and `reschedule_ipi_handler`.  Gate on `cfg(feature = "preempt-voluntary")` for initial roll-out.
6. **Track F — Stress test and validation.**  Run user-loop stress.  Run the I.1 acceptance gate.  Confirm `[WARN] [sched]` lines do not appear.
7. **Track G — Default-on flip.**  Flip the feature default to on.  Run the soak gate.  Remove the feature flag in a follow-up commit.

## Acceptance Criteria

### Primary (preemption fires)

- A user-mode CPU-bound task is preempted within one timer tick (~1 ms).
- After preemption, the next runnable task on the same core dispatches within 100 µs of the preemption point (measured by tracepoint).
- The preempted task's `resume_mode == Preempted` after preemption; on next dispatch the task resumes via `iretq` from its `preempt_frame`.
- No preemption fires when `preempt_count > 0`.  An in-QEMU test confirms: a kernel-mode busy-loop with `preempt_disable()` held is not preempted.
- No preemption fires when the IRQ interrupted ring 0.  Same in-QEMU test confirms: a kernel-mode busy-loop without `preempt_disable` is not preempted (because `from_user == false`) — this is by design for `PREEMPT_VOLUNTARY`.
- `kernel-core::preempt_model` host tests cover every transition; `cargo test -p kernel-core` passes.

### Secondary (user-pain relief)

- `cargo xtask run-gui --fresh` on the user's test hardware: cursor moves on mouse motion within 1 s of motion start; keyboard input typed in the framebuffer terminal appears within 100 ms; `term` reaches `TERM_SMOKE:ready`.  (Resolves the I.1 acceptance gate independently of 57c — both phases solve it from different angles.)
- 30 minutes idle plus 30 minutes synthetic load (including a CPU-bound user-mode task on each core) on 4 cores: no `[WARN] [sched] cpu-hog` warnings whose corrected `ran` exceeds the timeslice (200 ms).
- `cargo xtask check` clean.
- `cargo xtask test` regression suite passes.

### Engineering practice

- TDD: every track has tests landed before implementation; PR commit history shows test-first ordering.
- The `preempt-voluntary` feature flag is removable in a follow-up after a 24-hour soak passes.
- `docs/03-interrupts.md` is updated to describe the new IRQ-return preemption check.
- `docs/04-tasking.md` is updated to describe the dual-resume dispatch path.

## Companion Task List

- [Phase 57d Task List](./tasks/57d-voluntary-preemption-tasks.md)

## How Real OS Implementations Differ

- **Linux's `CONFIG_PREEMPT_VOLUNTARY`** is the equivalent model: explicit reschedule points (`might_resched()`) plus user-mode-return preemption.  m3OS is closer to "user-mode-return-only" because m3OS does not have the explicit reschedule-point sprinkling Linux uses.
- **Linux's `__preempt_count_dec_and_test`** combines the decrement and zero-check.  m3OS uses `fetch_sub` + a separate read; the cost is one extra atomic operation per `preempt_enable`.  Negligible at this scale.
- **Linux's preempt-resume path uses the same `entry_64.S` IRQ-return path** with a `restore_args` macro that handles both cooperative and preempted resume.  m3OS uses two separate routines for clarity; consolidation is a follow-up optimisation.
- **seL4** does not preempt user-mode at all between scheduling boundaries — it relies on cooperative yield.  m3OS aimed for `PREEMPT_VOLUNTARY` to balance latency and complexity.

## Deferred Until Later

- **Kernel-mode preemption (`PREEMPT_FULL`)** — Phase 57e.
- **Per-CPU `preempt_count`** — deferred to a later optimisation phase if hot-path cost matters.
- **Explicit reschedule points** (`might_resched`-style) inside long kernel-mode loops — deferred; Phase 57c removes most such loops, and 57e would close the rest.
- **Priority inheritance** (`rt_mutex` equivalent) — deferred; m3OS does not yet have priority scheduling.
- **CFS / EEVDF-style fair scheduling** — orthogonal to preemption; m3OS uses round-robin with timeslices.
