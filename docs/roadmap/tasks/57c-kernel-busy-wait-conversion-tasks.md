# Phase 57c — Kernel Busy-Wait Audit and Conversion: Task List

**Status:** Planned
**Source Ref:** phase-57c
**Depends on:** Phase 4 ✅, Phase 6 ✅, Phase 35 ✅, Phase 50 ✅, Phase 57a ✅
**Goal:** Catalogue every kernel busy-wait, convert hot/unbounded sites to block+wake pairs, annotate hardware-bounded sites with documented bounds and citations.  Eliminate the cooperative-scheduling-starvation user pain catalogued in `docs/handoffs/57a-validation-gate.md`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit catalogue (every kernel busy-spin classified) | — | Planned |
| B | Block+wake conversions (per-callsite, with regression tests) | A | Planned |
| C | Documented-bound annotations (per-callsite comments) | A | Planned |
| D | Wait-queue helper documentation update | A, B | Planned |
| E | Validation gate (user hardware + soak) | A, B, C | Planned |

Tracks A is the foundation.  Tracks B and C run in parallel after A lands.  Track D is a documentation closeout.  Track E is the final gate.

## Engineering Practice Gates (apply to every track)

- **TDD.**  Every Track B conversion has a regression test landed before the implementation.  PR commit history shows test-first ordering.
- **SOLID.**  Each conversion is single-purpose: one busy-spin replaced with one block+wake pair.  No bundled refactors.
- **DRY.**  No copy-pasted variant of `block_current_until` for new wait shapes.  Wakers are factored into per-source helpers.
- **Documented invariants.**  Every Track B conversion includes a doc comment on the new wait condition (what asserts it, who clears it, expected wake latency).  Every Track C annotation includes the bound + citation.
- **Lock ordering.**  Conversions preserve the 57a `pi_lock` (outer) → `SCHEDULER.lock` (inner) hierarchy.
- **Migration safety.**  Each conversion is independently revertable.  One PR per callsite or per subsystem.
- **Observability.**  The 57a `[WARN] [sched] cpu-hog` watchdog is the canonical regression signal — any task whose corrected `ran` exceeds 200 ms during the soak fails the validation gate.

---

## Track A — Audit Catalogue

### A.1 — Catalogue every kernel busy-spin

**Files:**
- `kernel/src/`

**Symbol:** every `core::hint::spin_loop()` and `while !.*\.load(...)` and `while .*\.try_lock\(\).is_none\(\)` pattern
**Why it matters:** Without a complete inventory, a partial conversion leaves a mix of converted and bounded-but-unannotated sites.  The catalogue is the source of truth for Tracks B and C.

**Acceptance:**
- [ ] Markdown table at `docs/handoffs/57c-busy-wait-audit.md` with rows = callsite, columns = (file:line, symbol, spin pattern, holder, bound, decision).
- [ ] Decision is one of: `convert` (Track B), `annotate` (Track C), `leave` (already documented or already converted in 57a).
- [ ] Every row maps to a Track B or Track C task.
- [ ] `git grep -E 'core::hint::spin_loop|while !.*\.load\(' kernel/src/` returns matches that are all in the catalogue.
- [ ] Catalogue includes the known sites enumerated in the design doc: `kernel/src/smp/ipi.rs:46`, `tlb.rs:102`, `tlb.rs:190`, `iommu/intel.rs:247`, `iommu/intel.rs:368`, `iommu/intel.rs:390`, `iommu/amd.rs:339`, `arch/x86_64/ps2.rs:207`, `arch/x86_64/ps2.rs:220`, `arch/x86_64/apic.rs:436`, `smp/boot.rs:277`, `rtc.rs:90`, `mm/frame_allocator.rs:876`, `mm/slab.rs:442`, `mm/slab.rs:604`, `main.rs:185`, `task/scheduler.rs:2111` (the on_cpu wait-spin from 57a — must stay), `task/scheduler.rs:2322` (the test-only entry), `arch/x86_64/syscall/mod.rs:3283` (the < 1 ms nanosleep TSC busy-wait — must stay).

### A.2 — Classification doc

**File:** `docs/handoffs/57c-busy-wait-audit.md` (extended)
**Symbol:** —
**Why it matters:** The decision rationale must be durable so a future reader understands why a site stayed.

**Acceptance:**
- [ ] Each row includes a "rationale" field with one sentence per decision.
- [ ] `convert` rows reference the planned wake source (IRQ, futex, notification, etc.).
- [ ] `annotate` rows reference the bound (HW latency, IPI delivery, RTC update window, etc.) and the citation (Intel SDM section, hardware datasheet, etc.).
- [ ] `leave` rows reference the prior 57a fix or the existing block+wake structure.

---

## Track B — Block+Wake Conversions

Each task is a single PR.  The conversion follows the template in the design doc: red test → green implementation → regression in CI.

