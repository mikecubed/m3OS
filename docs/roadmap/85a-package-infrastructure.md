# Phase 85a - Package & Build-Cache Infrastructure

**Status:** Planned
**Source Ref:** phase-85a
**Depends on:** Phase 45 (Ports System) ✅
**Builds on:** Generalizes the Phase 45 ports build — specifically the per-port `target/port-stage/<name>/.stamp` fingerprint and the `target/port-src/` SHA-256 tarball cache in `xtask/src/port_build.rs` — into a portable, content-addressed prebuilt-package store with a relocatable package format and an offline in-OS installer.
**Primary Components:** `xtask/src/port_build.rs`, `xtask/src/main.rs` (`populate_phase_69d_ports`, `DISK_SIZE`, image staging), a new `.m3pkg` format + `target/pkgcache/`, a new userspace `pkg` binary, `ports/` Portfile schema, `docs/85-cross-compiled-toolchains.md`

## Milestone Goal

The build pipeline can seal any port's DESTDIR-staged install tree into a single relocatable **`.m3pkg`** artifact, key it on a content hash of its inputs, and reuse it on later builds without recompiling. A userspace **`pkg install`** can install a `.m3pkg` from a local on-disk repo into `/usr`. The existing ncurses-class ports are retrofitted onto the substrate, proving a second image build of an unchanged tool performs **zero** compiler invocations.

## Why This Phase Exists

The toolchains in 85b/85c/85d are large (a Clang+LLD build links with many GB of RAM over a multi-hour compile; even a stripped static X86-only install runs to several hundred MB). The current ports system already avoids redundant work *within one machine* via the `.stamp` fingerprint, but that fingerprint is not a portable, content-addressed key, there is no relocatable package artifact, and there is no way to install a tool into a running m3OS except by rebuilding the whole image. Without this backbone, every toolchain sub-phase would either rebuild gigabytes from source on each image build or hand-roll its own ad-hoc staging. 85a builds the shared substrate once so 85b/85c/85d simply produce and consume `.m3pkg` artifacts.

## Learning Goals

- How content-addressed build caches key outputs on a hash of (source + toolchain + flags + dependency keys), and how a hit skips the build (Yocto sstate, Bazel action cache, Nix store).
- How DESTDIR staging produces a relocatable install tree, and how relocatable binary package formats (Redox `pkgar`, Alpine apk, Arch `.pkg.tar`) seal and verify it.
- How a minimal offline package installer records an installed-file database and lays files into a prefix.

## Feature Scope

### Area A — Content-addressed cache + `.m3pkg` format

A package key `hash(source rev/tarball SHA + musl toolchain identity + build flags + dependency artifact keys)`; a `.m3pkg` artifact (header with BLAKE3 content hashes + entry index + optional ed25519 signature, plus a data blob — `pkgar`-modeled; a `.tar.zst` + `.sha256` sidecar is the acceptable v1); a `target/pkgcache/<key>.m3pkg` store; and a DESTDIR + relocation contract for the build recipes.

### Area B — Offline in-OS `pkg` installer + ports retrofit

A userspace `pkg` binary (`pkg install <name>`, `pkg list`, `pkg verify`) that installs a `.m3pkg` from a **local on-disk repo** into `/usr`, recording `/var/lib/pkg/db`; image staging that installs from `.m3pkg` artifacts instead of mirroring stage trees; and the retrofit of ncurses/less/htop/tmux/libevent/zlib onto the substrate so the win is proven on existing ports. The **networked** fetch path is explicitly out of scope (Phase 86).

## Important Components and How They Work

### `xtask` packaging (`port_build.rs`)

A new sealing step runs after a port's DESTDIR install: it computes the content key, packs `target/port-stage/<name>/` into `target/pkgcache/<key>.m3pkg`, and records the key. A new resolve step, before any build, checks the cache for a matching key and short-circuits to install-from-artifact — the same control point as today's `.stamp` check at `port_build.rs:343-349`, but keyed portably.

### Image staging (`main.rs`)

`populate_phase_69d_ports` (and its successor) install from `.m3pkg` artifacts via the same `debugfs` write path into the ext2 data disk, rather than walking `target/port-stage`. Disk sizing (`DISK_SIZE`, currently 1 GB) is reviewed for headroom and an explicit budget recorded.

### Userspace `pkg`

A `no_std` userspace binary that opens a `.m3pkg`, verifies its hashes, and extracts entries under `/usr`, recording an installed-package database. Wired through the four-place userspace-binary procedure (workspace member, xtask `bins`, ramdisk entry, service/config if needed).

## How This Builds on Earlier Phases

- Extends the Phase 45 ports build by generalizing the `.stamp` fingerprint (`port_fingerprint` in `port_build.rs:156-182`) into a portable content-addressed key and adding a package artifact + installer.
- Reuses the Phase 45 musl toolchain plumbing (`musl_toolchain`/`find_musl_cc`/`musl_extra_ldflags_joined`) unchanged.

## Implementation Outline

1. Specify the `.m3pkg` format and the content-key algorithm; host-test the key + pack/unpack logic.
2. Add the seal-after-install and resolve-before-build steps to `port_build.rs`.
3. Build the userspace `pkg` installer + installed-file DB.
4. Switch image staging to install from `.m3pkg`.
5. Retrofit existing ports; record disk/RAM budget + relocation contract; bump kernel to `0.85.0`.

## Acceptance Criteria

- A port builds once into a `.m3pkg` under `target/pkgcache/`; a second `cargo xtask image` with unchanged inputs installs from the artifact with **zero** compiler invocations (asserted by a build-log check).
- `.m3pkg` pack/unpack + content-key logic is host-tested (`cargo xtask check`).
- `pkg install <name>` installs a bundled `.m3pkg` into `/usr` inside m3OS and `pkg list` shows it; verified on a boot.
- ncurses/less/htop/tmux/libevent/zlib are delivered as `.m3pkg` artifacts (retrofit), with no behavior regression in the existing TUI gates.
- The disk/RAM budget and the DESTDIR/relocation contract are documented; the static-repo hosting plan is recorded with the network fetch path deferred to Phase 86.

## Companion Task List

- [Phase 85a Task List](./tasks/85a-package-infrastructure-tasks.md)

## How Real OS Implementations Differ

- Redox `pkgar` carries full ed25519 signing + a fixed 136-byte remote header for partial fetch; 85a's v1 may ship hash-only verification and add signing later.
- Yocto/Nix recurse the dependency-hash graph to the full transitive closure; 85a keys on direct dependency artifact keys only.
- Mature managers ship a dependency solver, multiple repos, and atomic upgrades; 85a is a flat install-only model.

## Deferred Until Later

- Networked `pkg install`/`update` over HTTPS, `/etc/pkg.d/` remote repos, signing-key distribution — Phase 86.
- A dependency solver, package removal/upgrade transactions, and delta packages.
