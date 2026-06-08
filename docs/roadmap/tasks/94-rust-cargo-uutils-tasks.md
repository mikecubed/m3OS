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
| E | Documentation + capability-bullet decision | A–D | Planned |

---

## Track A — Rust-cargo musl toolchain + de-risk

### A.1 — Add `x86_64-unknown-linux-musl` as a build target

**Files:**
- `rust-toolchain.toml`
- `xtask/src/port_build.rs`

**Symbol:** `build_uutils` (target wiring); toolchain `targets` list
**Why it matters:** The repo currently builds Rust only for the bare-metal `x86_64-unknown-none` target (`.cargo/config.toml`); the musl std target is absent. Unlike the kernel/userspace bare-metal path, the musl target has **prebuilt std**, so it needs `rustup target add x86_64-unknown-linux-musl` (or a `targets =` entry in `rust-toolchain.toml`) and **no** `-Zbuild-std`.

**Acceptance:**
- [ ] The pinned toolchain has `x86_64-unknown-linux-musl` installed (documented prerequisite + verified by the build).
- [ ] A documented note records that the musl target is self-contained (bundles musl + `rust-lld`), so a pure-Rust crate needs no external `x86_64-linux-musl-gcc`.

### A.2 — Prove a trivial std Rust musl binary boots on m3OS

**File:** `xtask/src/port_build.rs` (throwaway probe or a minimal staged crate)
**Symbol:** N/A (bring-up probe)
**Why it matters:** Confirms the Linux-syscall compat layer runs a std Rust musl binary (not just musl C) before investing in the full uutils recipe — the single biggest feasibility risk.

**Acceptance:**
- [ ] A `fn main` std Rust crate cross-built for `x86_64-unknown-linux-musl` (release, static) runs on m3OS and prints to stdout via the compat layer.
- [ ] A version that spawns a thread (`std::thread`) and joins it runs successfully, confirming `clone`/`futex`/TLS suffice for std threading (de-risks parallel `sort`).

### A.3 — Confirm symlink round-trip through `seal_package` → `pkg install`

**Files:**
- `pkg-format/src/lib.rs`
- `xtask/src/port_build.rs`

**Symbol:** `pkg_format::pack` / `pkg_format::unpack`; `seal_package`
**Why it matters:** uutils' multicall shape depends on ~100 applet **symlinks** surviving `.m3pkg` install. `pkg-format` already supports symlinks (`is_symlink`, `pack_dir`, the `clear -> tput` test), but the git/dropbear ports left "won't round-trip" comments — those concern *hardlink/inode dedup*, not symlinks. This task pins the distinction with a test.

**Acceptance:**
- [ ] A staged relative symlink under `usr/local/bin/` round-trips through `pkg_format::pack` → `pkg_format::unpack` as a symlink (not a copy), asserted by a unit test.
- [ ] The git/dropbear source comments are clarified (or cross-referenced) to state the limitation is hardlink/inode dedup, not symlinks.

---

## Track B — uutils port recipe

### B.1 — `ports/util/coreutils/Portfile`

**File:** `ports/util/coreutils/Portfile`
**Symbol:** Portfile fields (`NAME`/`VERSION`/`URL`/`SHA256`/`DEPS`/`CATEGORY`)
**Why it matters:** Registers uutils as a discoverable port with a pinned, hash-verified source and an empty dependency set (the curated feature set is pure Rust).

**Acceptance:**
- [ ] Portfile pins an exact uutils release `VERSION` and its tarball `SHA256`.
- [ ] `DEPS=` is empty; `CATEGORY=util`.
- [ ] Port is discoverable by the `ports/util/<name>/Portfile` scan.

### B.2 — `build_uutils()` cross-build + dispatch registration

**File:** `xtask/src/port_build.rs`
**Symbol:** `build_uutils`; the `fn port_build` `match name` arm
**Why it matters:** This is the first cargo/Rust `build_*` function — the reusable template for future Rust-cargo ports.

**Acceptance:**
- [ ] `cargo build --release --target x86_64-unknown-linux-musl --no-default-features --features "<curated set>" --locked` produces a single static `coreutils` ELF.
- [ ] The arm is wired into `fn port_build`'s `match name`.
- [ ] The produced binary's `file`/header confirms static `x86_64` ELF; `strip_stage` shrinks it before seal.

### B.3 — Curated applet feature set + applet symlinks

**File:** `xtask/src/port_build.rs`
**Symbol:** `build_uutils` (feature list + symlink staging)
**Why it matters:** The feature set must cover at least the 63 hand-built applets while excluding applets that need facilities m3OS lacks (e.g. SELinux `chcon`/`runcon`); each enabled applet needs a `usr/local/bin/<applet> -> coreutils` symlink.

**Acceptance:**
- [ ] The enabled applet set is a documented superset of the `coreutils-rs` 63 applets, minus an explicit, commented exclusion list (SELinux, anything depending on unsupported syscalls).
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

