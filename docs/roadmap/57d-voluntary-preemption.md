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

### Assembly entry stub for timer + reschedule IPI (replaces the Rust `extern "x86-interrupt"` shape)

A Rust `extern "x86-interrupt"` function is **too late** to capture the interrupted task's full GPR state.  By the time the Rust handler body runs, the compiler is free to use any caller-saved register (`rax`, `rcx`, `rdx`, `rsi`, `rdi`, `r8..r11`) — calling `preempt_to_scheduler(&stack_frame, idx)` from Rust would only capture the Rust handler's transient state, not what the task held when the timer fired.  Resuming such a task would corrupt its register file.

57d replaces the timer and reschedule-IPI handlers with **naked-asm entry stubs** that branch on `(cs & 3)` and dispatch to one of two ring-typed Rust handlers — *not* a single Rust handler with a normalized "uniform" trap frame.

The "uniform layout" approach was unsound: the synthetic `rsp`/`ss` slots cannot be inserted *above* the CPU-pushed iretq frame on the IRQ stack, because the bytes immediately above that frame are real interrupted-kernel-stack data; and they cannot be inserted *below* the CPU frame without putting them at the wrong offset relative to the declared `PreemptTrapFrame` shape.  Two distinct trap-frame types — one per ring — sidestep the synthesis entirely:

- **`PreemptTrapFrameUser`** — `{gprs: [u64; 15], rip, cs, rflags, rsp, ss}` (15 GPRs + 5 iretq fields, 160 bytes).  Used when the interrupted code was in ring 3.  The CPU's 5-field iretq frame lands naturally at the trailing offsets after the GPR push.
- **`PreemptTrapFrameKernel`** — `{gprs: [u64; 15], rip, cs, rflags}` (15 GPRs + 3 iretq fields, 144 bytes).  Used when the interrupted code was in ring 0.  The CPU's 3-field same-CPL iretq frame lands naturally at the trailing offsets after the GPR push.  The interrupted kernel RSP is captured separately and passed as a second argument to the Rust handler (it is the address of the byte immediately above the CPU-pushed iretq frame at IRQ entry, recoverable as `lea rax, [rsp + 24]` *before* the GPR pushes).

