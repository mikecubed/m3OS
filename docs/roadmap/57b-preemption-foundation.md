# Phase 57b — Preemption Foundation

**Status:** Planned
**Source Ref:** phase-57b
**Depends on:** Phase 4 (Tasking) ✅, Phase 25 (SMP) ✅, Phase 35 (True SMP) ✅, Phase 57a (Scheduler Block/Wake Protocol Rewrite) ✅
**Builds on:** Extends the Phase 4 `switch_context` contract with a separate full-register-save path used only by preemption. Adds per-task preempt-discipline counters that wrap every existing `IrqSafeMutex` callsite from Phases 4–57a.
**Primary Components:** `kernel/src/task/scheduler.rs` (`IrqSafeMutex`, scheduler-lock sentinel), `kernel/src/task/mod.rs` (`Task` layout, `switch_context` ABI), `kernel/src/smp/mod.rs` (`PerCoreData`), `kernel/src/arch/x86_64/asm/switch.S` *(new file — currently inline `global_asm!` in `task/mod.rs`)*, `kernel-core/src/preempt_model.rs` *(new — pure-logic counter model with host tests)*

## Milestone Goal

The kernel becomes **preemption-capable but never actually preempts**.  Every existing spinlock callsite increments a per-task `preempt_count` while held, every task carries a full-register-save area large enough for an `iretq` frame, and `cargo xtask test` passes with zero behaviour change.  This is a no-op refactor that establishes the contract every later subphase will rely on: `preempt_count == 0` is the precondition for firing preemption, full register state can be saved without disturbing the cooperative `switch_context` path, and the single-page-per-task kernel-stack invariant is documented.

## Why This Phase Exists

Phase 57a closed the v1 lost-wake bug class but left the user-hardware acceptance gate (I.1) failing.  The remaining failure mode catalogued in `docs/handoffs/57a-validation-gate.md` is **cooperative-scheduling starvation**: a syscall busy-waiting in kernel mode, or a userspace task in a tight CPU-bound loop, monopolises its core because the kernel's timer IRQ does *not* preempt running code on return — it merely sets a `reschedule` flag that is consulted at voluntary yield points.

The targeted fix is described in `docs/appendix/preemptive-multitasking.md`: introduce Linux's `preempt_count` discipline so an IRQ handler can safely interrupt a running task and switch to the scheduler whenever `preempt_count == 0`.  The full programme is split into four subphases (57b/57c/57d/57e); this phase delivers the **infrastructure** without firing preemption anywhere — keeping the blast radius small while every spinlock callsite is touched in a uniform mechanical pattern.

The phase is intentionally **behaviour-neutral**.  No IRQ handler invokes the new preempt path, no task is preempted, no user-visible behaviour changes.  This isolates the refactor's risk to "did we forget to drop a `preempt_count` somewhere", which a single debug assertion at the user-mode return boundary will catch on the first user-mode entry per CPU.

## Learning Goals

