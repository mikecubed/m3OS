# Phase 43a - Crash Diagnostics & Assertions

**Status:** Complete
**Source Ref:** phase-43a
**Depends on:** Phase 35 (True SMP Multitasking) ✅, Phase 43 (SSH Server) ✅
**Builds on:** Extends the panic handler from `kernel/src/main.rs` and fault handlers from `kernel/src/arch/x86_64/interrupts.rs`; uses the per-core data and scheduler infrastructure from Phase 35; uses IPC endpoint machinery from Phase 06; uses the fork/process model from Phase 11
**Primary Components:** panic_diag module, enriched fault handlers, debug_assert boundaries at scheduler/fork/IPC/stack transitions

## Milestone Goal

Every kernel crash is self-diagnosing. When the kernel panics or takes a fatal
fault, the serial output includes full CPU register state, current task metadata,
and per-core scheduler context. `debug_assert!` invariants at every scheduler,
fork, IPC, and stack boundary catch state corruption at the point of origin
rather than surfacing later as an opaque `RIP=0x4` fault.

## Why This Phase Exists

Phases 1--43 built a multi-user, networked, SMP OS -- but the crash diagnostics
remained minimal. The panic handler printed only file:line and message. Fault
handlers printed a one-line summary. When a task was dispatched with a zero
`saved_rsp` (causing the `RIP=0x4` pattern), the developer had no register dump,
no per-core state, and no way to tell which task or core was responsible. SMP race
bugs were especially opaque because the faulting core's state alone was
insufficient to diagnose the root cause on another core.

## Learning Goals

- Understand what information a kernel crash dump should contain and why each
  field matters (registers, task state, per-core run queues).
- Learn how deadlock-safe diagnostic output works (`try_lock()` + fallback serial)
  in a panic context where arbitrary locks may be held.
- See how `debug_assert!` invariants at state-machine boundaries catch corruption
  at origin rather than at symptom.
- Understand the x86_64 fault model: page faults, GPFs, and double faults, and
  how error codes encode segment selector information.
- Learn why stack bounds validation on every context switch is critical for SMP
  correctness.

## Feature Scope

### Enriched Panic Handler (Track A)

`kernel/src/panic_diag.rs` provides `dump_crash_context()` which prints three
diagnostic sections using the deadlock-safe `_panic_print` path:

1. **CPU Registers** -- RAX through R15, RFLAGS, CR2, CR3 captured via inline
   assembly. RIP is not directly capturable; the panic location (file:line)
   serves as the proxy.
2. **Current Task Info** -- reads `per_core().current_task_idx`; if valid, uses
   `try_lock_scheduler()` to print TaskId, TaskState, saved_rsp, pid,
   assigned_core, and priority. If the scheduler lock is held, prints a
   diagnostic message instead of deadlocking.
3. **Per-Core State** -- iterates all online cores (up to `MAX_CORES`), printing
   current_task_idx, reschedule flag, and run queue length (via `try_lock()`).
   The faulting core is marked with `>>>`.

### Enriched Fault Handlers (Track B)

Each fault handler now calls `dump_crash_context()` and prints additional
context before the existing action (kill trampoline for userspace, halt for
kernel):

- **Page fault (userspace):** prints RSP, task index/state/saved_rsp, then full
  crash context.
- **Page fault (kernel):** prints CR3, then full crash context. Output labeled
  as "KERNEL page fault".
- **GPF (userspace and kernel):** prints PID, task state, and decoded error code
  (selector index, table indicator, external flag), then full crash context.
- **Double fault:** prints IST RSP (to detect stack overflow), then full crash
  context.

### Scheduler Boundary Assertions (Track C)

`debug_assert!` checks at every scheduler state transition:

| Function | What it guards |
|---|---|
| `pick_next` | Task is Ready, affinity allows core, saved_rsp != 0 (idle task) |
| `yield_now` | Task index in bounds, scheduler RSP != 0 |
| `block_current` | Task was Running before block, scheduler RSP != 0 |
| `wake_task` | Task index in bounds after find |
| `run` | Task RSP != 0 before dispatch, state is Running after mark, switched task in bounds |
| `enqueue_to_core` | core_id < MAX_CORES |

### Fork Boundary Assertions (Track D)

| Function | What it guards |
|---|---|
| `make_fork_ctx` | user_rip != 0, user_rsp != 0, user_rip is canonical userspace |
| `fork_child_trampoline` | user_rip != 0, user_rsp != 0, page table exists |
| `sys_fork` | child_cr3 != 0, child in PROCESS_TABLE after insert |
| `spawn_fork_task` | fork_ctx present after set |

### IPC Boundary Assertions (Track E)

| Function | What it guards |
|---|---|
| `recv_msg` | Endpoint ID in range |
| `send` | wake_task succeeds for receiver |
| `reply` | wake_task succeeds for caller |

### Stack Bounds Validation (Track F)

