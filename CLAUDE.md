# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**ostest** is a toy bootable OS in Rust: microkernel architecture, x86_64, UEFI boot. Goal is a functional userspace shell. Currently at Phase 1 (boot foundation complete).

## Build & Run

Uses the `xtask` pattern — always build through `cargo xtask`, never `cargo build` directly.

```bash
cargo xtask run          # build + launch in QEMU (primary dev workflow)
cargo xtask image        # build bootable disk image (UEFI raw + VHDX)
cargo xtask check        # clippy (-D warnings) + rustfmt check
cargo xtask fmt --fix    # auto-format all kernel source
cargo xtask test         # run all kernel tests in QEMU
cargo xtask test --test <name>  # run a single test
```

Tests cannot use `cargo test` — the kernel is `no_std` and tests run inside QEMU via the xtask harness.

## Architecture

Microkernel: ring 0 kernel handles only memory management, scheduling, IPC, and interrupt routing. Everything else is a ring 3 userspace server communicating through IPC.

```
Ring 0 (kernel/):           Ring 3 (userspace/ — planned Phase 5+):
  - Frame allocator           - init, console_server, vfs_server
  - Page table manager        - fat_server, kbd_server, shell
  - Scheduler
  - IPC engine
  - IDT / interrupt router
  - Syscall gate
```

### Workspace

```
kernel/src/
  arch/x86_64/   # GDT, IDT, paging, syscall gate
  mm/            # frame allocator, page tables, heap
  task/          # scheduler, context switch
  ipc/           # endpoints, capabilities, send/recv
  drivers/       # serial only (uart_16550 wrapper)
xtask/           # build system (host, std)
userspace/       # ring 3 server binaries (no_std) — not yet implemented
```

## Critical Conventions

### Target flags — do not remove

In `.cargo/config.toml` / target spec:
- `"disable-redzone": true` — hardware interrupts use the stack; removing this causes silent stack corruption
- `"-mmx,-sse"` — disables SIMD to avoid FPU state save/restore on context switches
- `"panic-strategy": "abort"` — no unwinding; panics halt the machine

### `no_std` everywhere in kernel and userspace

All crates under `kernel/` and `userspace/` are `#![no_std]`. Only use `alloc` types (`Vec`, `Box`, `Arc`) after Phase 2 heap initialization.

### `unsafe` only at hardware boundaries

Acceptable only for: hardware register/port I/O, page table/GDT/IDT setup, `enter_userspace()`/`switch_context()` asm stubs, global allocator initialization. Always wrap in a safe abstraction immediately.

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

### QEMU test exit convention

```rust
// Write to I/O port 0xf4 (isa-debug-exit device)
// QEMU exit codes: 0x21 = success, 0x23 = failure
const QEMU_EXIT_SUCCESS: u32 = 0x10;
const QEMU_EXIT_FAILURE: u32 = 0x11;
```

### `BootInfo` is read-only after init

Parse memory regions, framebuffer, RSDP during `kernel_main` init and store in typed kernel structures. Do not hold long-lived references to `BootInfo`.

## Key Crates

| Crate | Purpose |
|---|---|
| `bootloader_api` | Kernel entry point macro, `BootInfo` |
| `x86_64` | `PageTable`, `IDT`, `GDT`, `PhysAddr`/`VirtAddr`, port I/O |
| `uart_16550` | Serial port driver — primary debug output |
| `pic8259` | 8259 PIC init and EOI |
| `linked_list_allocator` | `#[global_allocator]` for kernel heap |
| `spin` | `Mutex`/`RwLock` for `no_std` |
| `log` | Logging facade; backend writes to serial |

## Documentation in `docs/`

Read before making significant changes:

| File | When |
|---|---|
| `docs/01-architecture.md` | Orientation — kernel vs. userspace split |
| `docs/06-ipc.md` | Before touching `kernel/src/ipc/` or syscalls |
| `docs/03-memory.md` | Before touching frame allocator, page tables, or heap |
| `docs/08-roadmap.md` | Open design questions and per-phase scope |
| `docs/09-testing.md` | Before writing kernel tests or modifying the xtask harness |
