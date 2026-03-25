# m³OS — Toy Bootable OS in Rust

A toy bootable operating system built in Rust, following a **microkernel architecture**
targeting **x86_64**. Designed for learning, with the eventual goal of running a real
userspace shell and programs.

## Documentation Index

| Document | Description |
|---|---|
| [Architecture](./01-architecture.md) | Microkernel design, component boundaries, privilege model |
| [Boot Process](./02-boot.md) | UEFI boot flow, `bootloader` crate, kernel entry |
| [Memory Management](./03-memory.md) | Frame allocator, page tables, kernel heap |
| [Interrupts & Exceptions](./04-interrupts.md) | IDT, PIC, exception handlers, hardware IRQs |
| [Tasking & Scheduling](./05-tasking.md) | Task model, context switching, scheduler |
| [IPC](./06-ipc.md) | Interprocess communication — the core microkernel primitive |
| [Userspace & Syscalls](./07-userspace.md) | Ring 3, address spaces, system call interface, servers |
| [Roadmap](./08-roadmap.md) | Phased implementation plan and open design questions |
| [Roadmap Guide](./roadmap/README.md) | Detailed learning-first milestones with per-phase pages and Mermaid diagrams |
| [Roadmap Task Lists](./roadmap/tasks/README.md) | Actionable per-phase task breakdowns paired with the roadmap milestones |
| [Testing](./09-testing.md) | QEMU-based test harness, exit conventions, writing tests |
| [ELF Loader & Process Model](./11-elf-loader-and-process-model.md) | ELF loading, per-process page tables, fork, process lifecycle |
| [POSIX Compatibility Layer](./12-posix-compatibility-layer.md) | Linux syscall ABI, musl libc, TLS, C runtime startup, user-memory safety |

## Quick Start

```bash
# Build and run in QEMU (requires nightly Rust, QEMU, OVMF)
cargo +nightly xtask run

# Build a bootable disk image (UEFI raw + VHDX for Hyper-V)
cargo +nightly xtask image

# Run tests (QEMU-based) — coming in Phase 2+
# cargo +nightly xtask test
```

## Design Principles

- **Minimal trusted computing base** — The kernel does as little as possible.
- **Safety by default** — `unsafe` is used only at hardware boundaries, always wrapped in safe abstractions.
- **Incremental** — Each phase produces a runnable artifact; nothing is left in a broken state.
- **Self-contained** — No large third-party runtimes; the crate ecosystem is used for hardware abstractions only.

## Key Reference Projects

| Project | How we use it |
|---|---|
| [blog_os](https://os.phil-opp.com/) | Primary tutorial for Phases 1–3 |
| [Redox OS](https://redox-os.org/) | IPC design, server model, VFS architecture |
| [Theseus OS](https://github.com/theseus-os/Theseus) | Rust ownership ideas for kernel safety |
| [Legacy C Kernel Analysis](./legacy-os-comparison.md) | Comparison with a prior x86 C kernel — what to adopt, reject, and learn from |