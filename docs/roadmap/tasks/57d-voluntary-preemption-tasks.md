# Phase 57d — Voluntary Preemption (PREEMPT_VOLUNTARY): Task List

**Status:** Planned
**Source Ref:** phase-57d
**Depends on:** Phase 3 ✅, Phase 4 ✅, Phase 25 ✅, Phase 35 ✅, Phase 57a ✅, Phase 57b ✅
**Goal:** Activate the 57b foundation by firing preemption at the IRQ-return boundary whenever the interrupted code is in user mode, `preempt_count == 0`, and the per-core `reschedule` flag is set.  User-mode CPU-bound tasks become preemptible within one timer tick; kernel-mode code remains non-preemptible.  Closes the latency gap left by `preempt_enable` zero-crossings via a deferred-reschedule record consumed at the next user-mode return boundary.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | TDD foundation (extend `preempt_model`; in-QEMU integration test stubs) | 57b ✅ | Planned |
| B | Naked-asm entry stubs for timer + reschedule IPI (full GPR save before Rust) | A | Planned |
| C | `preempt_to_scheduler` (Rust) + `preempt_resume_to_user` (asm) + `PreemptTrapFrame` layout | A, B | Planned |
| D | Dispatch integration (`Task::resume_mode`, dual-resume dispatch) | C | Planned |
| E | `preempt_enable` deferred-reschedule (zero-crossing record + user-mode-return consumer) | 57b ✅ | Planned |
| F | IRQ-handler preempt-count read using 57b's `current_preempt_count_ptr` | 57b ✅ | Planned |
| G | IRQ-return preemption check wired into Rust handlers | C, D, E, F | Planned |
| H | Stress test and validation gate | G | Planned |
| I | Default-on flip and feature-flag removal | H | Planned |

Tracks A–C are the foundation; D wires dispatch; E closes the deferred-reschedule latency gap; F wires the lock-free IRQ-side preempt-count read; G activates preemption (gated on `cfg(feature = "preempt-voluntary")`); H/I validate and roll out.

## Engineering Practice Gates (apply to every track)

- **TDD.**  Every implementation commit references a test commit landed earlier.  Tests added in the same commit as implementation are rejected.
- **SOLID.**  `preempt_to_scheduler` saves and switches; the scheduler picks; `preempt_resume_to_user` restores.  Each routine has one job.
- **DRY.**  Single `preempt_to_scheduler` for both timer and reschedule-IPI paths.  Single `preempt_resume_to_user` for restore.
- **Documented invariants.**  `from_user` check, `preempt_count == 0` precondition, `reschedule` flag set/clear semantics, `preempt_resched_pending` consumed-once-at-user-mode-return semantics.  Each documented at the relevant entry point.
- **Lock ordering.**  Naked-asm entry stub does not acquire any lock.  Rust handler reads atomics with `Relaxed` / `Acquire` ordering — no scheduler lock acquired in IRQ context.
- **Migration safety.**  IRQ-return check + asm-stub replacement gated on `cfg(feature = "preempt-voluntary")`.  Default off until H validates; flip in I.
- **Observability.**  Every preemption emits a `[TRACE] [preempt]` line under `--features sched-trace`.

---

## Track A — TDD Foundation

### A.1 — Extend `kernel-core::preempt_model` with preemption transition

**File:** `kernel-core/src/preempt_model.rs` (extended from 57b)
**Symbol:** `Event::Preempt`, `apply_preempt`, `Event::PreemptEnableZeroCrossing`
**Why it matters:** The state machine must capture both the IRQ-return preemption transition and the `preempt_enable` zero-crossing so property tests can assert correctness before any kernel-side implementation lands.

