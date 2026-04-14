# m³OS

A serious, still-maturing bootable operating system written in Rust, targeting
**x86_64** with a **microkernel-inspired architecture**. Built for learning and
experimentation, with a real userspace, networking, remote access, and a
roadmap toward stronger service isolation and broader platform support.

## Quick Start

### Host prerequisites

Required for the supported headless/reference path:

- nightly Rust (the repo pins `nightly` in `rust-toolchain.toml`)
- QEMU (`qemu-system-x86_64` plus `qemu-img`)
- OVMF firmware
- `debugfs` from `e2fsprogs` (used to populate the ext2 data disk)

Optional extras that widen what gets bundled into the image:

- `curl`, `tar`, and `sha256sum` to refresh the Lua/zlib host cache in `target/ports-src/`
- `musl-gcc` for the musl-linked C demo/test binaries
- `rustup target add x86_64-unknown-linux-musl` for the Rust `std` demo crates
- `sbsign` and `sbverify` for `cargo xtask image --sign`

Missing optional tools either skip those extras with warnings or leave the
cold-cache refresh path unavailable. The baseline headless image/build path
still works when the relevant caches are already primed.

```bash
# Build and run in headless QEMU (serial only)
cargo +nightly xtask run

# Recreate the ext2 data disk, then run the same headless workflow
cargo +nightly xtask run --fresh

# Build and run in QEMU with a window for framebuffer + keyboard input
cargo +nightly xtask run-gui

# Build a bootable disk image (UEFI raw + VHDX for Hyper-V)
cargo +nightly xtask image
```

`cargo +nightly xtask run` is the primary supported workflow. It builds
`target/x86_64-unknown-none/release/boot-uefi-m3os.img`, keeps the companion ext2
data disk at `target/x86_64-unknown-none/release/disk.img`, and also emits
`target/x86_64-unknown-none/release/boot-uefi-m3os.vhdx`. `run-gui` is available for
framebuffer debugging, but the headless serial path is the reference system.

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
- [Headless Hardening](docs/53-headless-hardening.md) — Learner-facing overview of the headless/reference workflow, gates, and support boundary
- [Roadmap Guide](docs/roadmap/README.md) — Phased implementation plan
- [Architecture & Syscalls](docs/appendix/architecture-and-syscalls.md) — Microkernel design, privilege model, syscall ABI

## Design Principles

- **Minimal TCB** — The kernel does as little as possible.
- **Safety by default** — `unsafe` only at hardware boundaries, always wrapped.
- **Incremental** — Each phase produces a runnable artifact.
- **Self-contained** — No large third-party runtimes.

## Headless/Reference System

m3OS targets a **headless/reference baseline** as its primary supported
configuration: `cargo +nightly xtask run` boots
`target/x86_64-unknown-none/release/boot-uefi-m3os.img` together with
`target/x86_64-unknown-none/release/disk.img` in QEMU, and `cargo +nightly xtask image`
builds the same artifacts without launching QEMU. The supported workflow is to boot,
log in, inspect services, verify storage and logging, compile bounded software, diagnose failures, and shut down cleanly over
the serial/headless path. SSH is the default remote-admin path; telnet is available
only with an explicit opt-in build flag.

The supported Rust std reference demos are `hello-rust`, `sysinfo-rust`,
`httpd-rust`, `calc-rust`, and `todo-rust`. They are shipped as manual
validation surfaces for the headless/reference image rather than part of the
mandatory smoke bundle. The ports baseline always ships `/usr/bin/port` and the
in-repo ports tree; ports that depend on host-fetched Lua/zlib sources require
that cache to be present when the image is built.

See [`docs/53-headless-hardening.md`](docs/53-headless-hardening.md) for the
learner-facing overview, and
[`docs/roadmap/53-headless-hardening.md`](docs/roadmap/53-headless-hardening.md)
for the exact supported workflow, validation gate bundle, and support boundary.
GUI, broad hardware certification, and large runtime ecosystems are later work.
