# Phase 52d - Kernel Completion and Roadmap Alignment

**Status:** Complete
**Source Ref:** phase-52d
**Depends on:** Phase 52a (Kernel Reliability Fixes) ✅, Phase 52b (Kernel Structural Hardening) ✅, Phase 52c (Kernel Architecture Evolution) ✅
**Builds on:** Audits the claimed outcomes of Phases 52a-52c, finishes the release-critical pieces that remained partial, closes the late boot blocker exposed on the integrated 52d branch, and explicitly re-baselines any scalability work that was designed but not actually landed.
**Primary Components:** docs/roadmap/52a-kernel-reliability-fixes.md, docs/roadmap/52b-kernel-structural-hardening.md, docs/roadmap/52c-kernel-architecture-evolution.md, docs/roadmap/README.md, docs/roadmap/tasks/52d-kernel-completion-and-roadmap-alignment-tasks.md, kernel/src/task/mod.rs, kernel/src/task/scheduler.rs, kernel/src/mm/user_mem.rs, kernel/src/arch/x86_64/syscall/mod.rs, kernel/src/ipc/notification.rs, kernel-core/src/tty.rs, userspace/stdin_feeder/src/main.rs, userspace/signal-test/signal-test.c, kernel/src/fs/ramdisk.rs, xtask/src/main.rs, .github/workflows/build.yml, .github/workflows/pr.yml

## Milestone Goal

The Phase 52 follow-on work now matches reality: the roadmap truthfully
describes what landed, syscall return state is task-owned end-to-end, keyboard
input uses the kernel line discipline instead of a ring-3 clone, the integrated
boot/login path survives the former `rt_sigaction` stall, generated initrd
payloads no longer dirty the source tree, and the smoke/regression gates for
login, fork, ion, and PTY flows are trustworthy again.

## Why This Phase Exists

An audit of Phases 52a, 52b, and 52c found that the codebase and the roadmap
drifted apart in a few important ways:

1. **Phase 52a is historically correct but underspecified after later work.**
   The manual `restore_caller_context` stop-gap was a valid fix, but it was
   immediately superseded by the 52b task-owned return-state direction and the
   roadmap never recorded that handoff clearly.

2. **Phase 52b had landed the structure of task-owned return state, but not the
   full contract.** `UserReturnState` existed, yet the implementation still
   saved state at block points, restored only part of the resume path from the
   task, and left `kernel_stack_top` / `fs_base` split between `Task`,
   `Process`, and per-core scratch.

3. **Phase 52c had landed kernel-side line-discipline infrastructure, but the
   live keyboard path still duplicated that logic in userspace.**
   `push_raw_input` and `LineDiscipline` existed, while
   `userspace/stdin_feeder` still read termios flags and implemented `ICANON`,
   `ISIG`, echo, and canonical editing itself.

4. **Some 52c scalability claims had been marked complete before the code
   matched them.** The scheduler still relied on the global `SCHEDULER` lock in
   the dispatch path, and notifications were still backed by fixed-size arrays
   with `MAX_NOTIFS` rather than a fully growable pool.

5. **Validation and branch hygiene became part of the kernel closure work.**
   The integrated `feat/phase-52d` branch exposed a boot-time deadlock before
   `login:`, and generated initrd payloads were still tracked in-tree so routine
   validation dirtied the source tree.

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
task-owned return-state contract. The shipped tree now snapshots the relevant
state once at syscall entry, restores resumed userspace tasks from one place
during dispatch, and keeps `Task.user_return` as the single authoritative resume
contract for post-syscall tasks.

### Keyboard input path convergence

Make the keyboard path use the kernel-side `LineDiscipline` that 52c already
introduced. `stdin_feeder` is now a scancode-decoder and raw-byte forwarder via
`push_raw_input`, not a second terminal stack with its own termios policy.

### Re-baselined scalability claims

Where 52c claimed more than the code currently provides, 52d either completes
the missing work or explicitly moves the unshipped design back into a later
phase. The goal is that roadmap text, code comments, and implementation agree
about the scheduler and notification model.

**Phase 52d Track D resolution:**

