# Copilot Instructions — ostest

ostest is a toy bootable operating system written in **Rust**, following a **microkernel
architecture**, targeting **x86_64** with UEFI boot. The codebase is educational but
aims for a functional userspace shell. See `docs/` for full design documentation.

---

## Build & Run Commands

> The project uses an `xtask` pattern — a Cargo workspace member that acts as the build system.

```bash
# Build kernel + create bootable disk image
cargo xtask image

# Build + launch in QEMU (primary dev workflow)
cargo xtask run

# Run kernel tests (QEMU-based, no native test runner)
cargo xtask test

# Run a single test (by name)
cargo xtask test --test <test_name>
```

> ⚠️ The kernel itself cannot be tested with `cargo test` directly — it requires a
> `no_std` target. All tests run inside QEMU via the xtask harness.

The custom x86_64 target is defined in `.cargo/config.toml`. Always build via `cargo xtask`,
not `cargo build`, or you will get the wrong target and linker flags.

**Critical target flags** (do not remove these from the target JSON):
- `"disable-redzone": true` — hardware interrupts use the stack; the red zone would be silently corrupted without this, causing heisenbugs
- `"-mmx,-sse"` — disables SIMD so we don't have to save/restore FPU state on every context switch
- `"panic-strategy": "abort"` — no unwinding in the kernel; panics halt the machine

If you add a new kernel crate, copy the target spec and `.cargo/config.toml` runner config — do not use the host target.

---

## Architecture

This is a **microkernel**: the kernel (ring 0) does only memory management, thread
scheduling, IPC, and interrupt routing. **Everything else runs in userspace ring 3 servers**
that communicate exclusively through IPC. No kernel modules. No in-kernel drivers.

### Privilege Boundary

```
Ring 0 (kernel/):           Ring 3 (userspace/):
  - Frame allocator           - init         (spawns all servers)
  - Page table manager        - console_server
  - Scheduler                 - vfs_server
  - IPC engine                - fat_server   (filesystem)
  - IDT / interrupt router    - kbd_server   (keyboard driver)
  - syscall gate              - shell
```

### Workspace Layout

```
ostest/
├── kernel/          # the microkernel (no_std, ring 0)
│   └── src/
│       ├── arch/x86_64/   # GDT, IDT, paging, syscall gate
│       ├── mm/            # frame allocator, page tables, heap
│       ├── task/          # scheduler, context switch
│       ├── ipc/           # endpoints, capabilities, send/recv
│       └── drivers/       # serial only (uart_16550 wrapper)
├── userspace/       # ring 3 server binaries (no_std)
│   ├── init/
│   ├── console_server/
│   ├── vfs_server/
│   ├── fat_server/
│   ├── kbd_server/
│   └── shell/
└── xtask/           # build system (host, std)
```

---

## Key Conventions

### `no_std` everywhere in the kernel and userspace

All crates under `kernel/` and `userspace/` are `#![no_std]`. Use `alloc` types (`Vec`,
`Box`, `Arc`) only after the kernel heap is initialized (Phase 2+). Do not add `std`
dependencies to these crates.

### Unsafe only at hardware boundaries

`unsafe` is only acceptable when:
- Reading/writing hardware registers or I/O ports
- Setting up page tables, GDT, IDT entries
- The initial `enter_userspace()` / `switch_context()` asm stubs
- Initializing the global allocator or static mut singletons

Wrap every `unsafe` operation in a safe abstraction immediately. Don't leave raw
`unsafe` blocks in business logic.

### Syscall ABI

The kernel uses a custom syscall convention (see `docs/07-userspace.md`):

| Register | Role |
|---|---|
| `rax` | Syscall number (in) / return value (out) |
| `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9` | Arguments 1–6 |

`rcx` and `r11` are **clobbered** by the `syscall` instruction — never use them for arguments.

### IPC model is decided — read the doc before implementing

