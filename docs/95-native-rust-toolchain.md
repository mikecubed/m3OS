# Native Rust Toolchain (on-device `rustc`)

**Aligned Roadmap Phase:** Phase 95
**Status:** In progress
**Source Ref:** phase-95
**Supersedes Legacy Doc:** N/A (new capability)

## Overview

> **Status (in progress):** the complete **host-side** toolchain is built + sealed
> and `pkg install rust` works on m3OS, but the on-device **code-generation
> milestone** (`rustc hello.rs` → `RUSTC_OK` on m3OS) is **diagnosed but blocked**
> on deep kernel/loader perf work — chiefly a **CPU-bound load of the ~162 MB
> `librustc_driver.so`** (LLVM statically linked in) through the loader's
> whole-file read+copy strategy, which needs a streaming/file-backed-mmap loader
> (the kernel's file-backed `mmap` is eager today), plus an SMP TLB-shootdown storm
> and an intermittent per-task kernel-stack overflow — deferred to a Phase 95
> follow-up (`95b`); see the task doc's implementation-status note. NOTE: rustc is
> **dynamic** musl, not fully static (a `crt-static` musl host can't build rustc's
> own proc-macro deps), so it `DEPS=musl` (the Phase 93 `libc.so`). The sections
> below describe the design as built.

Phase 95 makes m3OS **self-hosting for Rust**: a native `rustc` runs *on the
device* and compiles a Rust source file to a working native ELF that also runs on
m3OS — `rustc /usr/src/hello.rs -o /tmp/hello && /tmp/hello` prints `RUSTC_OK`.
The toolchain is a dynamic `x86_64-unknown-linux-musl` `rustc` (+ a prebuilt
`std`/`core`/`alloc` sysroot + the bundled `rust-lld` linker), host-cross-built
from pinned Rust 1.96.0 source and packaged behind an `M3OS_WITH_RUST` image
feature, installed offline with `pkg install rust` (which pulls `DEPS=musl`).

This is the **Rust analog of Phase 85d** (on-device Clang/LLVM/LLD), *not* of
Phase 86d. The single most important framing is the difference between running a
*program* and running a *compiler*:

| Toolchain phase | What runs on m3OS | Generates machine code on-device? |
|---|---|---|
| Go (86d) | one pre-built static Go *binary* | **No** — the Go compiler runs only on the host |
| Python (85c) | the CPython interpreter | No — bytecode, emits no native code |
| **Clang/LLVM/LLD (85d)** | the clang + lld binaries | **Yes** — compiles + links + runs C/C++ in m3OS |
| **Rust (this phase)** | the `rustc` + `rust-lld` binaries | **Yes** — the milestone |

m3OS userspace has *run* native Rust binaries since Phase 5, and has
*host-cross-compiled* Rust programs since Phase 44 (and via cargo since Phase 94).
The new thing is running the **toolchain itself** — on-device native code
generation by an LLVM-based compiler, which is the heaviest class of on-device
program and the exact problem Phase 85d already solved for C/C++.

This doc is the pedagogical companion to the implementation-focused
[design doc](./roadmap/95-native-rust-toolchain.md).

## What This Doc Covers

- The *running a program* vs *running the compiler* distinction, and why Phase
  85d (Clang) — not Phase 86d (Go) — is the correct precedent.
- Why an LLVM-based compiler is the heaviest on-device program class, and which
  Phase 85d kernel features it reuses unchanged (streaming ELF exec, positional
  I/O, generous rlimits, consistent `fstat` inode identity).
- The **bootstrap problem**: upstream ships a glibc toolchain; m3OS builds a
  **dynamic** musl-host `rustc` (a fully-static rustc is infeasible — a
  `crt-static` musl host can't build rustc's own proc-macro deps — so it
  `DEPS=musl`), via a deliberate multi-stage host build (host clang as the musl
  cross-compiler, **from-source X86-only** musl LLVM with `download-ci-llvm=false`,
  a reused musl libc++ sysroot).
- The **sysroot relocation contract** — a prebuilt `std`/`core` rlib set + a
  target resolved relative to the `rustc` binary, mirroring clang's resource dir.
- The reused **`rust-lld`** linker (the "no system linker" problem solved upstream
  in 85d), and how rustc is told to use it on a `cc`-less m3OS.
- Why **proc-macros** force a compile-time `dlopen` that needs the Phase 93
  `libc.so` + loader TLS — the wall to the mainstream crate ecosystem.

## Core Implementation

### The bootstrap problem: a *dynamic* musl-*host* `rustc`

Upstream Rust ships a **glibc** toolchain (`librustc_driver.so` against glibc).
That binary cannot run on m3OS, whose custom `ld-musl-x86_64.so.1` is a from-scratch
Rust loader. The natural instinct (and the original plan) was to build the toolchain
**fully static** against musl, exactly like static clang/Python/Go. That proved
**infeasible**: rustc's *own* source depends on proc-macro crates (`serde_derive`,
`darling_macro`, …), and a `crt-static` musl host cannot build dylibs/proc-macros
(`error: cannot produce proc-macro … target does not support these crate types`).
So Phase 95 ships a **dynamic** musl `rustc` (`crt-static = false`): the binary is
`PT_INTERP=/lib/ld-musl-x86_64.so.1` + `DT_NEEDED libc.so`, running on m3OS via the
**Phase 93** `/usr/lib/libc.so` + Rust loader — which is exactly why Phase 93 is a
listed dependency, and why `rust.m3pkg` carries `DEPS=musl`.

