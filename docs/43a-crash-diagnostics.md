# Phase 43a — Crash Diagnostics & Assertions

**Aligned Roadmap Phase:** Phase 43a
**Status:** Complete
**Source Ref:** phase-43a
**Depends on:** Phase 35 (True SMP Multitasking), Phase 43 (SSH Server)

## Overview

Phase 43a makes every kernel crash self-diagnosing. The panic handler and all
fault handlers now print full CPU register state, current task metadata, and
per-core scheduler context. `debug_assert!` invariants at scheduler, fork, IPC,
and stack boundaries catch state corruption at the point of origin rather than
surfacing later as an opaque fault.

## What This Doc Covers

- How the enriched panic handler captures and prints diagnostic context
- How fault handlers (page fault, GPF, double fault) produce crash dumps
- What `debug_assert!` invariants were added and where
- How to read and interpret a crash dump
- How stack bounds validation works

## Core Implementation

### Crash Diagnostics Module (`kernel/src/panic_diag.rs`)

The central function `dump_crash_context()` is called from the panic handler
and all fault handlers. It prints three diagnostic sections to the serial port:

**1. CPU Registers** — captured via inline assembly at the point of the panic
or fault call. Each general-purpose register (RAX through R15), RFLAGS, CR2
(faulting address), and CR3 (active page table) is printed as a 16-digit hex
value. RIP cannot be directly captured from inline assembly — the panic
location (file:line) printed by the caller serves as the proxy.

**2. Current Task Info** — reads the per-core `current_task_idx` atomic. If a
task is active (index >= 0), acquires the scheduler lock via `try_lock()` and
prints: TaskId, TaskState, saved_rsp, PID, assigned_core, and priority. If the
scheduler lock is already held (common when panicking during a context switch),
prints "scheduler lock held -- skipping task dump" instead of deadlocking.

**3. Per-Core State** — iterates all online cores (up to `MAX_CORES = 16`) and
prints: current_task_idx, reschedule flag, and run queue length. Each core's
run queue uses `try_lock()` to avoid deadlocks. The faulting core is marked
with `>>>` for quick identification.

All output uses the deadlock-safe `_panic_print` path from `kernel/src/serial.rs`,
which creates a fresh `SerialPort` if the global serial mutex is held.

### Enriched Fault Handlers (`kernel/src/arch/x86_64/interrupts.rs`)

Each fault handler now prints additional context before its existing action
(kill trampoline redirect for userspace faults, halt loop for kernel faults):

| Handler | Additional Output | Then |
|---|---|---|
| Page fault (userspace) | RSP, task index/state/saved_rsp, full crash context | Kill trampoline |
| Page fault (kernel) | CR3, full crash context | Halt loop |
| GPF (userspace) | PID, task state, decoded error code, full crash context | Kill trampoline |
| GPF (kernel) | Decoded error code, full crash context | Halt loop |
| Double fault | IST RSP, full crash context | Halt loop |

**GPF error code decoding**: The x86 GPF error code encodes a segment selector
reference: `selector_idx = err >> 3`, `table = (err >> 1) & 3` (0=GDT, 1=IDT,
2=LDT, 3=IDT), `external = err & 1`.

### Scheduler Boundary Assertions (`kernel/src/task/scheduler.rs`)

`debug_assert!` checks added at every scheduler state transition:

| Function | Assertion | What it catches |
|---|---|---|
| `pick_next` | state == Ready | Dispatching a blocked/dead task |
| `pick_next` | affinity_mask & core_bit != 0 | Wrong-core dispatch |
| `pick_next` (idle) | saved_rsp != 0 | Idle task with corrupt stack |
| `yield_now` | idx < tasks.len() | Out-of-bounds task index |
| `yield_now` | sched_rsp != 0 | Uninitialized scheduler stack |
| `block_current` | state == Running | Double-block or block-after-dead |
| `block_current` | sched_rsp != 0 | Uninitialized scheduler stack |
| `wake_task` | idx < tasks.len() | Invalid task index from find() |
| `run` | task_rsp != 0 | Dispatching with zero stack pointer |
| `run` | state == Running | State not set after mark |
| `run` | sidx < tasks.len() | Invalid switched-out task index |
| `enqueue_to_core` | core_id < MAX_CORES | Out-of-range core ID |

### Fork Boundary Assertions (`kernel/src/process/mod.rs`, `kernel/src/arch/x86_64/syscall.rs`)

| Function | Assertion | What it catches |
|---|---|---|
| `make_fork_ctx` | user_rip != 0 | Zero instruction pointer |
| `make_fork_ctx` | user_rsp != 0 | Zero stack pointer |
| `make_fork_ctx` | user_rip < 0x8000_0000_0000 | Kernel address leaking to userspace |
| `fork_child_trampoline` | ctx.user_rip != 0 | Corrupted fork context |
| `fork_child_trampoline` | ctx.user_rsp != 0 | Corrupted fork context |
| `fork_child_trampoline` | cr3_phys.is_some() | Missing page table for child |
| `sys_fork` | child_cr3 != 0 | Zero page table frame |
| `sys_fork` | child in PROCESS_TABLE | Missing process entry |
| `spawn_fork_task` | fork_ctx.is_some() | Context not stored in task |

### IPC Boundary Assertions (`kernel/src/ipc/endpoint.rs`)

