# Phase 85 - Cross-Compiled Toolchains (Umbrella)

**Status:** Planned
**Source Ref:** phase-85
**Depends on:** Phase 36 (Expanded Memory) ✅, Phase 44 (Rust Cross-Compilation) ✅, Phase 45 (Ports System) ✅, Phase 83 (Release 1.0 Gate) ✅
**Builds on:** Turns the existing Rust cross-compilation and ports baseline into a broader post-1.0 developer-toolchain story with larger bundled host-built binaries (git, Python, Clang) — and, first, into a **build-once / install-prebuilt packaging substrate** so those large artifacts are never rebuilt from source on a routine image build.
**Primary Components:** `xtask/src/port_build.rs`, `xtask/src/main.rs` (image staging, disk sizing), `ports/`, `kernel/src/fs/ramdisk.rs` / ext2 disk layout, a new `.m3pkg` package format + content-addressed package cache, a userspace `pkg` installer, `docs/git-roadmap.md`, `docs/python-roadmap.md`, `docs/clang-llvm-roadmap.md`

> **This is an umbrella phase, delivered as four sub-phases (85a–85d).** It follows the Phase 78 (USB Host Foundation) pattern: the umbrella doc defines the theme, scope split, and shared architecture; each sub-phase has its own design doc and task list and lands its own kernel-version patch bump. There is **no separate umbrella task list** — the companion task lists are the four sub-phase lists below.

## Milestone Goal

m3OS can host larger post-1.0 development tools — git, Python, and Clang — built once on the host and installed into the image as **prebuilt, relocatable, content-addressed packages** rather than rebuilt from source on every image build. git, Python, and Clang become routine parts of the developer environment instead of future one-off experiments, and the packaging substrate that makes them affordable retro-applies to the existing ncurses-class ports.

## Why This Phase Exists

Once the project has a defined release boundary (Phase 83), it can grow into a richer developer platform without muddying what 1.0 meant. Larger cross-compiled toolchains are one of the highest-leverage ways to make the OS more useful for real work, but they bring bigger binaries, larger libraries, more build-system complexity, and — the decisive cost — **multi-gigabyte, multi-hour from-source rebuilds** (a Clang+LLD build links with many GB of RAM over a multi-hour compile; even a stripped static X86-only install runs to several hundred MB — verify on a real build before quoting a figure). Rebuilding that on every `cargo xtask image` is intolerable.

This phase exists to make that growth **deliberate, reproducible, and cheap to rebuild**. The keystone is sub-phase **85a**: a build-once packaging substrate (content-addressed cache + relocatable package format + an in-OS installer) modeled on Redox's `cookbook` + `pkgar` + `REPO_BINARY` design and Yocto's shared-state cache. Only once that backbone exists do the three toolchains (85b/85c/85d) ride on top of it.

## Sub-Phase Decomposition

| Sub | Theme | Primary Outcome | Depends on | Kernel |
|---|---|---|---|---|
| **85a** | Package & Build-Cache Infrastructure | Content-addressed prebuilt-package cache + relocatable `.m3pkg` format + DESTDIR/relocation contract + offline in-OS `pkg` installer; existing ports retrofitted onto it | 45 | `0.85.0` |
| **85b** | git (local) | `git` built `NO_CURL NO_OPENSSL` (+zlib), local repo workflows (init/add/commit/log/diff/branch/merge), packaged via 85a; first real tool to exercise the substrate end-to-end | 85a | `0.85.1` |
| **85c** | Python (CPython) | Two-stage cross-built CPython + comprehensive non-networked stdlib, REPL + script workloads, packaged via 85a | 85a | `0.85.2` |
| **85d** | Clang/LLVM/LLD (+Release) | Host-cross-built static Clang + LLD (X86-only, `MinSizeRel`), C/C++ sample builds inside m3OS, feature-gated heavy artifact; carries the umbrella learning doc + capability cut | 85a (85b/85c land first) | `0.85.3` |

**Ordering rationale.** 85a is the backbone and must land first; 85b is the smallest tool and validates the substrate end-to-end at low risk; 85c is medium; 85d is the heavyweight artifact (multi-GB-RAM, multi-hour build) whose rebuild cost is the entire reason 85a exists, so it lands last and is gated behind an opt-in image feature. The whole family is **post-1.0 growth** — the kernel stays phase-tracked (`0.85.x`), never SemVer `1.0.0` (the Phase 83 posture).

## Build-Once Packaging & Distribution Architecture

This is the shared architecture every sub-phase consumes; it is specified in full in [Phase 85a](./85a-package-infrastructure.md). Summary:

