# Phase 57b — Preemption Foundation: Task List

**Status:** Planned
**Source Ref:** phase-57b
**Depends on:** Phase 4 ✅, Phase 25 ✅, Phase 35 ✅, Phase 52c ✅ (free-list reuse), Phase 57a ✅
**Goal:** Land per-task `preempt_count` discipline, the `PreemptFrame` save area, stable per-task storage, a per-CPU `current_preempt_count_ptr`, and lock-free spinlock-raises-`preempt_count` wiring as a no-op refactor.  Establish the contract every later subphase relies on.  No behaviour change; the kernel becomes preemption-CAPABLE but pre-emption is never actually fired.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Audit + TDD foundation (kernel-core model, lock declaration *and* acquisition catalogue) | — | Planned |
| B | Stable per-task storage (`Vec<Box<Task>>`); free-list and indexing audit | A | Planned |
| C | Per-CPU `current_preempt_count_ptr` + boot dummy + dispatch wiring | B | Planned |
| D | `Task::preempt_count` field + lock-free `preempt_disable` / `preempt_enable` helpers + user-mode-return assertion | C | Planned |
| E | `PreemptFrame` save-area struct + offset-of constants (unused in 57b) | A | Planned |
| F | `IrqSafeMutex` migration to raise `preempt_count` (lock-free) | C, D | Planned |
| G | Per-callsite migration of non-`IrqSafeMutex` lock sites (with IRQ-shared classification) | A, F | Planned |
| H | Documentation, invariants, version bump, validation | A–G | Planned |

Tracks A through D are the foundation — they must complete before F/G.  Track E is independent and can run in parallel.  Track H is the closeout gate.

**Critical ordering:** Tracks B and C must land before D, and D before F.  If F (`IrqSafeMutex` integration) lands before C (per-CPU pointer), `IrqSafeMutex::lock()` calls `preempt_disable()` which would have to call `scheduler_lock()` — and `scheduler_lock()` is itself an `IrqSafeMutex`, producing infinite recursion.  The lock-free `preempt_disable()` requires the per-CPU pointer to exist first; the per-CPU pointer requires stable `Task` storage to point into.

## Engineering Practice Gates (apply to every track)

- **TDD.**  Every implementation commit must reference a test commit that landed *earlier* in the same PR (or in a prior PR).  Tests added in the same commit as implementation are rejected on review.
- **SOLID.**  No new flag fields on `Task` beyond `preempt_count` and `preempt_frame`.  `preempt_count` is only mutated through `preempt_disable` / `preempt_enable`; no callsite touches the field directly.  Per-CPU state (`current_preempt_count_ptr`) lives on `PerCoreData`, not `Task`.
- **DRY.**  Single `preempt_disable` / `preempt_enable` pair; single `PreemptFrame` layout.  No per-callsite variant.
- **Documented invariants.**  `preempt_count` returns to 0 at every user-mode return; maximum nesting depth = 32; per-task storage of value, per-CPU storage of pointer.
- **Lock ordering.**  `preempt_count` is task-local and does not participate in the lock hierarchy.  `preempt_disable` / `preempt_enable` are lock-free — they do not acquire `SCHEDULER.lock` or any `IrqSafeMutex`.  This is what makes Track F's `IrqSafeMutex` integration non-recursive.
- **Migration safety.**  No-op refactor.  Worst case: forgotten `preempt_enable` panics on first user-mode return — caught immediately.  No feature gate required.
- **Observability.**  Debug-build assertion at the user-mode return panics on a non-zero count; release builds rely on the 57a stuck-task watchdog as the coarse signal.

---

## Track A — Audit and TDD Foundation

### A.1 — Catalogue every kernel spinlock callsite (declaration + acquisition + IRQ classification)

**Files:**
- `kernel/src/`
- `kernel-core/src/`

