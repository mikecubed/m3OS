# Phase 94 - Rust-Cargo Ports & uutils Coreutils

**Status:** Planned
**Source Ref:** phase-94
**Depends on:** Phase 12 ✅ (Linux-syscall compat), Phase 40 ✅ (threading/futex/TLS), Phase 44 ✅ (Rust cross-compilation lineage), Phase 85a ✅ (`.m3pkg` package & build-cache substrate)
**Builds on:** Establishes the project's **first Rust-cargo cross-compiled port class** on top of the Phase 85a `.m3pkg` substrate and the Phase 12 Linux-syscall compatibility layer. Reuses the Phase 44 "Rust runs in the OS" lineage but targets `x86_64-unknown-linux-musl` (std) rather than the bare-metal `x86_64-unknown-none` target the kernel and the hand-built userspace use. Does **not** replace the hand-built `coreutils-rs`; it shadows it via PATH precedence.
**Primary Components:** `xtask/src/port_build.rs` (`build_uutils`), `ports/util/coreutils/Portfile`, `pkg-format` (symlink round-trip), `userspace/shell` (PATH precedence — relied upon, unchanged), `coreutils-smoke` gate

## Milestone Goal

`pkg install coreutils` installs the upstream [uutils/coreutils](https://github.com/uutils/coreutils) — a single statically-linked Rust multicall binary plus per-applet symlinks — into `/usr/local/bin`, where it transparently shadows the hand-built `coreutils-rs` set by PATH precedence. The result is a GNU-compatible, upstream-maintained, well-tested coreutils running unmodified on m3OS, delivered as the project's first Rust-cargo `.m3pkg`. The hand-built `no_std` set remains the early-boot floor and the uninstall fallback.

## Why This Phase Exists

m3OS ships 63 hand-built coreutils applets (`userspace/coreutils-rs/`) — bare-metal `#![no_std]` binaries compiled for `x86_64-unknown-none` against `syscall-lib`, embedded directly in the kernel ramdisk (`kernel/src/fs/ramdisk.rs`). They are small and boot-critical, but they are bespoke: their flag coverage and edge-case behavior are partial reimplementations of GNU semantics, maintained by hand, and not exercised by any upstream test suite.

uutils/coreutils is a mature, GNU-compatible coreutils written in Rust, continuously tested against the GNU coreutils test suite. Adopting it gives m3OS correct, maintained tools without the per-applet maintenance burden — *if* it can run on the OS.

It can. m3OS already runs statically-linked musl C binaries (`git`, `curl`, `python`, `clang`) through the Phase 12 Linux-syscall compatibility layer (`kernel/src/arch/x86_64/syscall/mod.rs`: `syscall_entry` → `syscall_handler`, ~121 Linux syscalls) and the static-ELF loader (`kernel/src/mm/elf.rs`). A std Rust binary built for `x86_64-unknown-linux-musl` static-links musl and issues the exact same Linux syscalls — it is just another static musl ELF. The runtime substrate is therefore already proven; what is *new* is the build plumbing for the **port class**. Every existing *port* (`xtask/src/port_build.rs`) is autotools/cmake/make C/C++; there is **no Rust-cargo cross-build path among the ports**. The `x86_64-unknown-linux-musl` target itself is *not* new to the repo — `build_musl_rust_bins` and `build_ion` (`xtask/src/main.rs:3412`, `:3533`, Phase 44) already cross-compile std Rust crates for it with prebuilt std + `-C target-feature=+crt-static` and a `rustup target list --installed` availability probe — but that wiring has never been routed through a `Portfile`/`.m3pkg` recipe, and the target is not the workspace default (`.cargo/config.toml` defaults to bare-metal `x86_64-unknown-none`). This phase adds the *port-class* path, reusing the Phase 44 musl-target precedent for the toolchain wiring, and validates it end-to-end with uutils as the first consumer.

## Learning Goals

- How a std Rust binary built for `x86_64-unknown-linux-musl` becomes "just another static musl ELF" that the Linux-compat layer runs unmodified — and why the self-contained Rust musl target needs no external musl-gcc for a pure-Rust crate.
- How a multicall binary + applet symlinks is the size-efficient packaging shape, and how `pkg-format` round-trips symlinks through `.m3pkg` install.
- How PATH precedence (`/usr/local/bin` first) lets an installed port *shadow* a ramdisk-resident tool without removing it — and why the ramdisk floor must remain.
- The difference between the bare-metal `-Zbuild-std` path (kernel + hand-built userspace) and the prebuilt-std `rustup target add` path (musl ports).

## Feature Scope

### Area A — Rust-cargo musl port class (the new build path)

A new `build_uutils()` dispatch in `xtask/src/port_build.rs` cross-compiles a std Rust crate for `x86_64-unknown-linux-musl`:

- Adds `x86_64-unknown-linux-musl` as a build target (`rustup target add` for the pinned toolchain). Unlike the kernel/userspace bare-metal target, this target has **prebuilt std** — no `-Zbuild-std`.
- The Rust musl target is **self-contained** (bundles its own musl + uses `rust-lld`), so a pure-Rust crate needs **no** external `x86_64-linux-musl-gcc`, sidestepping the empty-static-compat-archive plumbing (`find_musl_cc`/`musl_extra_ldflags_joined`) that the C ports require. (If a future Rust port pulls a `cc`-built C dependency, it would re-enter that plumbing — out of scope here; uutils' selected feature set is pure Rust.)
- Static linking is the musl-target default (`-C target-feature=+crt-static`), producing a single fully-static `coreutils` binary that the existing `strip_stage` shrinks before sealing.

### Area B — uutils delivered as a coexisting `.m3pkg`

- **Multicall + symlinks:** build the single upstream `coreutils` multicall binary, then stage per-applet **symlinks** (`ls -> coreutils`, `cat -> coreutils`, …) under `usr/local/bin/`. `pkg-format` already round-trips symlinks (`pkg-format/src/lib.rs`: `is_symlink`, `pack` via its private `collect` walker, and the `clear -> tput` symlink case inside the `pack_unpack_round_trips_bytes_and_modes` test), and `seal_package` packs via `pkg_format::pack`, so the symlinks survive `.m3pkg` install. (This is distinct from the git/dropbear "won't round-trip" notes, which are about *no content dedup* — `.m3pkg` stores file content per path, so a symlink to a multi-MB binary would expand to a full copy; git's builtins are hardlinks to one 3.7 MB binary, and the dropbear comment at `port_build.rs:3082` warns a symlink *to a large binary* costs a full copy. uutils' applet symlinks point at one binary and are tiny, so symlink **semantics** round-trip and the size concern does not bite.)
- **Coexist, don't replace:** the package installs to `/usr/local/bin`, which is **first** in the shell's PATH (`userspace/shell/src/main.rs`: `/usr/local/bin:/bin:/sbin:/usr/bin`), so uutils applets shadow the `/bin` hand-built ones automatically. The ramdisk `coreutils-rs` set is **not** removed: it is the only coreutils available before the data disk mounts during early `init`, and it is the fallback if `coreutils` is uninstalled.
- **Leaf package:** the selected uutils feature set is pure Rust, so `DEPS=` is empty — the simplest possible solver entry, contrasting with the `zlib → mbedtls → … → git` chain.
- **Bundled, not pre-installed:** ships in `/usr/pkg/` (the `BUNDLE_ONLY_PORTS` list) and is pulled on demand by `pkg install coreutils`, mirroring `git`/`python`.

## Important Components and How They Work

### `build_uutils()` (new, `xtask/src/port_build.rs`)

Registered in the `fn port_build` `match name` dispatch and in the `ports/util/coreutils/Portfile`. It downloads the pinned uutils source tarball (SHA-256 in the Portfile), runs `cargo build --release --target x86_64-unknown-linux-musl --no-default-features --features "<curated unix set>" --locked`, copies the resulting `coreutils` binary into `<stage>/usr/local/bin/`, and creates the applet symlinks from the enabled applet list. The host build machine fetches crates from crates.io the same way the C ports fetch their source tarball; `--locked` pins against the upstream `Cargo.lock` for build determinism. (A fully-offline vendored-source tarball is the reproducibility upgrade — see *Deferred*.)

The curated feature set is **derived deterministically**, not guessed: start from uutils' `feature_unix` umbrella feature, reconcile the result to be a **superset of the 63 `[[bin]]` applets** in `userspace/coreutils-rs/Cargo.toml`, then subtract an **enumerated, commented** exclusion list (at minimum SELinux `chcon`/`runcon`, plus any applet depending on a facility m3OS lacks). The exact resulting `--features` string is pinned in `build_uutils` against the chosen uutils release (see task B.3). Because pure-Rust ports need no external `x86_64-linux-musl-gcc`, `build_uutils` may follow the `go`-style `if name == "coreutils"` early-return branch (a self-contained-toolchain port) rather than the shared-musl-gcc `match` arm — either wiring is valid as long as it is reachable from `fn port_build`.

### `pkg-format` symlink round-trip (existing, relied upon)

`pack` (via its private `collect` walker) captures symlinks with their target as content and `st_mode` file-type bits; `unpack` restores them via `std::os::unix::fs::symlink`. The `pack_unpack_round_trips_bytes_and_modes` test (`pkg-format/src/lib.rs`, its `clear -> tput` symlink case) proves a relative symlink under the staged prefix round-trips as a symlink, not a copy. This is the mechanism that makes the multicall shape affordable — without it, ~100 applets would each pack as a full copy of the multi-MB binary.

### Linux-syscall compat layer + static-ELF loader (existing, relied upon)

uutils runs through the same path as `git`/`python`: musl libc issues Linux syscalls, `syscall_entry` marshals them to `syscall_handler`, and the static-ELF loader maps the binary. The syscalls uutils exercises are already implemented: `openat`/`read`/`write`/`getdents64`/`newfstatat`/`statfs`/`readlinkat`/`symlinkat`/`fchmodat`/`utimensat`/`getrandom`, plus `clone`/`futex`/`set_tid_address` for any threaded applet (Phase 40 + the Phase 77 futex `CHILD_CLEARTID` lost-wakeup fix; Python's threads already exercise this).

### PATH precedence (existing, relied upon)

The shell searches `/usr/local/bin` first. No shell change is needed; installing uutils there is sufficient to shadow `/bin`. Uninstalling `coreutils` reverts to the ramdisk tools with no further action.

## How This Builds on Earlier Phases

- **Extends Phase 85a** by adding the first Rust-cargo recipe to the `.m3pkg`/pkgcache substrate; the content-addressed seal/resolve and offline in-OS `pkg install` are reused unchanged.
- **Reuses the Phase 12** Linux-syscall compat layer and static-ELF loader as the runtime — no kernel change is expected.
- **Reuses Phase 40 + Phase 77** threading/futex for parallel applets (`sort`).
- **Continues the Phase 44** "Rust in the OS" lineage, but via prebuilt-std musl cross-compilation rather than the bare-metal `-Zbuild-std` path.
- **Coexists with the Phase 41 / `coreutils-rs`** hand-built set rather than replacing it; the ramdisk floor stays for early boot.

## Implementation Outline

1. **De-risk the toolchain (Track A).** Add `x86_64-unknown-linux-musl` to the toolchain; cross-build a trivial std Rust "hello" crate; confirm it boots and runs on m3OS through the compat layer. Confirm a relative symlink round-trips through `seal_package` → `pkg install`.
2. **Write the port recipe (Track B).** `ports/util/coreutils/Portfile` (pinned version + SHA-256, `DEPS=`); `build_uutils()` with the curated feature set; stage multicall binary + applet symlinks; register in the dispatch and the `PORTS`/`BUNDLE_ONLY_PORTS` lists.
3. **Wire packaging & integration (Track C).** Seal to `.m3pkg`, bundle into `/usr/pkg/`, prove `pkg install coreutils` materializes the binary + symlinks into `/usr/local/bin`; keep the ramdisk set intact.
4. **Validate (Track D).** Add `coreutils-smoke` + a `M3OS_COREUTILS_REGRESSION` opt-in gate.
5. **Document & close out (Track E).** This design doc, the task doc, the roadmap README row (Status flipped to `Complete` at landing), the AGENTS.md package-management capability-bullet decision, the **Phase 94 learning doc** (`docs/94-rust-cargo-uutils.md`) linked from `docs/README.md` + `docs/appendix/codebase-map.md`, and the **unconditional kernel version bump** `0.93.0` → `0.94.0`.

## Learning Documentation Requirement

- Create `docs/94-rust-cargo-uutils.md` using the *aligned legacy learning doc* template in `docs/appendix/doc-templates.md` (seven sections: Overview, What This Doc Covers, Core Implementation, Key Files, How This Phase Differs From Later Work, Related Roadmap Docs, Deferred or Later-Phase Topics).
- Explain: the **prebuilt-std `rustup target add`** path versus the bare-metal `-Zbuild-std` path the kernel/userspace use; why the self-contained Rust musl target needs **no external `x86_64-linux-musl-gcc`** (contrasting the C ports' `find_musl_cc`/stub-archive plumbing) and how `build_musl_rust_bins` (Phase 44) already proved it; the **multicall-binary + applet-symlink** packaging shape and how `pkg-format` round-trips symlinks; and how **PATH precedence** lets an installed port *shadow* a ramdisk tool without removing the early-boot floor.
- Link it from `docs/README.md` (the Phase-Aligned Learning Docs table) and register it in `docs/appendix/codebase-map.md` (the Documentation Index) when the phase lands.

## Related Documentation and Version Updates

- Update `docs/roadmap/README.md` (flip the Phase 94 row Status `Planned` → `Complete` at landing) and `docs/README.md` (add the learning-doc row).
- Decide the `AGENTS.md` package-management capability-bullet edit (one-line rewrite to name the Rust-cargo musl port class, or no edit with rationale — per the "keep it small" policy).
- **Bump `kernel/Cargo.toml` `version` `0.93.0` → `0.94.0` and the `AGENTS.md` "kernel **v0.93.0**" line to match — unconditionally, as the standard per-phase bump.** No kernel *code* change is expected (uutils rides the existing Phase 12 compat layer); the version string is the only kernel-side edit, and every version reference (boot banner, `/proc/version`, `uname`) derives from `env!("CARGO_PKG_VERSION")`, so the single `Cargo.toml` edit propagates. A *patch* bump on top applies only if a syscall gap surfaces during bring-up.

## Acceptance Criteria

- `cargo xtask port build coreutils` produces a single static `coreutils` ELF for `x86_64-unknown-linux-musl` plus the applet symlinks, and seals a valid `coreutils.m3pkg` (`pkg_format::verify` passes).
- The sealed `.m3pkg` round-trips symlinks: after `pkg install coreutils`, `/usr/local/bin/ls` is a symlink whose target is `coreutils` (verified on-device), not a copy.
- On m3OS: `pkg install coreutils` succeeds with `DEPS=` empty (no chain), and `ls --version` reports the uutils version string (proving the static Rust musl binary *runs* via the compat layer).
- PATH shadowing works **without a path-resolver builtin** (m3OS's `sh0` has only `cd`/`exit`, and there is no `which`/`command`/`type` applet): with the package installed, `/usr/local/bin/ls` exists as a symlink to `coreutils` and `ls --version` prints the uutils banner (it wins because `PATH_DIRS` puts `/usr/local/bin` first); after `pkg remove coreutils`, `ls` falls back to the ramdisk `/bin/ls` — the hand-built output, with no uutils version string — and the shell still reaches a working prompt. Shadowing is proven by **which implementation's output you get**, not by resolving a path.
- A behavior battery passes with GNU-compatible output: `ls -la /`, `cp`/`mv`/`rm` a file tree, `wc -l`, `cat`, `sort` (including an input large enough to trigger any parallel path), `env`, and `sha256sum` whose 64-hex-digit **digest field** matches the existing `crypto-lib`-based `/bin/sha256sum` on the same input (compare the digest token, not the whole line, since GNU- vs hand-built output framing may differ).
- `coreutils-smoke` PASSES end-to-end in CI under `M3OS_COREUTILS_REGRESSION=1` (skip-with-reason when the musl/cargo toolchain is absent, mirroring `git-https-smoke`).
- The ramdisk `coreutils-rs` set is unchanged and the OS still boots and reaches a login prompt with `coreutils` **not** installed.
- The kernel reports `0.94.0` (`kernel/Cargo.toml` `version` + the `AGENTS.md` line), and the Phase 94 learning doc (`docs/94-rust-cargo-uutils.md`) ships, linked from `docs/README.md` and `docs/appendix/codebase-map.md`.

## Companion Task List

- [Phase 94 Task List](./tasks/94-rust-cargo-uutils-tasks.md)

## How Real OS Implementations Differ

- A real distro ships uutils (or GNU coreutils) as the **base** `/usr/bin` set installed by the package manager onto a writable root, with no separate "ramdisk floor" — the initramfs and the installed root are the same userland lineage. m3OS keeps a distinct bare-metal `no_std` floor because its earliest boot predates the data-disk mount.
- Distros build uutils dynamically against the system `libc.so`; m3OS must build it **fully static** because its `ld-musl` has no real `libc.so` (the same constraint that forced static Python in Phase 85c; lifted only by the future Phase 93 dynamic C runtime).
- Real package managers resolve and update from a network repo with signatures; m3OS installs offline from a bundled `/usr/pkg/` with content-addressed `.m3pkg`s.

## Deferred Until Later

- **Fully-offline vendored build.** A pre-vendored source tarball (uutils + `cargo vendor`) pinned by SHA-256 for hermetic, network-free, reproducible host builds. The first cut allows cargo to fetch crates during the host build (`--locked`).
- **Promotion into the ramdisk.** Selectively replacing individual `no_std` floor applets with uutils is out of scope; this phase is coexist-only.
- **SELinux / locale-heavy applets.** `chcon`/`runcon` and any applet depending on facilities m3OS lacks are excluded from the feature set, not stubbed.
- **`stat`/inode-identity rigor.** uutils is stricter about `(st_dev, st_ino)` than the hand-built tools (e.g. `ls -i`, hardlink detection in `du`/`cp`), so it may surface the VFS `st_ino` inconsistency that **Phase 88** addresses; full correctness there rides on Phase 88.
- **`findutils`/`diffutils`/other uutils projects.** Only `uutils/coreutils` is in scope; the Rust-cargo port class this phase establishes makes them straightforward follow-ons.
