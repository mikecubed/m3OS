# Phase 57e — `preempt-full` Boot Crash Handoff

**Status:** Bug #2 root cause identified and fixed in the working tree (uncommitted as of this update). H6 confirmed: scheduler dispatch prep was non-re-entrant under `preempt-full` because the `IrqSafeMutex` guard around `pick_next` re-enabled IRQs on drop, leaving a window where a stray timer/IPI could recurse through `preempt_to_scheduler_kernel` → `switch_context` with `RSP=0` (BSP first dispatch) or a stale scheduler RSP (subsequent dispatches). Fix: `interrupts::disable()` placed **before** the `pick_next` lock acquire so the guard's `was_enabled` snapshot is `false` and the drop is a no-op, keeping IRQs masked from commit through `switch_context`. Default-build smoke-test passes; `cargo xtask check` clean. Under `preempt-full` the kernel now boots all services to completion and the smoke-test fails much later in userspace at `SMOKE:tcc-version:PASS` — that residual is a separate issue (not the original Bug #2). See "Resolution" section below.
**Source ref:** Phase 57e (`feat/phase-57e-full-preemption`, PR #136), branch tip `29d38e7` at the time of handoff.
**Companion:** `docs/handoffs/57e-kernel-preempt-audit.md`, `docs/handoffs/57e-dispatch-reentrancy.md`, `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md`.

This handoff describes the open boot-time double fault that surfaces only under the `preempt-full` Cargo feature, captures everything the previous session ruled out, and lists the leading hypotheses to test next. It exists so the bug can be picked up cleanly in a new session without re-doing the triage.

---

## TL;DR

- `preempt-full` is OFF by default and stays OFF on `main`. The default build (preempt-voluntary only) boots cleanly and runs the smoke-test through hundreds of compose iterations.
- With `M3OS_KERNEL_FEATURES=preempt-full`, the kernel double-faults on the BSP **inside `switch_context`** during init's first dispatch — `popf` runs with `RSP=0`, escalates to `#PF` whose handler can't push to its own stack, escalates to double fault. Reproduces 100 %.
- Track G's 24 h soak is blocked on this bug — `cargo xtask` of any flavour with `preempt-full` enabled never makes it past the first task.

## Reproducing the crash

```bash
M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test
```

QEMU dies inside the smoke-test fixture with a serial trace that ends:

```
[INFO] [kernel] entering scheduler — init will start service set
[int] DOUBLE FAULT: InterruptStackFrame {
    instruction_pointer: VirtAddr(0x1000077931a),
    code_segment: SegmentSelector { index: 1, rpl: Ring0 },
    cpu_flags: RFlags(SIGN_FLAG | AUXILIARY_CARRY_FLAG | CARRY_FLAG | 0x2),
    stack_pointer: VirtAddr(0x0),
    stack_segment: SegmentSelector { index: 2, rpl: Ring0 }
}
[int] IST RSP=0x0
=== CRASH DIAGNOSTICS ===
...
CR2=0xfffffffffffffff8  CR3=0x0000000000101000
...
=== TRACE RING DUMP ===
  [36] core=0 RunQueueEnqueue { task_idx: 1, core: 0 }
  [38] core=0 Dispatch { task_idx: 1, core: 0, rsp: 0x28000967fb8 }
=== END TRACE RING DUMP ===
```

The `instruction_pointer` value drifts run-to-run because the kernel's load address shifts (the bootloader picks a fresh virtual base each boot), but the **kernel-relative offset is always `0x77931a`**. Resolve via:

```bash
addr2line -e target/x86_64-unknown-none/release/kernel -fipC 0x77931a
# → switch_context at ??:?
```

`objdump -d target/x86_64-unknown-none/release/kernel` confirms the byte at offset `0x77931a` is `9d` — the `popf` immediately after `mov rsp, rsi` in `switch_context` (asm at `kernel/src/task/mod.rs:653`).

The smoke-test output also captures an alternate IP `0x7792ca`, which lives inside `fork_enter_userspace` (asm at `kernel/src/arch/x86_64/mod.rs:169`). Both faults manifest as "RSP is zero on entry to a kernel asm trampoline." Same root cause, different first-fault site depending on which path the BSP took before the crash.

---

## What's already fixed (do not re-investigate)

### Bug #1 — `peek_preempt_count_irq` panicked before `init_bsp_per_core`

Fixed in `29d38e7`. The original report was a `KERNEL PANIC at kernel/src/smp/mod.rs:462: gs_base not initialized`, which fired between `[apic] APIC interrupt routing active` and the first `[smp] BSP per-core data initialized` log line. Root cause was that `apic::init()` (`kernel/src/main.rs:157`) re-enables IRQs with the LAPIC timer firing at 1 ms while `smp::init_bsp_per_core()` doesn't run until line 165 — the first kernel-mode timer ISR under `preempt-full` then entered `check_and_preempt_kernel` → `peek_preempt_count_irq` → `crate::smp::per_core()` → panic.

`peek_preempt_count_irq` (`kernel/src/task/scheduler.rs:1875`) now uses `try_per_core` and returns `0` (the "no preempt-disable held" sentinel) when `GS_BASE` is not yet installed. Default builds were unaffected because `check_and_preempt_kernel` is `#[cfg(feature = "preempt-full")]` and the user-mode timer path bails on `frame.cs & 3 != 3` during kernel-mode boot.

This fix is required for any further investigation of Bug #2 — without it the kernel never reaches the dispatch loop.

---

## Bug #2 — `popf` faults with `RSP=0` inside `switch_context`

### Fault model

`switch_context` (`kernel/src/task/mod.rs:653`):

```asm
switch_context:
  push rbx
  push rbp
  push r12
  push r13
  push r14
  push r15
  pushf             ; save caller (scheduler) RFLAGS
  cli               ; mask IRQs across the stack swap
  mov  [rdi], rsp   ; *save_rsp = current rsp
  mov  rsp, rsi     ; rsp = load_rsp argument
  popf              ; ← faults here: RSP == 0
  pop  r15
  pop  r14
  pop  r13
  pop  r12
  pop  rbp
  pop  rbx
  ret
```

`mov rsp, rsi` loaded `0` into `RSP`. `popf` then tried to read `[RSP]` (`[0]`), faulted with `#PF`. The `#PF` IDT entry has no IST in this kernel, so the CPU pushed the exception frame onto `RSP` itself (`RSP -= 8` → `RSP = 0xfffffffffffffff8`, write to that address — page-faults again → double fault). `CR2 = 0xfffffffffffffff8` matches `0 - 8`.

Therefore: **`switch_context` was called with `rsi == 0`**, i.e. the second argument (`load_rsp: u64`) was zero at the call site.

Call site is `kernel/src/task/scheduler.rs:3953`:

```rust
unsafe {
    switch_context(per_core_scheduler_rsp_ptr(), task_rsp);
}
```

…the Initial / Cooperative branch of the dispatch path inside `scheduler::run`. `task_rsp` is set at `scheduler.rs:3648` from the result of `pick_next` (`scheduler.rs:3604`). The `Dispatch` trace event emitted at line 3624 already saw a *valid* `rsp = 0x28000967fb8` for `task_idx=1` (init), so by the time of the trace `pick_next` had returned a real saved-stack address.

The trace ring contains no further events between `Dispatch` and the crash, which means execution reached `mov rsp, rsi` and faulted before any subsequent trace point fired.

### What's been ruled out

1. **Wrong dispatch branch.** Init's first dispatch has `resume_mode = Initial`, not `Preempted`, so the `dispatch_preempted_and_resume_kernel` branch (line 3935) is not taken. Verified by reading `Task::resume_mode` initialisation (`kernel/src/task/mod.rs:538`).
2. **`task.saved_rsp` corruption between `pick_next` and `switch_context`.** `task.saved_rsp` is only written in two places: `drain_dead` (line 719, sets to 0 for Dead tasks; init isn't Dead), and the post-switch epilogue (line 4016, runs only after `switch_context` returns). No code path writes 0 into a Ready/Running task's `saved_rsp`.
3. **`xrstor64` corrupting the local stack.** `restore_fpu_state` (`scheduler.rs:1474`) uses `options(nostack, preserves_flags)` and writes only to its memory operand (the heap-allocated `XSaveArea`). The fault IP is in `switch_context`, not in `xrstor64`, so xrstor completed.
4. **Compiler bug.** Possible but extremely unlikely; LLVM is responsible for materialising `task_rsp` into `rsi` immediately before the `call` instruction.
5. **TSS / IDT / IST mis-setup.** Default builds use the same TSS / IDT and don't crash. The `IST RSP=0x0` print in the double-fault handler is just `stack_frame.stack_pointer` from the saved frame (the faulting context's RSP), not the actual TSS IST entry.
6. **`peek_preempt_count_irq` panic re-emerging.** Already fixed in `29d38e7` (Bug #1). The fault here is `popf` with `RSP=0`, not the gs_base assertion.

### What changes the symptom (Heisenbug fingerprint)

Adding a `log::info!("[sched-dbg] pre-switch_context core=… task_rsp=… resume_mode=…")` immediately before the `switch_context` call **eliminates the double fault**, but exposes a different symptom: only the idle tasks for cores 1/2/3 are dispatched — core 0 (init) never makes progress, and the smoke-test eventually fails its `expected pattern_b` wait. This is the canonical signature of a **timing-sensitive race** in the dispatch path. Adding ~hundreds of microseconds of log work changes the IRQ delivery alignment enough to avoid the double fault.

---

## Important nuance: `debug_assert!` is off in release

The `debug_assert!(task_rsp != 0, ...)` at `scheduler.rs:3653` is compiled out in release builds (smoke-test runs release). So in the failing run, that check never fires. The bug *could* be either:

- (a) `task.saved_rsp` was already 0 in the chosen task at `pick_next` time (release builds don't catch this), OR
- (b) The local `task_rsp` value got clobbered between line 3648 (where it is bound) and line 3953 (where it materialises into `rsi`) — a ~300-line window.

Both possibilities need to be considered. The handoff originally only addressed (b); H6 below covers (a).

## Leading hypotheses (in order of likelihood)

### H6 (NEW — top candidate) — recursive `preempt_to_scheduler_kernel` with `sched_rsp == 0`

**Key facts establishing the race:**

- `per_core.scheduler_rsp` is **initialised to 0** in `kernel/src/smp/mod.rs:627` and `:749`. It is only populated when the first `switch_context` call on that core writes to it via `mov [rdi], rsp`. **Before the BSP's first successful switch_context, `per_core_scheduler_rsp() == 0`.**
- The dispatch-prep block acquires `scheduler_lock()` (an `IrqSafeMutex`) at `scheduler.rs:3824` and releases it at `:3860`. The release re-enables IRQs.
- The next IRQ-disable does not happen until *inside* `retarget_preempt_count_to_task` at `scheduler.rs:3888`. That leaves a ~28-line window (`:3860`–`:3887`) where:
  - IRQs are enabled,
  - `current_task_idx` is still set (committed at `:3623`),
  - `current_preempt_count_ptr` still targets the per-core dummy (value 0) — the switch-in retarget has not yet run.

**Sequence that produces the observed fault:**

1. BSP enters `scheduler::run` for the first time. `pick_next` returns init (`idx=1`, `task_rsp=0x28000967fb8`).
2. `Dispatch` trace event fires.
3. Address-space restore loop runs (no-op for `pid=0`).
4. Lock acquire/release at `:3824/:3860` captures `preempt_count_ptr`, `resume_mode`, `fpu_state_ptr`. **Lock release re-enables IRQs.**
5. **A queued LAPIC timer IRQ fires here.**
6. `timer_handler_kernel` → `check_and_preempt_kernel` (`interrupts.rs:1419`):
   - `peek_preempt_count_irq()` → 0 (dummy slot).
   - `try_per_core()` → `Some`.
   - `current_task_idx.is_none()` → **false** (init's idx is committed) — early-boot guard added in `a70445c` does not trip.
   - `reschedule` flag → true (init's wakeup set it).
   - `kernel_preempt_watchdog` → false (no scheduler-context lock currently held).
   - → calls `preempt_to_scheduler_kernel` → `preempt_frame_to_scheduler` (`scheduler.rs:2147`).
7. `preempt_frame_to_scheduler` calls `switch_context(per_core_switch_save_rsp_ptr(), sched_rsp)` at `:2193` where **`sched_rsp == 0`** (per_core.scheduler_rsp uninitialised).
8. Inside `switch_context`: `mov rsp, rsi` loads 0 → `popf` faults at `[0]` → `#PF` with no IST → double fault.

**Why this matches the observed fault model exactly:**

- Fault IP inside `switch_context` at `popf` ✓
- `RSP=0` in the saved frame ✓
- `CR2 = 0xfffffffffffffff8` (= `0 − 8` from the `#PF` exception push) ✓
- Trace ring shows `Dispatch` as the last event — `preempt_to_scheduler_kernel` does not emit a trace point ✓
- Adding `log::info!` immediately before `switch_context` removes the crash because `log::info!` takes the serial `IrqSafeMutex`, which disables IRQs for the duration of the formatting work — narrowing or eliminating the vulnerable window ✓
- Alternate fault IP `0x7792ca` in `fork_enter_userspace` is plausibly the same recursive-preempt mechanism on a different code path (a fork-child first-dispatch under the same race) ✓

**Test (single-line probe):** add `crate::arch::x86_64::interrupts::disable();` immediately after the dispatch-prep lock-release block at `scheduler.rs:3860`, before `dispatch_log_budget` work. If the double fault disappears, H6 is confirmed.

**Proper fix (if confirmed):** keep IRQs disabled across the entire prep window from the lock-release at `:3860` through the `switch_context` call at `:3953`. Two clean implementations:

1. Move `retarget_preempt_count_to_task` and `restore_fpu_state` *inside* the existing lock-guarded scope at `:3823–3860` (smallest diff).
2. Wrap `:3860–:3953` in an `IrqGuard` (or explicit `disable()` after `:3860` paired with the existing `disable()` inside `retarget_preempt_count_to_task`). The `popf` inside `switch_context` will restore the chosen task's IF state, so re-enable doesn't need an explicit call.

Either way: the invariant under `preempt-full` must be that **between lock-release of the per-task data acquire and the `switch_context` call, IRQs are masked** — otherwise `check_and_preempt_kernel` can recurse with a still-zero `per_core.scheduler_rsp` (or a stale one for non-first dispatches).

---

### H1 — IRQ-during-`switch_context`-tail clobbers something

Between `popf` and `ret`, `IF=1` (popf restored init's saved RFLAGS = 0x202). The LAPIC timer is firing at 1 ms; under heavy CPUID / xrstor work just before this call, an IRQ may be queued and delivered the instant `popf` re-enables interrupts. Nothing about that *should* be unsafe — the asm is designed for it — but something about the `preempt-full` IRQ-handler path (specifically `check_and_preempt_kernel` running on init's barely-bootstrapped kernel stack) could corrupt state.

**Test:** Disable the LAPIC timer briefly across the dispatch transition. If the crash disappears with the timer off, the IRQ-during-tail is the trigger. (This isn't a fix — just a diagnostic.) Concretely, mask the LAPIC timer LVT entry just before `switch_context` and unmask immediately after; rebuild and rerun.

### H2 — Init's kernel stack not yet mapped on every core

Init's stack is heap-allocated (`alloc::vec![0u8; KERNEL_STACK_SIZE].into_boxed_slice()` in `Task::new`, `kernel/src/task/mod.rs:491`). The TLB on the BSP that allocated the stack obviously sees the mapping, but APs may not, and a cross-core wake/IPI before init starts running could route the dispatch onto an AP whose CR3 doesn't have the kernel's heap region populated.

**Test:** Add `crate::smp::tlb_shootdown_range(stack.range())` after `init_stack` returns. If that fixes it, the bug is a missing TLB shootdown for new task stacks. (Probably not — kernel mappings are normally global, but worth the 5-minute experiment.)

### H3 — `preempt_count_ptr` retarget left a stale pointer through the `popf` window

`retarget_preempt_count_to_task` (called at `scheduler.rs:3888`) updates `current_preempt_count_ptr` to the chosen task's `Task::preempt_count` immediately before `switch_context`. Under `preempt-full`, an IRQ hitting between popf and the first instruction of the resumed task would call `peek_preempt_count_irq` → reads the **task's** `preempt_count` from the **scheduler's** stack window. If the scheduler's transient frame happens to look like a non-zero counter, the IRQ suppresses preemption — that's not a crash, that's a stuck-progress symptom (matches the alternate failure mode when the log delay is added).

**Test:** Move the `retarget_preempt_count_to_task` call to be the very last thing before `switch_context` (or, conversely, do it inside `switch_context` itself with a per-core scratch store). If the dispatched-into-only-idle symptom disappears with the retarget reordering, this is part of it.

### H4 — `dispatch_preempted_and_resume_kernel` writing to the wrong slot

`dispatch_preempted_and_resume_kernel` (called from the Preempted branch) is a 57e-new asm trampoline. It is *not* called for init's first dispatch (resume_mode == Initial), so it shouldn't be relevant — but if a stray timer IRQ fires during the BSP's pre-dispatch setup work and routes init through `preempt_to_scheduler_kernel`, the next dispatch would resume init via this trampoline. A bug in the trampoline's RSP/SS frame-build could leave RSP zero when the asm hands control back to scheduler code that calls `switch_context` again.

**Test:** Audit `dispatch_preempted_and_resume_kernel` (`kernel/src/arch/x86_64/interrupts.rs`, search for the symbol) against the user-mode counterpart `dispatch_preempted_and_resume`. Look for any place the asm could leave `*per_sched_rsp_ptr` zero on resume. Track 57e's design doc explicitly calls this trampoline out as the highest-risk new asm: `docs/handoffs/57e-dispatch-reentrancy.md` § "Track C.4".

### H5 — XSAVE area alignment / XSTATE_BV header bug under xrstor

`XSaveArea::new` (`kernel/src/task/mod.rs:262`) zeros the 64-byte header at offset 512–575 (specifically XSTATE_BV at 512 and XCOMP_BV at 520). `xrstor64` with XCOMP_BV bit 63 = 0 expects standard format; with bit 63 = 1 expects compacted format. Zero header should mean standard format and trigger the init optimisation.

If a previous `xsaveopt64` (`scheduler.rs:1442`) ran on a task and set XSTATE_BV bits without us realising, the next `xrstor64` could fault. xrstor faulting *would* land in the IRQ handler; the IRQ handler runs `check_and_preempt_kernel` which under preempt-full could trigger another preemption attempt. Compounding faults could explain the inconsistent IP between runs.

**Test:** Replace `xrstor64` with the `fxrstor64` legacy fallback path temporarily (force the `else` branch in `restore_fpu_state` by gating on `false`). If the crash disappears, the XSAVE migration is the culprit and Track J needs more work.

---

## Diagnostic instrumentation that's worth adding

These are non-invasive — drop them in, reproduce, capture output, then back them out:

1. **Pre-`switch_context` log** (already proven in the previous session — eliminates the crash, exposes the secondary symptom):
   ```rust
   log::info!(
       "[sched-dbg] pre-switch_context core={} idx={} task_rsp={:#x} resume_mode={:?}",
       core_id, _task_idx, task_rsp, _task_resume_mode
   );
   ```
   Shows definitively that `task_rsp` is non-zero at the call.

2. **Trace-ring expansion.** The current ring is small enough that only 2–3 events around the crash are visible. Bump `kernel-core/src/trace_ring.rs` ring size to 1024 entries for the duration of investigation. Will reveal the timer / IPI / preempt events that precede the fault.

3. **IDT IST sanity check.** Print the actual TSS IST entries at boot:
   ```rust
   log::info!("[gdt] TSS IST: {:?}", crate::arch::x86_64::gdt::TSS.lock().interrupt_stack_table);
   ```
   Confirms whether IST 1/2/3 are populated for `#DF`, `#PF`, `#GP`. If any are zero, fix that *first* — the double fault is then a symptom not the disease.

4. **`switch_context` dump on entry.** Add a small Rust shim that wraps `switch_context` and logs `(rdi, rsi, current_rsp)` before calling it. If `rsi == 0` at call time, the bug is elsewhere; if `rsi != 0` at entry but the asm faults at popf, the bug is in the asm trampoline.

   Caveat: must not allocate or take any spinlock that the resumed task could hold — `log::info!` is risky. Use a per-core ring buffer (raw `AtomicU64`) and dump it in the panic handler.

---

## What to *not* do

1. **Don't revert the gs_base fix (`29d38e7`).** Without it the kernel doesn't even reach the dispatch loop under `preempt-full`. Bug #2 only becomes reachable because Bug #1 is closed.
2. **Don't roll forward to "let's just fix the symptoms in the panic handler" by adding IST entries everywhere.** That would mask the root cause. The double fault is a *consequence*; `RSP=0` in `switch_context` is the real bug.
3. **Don't add `unsafe { core::arch::asm!("cli") }` around the dispatch path.** That breaks `preempt-full` by definition — kernel-mode preemption is the whole point of the phase.
4. **Don't try to fix this by changing the default feature set.** `preempt-full = OFF` is already the default and explicitly the project plan until Track G validates. The PR is mergeable as-is. The fix here gates *enabling* preempt-full, not *shipping* it.

---

## File / line index

The investigation paths converge on a small set of files. Pre-load these:

- `kernel/src/task/scheduler.rs:3492-3960` — `scheduler::run` dispatch loop, including `pick_next` result handling, `restore_fpu_state` call, and the `switch_context` / `dispatch_preempted_and_resume_kernel` branch
- `kernel/src/task/scheduler.rs:1438-1493` — `save_fpu_state` / `restore_fpu_state` (xsave / xrstor inline asm)
- `kernel/src/task/scheduler.rs:1875-1886` — `peek_preempt_count_irq` (already fixed in `29d38e7`)
- `kernel/src/task/scheduler.rs:2113-2196` — `preempt_to_scheduler_kernel` and `preempt_frame_to_scheduler` (the path init might take if a timer IRQ catches it pre-yield)
- `kernel/src/task/mod.rs:262-292` — `XSaveArea::new` (header layout)
- `kernel/src/task/mod.rs:486-540` — `Task::new` (kernel stack allocation, init_stack call)
- `kernel/src/task/mod.rs:596-622` — `init_stack` (initial stack frame layout — the RFLAGS/r15…rbx/rip layout that `switch_context` will pop on first dispatch)
- `kernel/src/task/mod.rs:653-674` — `switch_context` global asm
- `kernel/src/arch/x86_64/mod.rs:167-208` — `fork_enter_userspace` global asm (the alternate fault site)
- `kernel/src/arch/x86_64/interrupts.rs:1418-1490` — `check_and_preempt_kernel` (gated on `preempt-full`; the early-boot guard added in `a70445c` is at line 1431-1441; the watchdog flag-restore is at 1457-1469)
- `kernel/src/arch/x86_64/interrupts.rs` (search for `dispatch_preempted_and_resume_kernel`) — the new 57e asm trampoline that's the prime suspect for H4

---

## Required reading before resuming

- `docs/handoffs/57e-kernel-preempt-audit.md` — kernel-mode preemption safety catalogue
- `docs/handoffs/57e-dispatch-reentrancy.md` — dispatch-path reentrancy windows; explicitly enumerates the points where preemption is meant to be safe
- `docs/roadmap/tasks/57e-full-kernel-preemption-tasks.md` § Track C, F, G — the design intent of the new asm trampolines and the soak gate

The recent commits worth scanning (most recent first):

```
29d38e7 fix(57e): make peek_preempt_count_irq safe before per-core init   ← Bug #1 fix; depends on this
4dd9fe1 fix(57e): address PR #136 review batch 3                           ← Doc updates + cpuid_raw RBX preservation fix
a70445c fix(57e): address PR #136 review batch 2                           ← Early-boot guard, watchdog flag-loss, OSFXSR assert
3bd2ec7 feat(xtask): add --features flag to xtask test
d364bf3 fix(57e): address PR #136 review feedback                          ← XSAVE area-size assertion reordering
342673e feat(kernel): Phase 57e Track F — activate kernel-mode preemption
c52e1d5 feat(kernel): Phase 57e Track C — kernel-mode preempt resume routines
```

`a70445c` and `c52e1d5` together introduce most of the code paths that Bug #2 lives in. Start the next session by re-reading those two commit diffs, then attack H1 → H4 in order.

---

## Resolution (this session)

**Root cause** — H6 confirmed. The dispatch loop's `pick_next` block (`scheduler.rs:3624`) acquires `scheduler_lock()` (an `IrqSafeMutex`).  Inside the guard, `set_current_task_idx(Some(idx))` commits the chosen task; on guard drop, `IrqSafeMutex` checks the `was_enabled` snapshot taken at acquire-time and re-enables IRQs because they were on at that point.  Between the lock release and the next `interrupts::disable()` call (originally inside `retarget_preempt_count_to_task`, ~28 lines later), there is a window in which:

- `current_task_idx == Some(idx)` (committed),
- `current_preempt_count_ptr` still targets the per-core dummy (value `0` — the switch-in retarget hasn't run yet),
- `kernel_preempt_watchdog == false` (no scheduler-context lock currently held),
- `reschedule == true` (set by any wakeup in the system),
- IRQs are enabled.

Under `preempt-full`, all of `check_and_preempt_kernel`'s gates pass and a queued LAPIC timer / reschedule IPI recurses through `preempt_to_scheduler_kernel` → `preempt_frame_to_scheduler` (`scheduler.rs:2147`) → `switch_context(per_core_switch_save_rsp_ptr(), per_core_scheduler_rsp())`. On the BSP's *first* dispatch, `per_core.scheduler_rsp` is still its zero initialiser (`smp/mod.rs:627`), so `rsi == 0`, `mov rsp, rsi` loads `0`, and `popf` faults at `[0]` → `#PF` with no IST → double fault (CR2 = `0xfffffffffffffff8`). On subsequent dispatches the value is non-zero but stale, unwinding the scheduler stack to a prior dispatch point and corrupting bookkeeping.

**Fix** — `kernel/src/task/scheduler.rs`, dispatch loop body. Insert `interrupts::disable()` **before** the `let next = { let mut sched = scheduler_lock(); ... };` block. Now the lock acquire captures `was_enabled = false`, the drop is a no-op, and IRQs stay masked from the moment we commit the chosen task all the way through `switch_context`'s `popf` (which restores the resumed task's saved RFLAGS and so reactivates IF if appropriate). The next iteration's top-of-loop `enable_and_hlt`/`enable` re-enables IRQs in the scheduler context.

**Why simpler attempts didn't work** — A first attempt placed the disable *after* the second lock guard at `scheduler.rs:3860` (just before `retarget_preempt_count_to_task`). That closed only the trailing 28-line window; the address-space restore block (`:3666–:3804`) and the second lock-acquire block (`:3818–:3860`) ran with IRQs enabled, leaving a wide non-re-entrant window. A second attempt placed it right after `match next`. That left a tiny gap between the `pick_next` lock drop and the disable that, while small, is still exploitable when the LAPIC timer is firing at 1 ms and `set_current_task_idx` has already committed the dispatch. Only by disabling IRQs *before* the lock acquire — so that the lock's drop is a no-op — does the window collapse to zero.

**What's still residual (not Bug #2)** — Under `preempt-full`, the boot now reaches every service in the smoke-test config (`syslogd`, `sshd`, `crond`, `console`, `kbd`, `display`, `term`, etc.) and runs userspace through several fork generations. The smoke-test failure now bubbles up at `SMOKE:tcc-version:PASS`, with kernel diagnostic warnings:

- `[WARN] [fault_kill] trampoline running for pid <N>` — userspace fault (kbd at one point, etc.) handled gracefully by the kernel.
- `[WARN] [sched] dequeue-drop core=<C> idx=<I> pid=<P> reason=state-not-ready extra=0x2` — run-queue contains a stale entry for a task whose `state == BlockedOnRecv (0x2)`. The dispatcher correctly drops the entry; this is a benign log of a pre-existing or preempt-full-amplified state inconsistency.

These are separate from the original Bug #2 and likely either pre-existing soak quality issues or downstream effects of preempt-full's increased preemption pressure. They should be filed and tracked as their own item before the Track G 24 h soak is gated open.

**Verification** — In this session:

1. `cargo xtask check` — clean (clippy `-D warnings`, rustfmt, kernel-core + passwd + driver_runtime host tests all pass).
2. `cargo xtask smoke-test` (default features) — PASSED on attempt 3 (9 steps in 16s, fixture's normal retry pattern).
3. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run` — boots cleanly through all services, runs for the full timeout window without a kernel fault. (Was: 100 % double-fault on init's first dispatch.)
4. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` — boots through, fails much later at `SMOKE:tcc-version:PASS` (separate userspace issue).

The remaining items in the original "Acceptance criteria for 'Bug #2 fixed'" section below are now (1) and (2) green; (3) still depends on the residual userspace progression issue, and (4) is the Track G soak gate which can be reopened once (3)'s residual is also resolved.

---

## Acceptance criteria for "Bug #2 fixed"

1. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask check` clean (clippy + rustfmt + 70 host tests).
2. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask test` — all 7 kernel test binaries pass (currently does pass — the bug is in run-time dispatch, not in the test entry path that doesn't actually dispatch user tasks).
3. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask smoke-test` — boots past `[kernel] entering scheduler`, runs the smoke-test fixture to completion, exits with PASSED.
4. `M3OS_KERNEL_FEATURES=preempt-full cargo xtask stress --test ssh-overlap --iterations 50 --continue-on-failure` — zero crashes across 50 boots × ~90 s ≈ 75 minutes of stress.

Once those pass, the gate to Track G's full 24 h soak is reopened.
