# PR 131 Phase 57b-57e Roadmap Review

**Review date:** 2026-04-29
**Branch:** `docs/phase-57b-preemption-plan`
**PR:** 131, `docs: split Phase 57b kernel preemption into 57b/57c/57d/57e`
**Scope:** `docs/roadmap/57b-preemption-foundation.md`, `57c-kernel-busy-wait-conversion.md`, `57d-voluntary-preemption.md`, `57e-full-kernel-preemption.md`, their task lists, and the roadmap README changes.

## Summary

The split is directionally good. Separating the work into foundation, busy-wait conversion, user-mode preemption, and full kernel preemption is much easier to review and stage than the previous umbrella "kernel preemption" item. The new docs also satisfy the roadmap template at a structural level: each design doc has the required sections, each task list has track layout plus per-task File/Symbol/Why/Acceptance fields, and the README table/graph/gantt are updated.

However, the roadmap currently contains several implementation-level assumptions that would send the actual phase work into unsafe or unimplementable territory. The biggest issues are:

- 57b's `preempt_disable()` example acquires `scheduler_lock()`, but the same phase wires `preempt_disable()` into `IrqSafeMutex::lock()`. Because `scheduler_lock()` itself is backed by `IrqSafeMutex`, this design is recursively deadlocking.
- 57d's plan calls `preempt_to_scheduler(&InterruptStackFrame, idx)` from inside Rust `extern "x86-interrupt"` handlers. That is too late to save the interrupted task's full GPR state; the Rust handler prologue/body can already have clobbered registers.
- 57e's kernel-mode resume design treats same-ring `iretq` as if it used the same `ss:rsp` frame as a ring-3 return. It does not. Kernel-mode preemption needs a different frame/stack story.
- 57b promises a 57d `preempt_enable()` deferred-reschedule check, but 57d has no track for it. 57e then depends on latency claims that this missing mechanism would normally provide.

I would revise PR 131 before using it as implementation guidance. The documentation can still merge as a planning split if these are explicitly called out as open design corrections, but it should not be treated as a ready task plan as written.

## What Works

- The phase decomposition is sensible: 57b is no-op infrastructure, 57c is kernel busy-wait relief, 57d is user-mode preemption, and 57e is a stretch `PREEMPT_FULL` target.
- 57c is the strongest of the four plans. Its current `spin_loop` inventory lines up with the current tree: `rg 'core::hint::spin_loop' kernel/src` finds the same known sites listed in the 57c task doc, including SMP, IOMMU, PS/2, APIC, RTC, allocator, scheduler, and sub-1 ms nanosleep spins.
- The docs do a good job requiring durable audit artifacts in `docs/handoffs/`, not just ad hoc code comments.
- The validation gates are appropriately concrete: QEMU tests, `cargo xtask check`, `cargo xtask test`, real-hardware graphical checks, SSH reconnect loops, and soak windows.
- The README update replaces the old monolithic 57b row with the new 57b/57c/57d/57e rows and updates the dependency graph.

## Blocking Findings

### 1. 57b's `preempt_disable()` design recurses through `scheduler_lock()`

**Where:**
- `docs/roadmap/57b-preemption-foundation.md:103-121`
- `docs/roadmap/57b-preemption-foundation.md:123-125`
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:103-114`
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:173-184`
- Current code: `kernel/src/task/scheduler.rs:169-180`, `kernel/src/task/scheduler.rs:272-280`

The 57b design sample implements:

```rust
pub fn preempt_disable() {
    if let Some(idx) = get_current_task_idx() {
        let sched = scheduler_lock();
        sched.tasks[idx].preempt_count.fetch_add(1, Acquire);
    }
}
```

The same phase then requires `IrqSafeMutex::lock()` to call `preempt_disable()` before disabling interrupts. In the current kernel, `SCHEDULER_INNER` is an `IrqSafeMutex<Scheduler>` and `scheduler_lock()` immediately calls `SCHEDULER_INNER.lock()`. Once D.1 wires `preempt_disable()` into `IrqSafeMutex::lock()`, any attempt to acquire `scheduler_lock()` enters:

`scheduler_lock()` -> `IrqSafeMutex::lock()` -> `preempt_disable()` -> `scheduler_lock()` -> ...

This deadlocks or recurses before the first scheduler lock acquisition completes. It also means `SchedulerGuard` cannot "inherit the discipline" until `preempt_disable()` can update the current task without taking the scheduler lock.

