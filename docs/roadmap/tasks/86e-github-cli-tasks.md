# Phase 86e — GitHub CLI (`gh`) + Native Fallback: Task List

**Status:** In Progress
**Source Ref:** phase-86e
**Depends on:** Phase 86c (HTTPS/TLS + CA + PAT) — see [86c-https-git-transport-tasks.md](./86c-https-git-transport-tasks.md), Phase 86d (Go runtime) — see [86d-go-runtime-tasks.md](./86d-go-runtime-tasks.md), Phase 86a (entropy/clock/DNS/CA bundle, transitively), Phase 85 (Cross-Compiled Toolchains) ✅, Phase 77 (DNS reply delivery + outbound TCP `connect`) ✅
**Goal:** Ship `gh` (~40 MB Go) as a `.m3pkg` behind an `M3OS_WITH_GH` image feature, authenticate non-interactively via `GH_TOKEN`, register `gh` as a git credential helper (`gh auth setup-git`) so HTTPS `git` ops reuse the 86c curl + TLS + PAT machinery, validate authenticated read + write GitHub workflows over 86c HTTPS, document a native Rust GitHub-REST fallback, and bump the kernel to `0.86.4` — sitting strictly on top of 86c (transport) and 86d (runtime), with no new kernel surface.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the `92-vfs-bulk-io` style.)

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | `gh` Portfile + `build_gh` static cross-build + `M3OS_WITH_GH` image-feature bundling | 86d (Go toolchain), 85a | Planned |
| B | `GH_TOKEN` auth + `gh auth setup-git` credential helper + `gh-smoke` gate + native fallback | A, 86c | Planned |
| C | Docs + kernel version bump | A, B | Planned |

---

## Track A — `gh` build + bundling

### A.1 — Add the `gh` Portfile + `build_gh` static cross-build

**Files:**
- `ports/util/gh/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_gh`, registered in `PORTS` `xtask/src/main.rs:17446` and the `port_build` `match name` dispatch `xtask/src/port_build.rs:773` / `:873`)

**Symbol:** `build_gh`
**Why it matters:** `gh` is the headline GitHub-CLI artifact; building it statically via the standard port path keeps it idiomatic, and the build-from-source path is what consumes the 86d Go runtime toolchain.

