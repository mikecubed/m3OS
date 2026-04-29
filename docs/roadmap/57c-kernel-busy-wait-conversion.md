# Phase 57c — Kernel Busy-Wait Audit and Conversion

**Status:** Planned
**Source Ref:** phase-57c
**Depends on:** Phase 4 (Tasking) ✅, Phase 6 (IPC Core) ✅, Phase 35 (True SMP) ✅, Phase 50 (IPC Completion) ✅, Phase 57a (Scheduler Block/Wake Protocol Rewrite) ✅
**Builds on:** Reuses the Phase 57a `block_current_until` primitive; reuses Phase 6's `Notification` objects; reuses Phase 50's wait-queue infrastructure.  **Independent of Phase 57b** — this phase fixes the user-pain symptom (kernel-mode CPU monopoly) directly, without depending on the preempt-count infrastructure.
**Primary Components:** `kernel/src/blk/` (block-device polling), `kernel/src/net/` (NIC polling), `kernel/src/iommu/` (DMAR/IVRS poll loops), `kernel/src/arch/x86_64/syscall/mod.rs` (syscall busy-spins), `kernel/src/arch/x86_64/ps2.rs` (PS/2 poll loops), `kernel/src/arch/x86_64/apic.rs` (LAPIC poll), `kernel/src/smp/ipi.rs` (`wait_icr_idle`), `kernel/src/smp/tlb.rs` (TLB shootdown wait), `kernel/src/mm/slab.rs` / `kernel/src/mm/frame_allocator.rs` (allocator spin-loops), `kernel/src/main.rs` (boot-time `signal_reschedule` wait)

## Milestone Goal

Every kernel-mode busy-wait that can be triggered by a user-attributable workload either becomes a block+wake pair (preferred) or carries a documented bound on its critical section length.  After this phase, the residual graphical-stack regression catalogued in `docs/handoffs/57a-validation-gate.md` no longer reproduces from a kernel-mode CPU monopoly: every long-running spin in the kernel either parks the task on a wait-queue or completes within a hardware-bounded window.

## Why This Phase Exists