- **Scheduler:** The global `SCHEDULER` lock remains on the dispatch hot path.
  Per-core run queues, work-stealing, load-balancing with migration cooldown,
  and dead-slot recycling are all active and provide dispatch locality. But
  task state transitions (marking Running, saving RSP, clearing switching_out)
  still require the global lock. True lock-free per-core dispatch — where the
  hot path never acquires the global lock — is explicitly deferred because it
  requires splitting the `tasks` vec into per-core ownership or a lock-free
  task registry, which is a larger change than 52c/52d scope.

- **Notifications:** The notification pool remains fixed-size (`MAX_NOTIFS = 64`)
  because `PENDING` and `ISR_WAITERS` must be ISR-safe (lock-free atomics only).
  A growable pool would require allocation or lock-based indirection that is not
  safe in interrupt context. The fixed-size constraint is documented in
  `kernel/src/ipc/notification.rs` with exhaustion diagnostics (warning at 75%
  capacity, warning on full exhaustion). 64 slots cover foreseeable demand.

### Validation, bootfix, and artifact-hygiene closure

Repair the Phase 52 smoke/regression flows so failures in login, fork, ion
prompt, PTY, and signal-reset paths are diagnosable and meaningful. On the
integrated branch, this also required fixing the boot-time `rt_sigaction` stall
caused by lock-reentering user-copy paths and moving generated initrd payloads
out of `kernel/initrd/` into `target/generated-initrd/` so validation no longer
creates source-tree noise.

## Important Components and How They Work

### `UserReturnState` and scheduler dispatch

`kernel/src/task/mod.rs`, `kernel/src/task/scheduler.rs`, and
`kernel/src/arch/x86_64/syscall/mod.rs` now treat `UserReturnState` as the
authoritative resume contract. `syscall_handler` snapshots it once at entry, and
the scheduler restores `user_rsp`, `kernel_stack_top`, `fs_base`, and `cr3_phys`
from that struct for resumed userspace tasks.

### `user_mem` and `rt_sigaction` boot-blocker closure

`kernel/src/mm/user_mem.rs` now uses a per-core-only address-space helper in the
hot user-copy and generation-tracking paths, so those loops no longer reenter
`PROCESS_TABLE` while other syscalls hold it. `sys_rt_sigaction` copies the new
action in before locking, copies `oldact` out while holding the lock, and mutates
the disposition only after the copy succeeds, restoring both boot progress and
atomic `-EFAULT` behavior.

### `LineDiscipline`, `push_raw_input`, and `stdin_feeder`

`kernel-core/src/tty.rs` and `kernel/src/arch/x86_64/syscall/mod.rs` contain the
kernel-side line-discipline path, and `userspace/stdin_feeder` is now reduced to
scancode decoding plus `push_raw_input`. The special register-return termios
helpers remain only as deprecated diagnostic compatibility shims; the in-tree
keyboard path no longer depends on them.

### Scheduler and notification scope

`kernel/src/task/scheduler.rs` and `kernel/src/ipc/notification.rs` currently
mix real 52c improvements (`IsrWakeQueue`, load-balance cooldowns, growable
endpoints/caps, per-core run queues, work-stealing) with still-global or
fixed-size behavior (global `SCHEDULER` lock on dispatch, `MAX_NOTIFS = 64`
fixed-size notification pool). 52d Track D reconciles these by:

- Rewriting the scheduler module-level doc to truthfully describe what the
  global lock covers and what per-core infrastructure provides
- Documenting the fixed-size notification constraint with ISR-safety rationale
- Adding exhaustion diagnostics to `try_create`
- Correcting the 52c acceptance criteria and task checkboxes
- Explicitly deferring true lock-free dispatch and growable notifications

### `xtask` smoke/regression harness and generated initrd staging

`xtask/src/main.rs` defines the boot/login/shell smoke flow and the focused
regressions that currently matter most to the Phase 52 kernel work. 52d treats
these gates as first-class evidence for closing the phase, adds `kbd-echo`,
switches PTY coverage to `pty-test --quick`, and stages generated initrd
payloads under `target/generated-initrd/` so `kernel/src/fs/ramdisk.rs` can
embed them without tracking rebuild noise in git.

## How This Builds on Earlier Phases

- Clarifies the handoff from 52a's manual restore stop-gap to 52b's task-owned
  return-state design
