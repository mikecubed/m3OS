# Phase 43a - Crash Diagnostics & Assertions

**Status:** Complete
**Source Ref:** phase-43a
**Depends on:** Phase 35 (True SMP Multitasking) ✅, Phase 43 (SSH Server) ✅
**Builds on:** Uses the scheduler, per-core data, and IPC infrastructure from Phases 35 and 06; extends the panic handler and fault handlers from the interrupt subsystem
**Primary Components:** panic_diag module, enriched fault handlers, debug_assert boundaries at scheduler/fork/IPC/stack transitions

Phase 43a is a debugging-infrastructure phase. It adds no new user-visible
functionality -- only diagnostic output visible to the developer on crash or
panic, and debug_assert invariants that catch state corruption at the point of
origin rather than surfacing later as an opaque fault.

## Why This Phase Exists

The kernel's panic handler previously printed only file:line and message, with
no register state, task info, or per-core context. Fault handlers (page fault,
GPF, double fault) printed minimal information. When a crash occurred (e.g. the
`RIP=0x4` pattern from a stale `saved_rsp`), the developer had to guess which
task, core, and register values were involved.

## Feature Scope

### Enriched Panic Handler (Track A)

`kernel/src/panic_diag.rs` provides `dump_crash_context()` which prints:

- **CPU Registers:** RAX-R15, RFLAGS, CR2, CR3 captured via inline assembly
- **Current Task Info:** task index, TaskId, TaskState, saved_rsp, pid, assigned_core, priority
- **Per-Core State:** for all online cores: task_idx, reschedule flag, run queue length

All output uses the deadlock-safe `_panic_print` path. All locks use `try_lock()`
to avoid secondary deadlocks during panic.

### Enriched Fault Handlers (Track B)

- **Page fault (userspace):** prints RSP, task state, then full crash context
- **Page fault (kernel):** prints CR3, then full crash context
- **GPF:** prints task state, decoded error code (selector index, table, external flag), then full crash context
- **Double fault:** prints IST RSP, then full crash context

Existing behavior (kill trampoline redirect for userspace, halt for kernel) is
preserved -- diagnostics are printed before the existing action.

### Scheduler Boundary Assertions (Track C)

`debug_assert!` checks at every scheduler state transition:

| Function | What it guards |
|---|---|
| `pick_next` | Task is Ready, affinity allows core, saved_rsp != 0 |
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
| `recv_msg` | endpoint ID in range |
| `send` | wake_task succeeds for receiver |
| `reply` | wake_task succeeds for caller |

### Stack Bounds Validation (Track F)

| Check | Where |
|---|---|
| saved_rsp within task's kernel stack | Before dispatch (`run`) |
| saved_rsp within stack after yield/block | After switch_context returns |
| Scheduler RSP != 0 | Top of dispatch loop |

## Example Crash Dump Output

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

## Troubleshooting Common Crash Patterns

### `RIP=0x4` / zero saved_rsp
A task was dispatched with `saved_rsp=0`. Check the "Current Task" section --
if `saved_rsp=0x0000000000000000`, the task's stack was never initialized or was
corrupted. The assertions in `pick_next` and `run` now catch this before dispatch.

### Stale task state on another core
Check the "Per-Core State" section. If a core shows `task_idx=N` where task N is
also claimed by the faulting core, there is a double-dispatch race. The scheduler
assertions in `block_current` and `yield_now` guard against this.

### Fork child crashes immediately
Check whether `make_fork_ctx` or `fork_child_trampoline` assertions fired. A
zero `user_rip` or missing page table means the fork context was corrupted or
the wrong process entry was used.

## Implementation Notes

- All Track C-F assertions use `debug_assert!` (compiled out in release builds)
- Track A and B diagnostics are always-on since they only execute on fatal paths
- The `dump_crash_context()` helper will be extended by Phase 43b to also dump
  the kernel trace ring
- `try_lock_scheduler()` in `kernel/src/task/mod.rs` provides deadlock-safe
  scheduler access for panic/fault contexts

## Deferred Until Later

- Full lockdep-lite checker (Phase 43b or beyond)
- Allocator poisoning / redzones (KASAN-style, Phase 43b or beyond)
- Register dump via NMI (requires NMI handler work)
- Backtrace / stack unwinding (requires frame pointer chain or DWARF unwinder)
