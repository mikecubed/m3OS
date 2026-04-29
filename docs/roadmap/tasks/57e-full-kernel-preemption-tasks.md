# Phase 57e — Full Kernel Preemption (PREEMPT_FULL): Task List

**Status:** Planned (Stretch)
**Source Ref:** phase-57e
**Depends on:** Phase 57b ✅, Phase 57c ✅, Phase 57d ✅
**Goal:** Drop the `from_user` check from 57d's IRQ-return preemption point.  Kernel-mode code becomes preemptible at any point where `preempt_count == 0`.  Round-trip IPC and syscall wakeup latency floors drop by ≥10×.

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
- **DRY.**  `_user` and `_kernel` resume variants share an `iretq` core via a macro / helper.
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

## Track C — `preempt_resume_to_kernel`

### C.1 — Implement `preempt_resume_to_kernel` (assembly)

**File:** `kernel/src/arch/x86_64/asm/switch.S`
**Symbol:** `preempt_resume_to_kernel`
**Why it matters:** Mirrors `preempt_resume_to_user` but `iretq`s to ring 0 with kernel selectors.

**Acceptance:**
- [ ] Routine restores GPRs from `Task::preempt_frame.gprs`.
- [ ] Routine pushes the iretq frame with kernel `cs:ss` selectors.
- [ ] Routine `iretq`s to ring 0.
- [ ] In-QEMU test: a kernel task is preempted, dispatched, and resumed; the resume's RIP and register state match what was saved.

### C.2 — Dispatch path inspects `cs.rpl`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `dispatch`
**Why it matters:** The dispatch path must choose between `_user` and `_kernel` resume routines based on the saved `cs:ss` selectors.

**Acceptance:**
- [ ] Dispatch reads `Task::preempt_frame.cs.rpl()` and routes:
  - rpl == 3 → `preempt_resume_to_user`.
  - rpl == 0 → `preempt_resume_to_kernel`.
- [ ] Regression test: a user-mode preemption resumes via `_user`; a kernel-mode preemption resumes via `_kernel`.

### C.3 — Factor shared `iretq` core

**File:** `kernel/src/arch/x86_64/asm/switch.S`
**Symbol:** `_preempt_resume_common` macro
**Why it matters:** DRY — the GPR restore and `iretq` frame push are identical for `_user` and `_kernel`; the difference is the segment selectors.

**Acceptance:**
- [ ] The shared portion is in a macro / helper; the variants only set selector values.
- [ ] No regression from B.x tests.

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

## Track E — Latency Benchmarks

### E.1 — Round-trip IPC benchmark

**File:** `kernel/tests/preempt_latency.rs` (new)
**Symbol:** `bench_ipc_round_trip`
**Why it matters:** The "before" baseline must be captured under 57d before the headline change lands; the "after" measurement under 57e gates the merge.

**Acceptance:**
- [ ] Benchmark sends 1000 IPC requests and times the round-trip; reports median, P95, P99 latency.
- [ ] Baseline (57d): floor ~1 ms (timer tick).
- [ ] Target (57e): floor ~10 µs.
- [ ] CI runs the benchmark; merge requires ≥10× improvement.

### E.2 — Syscall wakeup benchmark

**File:** `kernel/tests/preempt_latency.rs`
**Symbol:** `bench_futex_wakeup`
**Why it matters:** A second latency dimension (futex wait → wake → resume).

**Acceptance:**
- [ ] Benchmark times 1000 futex wait/wake cycles.
- [ ] Baseline / target as E.1.
- [ ] CI runs the benchmark.

### E.3 — Audio-stack latency probe

**File:** `userspace/audio_server/tests/latency.rs` (new) or in-QEMU integration test
**Symbol:** —
**Why it matters:** End-to-end audio latency is a user-facing metric for `PREEMPT_FULL`.

**Acceptance:**
- [ ] Measure frame-to-output latency for the audio_server pipeline.
- [ ] Target: drop measurably below the 57d baseline; no buffer underruns under 4-task synthetic load.

---

## Track F — Drop the `from_user` Check

### F.1 — Headline change

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `timer_handler`, `reschedule_ipi_handler`
**Why it matters:** The single line that makes kernel-mode preemption possible.

**Acceptance:**
- [ ] In `timer_handler` and `reschedule_ipi_handler`, replace `if from_user && ...` with `if ...` (drop the `from_user` term).
- [ ] Gated on `cfg(feature = "preempt-full")`; default off.
- [ ] In-QEMU test: a kernel-mode CPU-bound task (one without `preempt_disable`) is preempted within 1 ms.

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
- The headline change is a single line; the rest of the phase is audit and validation that makes that line safe.  Reviewers should treat the audit catalogue at `docs/handoffs/57e-kernel-preempt-audit.md` as the source of truth for completeness.
- Every Track B `preempt_disable` wrapper corresponds to an "annotate" decision in 57c.  Reviewers should cross-check 57c's audit for any missed annotations before approving 57e Track B.
- The 24-hour soak gate (G) is the most important checkpoint.  Until G passes, the feature flag stays off in production.  H.3 (flag removal) is the final cleanup after H.2 (post-flip soak) confirms stability.