`build_rust` (`xtask/src/port_build.rs`) drives the upstream `x.py` bootstrap with
a cross-host configuration:

```
build  = x86_64-unknown-linux-gnu     # the stage0 bootstrap runs on the host
host   = [x86_64-unknown-linux-musl]  # the rustc BINARY is dynamic musl
target = [x86_64-unknown-linux-musl]  # it also generates musl code on-device
crt-static = false                    # dynamic — see the proc-macro note above
```

Like `build_go`/`build_uutils`, it branches **before** the shared musl-gcc
plumbing in `fn port_build` (the toolchain self-bootstraps via its own stage0
compiler, which `x.py` auto-downloads; no external `x86_64-linux-musl-gcc`).

### Host clang as the musl cross-compiler (the Phase 85d pattern)

`rustc` statically links LLVM (C++), so producing a musl-host `rustc` requires a
musl-targeting C/C++ toolchain. m3OS does not ship a musl GCC (Debian's
`x86_64-linux-musl-gcc` is a glibc specs-wrapper). The Phase 85d solution is reused
verbatim: **host clang is the cross-compiler** (`--target=x86_64-unknown-linux-musl`
+ a musl sysroot), driven through small `m3os-musl-clang`/`m3os-musl-clang++`
wrapper scripts that add `-fuse-ld=lld -rtlib=compiler-rt -unwindlib=libunwind`
(a musl sysroot has no GNU `ld` and no `libgcc_s`). `x.py`'s `cc`/`cxx` point at
those wrappers.

### From-source X86 LLVM for the musl host (`download-ci-llvm = false`)

`x.py` needs LLVM for the musl host (linked into `rustc`). Phase 95 builds it
**from the rustc-src tarball's vendored `src/llvm-project`** with
`download-ci-llvm = false` and `targets = "X86"` (X86-only — m3OS only targets
x86_64 — which is faster to build and a smaller `librustc_driver` to load
on-device). The earlier `download-ci-llvm = true` path (fetch the prebuilt **gnu**
CI LLVM and use its host-runnable `llvm-tblgen`/`llvm-nm`/`llvm-config` to drive the
musl cross-build) was abandoned: it mis-pointed the musl `rustc_llvm` shim's
includes, `llvm.targets` is incompatible with it, and the prebuilt *musl* `rust-dev`
`llvm-config` is itself a dynamic-musl binary that will not run on the glibc host.

### Reusing the Phase 85d musl libc++ sysroot

Both the cross-built LLVM and rustc's `rustc_llvm` C++ shim need a **static musl
libc++**. The rustc-src tarball's `src/llvm-project` ships `compiler-rt` and
`libunwind` but **not** `libcxx`/`libcxxabi` (rustc never builds libc++ itself).
So `build_rust` **reuses the `llvm` port's assembled sysroot at
`target/llvm-musl-sysroot`** (`libc++.a`/`libc++abi.a`/`libunwind.a` +
compiler-rt builtins) — exactly the artifact `build_node` reuses for V8. The
`rustc-smoke` gate therefore builds the `llvm` port first (a pkgcache hit when
warm). libc++'s ABI stability lets the LLVM-18-vintage sysroot's libc++ link the
rust toolchain's newer LLVM (1.96.0 carries LLVM 22) C++ code compiled by the same
clang.

### The sysroot relocation contract