**Recommended fix:**

Move the lock-free current-task access requirement into 57b, before D.1. Options:

- Add a per-core raw pointer to the current `Task::preempt_count`, updated during dispatch and cleared for the scheduler/idle context.
- Make task storage stable first, such as `Vec<Box<Task>>` or another non-moving allocation, then store a current task pointer in `PerCoreData`.
- Use the existing `PerCoreData::current_task_idx` only as an index, but do not use it to index `Scheduler.tasks` without resolving the stable-storage/reallocation problem.

57d's `current_task_idx_fast` track is too late. 57b needs the lock-free preempt-count access before `IrqSafeMutex` can be modified.

### 2. 57d cannot save interrupted GPRs from inside the Rust interrupt handler body

**Where:**
- `docs/roadmap/57d-voluntary-preemption.md:40-55`
- `docs/roadmap/57d-voluntary-preemption.md:61-69`
- `docs/roadmap/57d-voluntary-preemption.md:144-160`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:80-92`
- Current code: `kernel/src/arch/x86_64/interrupts.rs:794`, `kernel/src/arch/x86_64/interrupts.rs:1085`

57d plans to call `preempt_to_scheduler(&stack_frame, idx)` from `timer_handler` and `reschedule_ipi_handler`, both currently Rust `extern "x86-interrupt"` functions. The docs say the routine saves all GPRs plus the `iretq` frame into `Task::preempt_frame`.

That call site is too late to capture the interrupted program's register state. The CPU-created interrupt frame gives the return frame, but not all GPRs. The Rust interrupt ABI and compiler-generated handler body are free to use and clobber caller-saved registers before the explicit preemption check runs. By the time `preempt_to_scheduler` is called, saving `rax`, `rcx`, `rdx`, `rsi`, `rdi`, and `r8..r11` captures the handler's state, not the interrupted task's state.

This is a correctness blocker for 57d. A resumed user task would eventually return with corrupted registers.

**Recommended fix:**

57d should require an assembly interrupt-entry wrapper or equivalent naked/asm path that saves all GPRs immediately on entry, before calling any Rust. The Rust handler can then receive a pointer to a complete trap frame and decide whether to preempt. In practical terms:

- Introduce a real `TrapFrame`/`PreemptTrapFrame` layout that includes GPRs and the CPU return frame.
- Route timer and reschedule IPI through an assembly stub that saves GPRs, calls a Rust "handle timer/reschedule" function, and either restores/returns or transfers to the scheduler.
- Update 57b's `PreemptFrame` layout tests to match the actual assembly-created frame, not only an abstract `InterruptStackFrame`.

The current task list should not imply that a plain Rust shim receiving `&InterruptStackFrame` can recover the full interrupted register file.

### 3. 57e's kernel-mode `iretq` frame is wrong for same-CPL return

**Where:**
- `docs/roadmap/57b-preemption-foundation.md:56`
- `docs/roadmap/57e-full-kernel-preemption.md:70-75`
- `docs/roadmap/57e-full-kernel-preemption.md:126-128`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:106-116`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:130-138`

57e says `preempt_resume_to_kernel` mirrors `preempt_resume_to_user` and "pushes kernel-mode `cs:ss` selectors". That is not how same-privilege `iretq` works on x86-64. A ring-3 return pops `RIP`, `CS`, `RFLAGS`, `RSP`, and `SS` because it changes privilege level. A same-ring ring-0 return pops only `RIP`, `CS`, and `RFLAGS`; it does not pop `RSP` and `SS` as part of a privilege transition.

This also affects the saving side. For an interrupt that arrives while already in ring 0, the CPU does not provide the same user-mode `ss:rsp` frame that 57d relies on. The kernel preemption path must capture the interrupted kernel stack pointer explicitly and resume with a same-CPL frame layout.

**Recommended fix:**

Rewrite 57e Track C around two genuinely different resume cases:

- Ring-3 preemption: restore GPRs and return with the privilege-changing `iretq` frame.
- Ring-0 preemption: restore the saved kernel stack pointer and GPRs, then build/use the same-CPL `iretq` frame containing only `rip`, `cs`, and `rflags` in the right place.

The docs should stop saying the `_kernel` variant simply shares the same `iretq` core with different segment selectors. The shared code can still restore common GPRs, but the final stack frame and RSP handling are different.

### 4. The promised deferred reschedule on `preempt_enable()` is missing from 57d

**Where:**
- `docs/roadmap/57b-preemption-foundation.md:36`
- `docs/roadmap/57d-voluntary-preemption.md:218-226`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:186-228`
- `docs/roadmap/57e-full-kernel-preemption.md:88-94`

