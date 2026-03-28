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
    P9 -.->|optional| P10["10. Secure Boot"]
    P9 --> P11["11. Process Model"]
    P8 --> P11
    P11 --> P12["12. POSIX Compat"]
    P8 --> P13["13. Writable FS"]
    P12 --> P14["14. Shell and Tools"]
    P13 --> P14
    P3 --> P15["15. Hardware Discovery"]
    P12 --> P16["16. Network"]
    P15 --> P16
    P14 --> P17["17. Memory Reclamation"]
    P11 --> P17
    P17 --> P18["18. Directory and VFS"]
    P13 --> P18
    P18 --> P19["19. Signal Handlers"]
    P19 --> P20["20. Userspace Init and Shell"]
    P20 --> P21["21. Ion Shell Integration"]
    P21 --> P22["22. TTY and Terminal Control"]
    P16 --> P23["23. Socket API"]
    P22 --> P23
    P18 --> P24["24. Persistent Storage"]
    P15 --> P24
    P17 --> P25["25. SMP"]
    P4 --> P25
    P20 --> P26["26. Compiler Bootstrap"]
    P24 --> P26
```

## Detailed Phase Pages

### Foundation Phases (complete)

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
| 10 *(optional)* | Secure Boot signing for real hardware | [Secure Boot](./roadmap/10-secure-boot.md) |

### POSIX and Userspace Phases (complete)

| Phase | Focus | Link |
|---|---|---|
| 11 | ELF loader; fork, exec, wait | [Process Model](./roadmap/11-process-model.md) |
| 12 | Linux syscall ABI; musl libc; C programs run unmodified | [POSIX Compat](./roadmap/12-posix-compat.md) |
| 13 | tmpfs writable filesystem | [Writable FS](./roadmap/13-writable-fs.md) |
| 14 | Pipes, redirection, job control, core utilities | [Shell and Tools](./roadmap/14-shell-and-tools.md) |
| 15 | ACPI parsing, PCI enumeration, APIC replaces PIC | [Hardware Discovery](./roadmap/15-hardware-discovery.md) |
| 16 | virtio-net driver, Ethernet/ARP/IP/UDP/TCP | [Network](./roadmap/16-network.md) |
| 17 | Frame reclaim, heap growth, CoW fork, kernel stack cleanup | [Memory Reclamation](./roadmap/17-memory-reclamation.md) |
| 18 | `getdents64`, directory fds, real cwd, ramdisk layout | [Directory and VFS](./roadmap/18-directory-vfs.md) |
| 19 | User signal handlers, trampolines, `sigreturn`, `sigprocmask` | [Signal Handlers](./roadmap/19-signal-handlers.md) |
| 20 | Userspace PID 1 init, ring-3 shell, remove kernel shell | [Userspace Init and Shell](./roadmap/20-userspace-init-shell.md) |
| 21 | ion shell (Redox OS) replaces minimal custom shell | [Ion Shell Integration](./roadmap/21-ion-shell.md) |
| 22 | termios, cooked/raw mode, PTY stubs, window size, ion default shell | [TTY and Terminal Control](./roadmap/22-tty-pty.md) |
| 22b | ANSI/VT100 escape sequences, CSI parser, cursor movement, erase, SGR colors | [ANSI Escape Sequences](./roadmap/22-tty-pty.md#phase-22b-vt100--ansi-escape-sequence-processing-completed) |

### Usability Phases (planned)

| Phase | Focus | Link |
|---|---|---|
| 23 | Socket syscalls, expose TCP/UDP stack to userspace | [Socket API](./roadmap/23-socket-api.md) |
| 24 | virtio-blk driver, FAT32 read/write, persistent `/data` | [Persistent Storage](./roadmap/24-persistent-storage.md) |

### Advanced Phases (deferred)

| Phase | Focus | Link |
|---|---|---|
| 25 | AP startup, per-core scheduler, TLB shootdown | [SMP](./roadmap/25-smp.md) |
| 26 | TCC runs and compiles itself inside the OS | [Compiler Bootstrap](./roadmap/26-compiler-bootstrap.md) |

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
- [OS State Analysis](./17-os-state-analysis.md)
