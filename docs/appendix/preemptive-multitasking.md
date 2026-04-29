# Pre-emptive Multitasking — Design Notes (Future Phase)

**Status:** Planning notes — not yet scoped as a phase.
**Audience:** Whoever picks up the work after Phase 57a settles.
**Origin:** Problem surfaced during Phase 57a debugging — `syslogd` monopolised core 1 for ~3000 log lines after dispatch, blocking every other task queued on that core (console, stdin_feeder, nvme_driver, term). Diagnosis showed the kernel **does not pre-empt** running user code on timer IRQ; the per-core `reschedule` flag is set but only consulted at voluntary yield points.

---

## Current State (Phase 57a)

m3OS is **partially pre-emptible** — not fully cooperative, not fully pre-emptive.

What works:
- Timer IRQs and IPIs DO interrupt running user code; the handler runs in IRQ context.
- The IRQ handler calls `signal_reschedule()` which sets `per_core().reschedule.store(true)`.
- The IRQ handler EOIs and returns to the interrupted instruction.

What's missing (the gap):
- **No preemption-check on IRQ return.** Control goes straight back to the interrupted user code. The reschedule flag is only consulted at voluntary yield points (`yield_now`, `block_current_until`, syscall paths that internally yield).
- A user task in a tight CPU-bound loop monopolises its core until it voluntarily syscalls.
- A kernel-mode busy-wait inside a syscall has the same effect — the syscall holds the core until it returns.
- There is no `preempt_count` infrastructure, so even if we added a check we couldn't safely fire it (would deadlock against held spinlocks).

This is essentially Linux's old `CONFIG_PREEMPT_NONE` model — server-grade throughput tradeoff with latency-sensitivity in places that voluntarily yield.

---

## Goal

Match Linux's **`CONFIG_PREEMPT_VOLUNTARY`** as a first target (interrupts can preempt user mode but not kernel mode), with `CONFIG_PREEMPT` (full kernel preemption) as a stretch goal.

Concretely:
- A timer IRQ that fires while a task is in user mode AND the reschedule flag is set should NOT return to the interrupted user code; it should switch to the scheduler.
- Same for IPIs that delivered a wake / migration request.
- Kernel mode can stay non-preemptible initially — that's still a huge improvement over today.
- Spin-waits in kernel code remain functionally correct (no preemption disrupts them) but should be audited for replacement with block+wake pairs over time.

---

## Design Pieces (5 of them, increasing difficulty)

### 1. Full register save on preemption

Today `switch_context` saves only callee-saved registers (`rbx`, `rbp`, `r12`–`r15`, `rsp`, `rip`) — it's an ABI-clean call boundary, both sides agree caller-saved registers are dead.

A pre-emption point fires mid-instruction-stream where every register may be live: `rax`–`rdx`, `rsi`, `rdi`, `r8`–`r11`, `RFLAGS`, possibly x87/SSE state.

What's needed:
- A separate **preempt switch** routine that saves the full `iretq` frame (already on the IRQ stack: `rip`, `cs`, `rflags`, `rsp`, `ss`) plus all GPRs and segments, and uses `iretq` (not `ret`) to resume.
- New `Task` field for full register state OR repurpose the kernel stack (Linux uses the kernel stack for this).
- Cannot reuse the existing `switch_context` — that path is fine for cooperative yields.

Touches: `kernel/src/arch/x86_64/asm/switch.S`, `kernel/src/task/mod.rs` (Task layout), `kernel/src/arch/x86_64/interrupts.rs` (IRQ return path).

### 2. Per-task `preempt_count`

A counter that gates whether preemption is allowed:
- Starts at 0.
- Incremented entering a non-preemptible region (`preempt_disable`).
- Decremented leaving (`preempt_enable`).
- Pre-emption only fires when `preempt_count == 0`.

Without this, an IRQ that fires while the task holds a spinlock and we preempt → the new task tries to take the same lock → deadlock.

Linux pattern:
```c
preempt_disable();   // count++
// non-preemptible region
preempt_enable();    // count--; if count == 0 && need_resched, schedule()
```

Touches: `kernel/src/task/mod.rs` (Task field), `kernel/src/task/scheduler.rs` (helpers), every spinlock callsite (Piece 3).

### 3. Spinlocks raise `preempt_count`

Every spinlock acquire `preempt_disable`s; every release `preempt_enable`s. Concretely:
- `IrqSafeMutex::lock()` → preempt_disable + interrupts::disable.
- `IrqSafeMutex::Drop` → interrupts::enable + preempt_enable.
- `Task::pi_lock` (which is now `IrqSafeMutex<TaskBlockState>`) inherits this for free once `IrqSafeMutex` is updated.
- Same for any other `spin::Mutex` / `spin::RwLock` callsites that aren't already wrapped in `IrqSafeMutex`.