**Acceptance:**
- [ ] `Event::Preempt` added; `apply_preempt(state, count, reschedule, from_user) -> state` returns `Ready` when all four conditions hold; otherwise returns `state` unchanged.
- [ ] `Event::PreemptEnableZeroCrossing` added; sets `preempt_resched_pending` if the post-decrement count is 0 and `reschedule` is set.
- [ ] Property test: random sequences of (preempt, lock_acquire, lock_release, syscall_enter, syscall_exit) preserve the invariant `preempt_count == 0 at user-mode return`.
- [ ] Property test: a preempt that fires when `preempt_count > 0` returns `state` unchanged.
- [ ] Property test: a preempt that fires when `from_user == false` returns `state` unchanged (regression guard against accidental kernel-mode preemption — that's 57e).
- [ ] Property test: `preempt_enable` zero-crossing while `reschedule` is set always sets `preempt_resched_pending`; the next user-mode return consumes it.
- [ ] `cargo test -p kernel-core` passes.

### A.2 — In-QEMU integration test stubs

**File:** `kernel/tests/preempt_voluntary.rs` (new)
**Symbol:** —
**Why it matters:** The integration tests must exist in stub form before the implementation so the test contract is defined.

**Acceptance:**
- [ ] Stub test: `preempt_user_loop` — spawn a userspace task in a tight loop; assert it gets preempted within 100 ms.
- [ ] Stub test: `no_preempt_when_count_nonzero` — spawn a task that holds a `preempt_disable`; assert no preemption.
- [ ] Stub test: `no_preempt_when_kernel_mode` — spawn a task running a kernel-mode busy-loop (without `preempt_disable`); assert no preemption (because `from_user == false`).
- [ ] Stub test: `preempt_enable_zero_crossing` — drive a wake while a lock is held, release the lock, assert the next user-mode return triggers the deferred reschedule.
- [ ] Stubs compile and run (initially marked `#[ignore]`); G.x removes the ignore once preemption is wired.

---

## Track B — Naked-Asm Entry Stubs

### B.1 — Define `PreemptTrapFrame` layout

**File:** `kernel/src/arch/x86_64/preempt_trap_frame.rs` (new)
**Symbol:** `PreemptTrapFrame`
**Why it matters:** The asm stubs and the Rust handlers must agree on the on-stack layout exactly.  A `#[repr(C)]` struct with explicit ordering plus `core::mem::offset_of!` constants pins the agreement.

**Acceptance:**
- [ ] `#[repr(C)]` struct: `gprs: [u64; 15]` (rax, rbx, rcx, rdx, rsi, rdi, rbp, r8..r15) followed by the CPU-pushed iretq frame (`rip, cs, rflags, rsp, ss` for ring-3-interrupted, or `rip, cs, rflags` for ring-0-interrupted with `rsp/ss` slots zeroed).
- [ ] Compile-time test pinning every field offset.
- [ ] Doc comment cites the Intel SDM Vol 3A §6.14 reference for IRQ stack frame layout.

### B.2 — Implement `timer_entry` naked-asm stub

**Files:**
- `kernel/src/arch/x86_64/asm/preempt_entry.S` (new)
- `kernel/src/arch/x86_64/interrupts.rs` (replace `timer_handler` with a stub that calls `timer_entry`; the Rust body becomes `timer_handler_with_frame(&mut PreemptTrapFrame)`)

**Symbol:** `timer_entry`, `timer_handler_with_frame`
**Why it matters:** Without this, the Rust handler body has already clobbered caller-saved GPRs by the time `preempt_to_scheduler` runs — saving them at that point would capture the *handler's* state, not the interrupted task's.

**Acceptance:**
- [ ] **Ring-0 frame normalization (self-contained in 57d).**  On entry, branch on `(cs & 3)`.  For ring-3-interrupted code (rpl=3), use the CPU-pushed 5-field iretq frame as-is.  For ring-0-interrupted code (rpl=0), the CPU pushed only `rip, cs, rflags` (3 fields); the stub *synthesizes* the missing `ss` and `rsp` slots in `PreemptTrapFrame` order, with both initialised to zero in 57d.  The synthetic `rsp` slot becomes load-bearing in 57e (Track C.0 changes the value from zero to the captured pre-stub kernel RSP); 57e does *not* add the slots themselves.  On the non-preempting return path, the matching ring-test pops the synthetic slots before `iretq` so the CPU sees the original 3-field frame.
- [ ] After normalization, `timer_entry` saves all 15 GPRs in `PreemptTrapFrame.gprs` order onto the now-uniform 5-field-iretq stack.
- [ ] Stub passes `rsp` (which points to the bottom of the saved frame) as the first argument to `timer_handler_with_frame`.
- [ ] **System V AMD64 ABI invariants preserved across the `extern "C"` call (self-contained in 57d, no cross-phase dependency):**
  - RSP is 16-byte aligned at the call instruction's boundary (i.e., 16-byte aligned *before* the call; the call's implicit `push rip` brings it to `≡ 8 (mod 16)` inside the callee).  Math: after ring-0 normalization the stack always carries a 5-field iretq frame (40 bytes) plus 15 × 8 = 120 GPR bytes.  Total 160 bytes pushed since CPU entry.  At CPU entry RSP was 16-byte aligned (Intel SDM Vol 3A §6.14.1 guarantees this for the iretq frame slot).  160 mod 16 == 0, so the stub does *not* need an extra pad — but it must `cld` and avoid clobbering the alignment with any unbalanced subsequent operation.  If the GPR layout ever changes such that the total no longer divides cleanly by 16, the stub adds an explicit `sub rsp, 8` pad and undoes it after `call`.
  - All caller-saved registers above what the stub already pushed are clobbered freely by the Rust handler; the stub does not re-save them.
  - Direction flag (`DF`) is cleared on entry to the Rust call (per ABI) — the stub `cld`s before the `call`.
  - The Rust handler returns normally (i.e., when *not* preempting); the stub pops the GPRs in reverse order, undoes synthetic slots if applicable, and `iretq`s.  When preempting, `preempt_to_scheduler` is `-> !` and the pop/iretq epilogue is unreachable on that path.
