# Phase 43 - Rust Cross-Compilation Pipeline

## Milestone Goal

Rust programs compiled on the host run natively inside the OS. A documented toolchain
setup enables writing Rust code on the host, cross-compiling to an x86-64 ELF binary
linked against musl, and deploying it to the OS's disk image. Stretch goal: a minimal
Rust program compiles inside the OS itself.

## Learning Goals

- Understand Rust's cross-compilation model: targets, sysroots, and linkers.
- Learn how `#![no_std]` Rust programs interact with a minimal libc.
- See the difference between `std` programs (full libc) and `no_std` programs
  (bare syscalls) on a non-Linux OS.
- Experience building a custom Rust target specification.

## Feature Scope

### Path 1: musl-linked Rust binaries (primary)

Since the OS already runs musl-linked C programs (Phase 12), Rust programs compiled
with `--target x86_64-unknown-linux-musl` should work with minimal effort:

```bash
# On the host
rustup target add x86_64-unknown-linux-musl
cargo build --target x86_64-unknown-linux-musl --release
# Copy target/x86_64-unknown-linux-musl/release/myprogram to disk image
```

This works because our OS implements the Linux syscall ABI. The resulting binary
makes syscalls that our kernel handles.

**What this enables:**
- Any pure Rust program (no C dependencies) compiles and runs.
- Programs using `std::fs`, `std::net`, `std::process` work if the underlying
  syscalls are implemented.
- Can port existing Rust CLI tools (ripgrep, fd, bat, etc.) if they fit in the
  disk image and their syscall needs are met.

### Path 2: Custom Rust target (no_std)

For programs that want to use our native syscall ABI directly (not the Linux compat
layer), create a custom target specification:

```json
{
    "llvm-target": "x86_64-unknown-none",
    "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
    "arch": "x86_64",
    "os": "m3os",
    "env": "musl",
    "linker": "rust-lld",
    "panic-strategy": "abort",
    "disable-redzone": true,
    "features": "-mmx,-sse"
}
```

This enables `#![no_std]` Rust programs with a thin m3os syscall crate.

### Path 3: Rust inside the OS (stretch goal)

This is extremely ambitious but worth documenting as a long-term goal:
- Cross-compile `rustc` and `cargo` with musl (they are large binaries: ~50 MB+).
- Bundle in the disk image or make available via the network.
- Compile simple Rust programs inside the OS.
- This may require significantly more memory and disk space than earlier phases.

More realistically: port `mrustc` (a Rust compiler written in C++) which is simpler
and designed for bootstrapping, or write a minimal Rust-subset compiler.

### Demonstration Programs

Write and cross-compile several Rust programs to showcase the OS:

1. **`hello-rust`** — "Hello from Rust on m3OS!" (validates basic std works)
2. **`sysinfo`** — System information tool using std::fs to read /proc-like data
3. **`httpd`** — Minimal HTTP server using std::net (demonstrates networking from Rust)
4. **`calc`** — Interactive calculator using std::io (demonstrates terminal I/O)
5. **`todo`** — Persistent todo list using std::fs (demonstrates file I/O)

### xtask Integration

Extend the `cargo xtask` build system to:
- Cross-compile Rust userspace programs automatically.
- Include Rust binaries in the disk image alongside C binaries.
- Support both musl-linked and no_std Rust targets.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 12 (POSIX Compat) | Linux syscall ABI for musl-linked binaries |
| Phase 14 (Shell and Tools) | Run Rust programs from the shell |
| Phase 24 (Persistent Storage) | Store Rust programs on disk |

## Implementation Outline

1. Set up `x86_64-unknown-linux-musl` target on the host.
2. Write and cross-compile `hello-rust` — verify it runs in the OS.
3. Identify and implement any missing syscalls that `std` needs.
4. Write the demonstration programs.
5. Extend xtask to cross-compile and package Rust userspace programs.
6. Create the custom m3os target specification for `no_std` programs.
7. Write a `syscall` crate for native m3os programs.
8. Document the full cross-compilation workflow.
9. (Stretch) Investigate porting mrustc or rustc.

## Acceptance Criteria

- `hello-rust` runs inside the OS and prints its message.
- A Rust program using `std::fs` can read and write files.
- A Rust program using `std::net` can open TCP connections.
- The xtask build system can cross-compile and package Rust programs.
- The cross-compilation workflow is documented and reproducible.
- At least 3 non-trivial Rust demonstration programs work.

## Companion Task List

- Phase 43 Task List — *not yet created*

## How Real OS Implementations Differ

Real OS projects with Rust support (Redox, Fuchsia, Linux) have:
- Full `std` library ports with OS-specific backends
- Cargo registry access for pulling dependencies
- CI/CD pipelines for continuous cross-compilation
- Debug symbol support and remote debugging

Our approach leverages the Linux syscall compatibility layer to avoid writing a full
`std` backend. This is a pragmatic shortcut: musl-linked Rust binaries think they're
running on Linux, and our kernel provides enough of the Linux ABI to make them work.

## Deferred Until Later

- Native `std` backend for m3os (custom os module)
- Running `cargo` inside the OS
- Crate registry access from inside the OS
- Remote debugging (gdb stub)
- Dynamic linking of Rust programs