```asm
.global timer_entry
timer_entry:
    // Branch on ring of interrupted code BEFORE pushing GPRs.
    test qword ptr [rsp + 8], 3      // (cs & 3) — 0 for ring-0, 3 for ring-3
    jz   .Lkernel_path

.Luser_path:
    // Ring-3 entry: CPU pushed ss, rsp, rflags, cs, rip (5 × 8 = 40 bytes).
    // Push 15 GPRs in PreemptTrapFrameUser.gprs order.
    push r15
    push r14
    push r13
    push r12
    push r11
    push r10
    push r9
    push r8
    push rbp
    push rdi
    push rsi
    push rdx
    push rcx
    push rbx
    push rax
    cld                              // Sys V: DF clear at call boundary
    // RSP alignment: TSS.RSP0 is 16-byte aligned by convention; CPU pushed
    // 40 bytes (≡ 8 mod 16); 15 GPR pushes added 120 bytes (≡ 8 mod 16);
    // total 160 bytes ≡ 0 mod 16.  RSP is therefore 16-aligned at this point,
    // satisfying the Sys V pre-call requirement.  An assertion (e.g.,
    // `test rsp, 0xF; jnz .panic_misaligned`) is recommended in debug builds.
    mov  rdi, rsp                    // &PreemptTrapFrameUser
    call timer_handler_user
    pop  rax
    pop  rbx
    pop  rcx
    pop  rdx
    pop  rsi
    pop  rdi
    pop  rbp
    pop  r8
    pop  r9
    pop  r10
    pop  r11
    pop  r12
    pop  r13
    pop  r14
    pop  r15
    iretq                            // CPU pops 5 fields → ring 3

.Lkernel_path:
    // Ring-0 entry: CPU pushed rflags, cs, rip (3 × 8 = 24 bytes).  No rsp/ss.
    // Capture the interrupted kernel RSP (the byte immediately above the CPU
    // iretq frame) for the Rust handler / 57e _kernel resume.
    lea  rsi, [rsp + 24]             // rsi = captured kernel RSP (passed as second arg)
    push r15
    push r14
    push r13
    push r12
    push r11
    push r10
    push r9
    push r8
    push rbp
    push rdi
    push rsi                         // (placeholder; rsi value is irrelevant at this offset)
    push rdx
    push rcx
    push rbx
    push rax
    cld
    // RSP alignment for ring-0: the interrupted kernel-stack alignment is
    // *unspecified* (depends on where the IRQ fired).  CPU pushed 24 bytes
    // (≡ 8 mod 16); 15 GPR pushes added 120 bytes (≡ 8 mod 16); total 144
    // bytes (≡ 0 mod 16) relative to the interrupted RSP.  If interrupted
    // RSP was 16-aligned, RSP here is 16-aligned; otherwise it is offset by
    // 8.  The stub MUST enforce alignment explicitly:
    //   test rsp, 0xF
    //   jz  .Lkern_aligned
    //   sub rsp, 8
    //   mov qword ptr [rsp], <pad-marker>      // mark for unpad on return
    // .Lkern_aligned:
    // (Implementation may use a different mechanism — e.g., save original RSP
    // in a callee-saved register before pushes, restore after call, then mask
    // with `and rsp, ~0xF` before the call — but the invariant is the same:
    // RSP ≡ 0 mod 16 at the `call` instruction.)
    // (rsi was overwritten by the GPR push above — re-load before the call.)
    lea  rsi, [rsp + 16*8 + 0]       // re-derive captured kernel RSP from saved iretq frame
    mov  rdi, rsp                    // first arg: &PreemptTrapFrameKernel
    // (rsi is already the captured RSP — second arg)
    call timer_handler_kernel
    // Reverse the alignment fix if a pad was inserted.
    pop  rax
    pop  rbx
    pop  rcx
    pop  rdx
    pop  rsi
    pop  rdi
    pop  rbp
    pop  r8
    pop  r9
    pop  r10
    pop  r11
    pop  r12
    pop  r13
    pop  r14
    pop  r15
    iretq                            // CPU pops 3 fields → ring 0
```

(The asm sketch is illustrative; implementer may use any mechanism that satisfies the invariants — see the alignment rule below.)

The Rust side becomes:

```rust
#[unsafe(no_mangle)]
extern "C" fn timer_handler_user(frame: &mut PreemptTrapFrameUser) {
    // ... existing tick + reschedule-flag work ...
    crate::arch::x86_64::apic::lapic_eoi();

    // 57d preemption check — from_user is true by construction here
    // (this handler is reached only when (cs & 3) == 3 in the asm stub).
    let pc_ptr = crate::smp::per_core().current_preempt_count_ptr.load(Acquire);
    let pc = unsafe { (*pc_ptr).load(Relaxed) };
    if pc != 0 { return; }
    if !crate::smp::per_core().reschedule.swap(false, AcqRel) { return; }

    // Hand off to the scheduler; preempt_to_scheduler_user does not return.
    unsafe { preempt_to_scheduler_user(frame); }
}

#[unsafe(no_mangle)]
extern "C" fn timer_handler_kernel(frame: &mut PreemptTrapFrameKernel, captured_kernel_rsp: u64) {
    // ... existing tick + reschedule-flag work ...
    crate::arch::x86_64::apic::lapic_eoi();

    // 57d (PREEMPT_VOLUNTARY): kernel mode is non-preemptible.
    // The handler runs the tick / EOI work and returns; the asm stub iretq's
    // back to the interrupted kernel code with all GPRs intact.
    //
    // 57e (PREEMPT_FULL): drop this early-return; run the same preempt check
    // as the user handler; pass captured_kernel_rsp into preempt_to_scheduler_kernel,
    // which writes it into Task::preempt_frame.rsp for the _kernel resume routine.
    let _ = captured_kernel_rsp;     // unused in 57d
}
```

