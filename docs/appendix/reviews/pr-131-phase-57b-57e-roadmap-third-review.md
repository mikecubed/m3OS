# PR 131 Phase 57b-57e Roadmap Third Review

**Review date:** 2026-04-29
**Branch:** `docs/phase-57b-preemption-plan`
**PR:** 131, `docs: split Phase 57b kernel preemption into 57b/57c/57d/57e`
**Commit reviewed:** `2286a67` (`docs: address PR-131 re-review (pointer lifecycle, IDT/ABI, stale text)`)
**Prior reviews:**
- `docs/appendix/reviews/pr-131-phase-57b-57e-roadmap-review.md`
- `docs/appendix/reviews/pr-131-phase-57b-57e-roadmap-rereview.md`

## Summary

The latest commit substantially improves the roadmap. The earlier review items are now addressed in the task lists: the pointer lifecycle has explicit switch-out and switch-in tasks, 57d requires raw asm entry stubs, 57c wrapper timing is corrected, the duplicate 57d section is gone, and 57e no longer presents same-CPL `iretq` as a selector-only variant.

I would still hold the PR for two correctness issues in the roadmap guidance:

1. 57b still has contradictory text around `current_preempt_count_ptr` retargeting, including an incorrect claim that the post-`switch_context` retarget inherits an IF=0 window.
2. 57d still passes a full `&mut PreemptTrapFrame` to Rust for ring-0 interrupts before the missing `rsp` / `ss` slots are actually synthesized. That makes the 57d stub guidance unsound unless frame normalization moves into 57d.

The remaining items after those are cleanup-level precision fixes.

## Remaining Findings

### 1. Blocking: 57b still gives unsafe/contradictory pointer-retarget guidance

**Where:**
- `docs/roadmap/57b-preemption-foundation.md:54-60`
- `docs/roadmap/57b-preemption-foundation.md:158`
- `docs/roadmap/57b-preemption-foundation.md:209`
- `docs/roadmap/57b-preemption-foundation.md:219`
- `docs/roadmap/tasks/57b-preemption-foundation-tasks.md:146-158`

The new task-list lifecycle is the right direction: retarget to the scheduler dummy on switch-out, keep scheduler-context lock pairs on the dummy, then retarget to the incoming task only after scheduler-context locks are dropped and before entering the task.

However, the design doc still contains the old unsafe guidance:

- The implementation text still says the pointer is updated "between `pick_next` and `switch_context` while `SCHEDULER.lock` is still held." That is the exact ordering the prior re-review flagged, because an `IrqSafeMutex` guard can increment one pointee and decrement another.
- The acceptance criteria still say the pointer is "updated by the dispatch path between `pick_next` and `switch_context`" instead of naming the two-phase retarget: outgoing task -> scheduler dummy, scheduler dummy -> incoming task.
- The task list says the switch-out retarget "inherits `IF=0` from `switch_context`'s `cli`/`popf` window." Current `switch_context` does not provide that guarantee after it returns to the scheduler. It saves the scheduler's RFLAGS when dispatching a task and later restores those RFLAGS with `popf` when the task yields back. If the scheduler dispatched with IF=1, the scheduler resumes with IF=1 before Rust code can retarget the pointer.
- The design text says the switch-out epilogue retarget is "interrupt-masked" immediately after `switch_context` returns. That is not true with the current `switch_context` contract in `kernel/src/task/mod.rs`.

This matters because the pointer lifecycle is now the core safety mechanism for `IrqSafeMutex`. If the docs leave both the old and new orderings in place, implementers can follow a line that recreates the wrong-pointee decrement.

**Recommended fix:**

Make the design doc and task list use one precise lifecycle, with no old shortcut language:

- Switch-out: either retarget to `SCHED_PREEMPT_COUNT_DUMMY[core]` in an assembly handoff before restoring IF, or explicitly state that a fully-contained IRQ before the Rust retarget is allowed because it cannot straddle the retarget. Do not claim the current Rust-side post-return retarget inherits IF=0 unless the scheduler saved RFLAGS are intentionally made IF=0.
- Switch-in: explicitly disable interrupts before retargeting from dummy to `next_task.preempt_count`, keep them disabled through the `switch_context` call, and restore the incoming task's IF from its saved RFLAGS.
- Replace all "between `pick_next` and `switch_context` while `SCHEDULER.lock` is still held" text with "after all scheduler-context locks are dropped and before `switch_context`, in an interrupt-masked handoff window."
- Fix the cross-reference at `57b-preemption-foundation.md:60`: the retarget tasks are C.2 and C.3, and the regression test is C.4.

### 2. Blocking: 57d's `PreemptTrapFrame` is not valid for ring-0 interrupts unless 57d synthesizes the missing slots

