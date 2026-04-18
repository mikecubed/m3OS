# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**m3OS** (technical name: `m3os`) is a toy bootable OS in Rust: microkernel architecture, x86_64, UEFI boot. Kernel v0.51.0 with functional userspace (init, shell, coreutils, networking, SMP, storage, signals, editor, multi-user, PTY, telnet/SSH servers, crypto, musl cross-compilation, ports system, service manager, IPC). See `docs/appendix/codebase-map.md` for full workspace and source layout.

## Build & Run

Uses the `xtask` pattern — always build through `cargo xtask`, never `cargo build` directly.

```bash
cargo xtask run          # build + launch in QEMU (headless, serial output)
cargo xtask run --fresh  # same, but recreate data disk first
cargo xtask run-gui      # build + launch in QEMU (GUI with framebuffer)
cargo xtask run-gui --fresh  # same, but recreate data disk first
cargo xtask image        # build bootable disk image (UEFI raw + VHDX)
cargo xtask image --sign # build + sign EFI binary for Secure Boot
cargo xtask check        # clippy (-D warnings) + rustfmt + kernel-core host tests
cargo xtask fmt --fix    # auto-format all workspace source
cargo xtask test         # run all kernel tests in QEMU via ISA debug exit
cargo xtask test --test <name>  # run a single QEMU test binary
cargo xtask test --timeout 120  # custom timeout (default 60s)
cargo xtask test --display      # show QEMU window for debugging
cargo xtask sign         # sign EFI binary with Secure Boot keys
cargo xtask clean        # delete disk.img so next run recreates it
cargo test -p kernel-core       # run kernel-core host-side unit tests directly
```

After adding new service configs to the ext2 data disk, run `cargo xtask clean` to force disk recreation.

Tests cannot use `cargo test` on the kernel — it is `no_std` and tests run inside QEMU via the xtask harness. Pure-logic code lives in `kernel-core` and is testable on the host via `cargo test -p kernel-core`.

## Git Workflow

All work must happen on a feature branch with a pull request to `main`. Never commit directly to `main`.

```bash
git checkout -b feat/my-feature       # 1. create feature branch
# ... make changes ...
git add <files> && git commit         # 2. commit
git push -u origin feat/my-feature    # 3. push
gh pr create --base main              # 4. open PR to main
# 5. user merges PR after review
```

Branch naming: `feat/`, `fix/`, `refactor/`, `docs/` prefixes as appropriate.

## First-Time Setup

After cloning, install the git hooks so quality gates run before commits and pushes:

```bash
./setup.sh
```

This sets `core.hooksPath` to `.githooks/`, which contains pre-commit and pre-push hooks that run `cargo xtask check`.

## Architecture

Microkernel: ring 0 kernel handles memory management, scheduling, IPC, interrupt routing, and device drivers. Userspace processes run in ring 3 and communicate through IPC and syscalls.

```
Ring 0 (kernel/):                Ring 3 (userspace/):
  - Frame allocator                - init (PID 1 daemon)
  - Page table manager             - sh0 (built-in shell)
  - Scheduler (SMP-aware)          - coreutils (cat, ls, grep, etc.)
  - IPC engine + capabilities      - ping (ICMP network tool)
  - IDT / APIC / interrupt router  - edit (text editor)
                                   - login, su, passwd, adduser
                                   - id, whoami
                                   - ion shell (external)
  - Syscall gate
  - VFS + FAT32 + tmpfs
  - Network stack (IPv4/TCP/UDP)
  - Unix domain sockets (AF_UNIX)
  - VirtIO drivers (blk, net)
  - ACPI / PCI enumeration
  - Framebuffer console
  - TTY + signal handling
  - SMP (multi-core boot + IPI)
```

See `docs/appendix/codebase-map.md` for workspace crates, ports tree, and source layouts.

### Adding a New Userspace Binary

Adding a new userspace binary requires changes in **four** places. Missing any one of these causes the binary to either not be built, not be embedded in the kernel image, or not be found at runtime.

1. **Workspace member** — add the crate to `Cargo.toml` `members` list
2. **xtask build pipeline** — add to the `bins` array in `xtask/src/main.rs` (`build_userspace` function, ~line 141). Set `needs_alloc = true` if the crate depends on `alloc` (e.g., uses `kernel-core` or `Vec`/`Box`/`String`). If `needs_alloc` is true, the binary must define a `#[global_allocator]` (use `syscall_lib::heap::BrkAllocator`) and enable the `alloc` feature on `syscall-lib`.
3. **Ramdisk embedding** — add an `include_bytes!` static and a `BIN_ENTRIES` tuple in `kernel/src/fs/ramdisk.rs`. Generated binaries are staged by `xtask` under `target/generated-initrd/`; checked-in static initrd assets remain under `kernel/initrd/`. Without the ramdisk entry, `execve` returns ENOENT.
4. **Service config (if daemon)** — add a `.conf` file to the ext2 data disk builder in `xtask/src/main.rs` (`populate_ext2_files` function) AND to the `KNOWN_CONFIGS` fallback list in `userspace/init/src/main.rs`. Run `cargo xtask clean` to recreate the disk.


## Critical Conventions

### Target flags — do not remove