The IPC model is **synchronous rendezvous + async notification objects** (seL4-style).
Read `docs/06-ipc.md` before touching anything in `kernel/src/ipc/` or adding new
syscalls. Key points:
- All server-to-server communication uses sync `call`/`reply_recv`
- IRQ delivery and vsync use `Notification` objects (a word-sized bitfield, safe to
  signal from interrupt handlers)
- Bulk data (framebuffer pixels, file blocks) moves via **page capability grants**, never
  through IPC message payloads

### IPC is the only cross-process channel

Userspace servers must **never** share writable memory. All inter-server communication
goes through the kernel IPC mechanism (`sys_call` / `sys_reply_recv`). The server loop
pattern is:

```rust
let mut msg = recv(my_endpoint);
loop {
    let response = handle(msg);
    msg = reply_recv(client_cap, response); // reply + wait for next
}
```

### Interrupt handlers are minimal

Hardware interrupt handlers (in `kernel/src/arch/x86_64/`) do the least possible work:
read the scancode / acknowledge the interrupt / push to a ring buffer / send EOI.
No allocation, no blocking, no IPC from within an interrupt handler.

### Capabilities are handles into a per-process table

A capability is an integer index into the current process's `CapabilityTable`. The
kernel validates every handle on every syscall. You cannot forge or guess a capability.
When passing a capability to another process, use `sys_cap_grant` over IPC — the kernel
transfers the entry atomically.

### Boot information is read-only after init

`BootInfo` is passed by the `bootloader` crate at kernel entry. Parse everything you
need from it during `kernel_main` initialization (memory regions, framebuffer, RSDP)
and store it in typed kernel structures. Do not hold a long-lived reference to `BootInfo`
beyond the init sequence.

### QEMU test exit convention

Kernel tests signal pass/fail to the xtask harness via the **ISA debug exit device**
at I/O port `0xf4` (QEMU `-device isa-debug-exit`). The convention:

```rust
// exit codes seen by xtask: 0x21 = success, 0x23 = failure
// (QEMU doubles the value and ORs 1, so write 0x10 for success, 0x11 for failure)
const QEMU_EXIT_SUCCESS: u32 = 0x10;
const QEMU_EXIT_FAILURE: u32 = 0x11;

fn qemu_exit(code: u32) -> ! {
    unsafe { Port::new(0xf4).write(code) };
    loop { x86_64::instructions::hlt(); }
}
```

Tests print results to serial, then call `qemu_exit`. The xtask harness reads the exit
code and serial output to report pass/fail. See `docs/09-testing.md` for the full harness design.

### Context switch saves only callee-saved registers

The `switch_context(current, next)` assembly stub saves/restores only the callee-saved
registers (`rbx`, `rbp`, `r12`–`r15`, `rsp`, `rip`). The Rust compiler handles
caller-saved registers at call sites. Do not modify this convention without auditing
every call site.

---

## Key Crates

| Crate | What it provides |
|---|---|
| `bootloader_api` | Kernel entry point macro, `BootInfo` type |
| `x86_64` | `PageTable`, `IDT`, `GDT`, `PhysAddr`/`VirtAddr`, port I/O |
| `uart_16550` | Serial port driver — primary debug output |
| `pic8259` | 8259 PIC init and EOI — hardware interrupts |
| `linked_list_allocator` | `#[global_allocator]` for the kernel heap |
| `spin` | `Mutex`/`RwLock` for `no_std` |
| `log` | Logging facade; backend writes to serial |

---

## Documentation

All design documentation is in `docs/`. Read these before making significant changes:

| File | When to read it |
|---|---|
| `docs/01-architecture.md` | Orientation — what lives in the kernel vs. userspace |
| `docs/06-ipc.md` | Before touching anything in `kernel/src/ipc/` or syscalls |
| `docs/03-memory.md` | Before touching frame allocator, page tables, or heap |
| `docs/08-roadmap.md` | Open design questions and per-phase scope |
| `docs/09-testing.md` | Before writing kernel tests or modifying the xtask harness |
