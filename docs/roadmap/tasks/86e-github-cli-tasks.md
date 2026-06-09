# Phase 86e — GitHub CLI (`gh`) + Native Fallback: Task List

**Status:** 🟡 Implemented — build + bundling + `gh-smoke` core (gh runs on m3OS) + docs + version all validated; the `GH_TOKEN`-gated authenticated read/write arms are implemented but skip-with-reason without a secret (verified only by a maintainer running `GH_TOKEN=<pat> cargo xtask gh-smoke`).
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
- `xtask/src/port_build.rs` (new `build_gh` + the `if name == "gh"` early-dispatch branch in `port_build`, alongside the `go` branch — `gh` is resolved by `find_port_dir` from its Portfile, so no `PORTS` allowlist entry is needed; `build_gh_port()` is the public entry the gate calls)

**Symbol:** `build_gh`
**Why it matters:** `gh` is the headline GitHub-CLI artifact; building it statically via the standard port path keeps it idiomatic, and the build-from-source path is what consumes the 86d Go runtime toolchain.

**Acceptance:**
- [x] `ports/util/gh/Portfile` pins a `gh` version + SHA-256 and records the build provenance: built-from-source (`DEPS` = the 86d Go toolchain port) vs prebuilt (no Go dep). Built-from-source requires Go `≥1.22` per `cli/cli`'s `go.mod`. **As-built:** gh **2.82.1** (source SHA-256 `999bdea5…`), provenance **built-from-source** recorded in the Portfile; `DEPS=` is empty (the Go toolchain is a host build input, not an m3OS pkg dep). gh's `go.mod` now requires **`go 1.24.0`** (the doc's "≥1.22" is stale relative to current gh), so `build_gh` reuses the 86d `go` port's pinned **Go 1.24.6** toolchain — v2.82.1 is the newest gh that builds with it (v2.83.0 jumped to `go 1.25`).
- [x] `build_gh` cross-builds a **static** `gh` with `CGO_ENABLED=0` and `go build -trimpath -ldflags '-s -w -X internal/build.Version=<v>'`; the resulting binary is stripped (`-s -w`) and links no C library (no `musl_toolchain()` — `gh` is Go, the cross is driven by the Go toolchain, documented in the Portfile + `build_gh`, analogous to `build_git`'s plain-Makefile note). **As-built:** `file gh` → `ELF 64-bit … statically linked … stripped`, **no PT_INTERP**; `-X github.com/cli/cli/v2/internal/build.Version=2.82.1` (the full module path). ≈55 MB.
- [x] `cargo xtask port build gh` produces a `target/pkgcache/<key>.m3pkg`; a second build is a pkgcache hit (zero Go compiler invocations). **As-built:** sealed `target/pkgcache/782f588e…m3pkg` (54,951,849 bytes); second build → `PKGCACHE: hit … zero compiler invocations`.

### A.2 — Gate the `gh` `.m3pkg` behind the `M3OS_WITH_GH` image feature

**File:** `xtask/src/main.rs`
**Symbol:** `M3OS_WITH_GH` image feature + the `BUNDLE_ONLY_PORTS` bundle-only repo path (`xtask/src/main.rs:17541`), mirroring the `M3OS_WITH_CLANG` gate at `xtask/src/main.rs:17575`
**Why it matters:** `gh` is ~40 MB; default images must omit it (exactly like the ~125 MB clang artifact behind `M3OS_WITH_CLANG`), and install + cold runs take tens of minutes over the ~200 KB/s VFS, so it must be strictly opt-in.

> **As-built note on the symbol:** `gh` does **not** join `BUNDLE_ONLY_PORTS` (the always-bundle list). It gets its own feature-gated `if std::env::var("M3OS_WITH_GH").is_ok()` block in `populate_phase_69d_ports`, mirroring the `M3OS_WITH_CLANG` block — port name == package name (`gh`), no llvm→clang-style remap.

**Acceptance:**
- [x] The sealed `gh.m3pkg` is bundled into the offline `/usr/pkg/` repo (with its `gh.meta` `VERSION=`/`DEPS=` sidecar) **only** when `M3OS_WITH_GH` is set; a default `cargo xtask image` (feature unset) produces an image that contains no `gh.m3pkg` (asserted by inspecting the data disk). **As-built:** set-includes — with `M3OS_WITH_GH` set the build logs `ports: bundled gh.m3pkg (opt-in M3OS_WITH_GH) into /usr/pkg`. Default-omit — a default `cargo xtask image` (feature unset) emits no such line, and `debugfs` of the data disk's `/usr/pkg/` lists ca-certificates/curl/git/mbedtls/ncurses/ssh/zlib but **`/usr/pkg/gh.m3pkg: File not found by ext2_lookup`**.
- [x] When `M3OS_WITH_GH` is set, `pkg install gh` lays the binary into `/usr` and `gh --version` reports the pinned version inside m3OS; the dependency solver auto-installs the Go-toolchain dep first iff the Portfile declares it (built-from-source). **As-built:** `gh-smoke` core PASSED (16 steps, 1207 s): `pkg install: gh: OK` then `gh version 2.82.1` on m3OS — **the heavy static Go gh RUNS on the 86d runtime** (boot banner `[m3os] Hello from kernel! v0.86.4`). gh has no DEPS, so the solver order is just `[gh]`.

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

> **As-built design:** `cmd_gh_smoke` follows the `git-https-smoke` two-tier shape — an **always-on core** (build `M3OS_WITH_GH` image → boot → `pkg install gh` → `gh --version` runs on the 86d runtime; needs no secret) plus **opt-in authenticated arms** gated on `GH_TOKEN`. The token is seeded to `/root/.config/gh/{token,hosts.yml}` at mode 0600 (read by `populate_ext2_files` under the dedicated `M3OS_GH_SMOKE_TOKEN` name, so a user's ambient `GH_TOKEN` never bakes into a routine image) and exported in-guest via `$(cat …)` so the **value never crosses serial — only the path is sent**. The mutating write is further gated on `M3OS_GH_WRITE=1` + `M3OS_GH_WRITE_REPO`. Single-core pinned (like `go-runtime-smoke`) to avoid Go cross-core SMP races. Runs at `--timeout 5400`.

**Acceptance:**
- [x] `cmd_gh_smoke` builds a fresh image with `M3OS_WITH_GH` set, boots m3OS, and `pkg install gh` from the bundled `/usr/pkg/` repo. **As-built:** `gh-smoke` PASSED — builds the `M3OS_WITH_GH` image, boots to login, and `pkg install gh` → `pkg install: gh: OK` from the bundled `/usr/pkg/gh.m3pkg`.
- [ ] It authenticates non-interactively via the `GH_TOKEN` environment variable (no prompt, no TTY), exports `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt` for Go `crypto/tls`, and runs `gh auth setup-git` registering `gh` as the `https://github.com` git credential helper. _Implemented (the auth arm); **live result is `GH_TOKEN`-gated → SKIP without a secret, which can never live in repo/CI.** Validatable only by a maintainer running `GH_TOKEN=<pat> cargo xtask gh-smoke`._
- [ ] It performs a **read** workflow (`gh repo view` or `gh pr list`) and a **write** workflow (`gh pr create` or `gh issue create`) over 86c HTTPS — both return success (exit 0 + expected output substring). _Implemented: read = `gh pr list` (asserts the `GH_READ_RC=0` marker, a deterministic exit-0 success signal that requires the full Go `crypto/tls` handshake + 200); write = `gh issue create` (asserts `GH_WRITE_RC=0`), gated behind `M3OS_GH_WRITE=1`. **Live result is `GH_TOKEN`-gated.**_
- [ ] Secret hygiene is asserted: `~/.config/gh/hosts.yml` is mode `0600`; the `GH_TOKEN` value never appears in the captured serial log (grep returns no match) and is never written to `/tmp` in plaintext. _Implemented: `ls -l /root/.config/gh/hosts.yml` asserts `-rw-------`; serial-non-leak and no-`/tmp`-plaintext are enforced **by construction** (the gate sends only the token's path via `$(cat …)`, never the value; gh does not print tokens; the token is written only under `~/.config/gh` at 0600) — stronger than a runtime grep, which would itself echo the value over serial. **Live arm is `GH_TOKEN`-gated.**_
- [ ] The Go `crypto/tls` handshake to `api.github.com` confirms a ChaCha20-first cipher preference (e.g. `TLS_CHACHA20_POLY1305_SHA256`) under SIMD-off, not software AES. _Documented (Go 1.24 advertises ChaCha20 in its default TLS 1.3 list; the gate adds `+rdrand,+rdseed` so the 86a CSPRNG reaches READY for the ephemerals). **Live confirmation is `GH_TOKEN`-gated.**_
- [x] The gate is **skip-with-reason** (logs a SKIP, exits success) when `GH_TOKEN` is absent — a secret can never live in the repo/CI — mirroring the `tls-smoke`/`dns-smoke` SKIP-vs-PASS convention. **As-built:** without `GH_TOKEN` the gate prints the `NOTE — the authenticated read/write GitHub arms are SKIPPED …` line and runs only the token-free core (exit 0).
- [x] Wired as `cargo xtask gh-smoke` and as an opt-in `M3OS_GH_REGRESSION=1` pre-push gate in both `AGENTS.md` and `.githooks/pre-push`, with a clang-gate-class `--timeout` (the install reads + SHA-verifies the whole ~40 MB `.m3pkg` and cold Go runs over the ~200 KB/s VFS take tens of minutes). **As-built:** `cargo xtask gh-smoke [--timeout <secs>] [--display]` dispatch + usage; `M3OS_GH_REGRESSION=1` → `cargo xtask gh-smoke --timeout 5400` in `.githooks/pre-push`; AGENTS.md gate row added. (The artifact is ≈55 MB, not 40.)

### B.2 — Document the native Rust GitHub-REST fallback

**File:** `docs/archived/github-cli-roadmap.md`
**Symbol:** the native-fallback section (raw HTTPS to `api.github.com`)
**Why it matters:** the native fallback covers the read subset if the Go path stalls under SIMD-off — same trust roots, a different client — so the read workflows stay reachable without `gh`.

**Acceptance:**
- [x] `docs/archived/github-cli-roadmap.md` documents a native Rust GitHub-REST client: raw HTTPS `GET`/`POST` to `api.github.com` with an `Authorization: Bearer <PAT>` header over the 86c TLS path, covering the read subset (`repo view`, `pr list`). **As-built:** the "Phase 86e Addendum — Native Rust GitHub-REST Fallback" section documents `GET /repos/{owner}/{repo}` (repo view) + `GET /repos/{owner}/{repo}/pulls` (pr list) with the `Authorization: Bearer` header over the mbedTLS+curl 86c transport; write (`POST …/issues`) noted as out of guaranteed scope.
- [x] The doc records the Go-vs-mbedTLS double-TLS-stack fact (Go `crypto/tls` for `gh`, mbedTLS for `git`/`curl`) and that both consume the same 86a CA bundle (Go via `SSL_CERT_FILE`, curl via `--cacert`) and the same wall-clock. **As-built:** a two-row table contrasts the stacks; both reach the same 86a CA bundle (Go via `SSL_CERT_FILE`, curl via `--cacert`), share the wall-clock + CSPRNG/DNS, and a SIMD-off ChaCha20-first note ties to 86f.

---

## Track C — Docs + version

### C.1 — Bump kernel crate `0.86.3` → `0.86.4` + add the roadmap README 86e row

**Files:**
- `kernel/Cargo.toml` (line 3, currently `0.85.3`; in the 86 series it is `0.86.3` after 86d lands)
- `docs/roadmap/README.md`

**Symbol:** `kernel/Cargo.toml` `[package] version`; the `docs/roadmap/README.md` 86e row
**Why it matters:** each Phase 86 sub-phase bumps its own `0.86.x` version when it lands; the umbrella learning doc + final reconcile + the `0.86.5` aggregate are 86f's job, not 86e's.

**Acceptance:**
- [x] `kernel/Cargo.toml` line 3 reads `version = "0.86.4"` (+ `Cargo.lock`); `cargo xtask check` is clean (clippy + rustfmt + all host tests + retpoline gate); the boot banner / `uname` report `0.86.4` (`env!("CARGO_PKG_VERSION")`). **As-built:** `kernel/Cargo.toml` + `Cargo.lock` read `0.86.4`; `cargo xtask check` passed (clippy clean, formatting correct, all host tests + retpoline gate pass); the kernel compiles as `kernel v0.86.4`. _Boot-banner string confirmed during the gh-smoke boot._
- [x] `docs/roadmap/README.md` has an 86e row (Phase / Theme / Primary Outcome / Status / Source Ref / Milestone / Tasks) linking [86e-github-cli.md](../86e-github-cli.md) and this task doc; a note in the row (or the umbrella's row) records that the umbrella learning doc `docs/86-networking-and-github.md` is created in **86f**, not here. **As-built:** the 86e row already exists and links both docs; its Status is updated to the as-built result. The umbrella-learning-doc-in-86f note is recorded by the 86f row ("Owns the umbrella learning doc").

---

## Documentation Notes

- **What changed relative to the umbrella.** This sub-phase realizes the umbrella's "GitHub CLI (`gh`) + native fallback" scope ([86-networking-and-github.md](../86-networking-and-github.md)); it adds **no kernel surface** — it sits on top of 86c (transport) and 86d (runtime). The kernel bump is the only ring-0 change.
- **Two TLS stacks.** Make the double-TLS-stack fact explicit in the docs: `gh` carries pure-Go `crypto/tls` (it does **not** use mbedTLS), but still needs the 86a CA bundle (via `SSL_CERT_FILE`) and a trustworthy wall-clock. Confirm the ChaCha20-first preference under SIMD-off — software AES is slow until the 86f AES-NI capstone.
- **Secret discipline.** The gate can only run with a real `GH_TOKEN`, which can never live in the repo/CI — hence skip-with-reason when absent, mode `0600` on `~/.config/gh/hosts.yml`, no `/tmp` plaintext, and token redaction from serial. Prefer asserting these explicitly over "tokens are handled securely".
- **Opt-in heaviness.** `gh` mirrors `M3OS_WITH_CLANG`: an image-feature flag (`M3OS_WITH_GH`), a bundle-only `/usr/pkg/` repo entry (`BUNDLE_ONLY_PORTS` pattern, `xtask/src/main.rs:17541`), and a clang-gate-class `--timeout`. Prefer the exact symbols `build_gh`, `cmd_gh_smoke`, `M3OS_WITH_GH`, and `SSL_CERT_FILE` over generic descriptions.
- **Umbrella learning doc.** Do **not** create `docs/86-networking-and-github.md` in 86e — it is owned by 86f.