`preempt_to_scheduler_user` copies `PreemptTrapFrameUser` into `Task::preempt_frame` — all 5 trailing slots populated naturally (rip/cs/rflags/rsp/ss from the user-mode iretq frame).  In 57e, `preempt_to_scheduler_kernel` copies `PreemptTrapFrameKernel` plus the captured kernel RSP into `Task::preempt_frame` — rip/cs/rflags from the same-CPL iretq frame, `rsp` slot holds the captured kernel RSP, `ss` slot is zero.  Both share the same `Task::preempt_frame` destination (a uniform 5-trailing-slot `PreemptFrame` from 57b); the resume routines (`preempt_resume_to_user` 57d, `preempt_resume_to_kernel` 57e) read different parts of it based on `preempt_frame.cs.rpl()`.

**Stack-alignment invariant (per-path enforcement, no fixed-count proof):**

> Immediately before every `call` instruction in the asm stub, RSP must be 16-byte aligned (Sys V AMD64 ABI).
>
> - The user path's GPR-push count is fixed (15 × 8 = 120 bytes) and the CPU-pushed 5-field frame is 40 bytes (≡ 8 mod 16); the kernel-stack base (TSS.RSP0) is 16-aligned by convention; therefore the user path's pre-call alignment is naturally satisfied.  A debug-build `test rsp, 0xF; jnz panic` confirms.
> - The kernel path's pre-IRQ kernel-stack alignment is **unspecified** — it depends on where the IRQ fires in interrupted kernel code.  The kernel path therefore MUST explicitly enforce alignment before the `call`, either by conditional pad (`test rsp, 0xF; jz aligned; sub rsp, 8` with matching unpad on return) or by saving original RSP, masking with `and rsp, ~0xF`, and restoring after the call.

The implementer chooses the mechanism; the docs do not mandate one.  The acceptance criterion is the `movaps` regression test in 57d Track B.2: the test exercises both ring-3 and ring-0 entry paths and would crash on a misalignment.

`PreemptTrapFrameUser` and `PreemptTrapFrameKernel` are the asm-stub-built layouts on the IRQ stack.  `Task::preempt_frame` (the `PreemptFrame` from 57b E.1) is the uniform 5-trailing-slot destination on preempt.  The two trap-frame types exist *only* on the IRQ stack between asm entry and the Rust handler call — `Task::preempt_frame` is always the uniform `PreemptFrame` shape, with `rsp` reused for kernel RSP on kernel-preempted tasks and `ss` set to zero.

### `preempt_enable()` deferred-reschedule (zero-crossing path)

The IRQ-return preemption check above only fires when the next timer / reschedule-IPI arrives.  If a wake sets `reschedule` while the running task is holding a lock (`preempt_count > 0`), the IRQ handler observes `pc != 0` and skips preemption — but nothing in 57d so far re-checks when the lock is later released.  The result: preemption latency is bounded by the next timer tick (~1 ms) regardless of how soon the lock drops.

The Linux pattern that closes this gap is `preempt_enable() → schedule()` on the zero-crossing post-decrement.  57d adds the corresponding behaviour:

```rust
#[inline]
pub fn preempt_enable() {
    let pc_ptr = crate::smp::per_core().current_preempt_count_ptr.load(Acquire);
    let prev = unsafe { (*pc_ptr).fetch_sub(1, Release) };

    // 57d zero-crossing path.
    if prev == 1 && crate::smp::per_core().reschedule.load(Relaxed) {
        // Caller is now preemptible; reschedule is pending.  Fire the
        // scheduler at the next safe point.  In PREEMPT_VOLUNTARY this
        // is restricted: kernel-mode `preempt_enable` does NOT switch
        // tasks because kernel-mode must reach user-mode return first.
        // The trigger is recorded in per_core().preempt_resched_pending
        // and consumed at the next syscall / IRQ user-mode return boundary
        // (which already debug-asserts preempt_count == 0).
        crate::smp::per_core().preempt_resched_pending.store(true, Release);
    }
}
```

