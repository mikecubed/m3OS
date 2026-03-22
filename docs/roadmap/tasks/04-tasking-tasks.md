# Phase 4 Tasks - Tasking

**Depends on:** Phases 2 and 3

```mermaid
flowchart LR
    A["task struct + stacks"] --> B["context switch"]
    B --> C["ready queue"]
    C --> D["idle task"]
    D --> E["timer-driven preemption"]
    E --> F["docs + validation"]
```

## Implementation Tasks

- [x] P4-T001 Define the kernel task structure, saved register layout, and task states.
- [x] P4-T002 Create kernel stacks and bootstrap logic for newly spawned tasks.
- [x] P4-T003 Implement the context-switch assembly stub with a narrow, documented ABI.
- [x] P4-T004 Add a round-robin scheduler and ready queue.
- [x] P4-T005 Add an idle task that halts when no runnable work exists.
- [x] P4-T006 Trigger scheduling decisions from the timer interrupt path.

## Validation Tasks

- [x] P4-T007 Run at least two kernel tasks and verify their output interleaves over time.
- [x] P4-T008 Verify register state survives task switches.
- [x] P4-T009 Verify the idle task runs only when no other task is ready.

## Documentation Tasks

- [x] P4-T010 Document the context-switch contract, including which registers are saved.
- [x] P4-T011 Document the scheduler model and why round-robin is a good teaching default.
- [x] P4-T012 Add a short note explaining how mature kernels introduce priorities, affinities, and more complex wakeup paths.