**Symbol:** in-OS `pkg install`
**Why it matters:** Proves the installer materializes the multicall binary + symlinks into `/usr/local/bin` from the bundled repo with no dependency chain.

**Acceptance:**
- [ ] `pkg install coreutils` succeeds on m3OS with `DEPS=` empty.
- [ ] `/usr/local/bin/coreutils` exists and `/usr/local/bin/ls` is a symlink to it (verified on-device).
- [ ] `ls --version` reports the uutils version string (the static Rust musl binary runs via the compat layer).

### C.3 — PATH shadowing + ramdisk-floor preservation

**Files:**
- `userspace/shell/src/main.rs` (PATH order — relied upon, unchanged)
- `kernel/src/fs/ramdisk.rs` (floor — unchanged)

**Symbol:** `PATH_DIRS`; `BIN_ENTRIES`
**Why it matters:** Coexistence is structural: the ramdisk `no_std` set is the only coreutils before the data disk mounts, and the fallback after uninstall.

**Acceptance:**
- [ ] With `coreutils` installed, `command -v ls` resolves to `/usr/local/bin/ls`.
- [ ] After `pkg remove coreutils`, `command -v ls` resolves to `/bin/ls` and the shell still functions.
- [ ] The OS boots to a login prompt with `coreutils` **not** installed (floor intact); `kernel/src/fs/ramdisk.rs` `BIN_ENTRIES` are unchanged.

---

## Track D — Validation

### D.1 — `coreutils-smoke` gate

**Files:**
- `xtask/src/main.rs`
- `.githooks/pre-push`

**Symbol:** `cmd_coreutils_smoke` (new); `M3OS_COREUTILS_REGRESSION`
**Why it matters:** Asserts the install, the symlink round-trip, the runtime, and GNU-compatible behavior in one gate, mirroring `git-https-smoke`/`python-smoke`.

**Acceptance:**
- [ ] Gate builds the `.m3pkg`, boots m3OS, `pkg install coreutils`, then runs a battery: `ls -la /`, `cp`/`mv`/`rm` a tree, `wc -l`, `cat`, `sort` on an input large enough to trigger any parallel path, `env`, and `sha256sum`.
- [ ] `sha256sum` output is byte-identical to the existing `crypto-lib`-based `/bin/sha256sum` on the same input.
- [ ] The gate asserts `/usr/local/bin/ls` is a symlink (round-trip), and that `ls --version` shows the uutils version.
- [ ] Gate is opt-in via `M3OS_COREUTILS_REGRESSION=1`, **skips-with-reason** when the musl/cargo toolchain is absent, and runs at a timeout matched to the install size (smaller than the `git-https`/`clang` gates).

---

## Track E — Documentation

### E.1 — Design doc + README row

**Files:**
- `docs/roadmap/94-rust-cargo-uutils.md`
- `docs/roadmap/README.md`

**Symbol:** Phase 94 row (Post-1.0 Platform Growth table)
**Why it matters:** Roadmap traceability; the README row is required by the doc templates.

**Acceptance:**
- [x] Design doc conforms to the phase-design template (all sections populated).
- [x] README Post-1.0 row added with Theme, Primary Outcome, Status, Source Ref, Milestone, Tasks links.

### E.2 — Capability-bullet decision in CLAUDE.md / AGENTS.md

**File:** `AGENTS.md`
**Symbol:** Package-management capability bullet
**Why it matters:** The maintenance policy permits a capability-inventory edit only for a **new capability class**. The first Rust-cargo cross-compiled port arguably qualifies as a new build-system capability, but coreutils-as-such already exists.

**Acceptance:**
- [ ] A decision is recorded: either a one-line rewrite of the existing package-management bullet to mention the Rust-cargo port class, or no edit (with rationale), per the "keep it small" policy.
- [ ] If the kernel version is bumped (only if a syscall gap surfaces during bring-up), the version line in `AGENTS.md` is updated; otherwise it is left unchanged (this is expected to be a userspace + build-tooling phase).

---

## Documentation Notes

- This phase **coexists with**, and does not replace, the Phase 41 / `coreutils-rs` hand-built set; the ramdisk floor is preserved for early boot and uninstall fallback.
- The runtime is unchanged: uutils rides the existing Phase 12 Linux-syscall compat layer and static-ELF loader. No kernel change is expected; a kernel patch bump applies only if a syscall gap surfaces during A.2/D.1.
- The new build path is prebuilt-std `rustup target add x86_64-unknown-linux-musl`, **not** the bare-metal `-Zbuild-std` path used by the kernel and `coreutils-rs`.
- Prefer exact symbols: `build_uutils`, `BUNDLE_ONLY_PORTS`, `PATH_DIRS`, `BIN_ENTRIES`, `pkg_format::{pack,unpack}`, `seal_package`.