**Acceptance:**
- [ ] `ports/util/gh/Portfile` pins a `gh` version + SHA-256 and records the build provenance: built-from-source (`DEPS` = the 86d Go toolchain port) vs prebuilt (no Go dep). Built-from-source requires Go `≥1.22` per `cli/cli`'s `go.mod`.
- [ ] `build_gh` cross-builds a **static** `gh` with `CGO_ENABLED=0` and `go build -trimpath -ldflags '-s -w -X internal/build.Version=<v>'`; the resulting binary is stripped (`-s -w`) and links no C library (no `musl_toolchain()` — `gh` is Go, the cross is driven by the Go toolchain, documented in the Portfile + `build_gh`, analogous to `build_git`'s plain-Makefile note).
- [ ] `cargo xtask port build gh` produces a `target/pkgcache/<key>.m3pkg`; a second build is a pkgcache hit (zero Go compiler invocations).

### A.2 — Gate the `gh` `.m3pkg` behind the `M3OS_WITH_GH` image feature

**File:** `xtask/src/main.rs`
**Symbol:** `M3OS_WITH_GH` image feature + the `BUNDLE_ONLY_PORTS` bundle-only repo path (`xtask/src/main.rs:17541`), mirroring the `M3OS_WITH_CLANG` gate at `xtask/src/main.rs:17575`
**Why it matters:** `gh` is ~40 MB; default images must omit it (exactly like the ~125 MB clang artifact behind `M3OS_WITH_CLANG`), and install + cold runs take tens of minutes over the ~200 KB/s VFS, so it must be strictly opt-in.

**Acceptance:**
- [ ] The sealed `gh.m3pkg` is bundled into the offline `/usr/pkg/` repo (with its `gh.meta` `VERSION=`/`DEPS=` sidecar) **only** when `M3OS_WITH_GH` is set; a default `cargo xtask image` (feature unset) produces an image that contains no `gh.m3pkg` (asserted by inspecting the data disk).
- [ ] When `M3OS_WITH_GH` is set, `pkg install gh` lays the binary into `/usr` and `gh --version` reports the pinned version inside m3OS; the dependency solver auto-installs the Go-toolchain dep first iff the Portfile declares it (built-from-source).

---

## Track B — Auth + workflows + native fallback

### B.1 — `GH_TOKEN` auth, `gh auth setup-git` credential helper, and the `gh-smoke` gate

**Files:**
- `xtask/src/main.rs` (a `gh-smoke` serial gate, `cmd_gh_smoke`, modeled on `cmd_git_local_smoke` `xtask/src/main.rs:13584` and `cmd_clang_smoke` `:14111`)
- `AGENTS.md` (opt-in gate row, `M3OS_GH_REGRESSION=1`)
- `.githooks/pre-push`
- on-disk: `~/.config/gh/hosts.yml`, `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`

**Symbol:** `cmd_gh_smoke`
**Why it matters:** `gh` registers as a git credential helper so push/clone reuse the 86c curl + TLS + PAT path — 86e sits *on top of* 86c — and the gate proves authenticated GitHub workflows actually complete inside m3OS, while asserting the token never leaks.

**Acceptance:**
- [ ] `cmd_gh_smoke` builds a fresh image with `M3OS_WITH_GH` set, boots m3OS, and `pkg install gh` from the bundled `/usr/pkg/` repo.
- [ ] It authenticates non-interactively via the `GH_TOKEN` environment variable (no prompt, no TTY), exports `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt` for Go `crypto/tls`, and runs `gh auth setup-git` registering `gh` as the `https://github.com` git credential helper.
- [ ] It performs a **read** workflow (`gh repo view` or `gh pr list`) and a **write** workflow (`gh pr create` or `gh issue create`) over 86c HTTPS — both return success (exit 0 + expected output substring).
- [ ] Secret hygiene is asserted: `~/.config/gh/hosts.yml` is mode `0600`; the `GH_TOKEN` value never appears in the captured serial log (grep returns no match) and is never written to `/tmp` in plaintext.
- [ ] The Go `crypto/tls` handshake to `api.github.com` confirms a ChaCha20-first cipher preference (e.g. `TLS_CHACHA20_POLY1305_SHA256`) under SIMD-off, not software AES.
- [ ] The gate is **skip-with-reason** (logs a SKIP, exits success) when `GH_TOKEN` is absent — a secret can never live in the repo/CI — mirroring the `tls-smoke`/`dns-smoke` SKIP-vs-PASS convention.
- [ ] Wired as `cargo xtask gh-smoke` and as an opt-in `M3OS_GH_REGRESSION=1` pre-push gate in both `AGENTS.md` and `.githooks/pre-push`, with a clang-gate-class `--timeout` (the install reads + SHA-verifies the whole ~40 MB `.m3pkg` and cold Go runs over the ~200 KB/s VFS take tens of minutes).

### B.2 — Document the native Rust GitHub-REST fallback

**File:** `docs/archived/github-cli-roadmap.md`
**Symbol:** the native-fallback section (raw HTTPS to `api.github.com`)
**Why it matters:** the native fallback covers the read subset if the Go path stalls under SIMD-off — same trust roots, a different client — so the read workflows stay reachable without `gh`.

**Acceptance:**
- [ ] `docs/archived/github-cli-roadmap.md` documents a native Rust GitHub-REST client: raw HTTPS `GET`/`POST` to `api.github.com` with an `Authorization: Bearer <PAT>` header over the 86c TLS path, covering the read subset (`repo view`, `pr list`).
- [ ] The doc records the Go-vs-mbedTLS double-TLS-stack fact (Go `crypto/tls` for `gh`, mbedTLS for `git`/`curl`) and that both consume the same 86a CA bundle (Go via `SSL_CERT_FILE`, curl via `--cacert`) and the same wall-clock.

---

## Track C — Docs + version

### C.1 — Bump kernel crate `0.86.3` → `0.86.4` + add the roadmap README 86e row

**Files:**
- `kernel/Cargo.toml` (line 3, currently `0.85.3`; in the 86 series it is `0.86.3` after 86d lands)
- `docs/roadmap/README.md`

**Symbol:** `kernel/Cargo.toml` `[package] version`; the `docs/roadmap/README.md` 86e row
**Why it matters:** each Phase 86 sub-phase bumps its own `0.86.x` version when it lands; the umbrella learning doc + final reconcile + the `0.86.5` aggregate are 86f's job, not 86e's.

**Acceptance:**
- [ ] `kernel/Cargo.toml` line 3 reads `version = "0.86.4"` (+ `Cargo.lock`); `cargo xtask check` is clean (clippy + rustfmt + all host tests + retpoline gate); the boot banner / `uname` report `0.86.4` (`env!("CARGO_PKG_VERSION")`).
- [ ] `docs/roadmap/README.md` has an 86e row (Phase / Theme / Primary Outcome / Status / Source Ref / Milestone / Tasks) linking [86e-github-cli.md](../86e-github-cli.md) and this task doc; a note in the row (or the umbrella's row) records that the umbrella learning doc `docs/86-networking-and-github.md` is created in **86f**, not here.

---

## Documentation Notes

- **What changed relative to the umbrella.** This sub-phase realizes the umbrella's "GitHub CLI (`gh`) + native fallback" scope ([86-networking-and-github.md](../86-networking-and-github.md)); it adds **no kernel surface** — it sits on top of 86c (transport) and 86d (runtime). The kernel bump is the only ring-0 change.
- **Two TLS stacks.** Make the double-TLS-stack fact explicit in the docs: `gh` carries pure-Go `crypto/tls` (it does **not** use mbedTLS), but still needs the 86a CA bundle (via `SSL_CERT_FILE`) and a trustworthy wall-clock. Confirm the ChaCha20-first preference under SIMD-off — software AES is slow until the 86f AES-NI capstone.
- **Secret discipline.** The gate can only run with a real `GH_TOKEN`, which can never live in the repo/CI — hence skip-with-reason when absent, mode `0600` on `~/.config/gh/hosts.yml`, no `/tmp` plaintext, and token redaction from serial. Prefer asserting these explicitly over "tokens are handled securely".
- **Opt-in heaviness.** `gh` mirrors `M3OS_WITH_CLANG`: an image-feature flag (`M3OS_WITH_GH`), a bundle-only `/usr/pkg/` repo entry (`BUNDLE_ONLY_PORTS` pattern, `xtask/src/main.rs:17541`), and a clang-gate-class `--timeout`. Prefer the exact symbols `build_gh`, `cmd_gh_smoke`, `M3OS_WITH_GH`, and `SSL_CERT_FILE` over generic descriptions.
- **Umbrella learning doc.** Do **not** create `docs/86-networking-and-github.md` in 86e — it is owned by 86f.