- [ ] In-QEMU test: a synthetic *ring-3* interrupt fired with known register values reaches the Rust handler with all 15 GPRs preserved in the trap frame; the iretq frame fields match the CPU-pushed values.
- [ ] In-QEMU test: a synthetic *ring-0* interrupt produces a `PreemptTrapFrame` whose layout is byte-identical to the ring-3 case (same offsets), with the synthetic `ss` and `rsp` slots both zero.
- [ ] In-QEMU test (alignment regression): a Rust handler that contains `movaps` against an aligned local does not fault — proves the stub's stack alignment is correct in both ring-0 and ring-3 entry paths.
- [ ] In-QEMU test (round-trip): non-preempting return from a ring-0 interrupt restores the original 3-field iretq frame so the CPU's `iretq` consumes the right number of bytes.

### B.3 — Implement `reschedule_ipi_entry` naked-asm stub

**Files:**
- `kernel/src/arch/x86_64/asm/preempt_entry.S`
- `kernel/src/arch/x86_64/interrupts.rs` (replace `reschedule_ipi_handler` similarly)

**Symbol:** `reschedule_ipi_entry`, `reschedule_ipi_handler_with_frame`
**Why it matters:** Cross-core wakes deliver via the reschedule IPI; the same preemption check must fire on the receiving core.  Same correctness reasoning as B.2.

**Acceptance:**
- [ ] Mirror of B.2 for the reschedule IPI vector — same GPR layout, same ABI invariants, same alignment regression test.
- [ ] Shares the GPR save/restore macro with `timer_entry` to satisfy DRY.

### B.4 — IDT installation for naked-asm entry symbols

**Files:**
- `kernel/src/arch/x86_64/interrupts.rs` (IDT init)
- `kernel/src/arch/x86_64/asm/preempt_entry.S`

**Symbol:** IDT timer + reschedule-IPI entry installation
**Why it matters:** The current IDT init uses `idt[InterruptIndex::Timer as u8].set_handler_fn(timer_handler)`, which only accepts an `extern "x86-interrupt" fn(InterruptStackFrame)` symbol.  A raw assembly symbol (`timer_entry`) does not have that Rust signature.  Without explicit guidance, an implementer might wrap the asm in a thin `extern "x86-interrupt"` shim — defeating the entire point of B.2 by re-introducing the Rust-side caller-saved-clobber window the stub is designed to avoid.

