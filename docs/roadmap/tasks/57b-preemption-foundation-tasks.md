# Phase 57b — Preemption Foundation: Task List

**Status:** Planned
**Source Ref:** phase-57b
**Depends on:** Phase 4 ✅, Phase 25 ✅, Phase 35 ✅, Phase 57a ✅
**Goal:** Land per-task `preempt_count` discipline, the `PreemptFrame` save area, and spinlock-raises-`preempt_count` wiring as a no-op refactor.  Establish the contract every later subphase relies on.  No behaviour change; the kernel becomes preemption-CAPABLE but pre-emption is never actually fired.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit + TDD foundation (kernel-core model, callsite catalogue) | — | Planned |
| B | `Task::preempt_count` field + `preempt_disable` / `preempt_enable` helpers + user-mode-return assertion | A | Planned |
| C | `PreemptFrame` save-area struct + offset-of constants (unused in 57b) | A | Planned |
| D | `IrqSafeMutex` migration to raise `preempt_count` | B | Planned |
| E | Per-callsite migration of non-`IrqSafeMutex` lock sites | A, D | Planned |
| F | Documentation, invariants, version bump, validation | A–E | Planned |

Tracks A and B are the foundation — they must complete before D/E.  Track C is independent of B and can run in parallel.  Track F is the closeout gate.

## Engineering Practice Gates (apply to every track)

- **TDD.**  Every implementation commit must reference a test commit that landed *earlier* in the same PR (or in a prior PR).  Tests added in the same commit as implementation are rejected on review.
- **SOLID.**  No new flag fields on `Task` beyond `preempt_count` and `preempt_frame`.  `preempt_count` is only mutated through `preempt_disable` / `preempt_enable`; no callsite touches the field directly.
- **DRY.**  Single `preempt_disable` / `preempt_enable` pair; single `PreemptFrame` layout.  No per-callsite variant.
- **Documented invariants.**  `preempt_count` returns to 0 at every user-mode return; maximum nesting depth = 32; per-task placement (not per-CPU).
- **Lock ordering.**  `preempt_count` is task-local and does not participate in the lock hierarchy.  Acquiring or releasing it does not invalidate any other lock held.
- **Migration safety.**  No-op refactor.  Worst case: forgotten `preempt_enable` panics on first user-mode return — caught immediately.  No feature gate required.
- **Observability.**  `[INFO] [preempt]` log on first non-zero count observed at user-mode return; debug-build assertion panics.

---

## Track A — Audit and TDD Foundation

### A.1 — Catalogue every kernel spinlock callsite

**Files:**
- `kernel/src/`
- `kernel-core/src/`

**Symbol:** every `spin::Mutex::lock()`, `spin::RwLock::read()/write()`, `IrqSafeMutex::lock()`, `Mutex<T>::lock()` callsite
**Why it matters:** The 57b refactor must touch every lock callsite.  Without a complete inventory, a missed callsite leaves a path that does not raise `preempt_count`, and 57d's preemption can fire while that lock is held — deadlocking the core.

**Acceptance:**
- [ ] Markdown table at `docs/handoffs/57b-spinlock-callsite-audit.md` listing every callsite (file:line, symbol, lock kind, current wrapping pattern).
- [ ] Each row classified: "already `IrqSafeMutex` (inherits Track D)", "convert to `IrqSafeMutex`", or "explicit `preempt_disable`/`preempt_enable` wrapper".
- [ ] Every row maps to a Track E task (one PR per subsystem).
- [ ] `git grep -E 'spin::Mutex|spin::RwLock|IrqSafeMutex' kernel/ kernel-core/` is the source-of-truth for completeness; the audit covers every match.

### A.2 — Pure-logic counter model in `kernel-core`

**File:** `kernel-core/src/preempt_model.rs` (new)
**Symbol:** `Counter`, `disable`, `enable`, `count`, `assert_balanced`
**Why it matters:** The counter contract must be testable on the host before any kernel-side implementation lands.  TDD red phase.

**Acceptance:**
- [ ] `Counter` type wraps an `i32`.
- [ ] `disable(&mut self)` increments; `enable(&mut self)` decrements; `count(&self) -> i32` returns the current value.
- [ ] `assert_balanced(&self)` panics if `count() != 0`.
- [ ] Compiles on host (`cargo test -p kernel-core`) and in `no_std` kernel context.
- [ ] One doc comment per method explaining the invariant and ordering.

