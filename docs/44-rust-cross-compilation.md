# Rust Cross-Compilation

**Aligned Roadmap Phase:** Phase 44
**Status:** Complete
**Source Ref:** phase-44

## Overview

Phase 44 enables Rust programs compiled on the host to run natively inside m3OS.
Programs are cross-compiled with `--target x86_64-unknown-linux-musl`, producing
statically-linked ELF binaries that make Linux syscalls. Because m3OS already
implements a Linux syscall compatibility layer (Phase 12), these musl-linked
binaries run without any kernel changes. The phase also introduces a custom
`x86_64-m3os` target spec for `#![no_std]` programs and xtask automation for
building and packaging Rust userspace binaries.

## What This Doc Covers

- How musl-linked Rust programs run on m3OS via the Linux syscall ABI
- The `build_musl_rust_bins()` xtask function and its build pipeline
- The custom `x86_64-m3os.json` target specification for `no_std` programs
- The five demonstration programs and what they exercise
- RUSTFLAGS requirements for non-PIE static binaries

## Core Implementation

### Cross-Compilation with musl

Rust programs targeting m3OS use the standard `x86_64-unknown-linux-musl` target.
This produces statically-linked ELF binaries that:

1. Link against musl libc (not glibc) — fully self-contained, no shared libraries.
2. Make Linux syscalls (`syscall` instruction with the Linux ABI).
3. Run on m3OS because the kernel's Phase 12 compatibility layer handles Linux
   syscall numbers and translates them to native kernel operations.

The key RUSTFLAGS for correct binary generation:

```
-C relocation-model=static -C target-feature=+crt-static
```

This produces `ET_EXEC` (non-PIE) static binaries, which avoids conflicts with
musl's self-relocating CRT startup and matches what the kernel's ELF loader
expects.

Host prerequisites for the full Phase 44 path are explicit:

- `rustup target add x86_64-unknown-linux-musl`
- `strip` is optional; xtask falls back to a plain copy if it is unavailable

### xtask Build Integration (`build_musl_rust_bins`)

The `build_musl_rust_bins()` function in `xtask/src/main.rs` automates the
cross-compilation pipeline:

1. Defines the supported demo crate set: `hello-rust`, `sysinfo-rust`,
   `httpd-rust`, `calc-rust`, and `todo-rust`.
2. Creates placeholder files in `target/generated-initrd/` for each crate so the
   kernel's `include_bytes!` always resolves, even if the musl target is unavailable.
3. Checks that `x86_64-unknown-linux-musl` is installed via `rustup`.
4. Iterates over the five demo crates, building each with:
   ```
    cargo build --manifest-path userspace/<name>/Cargo.toml \
      --target x86_64-unknown-linux-musl --release
   ```
5. Strips debug symbols from the resulting binary (falls back to plain copy).
6. Copies the stripped binary to `target/generated-initrd/<name>` for inclusion in the
   initial ramdisk.

Each demo crate is a standalone project (not a workspace member) with its own
`Cargo.toml` and `userspace/<name>/target/` directory. This avoids polluting the
kernel workspace with `std`-dependent crates.

If the musl target is missing, xtask emits a warning, leaves placeholder files
in `target/generated-initrd/`, and still allows the image build to continue.
That produces a reduced image rather than a hard failure: the Rust std demos are
present as staged names, but they are not runnable until the host has
`x86_64-unknown-linux-musl` installed.

If an individual crate directory is missing or a specific build fails, xtask
warns and leaves that placeholder in place while still packaging the rest of the
image.

### Custom x86_64-m3os Target Spec

The file `x86_64-m3os.json` at the project root defines a custom Rust target for
`#![no_std]` programs that want to identify m3OS at compile time. It is derived
from `x86_64-unknown-none` with one change: `"os": "m3os"`.

Key properties (identical to `x86_64-unknown-none`):
- `disable-redzone: true` — required for interrupt safety
- `-mmx,-sse,+soft-float` — avoids FPU state in context switches
- `panic-strategy: abort` — no unwinding
- `relocation-model: static`, `code-model: kernel`

The target enables `#[cfg(target_os = "m3os")]` for conditional compilation.
Existing `no_std` crates compile unchanged for either target.

### Demonstration Programs

Five programs validate different `std` library subsystems:

| Program | std Feature | What it Validates |
|---|---|---|
| `hello-rust` | `println!` | Basic `std::io::stdout`, process exit |
| `sysinfo-rust` | `std::fs::read_to_string` | File I/O via `/proc/meminfo`, `/proc/uptime` |
| `httpd-rust` | `std::net::TcpListener` | TCP socket bind/accept/write |
| `calc-rust` | `std::io::stdin` | Interactive terminal input/output |
| `todo-rust` | `std::fs::{read,write}` | Persistent file creation and modification |

Each program exercises a different syscall path through musl to the kernel:
`write` (hello), `open`/`read` (sysinfo), `socket`/`bind`/`accept` (httpd),
`read` from stdin (calc), `open`/`write`/`close` (todo).

### Phase 53 Supported Path

The Phase 53 headless/reference baseline treats these five crates as the
supported Rust std demos:

- `hello-rust`
- `sysinfo-rust`
- `httpd-rust`
- `calc-rust`
- `todo-rust`

Host prerequisites for this path are explicit:

1. `rustup target add x86_64-unknown-linux-musl`
2. A normal host Rust toolchain capable of running `cargo xtask image` or
   `cargo xtask run --fresh`

This Rust std path is **manual validation**, not part of the mandatory smoke or
regression bundle. The supported reference check is:

1. Build an image with `cargo xtask image` (or boot with `cargo xtask run --fresh`)
2. Boot the guest
3. Run one or more shipped demos such as `/bin/hello-rust`, `/bin/sysinfo-rust`,
   or `/bin/todo-rust`

That keeps the baseline explicit without implying broader post-1.0 Rust
ecosystem support inside m3OS.

## Key Files

| File | Purpose |
|---|---|
| `xtask/src/main.rs` (`build_musl_rust_bins`) | Cross-compilation and packaging pipeline |
| `x86_64-m3os.json` | Custom Rust target spec for `no_std` programs |
| `userspace/hello-rust/src/main.rs` | Minimal Rust std validation |
| `userspace/sysinfo-rust/src/main.rs` | File I/O via std::fs |
| `userspace/httpd-rust/src/main.rs` | TCP server via std::net |
| `userspace/calc-rust/src/main.rs` | Interactive I/O via std::io |
| `userspace/todo-rust/src/main.rs` | Persistent storage via std::fs |

## How This Phase Differs From Later Work

- This phase uses the Linux syscall ABI (via musl) as a compatibility shortcut.
  A native m3OS `std` backend with OS-specific implementations is deferred.
- Only five hand-written demo programs are cross-compiled. Porting existing Rust
  ecosystem tools (ripgrep, fd, etc.) is possible but deferred.
- Running `cargo` or `rustc` inside m3OS is a stretch goal for future phases.
- Dynamic linking of Rust programs is not supported — all binaries are static.
- Debug symbol support and remote debugging (gdb stub) are deferred.

## Related Roadmap Docs

- [Phase 44 roadmap doc](./roadmap/44-rust-cross-compilation.md)
- [Phase 44 task doc](./roadmap/tasks/44-rust-cross-compilation-tasks.md)

## Deferred or Later-Phase Topics

- Native m3OS `std` backend (custom os module replacing Linux compat)
- Running `cargo` or `rustc` inside the OS
- Crate registry access from inside the OS
- Dynamic linking of Rust programs
- Porting existing Rust CLI tools from the ecosystem
