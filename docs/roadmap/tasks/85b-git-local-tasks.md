# Phase 85b — git (Local): Task List

**Status:** Implemented (kernel 0.85.1)
**Source Ref:** phase-85b
**Depends on:** Phase 85a (Package & Build-Cache Infrastructure), Phase 45 (Ports System) ✅
**Goal:** Cross-build a musl `git` configured for local-only repository work (`NO_CURL NO_OPENSSL` + zlib), package it via the Phase 85a `.m3pkg` substrate, install it with `pkg install git`, and validate the local repo workflow inside m3OS — making git the first real toolchain to exercise the 85a pipeline end-to-end.

> **Landed.** All three tracks are Done and every acceptance item below is checked `[x]`. The task list was authored ahead of implementation (intentionally all-unchecked at the time); it now records the as-built result.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `git` Portfile + `build_git` cross-build | 85a | Done |
| B | Packaging + install via `.m3pkg` | A, 85a | Done |
| C | Local-workflow validation gate + version bump | B | Done |

> **Implementation note (landed):** validating git end-to-end surfaced a kernel
> **tmpfs path-routing bug** that `build_git`/packaging could not have caught:
> `resolve_fs_target` (the chmod/chown/symlink router) stripped the `/tmp` mount
> prefix to a bare `/foo`, so a file `open(O_CREAT)`'d under the tmpfs `tmp/…`
> subtree was invisible to a subsequent `chmod` (ENOENT). `git init` chmods the
> `.git/config.lock` it just created, so init aborted. Fixed by routing
> `/tmp` + `/run` through the same `tmpfs_relative_path` convention every other
> tmpfs syscall uses (`kernel/src/arch/x86_64/syscall/mod.rs`). This is the
> "a port surfaces a kernel bug" pattern (cf. tmux/PTY). Regression guards: a
> `kernel-core` host unit test for `mount_relative_path` (runs in `cargo xtask
> check`) plus the end-to-end `git-local-smoke` gate. `docs/git-roadmap.md`'s
> chmod requirement row claimed "Working (ext2)" while chmod on the `/tmp`
> tmpfs was in fact broken; this phase fixes that and updates the row to
> "Working (ext2 + tmpfs)".

---

## Track A — git cross-build

### A.1 — Add the git Portfile + `build_git`

**Files:**
- `ports/util/git/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_git`, registered in `PORTS` `~main.rs:16111` and the `port_build` `match name` dispatch `~port_build.rs:371`)

**Symbol:** `build_git`
**Why it matters:** git is the cheapest of the three toolchains and the first real test of the 85a substrate on a multi-binary tool; doing it via the standard port path keeps it idiomatic.