The dominant churn lives here — every lock callsite gets reviewed for preempt-discipline. Mostly mechanical but easy to get wrong (forget a release path → preempt-disabled forever → no preemption ever fires for that task again until reboot).

Touches: every `kernel-core` and `kernel/` lock site. Property: `preempt_count` returns to 0 every time the task returns to user mode (verify with a debug assertion at the IRQ return path).

### 4. Pre-emption check at IRQ return

In the timer IRQ handler (and any IRQ that calls `signal_reschedule`), after EOI:

```rust
extern "x86-interrupt" fn timer_handler(stack_frame: InterruptStackFrame) {
    // ... existing tick + reschedule-flag work ...
    super::apic::lapic_eoi();

    // NEW: preemption check.
    let from_user = stack_frame.code_segment.rpl() == PrivilegeLevel::Ring3;
    if from_user
        && per_core().preempt_count.load(Relaxed) == 0
        && per_core().reschedule.load(Relaxed)
    {
        // Save full frame, switch to scheduler RSP, scheduler will pick next task.
        preempt_to_scheduler(&stack_frame);
        // returns via iretq to whatever the scheduler picked next.
    }
}
```

`preempt_to_scheduler` is the Piece-1 routine — it saves the full register state into the current task's "preempted" save area, then calls into the scheduler's `pick_next` and dispatches the chosen task.

Variant decisions:
- **`PREEMPT_VOLUNTARY` first**: only check `from_user`. Kernel mode stays non-preemptible. Safer to roll out; matches Linux's default desktop config.
- **`PREEMPT_FULL` later**: drop the `from_user` check. Kernel-mode preemption requires every spinlock callsite already audited for preempt-discipline (Piece 3 must be 100% complete). Latency wins are real (microseconds) but the failure mode is much worse.

Touches: `kernel/src/arch/x86_64/interrupts.rs` (timer + IPI handlers), new arch-specific `preempt_to_scheduler` routine.

### 5. Audit every kernel busy-wait

Today some kernel code does:

```rust
while !condition { core::hint::spin_loop(); }
```

confident that the surrounding cooperative model means nobody who'd update the condition is being preempted. Once preemption is real and works in kernel mode, the holder of the condition can be preempted, and the spinner just burns CPU.

Two responses per site:
- **Preferred:** convert to a wait-queue / `block_current_until` pair. The blocker actually parks, the waker actually wakes.
- **Acceptable for very short critical sections:** wrap in `preempt_disable` so the spinner can't be preempted (but the holder also can't be preempted while the spinner waits — both must complete in bounded time).

Concrete current sites to audit:
- `wake_task_v2` step 4 — `on_cpu` `smp_cond_load_acquire` spin. Already bounded by the dispatch epilogue runtime; should be fine but document the bound.
- `IrqSafeMutex` itself — the inner `spin::Mutex::lock()` spins. Already preempt-disabled by IRQ masking; OK.
- Any `wait_icr_idle()` (LAPIC ICR poll) in `kernel/src/smp/ipi.rs`. Hardware-bounded; OK.
- Per-driver polling loops in `kernel/src/blk/`, `kernel/src/net/`, etc. — these are the ones to check.

Touches: every kernel callsite that spins. Some can stay; others convert.

---

## Phasing / Order of Work

Three increments, each independently shippable:

### 57b — Foundation (no behavior change)