`rustc` finds `std` via a sysroot laid out **relative to its own binary** — the
same contract as clang's resource dir (Phase 85d). `x.py install` with
`DESTDIR=<stage>` and `prefix=/usr` lays `usr/bin/rustc`,
`usr/lib/rustlib/<target>/lib/*.rlib` (the prebuilt `std`/`core`/`alloc`), and
`usr/lib/rustlib/<target>/bin/rust-lld`. Because the layout is position-independent
of the install root, on m3OS `rustc --print sysroot` resolves under `/usr` no
matter where the `.m3pkg` unpacks.

### The linker: bundled `rust-lld` on a `cc`-less m3OS

`rustc` does not link — it invokes an external linker, and m3OS has no GNU `ld`.
Phase 85d already proved **LLD** links real programs on m3OS, so Phase 95 simply
bundles Rust's vendored `rust-lld` in the `.m3pkg` (its default linker) rather than
re-solving "no system linker" or depending on the heavy clang port. Because m3OS
has no system `cc`, the on-device compile passes
`-C linker-flavor=ld.lld -C link-self-contained=yes`, so `rustc` invokes its
bundled `rust-lld` directly (self-contained crt + linker) instead of shelling out
to an absent `cc` — and the resulting program is static-pie (no runtime deps).
Note an LLVM-linkage asymmetry. `librustc_driver.so` **statically** links LLVM (it
`DT_NEEDED`s only `libc.so`, so `rustc --version` does *not* load `libLLVM.so`),
while `rust-lld` is **dynamically** linked against the shared `libLLVM.so.<ver>`
(an LLVM build artifact that `x.py install` does *not* copy into the rust-std
component). `build_rust` therefore stages that dylib into `rustlib/<target>/lib/`
— `rust-lld`'s `RUNPATH` is `$ORIGIN/../lib` — so the `.m3pkg` ships it and rust-lld
loads it through the Phase 93 loader at link time on-device. (Without that staging,
the bundled rust-lld would be unloadable — `DT_NEEDED libLLVM.so.* not found`.)

### Reused kernel substrate (no new always-on kernel work for the core)

Running a multi-tens-of-MB static `rustc` reuses exactly what running clang forced
in Phase 85d: the **streaming ELF exec loader** (`mm::elf::load_elf_streaming`
backing `DiskElfSource`, for binaries far larger than the kernel heap; 512 MiB
cap), **positional `pread64`/`pwrite64`**, generous `getrlimit`/`prlimit64`, and
consistent `fstat` inode identity (the `st_ino=0` collapse clang surfaced, fixed
systemically by `fill_stat` in Phase 88). The "binary exceeds the kernel heap"
class stays closed; the version bump (`0.94.1` → `0.95.0`) is the only kernel-side
edit expected for the Area C core.

## Key Files

| File | Purpose |
|---|---|
| `ports/lang/rust/Portfile` | Pinned Rust 1.96.0 source + SHA-256; `DEPS=` empty (rust-lld bundled) |
| `xtask/src/port_build.rs` (`build_rust`) | The x.py cross-build recipe: musl-clang wrappers, `config.toml`, build, DESTDIR install + prune + validate |
| `xtask/src/port_build.rs` (`build_recipe_id` `"rust"` arm) | Content-key transcription of the build's defining flags |
| `target/llvm-musl-sysroot` (from the `llvm` port) | The reused static musl libc++ + compiler-rt builtins sysroot |
| `xtask/src/main.rs` (`M3OS_WITH_RUST` gate) | Opt-in bundling of `rust.m3pkg` into `/usr/pkg/` (mirrors `M3OS_WITH_CLANG`) |
| `xtask/src/main.rs` (`cmd_rustc_smoke`, `rustc_smoke_steps`) | The gate: `pkg install rust` → `rustc --version` → `--print sysroot` under `/usr` → `rustc hello.rs` (rust-lld) → `RUSTC_OK` |
| `kernel/src/mm/elf.rs`, `kernel/src/arch/x86_64/syscall/mod.rs` | The Phase 85d streaming-exec / `pread64` substrate rustc rides (unchanged) |

## How This Phase Differs From Later Work

- The Area C milestone is **proc-macro-free by construction**: the distribution
  sysroot's `std` is precompiled, so the milestone program links rlibs and never
  asks `rustc` to `dlopen` anything. The mainstream crate ecosystem is saturated
  with derive macros and is **not** covered by Area C.
