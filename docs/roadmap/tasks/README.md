# Roadmap Task Lists

This directory turns the roadmap milestones into concrete implementation task lists.
The milestone pages in `docs/roadmap/` explain the purpose, scope, and design intent of
each phase. The task pages here translate those goals into work items that can be
implemented and validated incrementally.

Each phase task list includes:

- implementation tasks
- validation tasks
- documentation tasks
- explicit dependencies on earlier phases

Every phase includes documentation work by design. A phase is not complete until the
project explains:

- what the feature is for
- how it is implemented here
- which simplifications were made
- how a mature operating system would usually differ at a high level

## Phase Task Flow

```mermaid
flowchart TD
    P1["Phase 1 Tasks"]
    P2["Phase 2 Tasks"]
    P3["Phase 3 Tasks"]
    P4["Phase 4 Tasks"]
    P5["Phase 5 Tasks"]
    P6["Phase 6 Tasks"]
    P7["Phase 7 Tasks"]
    P8["Phase 8 Tasks"]
    P9["Phase 9 Tasks"]

    P1 --> P2
    P1 --> P3
    P2 --> P4
    P3 --> P4
    P4 --> P5
    P5 --> P6
    P6 --> P7
    P7 --> P8
    P7 --> P9
    P8 --> P9
    P9 -.->|optional| P10["Phase 10 Tasks"]
```

## Task Documents

| Phase | Focus | Task List |
|---|---|---|
| 1 | Boot foundation | [Phase 1 Tasks](./01-boot-foundation-tasks.md) |
| 2 | Memory basics | [Phase 2 Tasks](./02-memory-basics-tasks.md) |
| 3 | Interrupts | [Phase 3 Tasks](./03-interrupts-tasks.md) |
| 4 | Tasking | [Phase 4 Tasks](./04-tasking-tasks.md) |
| 5 | Userspace entry | [Phase 5 Tasks](./05-userspace-entry-tasks.md) |
| 6 | IPC core | [Phase 6 Tasks](./06-ipc-core-tasks.md) |
| 7 | Core servers | [Phase 7 Tasks](./07-core-servers-tasks.md) |
| 8 | Storage and VFS | [Phase 8 Tasks](./08-storage-and-vfs-tasks.md) |
| 9 | Framebuffer and shell | [Phase 9 Tasks](./09-framebuffer-and-shell-tasks.md) |
| 10 *(optional)* | Secure Boot signing | [Phase 10 Tasks](./10-secure-boot-tasks.md) |

## Suggested Usage

Start from the milestone page for context, then use the task page to drive execution.
When a phase is complete, update the relevant subsystem docs before moving on.

Related documents:

- [Roadmap Guide](../README.md)
- [Roadmap Summary](../../08-roadmap.md)