- Why Linux's `preempt_count` discipline composes cleanly with spinlocks — the lock acquire and the preempt-disable form a single nested counter, so unlock and preempt-enable are symmetric.
- How a "no behaviour change" refactor that touches every spinlock callsite is made safe via a single debug assertion at the user-mode return boundary (`preempt_count == 0`) plus a property test on the counter.
- Why `switch_context` cannot be reused for preemption: the callee-saved-only ABI is only valid at a voluntary call boundary.  An IRQ-driven preemption point fires mid-instruction and must save the full GPR set plus `RFLAGS` and the `iretq` frame.
- How a per-CPU `holds_scheduler_lock` flag (introduced for Phase 57a's lock-ordering assertion) can be reused to confirm the SOLID Single-Responsibility split: `preempt_count` lives on the *task*, not the core, and is independent of run-queue manipulation.
- Why the kernel-stack-per-task invariant (Phase 4) makes preemption tractable: the full preempted register frame can live on the same kernel stack the task was running on, just like Linux uses `current->stack`.

## Feature Scope

### Stable per-task storage (prerequisite for lock-free preempt access)

Before `preempt_disable` / `preempt_enable` can be wired into `IrqSafeMutex::lock`, `Task` storage must allow a stable raw pointer into a task's `preempt_count` field.  The current `Scheduler::tasks: Vec<Task>` reallocates on `push`, so any pointer into a task is invalidated by a later `spawn`.  Without stable storage, the lock-free preempt-disable path described below has no safe address to work from.

- Convert `Scheduler::tasks` from `Vec<Task>` to `Vec<Box<Task>>`.  Indices are unchanged; only the in-place storage moves to the heap.  The free-list reuse path from Phase 52c works unchanged.
- The cost is one extra allocation per `spawn` and an extra pointer-dereference per `pick_next` access.  Both are negligible at this scale.
- Doc comment on `Scheduler::tasks` explicitly states: "addresses of `Task` instances are stable for the task's lifetime; raw pointers into `Task` fields may be cached by per-CPU dispatch state".

### Per-CPU `current_preempt_count_ptr` (lock-free access)

`preempt_disable` / `preempt_enable` must run without acquiring `SCHEDULER.lock`.  Once Track F wires those functions into `IrqSafeMutex::lock`, taking the scheduler lock would itself call `preempt_disable`, which would in turn try to take the scheduler lock — recursive deadlock.  The fix is per-CPU pointer access:

- New field `PerCoreData::current_preempt_count_ptr: AtomicPtr<AtomicI32>`.  Read by `preempt_disable` / `preempt_enable` with `Acquire` ordering.  Written by the dispatch / switch-out / switch-in paths with `Release` ordering, in the lifecycle described below.
- Per-core dummy: a `static SCHED_PREEMPT_COUNT_DUMMY: [AtomicI32; MAX_CORES]` is the initial pointee at boot and the canonical "scheduler-context" pointee at every dispatch boundary.  Increments on the dummy are observable nowhere — they cancel correctly at unlock and never overflow because each dummy is per-core.
- The `Vec<Box<Task>>` storage from the previous section is what makes the per-task pointee safe: the address embedded in the pointer is stable for the task's lifetime.

#### Pointer lifecycle (load-bearing invariant)

The hard rule: **every `IrqSafeMutex` guard must decrement the same preempt-count pointee it incremented.**  Violating this drives the wrong task's `preempt_count` negative and leaves the right task's count permanently elevated, neither of which 57d preemption tolerates.

The four phases of the dispatch path:

1. **Running-task context.**  Pointer targets `current_task().preempt_count`.  Every `IrqSafeMutex::lock` / `Drop` pair increments and decrements the running task's count — symmetric.
2. **Switch-out epilogue (explicit `cli`).**  Immediately after `switch_context` *returns* to the scheduler stack — i.e. on the path that runs *after* the outgoing task's RSP has been saved but *before* any code on the scheduler stack acquires another lock — retarget the pointer to `SCHED_PREEMPT_COUNT_DUMMY[core_id]` with `Release`.  The retarget runs with interrupts explicitly disabled (`x86_64::instructions::interrupts::disable()` on entry to the post-return code, restored after retarget).  *This is not an IF=0 window inherited from `switch_context`* — `switch_context` restores RFLAGS via `popf`, which restores the scheduler's saved IF state from when it dispatched the task.  If the scheduler dispatched with IF=1 (the common case), it resumes with IF=1.  The post-return code therefore must `cli` itself before reading or mutating the pointer.  Once retargeted, the scheduler-context code that follows (e.g. `pick_next`, run-queue manipulation) charges its lock acquisitions to the dummy.
3. **Scheduler/idle context.**  Pointer targets the per-core dummy.  `IrqSafeMutex::lock` / `Drop` pairs in `pick_next`, the deadline scanner, the watchdog, and the per-core idle loop all increment and decrement the dummy — symmetric.  Note that an IRQ that fires during scheduler context still calls `preempt_disable` (via any spinlock acquired in IRQ context); because the pointer is the dummy, the increment/decrement pair lands on the dummy and balances correctly.
4. **Switch-in handoff (explicit `cli`).**  After the scheduler has chosen the next task and has *released every lock it acquired under the dummy*, but *before* it calls `switch_context` to enter the chosen task, the dispatch path explicitly disables interrupts, retargets the pointer from the dummy to `next_task.preempt_count` with `Release`, and calls `switch_context`.  `switch_context` itself then `popf`s the chosen task's saved RFLAGS, restoring its IF state.  At no point between the retarget and the chosen task's first instruction is IF=1.

The fundamental property phases 2 and 4 enforce: when a lock is acquired in scheduler context and released in scheduler context, both ops hit the dummy.  When a lock is acquired in task context and released in task context, both hit the same `Task::preempt_count`.  No `IrqSafeMutex::lock` may straddle a pointer retarget — that is the failure mode the prior re-review flagged.

Tasks C.2 (switch-out retarget) and C.3 (switch-in retarget) of 57b's task list pin this invariant in code; C.4 is the regression test that asserts the property by construction.

### `preempt_count` infrastructure (Piece 2 from the appendix)

Add a per-task counter that gates preemption.

- New field `Task::preempt_count: AtomicI32`, initialised to `0`.
- New free functions `preempt_disable()` / `preempt_enable()` that read the per-CPU `current_preempt_count_ptr` (Acquire) and `fetch_add` / `fetch_sub` directly.  No scheduler lock, no `current_task_idx` lookup on the hot path.
- `preempt_disable` is a fetch-add 1 with `Acquire` ordering.  `preempt_enable` is a fetch-sub 1 with `Release` ordering; in 57b it never inspects the result.  The "deferred reschedule on `preempt_enable` zero-crossing" mechanism described in `docs/appendix/preemptive-multitasking.md` is **deferred to 57d** (where it can fire `signal_reschedule()` plus the IRQ-return preemption path); 57b's `preempt_enable` never inspects the post-decrement value.
- `preempt_count` is on the **task**, not the core.  This matches Linux's `preempt_count` location post-2003 (it lives in `thread_info` / `current_thread_info()`).  Per-CPU storage of the *value* is faster but requires careful migration handling at context-switch time; we keep per-task storage of the value and per-CPU storage of a *pointer* to that value, which is the common Linux/BSD pattern.
- The counter has a documented maximum nesting depth (16 plus a slack of 16 for diagnostic frames = 32).  A `debug_assert!` panics if exceeded — this catches "preempt_disable in a loop" bugs.

### Spinlock-raises-`preempt_count` discipline (Piece 3 from the appendix)

Every existing spinlock acquire must increment `preempt_count`; every release must decrement it.

- `IrqSafeMutex::lock()` calls `preempt_disable()` before disabling interrupts; `Drop` calls `preempt_enable()` after re-enabling interrupts.  The order matters: the interrupt-disable must outlive the preempt-disable so an IRQ that arrives in the unlock window cannot fire preemption while still holding the lock.
- `SCHEDULER_INNER.lock()` (already wrapped via the `scheduler_lock()` helper) inherits this for free — and only because `preempt_disable` is lock-free per the previous section.
- Every other `spin::Mutex` / `spin::RwLock` callsite in the kernel either:
  1. Migrates to `IrqSafeMutex` (preferred — most are already implicitly IRQ-safe via `without_interrupts` wrappers), or
  2. Adds an explicit `preempt_disable()` / `preempt_enable()` wrapper at the lock boundary.
- **IRQ-shared locks (any `spin::Mutex` taken in both task and ISR context) are classified separately** during the audit.  `preempt_disable` is *not* a substitute for masking same-core interrupts; an IRQ-shared `spin::Mutex` either migrates to `IrqSafeMutex` (which masks IRQs and raises preempt) or stays as a plain `spin::Mutex` wrapped in `without_interrupts(|| ...)` plus `preempt_disable` / `preempt_enable`.

The audit covers `kernel/src/`, `kernel-core/src/` (kernel build only), and the in-kernel uses in `kernel/initrd/` if any.  Track A.1 produces the full callsite catalogue using both declaration and acquisition scans.

### Full-register-save area on `Task` (Piece 1 from the appendix)

Add the storage required for an `iretq`-driven preemption return.

- New field `Task::preempt_frame: PreemptFrame` (zero-initialised).  `PreemptFrame` is a `#[repr(C)]` struct with the 15 GPRs (no `rsp`), `RFLAGS`, `cs`, `ss`, `rip`, and the saved `rsp` — populated by 57d's assembly entry stub from the CPU-pushed IRQ frame *plus* explicit GPR captures.
- The same `PreemptFrame` is used for both ring-3-interrupted and ring-0-interrupted preemption.  The CPU pushes a different frame shape in each case (5 fields for ring-3 → ring-0 transitions, 3 fields for ring-0 → ring-0); 57d's entry stub captures both shapes uniformly into the `PreemptFrame` slots, and 57e's resume routine selects the right `iretq` epilogue based on the saved `cs.rpl`.  See Phase 57e for the same-CPL `iretq` frame-shape detail.
- A `preempt_frame_offset_*` block in `kernel/src/task/mod.rs` exposes `core::mem::offset_of!` constants for the assembly path so 57d's `preempt_to_scheduler` can use literal offsets rather than computing them in Rust.
- The frame is **not used in 57b** — no code reads or writes it.  Adding it now lets 57d be a pure additive change with no `Task` layout churn.
- The existing cooperative `switch_context` path is unchanged.  `PreemptFrame` is a parallel save-area used only by the preempt path.

### Lock-ordering documentation update

Phase 57a documented the `pi_lock` (outer) → `SCHEDULER.lock` (inner) ordering at the top of `scheduler.rs`.  57b adds:

- `preempt_count` is **task-local**, not part of the lock hierarchy.  Acquiring or releasing `preempt_count` does not invalidate any other lock held by this task.
- Spinlocks (which raise `preempt_count`) compose with `pi_lock` and `SCHEDULER.lock` according to the existing hierarchy: `pi_lock` outer, `SCHEDULER.lock` inner, both wrapped in `preempt_disable`.  No spinlock may be acquired across a `preempt_enable` that drops to zero — but this is a 57d concern (no preemption fires until 57d).

### Diagnostic invariant: `preempt_count == 0` at every user-mode return

Add a `debug_assert!` at the user-mode return boundary in `kernel/src/arch/x86_64/syscall/mod.rs` (the syscall return path) and in the IRQ return path in `kernel/src/arch/x86_64/interrupts.rs` for IRQs that interrupted user mode: `current_task().preempt_count.load(Relaxed) == 0`.  A non-zero value means somebody forgot a `preempt_enable` — and we have caught the bug at the earliest possible moment.

## Engineering Practice Requirements

This phase is the SOLID/TDD/DRY foundation for 57c/57d/57e.  Practices are enforced by review and CI:

- **Test-Driven Development.**  Track A defines the `preempt_count` model and host tests in `kernel-core/src/preempt_model.rs` *before* any kernel-side implementation lands.  Property tests cover symmetric increment/decrement, overflow protection, and the "always returns to 0" invariant across random sequences of nested disable/enable.  Every per-callsite migration has a regression test that asserts the count returns to 0 across the protected critical section.
- **SOLID.**
  - *Single Responsibility.* `preempt_count` gates preemption only.  It does not track lock holders, lock kinds, or scheduler state.
  - *Open/Closed.* New synchronisation primitives (e.g., a future `RwLock` variant) plug into `preempt_disable` / `preempt_enable` without touching `Task`.
  - *Liskov.* Every spinlock variant exposes the same invariant: `preempt_count` is incremented exactly once on lock acquire and decremented exactly once on release.
  - *Interface Segregation.* Callers see `preempt_disable()` / `preempt_enable()`; they do not see `Task::preempt_count` directly.
  - *Dependency Inversion.* The `IrqSafeMutex` impl depends on the `preempt_disable` / `preempt_enable` free functions, not on the concrete `Task` field.
- **DRY.**  A single `preempt_disable` / `preempt_enable` pair replaces every ad-hoc `core::sync::atomic::compiler_fence + interrupts::disable` pattern that existed pre-57b.  `PreemptFrame` is the single full-register-save layout — no per-callsite variant.
- **Documented invariants.**  Top-of-file doc block in `scheduler.rs` documents:
  1. `preempt_count` semantics (raise on lock, drop on unlock, must return to 0 at user-mode return).
  2. Maximum nesting depth.
  3. Allocation: per-task, not per-CPU.
  4. The `PreemptFrame` is reserved for 57d; no 57b code touches it.
- **Lock-ordering hierarchy.**  Updated to mention `preempt_count` is a task-local counter that participates in no lock hierarchy.
- **Migration safety.**  The phase is a no-op refactor.  Worst case: a forgotten `preempt_enable` panics on first user-mode return — caught immediately, fixable with a one-line patch.  No feature gate required because the change is behaviour-neutral.
- **Observability.**  `kernel-core::preempt_model` exposes a counter inspector for debug builds; `[INFO] [preempt]` log line on first non-zero count observed at user-mode return helps diagnose the rare bug.

## Important Components and How They Work

### `Task::preempt_count: AtomicI32`

The new field on `Task`.  Initialised to `0` on task construction.  Only the task itself ever mutates the counter (preempt is task-local), so atomic ordering is mostly for compiler-fence semantics rather than cross-CPU synchronisation.  An `Acquire` on increment and `Release` on decrement is sufficient.

### `preempt_disable()` and `preempt_enable()`

Located at `kernel/src/task/scheduler.rs::preempt_disable` / `::preempt_enable`.  Lock-free free functions that operate on the per-CPU `current_preempt_count_ptr`:

```rust
#[inline]
pub fn preempt_disable() {
    let ptr = crate::smp::per_core().current_preempt_count_ptr.load(Acquire);
    // SAFETY: ptr is either a per-core SCHED_PREEMPT_COUNT_DUMMY or points to a
    // live Task::preempt_count.  Tasks are stored in Vec<Box<Task>> so the
    // address is stable for the task's lifetime.  The pointer is changed only
    // by the C.2 switch-out and C.3 switch-in retargets, each wrapped in an
    // explicit cli/interrupts-restore window — no IRQ can observe a torn
    // pointer because each retarget is a single AtomicPtr store with Release
    // ordering and the IRQ handler reads with Acquire.  This function does NOT
    // assume any IF=0 window inherited from switch_context (switch_context
    // restores the scheduler's saved RFLAGS via popf, which may be IF=1).
    unsafe { (*ptr).fetch_add(1, Acquire); }
}

#[inline]
pub fn preempt_enable() {
    let ptr = crate::smp::per_core().current_preempt_count_ptr.load(Acquire);
    unsafe { (*ptr).fetch_sub(1, Release); }
    // 57b: never inspects the post-decrement value.
    // 57d: extends this with a deferred-reschedule check (Track on `preempt_enable` zero-crossing).
}
```

This shape is mandatory: any version that calls `scheduler_lock()` recurses through `IrqSafeMutex::lock` once Track F wires `preempt_disable` into the lock path.  The lock-free design eliminates that hazard at the source.  The pointer is retargeted by the dispatch path in two phases (C.2 switch-out, C.3 switch-in) per the lifecycle described in "Per-CPU `current_preempt_count_ptr`" above — *not* in a single update straddling `switch_context`, and *not* relying on any IF=0 window inherited from `switch_context`.  Each retarget is wrapped in an explicit `cli` / interrupts-restore.

### `IrqSafeMutex::lock()` (modified)

Acquires `preempt_disable()` before `interrupts::disable()`; the existing `IrqSafeGuard::Drop` order (spin-unlock → `interrupts::enable`) is extended with a final `preempt_enable()`.  The drop order matters: spin-unlock first (so the lock is free for any new contender), interrupt-restore second (so an ISR that runs in the unlock window cannot reach a just-freed lock with stale `was_enabled` state — the existing 57a invariant), then `preempt_enable()` last (so a preempted-then-resumed task on the way out does not hold a count belonging to a lock it has already released).

### `PreemptFrame` (new struct, unused in 57b)

```rust
#[repr(C)]
pub struct PreemptFrame {
    pub gprs: [u64; 15],   // rax, rbx, rcx, rdx, rsi, rdi, rbp, r8..r15
    pub rip: u64,
    pub cs: u16,
    pub _pad_cs: [u8; 6],
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u16,
    pub _pad_ss: [u8; 6],
}
```

Mirrors the layout `iretq` consumes from the IRQ stack.  `_pad_*` bytes are zero-padding to keep the structure 8-byte aligned.  `kernel/src/task/mod.rs` exposes `core::mem::offset_of!` constants so the 57d assembly can use literal offsets.

### `kernel-core/src/preempt_model.rs` (new)

A pure-logic mirror of the counter, exercised by host tests.  `Counter::disable()` increments; `Counter::enable()` decrements.  `assert_balanced()` checks the count is 0.  Property tests assert that any random sequence of paired disable/enable returns the counter to 0, and that an unbalanced sequence (one extra disable) leaves the counter non-zero — proving the invariant the kernel relies on.

### Lock-ordering documentation update in `scheduler.rs`

The top-of-file doc block (currently documenting the `pi_lock`/`SCHEDULER.lock` hierarchy from 57a) gains a new section:

> ## `preempt_count`
>
> Each `Task` carries a `preempt_count: AtomicI32`.  It is incremented on every spinlock acquire and decremented on every release.  It must return to 0 at every user-mode return boundary; a `debug_assert!` panics on violation.
>
> `preempt_count` is task-local and **does not participate in the lock hierarchy**.  Acquiring or releasing `preempt_count` does not invalidate any other lock held.
>
> 57b ships the counter and the `PreemptFrame` save-area without firing preemption.  57d enables firing.

## How This Builds on Earlier Phases

- **Extends Phase 4 (Tasking)** by adding `preempt_count` and `PreemptFrame` to `Task` without changing the cooperative `switch_context` ABI.
- **Reuses Phase 25 / 35 (SMP)** `PerCoreData` for the existing `reschedule` flag (consumed by 57d) and the `holds_scheduler_lock` sentinel from 57a.
- **Reuses Phase 57a's `IrqSafeMutex`** as the single integration point for the spinlock-raises-`preempt_count` discipline.  Every callsite that already migrated to `IrqSafeMutex` in 57a inherits preempt-discipline for free.
- **Reuses Phase 43c (Regression and Stress)** infrastructure for the soak test that confirms `preempt_count` returns to 0 over a 30-minute load.

## Implementation Outline

1. **Track A — Audit and TDD foundation.**  Catalogue every spinlock declaration *and* every acquisition site (`.lock()`, `.try_lock()`, `.read()`, `.write()`) in `kernel/`.  Classify each as task-only, IRQ-only, or IRQ-shared.  Build the `preempt_model` host tests *before* any kernel change.
2. **Track B — Stable per-task storage.**  Convert `Scheduler::tasks` from `Vec<Task>` to `Vec<Box<Task>>` so raw pointers into `Task::preempt_count` are stable for the task's lifetime.  No behaviour change.  **Must complete before D / E.**
3. **Track C — Per-CPU `current_preempt_count_ptr`.**  Add the per-core pointer and the boot dummy.  Update on every dispatch.  **Must complete before D / E.**
4. **Track D — `Task::preempt_count` field and lock-free helpers.**  Add the field; implement `preempt_disable` / `preempt_enable` against the per-CPU pointer (no scheduler lock); add the user-mode-return debug assertion.
5. **Track E — `PreemptFrame` save-area.**  Add the struct, the offset-of constants, and the zero-init.  No code path uses it.
6. **Track F — `IrqSafeMutex` migration.**  Wire `preempt_disable` / `preempt_enable` into `IrqSafeMutex::lock` / `Drop`.
7. **Track G — Per-callsite migration.**  For every non-`IrqSafeMutex` lock in the kernel, either migrate to `IrqSafeMutex`, wrap with explicit `preempt_disable` / `preempt_enable`, or (for IRQ-shared `spin::Mutex` callsites that cannot migrate) wrap with `without_interrupts` plus `preempt_disable` / `preempt_enable`.
8. **Track H — Documentation, invariants, and validation gate.**  Update `scheduler.rs` top-of-file doc; bump kernel version; soak test.

## Acceptance Criteria

- `Scheduler::tasks` is `Vec<Box<Task>>`; addresses of `Task` instances are stable for the task's lifetime.  Existing `cargo xtask test` passes (no behaviour change relative to `Vec<Task>`).
- `PerCoreData` carries `current_preempt_count_ptr: AtomicPtr<AtomicI32>`, initialised to a per-core dummy and retargeted by the dispatch path in two phases per the lifecycle in "Per-CPU `current_preempt_count_ptr`": (a) switch-out epilogue retargets to the per-core dummy under explicit `cli`, (b) switch-in handoff retargets from the dummy to the incoming task under explicit `cli`.  No retarget straddles an `IrqSafeMutex` acquire/release pair; no retarget assumes IF=0 inherited from `switch_context`.
- `preempt_disable()` / `preempt_enable()` read the per-CPU pointer with `Acquire` and `fetch_add` / `fetch_sub` directly — no scheduler lock acquisition on the hot path.  Recursion-safety regression test: an `IrqSafeMutex` acquisition inside another `IrqSafeMutex` does not deadlock.
- All `Task` instances carry `preempt_count: AtomicI32` initialised to `0`.
- All `Task` instances carry `preempt_frame: PreemptFrame` zero-initialised; the layout matches the `iretq` frame on x86-64.
- `IrqSafeMutex::lock()` raises `preempt_count` exactly once on acquire; `Drop` lowers it exactly once on release.  Property test in `kernel-core::preempt_model` covers random nested sequences and asserts the count returns to 0.
- Every non-`IrqSafeMutex` lock identified in Track A.1's audit either:
  1. Has been migrated to `IrqSafeMutex`, or
  2. Has an explicit `preempt_disable` / `preempt_enable` wrapper at the lock boundary, with a regression test that asserts `preempt_count` returns to 0 across the critical section.
- `debug_assert!` at the user-mode return boundary (`syscall_handler` return path and the IRQ return for IRQs that interrupted user mode) confirms `preempt_count == 0`.  A 30-minute soak with `cargo xtask run-gui --fresh` produces zero panics from this assertion.
- `cargo xtask check` clean (`-D warnings`, rustfmt).
- `cargo xtask test` passes — no behaviour change relative to pre-57b.
- `kernel-core::preempt_model` host tests pass (`cargo test -p kernel-core`).
- `kernel/src/task/scheduler.rs` top-of-file doc block describes `preempt_count` semantics, maximum nesting depth, and the 57d/57e dependency.
- Kernel version bumped to `0.57.2` (or next-available patch).
- Phase 57b row added to `docs/roadmap/README.md` with status `Complete`; mermaid graph updated to show 57b → 57d edge.

## Companion Task List

- [Phase 57b Task List](./tasks/57b-preemption-foundation-tasks.md)

## How Real OS Implementations Differ

- **Linux uses per-CPU `preempt_count`** stored in `thread_info` for the running task.  The per-CPU placement is faster (no atomic on the hot path) but requires the count to be saved and restored at context-switch time.  m3OS uses per-task placement for simplicity; the throughput cost is small at this kernel's scale and the correctness is much easier to reason about.
- **Linux's `PREEMPT_COUNT` macro** has separate bit-fields for hardirq, softirq, and preempt sub-counts.  m3OS uses a flat counter — sufficient because m3OS does not yet have a softirq concept and IRQs are handled differently (no nested IRQ context).
- **Linux's `preempt_disable_notrace`** is a tracing-fast variant.  m3OS does not have a preempt-disable-trace bottleneck and uses a single uniform implementation.
- **seL4** does not have preempt_count — it has fixed-priority non-preemptible kernel mode, which is a stricter model.  m3OS aims for `PREEMPT_VOLUNTARY` (Linux desktop default) which is a softer model and a better fit for the toy-kernel learning goal.

## Deferred Until Later

- **Per-CPU placement** of `preempt_count`.  Deferred to a later optimisation phase if the hot path becomes a bottleneck.
- **Tracing variants** (`preempt_disable_notrace`).  Not needed at m3OS's current scale.
- **Hardirq / softirq sub-counts.**  m3OS does not have a softirq concept; the flat counter is sufficient.
- **Replacing `switch_context` with a unified preempt-aware switch.**  Phase 57b keeps the cooperative path unchanged; the preempt path is a separate routine introduced in 57d.
- **`smp_processor_id` debug assertions** (Linux's "running with preempt enabled in a per-CPU section" check).  m3OS uses `try_per_core` which is preempt-safe; an equivalent check is not required.
