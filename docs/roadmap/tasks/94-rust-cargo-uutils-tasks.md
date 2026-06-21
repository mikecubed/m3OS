# Phase 94 — Rust-Cargo Ports & uutils Coreutils: Task List

**Status:** Complete
**Source Ref:** phase-94
**Depends on:** Phase 12 ✅, Phase 40 ✅, Phase 44 ✅, Phase 85a ✅
**Goal:** Deliver upstream [uutils/coreutils](https://github.com/uutils/coreutils) as the project's first Rust-cargo `x86_64-unknown-linux-musl` `.m3pkg`, installed by `pkg install coreutils` into `/usr/local/bin` where it shadows the hand-built `coreutils-rs` set by PATH precedence. Establish the reusable Rust-cargo musl port class along the way. Coexist with — do not replace — the ramdisk floor.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Rust-cargo musl toolchain + symlink round-trip de-risk | — | Complete |
| B | uutils port recipe (`Portfile` + `build_uutils`) | A | Complete |
| C | Packaging, bundling, and PATH-shadow integration | B | Complete (runtime arms validated by `coreutils-smoke`) |
| D | `coreutils-smoke` validation gate | C | Complete |
| E | Documentation, learning doc, kernel version bump + capability-bullet decision | A–D | Complete |

---

## Track A — Rust-cargo musl toolchain + de-risk

### A.1 — Add `x86_64-unknown-linux-musl` as a build target

**Files:**
- `rust-toolchain.toml`
- `xtask/src/port_build.rs`

**Symbol:** `build_uutils` (target wiring); `build_musl_rust_bins` (reused availability-probe precedent, `xtask/src/main.rs:3412`)
**Why it matters:** The workspace *default* build target is bare-metal `x86_64-unknown-none` (`.cargo/config.toml:4`), but `x86_64-unknown-linux-musl` is **not** absent from the repo: `build_musl_rust_bins`/`build_ion` (`xtask/src/main.rs:3412`, `:3533`, Phase 44) already cross-compile std Rust for it with prebuilt std + `-C target-feature=+crt-static`. What is new is routing that target through a **port** recipe. Unlike the kernel/userspace bare-metal path, the musl target has **prebuilt std**, so it needs `rustup target add x86_64-unknown-linux-musl` (or a `targets =` entry in `rust-toolchain.toml`, which currently has none) and **no** `-Zbuild-std`.

**Acceptance:**
- [x] `rustup target list --installed` includes `x86_64-unknown-linux-musl`; `build_uutils` probes this (reusing the `build_musl_rust_bins` check at `xtask/src/main.rs:3435`) and aborts with an actionable `rustup target add x86_64-unknown-linux-musl` message if it is missing. *(`rust-toolchain.toml` also pins `targets = ["x86_64-unknown-linux-musl"]`.)*
- [x] A comment in `build_uutils` (and the design doc) records that the Rust musl target is self-contained (bundles musl + `rust-lld`), so a pure-Rust crate needs **no** external `x86_64-linux-musl-gcc` — citing `build_musl_rust_bins` as the existing precedent that de-risks this.

### A.2 — Prove a trivial std Rust musl binary boots on m3OS

**File:** `xtask/src/port_build.rs` (throwaway probe or a minimal staged crate)
**Symbol:** `build_musl_rust_bins` (reused as the bring-up probe harness, `xtask/src/main.rs:3412`)
**Why it matters:** Confirms the Linux-syscall compat layer runs a std Rust musl binary (not just musl C) before investing in the full uutils recipe — the single biggest feasibility risk. (Phase 44's `hello-rust`/`sysinfo-rust` demo crates already exercise this exact target, so the probe may reuse one of them rather than write a throwaway.)

**Acceptance:**
- [x] A `fn main` std Rust crate cross-built for `x86_64-unknown-linux-musl` (release, static) runs on m3OS and prints a known sentinel to stdout, asserted over serial via the compat layer. *Realized by the real implementation rather than a throwaway crate (the design doc explicitly permits reusing existing demos): Phase 44's `hello-rust` std-Rust-musl binary is already embedded + proven on m3OS, and the `coreutils-smoke` gate asserts the actual uutils `coreutils --version` → `coreutils 0.9.0 (multi-call binary)` over serial — a std-Rust-musl ET_EXEC binary booting through the Linux-compat layer + static-ELF loader. A standalone `UUTILS_PROBE_OK` crate would add permanent ramdisk cruft for strictly less confidence than running the real binary.*
- [x] The std threading runtime (`clone`/`futex`/TLS) is exercised: the static uutils binary embeds std's `pthread`/`futex`/PT_TLS path (the same path Phase 40/77 + Python's threads already prove on m3OS), and the `coreutils-smoke` gate runs `sort` on a 50000-line input — uutils' rayon-parallel sort path — which de-risks parallel `sort`. (The binary's std runtime startup, incl. TLS setup, is validated by every applet invocation; the multicall binary running at all proves the std runtime initializes on m3OS.)

