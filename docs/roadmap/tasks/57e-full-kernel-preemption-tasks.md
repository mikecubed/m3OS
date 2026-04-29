# Phase 57e — Full Kernel Preemption (PREEMPT_FULL): Task List

**Status:** Planned
**Source Ref:** phase-57e
**Depends on:** Phase 57b ✅, Phase 57c ✅, Phase 57d ✅
**Goal:** Drop the `from_user` check from 57d's IRQ-return preemption point.  Kernel-mode code becomes preemptible at any point where `preempt_count == 0`.  Per-trigger latency floors improve over the 57d baseline; cross-core IPI wakeup is the only path expected to drop into the microsecond range.  This is the **stretch goal** of the 57b/c/d/e programme — the realistic 1.0 release target is `PREEMPT_VOLUNTARY` parity at end of 57d.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit (kernel preempt invariants — second pass over 57c catalogue) | 57c, 57d ✅ | Planned |
| B | `preempt_disable` wrapping (per-callsite, where 57c annotated "annotate") | A | Planned |
| C | `preempt_resume_to_kernel` assembly + Rust shim | 57d B ✅ | Planned |
| D | Dispatch reentrancy audit | A, C | Planned |
| E | Latency benchmarks (round-trip IPC, syscall wakeup) | 57d ✅ | Planned |
| F | Drop the `from_user` check | A–E | Planned |
| G | 24-hour soak | F | Planned |
| H | Default-on flip and feature-flag removal | G | Planned |

## Engineering Practice Gates (apply to every track)

- **TDD.**  Every implementation commit references a test commit landed earlier.  Latency benchmarks land before the headline change so the "before" baseline is captured.
- **SOLID.**  `preempt_resume_to_kernel` only restores kernel-mode tasks; `preempt_resume_to_user` only restores user-mode.  No code branches on ring inside a single routine.
- **DRY.**  `_user` and `_kernel` resume variants share **only the GPR-restore portion** via a `_preempt_resume_common` macro.  The iretq frame layout and RSP handling are variant-specific.
- **Documented invariants.**  The `from_user` check is the *only* difference between 57d and 57e in the preemption decision; documented at the IRQ handler.  Every kernel busy-spin in 57c's catalogue is annotated with whether `preempt_disable` is required under `PREEMPT_FULL`.
- **Lock ordering.**  Unchanged from 57d.
- **Migration safety.**  Headline change gated on `cfg(feature = "preempt-full")`.  Default off until G validates; flip in H.
- **Observability.**  57d `[TRACE] [preempt]` line gains `kernel_mode=true|false` field.

---

## Track A — Audit

### A.1 — Second pass over 57c catalogue

**File:** `docs/handoffs/57e-kernel-preempt-audit.md` (new)
**Symbol:** —
**Why it matters:** Every kernel codepath must be classified for `PREEMPT_FULL` safety.  A missed callsite is a deadlock waiting to happen.

**Acceptance:**
- [ ] Markdown table with rows = every entry from `docs/handoffs/57c-busy-wait-audit.md` plus every spinlock callsite from `docs/handoffs/57b-spinlock-callsite-audit.md`.
- [ ] Columns: file:line, symbol, spin pattern, current `preempt_disable` discipline (none, IrqSafeMutex-inherited, explicit), required under PREEMPT_FULL, rationale.
- [ ] Every "annotate" entry from 57c maps to a Track B task that adds the `preempt_disable` wrapper.
- [ ] Every "convert" entry from 57c is verified preempt-safe (block+wake calls already preempt-safe by construction; verify).

### A.2 — Identify dispatch-path reentrancy windows

**File:** `docs/handoffs/57e-dispatch-reentrancy.md` (new)
**Symbol:** —
**Why it matters:** Under `PREEMPT_FULL`, the dispatch path itself can be preempted.  Each window where this is unsafe must be `preempt_disable`-wrapped.

**Acceptance:**
- [ ] Identifies every window in `pick_next` and `dispatch` where preemption would corrupt state:
  - `SCHEDULER.lock` held: `preempt_count > 0` → safe.
  - Post-`pick_next`, pre-`switch_context` window: brief; benign-preemption case (chosen task goes back on queue).
  - `switch_context` body: IF=0 between `cli` and `popf` → safe.
  - `preempt_resume_to_kernel` body: IF=0 until `iretq` → safe.
- [ ] Each window has a regression test that exercises preemption at that point.

---

## Track B — `preempt_disable` Wrapping

