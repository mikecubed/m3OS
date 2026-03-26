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
    P9 --> P11["Phase 11 Tasks"]
    P8 --> P11
    P11 --> P12["Phase 12 Tasks"]
    P8 --> P13["Phase 13 Tasks"]
    P12 --> P14["Phase 14 Tasks"]
    P13 --> P14
    P3 --> P15["Phase 15 Tasks"]
    P12 --> P16["Phase 16 Tasks"]
    P15 --> P16
    P14 --> P17["Phase 17 Tasks"]
    P11 --> P17
    P17 --> P18["Phase 18 Tasks"]
    P13 --> P18
    P18 --> P19["Phase 19 Tasks"]
    P19 --> P20["Phase 20 Tasks"]
    P20 --> P21["Phase 21 Tasks"]
    P16 --> P22["Phase 22 Tasks"]
    P21 --> P22
    P18 --> P23["Phase 23 Tasks"]
    P15 --> P23
    P17 --> P24["Phase 24 Tasks"]
    P4 --> P24
    P20 --> P25["Phase 25 Tasks"]
    P23 --> P25
```

## Task Documents

### Foundation Phases (complete)

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

### POSIX and Userspace Phases

| Phase | Focus | Task List |
|---|---|---|
| 11 | ELF loader and process model | [Phase 11 Tasks](./11-process-model-tasks.md) |
| 12 | POSIX compatibility layer | [Phase 12 Tasks](./12-posix-compat-tasks.md) |
| 13 | Writable filesystem | [Phase 13 Tasks](./13-writable-fs-tasks.md) |
| 14 | Shell and userspace tools | [Phase 14 Tasks](./14-shell-and-tools-tasks.md) |
| 15 | Hardware discovery (ACPI + PCI) | [Phase 15 Tasks](./15-hardware-discovery-tasks.md) |
| 16 | Network stack | [Phase 16 Tasks](./16-network-tasks.md) |

### Usability Phases

| Phase | Focus | Task List |
|---|---|---|
| 17 | Memory reclamation (free-list, CoW fork, heap growth) | [Phase 17 Tasks](./17-memory-reclamation-tasks.md) |
| 18 | Directory and VFS (`getdents64`, real cwd) | [Phase 18 Tasks](./18-directory-vfs-tasks.md) |
| 19 | Signal handlers (trampolines, `sigreturn`) | [Phase 19 Tasks](./19-signal-handlers-tasks.md) |
| 20 | Userspace init and shell (ring-3 PID 1) | [Phase 20 Tasks](./20-userspace-init-shell-tasks.md) |
| 21 | TTY and terminal control (termios, PTY) | [Phase 21 Tasks](./21-tty-pty-tasks.md) |
| 22 | Socket API (BSD sockets over TCP/UDP stack) | [Phase 22 Tasks](./22-socket-api-tasks.md) |
| 23 | Persistent storage (virtio-blk, FAT32 r/w) | [Phase 23 Tasks](./23-persistent-storage-tasks.md) |

### Advanced Phases

| Phase | Focus | Task List |
|---|---|---|
| 24 | SMP (AP startup, per-core scheduler, TLB shootdown) | [Phase 24 Tasks](./24-smp-tasks.md) |
| 25 | Compiler bootstrap (TCC self-hosting) | *not yet created* |

## Suggested Usage

Start from the milestone page for context, then use the task page to drive execution.
When a phase is complete, update the relevant subsystem docs before moving on.

Related documents:

- [Roadmap Guide](../README.md)
- [Roadmap Summary](../../08-roadmap.md)
