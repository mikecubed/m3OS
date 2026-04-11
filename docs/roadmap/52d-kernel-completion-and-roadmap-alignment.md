# Phase 52d - Kernel Completion and Roadmap Alignment

**Status:** Planned
**Source Ref:** phase-52d
**Depends on:** Phase 52a (Kernel Reliability Fixes) ✅, Phase 52b (Kernel Structural Hardening) ✅, Phase 52c (Kernel Architecture Evolution) ✅
**Builds on:** Audits the claimed outcomes of Phases 52a-52c, finishes the release-critical pieces that remained partial, and explicitly re-baselines any scalability work that was designed but not actually landed.
**Primary Components:** docs/roadmap/52a-kernel-reliability-fixes.md, docs/roadmap/52b-kernel-structural-hardening.md, docs/roadmap/52c-kernel-architecture-evolution.md, docs/roadmap/README.md, kernel/src/task/mod.rs, kernel/src/task/scheduler.rs, kernel/src/arch/x86_64/syscall/mod.rs, kernel/src/ipc/notification.rs, userspace/stdin_feeder/src/main.rs, xtask/src/main.rs

## Milestone Goal

The Phase 52 follow-on work matches reality: the roadmap truthfully describes
what landed, syscall return state is actually task-owned end-to-end, keyboard
input uses the kernel line discipline instead of a ring-3 clone, and the
smoke/regression gates for login, fork, ion, and PTY flows are trustworthy
again.

## Why This Phase Exists

An audit of Phases 52a, 52b, and 52c found that the codebase and the roadmap
drifted apart in a few important ways:

1. **Phase 52a is historically correct but underspecified after later work.**
   The manual `restore_caller_context` stop-gap was a valid fix, but it was
   immediately superseded by the 52b task-owned return-state direction and the
   roadmap never recorded that handoff clearly.

2. **Phase 52b landed the structure of task-owned return state, but not the full
   contract.** `UserReturnState` exists, yet the current implementation still
   saves state at block points, restores only `syscall_user_rsp` from the task,
   and leaves `kernel_stack_top` / `fs_base` split between `Task`, `Process`,
   and per-core scratch.

3. **Phase 52c landed kernel-side line-discipline infrastructure, but the live
   keyboard path still duplicates that logic in userspace.** `push_raw_input`
   and `LineDiscipline` exist, while `userspace/stdin_feeder` still reads
   termios flags and implements `ICANON`, `ISIG`, echo, and canonical editing
   itself.

4. **Some 52c scalability claims were marked complete before the code matched
   them.** The scheduler still relies on the global `SCHEDULER` lock in the
   dispatch path, and notifications are still backed by fixed-size arrays with
   `MAX_NOTIFS` rather than a fully growable pool.

5. **Validation is no longer a side concern.** If smoke and regression failures
   on boot/login/fork/ion/PTTY paths are hard to interpret, then later phases
   like 53 and 54 cannot claim a trustworthy base.

This phase exists to close those gaps directly instead of silently carrying them
forward.

## Learning Goals

- Understand the difference between a historical stop-gap fix and a completed
  architectural migration
- Learn how roadmap drift makes reliability work harder by obscuring what is
  actually guaranteed
- See why terminal/input paths must converge on one line discipline instead of
  keeping a compatibility clone in userspace
- Learn how to separate correctness-critical closure work from larger
  scalability ambitions that should be explicitly deferred
- Treat smoke, regression, and audit notes as part of the product, not as side
  documentation

## Feature Scope

### Audit-backed roadmap realignment

Record which 52a/52b/52c claims were fully delivered, which were superseded by
later work, and which were only partially implemented. This phase does not
erase history; it makes the history honest enough to use as an engineering
reference.

### Task-owned syscall return-state completion

Finish the 52b transition so the scheduler restores one authoritative
task-owned return-state contract. That means saving the relevant state once at
syscall entry, restoring it from one place during dispatch, and removing the
current split ownership between `Task.user_return`, `Process`, and
`PerCoreData`.

### Keyboard input path convergence

Make the keyboard path use the kernel-side `LineDiscipline` that 52c already
introduced. `stdin_feeder` should become a scancode-decoder and raw-byte
forwarder via `push_raw_input`, not a second terminal stack with its own termios
policy.

### Re-baselined scalability claims

Where 52c claimed more than the code currently provides, 52d either completes
the missing work or explicitly moves the unshipped design back into a later
phase. The goal is that roadmap text, code comments, and implementation agree
about the scheduler and notification model.

### Validation and regression closure

Repair the Phase 52 smoke/regression flows so failures in login, fork, ion
prompt, PTY, and signal-reset paths are diagnosable and meaningful. A completed
52d phase should restore confidence that CI failures represent real regressions
rather than roadmap drift or harness ambiguity.

## Important Components and How They Work