In `.cargo/config.toml` / target spec:
- `"disable-redzone": true` — hardware interrupts use the stack; removing this causes silent stack corruption
- `"-mmx,-sse"` — disables SIMD to avoid FPU state save/restore on context switches
- `"panic-strategy": "abort"` — no unwinding; panics halt the machine

### `no_std` everywhere in kernel and userspace

All crates under `kernel/` and `userspace/` are `#![no_std]`. Only use `alloc` types (`Vec`, `Box`, `Arc`) after heap initialization. `kernel-core` supports both `no_std` (kernel) and `std` (host tests) via feature flags.

### `unsafe` only at hardware boundaries

Acceptable only for: hardware register/port I/O, page table/GDT/IDT setup, `enter_userspace()`/`switch_context()` asm stubs, global allocator initialization, APIC/ACPI MMIO access, VirtIO ring manipulation. Always wrap in a safe abstraction immediately.

All crates use Rust **edition 2024** — the body of an `unsafe fn` is *not* implicitly unsafe. You must wrap unsafe operations in explicit `unsafe {}` blocks inside unsafe functions.

### IPC model — read the doc before touching `kernel/src/ipc/`

Synchronous rendezvous + async notification objects (seL4-style):
- Server-to-server: sync `call`/`reply_recv`
- IRQ/vsync: `Notification` objects (word-sized bitfield, safe to signal from interrupt handlers)
- Bulk data: page capability grants, never IPC payloads
- Userspace servers must never share writable memory

### Interrupt handlers

Do the minimum: read scancode / ack interrupt / push to ring buffer / send EOI. No allocation, no blocking, no IPC from within an interrupt handler.

### Capabilities

Integer index into the current process's `CapabilityTable`. Kernel validates every handle on every syscall. Transfer via `sys_cap_grant` — never forge or copy raw capability values.

### Syscall ABI

| Register | Role |
|---|---|
| `rax` | Syscall number (in) / return value (out) |
| `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` | Arguments 1–6 |

`rcx` and `r11` are clobbered by `syscall` — never use them for arguments.

### Context switch

`switch_context(current, next)` saves/restores only callee-saved registers (`rbx`, `rbp`, `r12`–`r15`, `rsp`, `rip`). Do not change without auditing every call site.

### SMP conventions

- BSP (bootstrap processor) completes full kernel init before waking APs
- APs initialize their own GDT, IDT, APIC, and enter the scheduler idle loop
- Use IPI for TLB shootdown on page table updates affecting multiple cores
- Per-CPU data accessed via APIC ID — avoid global mutable state without proper locking

### QEMU test exit convention

```rust
// Write to I/O port 0xf4 (isa-debug-exit device)
// QEMU exit codes: 0x21 = success, 0x23 = failure
const QEMU_EXIT_SUCCESS: u32 = 0x10;
const QEMU_EXIT_FAILURE: u32 = 0x11;
```

### Userspace-first rule

New high-level policy defaults to userspace. Before adding policy-heavy code to ring 0, check the architecture review checklist in `docs/appendix/architecture-and-syscalls.md`.

### `BootInfo` is read-only after init

Parse memory regions, framebuffer, RSDP during `kernel_main` init and store in typed kernel structures. Do not hold long-lived references to `BootInfo`.

## Key Crates

| Crate | Purpose |
|---|---|
| `bootloader_api` | Kernel entry point macro, `BootInfo` |
| `x86_64` | `PageTable`, `IDT`, `GDT`, `PhysAddr`/`VirtAddr`, port I/O |
| `uart_16550` | Serial port driver — primary debug output |
| `pic8259` | 8259 PIC init and EOI |
| `spin` | `Mutex`/`RwLock` for `no_std` |
| `log` | Logging facade; backend writes to serial |
| `kernel-core` | Shared pure-logic library, host-testable |

## Documentation in `docs/`

Before making significant changes to a subsystem, read the corresponding phase doc. Full index in `docs/appendix/codebase-map.md`. Roadmaps and task lists live in `docs/roadmap/`.

### Documentation templates — all docs must conform

All roadmap docs must follow the templates in `docs/appendix/doc-templates.md`. When creating or updating docs, use the matching template:

| Doc type | Template section | Required fields |
|---|---|---|
| Phase design doc | `docs/roadmap/NN-slug.md` | Status, Source Ref, Depends on, Builds on, Primary Components, Milestone Goal, Why This Phase Exists, Learning Goals, Feature Scope, Important Components and How They Work, How This Builds on Earlier Phases, Implementation Outline, Acceptance Criteria, Companion Task List, How Real OS Implementations Differ, Deferred Until Later |
| Phase task doc | `docs/roadmap/tasks/NN-slug-tasks.md` | Status, Source Ref, Depends on, Goal, Track Layout table, per-track sections with tasks containing File/Symbol/Why it matters/Acceptance, Documentation Notes |
| Roadmap README row | `docs/roadmap/README.md` | Phase, Theme, Primary Outcome, Status, Source Ref, Milestone link, Tasks link |

Rules:
- Never create a task doc without all template sections populated.
- Never create a design doc missing Status, Source Ref, Depends on, or Builds on.
- Task acceptance items must be concrete and measurable — no vague "works correctly".
- Each task must have File, Symbol, and Why it matters fields.
- Update the roadmap README row when creating or completing a phase.