**Acceptance:**
- [ ] `timer_entry` and `reschedule_ipi_entry` are exposed as `extern "C"` symbols whose addresses can be read in Rust.
- [ ] IDT install path mutates the `InterruptDescriptorTable` entry directly and calls `unsafe { idt[InterruptIndex::Timer as u8].set_handler_addr(VirtAddr::new(timer_entry as usize as u64)) }` (the `x86_64` crate at version 0.15.4 exposes `Entry::set_handler_addr(&mut self, VirtAddr)` as `unsafe`; this is the canonical raw-handler path that bypasses the `extern "x86-interrupt"` signature requirement).  The same shape applies to `reschedule_ipi_entry` at the reschedule-IPI vector.
- [ ] Existing IDT options from the current `set_handler_fn` path (IST index, present bit, DPL) are preserved by reading them from the prior entry before overwriting, or by re-setting them explicitly after `set_handler_addr`.
- [ ] Rationale documented in code: the stub *is* the IRQ handler; no Rust-side `extern "x86-interrupt"` wrapper exists, by design.
- [ ] Regression test: the IDT entry's `handler_addr` matches `timer_entry as usize`; the entry is `present=1`, IST index matches the prior `timer_handler` entry's IST index.
- [ ] If a future `x86_64` crate upgrade changes the API shape, this task's example is updated in lockstep with the upgrade PR; the asm symbol contract (`extern "C"` global symbol whose address is the IRQ entry point) does not change.

---

## Track C — `preempt_to_scheduler`, `preempt_resume_to_user`

### C.1 — Implement `preempt_to_scheduler` (Rust)

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_to_scheduler`
**Why it matters:** The bridge between the asm-stub-saved trap frame and the scheduler.  Copies the trap frame into the task's `preempt_frame`, marks the task ready, and transfers control to the scheduler dispatch entry — never returns to its caller.

**Acceptance:**
- [ ] `pub unsafe fn preempt_to_scheduler(frame: &mut PreemptTrapFrame) -> !`
- [ ] Body: copy `frame` into `current_task().preempt_frame`; set `state = Ready`, `on_cpu = false`, `resume_mode = Preempted`; run-queue-insert at home-core tail; jump to the per-core scheduler dispatch entry.
- [ ] Function is `-> !` so the asm stub's `pop`/`iretq` epilogue is unreachable on this path.
- [ ] Regression test: a synthetic call with a known frame produces a `preempt_frame` whose bytes match the input frame exactly.

### C.2 — Implement `preempt_resume_to_user` (asm)

**File:** `kernel/src/arch/x86_64/asm/preempt_entry.S`
**Symbol:** `preempt_resume_to_user`
**Why it matters:** The mirror of B.2's restore epilogue, but used by the dispatch path when the chosen task was previously preempted.  Must restore exactly what `preempt_to_scheduler` saved, in the right order, and `iretq` cleanly to ring 3.

**Acceptance:**
- [ ] `pub unsafe fn preempt_resume_to_user(frame: *const PreemptFrame) -> !` exposed via `extern "C"`.
- [ ] Routine restores GPRs from `frame.gprs`.
- [ ] Routine pushes the iretq frame (`ss, rsp, rflags, cs, rip`) onto the current stack from `frame.{ss, rsp, rflags, cs, rip}`.
- [ ] Routine `iretq`s — privilege transition to ring 3.
- [ ] In-QEMU test: a task is preempted, dispatched, and resumed; the resume's RIP and register state match what was saved.

### C.3 — Move `switch_context` inline asm to a separate `.S` file

**Files:**
- `kernel/src/arch/x86_64/asm/switch.S` (new — currently inline `global_asm!` in `task/mod.rs`)
- `kernel/src/task/mod.rs` (remove the `global_asm!` block)
- `kernel/build.rs` (build the new asm)

**Symbol:** `switch_context`
**Why it matters:** Adding two new asm routines plus the entry stubs is cleaner with dedicated `.S` files.  The cooperative path is unchanged.

**Acceptance:**
- [ ] `switch_context` moved verbatim to `kernel/src/arch/x86_64/asm/switch.S`.
- [ ] `kernel/build.rs` invokes the appropriate assembler.
- [ ] Existing `cargo xtask test` passes — no behaviour change.

---

## Track D — Dispatch Integration

### D.1 — Add `Task::resume_mode`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::resume_mode`, `ResumeMode`
**Why it matters:** The dispatch path must know whether to use the cooperative `switch_context` (callee-saved restore via `ret`) or the preempted `preempt_resume_to_user` (full restore via `iretq`).  `resume_mode` is a discriminant (single source of truth for the resume contract), not a flag — so it does not violate 57b's "no new flag fields" gate.

**Acceptance:**
- [ ] `Task::resume_mode: AtomicU8` field, initialised to `ResumeMode::Initial`.
- [ ] `ResumeMode` enum with variants `Initial`, `Cooperative`, `Preempted`.

