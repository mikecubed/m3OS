# m³OS — Bootable OS in Rust

A serious, still-maturing bootable operating system built in Rust, following a
**microkernel-inspired architecture** targeting **x86_64**. Designed for
learning and experimentation, with a real userspace, networking, remote access,
and a roadmap toward stronger service isolation and broader platform support.

## Documentation Index

### Phase-Aligned Learning Docs

| Document | Phase | Description |
|---|---|---|
| [Boot Process](./01-boot.md) | 1 | UEFI boot flow, `bootloader` crate, kernel entry |
| [Memory Management](./02-memory.md) | 2 | Frame allocator, page tables, kernel heap |
| [Interrupts & Exceptions](./03-interrupts.md) | 3 | IDT, PIC, exception handlers, hardware IRQs |
| [Tasking & Scheduling](./04-tasking.md) | 4 | Task model, context switching, scheduler |
| [Userspace Entry](./05-userspace-entry.md) | 5 | Ring 3 transition, syscall gate, first userspace binary |
| [IPC](./06-ipc.md) | 6 | Synchronous rendezvous, capabilities, notifications |
| [Core Servers](./07-core-servers.md) | 7 | init, console_server, kbd_server, service registry |
| [Storage and VFS](./08-storage-and-vfs.md) | 8 | VFS layer, ramdisk, file IPC protocol |
| [Framebuffer and Shell](./09-framebuffer-and-shell.md) | 9 | Pixel console, keyboard IPC, shell |
| [Secure Boot](./10-secure-boot.md) | 10 | Host-side signing, UEFI Secure Boot |
| [ELF Loader & Process Model](./11-elf-loader-and-process-model.md) | 11 | ELF loading, per-process page tables, fork, process lifecycle |
| [POSIX Compatibility Layer](./12-posix-compatibility-layer.md) | 12 | Linux syscall ABI, musl libc, TLS, C runtime startup |
| [Writable Filesystem](./13-writable-filesystem.md) | 13 | tmpfs at /tmp, file mutation syscalls |
| [Shell and Tools](./14-shell-and-tools.md) | 14 | Pipes, redirection, job control, coreutils |
| [Hardware Discovery](./15-hardware-discovery.md) | 15 | ACPI, PCI enumeration, APIC |
| [Network Stack](./16-network.md) | 16 | virtio-net, Ethernet, ARP, IPv4, TCP, UDP |
| [Memory Reclamation](./17-memory-reclamation.md) | 17 | Free-list allocator, CoW fork, heap growth |
| [Directory VFS](./18-directory-vfs.md) | 18 | getdents64, directory fds, per-process cwd |
| [Signal Handlers](./19-signal-handlers.md) | 19 | rt_sigaction, sigframe, sigreturn |
| [Userspace Init and Shell](./20-userspace-init.md) | 20 | Ring-3 PID 1, remove kernel shell |
| [Ion Shell Integration](./21-ion-shell.md) | 21 | Redox OS ion shell, cross-compilation |
| [TTY and Terminal Control](./22-tty-terminal.md) | 22 | termios, line discipline, cooked/raw mode |
| [ANSI Escape Sequences](./22b-ansi-escape.md) | 22b | VT100 CSI parser, cursor, SGR colors |
| [Socket API](./23-socket-api.md) | 23 | BSD socket syscalls, userspace ping, poll for sockets |
| [Persistent Storage](./24-persistent-storage.md) | 24 | virtio-blk, FAT32 read/write, /data mount |
| [SMP](./25-smp.md) | 25 | AP startup, per-core scheduler, TLB shootdown |
| [Text Editor](./26-text-editor.md) | 26 | Full-screen editor (kibi-style) |
| [User Accounts](./27-user-accounts.md) | 27 | Login, UID/GID, file permissions, passwd/shadow |
| [ext2 Filesystem](./28-ext2-filesystem.md) | 28 | Native Unix permissions, replaces FAT32 |
| [PTY Subsystem](./29-pty-subsystem.md) | 29 | Pseudo-terminal pairs, session management |
| [Telnet Server](./30-telnet-server.md) | 30 | Remote shell access over TCP |
| [Compiler Bootstrap](./31-compiler-bootstrap.md) | 31 | TCC compiles C programs inside the OS |
| [Build Tools](./32-build-tools.md) | 32 | make, ar, multi-file C projects |
| [Kernel Memory](./33-kernel-memory.md) | 33 | Buddy allocator, slab caches, working munmap |
| [Timekeeping](./34-timekeeping.md) | 34 | CMOS RTC, wall-clock time, CLOCK_REALTIME |
| [True SMP](./25-smp.md) | 35 | Per-core syscalls, priority scheduling, load balancing |
| [Expanded Memory](./33-kernel-memory.md) | 36 | Demand paging, mprotect, large mmap, 1 GB RAM/disk |
| [Crash Diagnostics](./43a-crash-diagnostics.md) | 43a | Enriched panic/fault handlers, scheduler/fork/IPC assertions |
| [DOOM Port](./47-doom.md) | 47 | Framebuffer mapping, raw scancodes, doomgeneric integration, and real-world input/performance lessons |
| [Security Foundation](./48-security-foundation.md) | 48 | Kernel-enforced credentials, RDRAND entropy, iterated password hashing, first-boot provisioning |
| [Architectural Declaration](./49-architectural-declaration.md) | 49 | Syscall decomposition, keep/move/transition matrix, userspace-first rule |
| [IPC Completion](./50-ipc-completion.md) | 50 | Capability grants, bulk-data transport, ring-3-safe registry, server-loop failure semantics |
| [Service Model Maturity](./51-service-model-maturity.md) | 51 | Stable service contract, restart backoff, crash classification, shutdown ordering, admin hardening |
| [First Service Extractions](./52-first-service-extractions.md) | 52 | Console and keyboard extracted to ring-3 services, restart behavior, IPC-based input/output |
| [Kernel Reliability Fixes](./52a-kernel-reliability-fixes.md) | 52a | Stale resume-state, sunset waker, clear_child_tid, and exec signal-reset fixes |
| [Kernel Structural Hardening](./52b-kernel-structural-hardening.md) | 52b | AddressSpace, typed user buffers, task-owned return state, batched TLB, frame zeroing |
| [Kernel Architecture Evolution](./52c-kernel-architecture-evolution.md) | 52c | VMA tree, growable IPC tables, kernel line-discipline infrastructure, ISR wakeups |
| [Kernel Completion and Roadmap Alignment](./52d-kernel-completion-and-roadmap-alignment.md) | 52d | Return-state closure, keyboard convergence, bootfixes, and release-gate/initrd cleanup |
| [Kernel Memory Modernization](./53a-kernel-memory-modernization.md) | 53a | Per-CPU page cache, magazine slabs, size-class allocator cutover, cross-CPU frees, and allocator-local reclaim |

