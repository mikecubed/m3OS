# Phase 85b — git (Local): Task List

**Status:** Planned (authored ahead of implementation)
**Source Ref:** phase-85b
**Depends on:** Phase 85a (Package & Build-Cache Infrastructure), Phase 45 (Ports System) ✅
**Goal:** Cross-build a musl `git` configured for local-only repository work (`NO_CURL NO_OPENSSL` + zlib), package it via the Phase 85a `.m3pkg` substrate, install it with `pkg install git`, and validate the local repo workflow inside m3OS — making git the first real toolchain to exercise the 85a pipeline end-to-end.

> **Planning task list authored ahead of implementation.** All acceptance items are intentionally **unchecked `[ ]`**. Builds on the 85a substrate; do not start before 85a lands.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `git` Portfile + `build_git` cross-build | 85a | Planned |
| B | Packaging + install via `.m3pkg` | A, 85a | Planned |
| C | Local-workflow validation gate + version bump | B | Planned |

---

## Track A — git cross-build

### A.1 — Add the git Portfile + `build_git`

**Files:**
- `ports/util/git/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_git`, registered in `PORTS` `~main.rs:16111` and the `port_build` `match name` dispatch `~port_build.rs:371`)

**Symbol:** `build_git`
**Why it matters:** git is the cheapest of the three toolchains and the first real test of the 85a substrate on a multi-binary tool; doing it via the standard port path keeps it idiomatic.

**Acceptance:**
- [ ] `ports/util/git/Portfile` pins a git version + SHA-256 and declares `DEPS=zlib`.
- [ ] `build_git` routes through `musl_toolchain()` + `musl_extra_ldflags_joined()` + `--host=x86_64-linux-musl` per the AGENTS.md port rules; it depends on the staged `target/port-stage/zlib`.

### A.2 — Local-only build configuration

**File:** `xtask/src/port_build.rs` (`build_git`)
**Symbol:** the git `make` invocation
**Why it matters:** the `NO_*` knobs are what keep git dependency-light and offline-only; getting them right is the whole build.

**Acceptance:**
- [ ] git builds with `NO_CURL=1 NO_OPENSSL=1 NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 NO_ICONV=1 NO_EXPAT=1 NO_REGEX=NeedsStartEnd NEEDS_LIBICONV=`, statically linked against the staged zlib (`-L<zlib_stage>/lib`), `prefix=/usr`, `make DESTDIR=<stage> install`. (`NO_EXPAT` drops the otherwise-pulled-in expat dependency; `NO_REGEX=NeedsStartEnd` uses git's bundled regex — both per `docs/git-roadmap.md`.)
- [ ] The `git` binary and `libexec/git-core/*` subcommands are stripped; the build asserts `NO_CURL`/`NO_OPENSSL` (no libcurl/OpenSSL linkage) so HTTPS cannot ride in unverified.

---

## Track B — Packaging + install

### B.1 — Seal git into a `.m3pkg` and install via `pkg`

**Files:**
- `xtask/src/port_build.rs` (85a seal step)
- `xtask/src/main.rs` (85a image-staging path)

**Symbol:** the 85a `seal_package` + `pkg install git`
**Why it matters:** git must flow through the 85a substrate (not a bespoke staging path) to validate that the substrate handles a real `libexec` + `share/git-core/templates` layout.

**Acceptance:**
- [ ] `cargo xtask port build git` produces a `target/pkgcache/<key>.m3pkg`; a second build is a pkgcache hit (zero compiler invocations).
- [ ] The git `.m3pkg` is bundled on the data disk and `pkg install git` lays it into `/usr` (incl. `libexec/git-core` + templates); the install is relocatable (resolves subcommands/templates relative to `/usr`).

---

## Track C — Validation + version

### C.1 — Local-workflow smoke gate

**Files:**
- `xtask/src/main.rs` (a `git-local-smoke` serial gate)
- `AGENTS.md` (opt-in gate row, `M3OS_GIT_REGRESSION=1`)

**Symbol:** `cmd_git_local_smoke`
**Why it matters:** proves git actually works inside m3OS, not just that it built.

**Acceptance:**
- [ ] The gate first establishes a commit identity (a bundled minimal `/etc/gitconfig` with `user.name`/`user.email`, or `GIT_AUTHOR_*`/`GIT_COMMITTER_*` env), since fresh git aborts a commit with no identity — and any `/usr/src` fixtures the script uses are written into the data disk via `populate_ext2_files`, with `cargo xtask clean` run to recreate the disk.
- [ ] A scripted session inside m3OS passes: `git init`; add + commit a file; edit + `git diff` shows the change; second commit; `git log --oneline` shows two commits; `git checkout -b feature` + add + commit; `git checkout main` + `git merge feature` then both files present; `git status` reports "nothing to commit, working tree clean".
- [ ] The gate is wired as an opt-in pre-push regression (`M3OS_GIT_REGRESSION=1`) in `AGENTS.md`.

### C.2 — Bump kernel crate `0.85.0` → `0.85.1`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.85.1"`
**Why it matters:** the 85b cut is the second Phase 85 sub-phase (mirrors 78b `0.78.1`).

**Acceptance:**
- [ ] `kernel/Cargo.toml` reads `0.85.1` (+ `Cargo.lock`); `cargo xtask check` clean; boot banner / `uname` report `0.85.1`.

---

## Documentation Notes

- **What changed relative to the standalone roadmap.** `docs/git-roadmap.md` Stage 1 is exactly this sub-phase; its Stage 2 (HTTPS remotes) is Phase 86. Keep the revived roadmap aligned when 85b lands.
- **Honesty.** No HTTPS/curl/TLS and no SSH transport here — the cheapest secure remote clone (git shell-out to `ssh`) is a Phase 86 secure-transport-track item, not 85b.
- **Prefer exact targets.** Reference the git `NO_*` flags and the staged zlib path explicitly, not "the right configure flags".
