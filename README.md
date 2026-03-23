# ostest

A toy bootable operating system written in Rust, targeting **x86_64** with a **microkernel architecture**. Built for learning OS fundamentals, with the goal of eventually running a real userspace shell.

## Quick Start

> Requires: nightly Rust, QEMU, OVMF

```bash
# Build and run in headless QEMU (serial only)
cargo +nightly xtask run

# Build and run in QEMU with a window for framebuffer + keyboard input
cargo +nightly xtask run-gui

# Build a bootable disk image (UEFI raw + VHDX for Hyper-V)
cargo +nightly xtask image
```

`run-gui` launches QEMU with an SDL window so the guest PS/2 keyboard can be used.
Click the QEMU window to grab input, then press `Ctrl+Alt+G` to release it.

## Project Layout

```
kernel/     — The microkernel (boots, runs in ring 0)
xtask/      — Build tooling (run, image, test)
docs/       — Architecture docs and learning roadmap
```

## Documentation

Full documentation lives in [`docs/`](docs/README.md):

- [Architecture](docs/01-architecture.md) — Microkernel design and privilege model
- [Boot Process](docs/02-boot.md) — UEFI boot flow and kernel entry
- [Memory Management](docs/03-memory.md) — Frame allocator, page tables, heap
- [Interrupts & Exceptions](docs/04-interrupts.md) — IDT, PIC, hardware IRQs
- [Tasking & Scheduling](docs/05-tasking.md) — Task model and context switching
- [IPC](docs/06-ipc.md) — The core microkernel primitive
- [Userspace & Syscalls](docs/07-userspace.md) — Ring 3, address spaces, syscall interface
- [Roadmap](docs/08-roadmap.md) — Phased implementation plan

## Design Principles

- **Minimal TCB** — The kernel does as little as possible.
- **Safety by default** — `unsafe` only at hardware boundaries, always wrapped.
- **Incremental** — Each phase produces a runnable artifact.
- **Self-contained** — No large third-party runtimes.
