# Phase 94 — Rust-Cargo Ports & uutils Coreutils: Task List

**Status:** Planned
**Source Ref:** phase-94
**Depends on:** Phase 12 ✅, Phase 40 ✅, Phase 44 ✅, Phase 85a ✅
**Goal:** Deliver upstream [uutils/coreutils](https://github.com/uutils/coreutils) as the project's first Rust-cargo `x86_64-unknown-linux-musl` `.m3pkg`, installed by `pkg install coreutils` into `/usr/local/bin` where it shadows the hand-built `coreutils-rs` set by PATH precedence. Establish the reusable Rust-cargo musl port class along the way. Coexist with — do not replace — the ramdisk floor.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Rust-cargo musl toolchain + symlink round-trip de-risk | — | Planned |
| B | uutils port recipe (`Portfile` + `build_uutils`) | A | Planned |
| C | Packaging, bundling, and PATH-shadow integration | B | Planned |
| D | `coreutils-smoke` validation gate | C | Planned |
| E | Documentation, learning doc, kernel version bump + capability-bullet decision | A–D | Planned |

---

## Track A — Rust-cargo musl toolchain + de-risk

### A.1 — Add `x86_64-unknown-linux-musl` as a build target

**Files:**
- `rust-toolchain.toml`
- `xtask/src/port_build.rs`

**Symbol:** `build_uutils` (target wiring); `build_musl_rust_bins` (reused availability-probe precedent, `xtask/src/main.rs:3412`)
**Why it matters:** The workspace *default* build target is bare-metal `x86_64-unknown-none` (`.cargo/config.toml:4`), but `x86_64-unknown-linux-musl` is **not** absent from the repo: `build_musl_rust_bins`/`build_ion` (`xtask/src/main.rs:3412`, `:3533`, Phase 44) already cross-compile std Rust for it with prebuilt std + `-C target-feature=+crt-static`. What is new is routing that target through a **port** recipe. Unlike the kernel/userspace bare-metal path, the musl target has **prebuilt std**, so it needs `rustup target add x86_64-unknown-linux-musl` (or a `targets =` entry in `rust-toolchain.toml`, which currently has none) and **no** `-Zbuild-std`.

**Acceptance:**
- [ ] `rustup target list --installed` includes `x86_64-unknown-linux-musl`; `build_uutils` probes this (reusing the `build_musl_rust_bins` check at `xtask/src/main.rs:3435`) and aborts with an actionable `rustup target add x86_64-unknown-linux-musl` message if it is missing.
- [ ] A comment in `build_uutils` (and the design doc) records that the Rust musl target is self-contained (bundles musl + `rust-lld`), so a pure-Rust crate needs **no** external `x86_64-linux-musl-gcc` — citing `build_musl_rust_bins` as the existing precedent that de-risks this.

### A.2 — Prove a trivial std Rust musl binary boots on m3OS

**File:** `xtask/src/port_build.rs` (throwaway probe or a minimal staged crate)
**Symbol:** `build_musl_rust_bins` (reused as the bring-up probe harness, `xtask/src/main.rs:3412`)
**Why it matters:** Confirms the Linux-syscall compat layer runs a std Rust musl binary (not just musl C) before investing in the full uutils recipe — the single biggest feasibility risk. (Phase 44's `hello-rust`/`sysinfo-rust` demo crates already exercise this exact target, so the probe may reuse one of them rather than write a throwaway.)

**Acceptance:**
- [ ] A `fn main` std Rust crate cross-built for `x86_64-unknown-linux-musl` (release, static) runs on m3OS and prints a known sentinel (e.g. `UUTILS_PROBE_OK`) to stdout, asserted over serial via the compat layer.
- [ ] A version that spawns a `std::thread` and joins it prints a second sentinel (e.g. `UUTILS_THREAD_OK`) after the join, confirming `clone`/`futex`/TLS suffice for std threading (de-risks parallel `sort`).

### A.3 — Confirm symlink round-trip through `seal_package` → `pkg install`

**Files:**
- `pkg-format/src/lib.rs`
- `xtask/src/port_build.rs`

**Symbol:** `pkg_format::pack` / `pkg_format::unpack`; `seal_package`
**Why it matters:** uutils' multicall shape depends on ~100 applet **symlinks** surviving `.m3pkg` install. `pkg-format` already supports symlinks (`is_symlink`, `pack` via its private `collect` walker, the `clear -> tput` symlink case in `pack_unpack_round_trips_bytes_and_modes`). The git/dropbear ports left "won't round-trip" comments, but those are about *no content dedup* (a symlink to a multi-MB binary expands to a full copy — a **size** concern), not symlink **correctness**. This task pins the distinction with a test.

**Acceptance:**
- [ ] A staged relative symlink under `usr/local/bin/` round-trips through `pkg_format::pack` → `pkg_format::unpack` as a symlink (not a copy), asserted by a unit test (extending or mirroring `pack_unpack_round_trips_bytes_and_modes`, `pkg-format/src/lib.rs:714`).
- [ ] The dropbear source comment (`xtask/src/port_build.rs:3082`, "no symlink/hardlink dedup, so a symlink would not round-trip") is corrected/cross-referenced to state the real limitation is *no content dedup* (symlink semantics round-trip; only a symlink to a large binary costs a full copy), and the git comment (`port_build.rs:2780`) is confirmed to concern hardlink/inode dedup.

---

## Track B — uutils port recipe

### B.1 — `ports/util/coreutils/Portfile`

**File:** `ports/util/coreutils/Portfile`
**Symbol:** `parse_portfile` fields — `NAME`/`VERSION`/`DESCRIPTION`/`CATEGORY`/`DEPS`/`URL`/`SHA256`/`MAINTAINER` (the full set every existing Portfile carries; `port_build` hard-requires only `URL`+`SHA256`)
**Why it matters:** Registers uutils as a discoverable port with a pinned, hash-verified source and an empty dependency set (the curated feature set is pure Rust). The B.1 field list must match the existing convention — all 14 current Portfiles include `DESCRIPTION` + `MAINTAINER` (surfaced by `port list`), which an `NAME/VERSION/URL/SHA256/DEPS/CATEGORY`-only Portfile would omit.

**Acceptance:**
- [ ] Portfile carries the full field set `NAME`/`VERSION`/`DESCRIPTION`/`CATEGORY`/`DEPS`/`URL`/`SHA256`/`MAINTAINER`, matching `ports/lib/zlib/Portfile` / `ports/util/tmux/Portfile`.
- [ ] `VERSION` pins a specific recent uutils/coreutils release (target ≥ 0.1.0; exact value + tarball `SHA256` filled at implementation time against the pinned tarball, so the B.3 feature-set enumeration and `--locked` `Cargo.lock` are deterministic against a known release).
- [ ] `DEPS=` is empty; `CATEGORY=util`.
- [ ] Port is discoverable by the `ports/util/<name>/Portfile` scan and listed by `cargo xtask port list`.

### B.2 — `build_uutils()` cross-build + dispatch registration

**File:** `xtask/src/port_build.rs`
**Symbol:** `build_uutils`; the `fn port_build` `match name` arm
**Why it matters:** This is the first cargo/Rust `build_*` function — the reusable template for future Rust-cargo ports.

**Acceptance:**
- [ ] `cargo build --release --target x86_64-unknown-linux-musl --no-default-features --features "<curated set>" --locked` produces a single static `coreutils` ELF.
- [ ] `build_uutils` is reachable from `fn port_build` — either a new `match name` arm or, since a pure-Rust crate needs no musl-gcc, a `go`-style `if name == "coreutils"` early-return branch (`xtask/src/port_build.rs`); both are acceptable.
- [ ] `file` on the artifact reports `ELF 64-bit LSB executable, x86-64, ... statically linked`; the binary is run through `strip_stage` (called from `seal_package`, `port_build.rs:1069`) and the post-strip size is smaller than the unstripped `cargo` output (both sizes logged).

### B.3 — Curated applet feature set + applet symlinks

**File:** `xtask/src/port_build.rs`
**Symbol:** `build_uutils` (feature list + symlink staging)
**Why it matters:** The feature set must cover at least the 63 `[[bin]]` applets in `userspace/coreutils-rs/Cargo.toml` while excluding applets that need facilities m3OS lacks (e.g. SELinux `chcon`/`runcon`); each enabled applet needs a `usr/local/bin/<applet> -> coreutils` symlink.

**Acceptance:**
- [ ] The enabled applet set is **derived deterministically**: start from uutils' `feature_unix` umbrella feature, then reconcile to a superset of the **63 `[[bin]]` applets** in `userspace/coreutils-rs/Cargo.toml`, minus a **commented, enumerated** exclusion list — each excluded applet named with the missing facility (at minimum `chcon`/`runcon`).
- [ ] The exact resulting `--features` string is pinned in `build_uutils` (a comment maps it to the derivation rule above), so the build is reproducible against the B.1-pinned release.
- [ ] One relative symlink per enabled applet is staged under `usr/local/bin/`.
- [ ] No applet symlink is left dangling (every symlink target is the staged `coreutils`).

---

## Track C — Packaging, bundling, and integration

### C.1 — Seal `.m3pkg` and bundle into `/usr/pkg/`

**File:** `xtask/src/main.rs`
**Symbol:** `BUNDLE_ONLY_PORTS`
**Why it matters:** uutils ships bundled-on-demand like `git`/`python`, not pre-installed into the root.

**Acceptance:**
- [ ] `coreutils` is added to `BUNDLE_ONLY_PORTS`; a fresh image bundles `coreutils.m3pkg` (+ `.meta`) into `/usr/pkg/`.
- [ ] `pkg_format::verify` passes on the sealed artifact.

### C.2 — `pkg install coreutils` end-to-end on m3OS

**Files:**
- `userspace/pkg/src/` (installer — exercised, not necessarily changed)
- `xtask/src/port_build.rs`

**Symbol:** in-OS `pkg install` → `install_path` / `parent_components` (`userspace/pkg/src/main.rs:268`, which `mkdir`s each parent so `/usr/local/bin` is created on demand)
**Why it matters:** Proves the installer materializes the multicall binary + symlinks into `/usr/local/bin` from the bundled repo with no dependency chain.

**Acceptance:**
- [ ] `pkg install coreutils` succeeds on m3OS with `DEPS=` empty.
- [ ] `/usr/local/bin/coreutils` exists and `/usr/local/bin/ls` is a symlink to it (verified on-device).
- [ ] `ls --version` reports the uutils version string (the static Rust musl binary runs via the compat layer).

### C.3 — PATH shadowing + ramdisk-floor preservation

**Files:**
- `userspace/shell/src/main.rs` (PATH order — relied upon, unchanged)
- `kernel/src/fs/ramdisk.rs` (floor — unchanged)

**Symbol:** `PATH_DIRS` (`userspace/shell/src/main.rs:19`); `BIN_ENTRIES` (`kernel/src/fs/ramdisk.rs:476`)
**Why it matters:** Coexistence is structural: the ramdisk `no_std` set is the only coreutils before the data disk mounts, and the fallback after uninstall. **Note:** m3OS has no path-resolver builtin — `sh0` (`userspace/shell`) has only `cd`/`exit`; `/bin/sh` is ion (no verified `command -v`); and there is no `which`/`command`/`type` applet — so shadowing must be proven by *which implementation's output you get*, not by resolving a path.

**Acceptance:**
- [ ] With `coreutils` installed, `/usr/local/bin/ls` is a symlink to `coreutils` and `ls --version` prints the uutils banner — it shadows the ramdisk `/bin/ls` because `PATH_DIRS` puts `/usr/local/bin` first.
- [ ] After `pkg remove coreutils`, `ls` falls back to the ramdisk `/bin/ls` (hand-built output, no uutils version string) and the shell still reaches a working prompt.
- [ ] The OS boots to a login prompt with `coreutils` **not** installed (floor intact); `kernel/src/fs/ramdisk.rs` `BIN_ENTRIES` are unchanged.

---

## Track D — Validation

### D.1 — `coreutils-smoke` gate

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`

**Symbol:** `cmd_coreutils_smoke` (new); the `Some("coreutils-smoke")` CLI dispatch arm + `usage()` entry (`xtask/src/main.rs:1460`); `M3OS_COREUTILS_REGRESSION`
**Why it matters:** Asserts the install, the symlink round-trip, the runtime, and GNU-compatible behavior in one gate, mirroring `git-https-smoke`/`python-smoke`.

**Acceptance:**
- [ ] The gate is invokable: `cmd_coreutils_smoke` fn exists, a `Some("coreutils-smoke") => …` arm is added to the top-level CLI `match`, and `coreutils-smoke` is listed in the `usage()` string (`xtask/src/main.rs:1460`) — all three, mirroring `python-smoke`.
- [ ] Gate builds the `.m3pkg`, boots m3OS, `pkg install coreutils`, then runs a battery: `ls -la /`, `cp`/`mv`/`rm` a tree, `wc -l`, `cat`, `sort` on an input large enough to trigger any parallel path, and `env`.
- [ ] `sha256sum`'s 64-hex-digit **digest field** (first whitespace-delimited token) matches the existing `crypto-lib`-based `/bin/sha256sum` on the same input — compare the digest token, not the full line, since output framing (`<hash>  <name>` spacing, binary-mode `*`) may differ.
- [ ] The gate asserts `/usr/local/bin/ls` is a symlink (round-trip), and that `ls --version` shows the uutils version (the runtime + shadow proof, without relying on a `command -v` builtin m3OS lacks).
- [ ] Gate is opt-in via `M3OS_COREUTILS_REGRESSION=1` (a guarded block in `.githooks/pre-push`), **skips-with-reason** when the musl/cargo toolchain is absent, and runs at `--timeout 1800` (the install is one multicall binary + symlinks — far smaller than the 5400s `git-https`/`clang` gates).

---

## Track E — Documentation

### E.1 — Design doc + roadmap README row + status flip

**Files:**
- `docs/roadmap/94-rust-cargo-uutils.md`
- `docs/roadmap/README.md` (the Phase 94 summary row, ~L480)

**Symbol:** Phase 94 summary row (Phase / Theme / Primary Outcome / Status / Source Ref / Milestone / Tasks)
**Why it matters:** Roadmap traceability; the README row is required by the doc templates, and the Status cell must reflect reality across the phase's life.

**Acceptance:**
- [x] Design doc conforms to the phase-design template (all sections populated, including the `Learning Documentation Requirement` + `Related Documentation and Version Updates` sections).
- [x] Roadmap README row present with Theme, Primary Outcome, Status, Source Ref, Milestone, Tasks links.
- [ ] At landing, the roadmap README Phase 94 row Status (`docs/roadmap/README.md:480`) is flipped `Planned` → `Complete` (mirrors Phase 93 task F.4).

### E.2 — Capability-bullet decision in AGENTS.md

**File:** `AGENTS.md`
**Symbol:** Package-management capability bullet
**Why it matters:** The maintenance policy permits a capability-inventory edit only for a **new capability class**. The first Rust-cargo cross-compiled port arguably qualifies as a new build-system capability, but coreutils-as-such already exists.

**Acceptance:**
- [ ] A decision is recorded: either a one-line rewrite of the existing package-management bullet to mention the Rust-cargo musl port class, or no edit (with rationale), per the "keep it small" policy. (The kernel-version line edit is handled by E.4, not here.)

### E.3 — Create the Phase 94 learning doc + register it

**Files:**
- `docs/94-rust-cargo-uutils.md` (new)
- `docs/README.md` (the Phase-Aligned Learning Docs table, after the Phase 93 row at L78)
- `docs/appendix/codebase-map.md` (the Documentation Index table, after the Phase 92 row at L176)

**Symbol:** the *aligned legacy learning doc* template (`docs/appendix/doc-templates.md`, the `## Template: aligned legacy learning doc` section) — fields: Aligned Roadmap Phase (94) / Status / Source Ref (`phase-94`) / Supersedes Legacy Doc (N/A) / Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs From Later Work / Related Roadmap Docs / Deferred or Later-Phase Topics
**Why it matters:** The design doc's *Learning Documentation Requirement* mandates it (mirroring Phase 93 task F.1). It teaches the prebuilt-std `rustup target add` path vs the bare-metal `-Zbuild-std` path, why the self-contained Rust musl target needs no external `x86_64-linux-musl-gcc` (citing `build_musl_rust_bins` as precedent), the multicall-binary + applet-symlink packaging shape, and PATH-shadow coexistence — the pedagogical companion to the implementation-focused design doc.

**Acceptance:**
- [ ] `docs/94-rust-cargo-uutils.md` exists and follows the seven-section aligned-learning-doc template (Aligned Roadmap Phase / Status / Source Ref / Overview / Key Files / Related Roadmap Docs / Deferred topics).
- [ ] It is linked from the `docs/README.md` Phase-Aligned Learning Docs table in the verbatim row format `| [Rust-Cargo Ports & uutils Coreutils](./94-rust-cargo-uutils.md) | 94 | … Links the [94 design](./roadmap/94-rust-cargo-uutils.md) + [task](./roadmap/tasks/94-rust-cargo-uutils-tasks.md) docs |`, in phase order after the Phase 93 row.
- [ ] It is registered in the `docs/appendix/codebase-map.md` Documentation Index (a `| docs/94-rust-cargo-uutils.md | Before touching the uutils port build (build_uutils) or the Rust-cargo musl port class … |` row). *(Note: Phase 93's learning doc was never added to this index — a pre-existing gap worth fixing in the same PR.)*

### E.4 — Bump the kernel version (`0.93.0` → `0.94.0`)

**Files:**
- `kernel/Cargo.toml` (the `version` field, L3)
- `AGENTS.md` (the "kernel **v0.93.0**" reference in the Project Overview, L7)

**Symbol:** `version = "0.93.0"`
**Why it matters:** Every phase lands with an **unconditional** kernel version bump — the design doc's Implementation Outline (step 5) + *Related Documentation and Version Updates* call for it, and AGENTS.md's maintenance policy explicitly permits bumping the version line when a phase lands (mirrors Phase 93 task F.7). No kernel *code* change is expected — the banner (`kernel/src/lib.rs:77`), `/proc/version` (`kernel/src/fs/procfs.rs:748`), and `uname` utsname (`kernel/src/arch/x86_64/syscall/mod.rs:15343`) all derive from `env!("CARGO_PKG_VERSION")`, so the single `Cargo.toml` edit propagates everywhere.

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version` is `0.94.0`.
- [ ] The AGENTS.md "kernel **v0.94.0**" reference (L7) is updated to match.
- [ ] No other source edit is needed for the version string (the three derived sites pick it up from `CARGO_PKG_VERSION`); prior-phase `0.93.0` mentions in `docs/roadmap/` are historical and left unchanged.

---

## Documentation Notes

- This phase **coexists with**, and does not replace, the Phase 41 / `coreutils-rs` hand-built set; the ramdisk floor is preserved for early boot and uninstall fallback.
- The runtime is unchanged: uutils rides the existing Phase 12 Linux-syscall compat layer and static-ELF loader. No kernel *code* change is expected — but the phase still lands the standard **unconditional** per-phase **minor** version bump `0.93.0` → `0.94.0` (Track E.4); a *patch* bump on top applies only if a syscall gap surfaces during A.2/D.1.
- The new build path is prebuilt-std `rustup target add x86_64-unknown-linux-musl`, **not** the bare-metal `-Zbuild-std` path used by the kernel and `coreutils-rs`. The target itself is not new — `build_musl_rust_bins`/`build_ion` (`xtask/src/main.rs:3412`, `:3533`, Phase 44) already cross-compile std Rust for it; this phase is the first to route it through a **port** recipe.
- `command -v` is **not** available on m3OS (no `sh0` builtin, no verified ion builtin, no `which`/`command`/`type` applet) — PATH-shadow acceptance (C.3, D.1) proves shadowing by which implementation's output you get, never by a path-resolver.
- The hand-built applet count is **63** (`[[bin]]` entries in `userspace/coreutils-rs/Cargo.toml`; the 64th `src/*.rs` is the shared `common.rs` module, not an applet).
- `pkg-format`'s directory-walking pack function is `pack` (via the private `collect` helper) — there is **no** `pack_dir` symbol.
- Prefer exact symbols: `build_uutils`, `build_musl_rust_bins`, `BUNDLE_ONLY_PORTS` (`xtask/src/main.rs:26012`), `PATH_DIRS`, `BIN_ENTRIES`, `pkg_format::{pack,unpack,verify}`, `seal_package`, `strip_stage`.