### What exists today (the starting point)

The ports system (`xtask/src/port_build.rs`) **already caches two layers**: source tarballs by `SHA256` under `target/port-src/`, and built stage trees by a fingerprint of `Portfile + patches + port_build.rs` under `target/port-stage/<name>/.stamp` (a fingerprint match **skips configure/make/install entirely**). Built ports are staged into the **ext2 data disk** (1 GB, `DISK_SIZE`) via `debugfs` in `populate_phase_69d_ports`. There is **no** relocatable package artifact, no content-addressed cross-machine cache, and **no in-OS package manager** today.

### What 85a adds (the "better way")

1. **Content-addressed package cache (the central idea).** Each port builds once into a DESTDIR-staged tree, which is sealed into a single relocatable artifact keyed by `hash(source rev/tarball + musl toolchain identity + build flags + dependency artifact keys)` — Yocto-sstate / Bazel-action-cache style. A matching key ⇒ *install the stored artifact*; no rebuild. This generalizes the existing per-port `.stamp` fingerprint into a portable, cross-machine key and **retro-applies the win to ncurses/less/htop/tmux** for free.
2. **A relocatable `.m3pkg` format**, modeled on Redox `pkgar`: a small header (BLAKE3 content hashes + an entry index, optional ed25519 signature) plus a data blob, installable to any prefix. (A `.tar.zst` + sidecar `.sha256` is the acceptable v1 if full pkgar crypto is more than wanted initially.)
3. **A DESTDIR + relocation contract.** Build with the real on-image prefix (`/usr`), `make install DESTDIR=<stage>`, seal `<stage>`. Relocation is mandatory for the big tools: Clang must resolve its resource dir (`lib/clang/<ver>/{include,lib}`) relative to the executable; Python relies on the `sys.prefix` landmark search with `bin/` + `lib/pythonX.Y/lib-dynload/` kept in fixed relative layout.
4. **An offline in-OS `pkg` installer.** A userspace `pkg install <name>` that installs a `.m3pkg` from a **local on-disk repo** (packages bundled on the data disk) into `/usr`, recording an installed-package database — **no network required**, so it fits inside the Phase 85 (pre-networking) boundary.
5. **A two-tier cache + a hosting/distribution plan.** Default to a local `target/pkgcache/<key>.m3pkg`. The optional remote tier — a static HTTP(S) package repo so CI builds Clang once and every later build/developer downloads it — is specified here but its **network fetch path is deferred to Phase 86** (it needs DNS + HTTPS).

### Hosting & distribution recommendation

- **Recommended host:** **GitHub Releases on a dedicated `m3os-pkgs` repository** — free, ≤2 GB per asset (fits Clang), integrates with the `gh` workflow already in use; a GitHub Actions job builds each toolchain once and uploads the `.m3pkg` assets. The static-repo URL layout mirrors Redox's `static.redox-os.org/pkg`. (Alternative: a `gh-pages` static repo, per the `redox-os-builder` precedent.) The final infrastructure choice is an implementation-time decision and the plan treats the backend as swappable.
- **In-OS package manager (apt/pacman analogue):** yes — the `pkg` client. Its **offline local-repo install lands in 85a**. Its **networked `pkg install`/`pkg update` over HTTPS from the hosted repo is deferred to [Phase 86](./86-networking-and-github.md)** (DNS + TLS), including `/etc/pkg.d/` repo registration (the Redox model). This is the explicit Phase 85 → 86 handoff.

## Learning Goals

- Understand how large host-built toolchains are staged into the m3OS image — and why "build from source every time" does not scale past ncurses-class ports.
- Learn the build-once / install-prebuilt pattern: content-addressed caching, DESTDIR staging, relocatable binary packages, and how real systems (Redox `cookbook`/`pkgar`, Yocto sstate, Nix, Buildroot, Alpine/Arch) implement it.
- Learn how disk size, memory pressure, and runtime expectations change once binaries become much larger than the early core utilities.
- See how the standalone roadmaps for git, Python, and Clang map onto the official phase plan, and where the local/remote boundary sits relative to Phase 86.
- Understand the difference between "toolchain exists" and "toolchain is part of the supported developer workflow."

## Feature Scope

### Packaging substrate (85a)

The content-addressed cache, `.m3pkg` format, DESTDIR/relocation contract, offline `pkg` installer, image-staging that installs from artifacts instead of rebuilding, and the retrofit of existing ports. Comprehensive details in the 85a design doc.

### git for local development workflows (85b)

