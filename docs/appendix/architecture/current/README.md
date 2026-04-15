# m3OS Current Architecture Reference

**Version:** Kernel v0.54.0 (Phase 54)
**Date:** 2026-04-13
**Purpose:** Detailed documentation of the current m3OS kernel architecture, focusing on subsystems identified in the `copy_to_user` reliability bug investigation and the SSHD hang analysis.

## Motivation

Two independent bug investigations exposed structural weaknesses across multiple kernel subsystems:

1. **`copy_to_user` intermittent reliability bug** (`docs/appendix/copy-to-user-reliability-bug.md`) — kernel writes correct data to a user buffer via the physical offset mapping, but userspace reads stale/zeroed values from the same virtual address. The root cause traces to an earlier address-space mapping divergence, not the copy primitive itself.

2. **SSHD post-authentication hang** (`docs/appendix/sshd-hang-analysis.md`) — the SSH server's multi-task async model stalls when PTY output encounters channel backpressure, due to missing write-side wakeup registration and a vendored library bug.

These bugs are symptoms of deeper architectural patterns that warrant systematic documentation and improvement. This reference captures the current state of each affected subsystem with enough detail to support the proposed changes in `docs/appendix/architecture/next/`.

## Document Index

| # | Document | Subsystem | Key Concerns |
|---|---|---|---|
| 01 | [Memory Management](01-memory-management.md) | Page tables, copy_to_user, TLB, frame allocator, address spaces | No AddressSpace object, direct-mapping writes, missing SMP shootdowns, no frame zeroing |
| 02 | [Process and Context](02-process-context.md) | Process struct, syscall return state, per-CPU vs per-task state | Stale `syscall_user_rsp` on blocking paths, manual `restore_caller_context` requirement |
| 03 | [IPC and Wakeups](03-ipc-and-wakeups.md) | IPC engine, notifications, blocking paths, wakeup contracts | IPC blocking paths miss restore, single-waiter notifications, hard limits |
| 04 | [Scheduler and SMP](04-scheduler-smp.md) | Scheduler, SMP boot, per-CPU data, TLB shootdown protocol | No involuntary preemption, disabled load balancing, serialized shootdowns |
| 05 | [Terminal and PTY](05-terminal-pty.md) | TTY, PTY, termios, console input path | Duplicated line discipline, copy_to_user workarounds, fixed PTY pool |
| 06 | [Async I/O Model](06-async-io-model.md) | Async executor, sunset SSH integration, poll/select/epoll | Write-side wakeup bug, cooperative scheduling limitations |

## Cross-Cutting Themes

These themes recur across multiple subsystems and are the primary targets for the "next" architecture:

### 1. Address Space Identity is Implicit

The kernel has no first-class `AddressSpace` object. A process's address space is identified solely by `page_table_root: Option<PhysAddr>` (the CR3 value). There is no per-CPU tracking of which address space is active beyond the hardware CR3 register and `current_pid`. This makes it impossible to answer "which address space does this CPU believe is current?" without reading CR3 directly.

**Affected subsystems:** Memory Management, Process Context, Scheduler

### 2. Per-Core Mutable Scratch as Return State

Syscall return-critical state (`syscall_user_rsp`, `syscall_stack_top`, FS.base) lives in mutable per-core `PerCoreData` fields, not in the task/process structure. When a task blocks and another task runs on the same core, the per-core fields are overwritten. Every blocking syscall handler must manually call `restore_caller_context()` before returning to userspace. Missing this call is the proven root cause of the stale-`syscall_user_rsp` bug.

**Affected subsystems:** Process Context, IPC, Scheduler

### 3. TLB Coherence Gaps on SMP

TLB invalidation is inconsistent across operations:
- `munmap` and `mprotect` issue per-page SMP shootdowns (correct but slow)
- `fork` CoW marking uses local CR3 reload only (no SMP shootdown)
- Demand paging uses local `invlpg` only
- CoW fault resolution uses local `invlpg` only

The single-address shootdown protocol (`SHOOTDOWN_LOCK` + one IPI per page) serializes all shootdowns and becomes O(pages) for bulk operations.

**Affected subsystems:** Memory Management, Scheduler/SMP

### 4. No Separation Between Policy and Mechanism in Terminal Input

The line discipline (canonical editing, signal generation, echo) is implemented twice: once in the kernel (`serial_stdin_feeder_task`) and once in userspace (`stdin_feeder`). Both share `TTY0.termios` state. Changes to line discipline logic must be applied in both places, and the `copy_to_user` bug forced the userspace implementation to use register-return workaround syscalls.

**Affected subsystems:** Terminal/PTY

### 5. Hard-Coded Resource Limits

| Resource | Limit | Location |
|---|---|---|
| IPC endpoints | 16 | `kernel/src/ipc/endpoint.rs` |
| Notifications | 16 | `kernel/src/ipc/notification.rs` |
| Services | 16 | `kernel-core/src/ipc/registry.rs` |
| PTY pairs | 16 | `kernel-core/src/pty.rs` |
| FDs per process | 32 | `kernel/src/process/mod.rs` |
| Capability slots | 64 | `kernel-core/src/ipc/capability.rs` |

**Affected subsystems:** IPC, Terminal/PTY, Process Context

## How to Read These Documents

Each subsystem document follows the same structure:

1. **Overview** — what the subsystem does and its role in the kernel
2. **Data Structures** — complete struct definitions with field descriptions
3. **Algorithms** — step-by-step descriptions of key operations with Mermaid diagrams
4. **Data Flow** — how data moves through the subsystem (Mermaid sequence/flowchart diagrams)
5. **Known Issues** — bugs, design limitations, and missing features with evidence
6. **Comparison Points** — what to compare against in external microkernels (detailed comparisons are in `docs/appendix/architecture/next/`)

## Source References

All claims in these documents reference specific source files and line numbers in the m3OS codebase at commit `1d7a49c` (HEAD of `feat/phase-52`). External kernel comparisons are documented with URLs and commit references in `docs/appendix/architecture/next/sources.md`.
