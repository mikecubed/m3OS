# Phase 85a — Package & Build-Cache Infrastructure: Task List

**Status:** Planned (authored ahead of implementation)
**Source Ref:** phase-85a
**Depends on:** Phase 45 (Ports System) ✅
**Goal:** Generalize the existing Phase 45 ports caching into a portable, content-addressed prebuilt-package store with a relocatable `.m3pkg` format and an offline in-OS `pkg` installer, then retrofit the existing ncurses-class ports onto it — so the large toolchains in 85b/85c/85d are built once and installed as artifacts, never rebuilt from source on a routine image build.

> **This is a planning task list authored ahead of implementation (post-1.0).** All implementation acceptance items are intentionally **unchecked `[ ]`** — they are the implementation contract for a future Phase 85a PR, not work already done. The umbrella-level documentation reconciliation (roadmap revival, README restructure, design-doc authoring) landed in the task-authoring PR.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `.m3pkg` format + content-addressed cache key (host-tested pure logic) | Phase 45 caching | ✅ Done |
| B | `xtask` seal-after-install + resolve-before-build + zero-rebuild gate in `port_build.rs`/`main.rs` | A | In progress |
| C | Userspace `pkg` installer + installed-file DB | A | In progress |
| D | Image staging installs from `.m3pkg`; existing-ports retrofit | B, C | Planned |
| E | Disk/RAM budget + relocation contract + hosting plan + data-disk resize + version bump | A–D | Planned |

---

## Track A — `.m3pkg` format + content key

### A.1 — Define the content-addressed package key

**File:** `xtask/src/port_build.rs`
**Symbol:** new `package_key(port_dir, toolchain_id, dep_keys) -> String`, generalizing `port_fingerprint` (`port_build.rs:156-182`)
**Why it matters:** the key is what lets an unchanged tool skip its (multi-GB, for 85d) build. It must be portable across machines, unlike the current `.stamp` which folds in `port_build.rs`'s own bytes.

**Acceptance:**
- [x] `package_key` hashes `{source tarball SHA-256 from the Portfile, the resolved musl toolchain identity, the build flags, and the sorted dependency artifact keys}` into a stable hex string; two invocations with identical inputs produce identical keys (host test). *(`pkg_format::compute_package_key` + `port_build::package_key`/`toolchain_id`; tests `key_is_stable_for_identical_inputs`, `key_dep_order_does_not_matter`.)*
- [x] Changing any input (tarball SHA, toolchain, a flag, a dep key) changes the key; documented which inputs are in vs out of the key (e.g. `target` dir path is excluded so the cache survives a moved tree). *(test `changing_any_input_changes_the_key`; IN/OUT contract documented on `compute_package_key`. `port_build.rs` source bytes are deliberately OUT — the old `.stamp` over-invalidated by folding them in.)*

### A.2 — `.m3pkg` pack/unpack + verify (host-tested)

**Files:**
- `xtask/src/port_build.rs` (or a new `xtask/src/m3pkg.rs`)
- a host-testable module (mirrors the `kernel_core::storage` host-test pattern)

**Symbol:** `m3pkg::{pack(stage_dir) -> bytes, unpack(bytes, dest), verify(bytes) -> bool}`
**Why it matters:** the artifact format is the unit the cache stores and the installer consumes; getting pack/unpack/verify right in host tests prevents silent corruption of a 1 GB Clang artifact.

**Acceptance:**
- [x] `.m3pkg` v1 = a header (format version + per-entry path/mode/SHA-256 content hash + entry index) plus a data blob; documented byte layout. *(Custom binary header in `pkg-format/src/lib.rs`. **Recorded choice:** SHA-256 (in-crate pure-`u32` impl) instead of BLAKE3, and a custom header instead of `.tar.zst` — `blake3`/`zstd` are unavailable offline and `sha2` does not codegen on the soft-float bare-metal target; the A.2 fallback clause permits this.)*
- [x] `pack` then `unpack` round-trips a staged tree byte-for-byte including file modes (host test); `verify` detects a flipped byte (host test). *(tests `pack_unpack_round_trips_bytes_and_modes` [files + symlink + modes], `pack_is_deterministic`, `verify_detects_a_flipped_content_byte`, `verify_rejects_bad_magic_and_truncation`.)*
- [x] Optional ed25519 signature field is reserved in the header even if unsigned in v1 (forward-compat for the Phase 86 networked repo). *(64-byte zeroed `signature` + `sig_present` byte; test `header_reserves_zeroed_signature_field`.)*

