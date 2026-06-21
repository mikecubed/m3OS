# Rust-Cargo Ports & uutils Coreutils

**Aligned Roadmap Phase:** Phase 94
**Status:** Complete
**Source Ref:** phase-94
**Supersedes Legacy Doc:** N/A (new capability)

## Overview

Phase 94 delivers the project's first **Rust-cargo cross-compiled port** — upstream
[uutils/coreutils 0.9.0](https://github.com/uutils/coreutils) — as a single statically-linked
multicall binary installed by `pkg install coreutils` into `/usr/local/bin`. Because that
directory sits first in the shell's PATH, each applet symlink (`ls -> coreutils`,
`cat -> coreutils`, …) transparently shadows the corresponding hand-built `coreutils-rs`
ramdisk tool. The ramdisk set is **not removed**: it remains the early-boot floor and the
uninstall fallback. After `pkg remove coreutils` the shell falls back to the ramdisk
tools with no further action.

This doc is the pedagogical companion to the implementation-focused
[design doc](./roadmap/94-rust-cargo-uutils.md): it teaches **why the prebuilt-std Rust
musl target is self-contained** (no external C cross-compiler needed for a pure-Rust crate),
how the multicall-binary + applet-symlink packaging shape is efficient, and how PATH
precedence makes coexistence automatic.

## What This Doc Covers

- The distinction between the bare-metal **`-Zbuild-std`** path the kernel/userspace use
  and the **prebuilt-std `rustup target add`** path used for musl ports.
- Why the `x86_64-unknown-linux-musl` Rust target is **self-contained** (no external
  `x86_64-linux-musl-gcc` required for a pure-Rust crate), contrasting the C ports'
  `find_musl_cc`/stub-archive plumbing.
- The **multicall-binary + applet-symlink** packaging shape and how `pkg-format` round-trips
  symlinks through `.m3pkg` install.
- How **PATH-shadow coexistence** lets an installed port shadow a ramdisk-resident tool without
  removing the early-boot floor.
- The curated `feat_os_unix_musl` feature set and what it excludes by construction.

## Core Implementation

### Two Rust cross-compilation paths in the same repo

The kernel and hand-built userspace binaries (`userspace/coreutils-rs/`, `userspace/init/`, …)
are built for `x86_64-unknown-none` — a bare-metal target with **no std**. Their standard
library is `-Zbuild-std`: the nightly toolchain recompiles `core`, `alloc`, and `compiler_builtins`
from source for that bare-metal target. Because there is no operating system to link against,
there is also no external C compiler in the loop for these crates.

Musl ports take a completely different path. `x86_64-unknown-linux-musl` is a **tier-2**
Rust target with a prebuilt standard library: `rustup target add x86_64-unknown-linux-musl`
downloads a pre-compiled `libstd`, `libcore`, and `libcompiler_builtins` whose musl libc is
**bundled inside the Rust target itself** (delivered by the toolchain, not installed
separately). The target uses `rust-lld` for linking. No `-Zbuild-std` flag is needed; no
external `x86_64-linux-musl-gcc` is needed for a crate with no C dependencies.

This is why the C ports (`git`, `curl`, `Python`, `Clang`) require the elaborate
`find_musl_cc()` + `musl_extra_ldflags_joined()` + stub-archive plumbing in
`xtask/src/port_build.rs`: they are autotools/cmake/make projects that invoke a C compiler,
and on toolchains that ship without empty `libdl.a`/`libpthread.a`/`librt.a` stubs the
configure link probe fails. A pure-Rust crate built for `x86_64-unknown-linux-musl` skips
all of that: `cargo build --target x86_64-unknown-linux-musl` is sufficient.

**Precedent:** `build_musl_rust_bins` and `build_ion` (`xtask/src/main.rs`, Phase 44) already
cross-compile std Rust crates for `x86_64-unknown-linux-musl` with prebuilt std +
`-C target-feature=+crt-static`. Phase 94 routes that same plumbing through a `Portfile`
recipe for the first time.

