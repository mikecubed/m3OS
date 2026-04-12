# Kernel Completion and Roadmap Alignment

**Aligned Roadmap Phase:** Phase 52d
**Status:** Complete
**Source Ref:** phase-52d
**Supersedes Legacy Doc:** (none -- new content)

## Overview

Phase 52d closes the gap between the 52a-52c story and the code actually
shipping in the tree. It completes the task-owned syscall return-state path,
finishes the live keyboard convergence onto `push_raw_input`, re-baselines the
remaining scheduler/notification limits, fixes the late pre-login boot stall
that appeared on the integrated branch, and aligns the release gates and initrd
staging model with the real development workflow.

## What This Doc Covers

- Audit-backed closure of the 52a/52b/52c follow-on gaps
- Task-owned return state and generation-aware user-copy diagnostics
- Live keyboard-path convergence on the shared kernel line discipline
- Scheduler and notification limits that remain explicit after the audit
- Bootfix and signal-regression closure on the integrated Phase 52d branch
- Generated initrd staging and CI/xtask gate alignment

## Core Implementation

### Audit-backed closure

Phase 52d updates the roadmap and task docs so the 52-series tells the shipped
story plainly:

- 52a is the historical bug-fix stop-gap
- 52b is the structural hardening phase
- 52c introduces the larger architectural pieces
- 52d closes the remaining implementation/documentation mismatch and records the
  intentional deferrals honestly

### Task-owned return state and generation tracking

`kernel/src/arch/x86_64/syscall/mod.rs` snapshots `UserReturnState` once at
syscall entry. `kernel/src/task/scheduler.rs` restores `user_rsp`,
`kernel_stack_top`, `fs_base`, and `cr3_phys` from `Task.user_return` for
resumed userspace tasks, so the scheduler owns the live resume contract instead
of split per-core scratch.

`kernel/src/mm/user_mem.rs` and `kernel/src/mm/mod.rs` also activate
`AddressSpace::generation` end to end: mapping-mutating operations bump the
counter, and user-copy helpers report divergence if a concurrent mapping change
races with the copy loop.

### Keyboard-path convergence

`userspace/stdin_feeder/src/main.rs` no longer reads termios flags or implements
canonical editing itself. It looks up the `kbd` service, receives raw scancodes
over IPC, translates only to bytes or VT100 escape sequences, and forwards them
via `push_raw_input`.

`kernel-core/src/tty.rs` remains the single live line-discipline implementation
for both serial and keyboard input. Canonical editing, echo, signal generation,
and escape-sequence filtering all happen there rather than in a second
userspace-only policy clone.

### Bootfix and release-gate closure

The integrated Phase 52d branch exposed a new boot stall after
`init: / mounted (ext2)`. The immediate hang was in `sys_rt_sigaction`, but the
deeper issue was lock reentry: hot user-copy and generation-reporting paths
could consult `PROCESS_TABLE` while a syscall already held it.

The closure fix was two-part:

1. `kernel/src/mm/user_mem.rs` now uses the per-core current address-space
   pointer in its hot copy/report paths instead of reentering `PROCESS_TABLE`.
2. `sys_rt_sigaction` copies the new action in before locking, copies `oldact`
   out while holding the lock, and mutates the disposition only after that copy
   succeeds, preserving atomic `-EFAULT` semantics.

`userspace/signal-test/signal-test.c` now covers both exec-time signal reset and
raw `rt_sigaction` atomicity, while `xtask` and CI agree on the same
`check`/smoke/regression closure surface.

### Generated initrd staging

Before Phase 52d closure, ordinary validation regenerated tracked binaries under
`kernel/initrd/`, creating noisy source-tree diffs. Generated payloads now stage
under `target/generated-initrd/`, while `kernel/initrd/` keeps only
source-owned static assets. `kernel/src/fs/ramdisk.rs` uses separate inclusion
macros for the two categories, so compile-time embedding still works without
tracking rebuild artifacts.

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/task/mod.rs` | `UserReturnState` definition and task-owned resume contract |
| `kernel/src/task/scheduler.rs` | Authoritative restore path for resumed userspace tasks |
| `kernel/src/mm/user_mem.rs` | User-copy helpers with generation reporting and per-core address-space lookup |
| `kernel/src/arch/x86_64/syscall/mod.rs` | Syscall-entry snapshot, `push_raw_input`, and `sys_rt_sigaction` bootfix |
| `kernel-core/src/tty.rs` | Shared kernel line discipline and escape-sequence filtering |
| `userspace/stdin_feeder/src/main.rs` | IPC scancode bridge that forwards bytes via `push_raw_input` |
| `userspace/signal-test/signal-test.c` | Exec-reset and `rt_sigaction` atomicity regression coverage |
| `xtask/src/main.rs` | Smoke/regression definitions and generated-initrd staging helpers |
| `kernel/src/fs/ramdisk.rs` | Static-vs-generated initrd asset embedding split |
| `.github/workflows/build.yml` | Build workflow gates aligned with Phase 52d closure |
| `.github/workflows/pr.yml` | PR workflow gates aligned with Phase 52d closure |

## How This Phase Differs From Later Hardening Work

- Phase 52d is a closure and truth-alignment phase, not a new subsystem
  invention phase.
- True lock-free per-core dispatch is still later work even though per-core run
  queues, work-stealing, and migration cooldowns are already live.
- Notifications remain fixed-size for ISR safety; a growable ISR-safe design is
  still deferred.
- The register-return termios helpers remain only as deprecated compatibility
  shims for diagnostics or out-of-tree code.
- Deep service extraction and headless hardening still belong to later phases
  such as 53 and 54.

## Related Roadmap Docs

- [Phase 52d roadmap doc](./roadmap/52d-kernel-completion-and-roadmap-alignment.md)
- [Phase 52d task doc](./roadmap/tasks/52d-kernel-completion-and-roadmap-alignment-tasks.md)
- [Phase 52c -- Kernel Architecture Evolution](./52c-kernel-architecture-evolution.md)
- [Phase 52b -- Kernel Structural Hardening](./52b-kernel-structural-hardening.md)
- [Phase 52a -- Kernel Reliability Fixes](./52a-kernel-reliability-fixes.md)
- [Phase 52 -- First Service Extractions](./52-first-service-extractions.md)

## Deferred or Later-Phase Topics

- True lock-free per-core dispatch
- Growable ISR-safe notification allocation
- Broader cleanup of deprecated compatibility/debugging syscalls
- Further service extraction and headless hardening beyond the first visible
  console/keyboard boundary
