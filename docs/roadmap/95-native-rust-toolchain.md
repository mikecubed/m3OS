# Phase 95 - Native Rust Toolchain (on-device `rustc` + `cargo`)

**Status:** In progress — host toolchain built + sealed and `pkg install rust` works on m3OS; the on-device `RUSTC_OK` code-generation milestone is diagnosed but blocked on a streaming-loader perf wall and deferred to a Phase-95 follow-up (`95b`)
**Source Ref:** phase-95
**Depends on:** Phase 93 ✅ (Dynamic C Runtime — `libc.so` + loader TLS for proc-macro `dlopen`), Phase 94 ✅ (Rust-cargo musl port class), Phase 85d ✅ (Clang/LLVM/LLD on-device precedent + large-exec / `pread64` kernel fixes), Phase 87 ✅ (VFS bulk-I/O — the large install), Phase 88 ✅ (VFS `stat` conformance), Phase 44 ✅ (Rust cross-compilation lineage)
**Builds on:** Extends the Phase 44 → 94 "Rust runs in the OS" lineage from *running* host-cross-compiled Rust to *running the Rust toolchain itself* on-device. Reuses the Phase 85d Clang/LLVM/LLD delivery pattern (the project's only existing on-device native code generator) and the Phase 85a `.m3pkg` substrate. It is the Rust analog of Phase 85d, not of Phase 86d.
**Primary Components:** `ports/lang/rust/Portfile`, `xtask/src/port_build.rs` (`build_rust`), the bundled `rust-lld` linker (or a `DEPS=clang` dependency on the Phase 85d LLD), a userspace Rust target spec + a prebuilt `std`/`core` sysroot, the `M3OS_WITH_RUST` image feature, the `rustc-smoke` / `cargo-smoke` gates

> **Implementation-status callout (read first).** Two findings correct the design
> below. (1) The toolchain is **dynamic**, not fully static: a `crt-static` musl
> host cannot build rustc's own proc-macro deps, so the shipped `rustc` is a
> dynamic musl binary (`PT_INTERP=/lib/ld-musl` + `DT_NEEDED libc.so`, `DEPS=musl`,
> running via the Phase 93 `libc.so` + loader). Wherever this doc says "fully
> static", read "dynamic musl, `DEPS=musl`". (2) The host build is **complete** and
> `pkg install rust` works on m3OS, but the on-device milestone (`rustc hello.rs` →
> `RUSTC_OK`) is **blocked** on a streaming/file-backed-mmap loader for the ~162 MB
> `librustc_driver.so` (the kernel's file-backed `mmap` is eager today) plus SMP
> TLB-shootdown batching + a kstack strategy — split out to
> **[Phase 95b](./95b-on-device-rustc.md)** ([tasks](./tasks/95b-on-device-rustc-tasks.md)).
> See the [task doc](./tasks/95-native-rust-toolchain-tasks.md) implementation-status note.

## Milestone Goal

A native `rustc` runs **on m3OS** and compiles a Rust source file to a working
native executable that also runs on m3OS — `rustc hello.rs && ./hello` prints
its output — using the bundled `rust-lld` to link and the standard library
supplied as a prebuilt sysroot resolved relative to the compiler binary. m3OS
becomes self-hosting for Rust the same way Phase 85d made it self-hosting for
C/C++.

A stretch milestone adds `cargo build` of a small **proc-macro-free** crate.
Full `cargo` against the mainstream crate ecosystem (which is saturated with
derive macros) and crates.io registry access depend on the **Phase 93**
`libc.so` + loader TLS (now landed) for proc-macro `dlopen` plus a registry-fetch
path, and are scoped to the Area D stretch / *Deferred Until Later*.

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
  loader, positional I/O (`pread64`/`pwrite64`), generous `rlimit`s (all landed
  in Phase 85d), and consistent `stat` identity (the `st_ino=0` collapse 85d's
  clang surfaced, fixed systemically in Phase 88).
- The difference between *host-cross-compiling* Rust (Phases 44, 94) and
  *self-hosting* the toolchain on the device (this phase), and why the latter is
  a categorically larger problem.
- Why Rust **proc-macros** require the compiler to `dlopen` a shared object at
  compile time, and therefore why a real `libc.so` (Phase 93) — not just the
  dynamic-linker machinery (Phase 76, already done) — is a hard prerequisite for
  the mainstream crate ecosystem, even though the toolchain *binary* itself is
  static.
- How a Rust **sysroot** (prebuilt `libstd`/`libcore` rlibs + a target spec) is
  resolved relative to the compiler binary, mirroring the Phase 85d clang
  resource-dir relocation contract.
- The **bootstrap problem**: upstream ships a glibc toolchain, so producing a
  musl `rustc`/`cargo` for m3OS is a multi-stage host build (the same discipline
  Phase 85d applied to clang) — landing on a *dynamic* musl `rustc` (`DEPS=musl`),
  since a `crt-static` musl host can't build rustc's own proc-macro deps.

## Feature Scope

### Area A — A dynamic musl `rustc` (host-cross-built)

A new `build_rust()` dispatch in `xtask/src/port_build.rs`, paralleling
`build_llvm()` (85d) and `build_go()` (86d), produces a **dynamic**
`x86_64-unknown-linux-musl` `rustc` (and, for Area D, `cargo`). A fully-static
rustc was the original plan (the clang/python/go discipline) but proved
**infeasible** — rustc's own source uses proc-macro crates, and a `crt-static`
musl host cannot build dylibs/proc-macros — so the shipped `rustc` is a dynamic
musl binary (`PT_INTERP=/lib/ld-musl` + `DT_NEEDED libc.so`) that runs on m3OS via
the **Phase 93** `/usr/lib/libc.so` + loader (hence `DEPS=musl`). The host's own
Rust toolchain is the build compiler (the analog of "host clang cross-compiles
LLVM" in 85d).

### Area B — Bundled linker + sysroot relocation contract

- **Linker.** `rustc` does not link; it invokes an external linker. m3OS has no
  GNU `ld`, but Phase 85d already ships **LLD** on-device. This phase either
  bundles `rust-lld` (Rust's vendored LLD, `rustc`'s default) or declares
  `DEPS=clang` to reuse the Phase 85d `ld.lld`. The "no system linker" problem
  is therefore already solved upstream of this phase.
- **Sysroot.** A prebuilt `libstd`/`libcore`/`liballoc` rlib set plus a
  userspace target spec is bundled and resolved **relative to the `rustc`
  binary**, exactly like clang's resource dir. Note: the existing
  `x86_64-m3os.json` is the m3OS *userspace* target (hardware-float
  `+sse,+sse2,+aes`), but it carries `code-model: kernel` and is tailored to the
  hand-built `no_std` ring-3 binaries, so it is unsuitable as-is as a Rust std
  toolchain target. A dedicated userspace Rust target spec (a new `.json`, or
  `x86_64-unknown-linux-musl` if the milestone program targets stock musl) with a
  prebuilt std is new work with no precedent in the tree (Go shipped no
  toolchain; clang ships a C/C++ sysroot, not a Rust one).

### Area C — On-device `rustc hello.rs` (the milestone)

Prove that the installed `rustc` compiles a Rust source file to a native ELF
on m3OS, links it via the bundled LLD, and runs it — the Rust analog of the
Phase 85d `CLANG_C_OK` gate. This is achievable **without** Phase 93 as long as
the compiled program and its dependency graph use **no proc-macros** (the
distribution sysroot's `std` is precompiled, so the milestone program links
against rlibs and never asks `rustc` to `dlopen` anything).

### Area D — `cargo` + proc-macros (gated on Phase 93) — stretch

`cargo build` of a real crate almost always pulls a proc-macro crate
(`serde_derive`, `thiserror`, …). A proc-macro is a `cdylib`/`dylib` `.so` that
`rustc` **`dlopen`s at compile time**; it references the Rust runtime and libc
symbols. With no `libc.so` in scope every external relocation in that `.so` is
undefined and the load fails. So:

- A static `rustc` compiling **proc-macro-free** crates works after Areas A–C.
- Anything using derive macros — i.e. most of the ecosystem — requires
  **Phase 93** (`libc.so`) **and** loader TLS support first. This is the Rust
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
on-device; a `cargo-smoke` gate validates the Area D stretch (now unblocked by
Phase 93).

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

The dynamic-linker *machinery* has been complete since Phase 76 → 76d (dlopen,
PLT lazy resolve, GNU-hash, symbol versioning). What was missing — and what
**Phase 93 has since supplied** — is a real `libc.so` for a loaded `.so` to bind
its `malloc`/`memcpy`/`__errno_location` relocations against, plus the loader
extensions a real libc forces that the synthetic test `.so`s never exercised:
**TLS** (the x86_64 variant-II TCB via `setup_static_tls`/`__init_tp`,
`TPOFF`/`DTPMOD`), copy relocations, and IFUNC. TLS was the dominant Phase 93
risk — required by both `__libc_start_main` and all of Rust `std` — and Phase 93
landed it (proven by the dynamic `python3` + `ctypes.CDLL` path). The proc-macro
`dlopen` prerequisite is therefore satisfied; validating it against a *Rust*
proc-macro `.so` is the Area D stretch. The Area C milestone stays
proc-macro-free by construction (the distribution sysroot's `std` is precompiled,
so the milestone program links rlibs and never asks `rustc` to `dlopen` anything).

### The linker (reused)

`rustc`'s default linker driver is `rust-lld` (LLVM LLD). Phase 85d already
proved LLD links real programs on m3OS (`clang -fuse-ld=lld`). This phase reuses
that work — either by bundling `rust-lld` in the Rust `.m3pkg` or by depending
on the clang port's `ld.lld`.

### The sysroot + userspace target spec (new)

`rustc` finds `std` via a sysroot directory laid out relative to its own binary.
This phase bundles the prebuilt rlib sysroot for a m3OS *userspace* target and
ensures `rustc --print sysroot` resolves under `/usr`. The userspace Rust target
spec is new: `x86_64-m3os.json` is the existing userspace target (hardware-float
`+sse,+sse2,+aes`), but its `code-model: kernel` and `no_std`/hand-built ring-3
layout make it unsuitable as a Rust std toolchain target as-is.

### Reused kernel features from Phase 85d (no new kernel work expected for Area C)

Running a multi-tens-of-MB static rustc reuses the exact bring-up clang forced:
the **streaming ELF exec loader** (`mm::elf::load_elf_streaming`) backing
`DiskElfSource` (binaries far larger than the kernel heap; 512 MiB cap),
**`pread64`/`pwrite64`** ("THE compile blocker" for LLVM-class positional I/O),
`getrlimit`/`prlimit64` (all landed in Phase 85d), and consistent `fstat` inode
identity (the `st_ino=0` collapse 85d's clang `FileManager` dedup exposed, fixed
systemically in Phase 88). The "binary exceeds the kernel heap" class is
permanently closed.

### Alternative bootstrap: `mrustc` (smaller, no LLVM, no proc-macro `dlopen`)

If "compile *some* Rust on-device" is acceptable short of the full toolchain,
`mrustc` (a C++ Rust-subset compiler that emits C and needs no LLVM) is a far
smaller on-device target and sidesteps proc-macro `dlopen` for its bootstrap
subset. It is a legitimate first cut that does not depend on Phase 93, at the
cost of language-version and feature coverage. Recorded as an option, not the
recommended path for a *general* Rust toolchain.

## How This Builds on Earlier Phases

- **Reuses Phase 85d** as the direct precedent: the on-device LLVM-class
  delivery pattern, the bundled LLD, and the kernel fixes (streaming exec,
  positional I/O, rlimits, fstat identity) that running clang already forced.
- **Reuses the Phase 85a** `.m3pkg`/pkgcache substrate and the `M3OS_WITH_*`
  opt-in image-feature pattern unchanged.
- **Depends on Phase 93** for proc-macro support (the dynamic `libc.so` + loader
  TLS), which is the gate to the mainstream crate ecosystem.
- **Depends on Phase 94** for the host-side Rust-cargo port-class plumbing and
  the userspace musl target lineage it establishes.
- **Benefits from Phases 87 and 88**: the large install rides the VFS bulk-I/O
  throughput work (87), and `cargo`/`build.rs` correctness benefits from the
  `stat` conformance pass (88).
- **Continues the Phase 44 → 94** "Rust in the OS" lineage by flipping it from
  host-compiled to device-compiled.

## Implementation Outline

1. **De-risk the bootstrap (Track A).** Cross-build a dynamic musl `rustc`
   (`DEPS=musl` — a `crt-static` host can't build rustc's proc-macro deps) on the
   host; confirm it loads and runs on m3OS through the Phase 85d streaming exec
   path (`rustc --version`, `rustc --print sysroot`). *(On-device load blocked on
   the streaming-loader wall → Phase 95b.)*
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
   slow ring-3 VFS — mitigated by Phase 87).
6. **(Stretch) cargo + proc-macros (Track D).** With Phase 93's `libc.so` +
   loader TLS now landed, bundle `cargo`, prove `cargo build` of a
   proc-macro-free crate, then a derive-macro crate via on-device `dlopen` of
   the proc-macro `.so`; `cargo-smoke` gate.
7. **Document + version.** Learning doc, README row update, bump
   `kernel/Cargo.toml` on landing.

## Learning Documentation Requirement

- Create `docs/95-native-rust-toolchain.md` using the *aligned legacy learning
  doc* template in `docs/appendix/doc-templates.md` (seven sections: Overview,
  What This Doc Covers, Core Implementation, Key Files, How This Phase Differs
  From Later Work, Related Roadmap Docs, Deferred or Later-Phase Topics).
- Explain: the distinction between *running a host-cross-compiled Rust binary*
  (Phases 44/94, already working) and *running the `rustc` toolchain itself*
  on-device (this phase); why an LLVM-based compiler is the heaviest class of
  on-device program and which Phase 85d kernel features it reuses (streaming ELF
  exec, `pread64`/`pwrite64`, generous `rlimit`s, `fstat` inode identity); the
  **sysroot relocation contract** (prebuilt `std`/`core` rlibs + a userspace
  target spec resolved relative to the `rustc` binary, mirroring clang's
  resource dir); the bundled `rust-lld` linker (reusing the Phase 85d LLD); and
  why **proc-macros** force a compile-time `dlopen` that needs the Phase 93
  `libc.so` + loader TLS, gating the mainstream crate ecosystem.
- Link it from `docs/README.md` (the Phase-Aligned Learning Docs table) and
  register it in `docs/appendix/codebase-map.md` (the Documentation Index) when
  the phase lands.

## Related Documentation and Version Updates

- Update `docs/roadmap/README.md` (flip the Phase 95 row Status `Planned` →
  `Complete` at landing, and replace the Tasks cell with the task-doc link) and
  `docs/README.md` (add the learning-doc row).
- Decide the `AGENTS.md` capability-bullet edit: on-device `rustc` is a new
  capability class — the project's first native Rust *code generator*, distinct
  from the Phase 85d C/C++ one — so a one-line rewrite of the developer-toolchain
  inventory naming on-device `rustc` is warranted; per the "keep it small"
  policy, rewrite an existing bullet rather than adding a new one.
- **Bump `kernel/Cargo.toml` `version` `0.94.1` → `0.95.0` and the `AGENTS.md`
  "kernel **v0.94.1**" line to match — unconditionally, as the standard per-phase
  minor bump.** No kernel *code* change is expected for Area C (rustc rides the
  Phase 12 compat layer + the Phase 85d streaming loader); the version string is
  the only kernel-side edit, and every version reference (boot banner,
  `/proc/version`, `uname`) derives from `env!("CARGO_PKG_VERSION")`, so the
  single `Cargo.toml` edit propagates. A *patch* bump on top applies only if a
  syscall gap surfaces during bring-up (the Phase 94 precedent).

## Acceptance Criteria

- `cargo xtask port build rust` produces a dynamic `x86_64-unknown-linux-musl`
  `rustc` (`DEPS=musl`) and seals a valid `rust.m3pkg` (`pkg_format::verify` passes). ✅
  (host build complete)
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
- **Area D (stretch, may be a separate `95b` sub-phase):** with Phase 93's
  `libc.so` + loader TLS present (landed), `cargo build` of a proc-macro-free
  crate succeeds on-device, and a derive-macro crate compiles via on-device
  `dlopen` of the proc-macro `.so` (`CARGO_PROCMACRO_OK`).
- The hand-built `no_std` userspace and the kernel build are unaffected (this
  phase adds a userspace toolchain, it does not change how m3OS itself is built).

## Companion Task List

- [Phase 95 Task List](./tasks/95-native-rust-toolchain-tasks.md)

## How Real OS Implementations Differ

- A real distro ships a **dynamic** `rustc`/`cargo` linked against the system
  `libc.so` and `librustc_driver.so`, installed via `rustup`/the package manager
  onto a writable root, with full crates.io registry access over TLS. m3OS's
  `rustc` is **also dynamic** (`librustc_driver.so` + `libc.so`, via the Phase 93
  loader) — unlike static clang/python/go, because rustc's own proc-macro deps
  can't build on a `crt-static` musl host — but it is installed **offline** from a
  bundled `.m3pkg` (`DEPS=musl`), not from a network registry.
- Real toolchains support proc-macros out of the box because the host always has
  a dynamic libc to `dlopen` a `.so` against; on m3OS that capability was a
  distinct, sequenced prerequisite — supplied by Phase 93 (`libc.so` + loader
  TLS, now landed) and exercised by the Area D stretch.
- Distros do not carry a separate bare-metal `no_std` floor; m3OS keeps its
  `x86_64-unknown-none` kernel/userspace lineage entirely separate from this
  userspace `x86_64-unknown-linux-musl` toolchain.
- Production `cargo` does network dependency resolution, build-script execution,
  incremental compilation caching, and parallel codegen units; this phase
  targets the on-device-compile milestone, not feature parity.

## Deferred Until Later

- **The on-device `RUSTC_OK` code-generation milestone itself** — split out to
  **[Phase 95b](./95b-on-device-rustc.md)** ([tasks](./tasks/95b-on-device-rustc-tasks.md)):
  the streaming / file-backed-mmap loader for the ~162 MB `librustc_driver.so`, SMP
  TLB-shootdown batching, and the kstack strategy.
- **Mainstream `cargo` + proc-macros** ride the **Phase 93** `libc.so` + loader
  TLS (now landed — the hard gate is cleared); split out to
  **[Phase 95b Track E](./tasks/95b-on-device-rustc-tasks.md)**.
- **crates.io registry access** (cargo HTTPS fetch wired to the Phase 86c TLS
  stack) and **`build.rs` with `cc`-crates** (on-device clang invocation).
- **A self-hosting rustc bootstrap on-device** (building rustc *on* m3OS) — the
  analog of clang self-hosting, itself still deferred in Phase 85d.
- **Incremental compilation, parallel codegen, and performance** — correctness
  and "it compiles at all" come first; the slow ring-3 VFS makes performance a
  Phase 87-dependent concern regardless.
- **Remote debugging (gdb stub) for Rust programs.**
- **A userspace SIMD-enabled target** for the toolchain's own code — orthogonal;
  see Phase 86f.