### B.1 — `WaitQueue::sleep` review and bounded-wait verification

**File:** `kernel/src/task/wait_queue.rs`
**Symbol:** `WaitQueue::sleep`
**Why it matters:** The 57a migration moved this to `block_current_until`; verify under the new audit that the path is correct and no subsidiary spin remains.

**Acceptance:**
- [ ] Code review confirms `sleep` calls `block_current_until` with no subsidiary spin.
- [ ] Regression test: a task sleeps on a wait queue, another task wakes it, the sleeping task accumulates < 5 ms `ran_ticks` across the wait.

### B.2 — `net_task` NIC IRQ wake-up

**File:** `kernel/src/main.rs`
**Symbol:** `net_task`
**Why it matters:** The networking RX path must not spin when the NIC has no traffic; otherwise it monopolises its core.

**Acceptance:**
- [ ] `net_task` blocks via `block_current_until(&NIC_WOKEN, None)` on the NIC IRQ wake source (verify 57a Track F.6 migration is intact).
- [ ] Regression test: NIC IRQ wakes the task within 1 ms of asserting; the task accumulates < 5 ms `ran_ticks` across an idle window.

### B.3 — Audit-surfaced conversion target #1

**File:** TBD by Track A.1 audit
**Symbol:** TBD
**Why it matters:** The audit may surface a previously-unknown busy-spin in `kernel/src/fs/`, `kernel/src/blk/`, or elsewhere.  Each surfaced site gets a B.x task slot.

**Acceptance:**
- [ ] Conversion replaces the spin with `block_current_until` + a wake source.
- [ ] Regression test asserts `ran_ticks` < 5 ms across the wait window.
- [ ] No regression in the existing test suite.

### B.4–B.8 — Audit-surfaced conversion targets #2–#6

**Files:** TBD by Track A.1 audit
**Symbol:** TBD
**Why it matters:** Slots reserved for the audit-surfaced sites.  Each is a single PR with the same structure as B.3.

**Acceptance:** As B.3, per site.

---

## Track C — Documented-Bound Annotations

Each task adds a doc comment to a hardware-bounded busy-spin.  No behavioural change.  One PR per subsystem.

### C.1 — `kernel/src/smp/`

**Files:**
- `kernel/src/smp/ipi.rs:46` — `wait_icr_idle`
- `kernel/src/smp/tlb.rs:102, 190` — TLB shootdown ack-wait
- `kernel/src/smp/boot.rs:277` — AP boot wait

**Symbol:** each spin's enclosing function
**Why it matters:** SMP busy-spins are bounded by IPI / hardware latency.  The bound must be documented so a future reviewer does not "fix" the spin.

**Acceptance:**
- [ ] Each site has a comment of the form: `// HW-bounded: ~1 µs (Intel SDM Vol 3A §10.6, 'Local APIC ICR Delivery'). preempt_disable wrapper added in 57b/57c integration commit.`
- [ ] Citations are accurate and verifiable.

### C.2 — `kernel/src/iommu/`

**Files:**
- `kernel/src/iommu/intel.rs:247, 368, 390` — VT-d command queue waits
- `kernel/src/iommu/amd.rs:339` — AMD-Vi command queue wait

**Symbol:** each spin's enclosing function
**Why it matters:** IOMMU command-queue waits are bounded by hardware register update latency.  Document.

**Acceptance:**
- [ ] Each site has a comment citing the IOMMU spec section and stating the bound.
- [ ] preempt_disable wrapper note included.

### C.3 — `kernel/src/arch/x86_64/`

**Files:**
- `kernel/src/arch/x86_64/ps2.rs:207, 220` — PS/2 controller poll
- `kernel/src/arch/x86_64/apic.rs:436` — LAPIC reset poll

**Symbol:** each spin's enclosing function
**Why it matters:** Arch-level hardware polls are bounded by chip response time.  Document.

**Acceptance:**
- [ ] Each site has a comment citing the chip datasheet section and stating the bound.

### C.4 — `kernel/src/mm/`

**Files:**
- `kernel/src/mm/frame_allocator.rs:876` — allocation retry
- `kernel/src/mm/slab.rs:442, 604` — slab refill spins

**Symbol:** each spin's enclosing function
**Why it matters:** Allocator-internal spins are bounded by per-CPU magazine refill time.  Document.

**Acceptance:**
- [ ] Each site has a comment stating the bound (sub-microsecond).

### C.5 — `kernel/src/rtc.rs`

**File:** `kernel/src/rtc.rs:90`
**Symbol:** `wait_for_update_complete`
**Why it matters:** The RTC update-in-progress wait is bounded by the RTC chip's update window (~244 µs).  Document.

**Acceptance:**
- [ ] Comment cites the RTC chip datasheet (MC146818) and states the 244 µs bound.

### C.6 — `kernel/src/main.rs:185`