### A.3 — Confirm symlink round-trip through `seal_package` → `pkg install`

**Files:**
- `pkg-format/src/lib.rs`
- `xtask/src/port_build.rs`

**Symbol:** `pkg_format::pack` / `pkg_format::unpack`; `seal_package`
**Why it matters:** uutils' multicall shape depends on ~100 applet **symlinks** surviving `.m3pkg` install. `pkg-format` already supports symlinks (`is_symlink`, `pack` via its private `collect` walker, the `clear -> tput` symlink case in `pack_unpack_round_trips_bytes_and_modes`). The git/dropbear ports left "won't round-trip" comments, but those are about *no content dedup* (a symlink to a multi-MB binary expands to a full copy — a **size** concern), not symlink **correctness**. This task pins the distinction with a test.

**Acceptance:**
- [x] A staged relative symlink under `usr/local/bin/` round-trips through `pkg_format::pack` → `pkg_format::unpack` as a symlink (not a copy), asserted by a unit test (`usr_local_bin_applet_symlink_round_trips` in `pkg-format/src/lib.rs`, mirroring `pack_unpack_round_trips_bytes_and_modes`; passes under `cargo xtask check`).
- [x] The dropbear source comment is corrected to state the real limitation is *no content dedup* (symlink semantics round-trip; only a symlink to a large binary costs a full copy, and the copy-vs-symlink choice is deliberate), and the git comment is annotated to confirm it concerns hardlink/inode dedup specifically (symlinks round-trip fine) — both cross-reference the Phase 94 applet-symlink shape.

---

## Track B — uutils port recipe

### B.1 — `ports/util/coreutils/Portfile`

**File:** `ports/util/coreutils/Portfile`
**Symbol:** `parse_portfile` fields — `NAME`/`VERSION`/`DESCRIPTION`/`CATEGORY`/`DEPS`/`URL`/`SHA256`/`MAINTAINER` (the full set every existing Portfile carries; `port_build` hard-requires only `URL`+`SHA256`)
**Why it matters:** Registers uutils as a discoverable port with a pinned, hash-verified source and an empty dependency set (the curated feature set is pure Rust). The B.1 field list must match the existing convention — all 14 current Portfiles include `DESCRIPTION` + `MAINTAINER` (surfaced by `port list`), which an `NAME/VERSION/URL/SHA256/DEPS/CATEGORY`-only Portfile would omit.

**Acceptance:**
- [x] Portfile carries the full field set `NAME`/`VERSION`/`DESCRIPTION`/`CATEGORY`/`DEPS`/`URL`/`SHA256`/`MAINTAINER`, matching `ports/lib/zlib/Portfile` / `ports/util/tmux/Portfile`.
- [x] `VERSION` pins uutils/coreutils **0.9.0** (≥ 0.1.0); tarball `SHA256=dafe0126…acab2` (the GitHub source tarball; re-download verified deterministic; carries `Cargo.lock` for `--locked`).
- [x] `DEPS=` is empty; `CATEGORY=util`.
- [x] Port is discoverable by the `ports/util/<name>/Portfile` scan and listed by `cargo xtask port list` (added to `BUILDABLE_PORTS` → RECIPE=yes).

### B.2 — `build_uutils()` cross-build + dispatch registration

**File:** `xtask/src/port_build.rs`
**Symbol:** `build_uutils`; the `fn port_build` `match name` arm
**Why it matters:** This is the first cargo/Rust `build_*` function — the reusable template for future Rust-cargo ports.