### D.2 — Set `resume_mode` at the suspending path

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `block_current_until`, `yield_now`, `preempt_to_scheduler`
**Why it matters:** Each suspension path must set the mode correctly so the dispatch path resumes via the right routine.

**Acceptance:**
- [ ] `block_current_until` sets `resume_mode = Cooperative` before `switch_context`.
- [ ] `yield_now` sets `resume_mode = Cooperative` before `switch_context`.
- [ ] `preempt_to_scheduler` sets `resume_mode = Preempted` before the scheduler RSP swap.
- [ ] Initial dispatch: `resume_mode = Initial → Cooperative` at first dispatch.

### D.3 — Dispatch path reads `resume_mode`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `dispatch`
**Why it matters:** The dispatch path is the consumer of `resume_mode`.  A wrong branch produces an `iretq` from a `switch_context`-saved frame (or vice versa) and the kernel crashes.

**Acceptance:**
- [ ] Dispatch reads `resume_mode` and branches:
  - `Cooperative` / `Initial`: existing `switch_context` path.
  - `Preempted`: new `preempt_resume_to_user` path.
- [ ] Regression test: a task that was cooperatively yielded resumes via `switch_context`; a task that was preempted resumes via `iretq`.
- [ ] Existing `cargo xtask test` passes — no preemption fires yet (Track G gates that).

---

## Track E — `preempt_enable` Deferred-Reschedule

### E.1 — Add `PerCoreData::preempt_resched_pending`

**File:** `kernel/src/smp/mod.rs`
**Symbol:** `PerCoreData::preempt_resched_pending`
**Why it matters:** The flag is the per-CPU record that a `preempt_enable` zero-crossing observed `reschedule == true`.  Consumed at the next user-mode return boundary.

**Acceptance:**
- [ ] `preempt_resched_pending: AtomicBool` field.
- [ ] Initialised false on each per-core init.

### E.2 — Wire `preempt_enable` zero-crossing logic

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_enable`
**Why it matters:** The Linux pattern that closes the "IRQ arrived while preemption disabled" latency gap.  Without it, 57b's promise that `preempt_enable` zero-crossings can fire a deferred reschedule is unfulfilled.

**Acceptance:**
- [ ] After `fetch_sub`, if the previous count was 1 *and* `per_core().reschedule.load(Relaxed) == true`, set `preempt_resched_pending = true` (Release).
- [ ] No scheduler lock acquired (preserves the lock-free invariant from 57b D.2).
- [ ] Under `PREEMPT_VOLUNTARY`, the function does *not* immediately call into the scheduler — the trigger is consumed at the next user-mode return.  This preserves the kernel-mode-non-preemptibility invariant that 57d relies on.

### E.3 — Consume `preempt_resched_pending` at user-mode return

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (syscall return path)
- `kernel/src/arch/x86_64/interrupts.rs` (IRQ return path for IRQs that interrupted user mode — the same place as the IRQ-return preemption check from G.1)

**Symbol:** the user-mode return boundary
**Why it matters:** This is where the deferred trigger becomes a real preemption.  Without consumption, the trigger leaks across user-mode returns and never fires.

**Acceptance:**
- [ ] At every user-mode return boundary, after the `preempt_count == 0` debug assertion, check `per_core().preempt_resched_pending.swap(false, AcqRel)`; if true, run the same scheduler entry as the IRQ-return preemption check.
- [ ] In-QEMU test: a wake fires while a lock is held; the lock is released; the next syscall return preempts the current task.

---

## Track F — Lock-Free Preempt-Count Read in IRQ

### F.1 — Reuse 57b's `current_preempt_count_ptr` in the IRQ path

**File:** `kernel/src/task/scheduler.rs` (helper) and `kernel/src/arch/x86_64/interrupts.rs` (consumer)
**Symbol:** `peek_preempt_count_irq()`
**Why it matters:** The IRQ handler must read `preempt_count` without any lock.  57b already added `PerCoreData::current_preempt_count_ptr` for exactly this purpose.  Reusing the existing primitive avoids the duplicate-fast-index pitfall flagged in PR-131 review.

**Acceptance:**
- [ ] `peek_preempt_count_irq()` performs `(*per_core().current_preempt_count_ptr.load(Acquire)).load(Relaxed)`.
- [ ] Doc comment cites 57b's stable-storage + per-CPU pointer guarantees.
- [ ] Regression test asserts the helper returns a value that matches the lock-acquired path's read.

---

## Track G — IRQ-Return Preemption Check

### G.1 — Wire the check into `timer_handler_with_frame`

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `timer_handler_with_frame`
**Why it matters:** The timer is the canonical preemption trigger.  The handler must read all four conditions (from_user, preempt_count, reschedule, preempt_resched_pending) coherently and call `preempt_to_scheduler` exactly once.

**Acceptance:**
- [ ] After the existing tick + EOI work:
  1. Read `from_user = (frame.cpu_frame.cs & 3) == 3`.
  2. If `!from_user`, return (kernel-mode is non-preemptible in `PREEMPT_VOLUNTARY`).
  3. Read `pc = peek_preempt_count_irq()`.
  4. If `pc != 0`, return.
  5. Read `reschedule = per_core().reschedule.swap(false, AcqRel)`.
  6. Also consume `preempt_resched_pending.swap(false, AcqRel)` for the deferred-reschedule case.
  7. If neither was set, return.
  8. Call `preempt_to_scheduler(frame)`.  Does not return.
- [ ] Gated on `cfg(feature = "preempt-voluntary")`; default off.
- [ ] In-QEMU test: feature-on, spawn a userspace tight loop, observe preemption within 1 ms.

### G.2 — Wire the check into `reschedule_ipi_handler_with_frame`

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `reschedule_ipi_handler_with_frame`
**Why it matters:** Cross-core wakes deliver via the reschedule IPI; the same preemption check must fire on the receiving core.

**Acceptance:**
- [ ] Identical check to G.1.
- [ ] Gated on the same feature flag.
- [ ] In-QEMU test: a wake delivered from core 0 to core 1 (where core 1 is running a tight user loop) preempts within 1 ms of the IPI.

### G.3 — Tracepoint emission

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_to_scheduler`
**Why it matters:** Every preemption must be reachable from the trace ring under `--features sched-trace`; without observability, debugging future preempt-discipline bugs is much harder.