| Function | Assertion | What it catches |
|---|---|---|
| `recv_msg` | ep_id < MAX_ENDPOINTS | Out-of-range endpoint ID |
| `send` | wake_task succeeded | Lost wakeup on send |
| `reply` | wake_task succeeded | Lost wakeup on reply |

### Stack Bounds Validation (`kernel/src/task/scheduler.rs`, `kernel/src/task/mod.rs`)

`Task::stack_bounds()` returns `(base, top)` of the task's kernel stack
allocation (`KERNEL_STACK_SIZE = 32 KiB`). Three validation points:

1. **Before dispatch** — `task_rsp` must fall within `[base, top]` before
   `switch_context` is called.
2. **After yield/block** — the RSP saved by `switch_context` is validated
   immediately, catching stack overflow at the point it happens.
3. **Scheduler RSP** — each core's scheduler RSP is checked for non-zero at
   the top of every dispatch loop iteration.

## Reading a Crash Dump

Example output on a kernel panic:

```
KERNEL PANIC at kernel/src/task/scheduler.rs:142
  assertion `left == right` failed
=== CRASH DIAGNOSTICS ===
--- CPU Registers ---
RAX=0x0000000000000004  RBX=0x0000000000000001
RCX=0xffff80000000a230  RDX=0x0000000000000000
RSI=0x0000000000000003  RDI=0xffff800000012000
RBP=0xffff80000001ff80  RSP=0xffff80000001fe00
R8 =0x0000000000000000  R9 =0x0000000000000000
R10=0x0000000000000000  R11=0x0000000000000202
R12=0x0000000000000000  R13=0x0000000000000000
R14=0x0000000000000000  R15=0x0000000000000000
RFLAGS=0x0000000000000202
CR2=0x0000000000000004  CR3=0x0000000000100000
--- Current Task ---
  task_idx=3 on core 0
  TaskId=4 state=Running saved_rsp=0x0000000000000000
  pid=2 assigned_core=0 priority=20
--- Per-Core State ---
>>> core 0 | online=true task_idx=3 resched=false run_queue=2
    core 1 | online=true task_idx=-1 resched=true run_queue=0
    core 2 | online=true task_idx=5 resched=false run_queue=1
    core 3 | online=true task_idx=-1 resched=false run_queue=0
=== END CRASH DIAGNOSTICS ===
```

**How to read it:**

- **CR2** — the address that caused a page fault (if the panic followed one)
- **CR3** — identifies which process's page table is active
- **saved_rsp=0x0** — this task has a zero stack pointer, which is the root
  cause of the `RIP=0x4` crash pattern
- **`>>>` marker** — identifies the faulting core
- **task_idx=-1** — that core is in its scheduler loop (no user task running)
- **"scheduler lock held"** — the panic occurred while the scheduler was locked;
  task info is unavailable but per-core atomics still print

## Key Files

| File | Purpose |
|---|---|
| `kernel/src/panic_diag.rs` | Crash diagnostics module: register capture, task dump, per-core dump |
| `kernel/src/main.rs` | Panic handler calls `dump_crash_context()` |
| `kernel/src/arch/x86_64/interrupts.rs` | Enriched page fault, GPF, double fault handlers |
| `kernel/src/task/scheduler.rs` | Scheduler assertions (C.1–C.6) and stack validation (F.1–F.3) |
| `kernel/src/task/mod.rs` | `try_lock_scheduler()`, `Task::stack_bounds()` |
| `kernel/src/process/mod.rs` | Fork context assertions (D.1–D.2) |
| `kernel/src/arch/x86_64/syscall.rs` | `sys_fork` assertions (D.3) |
| `kernel/src/ipc/endpoint.rs` | IPC assertions (E.1–E.4) |

## How This Phase Differs From Later Debugging Work

- This phase adds targeted `debug_assert!` invariants and crash dump formatting.
  It does not add runtime tracing, profiling, or continuous monitoring.
- Phase 43b will add a per-core lockless trace ring that records scheduler
  events, IPC messages, and interrupt entries. `dump_crash_context()` will be
  extended to dump the trace ring on crash.
- Phase 43c will add regression and stress testing infrastructure (xtask
  commands, CI tiers, property-based testing with proptest/loom).
- Full lockdep-lite lock ordering validation is deferred beyond Phase 43b.
- KASAN-style allocator poisoning and redzones are deferred.
- Stack unwinding / backtrace support requires frame pointer chains or a DWARF
  unwinder and is deferred.

## Related Roadmap Docs

- [Phase 43a roadmap doc](./roadmap/43a-crash-diagnostics.md)
- [Phase 43a task doc](./roadmap/tasks/43a-crash-diagnostics-tasks.md)

## Known Limitations

- **Registers reflect capture point, not fault point** — inline assembly captures
  registers inside `dump_crash_context()`, not at the original panic/fault site.
  Callee-saved registers (RBX, RBP, R12–R15) are likely preserved; caller-saved
  registers may have been clobbered by the call chain.
- **No backtrace** — only the panic location (file:line) is available, not a full
  call stack. Stack unwinding requires frame pointers or DWARF info.
- **Single-core register capture** — only the faulting core's registers are
  captured. Other cores continue running and their register state is not
  snapshotted (would require NMI-based cross-core interrupts).
- **debug_assert only in debug builds** — Track C–F assertions are compiled out
  in release builds. Track A–B diagnostics are always-on (they only execute on
  fatal paths).