**Symbol:** every lock declaration *and* every lock acquisition site
**Why it matters:** A type-only grep (`spin::Mutex|IrqSafeMutex`) catches imports and `Lazy<Mutex<T>>` declarations but misses many real `.lock()` callsites — especially through aliases, behind `Arc`, or in fields whose type is not visible on the same line.  The safety property the audit must establish is about **acquisition**, not declaration.  In addition, an IRQ-shared `spin::Mutex` cannot be made safe by `preempt_disable` alone — it also needs `interrupts::disable` (or migration to `IrqSafeMutex`).  Misclassifying an IRQ-shared lock as task-only produces a real deadlock under 57d.

**Acceptance:**
- [ ] Markdown table at `docs/handoffs/57b-spinlock-callsite-audit.md` listing every callsite with: file:line, symbol, lock kind, current wrapping pattern, **context** (task / IRQ / IRQ-shared / host-test).
- [ ] Audit produced by *both* scans:
  - Declaration scan: `rg -n 'spin::Mutex|spin::RwLock|use spin::Mutex|use spin::RwLock|IrqSafeMutex|BlockingMutex|Lazy<Mutex|Arc<Mutex' kernel/src kernel-core/src`.
  - Acquisition scan: `rg -n '\.lock\(|\.try_lock\(|\.read\(|\.write\(' kernel/src kernel-core/src`, filtered to lock-acquiring callsites (excluding `RwLock` reads on non-lock types, file `.read` etc.).
- [ ] Every row classified into exactly one of: "already `IrqSafeMutex` (inherits Track F)", "convert to `IrqSafeMutex`", "explicit `preempt_disable` + `without_interrupts`" (for IRQ-shared `spin::Mutex` callsites that must keep their type for ABI reasons), or "host-test only / no kernel exposure".
- [ ] Every row maps to a Track G task (one PR per subsystem).
- [ ] Each IRQ-shared classification cites the ISR that takes the same lock (e.g., `keyboard_handler` for `RAW_INPUT_ROUTER`).

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
**Why it matters:** 57d assembly will use literal offsets into `PreemptFrame`; if the Rust layout drifts from the asm offsets, registers will be saved into wrong slots and the resume will jump to garbage.  A compile-time test pinning the layout catches this immediately.

**Acceptance:**
- [ ] `PreemptFrame` is `#[repr(C)]` with explicit field order.
- [ ] `PREEMPT_FRAME_OFFSET_RAX`, `..._RIP`, `..._RFLAGS`, `..._RSP`, `..._CS`, `..._SS` constants exposed via `core::mem::offset_of!`.
- [ ] Compile-time test: `const _: () = assert!(PREEMPT_FRAME_OFFSET_RAX == 0);` (and similar for every offset) — fails the build on layout drift.
- [ ] Doc comment explains both ring-3-interrupted (5-field CPU frame) and ring-0-interrupted (3-field CPU frame) layouts the assembly entry stub will populate uniformly into `PreemptFrame` slots.

---

## Track B — Stable Per-Task Storage

### B.1 — Convert `Scheduler::tasks` to `Vec<Box<Task>>`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `Scheduler::tasks`, all readers / writers
**Why it matters:** `Vec<Task>` reallocates on `push`, invalidating any cached pointer into a `Task`.  Track C will cache a raw pointer to `Task::preempt_count` in per-CPU data; without stable addresses, that pointer can dangle after the next `spawn`.  `Vec<Box<Task>>` puts each `Task` on the heap behind a stable address; the outer `Vec` can still resize without moving the inner `Box` contents.