57b explicitly says that in 57d, `preempt_enable()` checks the post-decrement value against zero and fires a deferred reschedule if `reschedule` is set. That is the Linux-style `preempt_enable()` behavior that closes the "IRQ arrived while preemption disabled" window.

57d does not have a track for this. Its only activation path is the timer/reschedule-IPI IRQ-return check. If an IRQ sets `reschedule` while `preempt_count > 0`, the docs say the IRQ skips preemption. When the lock later drops the count to zero, nothing in 57d schedules immediately. The task continues until a future interrupt or voluntary yield.

This weakens 57d and undercuts 57e's latency claims. Full preemption normally needs both:

- IRQ-return preemption when an interrupt catches code at `preempt_count == 0`.
- `preempt_enable()` scheduling when the need to reschedule was deferred while the count was nonzero.

**Recommended fix:**

Add a 57d or 57e track for `preempt_enable()`'s zero-crossing path:

- On `fetch_sub`, if the old count was 1 and `per_core().reschedule` is set, enter a safe scheduler path.
- Define where this path is allowed to run in 57d. For `PREEMPT_VOLUNTARY`, it may need to skip kernel-mode scheduling unless explicitly at a safe return boundary.
- For 57e, make this path part of the headline behavior and test it separately from IRQ-return preemption.

Without this, the roadmap should remove the promise from 57b and lower 57e's latency expectations.

## High Findings

### 5. 57e's "10 us latency floor" acceptance is not supported by the planned trigger path

**Where:**
- `docs/roadmap/57e-full-kernel-preemption.md:11`
- `docs/roadmap/57e-full-kernel-preemption.md:88-94`
- `docs/roadmap/57e-full-kernel-preemption.md:182-186`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:170-191`

57e says round-trip IPC and syscall wakeup latency floors drop from about 1 ms to about 10 us by dropping the `from_user` check. That may be true for some cross-core wakeups if the target receives a reschedule IPI while running preemptible kernel code. It is not guaranteed by the plan as written.

For same-core wakeups, or wakeups where no immediate interrupt targets the running code, dropping `from_user` still leaves scheduling driven by the 1 kHz timer or voluntary points. The current 57e plan does not add self-IPI, explicit post-wake reschedule points, or the `preempt_enable()` zero-crossing scheduler path discussed above.

**Recommended fix:**

Revise the benchmark acceptance to separate:

- Cross-core reschedule IPI wakeup latency.
- Same-core wakeup latency.
- Timer-only preemption latency.
- `preempt_enable()` deferred-reschedule latency.

Keep the >=10x target only for cases where the implementation actually provides an immediate trigger. Otherwise, require measured baselines and phrase the target as "improves over the 57d baseline by a measured factor" rather than hard-coding 10 us.

### 6. 57b's spinlock audit grep is not a complete source of truth

**Where:**
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:35-48`
- Current examples: `kernel/src/stdin.rs:8`, `kernel/src/stdin.rs:54`, `kernel/src/process/mod.rs:765`, `kernel/src/arch/x86_64/syscall/mod.rs:12003`

The task list says `git grep -E 'spin::Mutex|spin::RwLock|IrqSafeMutex' kernel/ kernel-core/` is the source of truth. That catches type references and imports, but it does not reliably enumerate actual acquisition sites. Many files import `use spin::Mutex`, declare aliases, wrap locks in `Lazy`, put locks behind `Arc`, or call `.lock()` on fields whose type is not visible on the same line.

The safety property is about lock acquisition, not declaration. A declaration-only grep can miss call paths that need wrappers and can include declarations that are never acquired in task context.

**Recommended fix:**

Update A.1 to require both declaration and acquisition scans:

- `rg -n '\.lock\(|\.try_lock\(|\.read\(|\.write\(' kernel/src kernel-core/src`
- `rg -n 'spin::Mutex|spin::RwLock|use spin::Mutex|use spin::RwLock|IrqSafeMutex|BlockingMutex|Lazy<Mutex|Arc<Mutex' kernel/src kernel-core/src`