- **`cargo` + proc-macros** (Track D, a stretch / possible `95b`) is gated on
  **Phase 93**'s `libc.so` + loader TLS: a proc-macro is a `cdylib` `.so` that
  `rustc` `dlopen`s at compile time and binds its `malloc`/`memcpy`/TLS relocations
  against `/usr/lib/libc.so`. Phase 93 landed that path (proven by dynamic
  `python3` + `ctypes.CDLL`); Track D validates it against a *Rust* proc-macro `.so`.
- The toolchain is **dynamic**, not static: a fully-static rustc proved infeasible
  (a `crt-static` musl host can't build rustc's own proc-macro deps), so the rustc
  *binary itself* binds `/usr/lib/libc.so` via the Phase 93 loader (`DEPS=musl`) —
  unlike the static clang/python/go discipline. Proc-macro `.so`s loaded *by* rustc
  (Track D) bind against the same `libc.so`.
- m3OS keeps its bare-metal `x86_64-unknown-none` kernel/userspace lineage entirely
  separate from this userspace `x86_64-unknown-linux-musl` toolchain; this phase
  adds a userspace toolchain, it does not change how m3OS itself is built.

## Related Roadmap Docs

- [Phase 95 design doc](./roadmap/95-native-rust-toolchain.md)
- [Phase 95 task doc](./roadmap/tasks/95-native-rust-toolchain-tasks.md)
- [Phase 85d — Clang/LLVM/LLD on-device](./roadmap/85d-clang-llvm.md) (the direct precedent: on-device LLVM-class delivery, the bundled LLD, the streaming-exec/`pread64` kernel work)
- [Phase 94 — Rust-cargo ports & uutils](./94-rust-cargo-uutils.md) (the host-side Rust-cargo port-class lineage)
- [Phase 93 — Dynamic C Runtime](./93-dynamic-c-runtime.md) (`libc.so` + loader TLS — the proc-macro `dlopen` prerequisite)
- [Phase 85a — Package substrate](./roadmap/85a-package-infrastructure.md) (the `.m3pkg`/pkgcache/`pkg` installer + `M3OS_WITH_*` opt-in pattern)
- [Phase 12 — POSIX compatibility layer](./12-posix-compatibility-layer.md) (the Linux-syscall compat layer rustc rides)

## Deferred or Later-Phase Topics

- **The on-device `RUSTC_OK` code-generation milestone itself** (Phase 95's headline
  goal) — blocked on a **streaming / file-backed-mmap loader**. Loading the ~162 MB
  dynamic `librustc_driver.so` through the loader's current whole-file read+copy
  strategy (mmap a file-sized anon scratch → `sys_read` the whole file → mmap a
  second file-sized anon image → `copy` each `PT_LOAD` → relocate) is CPU-bound and
  times out; the kernel's file-backed `mmap` is **eager** (no demand-fault-from-file
  VMA backing), so the full fix is a kernel mm rework. A **loader-only partial win**
  exists (read each `PT_LOAD` directly into the final image via `lseek`+`read`,
  dropping the scratch buffer + the full-image RAM copy — ~halving anon footprint
  and copy traffic with no kernel change) but does not by itself unblock the
  milestone. Tracked in **[Phase 95b](./roadmap/95b-on-device-rustc.md)**, alongside
  SMP TLB-shootdown batching and a targeted/lazy kernel-stack strategy.
- **Mainstream `cargo` + proc-macros** (the old Track D, now
  **[Phase 95b Track E](./roadmap/tasks/95b-on-device-rustc-tasks.md)**): a
  proc-macro-free `cargo build` (`CARGO_OK`), then a derive-macro crate via
  on-device `dlopen` of the proc-macro `.so` against the Phase 93 `libc.so`
  (`CARGO_PROCMACRO_OK`).
- **crates.io registry access** (wiring cargo's HTTPS fetch to the Phase 86c TLS
  stack) and **`build.rs` with `cc`-crates** (on-device clang invocation).
- **A self-hosting `rustc` bootstrap on-device** (building `rustc` *on* m3OS) — the
  analog of clang self-hosting, itself still deferred in Phase 85d.
- **Incremental compilation, parallel codegen, and performance** — correctness and
  "it compiles at all" come first; the slow ring-3 VFS makes performance a
  Phase 87-dependent concern regardless.
- **A dedicated userspace Rust target spec.** Phase 95 targets stock
  `x86_64-unknown-linux-musl` (which ships a prebuilt std and is the proven Phase 94
  path); a bespoke `x86_64-m3os-user.json` with `-Zbuild-std` is an orthogonal
  future refinement.