**Acceptance:**
- [x] `ports/util/git/Portfile` pins a git version + SHA-256 and declares `DEPS=zlib`. (git 2.44.0, SHA `e358738d…`, `DEPS=zlib`.)
- [x] `build_git` routes through `musl_toolchain()` + `musl_extra_ldflags_joined()` per the AGENTS.md port rules; it depends on the staged `target/port-stage/zlib`. (`--host=x86_64-linux-musl` is N/A — git's build is a plain Makefile, not autotools `./configure`, so the cross is driven entirely by `CC=x86_64-linux-musl-gcc`; documented in the Portfile + `build_git`.)

### A.2 — Local-only build configuration

**File:** `xtask/src/port_build.rs` (`build_git`)
**Symbol:** the git `make` invocation
**Why it matters:** the `NO_*` knobs are what keep git dependency-light and offline-only; getting them right is the whole build.

**Acceptance:**
- [x] git builds with `NO_CURL=1 NO_OPENSSL=1 NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 NO_ICONV=1 NO_EXPAT=1 NO_REGEX=NeedsStartEnd NEEDS_LIBICONV=`, statically linked against the staged zlib (`-L<zlib_stage>/lib` + `ZLIB_PATH`), `prefix=/usr`, `make DESTDIR=<stage> install`. (Also `SKIP_DASHED_BUILT_INS=YesPlease` — without it git installs ~140 dashed `git-<builtin>` hardlinks that the no-dedup `.m3pkg` packer would balloon into hundreds of MB.)
- [x] The `git` binary and `libexec/git-core/*` subcommands are stripped (`strip_stage` at seal); the build asserts `NO_CURL`/`NO_OPENSSL` — no `git-remote-https`/`git-http-fetch` helper is built **and** the binary contains neither the `curl_easy_perform` nor `SSL_CTX_new` symbol — so HTTPS cannot ride in unverified. Local-only out-of-scope tools are pruned post-install — `scalar`, `git-shell`, `git-imap-send`, `git-http-backend`, `git-daemon`, `git-sh-i18n--envsubst`, **and the three `bin/` server-side pack helpers** (`git-upload-pack`/`git-receive-pack`/`git-upload-archive`, which install as hardlinks to the 3.7 MB `git` binary and the no-dedup packer would expand to ~11 MB). Sealed `.m3pkg` is **~7.4 MB** (`bin/git` + the exec-path `libexec/git-core/git` copy + helpers + templates).

---

## Track B — Packaging + install

### B.1 — Seal git into a `.m3pkg` and install via `pkg`

**Files:**
- `xtask/src/port_build.rs` (85a seal step)
- `xtask/src/main.rs` (85a image-staging path)

**Symbol:** the 85a `seal_package` + `pkg install git`
**Why it matters:** git must flow through the 85a substrate (not a bespoke staging path) to validate that the substrate handles a real `libexec` + `share/git-core/templates` layout.

**Acceptance:**
- [x] `cargo xtask port build git` produces a `target/pkgcache/<key>.m3pkg`; a second build is a pkgcache hit (zero compiler invocations — verified).
- [x] The git `.m3pkg` is bundled on the data disk (bundle-only, via `populate_phase_69d_ports`'s `BUNDLE_ONLY_PORTS`) and `pkg install git` lays it into `/usr` (incl. `libexec/git-core` + templates); the install is relocatable (`prefix=/usr` tree laid under `/`). Validated end-to-end by `git-local-smoke` (git runs from `/usr/bin/git`). The dependency solver auto-installs the `zlib` dep first (`git.meta` `DEPS=zlib`).

---

## Track C — Validation + version

### C.1 — Local-workflow smoke gate

**Files:**
- `xtask/src/main.rs` (a `git-local-smoke` serial gate)
- `AGENTS.md` (opt-in gate row, `M3OS_GIT_REGRESSION=1`)

**Symbol:** `cmd_git_local_smoke`
**Why it matters:** proves git actually works inside m3OS, not just that it built.

**Acceptance:**
- [x] The gate establishes a commit identity via a bundled minimal `/etc/gitconfig` (`user.name`/`user.email` + `init.defaultBranch=main` + `core.pager=cat` + `safe.directory=*`), written into the data disk via `populate_ext2_files`. (Verified git resolves its system config to `/etc/gitconfig` for `prefix=/usr`.) The repo is created in tmpfs `/tmp/gitsmoke`; no extra `/usr/src` fixtures needed, so no `cargo xtask clean` step is required (the gate recreates the disk itself).
- [x] A scripted session inside m3OS passes (all 47 steps, 179 s): `git init`; add + commit a file; edit + `git diff` shows the change (`+world`); second commit; `git log --oneline` shows two commits (`commit-two`, `commit-one`); `git checkout -b feature` + add + commit; `git checkout main` + `git merge feature` (fast-forward) then both files tracked (`git ls-files`); `git status` reports "nothing to commit, working tree clean".
- [x] The gate is wired as `cargo xtask git-local-smoke` and as an opt-in pre-push regression (`M3OS_GIT_REGRESSION=1`) in both `AGENTS.md` and `.githooks/pre-push`.

### C.2 — Bump kernel crate `0.85.0` → `0.85.1`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.85.1"`
**Why it matters:** the 85b cut is the second Phase 85 sub-phase (mirrors 78b `0.78.1`).

**Acceptance:**
- [x] `kernel/Cargo.toml` reads `0.85.1` (+ `Cargo.lock`); `cargo xtask check` clean (clippy + rustfmt + all host tests + retpoline gate); boot banner / `uname` report `0.85.1` (`env!("CARGO_PKG_VERSION")` → kernel built as `v0.85.1`).

---

## Documentation Notes

- **What changed relative to the standalone roadmap.** `docs/git-roadmap.md` Stage 1 is exactly this sub-phase; its Stage 2 (HTTPS remotes) is Phase 86. Keep the revived roadmap aligned when 85b lands.
- **Honesty.** No HTTPS/curl/TLS and no SSH transport here — the cheapest secure remote clone (git shell-out to `ssh`) is a Phase 86 secure-transport-track item, not 85b.
- **Prefer exact targets.** Reference the git `NO_*` flags and the staged zlib path explicitly, not "the right configure flags".