Then classify callsites by context:

- Task-only.
- IRQ-only.
- Shared task/IRQ, requiring interrupt masking as well as preempt discipline.
- Host-test-only or pure-logic.

This also keeps preempt discipline separate from IRQ safety. `preempt_disable()` is not a substitute for masking same-core interrupts when a task shares a plain `spin::Mutex` with an ISR.

### 7. 57c's wrapper timing is inconsistent with 57d's user-only preemption model

**Where:**
- `docs/roadmap/57c-kernel-busy-wait-conversion.md:21-25`
- `docs/roadmap/57c-kernel-busy-wait-conversion.md:66-82`
- `docs/roadmap/57c-kernel-busy-wait-conversion.md:180-186`
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:380-386`

57c is correctly independent of 57b for block/wake conversions and bound annotations. But it repeatedly says preempt-disable wrappers are needed "for 57d safety" or in a 57b/57c integration commit. 57d explicitly gates on `from_user`, so kernel-mode busy-spins cannot be preempted by 57d. Those wrappers become load-bearing in 57e, not 57d.

This is not just wording. It affects dependency planning:

- If wrappers are added immediately after 57b, they are dead weight until 57e.
- If wrappers are deferred to 57e, 57c can stay truly independent and focused on behavior-changing busy-wait conversions and annotations.

**Recommended fix:**

State the dependency plainly:

- 57c comments classify which spins will require wrappers under 57e.
- 57e Track B adds the wrappers.
- A 57b/57c integration commit is optional cleanup, not required for 57d.

## Medium Findings

### 8. 57b forbids extra `Task` flag fields, then adds one

**Where:**
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:21-29`
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:131-140`

The 57b engineering gates say no new `Task` flag fields beyond `preempt_count` and `preempt_frame`. B.4 then adds `preempt_logged_nonzero: AtomicBool` to `Task`.

**Recommended fix:** Either allow this diagnostic field explicitly, make it debug-only, or move the "logged once" state to a separate diagnostic map/ring. Also consider whether this field belongs in 57b at all; the debug assertion may be enough for a no-op foundation phase.

### 9. 57d duplicates an existing current-task index concept

**Where:**
- `docs/roadmap/57d-voluntary-preemption.md:104-110`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:159-182`
- Current code: `kernel/src/smp/mod.rs:181-185`, `kernel/src/task/scheduler.rs:331-346`

The docs plan to add `PerCoreData::current_task_idx_fast`, but current `PerCoreData` already has `current_task_idx: AtomicI32`, with `get_current_task_idx()` and `set_current_task_idx()` helpers. The real missing piece is not a second fast index; it is safe lock-free access to the current task's preempt count/frame without indexing a movable `Vec<Task>` under no lock.

**Recommended fix:** Reframe 57d Track D around the actual missing primitive: stable current-task access. If a new field is needed, make it a `current_task_ptr` or `current_preempt_count_ptr`, not a duplicate index.

### 10. 57c has one stale helper name

**Where:**
- `docs/roadmap/57c-kernel-busy-wait-conversion.md:54-59`
- `docs/roadmap/tasks/57c-kernel-busy-wait-conversion-tasks.md:80-88`

The design doc says `net_task` currently uses `block_current_unless_woken`; the task list says `block_current_until(&NIC_WOKEN, None)`. The latter matches the phase vocabulary and current scheduler primitive.

**Recommended fix:** Replace `block_current_unless_woken` with `block_current_until`.

### 11. "Planned (Stretch)" does not match the template's status enum