**File:** `kernel/src/main.rs`
**Symbol:** the boot-time timer-IRQ-detection spin (debug builds only)
**Why it matters:** This is a debug-only init-time spin; the bound is 10M iterations × spin_loop hint, used once.  Document.

**Acceptance:**
- [ ] Comment states the spin is debug-only, init-only, bounded by iteration count.

### C.7 — `kernel/src/task/scheduler.rs`

**Files:**
- `kernel/src/task/scheduler.rs:2111` — 57a `on_cpu` wait-spin
- `kernel/src/task/scheduler.rs:2322` — `#[cfg(test)] test_task_entry` (test-only)
- `kernel/src/arch/x86_64/syscall/mod.rs:3283` — `< 1 ms` nanosleep TSC busy-spin

**Symbol:** each
**Why it matters:** These are existing documented spins from prior phases.  Verify the 57a doc comment exists; if not, add it.

**Acceptance:**
- [ ] Each site has a comment citing the prior phase that introduced it (57a, < 1 ms nanosleep optimisation, etc.) and stating the bound.

---

## Track D — Wait-Queue Helper Documentation Update

### D.1 — Update `docs/04-tasking.md`

**File:** `docs/04-tasking.md`
**Symbol:** —
**Why it matters:** The narrative tasking documentation must describe the audit-derived block+wake patterns so future learners use them by default.

**Acceptance:**
- [ ] New subsection titled "Audit-derived block+wake patterns (Phase 57c)".
- [ ] Describes the conversion template: identify the holder, find or build a wake source, replace the spin with `block_current_until`.
- [ ] References the audit catalogue at `docs/handoffs/57c-busy-wait-audit.md`.

### D.2 — Update `docs/06-ipc.md`

**File:** `docs/06-ipc.md`
**Symbol:** —
**Why it matters:** Many block+wake conversions use `Notification` objects as the wake source; the IPC documentation must describe this canonical use.

**Acceptance:**
- [ ] New subsection covering Notification-as-wake-source pattern.

---

## Track E — Validation Gate

### E.1 — Real-hardware graphical-stack regression

**File:** procedural; results in PR description and `docs/handoffs/57c-validation-gate.md`
**Symbol:** —
**Why it matters:** The Phase 57a I.1 acceptance gate fails because of cooperative-scheduling starvation.  57c's primary acceptance test is that I.1 now passes.

**Acceptance:**
- [ ] On user test hardware, `cargo xtask run-gui --fresh`: cursor moves on mouse motion within 1 s; keyboard echoes within 100 ms; `term` reaches `TERM_SMOKE:ready`.
- [ ] Repeated 5 times, 5 successes (placement varies between boots).
- [ ] Zero `[WARN] [sched]` lines in the first 60 s of each boot.

### E.2 — 30 + 30 min soak

**File:** procedural; results in PR description
**Symbol:** —
**Why it matters:** A 60-minute soak with idle and load shows the conversions are stable under realistic conditions.

**Acceptance:**
- [ ] 30 min idle + 30 min synthetic IPC + futex + notification load on 4 cores.
- [ ] Zero `[WARN] [sched] cpu-hog` warnings whose corrected `ran` exceeds 200 ms.
- [ ] Zero `[WARN] [sched]` stuck-task warnings.
- [ ] No deadlocks, panics, or scheduler hangs.

### E.3 — SSH disconnect/reconnect soak

**File:** procedural
**Symbol:** —
**Why it matters:** Defence-in-depth — the 57a I.2 gate should also pass under 57c's improvements.

**Acceptance:**
- [ ] 50 consecutive SSH disconnect/reconnect cycles in one session without a scheduler hang.
- [ ] Zero `[WARN] [sched]` lines during the soak.

### E.4 — Documentation update

**Files:**
- `docs/roadmap/README.md` (Phase 57c row)
- `kernel/Cargo.toml` (version bump)
- `kernel/src/main.rs` (banner)

**Symbol:** —
**Why it matters:** The phase landing must be visible.

**Acceptance:**
- [ ] Phase 57c row added to the milestone summary table with status `Complete`.
- [ ] Kernel version bumped.
- [ ] Banner reflects the new version.

---

## Documentation Notes

- This phase **delivers user-pain relief without depending on the 57b foundation**.  The Track C `preempt_disable` wrappers are added in a 57b/57c integration commit *after* 57b lands; 57c itself only adds the bound comments.
- The audit catalogue at `docs/handoffs/57c-busy-wait-audit.md` is the durable artefact — future reviewers can find the audit decision for any kernel spin in one lookup.
- Track B's regression test pattern (`ran_ticks < 5 ms across the wait window`) is a reusable template for any future block+wake conversion.
- The Phase 57c row in `docs/roadmap/README.md`'s milestone summary should note that 57c can land in parallel with or before 57b.
