# Phase 57d — Voluntary Preemption (PREEMPT_VOLUNTARY): Task List

**Status:** Planned
**Source Ref:** phase-57d
**Depends on:** Phase 3 ✅, Phase 4 ✅, Phase 25 ✅, Phase 35 ✅, Phase 57a ✅, Phase 57b ✅
**Goal:** Activate the 57b foundation by firing preemption at the IRQ-return boundary whenever the interrupted code is in user mode, `preempt_count == 0`, and the per-core `reschedule` flag is set.  User-mode CPU-bound tasks become preemptible within one timer tick; kernel-mode code remains non-preemptible.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | TDD foundation (extend `preempt_model`; in-QEMU integration test stubs) | 57b ✅ | Planned |
| B | `preempt_to_scheduler` and `preempt_resume_to_user` assembly + Rust shim | A | Planned |
| C | Dispatch integration (`Task::resume_mode`, dual-resume dispatch) | B | Planned |
| D | Per-CPU fast path (`current_task_idx_fast`, `preempt_count_fast`) | C | Planned |
| E | IRQ-return preemption check (timer + reschedule-IPI handlers) | C, D | Planned |
| F | Stress test and validation gate | E | Planned |
| G | Default-on flip and feature-flag removal | F | Planned |

Tracks A and B are the foundation — they must land first.  Track C wires the dispatch integration.  Track D adds the IRQ-handler hot path.  Track E activates preemption (gated on `cfg(feature = "preempt-voluntary")`).  Tracks F and G are the validation and rollout.

## Engineering Practice Gates (apply to every track)

- **TDD.**  Every implementation commit references a test commit landed earlier.  Tests added in the same commit as implementation are rejected.
- **SOLID.**  `preempt_to_scheduler` saves and switches; the scheduler picks; `preempt_resume_to_user` restores.  Each routine has one job.
- **DRY.**  Single `preempt_to_scheduler` for both timer and reschedule-IPI paths.  Single `preempt_resume_to_user` for restore.
- **Documented invariants.**  `from_user` check, `preempt_count == 0` precondition, `reschedule` flag set/clear semantics.  Each documented at the IRQ handler.
- **Lock ordering.**  IRQ handler reads atomics with `Relaxed` ordering — no locks acquired in IRQ context.
- **Migration safety.**  IRQ-return check gated on `cfg(feature = "preempt-voluntary")`.  Default off until F validates; flip in G.
- **Observability.**  Every preemption emits a `[TRACE] [preempt]` line under `--features sched-trace`.

---

## Track A — TDD Foundation

### A.1 — Extend `kernel-core::preempt_model` with preemption transition

**File:** `kernel-core/src/preempt_model.rs` (extended from 57b)
**Symbol:** `Event::Preempt`, `apply_preempt`
**Why it matters:** The state machine must capture the preemption transition (Running → Ready, with `preempt_frame` populated) so property tests can assert correctness before any kernel-side implementation lands.

