# Phase 85b - git (Local)

**Status:** Implemented (kernel 0.85.1)
**Source Ref:** phase-85b
**Depends on:** Phase 85a (Package & Build-Cache Infrastructure), Phase 36 (Expanded Memory) ✅, Phase 45 (Ports System) ✅
**Builds on:** Adds the first real toolchain on top of the Phase 85a packaging substrate — a host-cross-built musl `git` configured for local-only repository work, linking the existing `ports/lib/zlib`.
**Primary Components:** `ports/util/git/Portfile`, `xtask/src/port_build.rs` (`build_git`), `ports/lib/zlib`, the Phase 85a `.m3pkg` pipeline, `docs/git-roadmap.md`

## Milestone Goal

`git` runs inside m3OS for local repository workflows — `init`, `add`, `commit`, `log`, `diff`, `status`, `branch`, `merge`, `checkout` — installed from a Phase 85a `.m3pkg`. It is the smallest of the three toolchains and serves as the end-to-end proof that the 85a substrate works on a real, non-trivial tool.

## Why This Phase Exists

Local version control is foundational developer tooling and a prerequisite for the remote git / GitHub work in Phase 86. git's build is the cheapest of the three toolchains (one hard dependency, zlib) yet still exercises the full 85a path: cross-build → DESTDIR stage → `.m3pkg` seal → offline `pkg install`. Landing git first de-risks 85c/85d.

## Learning Goals

- How git's `NO_*` Makefile knobs carve a minimal, dependency-light build.
- How git lays out `libexec/git-core` (hardlinked subcommands) + `share/git-core/templates`, and how `RUNTIME_PREFIX` makes that relocatable.
- How a real multi-binary tool flows through the 85a packaging substrate.

## Feature Scope

### Area A — Local git build

A musl `git` built with `NO_CURL=1 NO_OPENSSL=1 NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 NO_ICONV=1 NO_EXPAT=1 NO_REGEX=NeedsStartEnd NEEDS_LIBICONV=`, statically linked against `ports/lib/zlib` (the one mandatory dependency; SHA-1/SHA-256 are git built-ins). Pinned version, `prefix=/usr`, `make DESTDIR=<stage> install`, stripped `git` + `git-core/*`.

### Area B — Packaging + validation

Seal the staged tree into a `.m3pkg` (85a), bundle it on the data disk, `pkg install git`, and validate a scripted local workflow inside m3OS.

## Important Components and How They Work

### `build_git` in `port_build.rs`

A new port `build_*` function following the AGENTS.md musl-toolchain rules (`musl_toolchain()`, `musl_extra_ldflags_joined()`, `--host=x86_64-linux-musl`), depending on the staged zlib at `target/port-stage/zlib`. Registered in `PORTS` and the `port_build` dispatch.

### git runtime layout

`git` resolves subcommands from `libexec/git-core` and templates from `share/git-core/templates`; `RUNTIME_PREFIX` (or a fixed `/usr` install) keeps these relative to the binary so the `.m3pkg` is relocatable.

## How This Builds on Earlier Phases

- Consumes the Phase 85a `.m3pkg` pipeline + offline installer.
- Reuses `ports/lib/zlib` from the Phase 45/69d ports tree.

## Implementation Outline

1. Add `ports/util/git/Portfile` (pinned version + SHA-256) and `build_git`.
2. Cross-build `NO_CURL NO_OPENSSL` against staged zlib; DESTDIR install + strip.
3. Seal `.m3pkg`, bundle on disk, `pkg install git`.
4. Validate the local workflow; bump kernel to `0.85.1`.

## Acceptance Criteria

- `git` builds reproducibly via `cargo xtask port build git` and seals into a `.m3pkg`.
- Inside m3OS: a scripted session — `git init`; add + commit a file; edit + `git diff` shows the change; second commit; `git log --oneline` shows two commits; `git checkout -b feature` + commit; `git checkout main` + `git merge feature`; `git status` reports a clean tree — passes (serial-validated gate).
- git is installed via `pkg install git` from a bundled `.m3pkg`, not a bespoke staging path.
- HTTPS/curl/TLS remain absent (deferred to Phase 86); the build asserts `NO_CURL`/`NO_OPENSSL`.

## Companion Task List

- [Phase 85b Task List](./tasks/85b-git-local-tasks.md)

## How Real OS Implementations Differ

- Distributions ship full git with curl/TLS, Perl/Python subcommands, gitk/git-gui, and credential helpers; 85b is the local-only core.
- git's SSH transport (shell-out to `ssh` for the cheapest secure remote clone) and HTTPS are Phase 86, not here.

## Deferred Until Later

- Remote workflows (`clone`/`fetch`/`push`/`pull`) over HTTPS or SSH — Phase 86.
- `git send-email`, `git gui`/`gitk`, gettext i18n, Perl/Python subcommands, credential storage.