**Acceptance:**
- [ ] Under `cfg(feature = "sched-trace")`, every preemption emits a structured trace entry: `(pid, from_user, preempted_rip, target_pid, tick, trigger)` where `trigger ∈ {timer, reschedule_ipi, preempt_enable_zero_crossing}`.
- [ ] Default off — no overhead in the default build.
- [ ] Manual smoke: enable feature, reproduce a preemption, dump the trace ring, see the entry.

---

## Track H — Stress Test and Validation

### H.1 — Activate stub tests

**File:** `kernel/tests/preempt_voluntary.rs` (extended from A.2)
**Symbol:** —
**Why it matters:** The A.2 stubs become live tests under feature-on.

**Acceptance:**
- [ ] `preempt_user_loop` passes — a tight userspace loop is preempted within 1 ms; another task on the same core makes forward progress.
- [ ] `no_preempt_when_count_nonzero` passes — a kernel task with `preempt_disable` held is not preempted.
- [ ] `no_preempt_when_kernel_mode` passes — a kernel-mode busy-loop without `preempt_disable` is not preempted (because `from_user == false`).
- [ ] `preempt_enable_zero_crossing` passes — the deferred trigger fires at the next user-mode return.

### H.2 — User-loop stress test

**File:** `kernel/tests/preempt_user_stress.rs` (new)
**Symbol:** —
**Why it matters:** A 5-minute stress test confirms preemption under realistic load doesn't reveal hidden preempt-discipline bugs.

**Acceptance:**
- [ ] Spawn 4 userspace tight-loop tasks (one per core) plus a "metronome" task that increments a counter every 10 ms.
- [ ] Run for 5 minutes.
- [ ] Assert the metronome counter is within ±5 % of `30_000` (300 s × 100 ticks/s).
- [ ] No `[WARN] [sched]` lines.  No panics.  No deadlocks.

### H.3 — Real-hardware acceptance gate

**File:** procedural; results in `docs/handoffs/57d-validation-gate.md`
**Symbol:** —
**Why it matters:** The 57a I.1 gate.  If 57c already passed it, 57d should also pass it (defence in depth).  If 57c did not, 57d should now pass it.