Bundle git in a configuration suitable for local repository work. A musl `git` built with **`NO_CURL NO_OPENSSL`** (linking the existing `ports/lib/zlib`) is the right shape — it covers local repos (init/add/commit/log/diff/status/branch/merge). HTTPS/curl/TLS is deliberately deferred to Phase 86.

> **Secure-remote note (2026-05-29):** combined with an `ssh` client binary, the `NO_CURL NO_OPENSSL` build also yields the cheapest *first secure remote clone* (git's SSH transport shells out to `ssh` + `git-upload-pack`). That pairing is tracked in the secure-transport track of [`docs/roadmap/86-networking-and-github.md`](./86-networking-and-github.md#pre-planning-findings-2026-05-29--secure-transport-track), not 85b.

### Python interpreter and standard library (85c)

A host-built CPython (two-stage cross build via `--with-build-python`) with a comprehensive non-networked stdlib for scripting, REPL use, and local automation. Networking-dependent modules (`ssl`, `socket` DNS, `pip`, `asyncio`) are deferred to Phase 86.

### Clang/LLD and larger C/C++ builds (85d)

A post-TCC toolchain capable of building larger or more optimized native programs: a host-cross-built static **Clang + LLD**, `LLVM_TARGETS_TO_BUILD="X86"`, `MinSizeRel`, threads off, bundled with the musl sysroot + builtin headers + `compiler-rt` builtins + the C++ runtime (`libc++`/`libc++abi`/`libunwind`), behind an opt-in image feature because of its multi-hundred-MB footprint and multi-GB-RAM build.

### Toolchain staging and image layout (cross-cutting)

How the build pipeline stages, caches, installs, and validates these larger toolchains so the growth is maintainable instead of magical — owned by 85a and exercised by 85b/85c/85d.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Content-addressed prebuilt-package cache (85a) | The whole phase is unaffordable if Clang rebuilds from source on every image build |
| Reproducible host-build and staging flow | Large tools are not useful if the image pipeline is brittle |
| Relocation contract for Clang + Python | A package that only runs from its build prefix is not installable |
| Documented disk/RAM expectations | These binaries materially change system resource assumptions |
| Local git, Python, and Clang workflows | They are the core value of the phase |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Packaging-substrate baseline | 85a's cache + `.m3pkg` + offline installer exist and a second image build of an unchanged tool performs **zero** compiler invocations | Land 85a before any toolchain sub-phase |
| Release-boundary baseline | Phase 83 has already separated 1.0 commitments from 1.x growth | Add the missing support-boundary documentation first |
| Runtime baseline | Phases 36, 44, and 45 are stable enough for large binaries and installable software | Pull missing memory or packaging cleanup into this phase |
| Image-layout baseline | The system has a documented place for large toolchains and their libraries | Add the missing filesystem or staging-layout work |
| Validation baseline | There is a repeatable way to prove git, Python, and Clang work in the supported environment | Add the missing post-build validation steps |

## Important Components and How They Work

### Content-addressed package cache + `.m3pkg` (85a)

The real backbone of the phase. It caches host-built outputs keyed by their inputs, seals them into relocatable packages, copies them into the right image locations, and validates the install layout reproducibly — generalizing the existing `target/port-stage/<name>/.stamp` fingerprint into a portable, content-addressed artifact store.

### Toolchain-specific runtime expectations

git, Python, and Clang each bring different expectations: templates (`share/git-core/templates`), stdlib files (`lib/pythonX.Y` + `lib-dynload`), headers, libraries (`lib/clang/<ver>`), and large executable footprints. Each sub-phase makes those expectations explicit in the disk layout and documentation.

### Developer workflow integration

The toolchains matter only if they fit the supported developer workflow on m3OS — how they are invoked, where they live (`/usr/bin`, `/usr/lib`), how they are installed (`pkg install`), and what the project considers the normal supported use cases.

## How This Builds on Earlier Phases

- Builds on Phase 44's Rust cross-compilation baseline and Phase 45's ports and package layout — and **generalizes Phase 45's per-port `.stamp` caching** into the 85a content-addressed package store.
- Depends on Phase 83 so this larger ecosystem work is clearly treated as post-1.0 growth instead of hidden release debt.
- Prepares the ground for richer networked developer workflows in Phase 86 (remote git, `gh`, and the **networked** `pkg` fetch path).

## Implementation Outline

1. **85a** — Define `.m3pkg` + the content-addressed cache key; build the offline `pkg` installer; retrofit existing ports; document disk/RAM/relocation contract and the hosting plan.
2. **85b** — Cross-build local git (`NO_CURL NO_OPENSSL` + zlib), package via 85a, validate local repo workflows inside m3OS.
3. **85c** — Two-stage cross-build CPython + comprehensive stdlib, package via 85a, validate REPL + script workloads.
4. **85d** — Host-cross-build static Clang + LLD (X86-only), package via 85a behind an opt-in feature, validate C/C++ sample builds; cut the umbrella learning doc + capability inventory.
5. Throughout — record disk, RAM, and support-boundary implications; align the revived standalone roadmaps with the phase docs.

## Learning Documentation Requirement

- Create the umbrella learning doc `docs/85-cross-compiled-toolchains.md` (one doc for the family, per the Phase 78 precedent) using the aligned learning-doc template in `docs/appendix/doc-templates.md`. **Owned by 85d** (the "+Release" sub-phase).
- Explain the build-once packaging substrate, install layout, memory/disk implications, the relocation contract, and how git, Python, and Clang fit the post-1.0 developer story.
- Link the learning doc from `docs/README.md` when 85d lands.

## Related Documentation and Version Updates

- The standalone roadmaps `docs/git-roadmap.md`, `docs/python-roadmap.md`, `docs/clang-llvm-roadmap.md` were **revived from `docs/archived/` for this phase** (2026-06-04) and re-headed to point at 85b/85c/85d; keep them aligned as the sub-phases land.
- Update `docs/README.md` and `docs/roadmap/README.md` (the umbrella + 85a–d rows).
- Update any image-layout or storage docs that describe `/usr`, `/usr/lib`, or bundled toolchains.
- Each sub-phase bumps `kernel/Cargo.toml` to its `0.85.x` version when it lands (85a `0.85.0` → 85d `0.85.3`).

## Acceptance Criteria

- 85a's package cache + `.m3pkg` + offline `pkg install` exist; a second image build of an unchanged tool performs **zero** compiler invocations (cache hit), and the existing ncurses-class ports are retrofitted onto the substrate.
- git supports the documented local repository workflows inside m3OS, installed from a `.m3pkg`.
- Python can run the documented REPL and script workloads inside m3OS, installed from a `.m3pkg`.
- Clang/LLD can build and run the documented sample programs inside m3OS, installed from a `.m3pkg` behind the opt-in image feature.
- The host-build, packaging, and image-install flow for the supported toolchains is reproducible and documented, including the static-repo hosting plan (with the network fetch path deferred to Phase 86).
- Disk, memory, and support-boundary changes introduced by the larger toolchains are documented.

## Companion Task List

- [Phase 85a Task List](./tasks/85a-package-infrastructure-tasks.md)
- [Phase 85b Task List](./tasks/85b-git-local-tasks.md)
- [Phase 85c Task List](./tasks/85c-python-tasks.md)
- [Phase 85d Task List](./tasks/85d-clang-llvm-tasks.md)

## How Real OS Implementations Differ

- **Redox OS** solves exactly this with `cookbook` recipes (`recipe.toml` separating fetch/build/package stages), the **`pkgar`** relocatable signed package format (BLAKE3 + ed25519), and **`REPO_BINARY=1`** which downloads prebuilt packages from `static.redox-os.org/pkg` instead of building from source — recently extended to cache downloaded packages across image rebuilds. 85a is a deliberately smaller analogue of this.
- **Yocto** keys each task on a signature of its inputs + dependency hashes and extracts a prebuilt sstate tarball on a hit; **Nix/Guix** make the store path itself the cache key; **Bazel** splits an input-addressed action cache from a content-addressed store. 85a borrows the input-hash key.
- **Buildroot/Alpine/Arch** all build into a DESTDIR/`pkgdir` staging tree under fakeroot and seal it into a relocatable tarball package — the universal "build once, extract anywhere" mechanism.
- Mature systems ship many more toolchains, package feeds, and update mechanisms, plus dependency solvers and signing infrastructure, than m3OS should attempt here.
- The important thing is to make a small number of powerful tools reliable, installable, and cheap to rebuild — not to pretend to offer a whole distribution overnight.

## Deferred Until Later

- **Networked** package install/update (`pkg install` over HTTPS from the hosted repo), DNS, and `/etc/pkg.d/` remote registration — Phase 86.
- Networked git operations and GitHub integration — Phase 86.
- Python package installation (`pip`) and networking-heavy modules (`ssl`, `socket` DNS, `asyncio`) — Phase 86.
- Self-hosting the larger toolchains inside m3OS (building LLVM on m3OS) — beyond Phase 85.
- Broader language/runtime stacks (Node.js, etc.) beyond the documented git/Python/Clang set — Phase 89+.
- Package signing-key management and a dependency solver — only a flat install model is in 85a scope.