### A.3 — Property tests for the counter model

**File:** `kernel-core/tests/preempt_property.rs` (new)
**Symbol:** —
**Why it matters:** Hand-written tests cover the cases the author thought of; property fuzz catches the rest.  The counter must return to 0 across any random sequence of paired operations and must remain non-negative.

**Acceptance:**
- [ ] Property test runs ≥ 10 000 random sequences of paired `disable`/`enable` operations of nesting depth 1–32.
- [ ] Asserts: `count() == 0` after every balanced sequence.
- [ ] Asserts: `count() > 0` while any unmatched `disable` is pending.
- [ ] Asserts: `count()` never goes negative.
- [ ] Hooked into `cargo xtask check` so CI runs it on every build.

### A.4 — `PreemptFrame` layout test

**File:** `kernel-core/src/preempt_frame.rs` (new) plus tests
**Symbol:** `PreemptFrame`, `PREEMPT_FRAME_OFFSET_*` constants
**Why it matters:** The 57d assembly will use literal offsets into `PreemptFrame`; if the Rust layout drifts from the asm offsets, registers will be saved into wrong slots and the resume will jump to garbage.  A compile-time test pinning the layout catches this immediately.

**Acceptance:**
- [ ] `PreemptFrame` is `#[repr(C)]` with explicit field order.
- [ ] `PREEMPT_FRAME_OFFSET_RAX`, `..._RIP`, `..._RFLAGS`, `..._RSP`, `..._CS`, `..._SS` constants exposed via `core::mem::offset_of!`.
- [ ] Compile-time test: `const _: () = assert!(PREEMPT_FRAME_OFFSET_RAX == 0);` (and similar for every offset) — fails the build on layout drift.
- [ ] Doc comment explains the `iretq` frame layout `PreemptFrame` mirrors.

---

## Track B — `Task::preempt_count` and Helpers

### B.1 — Add `preempt_count` to `Task`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::preempt_count`
**Why it matters:** The counter is the gate every 57d preemption check consults.  Adding it now (with no-op semantics) lets every later subphase be additive.

**Acceptance:**
- [ ] `Task::preempt_count: AtomicI32` field, initialised to `0` at task construction.
- [ ] Doc comment on the field: "Per-task preempt-disable counter.  Incremented by `preempt_disable()`, decremented by `preempt_enable()`.  Must be 0 at every user-mode return.  Phase 57d/57e gate preemption on this == 0."
- [ ] Existing `cargo xtask test` passes (no semantic change yet — the counter is initialised but never read).

### B.2 — Implement `preempt_disable()` / `preempt_enable()`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_disable`, `preempt_enable`
**Why it matters:** These are the canonical entry points for every spinlock callsite.  Implementing them as free functions keeps the callsite call-graph clean; centralising the lookup of `current_task` keeps the policy in one place.

**Acceptance:**
- [ ] `preempt_disable()` performs `current_task().preempt_count.fetch_add(1, Acquire)` (with appropriate guards for the `current_task` lookup).
- [ ] `preempt_enable()` performs `fetch_sub(1, Release)`; in 57b the post-decrement value is not inspected.
- [ ] A debug assertion panics if the counter exceeds 32 (catches "preempt_disable in a loop" bugs).
- [ ] Both functions handle the case where `current_task_idx()` returns `None` (during early boot before the scheduler is ready).
- [ ] Unit test in `kernel-core` (model) and integration test in `kernel/tests/` exercising paired and nested operations.

### B.3 — User-mode-return debug assertion

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (syscall return path)
- `kernel/src/arch/x86_64/interrupts.rs` (IRQ return path for IRQs that interrupted user mode)

**Symbol:** the syscall-return and `iretq`-to-user-mode boundaries
**Why it matters:** The earliest possible detection of a forgotten `preempt_enable`.  Without this assertion, a missed wrapper might not surface until 57d when preemption fires inside the held lock — by which time the kernel has deadlocked.

**Acceptance:**
- [ ] At the syscall-return path (just before the `sysretq` or equivalent), `debug_assert!(current_task().preempt_count.load(Relaxed) == 0)`.
- [ ] At every IRQ-return-to-ring-3 path (timer, keyboard, mouse, NIC, etc.), the same assertion.
- [ ] In release builds the assertion is compiled out (no overhead).
- [ ] Existing `cargo xtask test` passes (no spinlock callsite forgets to release in current code; the assertion never trips).