**Acceptance:**
- [x] `cargo build --release --target x86_64-unknown-linux-musl --no-default-features --features feat_os_unix_musl --locked` produces a single static `coreutils` ELF (`build_uutils`).
- [x] `build_uutils` is reachable from `fn port_build` via a `go`-style `if name == "coreutils"` early-return branch (before the `musl_toolchain()` requirement — a pure-Rust crate needs no musl-gcc).
- [x] `file` on the artifact reports `ELF 64-bit LSB executable, x86-64, ... statically linked` (ET_EXEC, via `-C relocation-model=static` matching `build_musl_rust_bins`); `strip_stage` shrank it (14,187,232 B pre-strip → smaller stripped, both logged).

### B.3 — Curated applet feature set + applet symlinks

**File:** `xtask/src/port_build.rs`
**Symbol:** `build_uutils` (feature list + symlink staging)
**Why it matters:** The feature set must cover at least the 63 `[[bin]]` applets in `userspace/coreutils-rs/Cargo.toml` while excluding applets that need facilities m3OS lacks (e.g. SELinux `chcon`/`runcon`); each enabled applet needs a `usr/local/bin/<applet> -> coreutils` symlink.

**Acceptance:**
- [x] The enabled applet set is **derived deterministically**: uutils' musl unix umbrella `feat_os_unix_musl` (= `feat_Tier1` + `feat_require_unix_musl` + `feat_require_unix_hostid` + `feat_require_unix_utmpx`). *(0.9.0 has no `feature_unix`; `feat_os_unix_musl` is the musl-appropriate equivalent.)* It **excludes by construction** the applets m3OS lacks facilities for: SELinux `chcon`/`runcon` (only in `feat_require_selinux`) and `stdbuf` (`feat_require_unix_musl` drops it — needs an external `libstdbuf.so`, impossible in a static binary). Verified absent in the stage; covers every uutils-provided applet among the 63 hand-built `[[bin]]`s.
- [x] The exact `--features feat_os_unix_musl` string is pinned in `build_uutils` (with a comment mapping it to the derivation) and in `build_recipe_id("coreutils")`, reproducible against the 0.9.0 pin.
- [x] One relative symlink per enabled applet is staged under `usr/local/bin/` (106 symlinks, derived from the binary's own `coreutils --list` so they can never drift from the feature set).
- [x] No applet symlink is left dangling (every symlink target is the staged `coreutils`; verified: 1 regular file + 106 symlinks → `coreutils`).

---

## Track C — Packaging, bundling, and integration

### C.1 — Seal `.m3pkg` and bundle into `/usr/pkg/`

**File:** `xtask/src/main.rs`
**Symbol:** `BUNDLE_ONLY_PORTS`
**Why it matters:** uutils ships bundled-on-demand like `git`/`python`, not pre-installed into the root.

**Acceptance:**
- [x] `coreutils` is added to `BUNDLE_ONLY_PORTS`; a fresh image bundles `coreutils.m3pkg` (+ `.meta`) into `/usr/pkg/` (the `coreutils-smoke` image build does so, and `pkg install coreutils` finds it).
- [x] `pkg_format::verify` passes on the sealed artifact (the seal succeeded; the `BUNDLE_ONLY_PORTS` bundling loop in `xtask/src/main.rs` re-runs `pkg_format::verify` on the read-back artifact bytes — `Ok(bytes) if pkg_format::verify(&bytes)` — before pushing `usr/pkg/coreutils.m3pkg`, with the failed-verify arm skipping the bundle).

### C.2 — `pkg install coreutils` end-to-end on m3OS

**Files:**
- `userspace/pkg/src/` (installer — exercised, not necessarily changed)
- `xtask/src/port_build.rs`

**Symbol:** in-OS `pkg install` → `install_path` / `parent_components` (`userspace/pkg/src/main.rs:268`, which `mkdir`s each parent so `/usr/local/bin` is created on demand)
**Why it matters:** Proves the installer materializes the multicall binary + symlinks into `/usr/local/bin` from the bundled repo with no dependency chain.

**Acceptance:**
- [x] `pkg install coreutils` succeeds on m3OS with `DEPS=` empty (no "resolving … + dependencies" line — confirming the empty dep set). *(`coreutils-smoke` step "coreutils installed from .m3pkg".)*
- [x] `/usr/local/bin/coreutils` exists and `/usr/local/bin/ls` is a symlink to it (`ls -l` → `-> coreutils`, verified on-device).
- [x] `ls --version` reports the uutils version string (`coreutils 0.9.0 (multi-call binary)` / `ls (uutils coreutils) 0.9.0` — the static Rust musl ET_EXEC binary runs via the compat layer).

### C.3 — PATH shadowing + ramdisk-floor preservation

**Files:**
- `userspace/shell/src/main.rs` (PATH order — relied upon, unchanged)
- `kernel/src/fs/ramdisk.rs` (floor — unchanged)

**Symbol:** `PATH_DIRS` (`userspace/shell/src/main.rs:19`); `BIN_ENTRIES` (`kernel/src/fs/ramdisk.rs:476`)
**Why it matters:** Coexistence is structural: the ramdisk `no_std` set is the only coreutils before the data disk mounts, and the fallback after uninstall. **Note:** m3OS has no path-resolver builtin — `sh0` (`userspace/shell`) has only `cd`/`exit`; `/bin/sh` is ion (no verified `command -v`); and there is no `which`/`command`/`type` applet — so shadowing must be proven by *which implementation's output you get*, not by resolving a path.

**Acceptance:**
- [x] With `coreutils` installed, a BARE `ls --version` prints the uutils banner — it shadows the ramdisk `/bin/ls` because the login PATH (`userspace/login/src/main.rs`) puts `/usr/local/bin` first.
- [x] After `pkg remove coreutils` (removes the binary + all 106 symlinks), the binary is gone (`/bin/cat /usr/local/bin/coreutils` → `cat: cannot open file`, the ramdisk-floor wording) and a bare `ls /` still lists the root (now the ramdisk `/bin/ls`) with the shell still reaching a working prompt (`COREUTILS_SMOKE_DONE`).
- [x] The OS boots to a login prompt with `coreutils` **not** installed (the gate boots + logs in before installing — floor intact); `kernel/src/fs/ramdisk.rs` `BIN_ENTRIES` are unchanged (no edit).

---

## Track D — Validation

### D.1 — `coreutils-smoke` gate

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`

**Symbol:** `cmd_coreutils_smoke` (new); the `Some("coreutils-smoke")` CLI dispatch arm + `usage()` entry (`xtask/src/main.rs:1460`); `M3OS_COREUTILS_REGRESSION`
**Why it matters:** Asserts the install, the symlink round-trip, the runtime, and GNU-compatible behavior in one gate, mirroring `git-https-smoke`/`python-smoke`.

**Acceptance:**
- [x] The gate is invokable: `cmd_coreutils_smoke` fn exists, a `Some("coreutils-smoke") => …` arm is added to the top-level CLI `match`, and `coreutils-smoke` is listed in the `usage()` string — all three, mirroring `python-smoke`.
- [x] Gate builds the `.m3pkg`, boots m3OS, `pkg install coreutils`, then runs a battery: `ls -la /`, `cp`/`mv`/`rm` a tree (recursive), `wc -l`, `cat`, `sort` on a 50000-line input (parallel path), and `env`. *(Validated end-to-end by the `coreutils-smoke` PASS — see PR.)*
- [x] `sha256sum`'s 64-hex-digit **digest** matches the existing `crypto-lib`-based `/bin/sha256sum` on the same input — both wait for the same precomputed constant `5b21…e6bd` (each `Send` resets the serial buffer, so the second wait genuinely re-checks the ramdisk output).
- [x] The gate asserts `/usr/local/bin/ls` is a symlink (`-> coreutils`, round-trip), and that a bare `ls --version` shows the uutils version (runtime + PATH-shadow proof, no `command -v` needed).
- [x] **Inode-identity battery (Phase 88 `st_ino` rigor on the uutils path):** on the ext2/`vfs_server` root (`/root`, not tmpfs `/tmp`), `ln` creates a hardlink and `stat -c %h` reports `nlink=2` on both names then `nlink=1` after `rm`-ing the original (whose content survives via the link — shared, refcounted inode through `vfs_service_link`); `stat -c %i … | sort -u | wc -l` reports `1` for the two hardlinked names (shared inode) and `2` for two distinct files (distinct **non-zero** inodes — the regression guard against the 85d `st_ino=0` collapse `fill_stat` fixed). All assertions are against known constants (no capture-compare). *(Closes the design doc's `stat`/inode-identity deferral, unblocked once Phase 88 (PR #240) landed before this phase.)*
- [x] Gate is opt-in via `M3OS_COREUTILS_REGRESSION=1` (a guarded block in `.githooks/pre-push`), **skips-with-reason** when the musl Rust target is absent, and runs at `--timeout 1800` (one multicall binary + symlinks — far smaller than the 5400s `git-https`/`clang` gates).

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
- [x] At landing, the roadmap README Phase 94 row Status (`docs/roadmap/README.md:480`) is flipped `Planned` → `Complete` (the row also records the `unlinkat` patch bump).

### E.2 — Capability-bullet decision in AGENTS.md

**File:** `AGENTS.md`
**Symbol:** Package-management capability bullet
**Why it matters:** The maintenance policy permits a capability-inventory edit only for a **new capability class**. The first Rust-cargo cross-compiled port arguably qualifies as a new build-system capability, but coreutils-as-such already exists.

**Acceptance:**
- [x] Decision recorded: a **one-sentence addition** to the existing `**Package management**:` bullet naming the first Rust-cargo cross-compiled port (uutils/coreutils 0.9.0, prebuilt-std musl, `pkg install coreutils` → `/usr/local/bin`, PATH-shadow). No new bullet, no reflow — minimal per the "keep it small" policy (a genuinely new build-system capability class warrants the one line).

### E.3 — Create the Phase 94 learning doc + register it

**Files:**
- `docs/94-rust-cargo-uutils.md` (new)
- `docs/README.md` (the Phase-Aligned Learning Docs table, after the Phase 93 row at L78)
- `docs/appendix/codebase-map.md` (the Documentation Index table, after the Phase 92 row at L176)

**Symbol:** the *aligned legacy learning doc* template (`docs/appendix/doc-templates.md`, the `## Template: aligned legacy learning doc` section) — fields: Aligned Roadmap Phase (94) / Status / Source Ref (`phase-94`) / Supersedes Legacy Doc (N/A) / Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs From Later Work / Related Roadmap Docs / Deferred or Later-Phase Topics
**Why it matters:** The design doc's *Learning Documentation Requirement* mandates it (mirroring Phase 93 task F.1). It teaches the prebuilt-std `rustup target add` path vs the bare-metal `-Zbuild-std` path, why the self-contained Rust musl target needs no external `x86_64-linux-musl-gcc` (citing `build_musl_rust_bins` as precedent), the multicall-binary + applet-symlink packaging shape, and PATH-shadow coexistence — the pedagogical companion to the implementation-focused design doc.

**Acceptance:**
- [x] `docs/94-rust-cargo-uutils.md` exists and follows the seven-section aligned-learning-doc template (Overview / What This Doc Covers / Core Implementation / Key Files / How This Phase Differs From Later Work / Related Roadmap Docs / Deferred or Later-Phase Topics; header carries Aligned Roadmap Phase 94 / Status / Source Ref `phase-94`). Includes the `unlinkat` syscall-gap note.
- [x] It is linked from the `docs/README.md` Phase-Aligned Learning Docs table in the verbatim row format, in phase order after the Phase 93 row.
- [x] It is registered in the `docs/appendix/codebase-map.md` Documentation Index; the pre-existing Phase 93 learning-doc gap (`docs/93-dynamic-c-runtime.md`) is fixed in the same PR.

### E.4 — Bump the kernel version (`0.93.0` → `0.94.0`)

**Files:**
- `kernel/Cargo.toml` (the `version` field, L3)
- `AGENTS.md` (the "kernel **v0.93.0**" reference in the Project Overview, L7)

**Symbol:** `version = "0.93.0"`
**Why it matters:** Every phase lands with an **unconditional** kernel version bump — the design doc's Implementation Outline (step 5) + *Related Documentation and Version Updates* call for it, and AGENTS.md's maintenance policy explicitly permits bumping the version line when a phase lands (mirrors Phase 93 task F.7). No kernel *code* change is expected — the banner (`kernel/src/lib.rs:77`), `/proc/version` (`kernel/src/fs/procfs.rs:748`), and `uname` utsname (`kernel/src/arch/x86_64/syscall/mod.rs:15343`) all derive from `env!("CARGO_PKG_VERSION")`, so the single `Cargo.toml` edit propagates everywhere.

**Acceptance:**
- [x] `kernel/Cargo.toml` `version` is **`0.94.1`** (the standard minor bump `0.93.0`→`0.94.0` plus the **patch** bump `0.94.0`→`0.94.1` for the fd-relative `*at` syscall gaps surfaced by D.1 — `unlinkat`(263), `fchmodat`(268)/`fchmodat2`(452), `fchownat`(260), `mkdirat`(258) — the design doc's anticipated patch-bump-on-syscall-gap case).
- [x] The AGENTS.md "kernel **v0.94.1**" reference (L7) is updated to match.
- [x] No other source edit is needed for the version string (the three derived sites pick it up from `CARGO_PKG_VERSION`); prior-phase `0.93.0` mentions in `docs/roadmap/` are historical and left unchanged.

---

## Documentation Notes

- This phase **coexists with**, and does not replace, the Phase 41 / `coreutils-rs` hand-built set; the ramdisk floor is preserved for early boot and uninstall fallback.
- The runtime is largely unchanged: uutils rides the existing Phase 12 Linux-syscall compat layer and static-ELF loader. The phase lands the standard **unconditional** per-phase **minor** version bump `0.93.0` → `0.94.0` (Track E.4). **A family of fd-relative `*at` syscall gaps DID surface in D.1** (the anticipated patch-bump case): uutils' `uucore::safe_traversal` (unconditionally compiled for Linux musl — it is gated on `cfg(unix, not(redox))`, not a Cargo feature) performs recursive metadata ops by `(dirfd, name)`. musl on x86_64 keeps the legacy syscalls for the *non*-`at` wrappers (`chmod`=90/`chown`=92/`mkdir`=83/`unlink`=87), so plain `chmod`/`chown`/`mkdir -p`/`cp -p`/`cp -r` already worked, but the `*at` forms are bare `syscall(SYS_…at)` with **no legacy fallback**, and m3OS implemented `openat`/`newfstatat` with real dirfds but **not** the metadata `*at` set. The gaps and fixes: `rm -r` → **`unlinkat`(263)** (`sys_linux_unlinkat`, routed by `AT_REMOVEDIR`); `chmod -R` (NoFollow walk) → **`fchmodat2`(452)** then **`fchmodat`(268)** (`sys_linux_fchmodat`; 452 handled directly because the musl 268 NoFollow fallback emulates `AT_SYMLINK_NOFOLLOW` via `O_PATH`+`/proc/self/fd`, which m3OS lacks); `chown -R` → **`fchownat`(260)** (`sys_linux_fchownat`); `install -D` → **`mkdirat`(258)** (`sys_linux_mkdirat`). Each is a thin dirfd-aware wrapper over the existing `unlink`/`rmdir`/`chmod`/`chown`/`mkdir` core, reusing `resolve_path_from_dirfd` (the `AT_FDCWD` path stays byte-equivalent). These syscalls are the **only** kernel change; together they trigger a single **patch** bump on top: `0.94.0` → **`0.94.1`**. `coreutils-smoke` now exercises `rm -r`, `chmod -R`, `chown -R`, and `install -D` with read-back assertions so a silent ENOSYS regression fails the gate.
- The new build path is prebuilt-std `rustup target add x86_64-unknown-linux-musl`, **not** the bare-metal `-Zbuild-std` path used by the kernel and `coreutils-rs`. The target itself is not new — `build_musl_rust_bins`/`build_ion` (`xtask/src/main.rs:3412`, `:3533`, Phase 44) already cross-compile std Rust for it; this phase is the first to route it through a **port** recipe.
- `command -v` is **not** available on m3OS (no `sh0` builtin, no verified ion builtin, no `which`/`command`/`type` applet) — PATH-shadow acceptance (C.3, D.1) proves shadowing by which implementation's output you get, never by a path-resolver.
- The hand-built applet count is **63** (`[[bin]]` entries in `userspace/coreutils-rs/Cargo.toml`; the 64th `src/*.rs` is the shared `common.rs` module, not an applet).
- `pkg-format`'s directory-walking pack function is `pack` (via the private `collect` helper) — there is **no** `pack_dir` symbol.
- Prefer exact symbols: `build_uutils`, `build_musl_rust_bins`, `BUNDLE_ONLY_PORTS` (`xtask/src/main.rs:26012`), `PATH_DIRS`, `BIN_ENTRIES`, `pkg_format::{pack,unpack,verify}`, `seal_package`, `strip_stage`.
