# Phase 95 - Native Rust Toolchain (on-device `rustc` + `cargo`)

**Status:** Planned
**Source Ref:** phase-95
**Depends on:** Phase 91 (Dynamic C Runtime — `libc.so` for proc-macro `dlopen`), Phase 94 (Rust-cargo musl port class), Phase 85d ✅ (Clang/LLVM/LLD on-device precedent + large-exec / `pread64` kernel fixes), Phase 92 (VFS bulk-I/O — the large install), Phase 93 (VFS `stat` conformance), Phase 44 ✅ (Rust cross-compilation lineage)
**Builds on:** Extends the Phase 44 → 94 "Rust runs in the OS" lineage from *running* host-cross-compiled Rust to *running the Rust toolchain itself* on-device. Reuses the Phase 85d Clang/LLVM/LLD delivery pattern (the project's only existing on-device native code generator) and the Phase 85a `.m3pkg` substrate. It is the Rust analog of Phase 85d, not of Phase 86d.
**Primary Components:** `ports/lang/rust/Portfile`, `xtask/src/port_build.rs` (`build_rust`), the bundled `rust-lld` linker (or a `DEPS=clang` dependency on the Phase 85d LLD), a userspace Rust target spec + a prebuilt `std`/`core` sysroot, the `M3OS_WITH_RUST` image feature, the `rustc-smoke` / `cargo-smoke` gates

## Milestone Goal

A native `rustc` runs **on m3OS** and compiles a Rust source file to a working
native executable that also runs on m3OS — `rustc hello.rs && ./hello` prints
its output — using the bundled `rust-lld` to link and the standard library
supplied as a prebuilt sysroot resolved relative to the compiler binary. m3OS
becomes self-hosting for Rust the same way Phase 85d made it self-hosting for
C/C++.

A stretch milestone adds `cargo build` of a small **proc-macro-free** crate.
Full `cargo` against the mainstream crate ecosystem (which is saturated with
derive macros) and crates.io registry access are explicitly gated on Phase 91
(a real `libc.so` for proc-macro `dlopen`) and a registry-fetch path, and are
called out under *Deferred Until Later*.

## Why This Phase Exists

Every Rust milestone so far compiles Rust **on the host** and ships the result
into the OS:

- Phase 44 cross-compiles five `std` demo crates for `x86_64-unknown-linux-musl`.
- Phase 94 establishes the first Rust-*cargo* cross-build port class and ships
  upstream `uutils/coreutils` as a prebuilt `.m3pkg`.
- The kernel and all hand-built userspace are bare-metal `x86_64-unknown-none`
  Rust, also built on the host.

No Rust *toolchain* runs on m3OS. This phase closes that gap, and the design
problem it solves is precisely the one Phase 85d already solved for C/C++:
**on-device native code generation by an LLVM-based compiler.**

The single most important framing for this phase — and the reason it is *not* a
small follow-on to Phase 86d — is the distinction between running a *program*
and running a *compiler*:

| Toolchain phase | What runs on m3OS | Generates machine code on-device? |
|---|---|---|
| Go (86d) | one pre-built static Go *binary* | **No** — the Go compiler runs only on the host |
| Python (85c) | the CPython interpreter | No — interprets/bytecode-compiles, emits no native code |
| **Clang/LLVM/LLD (85d)** | the clang + lld binaries | **Yes** — compiles + links + runs C/C++ inside m3OS |
| **Rust (this phase)** | the `rustc` + linker binaries | **Yes** — the goal |

Phase 86d proving "a Go binary runs on m3OS" is *not* a precedent for "the Rust
toolchain runs on m3OS" — m3OS userspace has run native Rust binaries since
Phase 5. The correct precedent is Clang (85d). rustc is, like clang, a large
LLVM-based C++/Rust program that does positional file I/O, allocates heavily,
and must be loaded as a multi-tens-of-MB ELF — all of which Phase 85d already
forced the kernel to support.

It cannot be folded into Phase 94: Phase 94 *cross-compiles* Rust programs on
the host and ships the binary; this phase ships the *compiler* and generates
code on the device. They share the `.m3pkg` substrate and nothing else.

## Learning Goals

- Why an LLVM-based compiler (clang, rustc) is the heaviest class of on-device
  program, and which kernel features that forces — the streaming ELF exec
  loader, positional I/O (`pread64`/`pwrite64`), generous `rlimit`s, and
  consistent `stat` identity (all landed in Phase 85d).
- The difference between *host-cross-compiling* Rust (Phases 44, 94) and
  *self-hosting* the toolchain on the device (this phase), and why the latter is
  a categorically larger problem.
- Why Rust **proc-macros** require the compiler to `dlopen` a shared object at
  compile time, and therefore why a real `libc.so` (Phase 91) — not just the
  dynamic-linker machinery (Phase 76, already done) — is a hard prerequisite for
  the mainstream crate ecosystem, even though the toolchain *binary* itself is
  static.
- How a Rust **sysroot** (prebuilt `libstd`/`libcore` rlibs + a target spec) is
  resolved relative to the compiler binary, mirroring the Phase 85d clang
  resource-dir relocation contract.
- The **bootstrap problem**: upstream ships a dynamic toolchain, so producing a
  fully-static musl `rustc`/`cargo` for m3OS is a multi-stage host build, the
  same discipline Phase 85d applied to clang.

## Feature Scope

### Area A — A static `rustc` (host-cross-built)

A new `build_rust()` dispatch in `xtask/src/port_build.rs`, paralleling
`build_llvm()` (85d) and `build_go()` (86d), produces a **fully static**
`x86_64-unknown-linux-musl` `rustc` (and, for Area D, `cargo`). Upstream Rust
ships a dynamically-linked toolchain, so this is a deliberate static build —
the same constraint that forced static clang/python/go, because m3OS's
`ld-musl` has no real `libc.so` until Phase 91. The host's own Rust toolchain
is the build compiler (the analog of "host clang cross-compiles LLVM" in 85d).

### Area B — Bundled linker + sysroot relocation contract

- **Linker.** `rustc` does not link; it invokes an external linker. m3OS has no
  GNU `ld`, but Phase 85d already ships **LLD** on-device. This phase either
  bundles `rust-lld` (Rust's vendored LLD, `rustc`'s default) or declares
  `DEPS=clang` to reuse the Phase 85d `ld.lld`. The "no system linker" problem
  is therefore already solved upstream of this phase.
- **Sysroot.** A prebuilt `libstd`/`libcore`/`liballoc` rlib set plus a
  userspace target spec is bundled and resolved **relative to the `rustc`
  binary**, exactly like clang's resource dir. Note: the existing
  `x86_64-m3os.json` is the **kernel** target (soft-float, `code-model: kernel`)
  and is *not* a userspace target — a userspace target spec with a prebuilt std
  is new work with no precedent in the tree (Go shipped no toolchain; clang
  ships a C/C++ sysroot, not a Rust one).

### Area C — On-device `rustc hello.rs` (the milestone)

Prove that the installed `rustc` compiles a Rust source file to a native ELF
on m3OS, links it via the bundled LLD, and runs it — the Rust analog of the
Phase 85d `CLANG_C_OK` gate. This is achievable **without** Phase 91 as long as
the compiled program and its dependency graph use **no proc-macros** (the
distribution sysroot's `std` is precompiled, so the milestone program links
against rlibs and never asks `rustc` to `dlopen` anything).

### Area D — `cargo` + proc-macros (gated on Phase 91) — stretch

`cargo build` of a real crate almost always pulls a proc-macro crate
(`serde_derive`, `thiserror`, …). A proc-macro is a `cdylib`/`dylib` `.so` that
`rustc` **`dlopen`s at compile time**; it references the Rust runtime and libc
symbols. With no `libc.so` in scope every external relocation in that `.so` is
undefined and the load fails. So:

- A static `rustc` compiling **proc-macro-free** crates works after Areas A–C.
- Anything using derive macros — i.e. most of the ecosystem — requires
  **Phase 91** (`libc.so`) **and** loader TLS support first. This is the Rust
  analog of the same "static-only because no `libc.so`" wall that clang, Python,
  and Go all hit, and it is hit harder here because proc-macros are pervasive.

`cargo`'s crates.io registry fetch additionally needs a working HTTPS path
(present in userspace since Phase 86c) wired into cargo, plus `build.rs`
execution (subprocess spawn — and for `cc`-using crates, the on-device clang
from 85d). Treated as stretch / follow-on, not core to the milestone.

### Area E — Packaging + smoke gate

Reuse the Phase 85a substrate end-to-end: Portfile → `build_rust` →
`seal_package` → content-addressed `.m3pkg` → bundled into `/usr/pkg/` behind an
**`M3OS_WITH_RUST`** image feature (copying the `M3OS_WITH_CLANG` opt-in block
verbatim, because a 200–500 MB toolchain must be opt-in exactly like clang's
~125 MB artifact) → `pkg install rust`. A `rustc-smoke` gate validates Area C
on-device; a `cargo-smoke` gate (post-91) validates Area D.

## Important Components and How They Work

### `build_rust()` (new, `xtask/src/port_build.rs`)

Registered in the `fn port_build` `match name` dispatch and given a
`build_recipe_id("rust")` arm (the content-key contract: any host-built port's
configure flags must be transcribed there or its cached `.m3pkg` goes stale).
Like `build_go`, it branches *before* the shared musl-gcc plumbing
(`find_musl_cc` / `musl_extra_ldflags_joined`) because the Rust toolchain
bootstraps with its own compiler, not `x86_64-linux-musl-gcc`. It stages a
DESTDIR-style `usr/` tree (the static `rustc`/`cargo` binaries, the bundled LLD,
the std sysroot) that `seal_package` strips and packs.

### The proc-macro / `libc.so` dependency (the wall)

The dynamic-linker *machinery* is already complete (Phase 76 → 76d: dlopen,
PLT lazy resolve, GNU-hash, symbol versioning). What is missing — and what
Phase 91 supplies — is a real `libc.so` for a loaded `.so` to bind its
`malloc`/`memcpy`/`__errno_location` relocations against, plus the loader
extensions a real libc forces that the synthetic test `.so`s never exercised:
**TLS** (general-dynamic, `TPOFF`/`DTPMOD`), copy relocations, and IFUNC. TLS is
the dominant risk inside Phase 91 — it is unstarted, has no owning phase, and is
required by both `__libc_start_main` and all of Rust `std`. Until Phase 91 lands
TLS + `libc.so`, on-device Rust is limited to proc-macro-free compilation.

### The linker (reused)

`rustc`'s default linker driver is `rust-lld` (LLVM LLD). Phase 85d already
proved LLD links real programs on m3OS (`clang -fuse-ld=lld`). This phase reuses
that work — either by bundling `rust-lld` in the Rust `.m3pkg` or by depending
on the clang port's `ld.lld`.

### The sysroot + userspace target spec (new)

`rustc` finds `std` via a sysroot directory laid out relative to its own binary.
This phase bundles the prebuilt rlib sysroot for a m3OS *userspace* target and
ensures `rustc --print sysroot` resolves under `/usr`. The userspace target spec
is new (the kernel `x86_64-m3os.json` is unusable for userspace programs).

### Reused kernel features from Phase 85d (no new kernel work expected for Area C)

Running a multi-tens-of-MB static rustc reuses the exact bring-up clang forced:
the **streaming ELF exec loader** + `DiskElfSource` (binaries far larger than
the kernel heap; 512 MiB cap), **`pread64`/`pwrite64`** ("THE compile blocker"
for LLVM-class positional I/O), `getrlimit`/`prlimit64`, and the `fstat`
inode-identity fix (recursive-include dedup; the residual systemic pass is
Phase 93). The "binary exceeds the kernel heap" class is permanently closed.

### Alternative bootstrap: `mrustc` (smaller, no LLVM, no proc-macro `dlopen`)

If "compile *some* Rust on-device" is acceptable short of the full toolchain,
`mrustc` (a C++ Rust-subset compiler that emits C and needs no LLVM) is a far
smaller on-device target and sidesteps proc-macro `dlopen` for its bootstrap
subset. It is a legitimate first cut that does not depend on Phase 91, at the
cost of language-version and feature coverage. Recorded as an option, not the
recommended path for a *general* Rust toolchain.

## How This Builds on Earlier Phases

- **Reuses Phase 85d** as the direct precedent: the on-device LLVM-class
  delivery pattern, the bundled LLD, and the kernel fixes (streaming exec,
  positional I/O, rlimits, fstat identity) that running clang already forced.
- **Reuses the Phase 85a** `.m3pkg`/pkgcache substrate and the `M3OS_WITH_*`
  opt-in image-feature pattern unchanged.
- **Depends on Phase 91** for proc-macro support (the dynamic `libc.so` + loader
  TLS), which is the gate to the mainstream crate ecosystem.
- **Depends on Phase 94** for the host-side Rust-cargo port-class plumbing and
  the userspace musl target lineage it establishes.
- **Benefits from Phases 92 and 93**: the large install rides the VFS bulk-I/O
  throughput work (92), and `cargo`/`build.rs` correctness benefits from the
  `stat` conformance pass (93).
- **Continues the Phase 44 → 94** "Rust in the OS" lineage by flipping it from
  host-compiled to device-compiled.

## Implementation Outline

1. **De-risk the bootstrap (Track A).** Cross-build a minimal fully-static musl
   `rustc` on the host; confirm it loads and runs on m3OS through the Phase 85d
   streaming exec path (`rustc --version`, `rustc --print sysroot`).
2. **Sysroot + target (Track B).** Define the userspace target spec, bundle the
   prebuilt `std`/`core` rlibs, and prove `rustc --print sysroot` resolves under
   `/usr` (the relocation contract).
3. **Linker (Track B).** Bundle `rust-lld` or wire `DEPS=clang`; confirm `rustc`
   links a hello-world ELF on-device.
4. **Milestone (Track C).** `rustc hello.rs && ./hello` runs on m3OS
   (`RUSTC_OK`). No proc-macros in the dependency graph.
5. **Packaging (Track E).** Portfile + `build_rust` + `build_recipe_id` arm;
   bundle behind `M3OS_WITH_RUST`; `pkg install rust`; `rustc-smoke` gate at a
   long `--timeout` (the multi-hundred-MB install + cold rustc load over the
   slow ring-3 VFS — mitigated by Phase 92).
6. **(Post-91, stretch) cargo + proc-macros (Track D).** Once Phase 91 ships
   `libc.so` + loader TLS, bundle `cargo`, prove `cargo build` of a
   proc-macro-free crate, then a derive-macro crate via on-device `dlopen` of
   the proc-macro `.so`; `cargo-smoke` gate.
7. **Document + version.** Learning doc, README row update, bump
   `kernel/Cargo.toml` on landing.

## Acceptance Criteria

- `cargo xtask port build rust` produces a fully-static `x86_64-unknown-linux-musl`
  `rustc` and seals a valid `rust.m3pkg` (`pkg_format::verify` passes).
- On m3OS, behind `M3OS_WITH_RUST`: `pkg install rust` succeeds and
  `rustc --version` reports the pinned toolchain version (proving the static
  rustc *runs* via the Phase 12 compat layer + Phase 85d streaming loader).
- `rustc --print sysroot` resolves under `/usr` (relocation contract honored).
- **Area C milestone:** `rustc /usr/src/hello.rs -o /tmp/hello` compiles and
  links via the bundled LLD, and `/tmp/hello` runs on m3OS and prints its output
  (`RUSTC_OK`) — with **no proc-macros** in the dependency graph.
- `rustc-smoke` PASSES end-to-end in CI under an opt-in
  `M3OS_RUST_REGRESSION=1` gate (skip-with-reason when the host Rust toolchain is
  absent, mirroring `clang-smoke` / `git-https-smoke`).
- **Area D (post-91, may be a separate sub-phase):** with Phase 91's `libc.so` +
  loader TLS present, `cargo build` of a proc-macro-free crate succeeds
  on-device, and a derive-macro crate compiles via on-device `dlopen` of the
  proc-macro `.so` (`CARGO_PROCMACRO_OK`).
- The hand-built `no_std` userspace and the kernel build are unaffected (this
  phase adds a userspace toolchain, it does not change how m3OS itself is built).

## Companion Task List

- Phase 95 task list — defer until implementation planning begins (gated behind
  the unbuilt Phase 91 prerequisite for the proc-macro half; the Area A–C half
  can be planned once Phase 94's port-class plumbing lands).

## How Real OS Implementations Differ

- A real distro ships a **dynamic** `rustc`/`cargo` linked against the system
  `libc.so` and `librustc_driver.so`, installed via `rustup`/the package manager
  onto a writable root, with full crates.io registry access over TLS. m3OS must
  build the toolchain **fully static** (no `libc.so` until Phase 91) and installs
  it offline from a bundled `.m3pkg`.
- Real toolchains support proc-macros out of the box because the host always has
  a dynamic libc to `dlopen` a `.so` against; on m3OS that capability is a
  distinct, sequenced prerequisite (Phase 91).
- Distros do not carry a separate bare-metal `no_std` floor; m3OS keeps its
  `x86_64-unknown-none` kernel/userspace lineage entirely separate from this
  userspace `x86_64-unknown-linux-musl` toolchain.
- Production `cargo` does network dependency resolution, build-script execution,
  incremental compilation caching, and parallel codegen units; this phase
  targets the on-device-compile milestone, not feature parity.

## Deferred Until Later

- **Mainstream `cargo` + proc-macros** until Phase 91 ships `libc.so` + loader
  TLS (the hard gate; may itself be split into a `95b` once 91 lands).
- **crates.io registry access** (cargo HTTPS fetch wired to the Phase 86c TLS
  stack) and **`build.rs` with `cc`-crates** (on-device clang invocation).
- **A self-hosting rustc bootstrap on-device** (building rustc *on* m3OS) — the
  analog of clang self-hosting, itself still deferred in Phase 85d.
- **Incremental compilation, parallel codegen, and performance** — correctness
  and "it compiles at all" come first; the slow ring-3 VFS makes performance a
  Phase 92-dependent concern regardless.
- **Remote debugging (gdb stub) for Rust programs.**
- **A userspace SIMD-enabled target** for the toolchain's own code — orthogonal;
  see Phase 86f.