### `UserReturnState` and scheduler dispatch

`kernel/src/task/mod.rs` and `kernel/src/task/scheduler.rs` now contain the
scaffolding for task-owned return state, but the current flow is still
transitional. 52d makes the scheduler the single authority for restoring
return-critical state and pushes all durable ownership out of ad hoc per-core
scratch.

### `LineDiscipline`, `push_raw_input`, and `stdin_feeder`

`kernel-core/src/tty.rs` and `kernel/src/arch/x86_64/syscall/mod.rs` already
contain the kernel-side line-discipline path. `userspace/stdin_feeder` must be
reduced to scancode decoding so the keyboard path matches the serial path and no
longer depends on special termios workaround syscalls.

### Scheduler and notification scope

`kernel/src/task/scheduler.rs` and `kernel/src/ipc/notification.rs` currently
mix real 52c improvements (`IsrWakeQueue`, load-balance cooldowns, growable
endpoints/caps) with still-global or fixed-size behavior. 52d documents the
actual model and either finishes or explicitly re-defers the remaining pieces.

### `xtask` smoke/regression harness

`xtask/src/main.rs` defines the boot/login/shell smoke flow and the focused
regressions that currently matter most to the Phase 52 kernel work. 52d treats
these gates as first-class evidence for closing the phase.

## How This Builds on Earlier Phases

- Clarifies the handoff from 52a's manual restore stop-gap to 52b's task-owned
  return-state design
- Finishes the most important unfinished part of 52b: making return-state
  ownership actually match the design doc
- Extends 52c's `LineDiscipline`, `push_raw_input`, `IsrWakeQueue`, and growable
  IPC pools while correcting the roadmap claims that overshot the current code
- Reuses 43c's smoke/regression infrastructure as a release-quality gate instead
  of a best-effort debugging aid
- Prepares 53 (Headless Hardening) and 54 (Deep Serverization) to build on a
  truthful, testable kernel baseline

## Implementation Outline

1. Record the audited status of 52a/52b/52c in the roadmap docs and README
2. Add missing regression coverage for the exec-time signal-reset contract
3. Expand `UserReturnState` to match the actual resume contract needed by the
   syscall ABI
4. Snapshot return-critical state once at syscall entry before any blocking path
5. Make scheduler dispatch restore `syscall_user_rsp`, kernel stack/TSS state,
   `FS.base`, and CR3 from task-owned or task-associated state in one place
6. Wire `AddressSpace::generation` into mapping mutations and user-copy
   diagnostics so the dormant 52b instrumentation becomes active
7. Refactor `stdin_feeder` to scancode decode plus `push_raw_input`
8. Remove or quarantine the workaround-only termios return syscalls once the
   in-tree keyboard path no longer depends on them
9. Reconcile the scheduler/notification roadmap claims with the actual code by
   either finishing the missing implementation or explicitly re-deferring it
10. Close the phase only after the smoke and regression gates pass on the
    reference SMP4 workflow

## Acceptance Criteria

- Phase 52a/52b/52c docs and `docs/roadmap/README.md` explicitly distinguish
  delivered, superseded, partial, and deferred items
- `UserReturnState` is saved at syscall entry and scheduler dispatch restores
  the authoritative return-critical state without split ownership between
  `Task`, `Process`, and `PerCoreData`
- `AddressSpace::generation` is bumped on mapping changes and user-copy paths
  can detect or report mid-copy divergence
- `userspace/stdin_feeder` no longer reads termios flags or implements
  `ICANON`, `ISIG`, echo, `ICRNL`, or canonical-editing logic
- The keyboard input path uses `push_raw_input` and the kernel-side
  `LineDiscipline`
- The current scheduler/notification design is either implemented as documented
  or explicitly re-deferred in the roadmap with matching code comments
- The exec-time signal-reset behavior has explicit regression coverage
- `cargo xtask check` passes
- `cargo xtask smoke-test --timeout 180` passes
- `cargo xtask regression --timeout 90` passes

## Companion Task List

- [Phase 52d Task List](./tasks/52d-kernel-completion-and-roadmap-alignment-tasks.md)

## How Real OS Implementations Differ

- Linux, Redox, and seL4 keep syscall return-critical state with the task or TCB
  rather than splitting it across scheduler scratch and process metadata
- Mature terminal stacks do not keep a second line-discipline implementation in
  a helper daemon after introducing a kernel-side discipline
- Production kernels do not mark scalability or reliability phases complete
  while CI gates and roadmap text still disagree about the shipped model

## Deferred Until Later

- Full fair scheduling or EEVDF/CFS-style runtime accounting
- Cluster-aware or NUMA-aware work-stealing policy
- A growable ISR-safe notification pool if a sound design is not ready during
  52d
- Broader cleanup of compatibility/debugging syscalls that are not exercised by
  in-tree code after the Phase 52 closure work is complete