This is **deferred-reschedule under `PREEMPT_VOLUNTARY` semantics**: the trigger is set, but the actual scheduler switch happens at the next user-mode return boundary (which already runs the IRQ-return preemption check).  This keeps the kernel-mode non-preemptibility invariant intact while still closing the worst-case latency gap.

57e drops the kernel-mode restriction; under `PREEMPT_FULL`, `preempt_enable` may switch tasks immediately if it is being called from a kernel-mode preemption-safe context.

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

The dispatch path reads `resume_mode` and selects the restore routine.  This is a small additive change — and one that is permissible despite 57b's "no new flag fields" gate because `resume_mode` is a discriminant (single source of truth for *how* the task is restored), not a flag.

### Scheduler integration

The scheduler's `pick_next` and `dispatch` routines must accept a preempted task:

- `pick_next` already returns the next runnable task; no change needed (the preempted task is enqueued before `pick_next` runs).
- `dispatch` reads `resume_mode` and selects between `switch_context` (cooperative) and `preempt_resume_to_user` (preempted).  Both routines end up at user mode; the difference is the register-restore mechanism.

### Lock-free `preempt_count` access (re-uses 57b's `current_preempt_count_ptr`)

The IRQ handler reads `preempt_count` on every timer tick.  This must be lock-free.  57b already added `PerCoreData::current_preempt_count_ptr` for this purpose (it is the foundation that lets `IrqSafeMutex::lock` call `preempt_disable` non-recursively); the IRQ handler reuses it directly:

```rust
let pc_ptr = crate::smp::per_core().current_preempt_count_ptr.load(Acquire);
let pc = unsafe { (*pc_ptr).load(Relaxed) };
```

No new per-CPU `current_task_idx_fast` is required — that would be a duplicate of `PerCoreData::current_task_idx` (which already exists from Phase 35 / 57a) and would still need a separate stable-storage story.  The `current_preempt_count_ptr` is the right primitive: it gives the IRQ handler exactly what it needs (the count) without requiring a `Task` lookup.

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
  - *Single Responsibility.*  `preempt_to_scheduler_user` saves and switches; the scheduler picks; `preempt_resume_to_user` restores.  Each routine has one job.  In 57e, the parallel `_kernel` variants apply.
  - *Open/Closed.*  New IRQ sources that want to fire preemption (e.g., a future IPC reschedule-IPI variant) plug in via `signal_reschedule()` + their own ring-aware asm entry stub that follows the same two-path pattern; no scheduler changes required.
  - *Liskov.*  A preempted task and a cooperatively-yielded task are indistinguishable to `pick_next` — both are `Ready` in the run queue.
  - *Interface Segregation.*  Each Rust handler sees its own typed trap frame (`PreemptTrapFrameUser` or `PreemptTrapFrameKernel`), not a union.
  - *Dependency Inversion.*  The IRQ handlers depend on `current_preempt_count_ptr` and the `reschedule` atomics, not on `Task` internals.
- **DRY.**  A shared GPR push/pop macro covers both ring paths in `preempt_entry.S`.  Both Rust handlers share the tick / EOI / reschedule-flag work via a common helper.
- **Documented invariants.**
  - **Ring-aware dispatch.**  The asm stub branches on `(cs & 3)` *before* any GPR push.  The user path is reached only when interrupted code was in ring 3; the kernel path only when in ring 0.  `from_user == true` is therefore a structural property of the user handler, not a runtime check inside it.
  - **57d kernel-mode skip.**  Under `PREEMPT_VOLUNTARY`, the kernel handler returns early (no preempt fires).  This is the kernel-mode-non-preemptibility invariant.  57e drops this early-return.
  - **`preempt_count == 0`.**  Required precondition for the user-handler preempt path.  A non-zero count indicates a held lock or explicit preempt-disable; preemption silently skips.
  - **`reschedule` flag.**  Set by the timer or by `signal_reschedule_all()`.  Cleared by the scheduler dispatch path or by the IRQ handler's `swap(false)` consumption.
  - **User-mode → preempt_count == 0 always.**  In ring 3, no kernel locks are held; `preempt_count` is always 0.  A `debug_assert!` confirms this at the start of the user handler.
