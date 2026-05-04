# Phase 57e — Dispatch Path Reentrancy Windows (Track A.2)

**Status:** Landed alongside the 57e implementation branch
**Source ref:** phase-57e Track A.2
**Companion:** `docs/handoffs/57e-kernel-preempt-audit.md`

Under `PREEMPT_FULL`, the dispatch path itself can be preempted.  Each
window where this would corrupt scheduler state must either raise
`preempt_count` (so the kernel-mode preempt check in
`check_and_preempt_kernel` is suppressed) or run with `IF == 0` (so no IRQ
fires at all).  This document enumerates every such window in
`kernel/src/task/scheduler.rs::pick_next` and `dispatch`, classifies its
preemption-safety property, and records the wrapper.

## Scheduler-context windows

| Window | Location (file:line) | IF state | preempt_count | Safety |
|---|---|---|---|---|
| `SCHEDULER.lock` held | `scheduler.rs::scheduler_lock` returns `IrqSafeGuard` | `IF=0` (mask + restore) | `≥1` (IrqSafeMutex raises) | preempt_count > 0 → safe |
| `pi_lock` held | `Task::with_block_state` | `IF=0` | `≥1` (IrqSafeMutex raises) | preempt_count > 0 → safe |
| Post-`pick_next`, pre-`switch_context` window | `scheduler.rs:3520–3680` | unspecified (caller-restored) | drops to 0 after IrqSafeGuard release | benign-preemption case (chosen task goes back on queue) |
| `switch_context` body | `task/mod.rs::switch_context` asm | IF=0 between `pushf`/`cli` and `popf` | irrelevant (asm window) | IF=0 → no IRQ → safe |
| `preempt_resume_to_user` body | `interrupts.rs:706–738` | IF=0 (caller invariant) | irrelevant | IF=0 until `iretq` → safe |
| `dispatch_preempted_and_resume` body | `interrupts.rs:769–794` | IF=0 (caller invariant + explicit `cli`) | irrelevant | IF=0 → safe |
| `preempt_resume_to_kernel` body | `interrupts.rs:798–838` (57e Track C.1) | IF=0 (caller invariant) | irrelevant | IF=0 until `iretq` → safe |
| `dispatch_preempted_and_resume_kernel` body | `interrupts.rs:840–870` (57e Track C.4) | IF=0 (caller invariant + explicit `cli`) | irrelevant | IF=0 → safe |

### Post-pick_next benign-preemption case

The window between `pick_next` returning a chosen task and the actual
`switch_context` call (or its `dispatch_preempted_and_resume[_kernel]`
trampoline) drops `preempt_count` to 0 when the IrqSafeGuard from
`scheduler_lock()` is released.  IF may be 0 or 1 at that point depending
on the prior caller's state.

If a kernel-mode preemption fires here, the chosen task is already in the
run queue (state = Ready).  The preemption causes the dispatch loop to:

1. Save its own continuation via `preempt_to_scheduler_kernel`.
2. Re-enter `pick_next`, which may return the same chosen task.
3. Dispatch normally on the second pass.

No state is corrupted — the chosen task's enqueue is idempotent.  This
window is therefore a **benign preemption case**: extra latency, no
correctness issue.  Documented here so a future debugger sees the
double-pick in trace logs and recognizes the pattern.

## FPU / XSAVE coverage

`restore_fpu_state` is called *before* the dispatch path branches on
`resume_mode == Preempted` (currently
`kernel/src/task/scheduler.rs:3713–3715`), so both kernel-mode and
user-mode preempt-resume paths inherit the same FPU restore.

The kernel build flags (`-mmx,-sse`) mean kernel code does not dirty
XMM/YMM, but a kernel-mode preemption can interrupt a thread mid-syscall
whose user FPU state is live in the registers; that state is captured at
switch-out via the same `save_fpu_state` call (`scheduler.rs:3792–3794`).

Under 57e Track J, the save/restore call sites are unchanged — only the
underlying instruction migrates from `fxsave64`/`fxrstor64` to
`xsave64`/`xrstor64` so AVX YMM upper halves survive the round trip.

## Regression test coverage

Each window has a stub test in the QEMU integration suite:

- `kernel/tests/preempt_voluntary.rs` — H.1.x kernel-side logic tests
  (already shipped in 57d) cover `preempt_count` discipline.
- `kernel/tests/preempt_latency.rs` — Track E benchmark stubs document the
  measurement protocol; bodies are `#[ignore]` until the harness wires
  task spawn / SMP boot.
- `kernel/tests/xsave_avx.rs` — Track J.5 XSAVE regression with two live
  CPUID-parser tests + two `#[ignore]` YMM-survives-yield stubs.

The full Track G activation (24-hour soak with all benchmarks measured at
both ends) remains the gating validation.

## Open follow-ups

None blocking the 57e implementation branch.