**Acceptance:**
- [ ] On user test hardware, `cargo xtask run-gui --fresh` with `preempt-voluntary` enabled: cursor moves, keyboard echoes, `term` reaches `TERM_SMOKE:ready`.
- [ ] Repeated 5 times, 5 successes.
- [ ] Zero `[WARN] [sched]` lines.

### H.4 — 30 + 30 min soak

**File:** procedural
**Symbol:** —
**Why it matters:** Catches preempt-discipline bugs that only appear under sustained load.

**Acceptance:**
- [ ] 30 min idle + 30 min synthetic load on 4 cores with `preempt-voluntary` enabled.
- [ ] Zero `[WARN] [sched] cpu-hog` warnings whose corrected `ran` exceeds 200 ms.
- [ ] Zero `[WARN] [preempt]` lines.
- [ ] No deadlocks, panics, or scheduler hangs.

---

## Track I — Default-On Flip

### I.1 — Flip feature default to on

**Files:**
- `kernel/Cargo.toml`
- `xtask/src/main.rs` (if the build path needs adjustment)

**Symbol:** `preempt-voluntary` feature default
**Why it matters:** The phase isn't done until the default build runs with preemption enabled.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `default = ["preempt-voluntary"]` (or equivalent).
- [ ] `cargo xtask check` clean.
- [ ] `cargo xtask test` passes — preemption is on for every test.

### I.2 — 24-hour post-flip soak

**File:** procedural
**Symbol:** —
**Why it matters:** Final confidence gate.  A 24-hour soak with the default build catches discipline bugs that escaped the 1-hour soak.

**Acceptance:**
- [ ] 24-hour soak with `cargo xtask run --device e1000` plus a synthetic load (SSH disconnect/reconnect script + IPC ping/pong + futex wait/wake).
- [ ] No regressions; results documented.

### I.3 — Remove the feature flag

**Files:**
- `kernel/Cargo.toml`
- All `cfg(feature = "preempt-voluntary")` callsites in `kernel/src/`

**Symbol:** —
**Why it matters:** Cleanup.  After the soak passes, the flag is dead code.

**Acceptance:**
- [ ] Feature flag removed from `Cargo.toml`.
- [ ] All `cfg(feature = "preempt-voluntary")` blocks unwrapped to be unconditional.
- [ ] `git grep preempt-voluntary` returns zero results.

### I.4 — Documentation update

**Files:**
- `docs/03-interrupts.md`
- `docs/04-tasking.md`
- `docs/roadmap/README.md`
- `kernel/Cargo.toml` (version bump)
- `kernel/src/main.rs` (banner)

**Symbol:** —
**Why it matters:** The phase landing must be documented.

**Acceptance:**
- [ ] `docs/03-interrupts.md` updated to describe the asm entry stubs and IRQ-return preemption check.
- [ ] `docs/04-tasking.md` updated to describe the dual-resume dispatch path and the `preempt_enable` deferred-reschedule.
- [ ] `docs/roadmap/README.md`: Phase 57d row marked Complete; mermaid graph updated.
- [ ] Kernel version bumped.
- [ ] Boot banner reflects the new version.

---

## Documentation Notes

- This phase activates the 57b foundation.  Without 57b's stable per-task storage, `current_preempt_count_ptr`, `preempt_count`, and `PreemptFrame`, this phase cannot land.
- This phase does **not** depend on 57c.  57c reduces kernel-mode CPU monopoly; 57d adds user-mode preemption.  The two are complementary fixes for the same user pain.
- Track B's naked-asm entry stubs are non-negotiable for correctness: a Rust `extern "x86-interrupt"` cannot save the interrupted task's full GPR state because the compiler's prologue can clobber caller-saved registers before the explicit preemption check runs.  This was the central correctness blocker raised in the PR-131 review.
- Track E's `preempt_enable` deferred-reschedule closes the latency gap between "lock released, reschedule pending" and "next timer tick".  Under `PREEMPT_VOLUNTARY` the trigger is recorded and consumed at the next user-mode return — never immediately, because kernel-mode is non-preemptible in this phase.
- Track F deliberately reuses 57b's `current_preempt_count_ptr` rather than introducing a duplicate `current_task_idx_fast`.  The existing `PerCoreData::current_task_idx` (Phase 35 / 57a) is unchanged.
- The `preempt-voluntary` feature flag is a rollback safety net.  The flag is removed in I.3 only after the 24-hour soak passes.