### The `build_uutils` port recipe

`build_uutils` in `xtask/src/port_build.rs` follows a **go/gh-style early-return branch**
in `fn port_build` (before the `musl_toolchain()` requirement arm) because no musl-gcc is
needed:

1. Confirms `x86_64-unknown-linux-musl` is installed (via `rustup target list --installed`),
   downloading it if not.
2. Downloads the pinned uutils source tarball (version + SHA-256 in
   `ports/util/coreutils/Portfile`).
3. Runs:
   ```
   cargo build --release \
     --target x86_64-unknown-linux-musl \
     --no-default-features \
     --features feat_os_unix_musl \
     --locked
   ```
   `--locked` pins against the upstream `Cargo.lock` for build determinism.
   Static linking is the musl-target default (`-C target-feature=+crt-static`), so the
   output is a single fully-static `coreutils` ELF.
4. Copies the binary into `<stage>/usr/local/bin/coreutils`.
5. Runs `coreutils --list` against the just-built binary and creates a relative symlink
   (`ls -> coreutils`, `cat -> coreutils`, …) for each listed applet under
   `<stage>/usr/local/bin/`.
6. Calls the shared `strip_stage` + `seal_package` path to produce `coreutils.m3pkg`.

### The curated `feat_os_unix_musl` feature set

uutils organises features hierarchically. `feat_os_unix_musl` expands to
`feat_Tier1` (the mandatory POSIX core: `cat`, `ls`, `cp`, `mv`, `rm`, `echo`, `wc`, …) +
`feat_require_unix_musl` (musl-specific Unix extras) + `hostid` + `utmpx`.

By construction this feature set **excludes**:
- **SELinux applets** (`chcon`, `runcon`) — only in `feat_require_selinux`, which m3OS
  has no kernel support for.
- **`stdbuf`** — needs an external `libstdbuf.so` shared object, impossible in a static binary.

No stub or placeholder is added for the excluded applets; they simply are not present in
the installed set.

### The multicall binary and `pkg-format` symlink round-trip

A multicall binary (like BusyBox) packs multiple tools into one ELF: the binary inspects
`argv[0]` at startup and dispatches to the matching applet. For ~100 applets, shipping one
`coreutils` binary plus ~100 tiny symlinks costs far less than shipping 100 separate binary
copies.

`pkg-format` round-trips symlinks correctly: the `pack` routine (via its private `collect`
walker) captures symlink targets as file content and preserves the `S_IFLNK` mode bits;
`unpack` restores them via `std::os::unix::fs::symlink`. The
`pack_unpack_round_trips_bytes_and_modes` test in `pkg-format/src/lib.rs` (the
`clear -> tput` symlink case) proves a relative symlink under the staged prefix round-trips
as a symlink, not a copy. Without this, ~100 applets would each expand to a full copy of
the multi-MB binary inside the `.m3pkg`, defeating the purpose of the multicall shape.

### PATH-shadow coexistence

The shell searches `PATH` in order: `/usr/local/bin:/bin:/sbin:/usr/bin`
(`userspace/shell/src/main.rs`). After `pkg install coreutils` materialises symlinks under
`/usr/local/bin`, the shell finds `/usr/local/bin/ls` (pointing to `coreutils`) before it
reaches `/bin/ls` (the ramdisk `coreutils-rs` applet). No shell change is needed.

m3OS has no `which`, `command -v`, or `type` builtin, so PATH shadowing cannot be
*inspected* by resolving a path. Instead it is proven by **which implementation's output you
get**: with the package installed, `ls --version` prints the uutils version banner; after
`pkg remove coreutils` the same command falls back to the ramdisk tool's output (which
prints no version banner, since the hand-built applets do not implement `--version`).

The ramdisk `coreutils-rs` set is never removed. It is the only coreutils available before
the data disk mounts during early `init`, and it is the uninstall fallback.

