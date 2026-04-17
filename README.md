# m³OS

A serious, still-maturing bootable operating system written in Rust, targeting
**x86_64** with a **microkernel-inspired architecture**. Built for learning and
experimentation, with a real userspace, networking, remote access, and a
roadmap toward stronger service isolation and broader platform support.

## Quick Start

### Host prerequisites

Required for the supported headless/reference path:

- **Rust** — nightly toolchain (pinned via `rust-toolchain.toml`). Install via [rustup](https://rustup.rs/).
- **QEMU** — `qemu-system-x86_64` plus `qemu-img`
- **OVMF** — UEFI firmware for QEMU
- **e2fsprogs** — `mkfs.ext2` and `debugfs` (used to build the ext2 data disk)

Optional, each unlocks an additional build target or workflow:

- **musl cross-compiler** — required for the C userspace and musl-linked Rust demos
- **`rustup target add x86_64-unknown-linux-musl`** — required for the Rust `std` demo crates
- **`curl`, `tar`, `sha256sum`** — required to refresh the Lua/zlib port cache in `target/ports-src/`
- **`sbsigntool`** (`sbsign`/`sbverify`) — required for `cargo xtask image --sign` (Secure Boot)

Missing optional tools are skipped with warnings rather than blocking the build.

#### Install on Ubuntu / Debian (including WSL)

```bash
sudo apt install qemu-system-x86 qemu-utils ovmf e2fsprogs \
                 musl-tools sbsigntool curl build-essential
```

#### Install on Arch Linux (including Omarchy)

```bash
sudo pacman -S qemu-base edk2-ovmf e2fsprogs sbsigntools curl
# musl cross-compiler comes from the AUR:
yay -S musl-gcc-cross-bin
```

The xtask build system auto-detects both Debian/Ubuntu toolchain names
(`x86_64-linux-musl-gcc`) and Arch cross-compiler names
(`x86_64-unknown-linux-musl1.2-gcc`), and searches the OVMF paths for both
distros.

#### Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-linux-musl   # for Rust std demos
```

### First-time setup

After cloning, install the git hooks so quality gates run before each commit/push:

```bash
./setup.sh
```

### Build and run

```bash
# Build and run in headless QEMU (serial only)
cargo xtask run

# Recreate the ext2 data disk, then run headless
cargo xtask run --fresh

# Build and run in QEMU with a framebuffer window + keyboard input
cargo xtask run-gui

# Build a bootable disk image (UEFI raw + VHDX for Hyper-V)
cargo xtask image

# Lint, format-check, run host-side kernel-core tests
cargo xtask check
```

`cargo xtask run` is the primary supported workflow. It builds
`target/x86_64-unknown-none/release/boot-uefi-m3os.img`, keeps the companion ext2
data disk at `target/x86_64-unknown-none/release/disk.img`, and also emits
`target/x86_64-unknown-none/release/boot-uefi-m3os.vhdx`. `run-gui` is available for
framebuffer debugging, but the headless serial path is the reference system.

`run-gui` launches QEMU with an SDL window so the guest PS/2 keyboard can be used.
Click the QEMU window to grab input, then press `Ctrl+Alt+G` to release it. On
Wayland-based desktops, if keyboard input feels unresponsive, prefix the command
with `SDL_VIDEODRIVER=x11` to force SDL through XWayland.

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
configuration: `cargo xtask run` boots
`target/x86_64-unknown-none/release/boot-uefi-m3os.img` together with
`target/x86_64-unknown-none/release/disk.img` in QEMU, and `cargo xtask image`
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