Land Pieces 1, 2, 3 together. The kernel becomes preemption-CAPABLE but pre-emption is never actually fired (the IRQ handler doesn't call `preempt_to_scheduler` yet). This is a no-op refactor that adds:
- Full register save routine + Task field.
- `preempt_count` infrastructure.
- All spinlock callsites updated to preempt_disable/enable.

Test: existing `cargo xtask test` passes; no functional change. Spot-check `preempt_count` returns to 0 at every user-mode boundary.

Risk: low. Worst case is a forgotten `preempt_enable` somewhere — caught by debug assertion at IRQ return.

### 57c — Voluntary preemption (user-mode only)

Add Piece 4 with `PREEMPT_VOLUNTARY` semantics. Kernel mode stays non-preemptible. This is when the actual behaviour change lands:

Test acceptance:
- `cargo xtask run-gui --fresh` — `syslogd` no longer monopolises core 1; `term` reaches the framebuffer.
- A new in-QEMU test: spawn a CPU-bound user task, verify it gets preempted within ~10 ms.
- `cargo xtask test` regression suite passes.
- Soak (10 minutes) — no panic, no cpu-hog warnings whose corrected `ran` exceeds the timeslice.

Risk: medium. The new failure mode is "preempted at a moment that exposes a missing `preempt_disable` somewhere we missed in 57b." Hopefully `preempt_count == 0` debug assertions catch most of these.

### 57d — Full kernel preemption (stretch)

Drop the `from_user` check. Audit complete (Piece 5). Kernel code becomes preemptible.

Test: latency benchmarks (round-trip IPC, syscall wakeup) drop into the microsecond range.

Risk: high. Every kernel spin-wait that hasn't been audited becomes a potential lockup. Defer until 57c has been running clean for at least a release cycle.

---

## Prerequisites

Phase 57a's Phase 57a's contributions make 57b cheaper:
- `pi_lock` outer / `SCHEDULER.lock` inner ordering is documented and asserted — preempt-disable wrapping is uniform across both.
- `IrqSafeMutex` already exists and already disables interrupts on lock — adding `preempt_disable` to it is a one-line change that catches every callsite.
- `Task::on_cpu` already exists for the cross-core wake-side spin-wait. The "preempted" state is conceptually similar; some bookkeeping can share the field.

Phase 57a should be **fully merged and validated** before 57b begins. Don't try to interleave preemption work with the in-flight scheduler rewrite.

---

## Risks Specific to m3OS

1. **`switch_context` is hot.** Every yield, every block, every dispatch. If we add a "is this preemption?" check or branch, performance can regress. Mitigation: separate `preempt_switch_context` routine for the preempt path; keep the cooperative path lean.

2. **AP cores bring their own surprises.** Phase 57a found that core 1's behaviour diverges subtly from BSP. The first preemption attempt on an AP that's never been preempted may surface an init-time bug (e.g. a stale per-CPU field). Test on 4 cores from day 1; don't validate on BSP-only.

3. **Userspace tests assume cooperative scheduling.** Some `userspace/` tests have implicit ordering assumptions ("if I yield, X will definitely run before me again"). Pre-emption breaks these. Audit `userspace/tests/` for any that depend on cooperative semantics.

4. **`syslogd` and `vfs_server` ordering.** Today's symptom — syslogd monopolising core 1 — won't be fixed by preemption alone if syslogd is busy-waiting in a syscall. The real fix for that case is **converting kernel busy-waits to block+wake**. Pre-emption only papers over user-mode CPU monopoly; kernel-mode monopoly needs the audit in Piece 5.

---

## Open Questions

- **Target preemption granularity** — match Linux's `HZ=1000` (1 ms quantum)? Already what `TICKS_PER_SEC` is. Probably keep it.
- **Per-task vs per-CPU preempt_count** — Linux uses per-CPU. We have `try_per_core()` already. Per-CPU is faster (no atomic on the hot path) but requires care across context switches.
- **Should `syscall` entry preempt-disable?** Linux's syscall entry path already raises preempt_count via the spin-disabling enter sequence. We'd need to mirror that or accept that syscall bodies aren't preemptible (matches `PREEMPT_VOLUNTARY` semantics).
- **What about the `rcu`-style read-side critical sections?** m3OS doesn't have RCU yet. If/when added, preempt_count interactions need to be designed in.
- **Soft IRQs / bottom-halves?** Linux runs them with preemption disabled. m3OS's "drain pending waiters" / "watchdog scan" code is similar in spirit; do they need preempt-disabling?

---

## Pointers

- Linux source for reference:
  - `arch/x86/kernel/entry_64.S` — full register save on IRQ entry.
  - `kernel/sched/core.c::__schedule` — preemption decision point.
  - `include/linux/preempt.h` — `preempt_disable` / `preempt_enable` / `preempt_count`.
- m3OS files most affected:
  - `kernel/src/arch/x86_64/interrupts.rs` (IRQ return path).
  - `kernel/src/arch/x86_64/asm/switch.S` (switch routines).
  - `kernel/src/task/mod.rs` (Task layout).
  - `kernel/src/task/scheduler.rs` (`IrqSafeMutex`, `pi_lock` helper, `with_block_state`).
  - Every callsite of `IrqSafeMutex::lock()` / `spin::Mutex::lock()` (Piece 3 audit).

---

## Bottom Line

Pre-emptive multitasking is a phase-scale undertaking — about 1-2 weeks for an experienced kernel hacker, probably 3-4 weeks accounting for regression burn-in. It's not a quick fix.

For today's `syslogd`-monopolising-core-1 bug, the targeted fix is:
1. Identify the specific syscall busy-waiting (per-pid syscall trace).
2. Convert it to `block_current_until` + a proper waker.
3. Move on.

Pre-emption should be deliberate, not driven by a specific bug. Plan it as 57b/57c/57d as outlined above; don't squeeze it in under time pressure.
