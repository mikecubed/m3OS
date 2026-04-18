# Phase 44 - Rust Cross-Compilation Pipeline

**Status:** Complete
**Source Ref:** phase-44
**Depends on:** Phase 12 (POSIX Compat) ✅, Phase 14 (Shell and Tools) ✅, Phase 24 (Persistent Storage) ✅
**Builds on:** Extends Phase 12's Linux syscall ABI compatibility layer to run Rust
`std` programs compiled against musl, and extends the xtask build system to handle
a second Rust compilation target alongside the existing `x86_64-unknown-none` path.
**Primary Components:** musl Rust target, custom x86_64-m3os target spec, xtask
build integration, demonstration programs

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

## Why This Phase Exists

The OS already runs musl-linked C programs and `#![no_std]` Rust programs, but
there is no way to write Rust programs that use the full standard library (`std::fs`,
`std::net`, `std::io`) and run them inside the OS. Developers writing userspace
programs face a choice between C (with musl and full libc) or Rust (with `no_std`
and raw syscalls). This phase bridges that gap by enabling musl-linked Rust `std`
programs, which get the ergonomics of Rust's standard library while reusing the
Linux syscall ABI that Phase 12 already provides. It also formalizes a custom
`x86_64-m3os` target spec for `no_std` Rust programs that want to identify the OS
at compile time via `#[cfg(target_os = "m3os")]`.

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
layer), create a custom target specification.

The custom target spec lives at `x86_64-m3os.json` in the project root. It produces
functionally identical code to `x86_64-unknown-none` — the only semantic difference
is `"os": "m3os"`, which enables `#[cfg(target_os = "m3os")]` for conditional
compilation. Existing `no_std` crates do **not** need to migrate; both targets work.

```json
{
    "llvm-target": "x86_64-unknown-none-elf",
    "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
    "arch": "x86_64",
    "os": "m3os",
    "target-endian": "little",
    "target-pointer-width": "64",
    "target-c-int-width": "32",
    "linker-flavor": "gnu-lld",
    "linker": "rust-lld",
    "panic-strategy": "abort",
    "disable-redzone": true,
    "features": "-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2,+soft-float",
    "executables": true,
    "has-thread-local": false,
    "position-independent-executables": false,
    "static-position-independent-executables": false,
    "relocation-model": "static",
    "code-model": "kernel",
    "max-atomic-width": 64,
    "plt-by-default": false,
    "stack-probes": { "kind": "inline" },
    "pre-link-args": { "gnu-lld": ["--gc-sections"] }
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

## Important Components and How They Work

### x86_64-unknown-linux-musl target

The standard Rust target for statically-linked Linux binaries. Programs compiled
with this target use musl libc and make Linux syscalls, which the kernel's Phase 12
compatibility layer handles. This is the primary compilation path for Rust `std`
programs. The target is pre-built (no `-Zbuild-std` needed), which simplifies the
build compared to the kernel's `x86_64-unknown-none` target.

### Custom x86_64-m3os.json target specification

The file `x86_64-m3os.json` at the project root defines a custom Rust compilation
target for m3OS. It is derived from the built-in `x86_64-unknown-none` target
(verified via `rustc --print target-spec-json -Z unstable-options --target x86_64-unknown-none`)
with one key change: `"os": "m3os"`.

- Produces the exact same machine code as `x86_64-unknown-none` — same LLVM target
  (`x86_64-unknown-none-elf`), same data layout, same code model (`kernel`), same
  feature flags (`-mmx,-sse,+soft-float`), same panic strategy (`abort`).
- The primary value is `#[cfg(target_os = "m3os")]` for conditional compilation.
  This lets shared crates include m3os-specific code paths without affecting builds
  for other targets.
- Preserves all critical kernel conventions: `disable-redzone: true` (required for
  interrupt safety), SIMD disabled (avoids FPU state in context switches),
  `panic-strategy: abort` (no unwinding in kernel).
- Existing `no_std` crates do **not** need to migrate — any crate that compiles for
  `x86_64-unknown-none` also compiles for `x86_64-m3os.json` without changes.
- Programs compiled for this target use `#![no_std]` and call m3OS syscalls directly
  via the `syscall-lib` crate.

### xtask build integration (build_musl_rust_bins)

A new function in xtask that manages cross-compilation of musl-linked Rust crates.
It invokes `cargo build --target x86_64-unknown-linux-musl --release`, strips the
resulting binaries, and copies them to `kernel/initrd/` for inclusion in the disk
image. This follows the same pattern as the existing `build_ion()` function.

### Demonstration programs

Five Rust `std` programs that exercise different areas of the standard library:
`hello-rust` (basic validation), `sysinfo-rust` (`std::fs`), `httpd-rust`
(`std::net`), `calc-rust` (`std::io`), and `todo-rust` (`std::fs` persistence).
Each validates a different syscall path through musl to the kernel.

## How This Builds on Earlier Phases

- Extends Phase 12 by running Rust `std` programs through the same Linux syscall
  ABI that already supports musl-linked C programs.
- Extends Phase 14 by adding Rust binaries that can be launched from the shell
  alongside existing C and `no_std` Rust programs.
- Extends Phase 24 by using persistent disk storage for todo data and other file
  I/O from Rust programs.
- Extends the xtask build system (used since Phase 1) with a third compilation
  path for musl-linked Rust binaries.

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

- [Phase 44 Task List](./tasks/44-rust-cross-compilation-tasks.md)

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

## Status Notes (Closure)

Phase 44 closes with the musl-linked `std` path as the supported way to ship
Rust programs that use the standard library. All five demonstration crates exist
under `userspace/{hello,sysinfo,httpd,calc,todo}-rust/`, each as a non-workspace
crate (`[workspace]` table at the top of every `Cargo.toml`) so that the
workspace-wide `x86_64-unknown-none` default target does not interfere with the
musl build. The xtask integration lives in `xtask/src/main.rs` —
`build_musl_rust_bins()` (line 430) compiles the crates with
`--target x86_64-unknown-linux-musl`, sets `RUSTFLAGS="-C relocation-model=static
-C target-feature=+crt-static"` to produce ET_EXEC binaries that the kernel ELF
loader accepts, strips the outputs, and stages them under
`target/generated-initrd/` for embedding via the
`generated_initrd_asset!` macro in `kernel/src/fs/ramdisk.rs`. The custom
`x86_64-m3os.json` target spec exists at the project root for `no_std` programs
that want to identify the OS at compile time via `#[cfg(target_os = "m3os")]`;
it is functionally equivalent to `x86_64-unknown-none` and does not require
existing crates to migrate. The full residual gap analysis and recommended
follow-on work live in [`docs/appendix/rust-std-userspace.md`](../appendix/rust-std-userspace.md).