- Finishes the most important unfinished part of 52b by making the task-owned
  resume contract and generation diagnostics match the design doc
- Extends 52c's `LineDiscipline`, `push_raw_input`, `IsrWakeQueue`, and growable
  IPC pools while correcting the roadmap claims that overshot the current code
- Reuses 43c's smoke/regression infrastructure as a release-quality gate instead
  of a best-effort debugging aid
- Prepares 53 (Headless Hardening) and 54 (Deep Serverization) to build on a
  truthful, testable kernel baseline with a clean initrd staging model

## Implementation Outline

1. Recorded the audited status of 52a/52b/52c in the roadmap docs and README
2. Added missing regression coverage for the exec-time signal-reset contract and
   the `rt_sigaction` atomicity case
3. Expanded `UserReturnState` to match the actual resume contract needed by the
   syscall ABI
4. Snapshotted return-critical state once at syscall entry before any blocking
   path
5. Made scheduler dispatch restore `syscall_user_rsp`, kernel stack/TSS state,
   `FS.base`, and CR3 from the task-owned resume contract for resumed userspace
   tasks
6. Wired `AddressSpace::generation` into mapping mutations and user-copy
   diagnostics so the dormant 52b instrumentation became active
7. Refactored `stdin_feeder` to scancode decode plus `push_raw_input`
8. Quarantined the workaround-only termios return syscalls as deprecated
   compatibility helpers after the in-tree keyboard path stopped using them
9. Reconciled the scheduler/notification roadmap claims with the actual code by
   explicitly re-deferring the still-unshipped pieces
10. Fixed the integrated-branch boot blocker in `user_mem` / `rt_sigaction` and
    moved generated initrd payloads into `target/generated-initrd/`
11. Closed the phase after `check`, smoke, and regression passed on the Phase 52d
    reference workflow

## Acceptance Criteria

- Phase 52a/52b/52c docs and `docs/roadmap/README.md` explicitly distinguish
  delivered, superseded, partial, and deferred items
- `UserReturnState` is saved at syscall entry and scheduler dispatch restores
  the authoritative return-critical state for resumed userspace tasks without
  split ownership in the live return path
- `AddressSpace::generation` is bumped on mapping changes and user-copy paths
  can detect or report mid-copy divergence
- `userspace/stdin_feeder` no longer reads termios flags or implements
  `ICANON`, `ISIG`, echo, `ICRNL`, or canonical-editing logic
- The keyboard input path uses `push_raw_input` and the kernel-side
  `LineDiscipline`
- The integrated boot path reaches `login:` again after the former
  `sys_rt_sigaction` stall, and the raw-syscall regression proves the
  `oldact`/mutation ordering remains atomic on `-EFAULT`
- The current scheduler/notification design is either implemented as documented
  or explicitly re-deferred in the roadmap with matching code comments
  *(Track D: global scheduler lock re-deferred with truthful code/doc comments;
  fixed-size notification pool documented with ISR-safety rationale and
  exhaustion diagnostics)*
- The exec-time signal-reset behavior has explicit regression coverage
- The keyboard input path (serial→TTY→stdin→shell) has explicit regression
  coverage (`kbd-echo`)
- Generated initrd payloads are staged under `target/generated-initrd/`, while
  `kernel/initrd/` retains only source-owned static assets
- `cargo xtask check` passes
- `cargo xtask smoke-test --timeout 180` passes
- `cargo xtask regression --timeout 90` passes (fork-overlap, ipc-wake,
  pty-overlap, signal-reset, kbd-echo)

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
- **True per-core scheduling** (lock-free dispatch hot path) — per-core run
  queues and work-stealing landed in 52c, but the global `SCHEDULER` lock is
  still acquired on every dispatch iteration for task state reads and
  transitions; splitting task ownership per-core requires a larger
  architectural change deferred past Phase 52
- **Growable ISR-safe notification pool** — a sound design (two-level: fixed
  ISR-visible fast table + growable overflow) exists conceptually but is not
  needed at current scale (`MAX_NOTIFS = 64` covers foreseeable demand);
  exhaustion diagnostics are in place
- Broader cleanup of compatibility/debugging syscalls that are not exercised by
  in-tree code after the Phase 52 closure work is complete