For each "annotate" entry in 57c that requires `preempt_disable` under `PREEMPT_FULL`, wrap the spin.  One PR per subsystem.

### B.1 — `kernel/src/smp/`

**Files:**
- `kernel/src/smp/ipi.rs` — `wait_icr_idle`
- `kernel/src/smp/tlb.rs` — TLB shootdown ack-wait
- `kernel/src/smp/boot.rs` — AP boot wait

**Symbol:** each spin's enclosing function
**Why it matters:** SMP busy-spins must not be preempted — preemption mid-spin would block the holder's IPI delivery.

**Acceptance:**
- [ ] Each spin wrapped in `preempt_disable` / `preempt_enable`.
- [ ] Regression test asserts no preemption fires inside the spin (verify via tracepoint count).

### B.2 — `kernel/src/iommu/`, `kernel/src/arch/x86_64/`, `kernel/src/mm/`, `kernel/src/rtc.rs`

**Files:** all per-subsystem locations from 57c Track C
**Symbol:** each spin's enclosing function
**Why it matters:** Same as B.1 per subsystem.

**Acceptance:**
- [ ] Every "annotate" spin from 57c Track C now has a `preempt_disable` wrapper.
- [ ] Regression tests per subsystem confirm preemption skip.

### B.3 — Per-CPU data accesses

**Files:** every callsite that uses `try_per_core()` / `per_core()` for stateful access (not just read)
**Symbol:** the per-CPU access pattern
**Why it matters:** Under `PREEMPT_FULL`, a task that reads a per-CPU value, gets preempted, migrates to another core, and resumes will see the new core's per-CPU value — silent data race.  `preempt_disable` around the access prevents migration.

**Acceptance:**
- [ ] Every per-CPU stateful access is wrapped in `preempt_disable`.
- [ ] Audit doc lists each callsite.
- [ ] Regression test demonstrates the protection works.

---

## Track C — `preempt_resume_to_kernel` (Same-CPL `iretq` Frame)

### C.0 — Make 57d's synthetic ring-0 `rsp` slot load-bearing