### B.4 — `[INFO] [preempt]` first-non-zero log

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** observability hook in the user-mode return path
**Why it matters:** In release builds the debug assertion compiles out.  A logged warning on first non-zero count observed at user-mode return surfaces the bug in production — slower than the debug panic but still actionable.

**Acceptance:**
- [ ] Per-task `preempt_logged_nonzero: AtomicBool` (added to `Task`); log fires once per task per session.
- [ ] Log line format: `[WARN] [preempt] pid=X count=Y at user-mode return`.
- [ ] Test: a synthetic test path that forgets a `preempt_enable` triggers the log; the next correct path does not.

---

## Track C — `PreemptFrame` Save-Area

### C.1 — `PreemptFrame` struct on `Task`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::preempt_frame`, `PreemptFrame`
**Why it matters:** 57d's `preempt_to_scheduler` writes into this field.  Adding it now (zero-initialised, untouched) lets 57d be a pure additive change with no `Task` layout churn.

**Acceptance:**
- [ ] `Task::preempt_frame: PreemptFrame` field, zero-initialised.
- [ ] `PreemptFrame` is the `#[repr(C)]` struct from A.4.
- [ ] Doc comment: "Phase 57b infrastructure.  Written by 57d's `preempt_to_scheduler`; read by 57d's `preempt_resume_to_user`.  Unused in 57b."
- [ ] Existing `cargo xtask test` passes.
- [ ] `kernel/src/task/mod.rs` exposes `PREEMPT_FRAME_OFFSET_*` constants via `core::mem::offset_of!(Task, preempt_frame) + PREEMPT_FRAME_OFFSET_<reg>` for the future 57d assembly.

### C.2 — `Task` layout regression test

**File:** `kernel/tests/task_layout.rs` (new) or `kernel-core/tests/preempt_layout.rs`
**Symbol:** `Task::preempt_frame` offset
**Why it matters:** A drift in `Task` field ordering (e.g., adding a field before `preempt_frame`) silently breaks the 57d assembly.  A compile-time check pins the offset.

**Acceptance:**
- [ ] Compile-time test asserting `core::mem::offset_of!(Task, preempt_frame)` equals the documented constant.
- [ ] Doc comment in the test explains why the offset is load-bearing for 57d.

---

## Track D — `IrqSafeMutex` Raises `preempt_count`

### D.1 — Wire `preempt_disable` into `IrqSafeMutex::lock`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `IrqSafeMutex::lock`, `IrqSafeGuard`
**Why it matters:** This is the single high-leverage point that gives every existing `IrqSafeMutex` callsite preempt-discipline for free — no per-callsite migration required.

**Acceptance:**
- [ ] `IrqSafeMutex::lock()` calls `preempt_disable()` *before* `interrupts::disable()`.
- [ ] `IrqSafeGuard::Drop` calls `preempt_enable()` *after* `interrupts::enable()`.
- [ ] Drop-order regression test: a synthetic test confirms the spin-unlock fires before interrupt-restore (the existing 57a invariant) and `preempt_enable` fires last.
- [ ] `try_lock` mirrors the same pattern (raise on success, no-op on `None` return).
- [ ] Existing `cargo xtask test` passes — the counter is incremented and decremented at every callsite without any semantic difference.

### D.2 — `SchedulerGuard` inherits the discipline

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `scheduler_lock`, `SchedulerGuard`
**Why it matters:** The 57a `SchedulerGuard` wraps `IrqSafeGuard`.  D.1 gives it preempt-discipline automatically.  Verify, document, and test.