**Acceptance:**
- [ ] `Scheduler::tasks: Vec<Box<Task>>`.
- [ ] All callsites that index `tasks[idx]` continue to compile and behave identically (Rust's auto-deref handles the `Box`).
- [ ] Free-list reuse from Phase 52c (`free_indices`) works unchanged: a freed slot is overwritten with a new `Box<Task>` rather than re-using the prior `Task` in place.
- [ ] Doc comment on `Scheduler::tasks`: "Addresses of `Task` instances are stable for the task's lifetime.  Per-CPU dispatch state (`current_preempt_count_ptr`) caches raw pointers into `Task::preempt_count` and relies on this stability."
- [ ] Existing `cargo xtask test` passes — no behaviour change.

### B.2 — Stable-address regression test

**File:** `kernel/tests/task_storage_stable.rs` (new)
**Symbol:** —
**Why it matters:** A direct test that takes a pointer into `tasks[idx].preempt_count`, spawns enough new tasks to force the outer `Vec` to grow several times, then re-reads the pointer and confirms the address has not changed.

**Acceptance:**
- [ ] Test spawns N+1 dummy tasks where N triggers ≥ 3 `Vec` reallocations.
- [ ] Asserts the cached pointer to `tasks[k].preempt_count` (for some early `k`) still points to the same value the original task wrote.
- [ ] Test runs in QEMU as part of `cargo xtask test`.

---

## Track C — Per-CPU `current_preempt_count_ptr`

### C.1 — Add `current_preempt_count_ptr` to `PerCoreData`

**File:** `kernel/src/smp/mod.rs`
**Symbol:** `PerCoreData::current_preempt_count_ptr`, `SCHED_PREEMPT_COUNT_DUMMY`
**Why it matters:** This is the lock-free entry point for `preempt_disable` / `preempt_enable`.  Without it, the helpers must reach `Task::preempt_count` through the scheduler lock, which `IrqSafeMutex::lock` would then try to take while already inside `IrqSafeMutex::lock` — recursive deadlock.

**Acceptance:**
- [ ] `PerCoreData::current_preempt_count_ptr: AtomicPtr<AtomicI32>`.
- [ ] `static SCHED_PREEMPT_COUNT_DUMMY: [AtomicI32; MAX_CORES]` initialised to all zero.  Used both as the boot pointee and as the canonical scheduler-context pointee at every dispatch boundary.
- [ ] During `init_per_core` and `init_ap_per_core`, `current_preempt_count_ptr` is initialised to `&SCHED_PREEMPT_COUNT_DUMMY[core_id] as *const _ as *mut _`.
- [ ] Doc comment on the field documents the invariants: pointer is always valid (per-core dummy or live `Task::preempt_count`); updated only by C.2 (switch-out retarget) and C.3 (switch-in retarget) in interrupt-masked windows; read by `preempt_disable` / `preempt_enable` with `Acquire`; `Vec<Box<Task>>` storage from Track B is what makes the cached `Task::preempt_count` address stable.

### C.2 — Switch-out epilogue retargets pointer to the per-core dummy

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** the dispatch path immediately after `switch_context` returns to the scheduler stack
**Why it matters:** The hard rule is **every `IrqSafeMutex` guard must decrement the same pointee it incremented.**  If the pointer still targets the outgoing task when scheduler-context code (e.g. `pick_next`) takes its first lock, the lock acquisition charges the outgoing task; the corresponding release would then *also* hit the outgoing task — fine.  But if the retarget happens between acquire and release, the release lands on the wrong pointee.  The fix: retarget *before* the scheduler takes any new lock and *after* every existing scheduler-stack guard has released.

**Acceptance:**
- [ ] Immediately after `switch_context` returns onto the scheduler stack — and before any `IrqSafeMutex::lock` runs on that stack — the dispatch path retargets `current_preempt_count_ptr` to `&SCHED_PREEMPT_COUNT_DUMMY[core_id]` with `Release` ordering.
- [ ] Retarget runs inside an explicit `interrupts::disable()` / `interrupts::enable()` window (or `without_interrupts(|| ...)` wrapper).  Do **not** assume `switch_context` left IF=0: `switch_context` `popf`s the scheduler's saved RFLAGS on resume, restoring whatever IF the scheduler had when it dispatched the task (typically IF=1).  The retarget must `cli` itself.
- [ ] `cargo xtask test` passes; no scheduler-context lock-acquire/release pair straddles the retarget.

### C.3 — Switch-in handoff retargets pointer to the incoming task

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** the dispatch path immediately before the next `switch_context` call (entering the chosen task)
**Why it matters:** Mirror of C.2.  The retarget must happen *after* the scheduler has released every lock it acquired against the dummy and *before* `switch_context` transfers to the chosen task — and IRQs must remain disabled across the retarget *and* the call to `switch_context`, so that no IRQ-context `preempt_disable` reads a half-updated pointer or runs between retarget and dispatch.

**Acceptance:**
- [ ] After the scheduler has released every `IrqSafeMutex` guard it acquired in scheduler context, the dispatch path explicitly disables interrupts (`interrupts::disable()`) before retargeting `current_preempt_count_ptr` to `&next_task.preempt_count` with `Release` ordering, and then calls `switch_context` while IRQs remain disabled.
- [ ] `switch_context` then `popf`s the chosen task's saved RFLAGS, restoring its IF state — between the retarget and the chosen task's first instruction IF is never 1.
- [ ] When `switch_context` returns into the chosen task, that task's `IrqSafeMutex::lock` / `Drop` pairs charge its own `preempt_count` — symmetric.
- [ ] Document in the dispatch path that any IRQ that fires *before* this retarget (i.e. while the pointer still targets the dummy) is safe by construction: its `preempt_disable` / `preempt_enable` pair both hit the dummy.

### C.4 — Pointer-lifecycle regression test

**File:** `kernel/tests/preempt_pointer_lifecycle.rs` (new)
**Symbol:** —
**Why it matters:** The lifecycle invariant ("acquire and release hit the same pointee") is subtle enough that an explicit regression test is required.  Without it, a future refactor that moves a scheduler-internal lock relative to the retarget could silently regress.

**Acceptance:**
- [ ] Test that drives a dispatch handoff and asserts: an `IrqSafeMutex` taken in task context and released in task context cycles `Task::preempt_count` exactly once and ends at 0.
- [ ] Test that drives a dispatch handoff and asserts: an `IrqSafeMutex` taken in scheduler context (during `pick_next`) and released in scheduler context cycles `SCHED_PREEMPT_COUNT_DUMMY[core_id]` exactly once and ends at 0.
- [ ] Test that asserts: across N dispatch cycles, no task's `preempt_count` ever goes negative or accumulates a non-zero residual.
- [ ] Test runs in QEMU as part of `cargo xtask test`.

### C.5 — Pointer update tracepoint

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** dispatch path
**Why it matters:** Phase 57a's `sched-trace` feature emits state-transition records.  Adding a `(old_ptr, new_ptr, core, task_id, phase)` record under the same gate (where `phase ∈ {switch_out, switch_in}`) makes future debug sessions trivially able to reconstruct the per-CPU pointer history and identify any acquire/release that straddled a retarget.

**Acceptance:**
- [ ] Under `cfg(feature = "sched-trace")`, both C.2 and C.3 emit a `preempt_ptr_update` record.
- [ ] Default off (no overhead).

---

## Track D — `Task::preempt_count` and Lock-Free Helpers

### D.1 — Add `preempt_count` to `Task`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::preempt_count`
**Why it matters:** The counter is the gate every 57d preemption check consults.  Adding it now (with no-op semantics) lets every later subphase be additive.

**Acceptance:**
- [ ] `Task::preempt_count: AtomicI32` field, initialised to `0` at task construction.
- [ ] Doc comment on the field: "Per-task preempt-disable counter.  Incremented by `preempt_disable()`, decremented by `preempt_enable()`.  Must be 0 at every user-mode return.  Phase 57d/57e gate preemption on this == 0."
- [ ] Existing `cargo xtask test` passes (no semantic change yet — the counter is initialised but never read).

### D.2 — Implement lock-free `preempt_disable()` / `preempt_enable()`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `preempt_disable`, `preempt_enable`
**Why it matters:** These are the canonical entry points for every spinlock callsite.  They **must not** acquire any lock — once Track F wires them into `IrqSafeMutex::lock`, taking a lock here would recurse.

**Acceptance:**
- [ ] `preempt_disable()` reads `per_core().current_preempt_count_ptr` with `Acquire`, then `(*ptr).fetch_add(1, Acquire)`.  No `scheduler_lock()` call.  No `IrqSafeMutex::lock()` call.  No `current_task_idx()` lookup.
- [ ] `preempt_enable()` reads the same pointer, then `(*ptr).fetch_sub(1, Release)`.  In 57b the post-decrement value is **not** inspected — the deferred-reschedule on zero-crossing is 57d's responsibility, explicitly deferred per `docs/roadmap/57d-voluntary-preemption.md`.
- [ ] Both functions tolerate a null-equivalent state by virtue of the boot dummy (the pointer is always valid; an early-boot increment lands on the dummy and is harmless).
- [ ] A debug assertion panics if the post-increment counter exceeds 32 (catches "preempt_disable in a loop" bugs).
- [ ] Recursion-safety regression test: a synthetic test that calls `preempt_disable()` from inside `IrqSafeMutex::lock()` (after Track F lands) does not deadlock.
- [ ] Unit test in `kernel-core` (model) and integration test in `kernel/tests/` exercising paired and nested operations.

### D.3 — User-mode-return debug assertion

**Files:**
- `kernel/src/arch/x86_64/syscall/mod.rs` (syscall return path)
- `kernel/src/arch/x86_64/interrupts.rs` (IRQ return path for IRQs that interrupted user mode)

**Symbol:** the syscall-return and `iretq`-to-user-mode boundaries
**Why it matters:** The earliest possible detection of a forgotten `preempt_enable`.  Without this assertion, a missed wrapper might not surface until 57d when preemption fires inside the held lock — by which time the kernel has deadlocked.

**Acceptance:**
- [ ] At the syscall-return path (just before the `sysretq` or equivalent), `debug_assert!((*per_core().current_preempt_count_ptr.load(Acquire)).load(Relaxed) == 0)`.
- [ ] At every IRQ-return-to-ring-3 path (timer, keyboard, mouse, NIC, etc.), the same assertion.
- [ ] In release builds the assertion is compiled out (no overhead).
- [ ] Existing `cargo xtask test` passes (no spinlock callsite forgets to release in current code; the assertion never trips).

---

## Track E — `PreemptFrame` Save-Area

### E.1 — `PreemptFrame` struct on `Task`

**File:** `kernel/src/task/mod.rs`
**Symbol:** `Task::preempt_frame`, `PreemptFrame`
**Why it matters:** 57d's assembly entry stub writes into this field.  Adding it now (zero-initialised, untouched) lets 57d be a pure additive change with no `Task` layout churn.

**Acceptance:**
- [ ] `Task::preempt_frame: PreemptFrame` field, zero-initialised.
- [ ] `PreemptFrame` is the `#[repr(C)]` struct from A.4.
- [ ] Doc comment: "Phase 57b infrastructure.  Written by 57d's assembly entry stub; read by 57d/57e's preempt-resume routines.  Unused in 57b."
- [ ] Existing `cargo xtask test` passes.
- [ ] `kernel/src/task/mod.rs` exposes `PREEMPT_FRAME_OFFSET_*` constants for the future 57d assembly.

### E.2 — `Task` layout regression test

**File:** `kernel/tests/task_layout.rs` (new) or `kernel-core/tests/preempt_layout.rs`
**Symbol:** `Task::preempt_frame` offset
**Why it matters:** A drift in `Task` field ordering (e.g., adding a field before `preempt_frame`) silently breaks the 57d assembly.  A compile-time check pins the offset.

**Acceptance:**
- [ ] Compile-time test asserting `core::mem::offset_of!(Task, preempt_frame)` equals the documented constant.
- [ ] Doc comment in the test explains why the offset is load-bearing for 57d.

---

## Track F — `IrqSafeMutex` Raises `preempt_count`

### F.1 — Wire `preempt_disable` into `IrqSafeMutex::lock`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `IrqSafeMutex::lock`, `IrqSafeGuard`
**Why it matters:** This is the single high-leverage point that gives every existing `IrqSafeMutex` callsite preempt-discipline for free — no per-callsite migration required.  It is also the recursion hazard if D.2 is not lock-free; D.2 must land first.

**Acceptance:**
- [ ] `IrqSafeMutex::lock()` calls `preempt_disable()` *before* `interrupts::disable()`.
- [ ] `IrqSafeGuard::Drop` calls `preempt_enable()` *after* `interrupts::enable()`.
- [ ] Drop-order regression test: a synthetic test confirms the spin-unlock fires before interrupt-restore (the existing 57a invariant) and `preempt_enable` fires last.
- [ ] `try_lock` mirrors the same pattern (raise on success, no-op on `None` return).
- [ ] Recursion-safety test: nested `IrqSafeMutex` acquisition (e.g., `pi_lock` outer, `SCHEDULER.lock` inner) produces `preempt_count == 2` at the innermost point and returns to 0 on full unwind.  No deadlock.
- [ ] Existing `cargo xtask test` passes.

### F.2 — `SchedulerGuard` inherits the discipline

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `scheduler_lock`, `SchedulerGuard`
**Why it matters:** The 57a `SchedulerGuard` wraps `IrqSafeGuard`.  F.1 gives it preempt-discipline automatically.  Verify, document, and test.

**Acceptance:**
- [ ] No code change required (the wrapper inherits F.1's discipline).
- [ ] Doc comment on `scheduler_lock` updated to mention preempt-discipline.
- [ ] Regression test: scheduler lock acquire/release cycles `preempt_count` exactly once.

---

## Track G — Per-Callsite Migration

For each subsystem identified in A.1's audit, convert non-`IrqSafeMutex` locks per the audit's classification.  Each task is a single PR.  Three migration shapes:

1. **Convert to `IrqSafeMutex`** (preferred): replaces a plain `spin::Mutex` with `IrqSafeMutex`; inherits Track F's preempt-discipline plus IRQ masking.  Used when the lock is task-only or needs IRQ masking anyway.
2. **`without_interrupts` + explicit `preempt_disable`/`preempt_enable`** (for IRQ-shared `spin::Mutex` callsites that cannot migrate due to ABI constraints).  Both are required: `preempt_disable` is *not* a substitute for IRQ masking on a same-core ISR-shared lock.
3. **Plain `preempt_disable` / `preempt_enable`** (for task-only locks where the caller has a reason not to use `IrqSafeMutex`).

### G.1 — `kernel/src/blk/`

**Files:**
- `kernel/src/blk/virtio_blk.rs`
- `kernel/src/blk/remote.rs`

**Symbol:** every lock callsite in the block layer
**Why it matters:** Block-device locks are held across the request-submit / IRQ-completion path; missing preempt-discipline here would deadlock the storage stack on a 57d preemption.  The IRQ-completion side must be classified explicitly (per A.1).

**Acceptance:**
- [ ] Every callsite in `kernel/src/blk/` migrated per its A.1 classification.
- [ ] Regression test asserts `preempt_count` returns to 0 across a virtio-blk request submit + IRQ wake.
- [ ] `cargo xtask test` passes.

### G.2 — `kernel/src/net/`

**Files:**
- `kernel/src/net/virtio_net.rs`
- `kernel/src/net/tcp.rs`, `udp.rs`, `arp.rs`, `unix.rs`, `remote.rs`

**Symbol:** every lock callsite in the network stack
**Why it matters:** Network-stack locks are held across send/recv paths and IRQ wakes; same risk profile as the block layer.

**Acceptance:**
- [ ] Every callsite in `kernel/src/net/` migrated per A.1 classification.
- [ ] Regression test asserts `preempt_count` returns to 0 across a TCP send/recv round-trip.
- [ ] `cargo xtask test` passes.

### G.3 — `kernel/src/fs/`

**Files:**
- `kernel/src/fs/tmpfs.rs`, `ext2.rs`, `fat32.rs`, `vfs.rs`, `procfs.rs`, `protocol.rs`

**Symbol:** every lock callsite in the file-system layer
**Why it matters:** FS locks are held across read/write/getdents paths; preempt-discipline must be uniform.

**Acceptance:**
- [ ] Every FS callsite migrated per A.1 classification.
- [ ] Regression test exercises a write/read/getdents triple and asserts `preempt_count` returns to 0.

### G.4 — `kernel/src/mm/`

**Files:**
- `kernel/src/mm/slab.rs`, `frame_allocator.rs`, `heap.rs`

**Symbol:** every allocator-internal lock callsite
**Why it matters:** Allocator locks are held during page-fault handling and slab-cache refills; a missed wrapper here is catastrophic under 57d.

**Acceptance:**
- [ ] Every callsite in `kernel/src/mm/` migrated per A.1 classification.
- [ ] Regression test exercises a heap allocation path and asserts `preempt_count` returns to 0.

### G.5 — `kernel/src/iommu/`

**Files:**
- `kernel/src/iommu/intel.rs`, `amd.rs`, `registry.rs`

**Symbol:** every IOMMU command-queue lock callsite
**Why it matters:** IOMMU locks gate DMA mapping; preempt-discipline must be uniform.

**Acceptance:**
- [ ] Every IOMMU callsite migrated per A.1 classification.
- [ ] Regression test exercises an IOMMU map/unmap and asserts `preempt_count` returns to 0.

### G.6 — `kernel/src/process/`, `kernel/src/ipc/`, `kernel/src/syscall/`

**Files:**
- `kernel/src/process/futex.rs`, `mod.rs`
- `kernel/src/ipc/notification.rs`
- `kernel/src/syscall/device_host.rs`

**Symbol:** every lock callsite in the process / IPC / syscall layer
**Why it matters:** These paths span the syscall fast-path; preempt-discipline must be uniform.

**Acceptance:**
- [ ] Every callsite migrated per A.1 classification.
- [ ] Regression test exercises a futex wait/wake, a notification deliver, and a device-host syscall.

### G.7 — `kernel/src/{pipe,serial,tty,pty,stdin,signal,trace,testing,fb,rtc}.rs`

**Symbol:** every remaining lock callsite, including the IRQ-shared `RAW_INPUT_ROUTER` (`kernel/src/arch/x86_64/interrupts.rs::keyboard_handler` writes; `read_raw_scancode` reads in task context — already in a `without_interrupts` wrapper, must add `preempt_disable`/`preempt_enable`).
**Why it matters:** Catch-all for the remaining single-file subsystems.  Several of these contain ISR-shared `spin::Mutex` callsites that fall into migration shape (2) above.

**Acceptance:**
- [ ] Every callsite migrated per A.1 classification.
- [ ] `cargo xtask check` clean; `cargo xtask test` passes.

### G.8 — `kernel/src/smp/`, `kernel/src/arch/x86_64/`

**Files:**
- `kernel/src/smp/mod.rs`, `tlb.rs`, `ipi.rs`
- `kernel/src/arch/x86_64/ps2.rs`, `interrupts.rs`, `syscall/mod.rs`

**Symbol:** every lock callsite in the SMP / arch layer
**Why it matters:** SMP layer touches per-core data; arch layer touches IRQ paths.  Both must be preempt-disciplined under 57d.

**Acceptance:**
- [ ] Every callsite migrated per A.1 classification.
- [ ] Regression test exercises an IPI delivery (TLB shootdown) and asserts `preempt_count` returns to 0.

### G.9 — `kernel-core/src/`

**Files:**
- `kernel-core/src/magazine.rs`
- `kernel-core/src/device_host/registry_logic.rs`

**Symbol:** every lock callsite in `kernel-core` (kernel-build only; host-build paths are not affected)
**Why it matters:** `kernel-core` types embedded in kernel `Task` / scheduler must be preempt-disciplined.

**Acceptance:**
- [ ] Every callsite migrated; host tests in `kernel-core` continue to pass on the host (where `preempt_disable` is a no-op stub).
- [ ] Kernel-side regression test asserts `preempt_count` returns to 0 across a magazine refill.

---

## Track H — Documentation, Invariants, Validation

### H.1 — Top-of-file doc block in `scheduler.rs`

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** module doc
**Why it matters:** A future reader must be able to read the top of `scheduler.rs` and understand the `preempt_count` discipline.  Without this, the discipline is folklore.

**Acceptance:**
- [ ] New section in the doc block titled `## preempt_count`.
- [ ] Documents: per-task storage of value, per-CPU storage of pointer, raise on lock, drop on unlock, return-to-0 invariant, max nesting depth, recursion-safety rationale (lock-free helpers), 57d/57e dependency.
- [ ] References `docs/appendix/preemptive-multitasking.md` and `docs/roadmap/57b-preemption-foundation.md`.

### H.2 — Update `docs/04-tasking.md`

**File:** `docs/04-tasking.md`
**Symbol:** —
**Why it matters:** The narrative documentation must describe preempt-discipline so future learners understand the kernel's current state.

**Acceptance:**
- [ ] New subsection titled "Preempt-discipline (Phase 57b)".
- [ ] Describes `preempt_count`, `current_preempt_count_ptr`, `IrqSafeMutex` integration, and the user-mode-return invariant.
- [ ] References Phase 57b and the appendix.

### H.3 — Kernel version bump

**Files:**
- `kernel/Cargo.toml`
- `kernel/src/main.rs` (banner)

**Symbol:** version constant
**Why it matters:** The phase landing must be visible to anyone reading the boot banner.

**Acceptance:**
- [ ] `kernel/Cargo.toml` version bumped (e.g., `0.57.2`).
- [ ] Boot banner reflects the new version.

### H.4 — 30-minute soak gate

**File:** procedural; results documented in PR description
**Symbol:** —
**Why it matters:** The phase is a no-op refactor.  A soak gate confirms the refactor is genuinely no-op: no panics from the user-mode-return assertion, no scheduler regressions.

**Acceptance:**
- [ ] 30-minute soak with `cargo xtask run-gui --fresh` plus synthetic IPC + futex + notification load on 4 cores.
- [ ] Zero panics from the user-mode-return debug assertion.
- [ ] No scheduler regressions (no `[WARN] [sched]` lines that did not appear pre-57b).

### H.5 — README update

**File:** `docs/roadmap/README.md`
**Symbol:** —
**Why it matters:** The roadmap must reflect the new subphase split.

**Acceptance:**
- [ ] Phase 57b row marked `Complete` after merge.
- [ ] Mermaid graph updated.
- [ ] Gantt chart updated.

---

## Documentation Notes

- This phase replaces the umbrella "57b — Kernel Preemption" entry in `docs/roadmap/README.md` with four subphases: 57b (this), 57c (audit), 57d (voluntary preemption), 57e (full kernel preemption).
- The appendix at `docs/appendix/preemptive-multitasking.md` is the design source of truth and should be referenced from every commit that lands a Track A–H task.
- The `preempt_disable` / `preempt_enable` pair is **lock-free by mandate**.  Any reviewer who sees a future patch that adds a `scheduler_lock()` or `IrqSafeMutex::lock()` call inside these helpers must reject it — that change reintroduces the recursive-deadlock hazard described in the PR-131 review.
- 57c's `preempt_disable` wrappers around hardware-bounded busy-spins are load-bearing for **57e** (full kernel preemption), not 57d.  Under 57d (voluntary preemption), kernel-mode is non-preemptible by construction (the `from_user` check); the wrappers do not change behaviour until 57e drops that check.  See `docs/roadmap/57e-full-kernel-preemption.md` Track B.
- The `preempt_enable` zero-crossing scheduler trigger (the Linux `preempt_enable() → schedule()` pattern) is **explicitly deferred to 57d**.  57b's `preempt_enable` is a pure decrement.
- The Track A.1 audit catalogue at `docs/handoffs/57b-spinlock-callsite-audit.md` is the durable artefact — future reviewers can find the discipline applied to any kernel lock in one lookup.