### Why uutils runs unmodified on m3OS

A std Rust binary built for `x86_64-unknown-linux-musl` static-links musl libc and issues
the same Linux syscalls as the C ports (`git`, `Python`). m3OS's Phase 12 Linux-syscall
compatibility layer (`kernel/src/arch/x86_64/syscall/mod.rs`) already handles the ~121
Linux syscalls those ports exercise: `openat`, `read`, `write`, `getdents64`, `newfstatat`,
`statfs`, `readlinkat`, `symlinkat`, `utimensat`, `getrandom`, and the
threading path (`clone`/`futex`/`set_tid_address`). The static-ELF loader (`kernel/src/mm/elf.rs`)
maps the binary. The runtime substrate is already proven; Phase 94 adds only the build
plumbing — plus one small, contained family of kernel syscalls described next.

**A family of fd-relative `*at` syscall gaps surfaced (the anticipated patch-bump case).**
uutils' `uucore::safe_traversal` performs recursive metadata ops via the TOCTOU-safe
fd-relative path — `openat()` the parent directory, then operate by `(dirfd, name)` — rather
than the legacy path syscalls. Crucially, **musl on x86_64 keeps the legacy syscalls for the
non-`at` wrappers** (`chmod`→`SYS_chmod`(90), `chown`→`SYS_chown`(92), `mkdir`→`SYS_mkdir`(83),
`unlink`→`SYS_unlink`(87)), but the `*at` forms are bare `syscall(SYS_…at)` with **no legacy
fallback**. So plain `chmod`/`chown`/`mkdir -p`/`cp -p`/`cp -r` worked from day one, while the
recursive / fd-relative applets hit syscalls m3OS had never implemented:

| uutils applet (path) | fd-relative syscall | was missing |
|---|---|---|
| `rm -r` (`uucore` safe removal) | `unlinkat`(263) | yes → `rm -r` removed *nothing* |
| `chmod -R` (NoFollow walk) | `fchmodat2`(452) then `fchmodat`(268) | yes |
| `chown -R` | `fchownat`(260) | yes |
| `install -D` (parent-dir create) | `mkdirat`(258) | yes |

Each was added as a thin dirfd-aware wrapper over the **existing** `unlink`/`rmdir`/`chmod`/
`chown`/`mkdir` core, routing through the `resolve_path_from_dirfd` helper m3OS already used
for `openat`/`newfstatat`; the `AT_FDCWD` path stays byte-equivalent to the prior behaviour.
`fchmodat2`(452) is handled directly (not left to ENOSYS): `chmod -R`'s NoFollow path tries
raw 452 first and the musl `fchmodat`(268) fallback emulates `AT_SYMLINK_NOFOLLOW` via
`O_PATH` + `/proc/self/fd`, which m3OS does not implement — so 452 must succeed in-kernel.
These syscalls (`unlinkat`/`fchmodat`/`fchmodat2`/`fchownat`/`mkdirat`) are the **only** kernel
change in the phase and carried a single **patch** bump (`0.94.0` → `0.94.1`). Lesson: a
std/musl binary that "just issues Linux syscalls" can still exercise a whole corner of the
ABI (fd-relative metadata ops) the existing C ports never hit — `coreutils-smoke`, which now
exercises `rm -r`, `chmod -R`, `chown -R`, and `install -D` and reads each result back, is
what caught it.

## Key Files

| File | Purpose |
|---|---|
| `xtask/src/port_build.rs` (`build_uutils`) | The port recipe: toolchain check, tarball download, `cargo build`, symlink staging, seal |
| `ports/util/coreutils/Portfile` | Pinned uutils version, SHA-256, `DEPS=` (empty — pure Rust) |
| `pkg-format/src/lib.rs` (`pack`, `pack_unpack_round_trips_bytes_and_modes`) | Symlink round-trip in the `.m3pkg` format + the `clear -> tput` test case that proves it |
| `userspace/shell/src/main.rs` | PATH order (`/usr/local/bin` first) — relied upon, unchanged |
| `kernel/src/arch/x86_64/syscall/mod.rs` | The Linux-compat layer uutils rides |
| `xtask/src/main.rs` (`build_musl_rust_bins`, `build_ion`) | Phase 44 precedent for the prebuilt-std musl Rust cross-build path |
| `xtask/src/main.rs` (`coreutils-smoke`) | The gate: `pkg install coreutils`, applet battery, `sha256sum` cross-check, `pkg remove` fallback |