| Check | Location |
|---|---|
| saved_rsp within task's kernel stack | Before dispatch in `run` |
| saved_rsp within stack after yield/block | After switch_context returns in `run` |
| Scheduler RSP != 0 | Top of dispatch loop in `run` |

## Important Components and How They Work

### `dump_crash_context()` (`kernel/src/panic_diag.rs`)

The central diagnostic function. Called from the panic handler and all fault
handlers. Captures registers via inline assembly, reads per-core data via
`per_core()` (guarded by `is_per_core_ready()`), and reads task state via
`try_lock_scheduler()`. All output goes through `_panic_print`, which creates
a fresh `SerialPort` if the global serial mutex is held -- ensuring output even
when panicking while holding the serial lock.

### `try_lock_scheduler()` (`kernel/src/task/mod.rs`)

A non-blocking wrapper around `SCHEDULER.try_lock()` that returns
`Option<MutexGuard<Scheduler>>`. Used by panic_diag and fault handlers to
safely inspect task state without risking a secondary deadlock. If the scheduler
is locked (e.g. the panic occurred during a context switch), the diagnostic
output notes "scheduler lock held" and continues.

### `Task::stack_bounds()` (`kernel/src/task/mod.rs`)

Returns `Option<(u64, u64)>` -- the base and top addresses of a task's kernel
stack allocation. Used by Track F assertions to validate that `saved_rsp` falls
within the expected range on every dispatch and every yield/block save.

## How This Builds on Earlier Phases

- Extends the panic handler from `kernel/src/main.rs` by calling
  `dump_crash_context()` after printing location and message
- Extends the fault handlers from Phase 04 (interrupts) by adding diagnostic
  output before the existing kill/halt actions
- Extends the scheduler from Phase 35 (SMP) with `debug_assert!` invariants at
  every state transition and stack validation on every context switch
- Extends the fork model from Phase 11 (process model) with assertions on the
  fork context publish/consume path
- Extends the IPC engine from Phase 06 with assertions on block/wake delivery
  integrity

## Implementation Outline

1. Create `kernel/src/panic_diag.rs` with `dump_crash_context()` (registers,
   task info, per-core state).
2. Update the panic handler in `kernel/src/main.rs` to call
   `dump_crash_context()`.
3. Add `try_lock_scheduler()` and `Scheduler::get_task()` for safe non-blocking
   scheduler access.
4. Enrich page fault, GPF, and double fault handlers to call
   `dump_crash_context()` with additional context.
5. Add `debug_assert!` checks in `pick_next`, `yield_now`, `block_current`,
   `wake_task`, `run`, and `enqueue_to_core`.
6. Add `debug_assert!` checks in `make_fork_ctx`, `fork_child_trampoline`,
   `sys_fork`, and `spawn_fork_task`.
7. Add `debug_assert!` checks in `recv_msg`, `send`, and `reply`.
8. Add `Task::stack_bounds()` and stack validation assertions in `run`.
9. Verify `cargo xtask check` and `cargo xtask test` pass.

## Acceptance Criteria

- `dump_crash_context()` prints registers, task info, and per-core state on
  every panic and fault using the deadlock-safe serial path
- All four fault handlers (page fault userspace/kernel, GPF, double fault) call
  `dump_crash_context()` and print additional context (RSP, CR3, error code)
- `debug_assert!` invariants present at every scheduler, fork, IPC, and stack
  boundary as specified in the task list
- `cargo xtask check` passes (clippy clean, rustfmt, 189 host tests)
- `cargo xtask test` passes (all 8 QEMU kernel tests)
- No `debug_assert` panics during normal boot

## Companion Task List

- [Phase 43a Task List](./tasks/43a-crash-diagnostics-tasks.md)

## How Real OS Implementations Differ

- Linux uses `printk` with structured log levels and a ring buffer (`dmesg`);
  panic output goes through a dedicated "oops" formatter that includes a full
  stack trace with DWARF-based symbol resolution.
- Linux's `BUG_ON()` / `WARN_ON()` macros include file:line and produce a full
  register dump, similar to this phase's `debug_assert!` + crash context
  approach but always-on (not debug-only).
- Production kernels use KASAN (address sanitizer), UBSAN (undefined behavior),
  and lockdep (lock ordering) -- much heavier instrumentation than the targeted
  assertions in this phase.
- Windows uses structured exception handling (SEH) with per-thread exception
  chains and writes crash dumps (minidump / full dump) to disk for post-mortem
  analysis.
- Real kernels support NMI-triggered register dumps across all cores
  simultaneously; m3OS captures registers only on the faulting core.

## Deferred Until Later

- Full lockdep-lite checker (Phase 43b or beyond)
- Allocator poisoning / redzones (KASAN-style, Phase 43b or beyond)
- Register dump via NMI (requires NMI handler work)
- Backtrace / stack unwinding (requires frame pointer chain or DWARF unwinder)
- Structured crash dump to disk for post-mortem analysis