**Acceptance:**
- [ ] No code change required (the wrapper inherits D.1's discipline).
- [ ] Doc comment on `scheduler_lock` updated to mention preempt-discipline.
- [ ] Regression test: scheduler lock acquire/release cycles `preempt_count` exactly once.

---

## Track E — Per-Callsite Migration

For each subsystem identified in A.1's audit, convert non-`IrqSafeMutex` locks to `IrqSafeMutex`, or wrap with explicit `preempt_disable` / `preempt_enable`.  Each task is a single PR.

### E.1 — `kernel/src/blk/`

**Files:**
- `kernel/src/blk/virtio_blk.rs`
- `kernel/src/blk/remote.rs`

**Symbol:** every `spin::Mutex` / `IrqSafeMutex` callsite in the block layer
**Why it matters:** Block-device locks are held across the request-submit / IRQ-completion path; missing preempt-discipline here would deadlock the storage stack on a 57d preemption.

**Acceptance:**
- [ ] Every callsite in `kernel/src/blk/` either uses `IrqSafeMutex` (preferred) or has an explicit `preempt_disable` / `preempt_enable` wrapper.
- [ ] Regression test asserts `preempt_count` returns to 0 across a virtio-blk request submit + IRQ wake.
- [ ] `cargo xtask test` passes.

### E.2 — `kernel/src/net/`

**Files:**
- `kernel/src/net/virtio_net.rs`
- `kernel/src/net/tcp.rs`, `udp.rs`, `arp.rs`, `unix.rs`, `remote.rs`

**Symbol:** every `spin::Mutex` / `IrqSafeMutex` callsite in the network stack
**Why it matters:** Network-stack locks are held across send/recv paths and IRQ wakes; same risk profile as the block layer.

**Acceptance:**
- [ ] Every callsite in `kernel/src/net/` migrated.
- [ ] Regression test asserts `preempt_count` returns to 0 across a TCP send/recv round-trip.
- [ ] `cargo xtask test` passes.

### E.3 — `kernel/src/fs/`

**Files:**
- `kernel/src/fs/tmpfs.rs`, `ext2.rs`, `fat32.rs`

**Symbol:** every lock callsite in the file-system layer
**Why it matters:** FS locks are held across read/write/getdents paths; preempt-discipline must be uniform.

**Acceptance:**
- [ ] Every FS callsite migrated.
- [ ] Regression test exercises a write/read/getdents triple and asserts `preempt_count` returns to 0.

### E.4 — `kernel/src/mm/`

**Files:**
- `kernel/src/mm/slab.rs`, `frame_allocator.rs`, `heap.rs`

**Symbol:** every allocator-internal lock callsite
**Why it matters:** Allocator locks are held during page-fault handling and slab-cache refills; a missed wrapper here is catastrophic under 57d.

**Acceptance:**
- [ ] Every callsite in `kernel/src/mm/` migrated.
- [ ] Regression test exercises a heap allocation path and asserts `preempt_count` returns to 0.

### E.5 — `kernel/src/iommu/`

**Files:**
- `kernel/src/iommu/intel.rs`, `amd.rs`, `registry.rs`

**Symbol:** every IOMMU command-queue lock callsite
**Why it matters:** IOMMU locks gate DMA mapping; preempt-discipline must be uniform.

**Acceptance:**
- [ ] Every IOMMU callsite migrated.
- [ ] Regression test exercises an IOMMU map/unmap and asserts `preempt_count` returns to 0.

### E.6 — `kernel/src/process/`, `kernel/src/ipc/`, `kernel/src/syscall/`

**Files:**
- `kernel/src/process/futex.rs`, `mod.rs`
- `kernel/src/ipc/notification.rs`
- `kernel/src/syscall/device_host.rs`

**Symbol:** every lock callsite in the process / IPC / syscall layer
**Why it matters:** These paths span the syscall fast-path; preempt-discipline must be uniform.

**Acceptance:**
- [ ] Every callsite migrated.
- [ ] Regression test exercises a futex wait/wake, a notification deliver, and a device-host syscall.

### E.7 — `kernel/src/{pipe,serial,tty,pty,stdin,signal,trace,testing,fb,rtc}.rs`

**Symbol:** every remaining lock callsite
**Why it matters:** Catch-all for the remaining single-file subsystems.

**Acceptance:**
- [ ] Every callsite migrated.
- [ ] `cargo xtask check` clean; `cargo xtask test` passes.

### E.8 — `kernel/src/smp/`, `kernel/src/arch/x86_64/`

**Files:**
- `kernel/src/smp/mod.rs`, `tlb.rs`, `ipi.rs`
- `kernel/src/arch/x86_64/ps2.rs`, `interrupts.rs`, `syscall/mod.rs`

**Symbol:** every lock callsite in the SMP / arch layer
**Why it matters:** SMP layer touches per-core data; arch layer touches IRQ paths.  Both must be preempt-disciplined under 57d.

**Acceptance:**
- [ ] Every callsite migrated.
- [ ] Regression test exercises an IPI delivery (TLB shootdown) and asserts `preempt_count` returns to 0.

### E.9 — `kernel-core/src/`

**Files:**
- `kernel-core/src/magazine.rs`
- `kernel-core/src/device_host/registry_logic.rs`

**Symbol:** every lock callsite in `kernel-core` (kernel-build only; host-build paths are not affected)
**Why it matters:** `kernel-core` types embedded in kernel `Task` / scheduler must be preempt-disciplined.

**Acceptance:**
- [ ] Every callsite migrated; host tests in `kernel-core` continue to pass on the host (where `preempt_disable` is a no-op stub).
- [ ] Kernel-side regression test asserts `preempt_count` returns to 0 across a magazine refill.

---

## Track F — Documentation, Invariants, Validation

### F.1 — Top-of-file doc block in `scheduler.rs`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** module doc
**Why it matters:** A future reader must be able to read the top of `scheduler.rs` and understand the `preempt_count` discipline.  Without this, the discipline is folklore.

**Acceptance:**
- [ ] New section in the doc block titled `## preempt_count`.
- [ ] Documents: per-task placement, raise on lock, drop on unlock, return-to-0 invariant, max nesting depth, 57d/57e dependency.
- [ ] References `docs/appendix/preemptive-multitasking.md` and `docs/roadmap/57b-preemption-foundation.md`.

### F.2 — Update `docs/04-tasking.md`

**File:** `docs/04-tasking.md`
**Symbol:** —
**Why it matters:** The narrative documentation must describe preempt-discipline so future learners understand the kernel's current state.

**Acceptance:**
- [ ] New subsection titled "Preempt-discipline (Phase 57b)".
- [ ] Describes `preempt_count`, `IrqSafeMutex` integration, and the user-mode-return invariant.
- [ ] References Phase 57b and the appendix.

### F.3 — Kernel version bump

**Files:**
- `kernel/Cargo.toml`
- `kernel/src/main.rs` (banner)

**Symbol:** version constant
**Why it matters:** The phase landing must be visible to anyone reading the boot banner.

**Acceptance:**
- [ ] `kernel/Cargo.toml` version bumped (e.g., `0.57.2`).
- [ ] Boot banner reflects the new version.

### F.4 — 30-minute soak gate

**File:** procedural; results documented in PR description
**Symbol:** —
**Why it matters:** The phase is a no-op refactor.  A soak gate confirms the refactor is genuinely no-op: no panics from the user-mode-return assertion, no `[WARN] [preempt]` lines, no scheduler regressions.

**Acceptance:**
- [ ] 30-minute soak with `cargo xtask run-gui --fresh` plus synthetic IPC + futex + notification load on 4 cores.
- [ ] Zero `[WARN] [preempt]` lines in the serial log.
- [ ] Zero panics from the user-mode-return debug assertion.
- [ ] No scheduler regressions (no `[WARN] [sched]` lines that did not appear pre-57b).

### F.5 — README update

**File:** `docs/roadmap/README.md`
**Symbol:** —
**Why it matters:** The roadmap must reflect the new subphase split.

**Acceptance:**
- [ ] Phase 57b row added to the milestone summary table with status `Complete` (after merge).
- [ ] Mermaid graph updated to show 57b → 57d edge.
- [ ] Gantt chart updated.

---

## Documentation Notes

- This phase replaces the umbrella "57b — Kernel Preemption" entry in `docs/roadmap/README.md` with four subphases: 57b (this), 57c (audit), 57d (voluntary preemption), 57e (full kernel preemption).
- The appendix at `docs/appendix/preemptive-multitasking.md` is the design source of truth and should be referenced from every commit that lands a Track A–F task.
- The 57b/57c integration commit (when 57b lands) wraps each Track-C-annotated busy-spin in 57c with a `preempt_disable` / `preempt_enable` pair.  This is a follow-up commit, not a 57b task; 57c only adds the comment, 57b adds the wrapper.
- The Track A.1 audit catalogue at `docs/handoffs/57b-spinlock-callsite-audit.md` is the durable artefact — future reviewers can find the discipline applied to any kernel lock in one lookup.