## How This Phase Differs From Later Work

- This phase introduces the **Rust-cargo port class** as a path in `fn port_build`, valid
  only for pure-Rust crates with no C FFI dependencies. C ports still go through
  `musl_toolchain()` + stub-archive plumbing.
- The uutils build **fetches crates from crates.io** during the host build (`--locked`).
  A fully-offline vendored-source tarball (hermetic, network-free) is the reproducibility
  upgrade deferred to a follow-on.
- `stat`/inode-identity rigor (uutils `ls -i`, hardlink detection in `du`/`cp`) depends on the
  VFS returning consistent, non-zero `(st_dev, st_ino)` — delivered by **Phase 88** (PR #240,
  before this phase), so `coreutils-smoke` exercises it directly on the ext2 root (hardlink
  nlink + shared-vs-distinct inode assertions) rather than deferring it.
- **Phase 95** (Native Rust Toolchain) will reuse the prebuilt-std musl path to cross-compile
  a proc-macro `.so` — a more complex Rust-cargo port because proc-macros are `dlopen`'d at
  compile time and need the Phase 93 dynamic loader.

## Related Roadmap Docs

- [Phase 94 design doc](./roadmap/94-rust-cargo-uutils.md)
- [Phase 94 task doc](./roadmap/tasks/94-rust-cargo-uutils-tasks.md)
- [Phase 85a — Package substrate](./roadmap/85a-package-infrastructure.md) (the `.m3pkg`/pkgcache/`pkg` installer this phase extends)
- [Phase 44 — Rust cross-compilation](./roadmap/44-rust-cross-compilation.md) (the `build_musl_rust_bins`/`build_ion` precedent)
- [Phase 12 — POSIX compatibility layer](./12-posix-compatibility-layer.md) (the Linux-syscall compat layer uutils rides)

## Deferred or Later-Phase Topics

- **Fully-offline vendored build.** A pre-vendored source tarball (uutils + `cargo vendor`)
  pinned by SHA-256 for hermetic, network-free, reproducible host builds. The first cut
  allows cargo to fetch crates during the host build (`--locked`).
- **Ramdisk promotion.** Selectively replacing individual `no_std` floor applets with uutils
  is out of scope; this phase is coexist-only.
- **SELinux / locale-heavy applets.** `chcon`/`runcon` and any applet depending on facilities
  m3OS lacks are excluded from the feature set, not stubbed.
- **`stat`/inode-identity rigor.** uutils is stricter about `(st_dev, st_ino)` than the
  hand-built tools (`ls -i`, hardlink detection in `du`/`cp`). The enabling fix — a canonical
  `fill_stat()` yielding consistent, non-zero `(st_dev, st_ino)` — landed in **Phase 88**
  (PR #240, before this phase), so this is no longer deferred: `coreutils-smoke` asserts it on
  the ext2 root (hardlink `nlink` 2→1 across an unlink; shared inode for hardlinked names,
  distinct non-zero inodes for distinct files). Deeper locale/`-i`-format edge cases stay
  best-effort.
- **`findutils`/`diffutils`/other uutils projects.** Only `uutils/coreutils` is in scope;
  the Rust-cargo port class this phase establishes makes them straightforward follow-ons.
- **Rust-cargo ports with C FFI.** A future Rust crate that pulls a `cc`-built C dependency
  re-enters the `find_musl_cc()`/stub-archive plumbing — out of scope here.