**Where:**
- `docs/roadmap/57e-full-kernel-preemption.md:3`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:3`
- Template: `docs/appendix/doc-templates.md`

The roadmap template lists `Complete | In Progress | Planned`. `Planned (Stretch)` is understandable, but it is not one of the template values.

**Recommended fix:** Use `**Status:** Planned` and put "Stretch goal" in the milestone goal or first paragraph. This keeps machine/human scans consistent.

## Per-Phase Notes

### Phase 57b

The phase is valuable but needs a corrected foundation before implementation:

- Move lock-free current-task/preempt-count access into 57b.
- Do not call `scheduler_lock()` from `preempt_disable()` or `preempt_enable()`.
- Make task storage stability an explicit prerequisite if any IRQ/lock path reads `Task` fields without the scheduler lock.
- Broaden the spinlock audit to actual acquisition sites and classify IRQ-shared locks separately.
- Resolve the `preempt_logged_nonzero` field conflict.

The existing no-op-refactor framing is right. The implementation mechanics need to be changed so the refactor remains no-op and does not deadlock at the first scheduler lock.

### Phase 57c

This is the most ready phase. It is scoped as a true audit/conversion phase and is useful even if preemption is delayed. The main revisions are:

- Fix the stale helper name.
- Clarify that preempt-disable wrappers are for 57e, not 57d.
- Expand the audit pattern beyond `spin_loop` and `while !load` if the phase intends to catch bounded mutex/spin behavior, but keep the current known-site list because it matches the current tree.

### Phase 57d

The user-mode preemption goal is right, but the implementation plan must be reworked around a real trap-frame path:

- Timer/reschedule IPI entry must save GPRs before Rust code runs.
- The preemption decision should operate on a complete saved frame.
- Dispatch must distinguish cooperative and preempted tasks, but the resume mode design should be backed by tests that validate actual register preservation, not only state transitions.
- Add or explicitly defer the `preempt_enable()` zero-crossing reschedule behavior promised by 57b.

### Phase 57e

The stretch framing is appropriate, but the technical content needs the most revision:

- Kernel-mode same-CPL interrupt return cannot share the ring-3 `ss:rsp` frame shape.
- Dropping `from_user` is not the only practical difference once same-ring frame handling, current kernel RSP capture, migration/per-CPU data safety, and deferred reschedule are accounted for.
- The 10 us latency target should be tied to specific trigger mechanisms and measured baselines.
- The per-CPU data access audit is important and should remain.

## Template Compliance

- Design docs: all four have Status, Source Ref, Depends on, Builds on, Primary Components, Milestone Goal, Why This Phase Exists, Learning Goals, Feature Scope, Important Components, How This Builds, Implementation Outline, Acceptance Criteria, Companion Task List, Real OS Differences, and Deferred Until Later.
- Task docs: all four have Status, Source Ref, Depends on, Goal, Track Layout, per-track task sections with File/Symbol/Why/Acceptance, and Documentation Notes.
- README: the roadmap summary rows and graph/gantt were updated for the split.
- Minor exception: 57e uses `Planned (Stretch)` instead of the template's plain `Planned`.

## Recommended PR 131 Edits Before Merge

1. Replace the 57b `preempt_disable()` example with a lock-free current-task mechanism and add a task for stable current-task access before `IrqSafeMutex` integration.
2. Add a 57d track for an assembly interrupt-entry/trap-frame path that saves all GPRs before Rust handler code.
3. Rewrite 57e's kernel-mode resume section to account for same-CPL `iretq` frame shape and interrupted kernel RSP capture.
4. Add a 57d/57e task for `preempt_enable()` deferred-reschedule behavior, or remove the promise and adjust latency expectations.
5. Reword 57e latency acceptance to distinguish timer, cross-core IPI, same-core wakeup, and deferred-reschedule cases.
6. Broaden the 57b lock audit source-of-truth to actual acquisition sites and IRQ-shared lock classification.
7. Clarify that 57c's preempt-disable wrappers are load-bearing for 57e, not for 57d.
8. Fix minor consistency issues: `block_current_unless_woken`, `Planned (Stretch)`, and the B.4 diagnostic field conflict.

## Verification Performed

- Compared PR 131 branch against `origin/main`: nine docs changed, all under `docs/roadmap/` plus README.
- Confirmed PR metadata with `gh pr view 131`.
- Read the roadmap template in `docs/appendix/doc-templates.md`.
- Read all 57b/57c/57d/57e design docs and task lists.
- Spot-checked current kernel internals relevant to the roadmap:
  - `IrqSafeMutex`, `SchedulerGuard`, `scheduler_lock`, and `get_current_task_idx` in `kernel/src/task/scheduler.rs`.
  - `Task` layout and current inline `switch_context` assembly in `kernel/src/task/mod.rs`.
  - `PerCoreData::current_task_idx` in `kernel/src/smp/mod.rs`.
  - Current Rust `extern "x86-interrupt"` timer and reschedule IPI handlers in `kernel/src/arch/x86_64/interrupts.rs`.
  - Current `core::hint::spin_loop` sites in `kernel/src/`.

No build or QEMU test was run because this was a roadmap/documentation review.