**Acceptance:**
- [ ] `Event::Preempt` added; `apply_preempt(state, count, reschedule, from_user) -> state` returns `Ready` when all four conditions hold; otherwise returns `state` unchanged.
- [ ] Property test: random sequences of (preempt, lock_acquire, lock_release, syscall_enter, syscall_exit) preserve the invariant `preempt_count == 0 at user-mode return`.
- [ ] Property test: a preempt that fires when `preempt_count > 0` returns `state` unchanged.
- [ ] Property test: a preempt that fires when `from_user == false` returns `state` unchanged (regression guard against accidental kernel-mode preemption — that's 57e).
- [ ] `cargo test -p kernel-core` passes.

### A.2 — In-QEMU integration test stubs

**File:** `kernel/tests/preempt_voluntary.rs` (new)
**Symbol:** —
**Why it matters:** The integration tests must exist in stub form before the implementation so the test contract is defined.

**Acceptance:**
- [ ] Stub test: `preempt_user_loop` — spawn a userspace task in a tight loop; assert it gets preempted within 100 ms.
- [ ] Stub test: `no_preempt_when_count_nonzero` — spawn a task that holds a `preempt_disable`; assert no preemption.
- [ ] Stub test: `no_preempt_when_kernel_mode` — spawn a task running a kernel-mode busy-loop (without `preempt_disable`); assert no preemption (because `from_user == false`).
- [ ] Stubs compile and run (initially marked `#[ignore]`); E.x removes the ignore once preemption is wired.

---

## Track B — `preempt_to_scheduler` and `preempt_resume_to_user`

### B.1 — Move `switch_context` inline asm to a separate `.S` file

**Files:**
- `kernel/src/arch/x86_64/asm/switch.S` (new)
- `kernel/src/task/mod.rs` (remove the `global_asm!` block)
- `kernel/build.rs` (build the new asm)

**Symbol:** `switch_context`
**Why it matters:** Adding two new routines (`preempt_to_scheduler`, `preempt_resume_to_user`) is cleaner with a dedicated `.S` file.  The cooperative path is unchanged.

**Acceptance:**
- [ ] `switch_context` moved verbatim to `kernel/src/arch/x86_64/asm/switch.S`.
- [ ] `kernel/build.rs` invokes the appropriate assembler.
- [ ] Existing `cargo xtask test` passes — no behaviour change.

### B.2 — Implement `preempt_to_scheduler` (assembly)

**File:** `kernel/src/arch/x86_64/asm/switch.S`
**Symbol:** `preempt_to_scheduler`
**Why it matters:** The full register save is the load-bearing change.  Saving the wrong register, or saving to the wrong offset, produces a corrupt `preempt_frame` that resumes to garbage.

**Acceptance:**
- [ ] Routine saves `rax, rbx, rcx, rdx, rsi, rdi, rbp, r8..r15` into `Task::preempt_frame.gprs[0..15]` using literal offsets from 57b's `PREEMPT_FRAME_OFFSET_*` constants.
- [ ] Routine reads `rip, cs, rflags, rsp, ss` from the IRQ stack frame and saves them into `preempt_frame.{rip,cs,rflags,rsp,ss}`.
- [ ] Routine sets `Task::on_cpu = false` (Release).
- [ ] Routine sets `Task::resume_mode = Preempted` (Relaxed; the dispatch path reads it under the scheduler lock).
- [ ] Routine swaps RSP to the per-core scheduler RSP and jumps to the scheduler dispatch entry.
- [ ] Layout test pinned in 57b A.4 ensures the offsets remain stable.

### B.3 — Implement `preempt_resume_to_user` (assembly)

**File:** `kernel/src/arch/x86_64/asm/switch.S`
**Symbol:** `preempt_resume_to_user`
**Why it matters:** The mirror of B.2.  Must restore exactly what was saved, in the right order, and `iretq` cleanly to ring 3.

**Acceptance:**
- [ ] Routine restores GPRs from `Task::preempt_frame.gprs[0..15]`.
- [ ] Routine pushes the iretq frame (`ss, rsp, rflags, cs, rip`) onto the current stack from `preempt_frame.{ss,rsp,rflags,cs,rip}`.
- [ ] Routine `iretq`s to ring 3.
- [ ] In-QEMU test: a task is preempted, dispatched, and resumed; the resume's RIP and register state match what was saved.

### B.4 — Rust shim around `preempt_to_scheduler`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_to_scheduler` (Rust)
**Why it matters:** The IRQ handler calls into Rust to do the run-queue insertion and `pick_next` lookup; the Rust shim isolates the C-ABI boundary.

**Acceptance:**
- [ ] `preempt_to_scheduler(stack_frame: &InterruptStackFrame, idx: usize)` performs run-queue insertion of the preempted task (state = Ready, on_cpu = false).
- [ ] After insertion, the shim calls the assembly entry point that performs the RSP swap.
- [ ] The shim is `extern "C"` so the asm can call it without name-mangling concerns.

---

## Track C — Dispatch Integration

### C.1 — Add `Task::resume_mode`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::resume_mode`, `ResumeMode`
**Why it matters:** The dispatch path must know whether to use the cooperative `switch_context` (callee-saved restore via `ret`) or the preempted `preempt_resume_to_user` (full restore via `iretq`).

**Acceptance:**
- [ ] `Task::resume_mode: AtomicU8` field, initialised to `ResumeMode::Initial`.
- [ ] `ResumeMode` enum with variants `Initial`, `Cooperative`, `Preempted`.
- [ ] `with_block_state` updated to set the mode at appropriate transitions (see C.2).

### C.2 — Set `resume_mode` at the suspending path

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `block_current_until`, `yield_now`, `preempt_to_scheduler`
**Why it matters:** Each suspension path must set the mode correctly so the dispatch path resumes via the right routine.

**Acceptance:**
- [ ] `block_current_until` sets `resume_mode = Cooperative` before `switch_context`.
- [ ] `yield_now` sets `resume_mode = Cooperative` before `switch_context`.
- [ ] `preempt_to_scheduler` sets `resume_mode = Preempted` before the scheduler RSP swap.
- [ ] Initial dispatch: `resume_mode = Initial → Cooperative` at first dispatch.

### C.3 — Dispatch path reads `resume_mode`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `dispatch`
**Why it matters:** The dispatch path is the consumer of `resume_mode`.  A wrong branch produces an `iretq` from a `switch_context`-saved frame (or vice versa) and the kernel crashes.

**Acceptance:**
- [ ] Dispatch reads `resume_mode` and branches:
  - `Cooperative` / `Initial`: existing `switch_context` path.
  - `Preempted`: new `preempt_resume_to_user` path.
- [ ] Regression test: a task that was cooperatively yielded resumes via `switch_context`; a task that was preempted resumes via `iretq`.
- [ ] Existing `cargo xtask test` passes — no preemption fires yet (Track E gates that).

---

## Track D — Per-CPU Fast Path

### D.1 — Add `current_task_idx_fast` to `PerCoreData`

**File:** `kernel/src/smp/mod.rs`
**Symbol:** `PerCoreData::current_task_idx_fast`
**Why it matters:** The IRQ handler must read `current_task_idx` without acquiring the scheduler lock (which it cannot, in IRQ context).  A per-CPU atomic written on every dispatch lets the IRQ handler do a Relaxed read.

**Acceptance:**
- [ ] `PerCoreData::current_task_idx_fast: AtomicI32`, default `-1`.
- [ ] Updated by the dispatch path on every context switch (write the chosen task's index, or `-1` for the idle task).
- [ ] `current_task_idx_fast()` helper performs a Relaxed read.
- [ ] Regression test: the fast read tracks the slow-path read across a series of dispatches.

### D.2 — Helper to read `Task::preempt_count` without scheduler lock

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `peek_preempt_count(idx: usize) -> i32`
**Why it matters:** The IRQ handler must read `preempt_count` without acquiring the scheduler lock.  A direct Relaxed read of `Task::preempt_count` on the validated index is safe because tasks are not freed during their own runtime.

**Acceptance:**
- [ ] `peek_preempt_count(idx)` performs a Relaxed read of `Task::preempt_count` directly via the task table pointer.
- [ ] Doc comment justifying the unsafe access (task table is stable while the task is alive; the IRQ-context reader cannot race the task's own death because the IRQ fires *during* the task's execution).
- [ ] Regression test asserts the helper returns the same value as the lock-acquired path.

---

## Track E — IRQ-Return Preemption Check

### E.1 — Wire the check into `timer_handler`

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `timer_handler`
**Why it matters:** The timer is the canonical preemption trigger.  The handler must read all three conditions and call `preempt_to_scheduler` atomically.

**Acceptance:**
- [ ] After the existing EOI, the handler:
  1. Reads `from_user = stack_frame.code_segment.rpl() == PrivilegeLevel::Ring3`.
  2. If `!from_user`, returns (kernel-mode is non-preemptible in `PREEMPT_VOLUNTARY`).
  3. Reads `idx = current_task_idx_fast()`.
  4. If `idx == -1` (idle task), returns.
  5. Reads `pc = peek_preempt_count(idx)`.
  6. If `pc != 0`, returns (some lock or explicit disable is held).
  7. Reads `reschedule = per_core().reschedule.load(Relaxed)`.
  8. If `!reschedule`, returns.
  9. Clears `reschedule`; calls `preempt_to_scheduler(&stack_frame, idx)`.  Does not return.
- [ ] Gated on `cfg(feature = "preempt-voluntary")`; default off.
- [ ] In-QEMU test: feature-on, spawn a userspace tight loop, observe preemption within 1 ms.

### E.2 — Wire the check into `reschedule_ipi_handler`

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `reschedule_ipi_handler`
**Why it matters:** Cross-core wakes deliver via the reschedule IPI; the same preemption check must fire on the receiving core.

**Acceptance:**
- [ ] Identical check to E.1, in `reschedule_ipi_handler`.
- [ ] Gated on the same feature flag.
- [ ] In-QEMU test: a wake delivered from core 0 to core 1 (where core 1 is running a tight user loop) preempts within 1 ms of the IPI.

### E.3 — Tracepoint emission

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_to_scheduler` (Rust shim)
**Why it matters:** Every preemption must be reachable from the trace ring under `--features sched-trace`; without observability, debugging future preempt-discipline bugs is much harder.

**Acceptance:**
- [ ] Under `cfg(feature = "sched-trace")`, every preemption emits a structured trace entry: `(pid, from_user, preempted_rip, target_pid, tick)`.
- [ ] Default off — no overhead in the default build.
- [ ] Manual smoke: enable feature, reproduce a preemption, dump the trace ring, see the entry.

---

## Track F — Stress Test and Validation

### F.1 — Activate stub tests

**File:** `kernel/tests/preempt_voluntary.rs` (extended from A.2)
**Symbol:** —
**Why it matters:** The A.2 stubs become live tests under feature-on.

**Acceptance:**
- [ ] `preempt_user_loop` passes — a tight userspace loop is preempted within 1 ms; another task on the same core makes forward progress.
- [ ] `no_preempt_when_count_nonzero` passes — a kernel task with `preempt_disable` held is not preempted.
- [ ] `no_preempt_when_kernel_mode` passes — a kernel-mode busy-loop without `preempt_disable` is not preempted (because `from_user == false`).

### F.2 — User-loop stress test

**File:** `kernel/tests/preempt_user_stress.rs` (new)
**Symbol:** —
**Why it matters:** A 5-minute stress test confirms preemption under realistic load doesn't reveal hidden preempt-discipline bugs.

**Acceptance:**
- [ ] Spawn 4 userspace tight-loop tasks (one per core) plus a "metronome" task that increments a counter every 10 ms.
- [ ] Run for 5 minutes.
- [ ] Assert the metronome counter is within ±5 % of `30_000` (300 s × 100 ticks/s).
- [ ] No `[WARN] [sched]` lines.  No panics.  No deadlocks.

### F.3 — Real-hardware acceptance gate

**File:** procedural; results in `docs/handoffs/57d-validation-gate.md`
**Symbol:** —
**Why it matters:** The 57a I.1 gate.  If 57c already passed it, 57d should also pass it (defence in depth).  If 57c did not, 57d should now pass it.

**Acceptance:**
- [ ] On user test hardware, `cargo xtask run-gui --fresh` with `preempt-voluntary` enabled: cursor moves, keyboard echoes, `term` reaches `TERM_SMOKE:ready`.
- [ ] Repeated 5 times, 5 successes.
- [ ] Zero `[WARN] [sched]` lines.

### F.4 — 30 + 30 min soak

**File:** procedural
**Symbol:** —
**Why it matters:** Catches preempt-discipline bugs that only appear under sustained load.

**Acceptance:**
- [ ] 30 min idle + 30 min synthetic load on 4 cores with `preempt-voluntary` enabled.
- [ ] Zero `[WARN] [sched] cpu-hog` warnings whose corrected `ran` exceeds 200 ms.
- [ ] Zero `[WARN] [preempt]` lines.
- [ ] No deadlocks, panics, or scheduler hangs.

---

## Track G — Default-On Flip

### G.1 — Flip feature default to on

**Files:**
- `kernel/Cargo.toml`
- `xtask/src/main.rs` (if the build path needs adjustment)

**Symbol:** `preempt-voluntary` feature default
**Why it matters:** The phase isn't done until the default build runs with preemption enabled.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `default = ["preempt-voluntary"]` (or equivalent).
- [ ] `cargo xtask check` clean.
- [ ] `cargo xtask test` passes — preemption is on for every test.

### G.2 — 24-hour post-flip soak

**File:** procedural
**Symbol:** —
**Why it matters:** Final confidence gate.  A 24-hour soak with the default build catches discipline bugs that escaped the 1-hour soak.

**Acceptance:**
- [ ] 24-hour soak with `cargo xtask run --device e1000` plus a synthetic load (SSH disconnect/reconnect script + IPC ping/pong + futex wait/wake).
- [ ] No regressions; results documented.

### G.3 — Remove the feature flag

**Files:**
- `kernel/Cargo.toml`
- All `cfg(feature = "preempt-voluntary")` callsites in `kernel/src/`

**Symbol:** —
**Why it matters:** Cleanup.  After the soak passes, the flag is dead code.

**Acceptance:**
- [ ] Feature flag removed from `Cargo.toml`.
- [ ] All `cfg(feature = "preempt-voluntary")` blocks unwrapped to be unconditional.
- [ ] `git grep preempt-voluntary` returns zero results.

### G.4 — Documentation update

**Files:**
- `docs/03-interrupts.md`
- `docs/04-tasking.md`
- `docs/roadmap/README.md`
- `kernel/Cargo.toml` (version bump)
- `kernel/src/main.rs` (banner)

**Symbol:** —
**Why it matters:** The phase landing must be documented.

**Acceptance:**
- [ ] `docs/03-interrupts.md` updated to describe the IRQ-return preemption check.
- [ ] `docs/04-tasking.md` updated to describe the dual-resume dispatch path.
- [ ] `docs/roadmap/README.md`: Phase 57d row marked Complete; mermaid graph updated.
- [ ] Kernel version bumped.
- [ ] Boot banner reflects the new version.

---

## Documentation Notes

- This phase activates the 57b foundation.  Without 57b's `preempt_count` and `PreemptFrame` infrastructure, this phase cannot land.
- This phase does **not** depend on 57c.  57c reduces kernel-mode CPU monopoly; 57d adds user-mode preemption.  The two are complementary fixes for the same user pain.
- The `preempt-voluntary` feature flag is a rollback safety net.  If a regression is found in production, flipping the flag off restores cooperative scheduling immediately.  The flag is removed in G.3 only after the 24-hour soak passes.
- Track E's IRQ-return check is the **only** new behavioural change.  Tracks A–D are infrastructure; Track F validates; Track G rolls out.  This isolates the risk to a single conditional.