**Where:**
- `docs/roadmap/57d-voluntary-preemption.md:51-70`
- `docs/roadmap/57d-voluntary-preemption.md:211-213`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:77`
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:91-101`
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md:106-115`

57d's asm stub must handle timer and reschedule-IPIs that interrupt both ring 3 and ring 0. The docs say `PreemptTrapFrame` is `gprs` followed by an iretq frame with `rip, cs, rflags, rsp, ss` for ring-3-interrupted code, or `rip, cs, rflags` for ring-0-interrupted code with `rsp/ss` slots zeroed.

The current 57d pseudocode does not create those ring-0 `rsp` / `ss` slots before passing `rsp` as `&mut PreemptTrapFrame` to Rust. The CPU only pushes three fields on a same-CPL interrupt. If the stub only pushes 15 GPRs, then a Rust `&mut PreemptTrapFrame` for a ring-0 interrupt covers bytes that are not part of a normalized trap frame. Even if 57d's Rust handler only reads `cs` and returns for ring-0 cases, the reference itself describes a full struct that has not been fully materialized.

The 57e task list adds "C.0 - Capture interrupted kernel RSP in 57d's asm entry stub", but that is too late if 57d already exposes a full `PreemptTrapFrame` reference to Rust. 57e can change what value goes into the `rsp` slot for full kernel preemption, but 57d needs to decide whether the slots exist at all.

**Recommended fix:**

Move frame normalization into 57d Track B:

- On entry, branch on the saved `cs & 3` before calling Rust.
- For ring-3 interrupts, use the CPU-pushed `rsp` / `ss`.
- For ring-0 interrupts, synthesize a uniform frame before constructing `&mut PreemptTrapFrame`: create `rsp` / `ss` slots, set them to zero in 57d or capture the pre-stub kernel RSP immediately if that is cheap enough, and undo any synthetic slots on the non-preempting return path before `iretq`.
- Pin this with offset tests for both ring-3 and ring-0 synthetic interrupts.

After that, 57e C.0 can be reworded as "change the 57d ring-0 synthetic `rsp` slot from zero to the captured pre-stub kernel RSP and make it load-bearing for `_kernel` resume."

### 3. High: 57d's ABI/stack-alignment detail still depends on future 57e padding

**Where:**
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:95-101`

The stack-alignment acceptance item now calls out the SysV AMD64 requirement, which is good. But the explanation says the ring-0 case has "the 2 missing slots accounted for by the entry-stub padding established in 57e Track C.0."

That creates a cross-phase correctness gap: 57d's `timer_entry` and `reschedule_ipi_entry` are live before 57e. They must call Rust correctly for ring-0 interrupts in 57d, even though 57d will not preempt ring-0 code. Stack alignment, direction-flag clearing, synthetic-slot layout, and return-stack restoration all belong to 57d Track B.

**Recommended fix:** Keep 57d self-contained. The 57d entry stubs should document exact alignment math after their own ring-0 frame normalization, without relying on 57e. 57e may extend the saved data, but should not provide padding required for 57d's Rust call to be ABI-correct.

### 4. Medium: README still overclaims 57e latency

**Where:**
- `docs/roadmap/README.md:314`

The 57e design and task list now correctly describe latency as per-trigger: cross-core IPI wakeup is the realistic large improvement, same-core wakeup is mostly no-regression, timer preemption remains tick-bounded, and `preempt_enable` zero-crossing has its own floor.

The roadmap README row still says "IPC and syscall wakeup latency floors drop >=10x." That reintroduces the over-broad claim the latest 57e text removed.

**Recommended fix:** Change the 57e README outcome to match the design doc, for example: "Cross-core IPI wakeup latency improves measurably; same-core and timer-triggered paths are benchmarked separately and must not regress."

### 5. Low: 57d raw-IDT API example names a non-existent constructor

**Where:**
- `docs/roadmap/tasks/57d-voluntary-preemption-tasks.md:127`

The task now correctly requires the raw handler-address path instead of `set_handler_fn`. The concrete example says to "build an `Entry` via `Entry::new(...).set_handler_addr(...)". With the local `x86_64 = 0.15.4` crate, `Entry` exposes `missing()` and `set_handler_addr(&mut self, VirtAddr)`, while existing IDT code should normally mutate the relevant `InterruptDescriptorTable` entry directly.

The task includes "or equivalent - confirm the exact API", so this is not a blocker. It is still worth making exact because Track B is already a fragile asm/IDT change.

**Recommended fix:** Replace the example with the actual shape expected for this repo, e.g. mutate `idt[InterruptIndex::Timer as u8]` and call unsafe `set_handler_addr(VirtAddr::new(timer_entry as usize as u64))`, preserving whatever options the current `set_handler_fn` path sets.

## Prior Re-Review Findings Status

- **57b pointer lifecycle:** partially addressed. The task list now has the right C.2/C.3/C.4 structure, but the design doc and C.2 IF-window text still contradict the safe lifecycle.
- **57d asm entry stubs need ABI and IDT-install details:** partially addressed. IDT and ABI requirements exist now, but the ring-0 frame normalization and future-phase padding dependency still need correction.
- **57c wrapper timing:** addressed. The stale 57d/57b wrapper timing language is gone.
- **57d duplicate `Task` state additions section:** addressed. The duplicate section was removed.
- **57e stale helper-name and same-CPL text:** addressed in the phase docs. The remaining README latency row is a separate summary-table cleanup.

## Verification Performed

- Re-read the changed 57b/57c/57d/57e roadmap docs and task lists at commit `2286a67`.
- Compared the fixes against the findings in `pr-131-phase-57b-57e-roadmap-rereview.md`.
- Spot-checked current `switch_context` and scheduler dispatch code to validate the IF-window assumption:
  - `kernel/src/task/mod.rs` documents that `switch_context` restores saved RFLAGS with `popf`.
  - `kernel/src/task/scheduler.rs` dispatches via `switch_context(per_core_scheduler_rsp_ptr(), task_rsp)` and resumes scheduler code after the task yields.
- Checked the local `x86_64` crate API (`0.15.4`) for raw IDT handler address support.

No build or QEMU test was run; this is a roadmap/documentation review.
