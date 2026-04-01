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
    P21 --> P22["Phase 22 Tasks"]
    P16 --> P23["Phase 23 Tasks"]
    P22 --> P23
    P18 --> P24["Phase 24 Tasks"]
    P15 --> P24
    P17 --> P25["Phase 25 Tasks"]
    P4 --> P25
    P20 --> P26["Phase 26 Tasks"]
    P24 --> P26

    %% Productivity phases
    P22 --> P26
    P12 --> P27["Phase 27 Tasks"]
    P24 --> P27
    P27 --> P28["Phase 28 Tasks"]
    P24 --> P28
    P22 --> P29["Phase 29 Tasks"]
    P27 --> P29
    P23 --> P30["Phase 30 Tasks"]
    P27 --> P30
    P29 --> P30
    P26 --> P31["Phase 31 Tasks"]
    P14 --> P31
    P31 --> P32["Phase 32 Tasks"]
    P26 --> P32

    %% Kernel infrastructure phases
    P17 --> P33["Phase 33 Tasks"]
    P25 --> P33
    P15 --> P34["Phase 34 Tasks"]
    P25 --> P35["Phase 35 Tasks"]
    P33 --> P35
    P23 --> P36["Phase 36 Tasks"]
    P22 --> P36
    P35 --> P36
    P28 --> P37["Phase 37 Tasks"]
    P27 --> P37
    P23 --> P38["Phase 38 Tasks"]
    P37 --> P38
    P36 --> P38
    P35 --> P39["Phase 39 Tasks"]
    P33 --> P39

    %% Application phases
    P14 --> P40["Phase 40 Tasks"]
    P27 --> P40
    P37 --> P40
    P31 --> P41["Phase 41 Tasks"]
    P41 --> P42["Phase 42 Tasks"]
    P29 --> P42
    P27 --> P42
    P36 --> P42
    P12 --> P43["Phase 43 Tasks"]
    P24 --> P43
    P31 --> P44["Phase 44 Tasks"]
    P32 --> P44
    P40 --> P44
    P27 --> P45["Phase 45 Tasks"]
    P30 --> P45
    P24 --> P45
    P34 --> P45
    P38 --> P45
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

### POSIX and Userspace Phases (complete)

| Phase | Focus | Task List |
|---|---|---|
| 11 | ELF loader and process model | [Phase 11 Tasks](./11-process-model-tasks.md) |
| 12 | POSIX compatibility layer | [Phase 12 Tasks](./12-posix-compat-tasks.md) |
| 13 | Writable filesystem | [Phase 13 Tasks](./13-writable-fs-tasks.md) |
| 14 | Shell and userspace tools | [Phase 14 Tasks](./14-shell-and-tools-tasks.md) |
| 15 | Hardware discovery (ACPI + PCI) | [Phase 15 Tasks](./15-hardware-discovery-tasks.md) |
| 16 | Network stack | [Phase 16 Tasks](./16-network-tasks.md) |

### Usability Phases (complete)

| Phase | Focus | Task List |
|---|---|---|
| 17 | Memory reclamation (free-list, CoW fork, heap growth) | [Phase 17 Tasks](./17-memory-reclamation-tasks.md) |
| 18 | Directory and VFS (`getdents64`, real cwd) | [Phase 18 Tasks](./18-directory-vfs-tasks.md) |
| 19 | Signal handlers (trampolines, `sigreturn`) | [Phase 19 Tasks](./19-signal-handlers-tasks.md) |
| 20 | Userspace init and shell (ring-3 PID 1) | [Phase 20 Tasks](./20-userspace-init-shell-tasks.md) |
| 21 | Ion shell integration (ion replaces custom shell) | [Phase 21 Tasks](./21-ion-shell-tasks.md) |
| 22 | TTY and terminal control (termios, PTY) | [Phase 22 Tasks](./22-tty-pty-tasks.md) |
| 23 | Socket API (BSD sockets over TCP/UDP stack) | [Phase 23 Tasks](./23-socket-api-tasks.md) |
| 24 | Persistent storage (virtio-blk, FAT32 r/w) | [Phase 24 Tasks](./24-persistent-storage-tasks.md) |

### Advanced Phases (complete)

| Phase | Focus | Task List |
|---|---|---|
| 25 | SMP (AP startup, per-core scheduler, TLB shootdown) | [Phase 25 Tasks](./25-smp-tasks.md) |
| 26 | Text editor (kibi-style full-screen editor) | [Phase 26 Tasks](./26-text-editor-tasks.md) |
| 27 | User accounts (login, passwd, multi-user) | [Phase 27 Tasks](./27-user-accounts-tasks.md) |
| 28 | ext2 filesystem (persistent storage) | [Phase 28 Tasks](./28-ext2-filesystem-tasks.md) |
| 29 | PTY subsystem (pseudo-terminal pairs) | [Phase 29 Tasks](./29-pty-subsystem-tasks.md) |

### Productivity Phases

| Phase | Focus | Task List |
|---|---|---|
| 30 | Telnet server (remote shell access) | [Phase 30 Tasks](./30-telnet-server-tasks.md) |
| 31 | Compiler bootstrap (TCC) | [Phase 31 Tasks](./31-compiler-bootstrap-tasks.md) |
| 32 | Build tools (make, ar) | [Phase 32 Tasks](./32-build-tools-tasks.md) |

### Kernel Infrastructure Phases

| Phase | Focus | Task List |
|---|---|---|
| 33 | Kernel memory improvements (slab, OOM retry, munmap) | *not yet created* |
| 34 | Real-time clock and timekeeping | *not yet created* |
| 35 | True SMP multitasking (per-core dispatch, priorities) | *not yet created* |
| 36 | I/O multiplexing (select, epoll, non-blocking) | *not yet created* |
| 37 | Filesystem enhancements (symlinks, /proc, permissions) | *not yet created* |
| 38 | Unix domain sockets (AF_UNIX) | *not yet created* |
| 39 | Threading primitives (clone, futex, TLS) | *not yet created* |

### Application Phases

| Phase | Focus | Task List |
|---|---|---|
| 40 | Expanded coreutils (head, tail, sort, find, diff, ps) | *not yet created* |
| 41 | Crypto primitives (SHA-256, Ed25519, ChaCha20) | *not yet created* |
| 42 | SSH server (encrypted remote access) | *not yet created* |
| 43 | Rust cross-compilation | *not yet created* |
| 44 | Ports system (source-based package building) | *not yet created* |
| 45 | System services (init, syslog, cron) | *not yet created* |

## Suggested Usage

Start from the milestone page for context, then use the task page to drive execution.
When a phase is complete, update the relevant subsystem docs before moving on.

Related documents:

- [Roadmap Guide](../README.md)
- [Roadmap Summary](../../08-roadmap.md)