### Roadmap

| Document | Description |
|---|---|
| [Roadmap Guide](./roadmap/README.md) | Detailed learning-first milestones with per-phase pages and Mermaid diagrams |
| [Roadmap Task Lists](./roadmap/tasks/README.md) | Actionable per-phase task breakdowns for implemented and near-term phases; later phases add task docs when implementation planning begins |

### Appendix (cross-cutting and historical)

| Document | Description |
|---|---|
| [Architecture & Syscalls](./appendix/architecture-and-syscalls.md) | Microkernel design, privilege model, syscall ABI, address space layout |
| [Testing](./appendix/testing.md) | QEMU-based test harness, exit conventions, writing tests |
| [Legacy C Kernel Comparison](./appendix/legacy-os-comparison.md) | Comparison with a prior x86 C kernel |
| [State Analysis (March 2026)](./appendix/state-analysis-march-2026.md) | Historical snapshot of OS state before Phases 17-34 |
| [Phase 21 Handoff](./appendix/phase-21-handoff.md) | Ion shell integration PR handoff notes |

### Standalone Roadmaps

| Document | Description |
|---|---|
| [Clang/LLVM Roadmap](./clang-llvm-roadmap.md) | Clang/LLVM cross-compilation strategy |
| [Python Roadmap](./python-roadmap.md) | Python cross-compilation strategy |
| [Node.js Roadmap](./nodejs-roadmap.md) | Node.js cross-compilation strategy |
| [git Roadmap](./git-roadmap.md) | git cross-compilation strategy |
| [GitHub CLI Roadmap](./github-cli-roadmap.md) | gh CLI cross-compilation strategy |
| [Claude Code Roadmap](./claude-code-roadmap.md) | Claude Code on m3OS strategy |
| [Rust Crate Acceleration](./rust-crate-acceleration.md) | Rust crate porting strategy |

### Evaluation

| Document | Description |
|---|---|
| [Project Evaluation](./evaluation/README.md) | Repo-wide review of current state, security, usability gaps, GUI path, and Rust OS comparisons |
| [Evaluation Roadmap](./evaluation/roadmap/README.md) | Release-oriented path to 1.0 and beyond, tied back to the official implementation roadmap |

## Quick Start

```bash
# Build and run in QEMU (requires nightly Rust, QEMU, OVMF)
cargo +nightly xtask run

# Build a bootable disk image (UEFI raw + VHDX for Hyper-V)
cargo +nightly xtask image

# Run tests
cargo +nightly xtask test
```

## Design Principles

- **Minimal trusted computing base** -- The kernel does as little as possible.
- **Safety by default** -- `unsafe` is used only at hardware boundaries, always wrapped in safe abstractions.
- **Incremental** -- Each phase produces a runnable artifact; nothing is left in a broken state.
- **Self-contained** -- No large third-party runtimes; the crate ecosystem is used for hardware abstractions only.