**File:** `kernel/src/arch/x86_64/asm/preempt_entry.S` (extends 57d Track B.2's already-present synthetic slots)
**Symbol:** `timer_entry`, `reschedule_ipi_entry`
**Why it matters:** 57d Track B.2 already synthesizes `rsp` and `ss` slots in the ring-0 branch (initialised to zero) so `PreemptTrapFrame` has a uniform layout for the Rust handler.  57d does not preempt ring-0 code, so the zero values are harmless.  57e *does* preempt ring-0 code and the `_kernel` resume routine sets RSP to `preempt_frame.rsp` — so this slot must contain the actual interrupted kernel RSP, not zero.  C.0 is therefore a value change, not a layout change.

**Acceptance:**
- [ ] In the ring-0 entry branch in `timer_entry` and `reschedule_ipi_entry`, replace the `mov qword ptr [rsp + 8], 0` synthetic-slot initialisation with code that captures the pre-stub kernel RSP into the slot.  The capture uses `lea rax, [rsp + 24]` (or whatever offset addresses the byte just past the CPU-pushed 3-field iretq frame, equivalent to RSP at the moment immediately before CPU dispatch) before the `sub rsp, 16` that creates the slots — the captured value is the interrupted kernel-stack RSP.
- [ ] `PreemptTrapFrame.ss` slot remains zero (same-CPL `iretq` does not pop it; setting it to a kernel SS value is harmless but unnecessary).
- [ ] On the non-preempting return path, the synthetic slots are still popped before `iretq` (unchanged from 57d Track B.2).
- [ ] In-QEMU test: a synthetic ring-0 interrupt produces a `PreemptTrapFrame` whose `rsp` matches the kernel-stack pointer at the moment of CPU entry.
- [ ] No layout or offset change to `PreemptTrapFrame` — only the value written into the synthetic `rsp` slot changes from zero to the captured RSP.

### C.1 — Implement `preempt_resume_to_kernel` (assembly, same-CPL `iretq`)

**File:** `kernel/src/arch/x86_64/asm/preempt_entry.S`
**Symbol:** `preempt_resume_to_kernel`
**Why it matters:** Same-CPL `iretq` is structurally different from privilege-changing `iretq`.  The CPU pops only `rip, cs, rflags` (no `rsp`, no `ss`).  Pushing 5 fields and `iretq`ing would corrupt the stack.

**Acceptance:**
- [ ] Routine restores GPRs from `Task::preempt_frame.gprs`.
- [ ] Routine sets `RSP = preempt_frame.rsp` (placing the stack pointer where the interrupted code was running).
- [ ] Routine pushes only 3 fields onto that stack: `rip, cs, rflags` (in iretq pop order).
- [ ] Routine `iretq`s.  CPU pops the 3 fields and resumes at `rip` in ring 0.
- [ ] In-QEMU test: a kernel task is preempted, dispatched, and resumed; the resumed task's RIP, RSP, and GPRs match what was saved.
- [ ] Negative test: pushing 5 fields and `iretq`ing produces a fault (validates the test catches the wrong frame shape).

### C.2 — Dispatch path inspects `cs & 3` and routes correctly

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `dispatch`
**Why it matters:** The dispatch path must choose between `_user` and `_kernel` resume routines based on the saved `cs.rpl()`.  A wrong branch produces a privilege-changing iretq from a same-CPL frame (or vice versa), which faults.

**Acceptance:**
- [ ] Dispatch reads `Task::preempt_frame.cs & 3` and routes:
  - rpl == 3 → `preempt_resume_to_user` (5-field iretq frame from 57d Track C.2).
  - rpl == 0 → `preempt_resume_to_kernel` (3-field iretq frame from C.1).
- [ ] Regression test: a user-mode preemption resumes via `_user`; a kernel-mode preemption resumes via `_kernel`.
- [ ] Negative test: a deliberately misrouted task (e.g., user-mode `cs` with `_kernel` resume) faults — confirming the branch is the only thing standing between the two paths.

### C.3 — Factor shared GPR-restore macro

**File:** `kernel/src/arch/x86_64/asm/preempt_entry.S`
**Symbol:** `_preempt_resume_common` macro
**Why it matters:** DRY — GPR restore and segment-load are identical between the two variants.  The iretq frame layout and RSP handling are variant-specific and *not* shared.

**Acceptance:**
- [ ] `_preempt_resume_common` macro covers GPR restore only.
- [ ] Variant routines call the macro then handle their own iretq frame layout.
- [ ] No regression from C.1 / C.2 tests.

---

## Track D — Dispatch Reentrancy Audit

### D.1 — Validate dispatch windows

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `pick_next`, `dispatch`
**Why it matters:** Each window identified in A.2 must have its preemption-safety property tested.

**Acceptance:**
- [ ] Each window has a regression test that fires preemption at that point and asserts no corruption (no panic, no deadlock, no stale state).
- [ ] If a window requires explicit `preempt_disable`, the wrapper is added and the test fails before / passes after.

### D.2 — Property test for kernel-mode preemption transitions

**File:** `kernel-core/src/preempt_model.rs` (extended)
**Symbol:** kernel-mode preempt event
**Why it matters:** Property tests cover random sequences of (kernel-mode preempt, lock acquire, lock release, syscall enter, syscall exit) and assert the invariants hold.

**Acceptance:**
- [ ] Property test runs ≥ 10 000 random sequences.
- [ ] Asserts: preemption only fires when `preempt_count == 0`.
- [ ] Asserts: preemption never fires while `SCHEDULER.lock` is held (because `SCHEDULER.lock` raises `preempt_count`).
- [ ] Asserts: a preempted kernel-mode task resumes to its kernel-mode `rip`.

---

## Track E — Latency Benchmarks (per-trigger)

Each benchmark establishes a 57d baseline first (run with `preempt-full` *off*) and then measures under 57e (run with `preempt-full` *on*).  The four benchmarks measure structurally different trigger paths because dropping `from_user` affects them by very different amounts.

### E.1 — Cross-core reschedule-IPI wakeup benchmark

**File:** `kernel/tests/preempt_latency.rs` (new)
**Symbol:** `bench_cross_core_ipi_wakeup`
**Why it matters:** This is the path where `PREEMPT_FULL` is expected to deliver the biggest latency improvement.  Under 57d, an IPI delivered to a core in kernel mode is ignored by the preemption check (`from_user == false`); under 57e it preempts immediately.

**Acceptance:**
- [ ] Task A on core 0 wakes Task B blocked on core 1 via futex; measure wake-to-dispatch latency.
- [ ] Reports median, P95, P99 over 1000 iterations.
- [ ] 57d baseline captured with `preempt-full` off.  57e measurement with `preempt-full` on.
- [ ] Acceptance: 57e P95 < 57d P95 *by a measured factor reported in the PR description*.  Target ≥10× drop; merge-blocking only if the measured factor is ≤1×.

### E.2 — Same-core wakeup benchmark

**File:** `kernel/tests/preempt_latency.rs`
**Symbol:** `bench_same_core_wakeup`
**Why it matters:** `PREEMPT_FULL` does *not* add a self-IPI; same-core wakes still rely on the next timer tick or `preempt_enable` zero-crossing.  This benchmark establishes that 57e does not silently regress this path while improving the cross-core path.

**Acceptance:**
- [ ] Task A on core 0 wakes Task B *also on core 0* via futex.
- [ ] Reports median, P95, P99 over 1000 iterations.
- [ ] Acceptance: 57e P95 ≤ 57d P95 + 5 % (no regression).
- [ ] No order-of-magnitude improvement is claimed for this trigger.

### E.3 — Timer-only kernel-mode preemption benchmark

**File:** `kernel/tests/preempt_latency.rs`
**Symbol:** `bench_kernel_timer_preempt`
**Why it matters:** A kernel-mode CPU-bound loop (without `preempt_disable`) must be preempted at the next timer tick.  Under 57d this never happens; under 57e it must.

**Acceptance:**
- [ ] Spawn a kernel task running a tight loop with `preempt_count == 0`.
- [ ] Measure time from loop start to first preemption.
- [ ] Acceptance: 57e P95 < 1.5 × `1000 / TICKS_PER_SEC` ms (one timer tick plus a margin).

### E.4 — `preempt_enable` zero-crossing benchmark

**File:** `kernel/tests/preempt_latency.rs`
**Symbol:** `bench_preempt_enable_zero_crossing`
**Why it matters:** Under 57d, `preempt_enable` zero-crossings record `preempt_resched_pending` and consume it at the next user-mode return.  Under 57e, kernel-mode `preempt_enable` may fire the scheduler immediately if the calling context is preempt-safe.

**Acceptance:**
- [ ] An IRQ sets `reschedule` while the running task holds a lock; the lock is released; measure release-to-scheduler-entry latency.
- [ ] 57d baseline: latency = time-to-next-user-mode-return (potentially milliseconds depending on workload).
- [ ] 57e target: latency drops to microsecond range when the calling context is preempt-safe.
- [ ] Acceptance: 57e P95 < 57d P95 by a measured factor.

### E.5 — Audio-stack latency probe (qualitative)

**File:** `userspace/audio_server/tests/latency.rs` (new) or in-QEMU integration test
**Symbol:** —
**Why it matters:** End-to-end audio latency is a user-facing metric.  This is *not* a hard-gating benchmark; it confirms the synthetic improvements in E.1 / E.4 translate to a user-visible improvement.

**Acceptance:**
- [ ] Measure frame-to-output latency for the audio_server pipeline.
- [ ] Acceptance: no regression vs 57d baseline; no buffer underruns under 4-task synthetic load.
- [ ] An order-of-magnitude improvement is *not* required.

---

## Track F — Drop the `from_user` Check

### F.1 — Headline decision change

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `timer_handler_with_frame`, `reschedule_ipi_handler_with_frame` (Rust handlers introduced in 57d Track B)
**Why it matters:** Drops the `from_user` early-return that kept kernel mode non-preemptible under `PREEMPT_VOLUNTARY`.  The decision-side change is one conditional removed; the rest of 57e (Tracks A–E and the kernel-mode `preempt_enable` immediacy in Track F.x) is what makes the change safe to ship.

**Acceptance:**
- [ ] In both Rust handlers, drop the `if !from_user { return; }` early-exit.
- [ ] The remaining checks (`preempt_count == 0`, `reschedule` flag swap) are unchanged.
- [ ] Gated on `cfg(feature = "preempt-full")`; default off.
- [ ] In-QEMU test: a kernel-mode CPU-bound task (one without `preempt_disable`) is preempted within 1 ms.

### F.2 — Kernel-mode `preempt_enable` immediate zero-crossing

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_enable`
**Why it matters:** Under 57d, `preempt_enable` zero-crossings record `preempt_resched_pending` and consume it at the next user-mode return — because kernel mode is non-preemptible.  Under 57e, kernel mode is preemptible, so `preempt_enable` may fire the scheduler immediately when the post-decrement count is 0 *and* `reschedule` is set *and* the calling context is preempt-safe (no scheduler lock held, IF state is sane).

**Acceptance:**
- [ ] `preempt_enable` post-decrement: if previous count == 1 *and* `per_core().reschedule` is set, *and* the call is gated by a "kernel preempt-safe" precondition, call into the scheduler immediately rather than only setting `preempt_resched_pending`.
- [ ] The deferred-record path from 57d Track E remains as a fallback for contexts where immediate switch is unsafe.
- [ ] Gated on `cfg(feature = "preempt-full")`; under `preempt-voluntary` only, the 57d behaviour is preserved.
- [ ] Latency benchmark E.4 (preempt_enable zero-crossing) under 57e measures the immediate-switch latency floor, not the deferred-to-user-return floor.

### F.2 — Tracepoint update

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_to_scheduler` (Rust shim)
**Why it matters:** The trace entry must include `kernel_mode` so a future debugger can distinguish 57d-style and 57e-style preemptions.

**Acceptance:**
- [ ] Trace entry includes `kernel_mode: bool` field.
- [ ] Manual smoke: enable feature, reproduce a kernel-mode preemption, dump trace ring, see the entry.

---

## Track G — 24-Hour Soak

### G.1 — Standard graphical-stack workload

**File:** procedural; results in `docs/handoffs/57e-soak-result.md`
**Symbol:** —
**Why it matters:** A 24-hour soak with realistic load is the gate.  Any deadlock, panic, or `[WARN]` line during the soak fails the phase.

**Acceptance:**
- [ ] 24-hour run with `cargo xtask run-gui` plus a synthetic load: SSH disconnect/reconnect every 10 s, IPC ping/pong every 100 ms, futex wait/wake every 50 ms.
- [ ] Zero `[WARN] [sched]` lines.
- [ ] Zero `[WARN] [preempt]` lines.
- [ ] No deadlocks, panics, or scheduler hangs.
- [ ] No buffer underruns in audio_server.

### G.2 — Latency benchmark validation

**File:** procedural; results documented
**Symbol:** —
**Why it matters:** The latency targets from Track E must hold post-soak.

**Acceptance:**
- [ ] Re-run E.1 / E.2 / E.3 benchmarks at the end of the soak; results match the pre-soak measurements within ±10 %.

---

## Track H — Default-On Flip

### H.1 — Flip feature default

**File:** `kernel/Cargo.toml`
**Symbol:** `preempt-full` feature default
**Why it matters:** Phase isn't done until the default build runs PREEMPT_FULL.

**Acceptance:**
- [ ] `default = ["preempt-voluntary", "preempt-full"]` (or equivalent).
- [ ] `cargo xtask check` clean; `cargo xtask test` passes.

### H.2 — Post-flip soak

**File:** procedural
**Symbol:** —
**Why it matters:** Final confidence gate after the default-on flip.

**Acceptance:**
- [ ] 24-hour soak with default build; results match G.1.

### H.3 — Remove the feature flag

**Files:**
- `kernel/Cargo.toml`
- All `cfg(feature = "preempt-full")` callsites

**Symbol:** —
**Why it matters:** Cleanup.

**Acceptance:**
- [ ] Feature flag removed; all `cfg` blocks unwrapped.
- [ ] `git grep preempt-full` returns zero results.

### H.4 — Documentation update

**Files:**
- `docs/03-interrupts.md`
- `docs/04-tasking.md`
- `docs/roadmap/README.md`
- `kernel/Cargo.toml` (version bump)

**Symbol:** —
**Why it matters:** Phase landing must be documented.

**Acceptance:**
- [ ] Documentation updated to describe `PREEMPT_FULL` semantics.
- [ ] Phase 57e row marked Complete in README.
- [ ] Kernel version bumped (e.g., `0.57.5` or `0.58.0` if this is the gate to release 1.0).

---

## Documentation Notes

- This phase is the **stretch goal** of the 57b/57c/57d/57e programme.  Whether to land 57e depends on m3OS's release goals and the soak data from 57c/57d.  A credible release-1.0 plateau exists at 57d (PREEMPT_VOLUNTARY parity with Linux desktop default).
- The decision-side change is a single conditional removed (Track F.1).  The full 57e implementation surface is larger: same-CPL `iretq` resume routine and matching kernel-RSP capture (Track C), per-CPU access audit (Track B.3), kernel-mode `preempt_enable` immediate zero-crossing (Track F.2).  Reviewers should treat the audit catalogue at `docs/handoffs/57e-kernel-preempt-audit.md` as the source of truth for completeness across all of these.
- Every Track B `preempt_disable` wrapper corresponds to an "annotate" decision in 57c.  Reviewers should cross-check 57c's audit for any missed annotations before approving 57e Track B.
- The 24-hour soak gate (G) is the most important checkpoint.  Until G passes, the feature flag stays off in production.  H.3 (flag removal) is the final cleanup after H.2 (post-flip soak) confirms stability.
