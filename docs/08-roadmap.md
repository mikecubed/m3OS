# Roadmap Summary

This file is the short version of the roadmap. The detailed milestone set now lives in
[`docs/roadmap/`](./roadmap/README.md), where each phase has its own page covering the
feature goal, implementation approach, acceptance criteria, deferrals, and a short note
about how real operating systems usually differ. Actionable task lists now live in
[`docs/roadmap/tasks/`](./roadmap/tasks/README.md).

## Phase Overview

```mermaid
flowchart TD
    P1["1. Boot Foundation"]
    P2["2. Memory Basics"]
    P3["3. Interrupts"]
    P4["4. Tasking"]
    P5["5. Userspace Entry"]
    P6["6. IPC Core"]
    P7["7. Core Servers"]
    P8["8. Storage and VFS"]
    P9["9. Framebuffer and Shell"]

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
```

## Detailed Phase Pages

| Phase | Focus | Link |
|---|---|---|
| 1 | Bootable kernel, serial, panic path | [Boot Foundation](./roadmap/01-boot-foundation.md) |
| 2 | Frames, paging, heap | [Memory Basics](./roadmap/02-memory-basics.md) |
| 3 | Exceptions, timer, keyboard IRQ | [Interrupts](./roadmap/03-interrupts.md) |
| 4 | Context switching and scheduler | [Tasking](./roadmap/04-tasking.md) |
| 5 | Ring 3 and syscall entry | [Userspace Entry](./roadmap/05-userspace-entry.md) |
| 6 | Endpoints, capabilities, notifications | [IPC Core](./roadmap/06-ipc-core.md) |
| 7 | `init`, console, keyboard services | [Core Servers](./roadmap/07-core-servers.md) |
| 8 | VFS and read-only storage | [Storage and VFS](./roadmap/08-storage-and-vfs.md) |
| 9 | Screen output and shell | [Framebuffer and Shell](./roadmap/09-framebuffer-and-shell.md) |

## Documentation Expectation Per Phase

Each phase should produce documentation that explains:

- what the feature is for
- how it is implemented in this project
- which parts are intentionally simplified
- how mature operating systems would usually approach the same problem

## Related Reading

- [Roadmap Guide](./roadmap/README.md)
- [Roadmap Task Lists](./roadmap/tasks/README.md)
- [Architecture](./01-architecture.md)
- [IPC](./06-ipc.md)
- [Userspace & Syscalls](./07-userspace.md)
- [Testing](./09-testing.md)
