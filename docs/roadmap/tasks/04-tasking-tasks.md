# Phase 04 — Tasking: Task List

**Status:** Complete
**Source Ref:** phase-04
**Depends on:** Phase 2 ✅, Phase 3 ✅
**Goal:** Define the kernel task structure, implement a context-switch assembly stub, build a round-robin scheduler with an idle task, and enable timer-driven preemptive scheduling.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Task struct + kernel stacks | Phase 2, Phase 3 | ✅ Done |
| B | Context switch + scheduler | A | ✅ Done |
| C | Idle task + preemption | B | ✅ Done |
| D | Validation + docs | C | ✅ Done |

---

## Track A — Task Struct + Kernel Stacks

### A.1 — Define the kernel task structure and task states

**File:** `kernel/src/task/mod.rs`
**Symbols:** `Task`, `TaskState`
**Why it matters:** Every schedulable unit needs a descriptor holding its saved registers, state, stack pointer, and metadata.

**Acceptance:**
- [x] `Task` struct holds saved register layout, task name, and state
- [x] `TaskState` enum distinguishes `Ready`, `Running`, blocked variants

### A.2 — Create kernel stacks and bootstrap logic for new tasks

**File:** `kernel/src/task/mod.rs`
**Symbols:** `Task::new`, `init_stack`
**Why it matters:** Each task needs its own stack, and the initial stack frame must be set up so `switch_context` can resume execution at the task entry point.

**Acceptance:**
- [x] `Task::new()` allocates a kernel stack
- [x] `init_stack()` sets up the initial stack frame so `switch_context` can dispatch to the entry function

---

## Track B — Context Switch + Scheduler

### B.1 — Implement the context-switch assembly stub

**File:** `kernel/src/task/mod.rs`
**Symbol:** `switch_context`
**Why it matters:** This is the lowest-level mechanism for saving one task's register state and restoring another's — correctness here is critical to all multitasking.

**Acceptance:**
- [x] `switch_context(save_rsp, load_rsp)` saves and restores callee-saved registers (`rbx`, `rbp`, `r12`-`r15`, `rsp`, `rip`)
- [x] ABI contract is documented in comments

### B.2 — Add a round-robin scheduler and ready queue

**File:** `kernel/src/task/scheduler.rs`
**Symbol:** `Scheduler`
**Why it matters:** The scheduler determines which task runs next and drives all cooperative and preemptive multitasking.

**Acceptance:**
- [x] `Scheduler` struct maintains the task list and round-robin index
- [x] Scheduler picks the next `Ready` task and dispatches via `switch_context`

---

## Track C — Idle Task + Preemption

### C.1 — Add an idle task

**File:** `kernel/src/task/scheduler.rs`
**Why it matters:** When no runnable work exists, the CPU must halt rather than spin, reducing power consumption and making timer interrupts the wakeup path.

**Acceptance:**
- [x] Per-core idle tasks are registered in the scheduler
- [x] Idle task runs only when no other task is `Ready`

### C.2 — Trigger scheduling from the timer interrupt

**File:** `kernel/src/arch/x86_64/interrupts.rs`
**Symbol:** `timer_handler`
**Why it matters:** Timer-driven preemption ensures no single task monopolizes the CPU, which is the foundation of a multitasking OS.

**Acceptance:**
- [x] Timer interrupt sets a reschedule flag via `signal_reschedule`
- [x] Scheduler loop checks the flag and preempts the current task

---

## Track D — Validation + Docs

### D.1 — Validate multitasking behavior

**Why it matters:** Confirms that the context switch, scheduler, and preemption work together correctly.

**Acceptance:**
- [x] At least two kernel tasks run and their output interleaves over time
- [x] Register state survives task switches
- [x] Idle task runs only when no other task is ready

### D.2 — Document the tasking model

**Why it matters:** The context-switch contract and scheduler model are foundational knowledge that many later phases depend on.

**Acceptance:**
- [x] Context-switch contract documented, including which registers are saved
- [x] Scheduler model documented and why round-robin is a good teaching default
- [x] A note explains how mature kernels introduce priorities, affinities, and more complex wakeup paths

---

## Documentation Notes

- Adds `kernel/src/task/mod.rs` and `kernel/src/task/scheduler.rs`.
- Depends on both Phase 2 (heap for task allocation) and Phase 3 (timer interrupt for preemption).