- **Lock ordering.**  The IRQ handler reads three atomics with `Relaxed` ordering — no locks acquired in the IRQ context.  The scheduler dispatch (reached via `preempt_to_scheduler`) takes `SCHEDULER.lock` (inner) but never `pi_lock` (outer) — preserving the 57a hierarchy.
- **Migration safety.**  The IRQ-return check is gated on a `cfg(feature = "preempt-voluntary")` flag during initial roll-out.  Default off; opt-in for testing.  Final landing flips the default to on; the flag is removed in a follow-up cleanup commit.
- **Observability.**  Every preemption emits a `[TRACE] [preempt]` line under `--features sched-trace` (extends 57a's tracepoint).  The watchdog continues to fire on stuck tasks; a stuck-task warning that includes "preempted=N" frames helps diagnose preempt-discipline bugs.

## Important Components and How They Work

### Naked-asm entry stubs (`timer_entry`, `reschedule_ipi_entry`)

Located at `kernel/src/arch/x86_64/asm/preempt_entry.S` (new).  Each replaces the corresponding `extern "x86-interrupt"` function.  On entry: branch on `(cs & 3)` *before any GPR push* and dispatch to one of two paths:

- **Ring-3 (user) path.**  Push 15 GPRs in `PreemptTrapFrameUser.gprs` order onto the IRQ stack.  The CPU's 5-field iretq frame already sits at the trailing offsets after the GPR block, completing the `PreemptTrapFrameUser` shape naturally.  `cld`; `call timer_handler_user(&mut PreemptTrapFrameUser)`.  On Rust return, pop GPRs in reverse order; `iretq`.  Stack alignment at the `call` is satisfied by construction (TSS.RSP0 is 16-aligned; CPU push of 40 bytes + 15 × 8 GPR bytes = 160 bytes ≡ 0 mod 16).
- **Ring-0 (kernel) path.**  Capture the interrupted kernel RSP via `lea rsi, [rsp + 24]` *before* the GPR push (it's the address immediately above the CPU's 3-field iretq frame).  Push 15 GPRs in `PreemptTrapFrameKernel.gprs` order; the CPU's 3-field iretq frame already sits at the trailing offsets, completing the `PreemptTrapFrameKernel` shape naturally.  Enforce 16-byte RSP alignment explicitly (test/pad or save-and-`and`-mask) — interrupted-kernel-stack alignment is unspecified.  `cld`; `call timer_handler_kernel(&mut PreemptTrapFrameKernel, captured_kernel_rsp)`.  On Rust return, undo any pad; pop GPRs; `iretq`.

The two-path design avoids the in-place "normalize a uniform 5-field frame on the IRQ stack" approach, which is unsound: synthetic `rsp`/`ss` slots cannot be inserted *above* the CPU iretq frame (those bytes belong to interrupted-kernel-stack data) and cannot be inserted *below* without putting them at the wrong offset relative to the declared frame.  Each path's `PreemptTrapFrame*` shape matches the natural CPU-pushed frame; no synthesis is required at IRQ entry.

`Task::preempt_frame` (from 57b E.1) remains a uniform `PreemptFrame` with 5 trailing slots, used as the destination on preempt by both `preempt_to_scheduler_user` and `preempt_to_scheduler_kernel`.  For kernel-preempted tasks, the `PreemptFrame.rsp` slot holds the captured kernel RSP and `PreemptFrame.ss` is zero (same-CPL `iretq` does not pop ss).  The resume routines (`preempt_resume_to_user` 57d, `preempt_resume_to_kernel` 57e) read different parts of `PreemptFrame` based on `cs.rpl()`.

### `preempt_to_scheduler` (Rust)

Located at `kernel/src/task/scheduler.rs`.  Called from the Rust handler when the preemption check passes.  Does not return through the asm stub — instead, it copies `PreemptTrapFrame` into `current_task().preempt_frame`, marks the task `state = Ready`, `on_cpu = false`, `resume_mode = Preempted`, run-queues it, and jumps to the per-core scheduler dispatch entry.  The dispatch entry's epilogue is the cooperative `switch_context` for the next-chosen task (or `preempt_resume_to_user` if that task was previously preempted).

### `preempt_resume_to_user` (assembly)

Located at `kernel/src/arch/x86_64/asm/preempt_entry.S`.  Called by the dispatch path when the chosen task's `resume_mode == Preempted` and `preempt_frame.cs.rpl() == 3`.

```asm
preempt_resume_to_user:
    // rdi = &Task::preempt_frame
    // 1. Restore GPRs from preempt_frame.gprs.
    // 2. Push iretq frame (ss, rsp, rflags, cs, rip) onto current stack from preempt_frame.{ss,rsp,rflags,cs,rip}.
    // 3. iretq — privilege level changes to ring 3; CPU pops all five fields.
```

### `preempt_enable` zero-crossing record

Located at `kernel/src/task/scheduler.rs::preempt_enable`.  On `fetch_sub`, if the post-decrement count is 0 *and* `per_core().reschedule` is set, record `per_core().preempt_resched_pending = true`.  The actual scheduler entry is deferred to the next user-mode return boundary, where the IRQ-return preemption check (or the syscall-return path's debug assertion) consumes the record and switches.  Under `PREEMPT_VOLUNTARY`, kernel-mode `preempt_enable` does *not* immediately switch — that would violate the kernel-mode-non-preemptibility invariant.  57e drops this restriction.

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
2. **Track B — Asm entry stubs.**  Replace `timer_handler` and `reschedule_ipi_handler` with naked-asm stubs that save all GPRs into a `PreemptTrapFrame` before calling Rust.  The Rust handler receives `&mut PreemptTrapFrame`.
3. **Track C — `preempt_to_scheduler` and `preempt_resume_to_user`.**  Implement `preempt_to_scheduler` (Rust) that copies `PreemptTrapFrame` into `Task::preempt_frame` and transfers to the scheduler.  Implement `preempt_resume_to_user` (asm) that restores from `preempt_frame` and `iretq`s to ring 3.
4. **Track D — Dispatch integration.**  Add `Task::resume_mode`; route the dispatch path to either `switch_context` or `preempt_resume_to_user` based on the mode.
5. **Track E — `preempt_enable` deferred-reschedule (zero-crossing).**  On `fetch_sub`, if the post-decrement count is 0 *and* `per_core().reschedule` is set, record `preempt_resched_pending`.  The user-mode return boundary consumes the record and runs the IRQ-return preemption check inline.  Closes the latency-bound-by-next-timer-tick gap promised by 57b.
6. **Track F — Lock-free preempt-count read in IRQ.**  Reuse 57b's `current_preempt_count_ptr` for the in-IRQ read.  No new per-CPU index field.
7. **Track G — IRQ-return check.**  Wire the check into the new Rust `timer_handler_with_frame` / `reschedule_ipi_handler_with_frame`.  Gate on `cfg(feature = "preempt-voluntary")` for initial roll-out.
8. **Track H — Stress test and validation.**  Run user-loop stress.  Run the I.1 acceptance gate.  Confirm `[WARN] [sched]` lines do not appear.
9. **Track I — Default-on flip.**  Flip the feature default to on.  Run the soak gate.  Remove the feature flag in a follow-up commit.

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