---

## Track B — `xtask` packaging integration

### B.1 — Seal the DESTDIR stage into a cached `.m3pkg`

**File:** `xtask/src/port_build.rs` (after the per-port `build_*` DESTDIR install, near the `.stamp` write at `port_build.rs:386`)
**Symbol:** new `seal_package(name, stage, key)` writing `target/pkgcache/<key>.m3pkg`
**Why it matters:** this is the "build once" half — every successful build produces a reusable artifact keyed by A.1.

**Acceptance:**
- [ ] After a port builds, its `target/port-stage/<name>/` tree is packed into `target/pkgcache/<key>.m3pkg`; the key is recorded alongside the existing `.stamp`.
- [ ] The pkgcache survives across `cargo xtask clean` of the disk image (it is build output, not disk state) — documented which `clean` targets do/don't purge it.

### B.2 — Resolve-before-build: install from cache on a key hit

**File:** `xtask/src/port_build.rs` (the early-return point at `port_build.rs:343-349`, today's `.stamp` check)
**Symbol:** extend the early-return to consult `target/pkgcache/<key>.m3pkg`
**Why it matters:** this is the "never rebuild" half — a key hit short-circuits configure/make/install entirely, exactly as the `.stamp` check does today but keyed portably.

**Acceptance:**
- [ ] When `target/pkgcache/<key>.m3pkg` exists for the computed key, the build is skipped and the stage is materialized from the artifact; a log line states "pkgcache hit (key …), zero compiler invocations".
- [ ] On a key miss the port builds normally and B.1 seals the result; the existing `.stamp` fast-path is preserved as the same-machine inner loop.

### B.3 — Zero-rebuild assertion gate

**File:** `xtask/src/main.rs`
**Symbol:** new `cmd_pkgcache_hit_check` (a `cargo xtask` sub-step)
**Why it matters:** "a second build does zero compiler invocations" is the headline acceptance of the whole umbrella phase; it must be a mechanically-checked gate, not a hand-waved log read, so a cache regression fails the build.

**Acceptance:**
- [ ] `cmd_pkgcache_hit_check` runs a build of a target port twice (warm cache) and **fails** if the second run logs any compiler/`make`/`cmake`/`ninja` invocation; it passes only on a pure pkgcache hit.
- [ ] The gate is the artifact referenced by the B.2 / D.1 / 85d B.1 "zero compiler invocations" acceptance items, and is wired as an opt-in regression row in `AGENTS.md`.

---

## Track C — Userspace `pkg` installer

### C.1 — `pkg` binary (offline local-repo install)

**Files:**
- `userspace/pkg/` (new crate — four-place wiring per AGENTS.md step 2: workspace member, the `bins` array in `build_userspace_bins()` (`xtask/src/main.rs`, ~line 956), `kernel/src/fs/ramdisk.rs` `include_bytes!`+`BIN_ENTRIES`, and a config if it becomes a daemon)
- `userspace/pkg/src/main.rs`

**Symbol:** `pkg` with `install <name>`, `list`, `verify`
**Why it matters:** this is the in-OS apt/pacman analogue; offline install from a bundled local repo is the part that fits inside the pre-networking Phase 85 boundary.

**Acceptance:**
- [ ] `pkg install <name>` reads `/usr/pkg/<name>.m3pkg` (the local on-disk repo), verifies hashes (A.2), and extracts entries under `/usr`, recording `/var/lib/pkg/db`.
- [ ] `pkg list` prints installed packages from the DB; `pkg verify <name>` re-checks installed files against the DB hashes.
- [ ] `pkg` performs **no** network access (a grep/SBOM check confirms no socket syscalls); the networked path is explicitly a Phase 86 task.
- [ ] `pkg` is wired with `needs_alloc = true` in the `bins` array, defines `syscall_lib::heap::BrkAllocator` as its `#[global_allocator]`, and enables the `alloc` feature on `syscall-lib` (it uses `Vec`/`String` to parse the package DB) — per AGENTS.md step 2.

### C.2 — Installed-file database

**File:** `userspace/pkg/src/` (db module)
**Symbol:** the `/var/lib/pkg/db` reader/writer
**Why it matters:** a record of what each package owns is the minimum needed for `list`/`verify` and for a future remove/upgrade.

**Acceptance:**
- [ ] The DB records, per package: name, version, content key, and the list of installed paths + hashes; format is documented and forward-compatible.
- [ ] Re-installing the same package is idempotent (no duplicate DB entries).

---

## Track D — Image staging + ports retrofit

### D.1 — Image staging installs from `.m3pkg`

**File:** `xtask/src/main.rs` (`populate_phase_69d_ports` ~`main.rs:16110`, and the `cmd_image` call site)
**Symbol:** a staging path that writes `.m3pkg` artifacts into the data disk (the local repo `/usr/pkg/`) via the existing `debugfs` write loop
**Why it matters:** the image must bundle the `.m3pkg` artifacts (so `pkg install` works offline) instead of mirroring raw stage trees.

**Acceptance:**
- [ ] `cargo xtask image` writes each selected port's `.m3pkg` into `/usr/pkg/` on the data disk (and optionally pre-installs core ports into `/usr`), via `debugfs`, replacing the raw stage-tree mirror.
- [ ] The image build performs **zero** compiler invocations when every selected port is a pkgcache hit (verified by the B.3 gate).
- [ ] `cargo xtask clean` is run to force ext2 recreation after the new `/usr/pkg/` staging is wired (AGENTS.md data-disk rule); documented in the task.

### D.2 — Retrofit existing ports onto the substrate

**Files:** `ports/lib/{zlib,ncurses,libevent}`, `ports/util/{less,htop,tmux}`, `xtask/src/port_build.rs`
**Symbol:** the existing `build_ncurses`/`build_less`/`build_htop`/`build_tmux`/`build_libevent` functions
**Why it matters:** proves the substrate on real ports and gives the existing TUI gates the cache win for free.

**Acceptance:**
- [ ] ncurses, libevent, zlib, less, htop, tmux each produce a `.m3pkg` and install via the D.1 path with **no** regression in the existing TUI gates (`tui-app-smoke`, `htop-render-probe`).
- [ ] `less` (which is also embedded in the ramdisk per `ramdisk.rs:150`) remains available early; the retrofit does not break its ramdisk presence.

---

## Track E — Budget, contract, hosting, version

### E.1 — Disk/RAM budget + DESTDIR/relocation contract

**File:** `docs/roadmap/85a-package-infrastructure.md` + `docs/85-cross-compiled-toolchains.md` (umbrella learning doc, created in 85d)
**Symbol:** the documented budget + contract
**Why it matters:** the large tools materially change resource assumptions; the relocation contract is what makes Clang/Python packages installable rather than build-prefix-locked.

**Acceptance:**
- [ ] The `DISK_SIZE` (`main.rs:14077`, currently 1 GB) headroom is assessed against the 85b/85c/85d footprints (git tens of MB, Python tens of MB, Clang several hundred MB — verify on a real build) and an explicit budget is recorded (incl. whether the data disk must grow for the opt-in Clang artifact — see E.4).
- [ ] The DESTDIR + relocation contract is documented: build at `prefix=/usr`, `make install DESTDIR=<stage>`; **strip executables and `.so`s before sealing**; Clang resource dir relative to the binary; Python `bin/`+`lib/pythonX.Y/` fixed relative layout.
- [ ] The **host-build** RAM/link-memory requirement (Clang links with many GB of RAM) is recorded with any CI-runner implication, distinct from the on-image disk budget.

### E.2 — Hosting/distribution plan (network fetch deferred to Phase 86)

**File:** `docs/roadmap/85a-package-infrastructure.md`
**Symbol:** the recorded hosting recommendation
**Why it matters:** the ~1 GB Clang artifact wants a remote store so CI builds it once; the decision belongs here even though the fetch path is Phase 86.

**Acceptance:**
- [ ] The static-repo layout + recommended host (GitHub Releases on `m3os-pkgs`, or gh-pages) is documented, mirroring the Redox `REPO_BINARY` → `static.redox-os.org/pkg` model.
- [ ] The networked `pkg install`/`update` over HTTPS + `/etc/pkg.d/` is explicitly listed as a Phase 86 handoff (cross-referenced from `docs/roadmap/86-networking-and-github.md`).
- [ ] It is noted that the hash-only `.m3pkg` verification (A.2) is acceptable for offline/local install, but the **networked** install in Phase 86 will require the reserved ed25519 signature field to be populated (trust over an untrusted transport).

### E.3 — Bump kernel crate `0.84.0` → `0.85.0`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.85.0"`
**Why it matters:** the `0.NN.0 = Phase NN` convention; 85a is the first Phase 85 sub-phase to land, so it opens the `0.85.x` line (mirrors 78a `0.78.0`).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` reads `0.85.0` (+ `Cargo.lock`); `cargo xtask check` is clean and the boot banner / `uname` (`env!("CARGO_PKG_VERSION")`) report `0.85.0`.
- [ ] No reference bumps the kernel crate to `1.0.0` (the Phase 83 phase-tracked posture is unchanged).

### E.4 — Grow the data disk for the opt-in Clang artifact (if needed)

**File:** `xtask/src/main.rs` (`DISK_SIZE` ~14077, plus the raw-image / QEMU `-drive` sizing)
**Symbol:** `DISK_SIZE` and the matching image-size constants
**Why it matters:** the E.1 assessment is likely to show the existing 1 GB data disk cannot hold the opt-in Clang artifact alongside git/Python/the existing ports; assessing is not enough — the resize must actually be performed (gated so default images stay small).

**Acceptance:**
- [ ] If E.1 shows insufficient headroom, `DISK_SIZE` (and the raw-image/QEMU sizing) is increased, gated/documented so the default (no-Clang) image is unchanged; an opt-in Clang image builds **and boots** with the artifact present and `pkg install clang` succeeding.
- [ ] If E.1 shows the 1 GB disk is sufficient, that finding is recorded explicitly (no silent assumption).

---

## Documentation Notes

- **What changed relative to Phase 45.** 85a generalizes the per-port `.stamp` fingerprint (`port_build.rs:156-182`) and the `target/port-src/` SHA-256 tarball cache into a portable content-addressed `.m3pkg` store, and adds the first in-OS installer — it does not replace the musl toolchain plumbing (`musl_toolchain`/`find_musl_cc`/`musl_extra_ldflags_joined`), which is reused unchanged.
- **Prefer exact targets.** Reference exact files (`xtask/src/port_build.rs:343-349` resolve point, `:386` seal point, `xtask/src/main.rs:16110` staging, `:14077` `DISK_SIZE`).
- **Pure logic is host-tested.** The content key (A.1) and pack/unpack/verify (A.2) are host-tested in `xtask` exactly as `kernel_core::storage` is, so a format slip is a failing `cargo xtask check`. The AGENTS.md `cargo xtask check` host-test enumeration (the build-command list) must be updated to name the new `m3pkg`/content-key tests.
- **Honesty.** `pkg` is offline-only in 85a; the networked path is Phase 86 — the docs must not imply network install works pre-86.
