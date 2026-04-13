# m³OS

A serious, still-maturing bootable operating system written in Rust, targeting
**x86_64** with a **microkernel-inspired architecture**. Built for learning and
experimentation, with a real userspace, networking, remote access, and a
roadmap toward stronger service isolation and broader platform support.

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

- [Boot Process](docs/01-boot.md) — UEFI boot flow and kernel entry
- [Memory Management](docs/02-memory.md) — Frame allocator, page tables, heap
- [Interrupts & Exceptions](docs/03-interrupts.md) — IDT, PIC, hardware IRQs
- [Tasking & Scheduling](docs/04-tasking.md) — Task model and context switching
- [IPC](docs/06-ipc.md) — The core microkernel primitive
- [Roadmap Guide](docs/roadmap/README.md) — Phased implementation plan
- [Architecture & Syscalls](docs/appendix/architecture-and-syscalls.md) — Microkernel design, privilege model, syscall ABI

## Design Principles

- **Minimal TCB** — The kernel does as little as possible.
- **Safety by default** — `unsafe` only at hardware boundaries, always wrapped.
- **Incremental** — Each phase produces a runnable artifact.
- **Self-contained** — No large third-party runtimes.

## Headless/Reference System

m3OS targets a **headless/reference baseline** as its primary supported
configuration: boot in QEMU, log in, manage services, build software, diagnose
failures, and shut down cleanly. SSH is the default remote-admin path; telnet
is available only with an explicit opt-in build flag.

See [`docs/roadmap/53-headless-hardening.md`](docs/roadmap/53-headless-hardening.md)
for the supported workflow, validation gate bundle, and support boundary.
GUI, broad hardware certification, and large runtime ecosystems are later work.
