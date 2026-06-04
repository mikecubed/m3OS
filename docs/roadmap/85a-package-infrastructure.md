# Phase 85a - Package & Build-Cache Infrastructure

**Status:** Implemented (kernel 0.85.0)
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
- **zlib host `.m3pkg` retrofit** — zlib is currently served by the separate
  in-guest `target/ports-src` system (its `Portfile` carries a placeholder
  tarball SHA) and has no host `build_zlib` recipe; folding it onto the host
  `.m3pkg` substrate (a `build_zlib` recipe + a verified Portfile SHA) is a
  small follow-up. The 5 host-built ports (ncurses, libevent, less, htop, tmux)
  are retrofitted in 85a.

## Resource Budget, Relocation Contract & Hosting (Track E)

### E.1 — Disk / RAM budget

**On-image disk (measured on a real 85a build).** The 5 retrofitted ports add
~34 MB to the 1 GB ext2 data disk:

| Component | Size |
|---|---|
| `/usr/pkg/*.m3pkg` (offline repo: ncurses 7.75 MB, tmux 1.75 MB, libevent 1.57 MB, htop 0.80 MB, less 0.75 MB) | ~13 MB |
| Pre-installed tree under `/usr` (ncurses terminfo DB ≈ 12 MB dominates) | ~21 MB |
| **85a total** | **~34 MB (~3 % of the 1 GB disk)** |

**Projected umbrella footprints** (for 85b/85c/85d planning): git ≈ tens of MB,
Python ≈ tens of MB, Clang several hundred MB even as a stripped static
X86-only install. The 1 GB disk comfortably holds 85a + git + Python; the
opt-in **Clang** artifact is the only component that would not fit alongside
everything in 1 GB.

**Host-build RAM / link memory** (distinct from on-image disk). The 85a ports
build in well under 1 GB of RAM. The 85d **Clang + LLD** build is the outlier:
its final (especially LTO) link can require **many GB of RAM (≈8–16 GB+)** on
the build host / CI runner — this is a host-build constraint, not an on-image
disk constraint, and is the main reason 85a builds the toolchains **once** and
ships them as artifacts (the whole point of this phase).

### E.1 — DESTDIR + relocation contract

Recipes that feed the `.m3pkg` substrate MUST:

1. **Build at the final runtime prefix** (`--prefix=/usr` for the toolchains;
   the existing ncurses-class ports use `--prefix=/usr/local` + datadir
   `/usr/share`), and **stage** via `make install DESTDIR=<target/port-stage/<name>>`
   so the packed tree's entry paths are prefix-relative (`usr/...`) and the
   `pkg` installer lays them under `/` unchanged.
2. **Strip executables and shared objects before sealing** to keep artifacts
   small (mandatory for the multi-hundred-MB Clang artifact; the 85a
   ncurses-class binaries are not yet stripped — a minor future optimization,
   recorded here for honesty).
3. Keep **relocatable internal layout**: Clang's resource dir is resolved
   relative to the binary; Python uses a fixed relative `bin/` + `lib/pythonX.Y/`
   layout. No build-prefix-absolute paths baked into the installed files.

The `.m3pkg` v1 byte layout itself is documented authoritatively in
`pkg-format/src/lib.rs` (magic + version + reserved ed25519 signature +
per-entry path/mode/**SHA-256**/index + data blob). **Recorded hashing choice:**
SHA-256 (a compact in-crate pure-`u32` implementation), not BLAKE3, and a
custom binary header, not `.tar.zst` — `blake3`/`zstd` are unavailable in the
offline build environment and the RustCrypto `sha2` crate does not codegen on
the soft-float / no-SSE `x86_64-unknown-none` target the installer builds for.
The A.2 fallback clause permits this; the ed25519 field is reserved (zeroed) for
the Phase 86 signed networked repo.

### E.2 — Hosting / distribution plan (network fetch is Phase 86)

- **Static-repo layout + host.** Publish `.m3pkg` artifacts as a flat static
  repo on **GitHub Releases of an `m3os-pkgs` repository** (or a `gh-pages`
  branch), mirroring the Redox model (`REPO_BINARY` → `static.redox-os.org/pkg`).
  CI builds each large toolchain **once** and uploads the artifact; routine
  image builds and (Phase 86) in-OS `pkg install` fetch it instead of rebuilding.
- **Phase 86 handoff.** Networked `pkg install` / `pkg update` over HTTPS plus
  `/etc/pkg.d/` remote-repo config is explicitly Phase 86 — cross-referenced
  from `docs/roadmap/86-networking-and-github.md`.
- **Trust.** The hash-only `.m3pkg` verification (A.2) is sufficient for
  **offline / local** install (the artifact is already on a trusted disk). The
  **networked** Phase 86 install over an untrusted transport will require the
  reserved **ed25519** signature field to be populated and verified against a
  distributed public key.

### E.4 — Data-disk sizing finding

**The existing 1 GB `DISK_SIZE` is sufficient for Phase 85a** (5-port retrofit
≈ 34 MB; ~3 % utilisation) and for the projected git + Python footprints, so
**no resize is performed in 85a** — the default image stays 1 GB. The only
component that would overflow 1 GB is the opt-in **Clang** artifact (several
hundred MB packed, ~1 GB unpacked); growing `DISK_SIZE` for a **gated** Clang
image is therefore deferred to **Phase 85d**, where the artifact actually
exists, so the default (no-Clang) image is never enlarged. This finding is
recorded explicitly rather than assumed.