The Phase 57a validation gate I.1 fails because of cooperative-scheduling starvation — kernel busy-spins inside syscalls monopolise the host core.  Two of these have been patched ad hoc (`virtio_blk::do_request` and `sys_poll`'s no-waiter yield-loop) but the appendix's Piece-5 audit identifies more candidates and recommends a systematic conversion.

The 57b/57d/57e programme (preemption infrastructure + IRQ-return preemption check) **does not fix kernel-mode CPU monopoly** when running in `PREEMPT_VOLUNTARY` mode (which is the realistic 57d landing).  The handoff doc states it explicitly:

> Pre-emption only papers over user-mode CPU monopoly; kernel-mode monopoly needs the audit in Piece 5.

This phase delivers Piece 5 as a self-contained body of work that:

1. Provides **immediate, behaviour-changing user-pain relief** without requiring the 57b foundation.
2. **Composes** with 57b/57d when those land — every audited site is annotated with whether it requires preempt-disable wrapping (for 57d safety) or stays as block+wake (preempt-safe by construction).
3. Makes 57e (full kernel preemption) safe by default — without this audit, dropping the `from_user` check in 57d/e risks lockups every time a kernel busy-spin holds the condition the spinner waits on.

The phase is large but mechanical: each callsite is reviewed in isolation, classified as one of {block+wake, hardware-bounded, deferred}, and converted or annotated accordingly.

## Learning Goals

- How to triage a kernel busy-spin: is the holder hardware (HW-bounded), software running on this core (cooperative), or software running on another core (cross-core synchronisation)?  Each answer suggests a different fix.
- How to convert a polling syscall loop to a `block_current_until` primitive: producing a `&AtomicBool` condition and an `IsrWakeQueue` or notification source that asserts it.
- Why some busy-spins should stay (e.g., `wait_icr_idle` at the LAPIC, where the hardware-bounded latency is < 1 µs and a context-switch costs more) and how to document that decision so a future reader does not "fix" it.
- How to structure a per-callsite acceptance test: the test must exercise the path (an injected stimulus), measure the runtime budget (the spin must complete within X µs / yield within Y ms), and assert no regression.
- Why the audit produces three artefacts: the catalogue (every site classified), the conversion list (which ones change), and the documented-bound list (which ones stay) — all checked into the repo as durable references.

## Feature Scope

### Audit catalogue (artefact)

A single document `docs/handoffs/57c-busy-wait-audit.md` listing every `core::hint::spin_loop` invocation in `kernel/src/`, plus every `while !X.load(...)` and `while X.try_lock().is_none()` pattern, classified by:

- **File / line / symbol** of the spin.
- **Holder** — what code, on what core, completes the condition the spinner waits on.
- **Bound** — hardware (e.g., LAPIC ICR delivery latency), bounded by an existing IRQ delivery, or unbounded (depends on a software task that may itself be preempted/blocked).
- **Decision** — convert to block+wake, wrap with preempt-disable + bound annotation, or document as bounded and leave alone.

The catalogue is the source of truth for Track B's conversions.  Every convert decision becomes a Track B task; every preempt-disable decision becomes a Track C task; every leave-alone decision becomes a Track D documentation task.

### Block+wake conversions (Track B)

The convert set known a priori, subject to expansion by the audit:

- **`virtio_blk` request poll** (already converted ad hoc — re-validated under the audit).
- **`sys_poll` no-waiter yield-loop** (already converted ad hoc — re-validated under the audit).
- **`net_task` NIC IRQ wake-up** — currently uses `block_current_unless_woken`; verify the 57a migration is complete and the wake source is the e1000 RX IRQ.
- **`WaitQueue::sleep`** generic wait-queue primitive — verify it bottoms out in `block_current_until`.
- **NVMe completion polling** in the userspace driver — already off-kernel; verify it does not hot-loop in the device-host syscall on the kernel side.
- **`futex_wait` on a contended condition** — uses `block_current_until` already; verify.

The candidate set the audit will likely surface (subject to confirmation):

- A `WaitQueue::sleep` -style primitive somewhere in the file system layer that polls instead of waiting.
- A blocking-mutex `lock()` path that spins for too many iterations before falling back to block.

### Preempt-disable annotations (Track C)

For sites that stay as spins because the critical section is hardware- or context-bounded:

- **`wait_icr_idle()` (`kernel/src/smp/ipi.rs:46`)** — LAPIC ICR delivery is bounded by IPI latency.  Annotate with a comment citing Intel SDM Vol 3A §10.6.  Wrap in `preempt_disable` (a 57b primitive) when 57b lands so a 57d preemption point does not interrupt the spin.
- **`tlb_shootdown` ack wait (`kernel/src/smp/tlb.rs:102, 190`)** — bounded by IPI delivery + remote-CPU IRQ-handler runtime, which is bounded itself.  Annotate.
- **IOMMU command-queue waits (`kernel/src/iommu/intel.rs:247, 368, 390`, `iommu/amd.rs:339`)** — hardware-bounded by IOMMU register update latency.  Annotate.
- **PS/2 controller wait (`kernel/src/arch/x86_64/ps2.rs:207, 220`)** — bounded by PS/2 controller response time, microseconds.  Annotate.
- **APIC reset wait (`kernel/src/arch/x86_64/apic.rs:436`)** — bounded.  Annotate.
- **AP boot wait (`kernel/src/smp/boot.rs:277`)** — bounded by AP startup IPI sequence.  Annotate.
- **RTC update-in-progress wait (`kernel/src/rtc.rs:90`)** — bounded by RTC chip's update window (~244 µs).  Annotate.
- **Frame-allocator allocation retry (`kernel/src/mm/frame_allocator.rs:876`)** — bounded by per-CPU magazine refill time.  Annotate.
- **Slab allocator spins (`kernel/src/mm/slab.rs:442, 604`)** — bounded.  Annotate.
- **Boot-time signal_reschedule wait (`kernel/src/main.rs:185`)** — debug-build only; bounded by 10M iterations × spin_loop hint, used only during init.  Annotate.

The 57c phase **does not** require 57b's `preempt_disable` to land before this annotation; the comment can be added now and the actual `preempt_disable` wrapper added when 57b lands.  Track C lands the comments; the wrappers are added in a 57b/57c integration commit.

### Documented-bound annotations (Track D)

Every site Track C touches gets a one-line code comment stating the bound (e.g., `// HW-bounded: ~1 µs (Intel SDM Vol 3A §10.6)`).  No behavioural change.

### Test infrastructure (Track E)

Each Track B conversion ships a regression test:

- A unit test (in `kernel-core` where possible, or an in-QEMU test where kernel state is required) that exercises the converted path and verifies block+wake semantics.
- An integration test (in-QEMU) that drives a stimulus through the syscall and measures the runtime budget — e.g., `sys_poll(2000)` returns after ~2 s wall clock with the converted task showing zero runtime accumulation.
- A `cargo xtask test` test that runs the regression set in CI.

## Engineering Practice Requirements

- **Test-Driven Development.**  Each Track B conversion lands as test-first commit followed by impl commit.  Track A's audit catalogue is the test contract: every convert site has a regression test in `kernel-core` (model) or `kernel/tests/` (in-QEMU integration) that asserts the converted behaviour.
- **SOLID.**
  - *Single Responsibility.*  Each conversion replaces a poll-loop with a `block_current_until` callsite; no other concern is bundled.  No "fix this and refactor that" combos.
  - *Open/Closed.*  New wait conditions plug in via the existing `block_current_until` primitive; no scheduler change is required for any conversion.
  - *Liskov.*  Every converted callsite preserves the original syscall return contract (errno, return value, side effects) bit-for-bit.
  - *Interface Segregation.*  The block primitive's contract is `(condition: &AtomicBool, deadline: Option<u64>) -> BlockOutcome`; each callsite uses only what it needs.
  - *Dependency Inversion.*  Drivers depend on `block_current_until` and `Notification`; they do not directly access scheduler internals.
- **DRY.**  No copy-pasted variant of `block_current_until` for new wait shapes.  Wakers are factored into per-source helpers (e.g., `kernel/src/blk/virtio_blk::wake_request_completion`).
- **Documented invariants.**  Every Track C annotation includes the bound + citation in code.  Every Track B conversion includes a doc comment on the new wait condition (what asserts it, who clears it, the expected wake latency).
- **Lock ordering.**  `block_current_until` interacts with `pi_lock` (outer) and `SCHEDULER.lock` (inner); the 57a hierarchy is preserved at every conversion site.  Reviewers reject conversions that take `pi_lock` while holding `SCHEDULER.lock`.
- **Migration safety.**  Each conversion is independently revertable.  Conversions land one per PR (or grouped into a single PR per subsystem).  No phase-wide "big bang" migration.
- **Observability.**  The `[WARN] [sched] cpu-hog` message introduced in 57a is the canonical regression signal — any task whose corrected `ran` exceeds 200 ms during the soak test fails the validation gate.

## Important Components and How They Work

### Audit catalogue (`docs/handoffs/57c-busy-wait-audit.md`)

A markdown table:

| File | Line | Symbol | Spin pattern | Holder | Bound | Decision |
|---|---|---|---|---|---|---|
| `kernel/src/blk/virtio_blk.rs` | (line) | `do_request` | `while !req.complete.load()` | this-core IRQ handler on completion | unbounded (waits for IRQ which may be deferred) | convert (already done) |
| `kernel/src/smp/ipi.rs` | 46 | `wait_icr_idle` | `while ICR & DELIVERED` | LAPIC hardware | HW-bounded ~1 µs | annotate |
| ... | ... | ... | ... | ... | ... | ... |

Track A.1 produces the table.  The audit is the input to Tracks B/C/D.

### Block+wake conversion pattern

For each Track B site, the conversion follows a uniform pattern:

```rust
// Before: busy-poll
while !condition.load(Ordering::Acquire) {
    core::hint::spin_loop();
}

// After: block + wake (if condition is asserted by an IRQ or another task)
let woken = AtomicBool::new(false);
register_waker(&woken, source);  // source = IRQ wake queue, futex, etc.
loop {
    if condition.load(Ordering::Acquire) { break; }
    let outcome = block_current_until(&woken, None);
    if matches!(outcome, BlockOutcome::ConditionTrue | BlockOutcome::Woken) { continue; }
}
```

The `register_waker` step is per-source; the rest is template.  Each Track B task is a one-PR change with the converted callsite + the regression test.

### Preempt-disable annotation pattern

For each Track C site, the annotation is:

```rust
// HW-bounded: ~1 µs (Intel SDM Vol 3A §10.6, 'Local APIC ICR Delivery').
// preempt_disable() wrapper added in Phase 57b/57c integration.
while ICR.read() & ICR_DELIVERED != 0 {
    core::hint::spin_loop();
}
```

When 57b lands, the wrapper becomes:

```rust
preempt_disable();
while ICR.read() & ICR_DELIVERED != 0 {
    core::hint::spin_loop();
}
preempt_enable();
```

The 57c phase only adds the comment; the wrapper is a 57b integration concern.

### Per-callsite regression test

Each Track B conversion ships a regression test.  Examples:

- `virtio_blk::tests::do_request_blocks_until_completion` — spawn a task that issues a virtio-blk request, fire the completion IRQ from a stub, assert the task's `ran_ticks` is < 5 ms across the request.
- `sys_poll::tests::poll_no_waiter_blocks_until_timeout` — call `sys_poll(fd, 100)` on a quiescent fd, assert the task's `ran_ticks` is < 5 ms across the 100 ms.
- `WaitQueue::tests::sleep_blocks_until_wake` — sleep on a wait queue, fire a wake from another task, assert the sleeping task's `ran_ticks` is < 5 ms across the wait.

The pattern: drive a stimulus, wait for the syscall to return, verify `ran_ticks` is small (i.e., the task was actually parked, not spinning).

## How This Builds on Earlier Phases

- **Reuses Phase 57a's `block_current_until` primitive** as the canonical block primitive.  Every Track B conversion calls into it.
- **Reuses Phase 6 / 50 `Notification` objects** as the canonical wake source for IRQ-driven wakes.
- **Reuses Phase 43c (Regression and Stress)** infrastructure for the per-callsite regression tests and the soak test.
- **Independent of Phase 57b** — the audit annotations land before 57b's `preempt_disable` exists.  The 57b/57c integration commit adds the wrappers in lockstep with the 57b foundation.
- **Closes the residual Phase 57a graphical-stack regression** (the cursor-stuck-at-(0,0) symptom catalogued in the validation gate handoff) without requiring 57d preemption to land.

## Implementation Outline

1. **Track A — Audit.**  Catalogue every `core::hint::spin_loop` and `while !.*\.load` pattern in `kernel/src/`.  Classify each: convert / annotate / leave.  Produce `docs/handoffs/57c-busy-wait-audit.md`.
2. **Track B — Conversions.**  For each "convert" entry: write the regression test (red), implement the block+wake (green), validate.  One PR per callsite or per subsystem.
3. **Track C — Annotations.**  For each "annotate" entry: add the comment with bound + citation.  Single PR per subsystem.
4. **Track D — Documentation.**  Update `docs/04-tasking.md` and `docs/06-ipc.md` with a section on the new wait-queue helpers and the audit-derived conversion guidelines.
5. **Track E — Validation gate.**  Run the I.1 acceptance test (cursor regression) on user hardware.  Run the soak test for 30 minutes.  Confirm `[WARN] [sched] cpu-hog` lines do not appear.

## Acceptance Criteria

### Primary (audit + conversion)

- `docs/handoffs/57c-busy-wait-audit.md` exists, classifying every `core::hint::spin_loop` and `while !.*\.load` pattern in `kernel/src/`.
- Every "convert" entry has a corresponding Track B PR that lands the conversion + regression test.
- Every "annotate" entry has a comment in the source citing the bound + reference.
- `git grep -E 'core::hint::spin_loop|while !.*\.load\(' kernel/src/` returns matches that are all either: (a) inside a `block_current_until`-driven path, or (b) annotated with a bound comment.
- `cargo xtask test` regression suite passes.
- `cargo xtask check` clean.

### Secondary (user-pain relief)

- `cargo xtask run-gui --fresh` on the user's test hardware: cursor moves on mouse motion within 1 s of motion start; keyboard input typed in the framebuffer terminal appears within 100 ms; `term` reaches `TERM_SMOKE:ready`. (Resolves the I.1 acceptance gate.)
- 30 minutes idle plus 30 minutes synthetic IPC + futex + notification load on 4 cores: no `[WARN] [sched]` cpu-hog warnings whose corrected `ran` exceeds 200 ms.
- 50 consecutive SSH disconnect/reconnect cycles in one session without a scheduler hang. (Defence-in-depth: also resolves the 57a I.2 gate.)

### Engineering practice

- Every Track B conversion has a test commit landed before the implementation commit.  PR commit history shows test-first ordering.
- Every Track C annotation has a citation + bound stated in the comment.
- `docs/handoffs/57c-busy-wait-audit.md` is the durable artefact — future reviewers can find the audit decision for any kernel spin in one lookup.

## Companion Task List

- [Phase 57c Task List](./tasks/57c-kernel-busy-wait-conversion-tasks.md)

## How Real OS Implementations Differ

- **Linux runs `lockdep` continuously** to catch lock-order, sleep-while-atomic, and bound violations.  m3OS uses static review + targeted regression tests; lockdep equivalence is deferred.
- **Linux's `might_sleep()` macro** instruments every callsite that could sleep so a `sleep_in_atomic` bug fires at the moment it occurs.  m3OS uses the `[WARN] [sched] cpu-hog` watchdog as a coarse equivalent.
- **Linux's `cpu_relax()`** is the equivalent of `core::hint::spin_loop` but with arch-specific tuning for SMT / hyperthreading.  m3OS uses `core::hint::spin_loop` directly; the optimisation is not yet relevant.
- **seL4** does not have any kernel busy-spins by construction — the kernel is single-threaded and run-to-completion.  m3OS has SMP and so cross-core synchronisation occasionally requires bounded spins; the audit makes those spins explicit.

## Deferred Until Later

- **Lockdep equivalent** for runtime lock-ordering and sleep-in-atomic checking — a separate kernel-infrastructure phase.
- **`might_sleep()`-style instrumentation** on every callsite that could yield — same.
- **Loom-style formal interleaving search** of converted block+wake pairs — a stretch goal in 57e or a later phase.
- **Per-CPU load balancing** of converted-syscall-task placement — orthogonal to this phase.
