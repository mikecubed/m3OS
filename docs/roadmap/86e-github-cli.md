# Phase 86e - GitHub CLI (`gh`) + Native Fallback

**Status:** 🟡 Implemented — `gh-smoke` core PASSED on m3OS (static Go `gh` 2.82.1 builds, bundles behind `M3OS_WITH_GH`, installs, and `gh --version` runs on the 86d runtime; kernel `0.86.4`). The authenticated read/write GitHub workflows are implemented but `GH_TOKEN`-gated (skip-with-reason without a secret); live-auth verification awaits a maintainer with a PAT.
**Source Ref:** phase-86e
**Depends on:** Phase 86c (HTTPS/TLS + CA + PAT) — see [86c-https-git-transport.md](./86c-https-git-transport.md), Phase 86d (Go runtime) — see [86d-go-runtime.md](./86d-go-runtime.md), Phase 86a (entropy/clock/DNS/CA bundle, transitively), Phase 85 (Cross-Compiled Toolchains) ✅, Phase 77 (DNS reply delivery + outbound TCP `connect`) ✅
**Builds on:** Sub-phase **86e** of the Phase 86 umbrella ([86-networking-and-github.md](./86-networking-and-github.md)). It sits *on top of* the 86c HTTPS/TLS + CA + PAT machinery (`gh auth setup-git` registers `gh` as a git credential helper) and consumes the 86d Go runtime to run the `gh` binary; it adds no kernel surface.
**Primary Components:** `ports/util/gh/Portfile` + `xtask/src/port_build.rs` (`build_gh`), the `M3OS_WITH_GH` image feature in `xtask/src/main.rs` (mirroring `M3OS_WITH_CLANG` / `BUNDLE_ONLY_PORTS`), `xtask/src/main.rs` (`cmd_gh_smoke`), the on-disk `~/.config/gh/hosts.yml` + `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`, `docs/archived/github-cli-roadmap.md`, `docs/roadmap/README.md`

## Milestone Goal

`gh`, the GitHub CLI, runs inside m3OS and completes authenticated GitHub workflows — reading a repo / listing PRs, and creating a PR / opening an issue — over real HTTPS to `api.github.com`. It ships as a single `~40 MB` Go `.m3pkg` gated behind an `M3OS_WITH_GH` image feature (so default images stay small, exactly like Clang behind `M3OS_WITH_CLANG`), authenticates non-interactively via `GH_TOKEN`, and `gh auth setup-git` registers it as a git credential helper so subsequent HTTPS `git` push/clone reuse the Phase 86c curl + TLS + PAT path. A native Rust GitHub-REST fallback (raw HTTPS to `api.github.com`) is documented for the read subset in case the Go path stalls. Kernel bumps to `0.86.4`.

## Why This Phase Exists

The transport arc (86b SSH, 86c HTTPS) gives m3OS authenticated `git` remotes; the GitHub-CLI arc closes the loop by letting the OS *operate* on GitHub itself — open a PR, file an issue, inspect CI — not just move packfiles. `gh` is the canonical tool for that, and it is the headline payoff of bringing up the Go runtime in 86d: 86d proves *a* static Go binary runs (goroutine + plaintext HTTP); 86e proves the *real, heavy, TLS-using* Go program runs end-to-end against a live remote.

It is deliberately a separate sub-phase from 86d because the risks are different. 86d is kernel bring-up (`MAP_FIXED`, edge-`epoll`, `SIGURG`). 86e is packaging + authentication + secret-handling: a ~40 MB artifact that must be opt-in, a token that must never leak to serial or `/tmp`, and a credential-helper handshake that must correctly hand HTTPS `git` operations back to the 86c stack. Bundling `gh` only after 86d has independently de-risked the runtime keeps each failure mode legible.

There is also a deliberate architectural fact this phase makes concrete: m3OS now carries **two TLS implementations**. `git`/`curl` use the 86c C **mbedTLS** stack; `gh` carries its own pure-Go **`crypto/tls`**. `gh` therefore does *not* depend on mbedTLS — but it still depends on the 86a foundation (a trustworthy CSPRNG, a sane wall-clock, DNS) and on the *same* CA bundle, reached through Go's `SSL_CERT_FILE` rather than a curl `--cacert`.

## Learning Goals

- How a managed-runtime CLI authenticates without a TTY (`GH_TOKEN` env var, `~/.config/gh/hosts.yml`) and why interactive `gh auth login` is the wrong path on a headless OS.
- How `gh auth setup-git` makes `gh` a git **credential helper**, so an HTTPS `git` op transparently reuses the 86c curl + TLS + PAT machinery instead of re-implementing transport.
- Why Go's `crypto/tls` is a *second* TLS stack that still consumes the shared 86a CA bundle (via `SSL_CERT_FILE`) and clock — and how the SIMD-off constraint makes its software ChaCha20 path the one to prefer over software AES until 86f.
- How to test a network-auth tool deterministically *without* committing a secret: `GH_TOKEN` can never live in the repo or CI, so the gate is skip-with-reason when the token is absent.
- The packaging discipline for a multi-hundred-MB-class opt-in artifact (image feature flag, bundle-only repo entry, very long install/run timeout over the ~200 KB/s VFS).

## Feature Scope

### `gh` build + bundling

A statically-linked `gh` (Go `1.22+`, `CGO_ENABLED=0`) cross-built host-side via a new `ports/util/gh/Portfile` + `build_gh`, sealed into a `.m3pkg`, and bundled into the offline `/usr/pkg/` repo **only when the `M3OS_WITH_GH` image feature is set** — mirroring how `M3OS_WITH_CLANG` keeps the ~125 MB clang artifact out of default images. Default images omit the ~40 MB `gh` entirely. The Portfile records the build provenance decision: built-from-source (which `DEPS` the 86d Go toolchain) vs prebuilt.

### Auth + workflows + native fallback

Non-interactive authentication via the `GH_TOKEN` environment variable (no prompt, no TTY), with `gh auth setup-git` registering `gh` as a git credential helper. A `cmd_gh_smoke` gate that, when `GH_TOKEN` is present, installs `gh`, authenticates, performs a **read** (`gh repo view` / `gh pr list`) and a **write** (`gh pr create` / `gh issue create`) over 86c HTTPS, and asserts secret hygiene. A documented **native Rust GitHub-REST fallback** (raw HTTPS `GET`/`POST` to `api.github.com` with a `Bearer` PAT) covering the read subset, for when the Go path stalls.

### Docs + version

Bump `kernel/Cargo.toml` `0.86.3` → `0.86.4`, add the `docs/roadmap/README.md` 86e row, and update `docs/archived/github-cli-roadmap.md`. The umbrella *learning* doc (`docs/86-networking-and-github.md`) is **not** created here — per the umbrella, it is owned by 86f (the last sub-phase); 86e only adds its roadmap-README row.

## Important Components and How They Work

### `build_gh` in `port_build.rs` + the `M3OS_WITH_GH` feature

A new port `build_*` function following the standard port plumbing, registered in `PORTS` (`xtask/src/main.rs:17446`) and the `port_build` `match name` dispatch (`xtask/src/port_build.rs:773` / `:873`). Because `gh` is Go (not autotools/musl-C), the cross is driven by the Go toolchain (`go build -trimpath -ldflags '-s -w -X internal/build.Version=<v>'`, `CGO_ENABLED=0`), not `musl_toolchain()` — analogous to how `build_git` notes that git's plain Makefile bypasses `./configure`. The sealed `.m3pkg` is bundled only under `M3OS_WITH_GH`, mirroring the feature-gated `if env::var("M3OS_WITH_CLANG")` image-staging block at `xtask/src/main.rs:17575` — **distinct from** the always-bundle `BUNDLE_ONLY_PORTS` list at `:17541` (currently `git`, `python` only), which `gh` does **not** join.

### `cmd_gh_smoke` in `main.rs`

A serial gate modeled on `cmd_git_local_smoke` (`xtask/src/main.rs:13584`) and `cmd_clang_smoke` (`:14111`). It builds a fresh image with `M3OS_WITH_GH` set (so the artifact bundles into `/usr/pkg/`), boots m3OS, `pkg install gh` (no deps if prebuilt; the Go toolchain dep if built-from-source), exports `GH_TOKEN` + `SSL_CERT_FILE`, runs `gh auth setup-git`, then a read and a write against GitHub, and asserts the token never reached serial or `/tmp` and that `~/.config/gh/hosts.yml` is mode `0600`. It is **skip-with-reason** when `GH_TOKEN` is absent (a secret can never live in the repo/CI). Runs at a clang-gate-class `--timeout` because install + cold runs of a 40 MB Go binary over the ~200 KB/s ring-3 VFS take tens of minutes.

### Credential-helper handshake (86e → 86c)

`gh auth setup-git` writes a git config stanza that makes `gh` the credential helper for `https://github.com`. When `git` then performs an HTTPS operation, it shells out to `gh` for the credential, and the *transport* is still the 86c curl + mbedTLS + PAT path. 86e therefore sits strictly on top of 86c: it supplies the credential, 86c moves the bytes.

### Native Rust GitHub-REST fallback

Documented (not the primary path): a small Rust client that issues raw HTTPS `GET`/`POST` to `api.github.com` with an `Authorization: Bearer <PAT>` header over the 86c TLS path, covering the *read* subset (`repo view`, `pr list`). It exists so the read workflows are still reachable if the Go runtime or Go `crypto/tls` stalls under SIMD-off; it is the same trust roots, a different client.

## How This Builds on Earlier Phases

- **Extends Phase 86c** by reusing its curl + mbedTLS + X.509 + PAT machinery as the *transport* under the `gh` credential helper, instead of writing a new git-over-HTTPS path.
- **Builds on Phase 86d** by running the `gh` Go binary on the runtime that 86d brought up (`MAP_FIXED`, edge-`epoll`, `SIGURG`/`tgkill` preemption delivered at syscall-return); `gh` is I/O-bound, so syscall-return preemption (no timer-IRQ-return path) is sufficient.
- **Builds on Phase 86a (transitively)** for the CSPRNG (Go `crypto/tls` ephemerals), the wall-clock floor (cert `notBefore`/`notAfter`), DNS (`api.github.com` A-record), and the shared CA bundle reached via `SSL_CERT_FILE`.
- **Reuses Phase 85**'s `.m3pkg` substrate + `M3OS_WITH_CLANG`-style opt-in image-feature pattern + `BUNDLE_ONLY_PORTS` bundling, without pulling those phases back onto the release-critical path.

## Implementation Outline

1. Add `ports/util/gh/Portfile` (pinned `gh` version + SHA-256, build-from-source-vs-prebuilt provenance recorded; `DEPS` the 86d Go toolchain iff built-from-source) and `build_gh` (static `go build -trimpath -ldflags '-s -w -X internal/build.Version'`, `CGO_ENABLED=0`). Register in `PORTS` and the `port_build` dispatch.
2. Gate the sealed `.m3pkg` behind the `M3OS_WITH_GH` image feature and add `gh` to the bundle-only repo path, mirroring the clang gate at `xtask/src/main.rs:17575` / `BUNDLE_ONLY_PORTS` at `:17541`.
3. Wire non-interactive auth: `GH_TOKEN` env var, `~/.config/gh/hosts.yml` (mode `0600`), `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt` for Go `crypto/tls`, and `gh auth setup-git` registering the credential helper into the 86c path.
4. Add `cmd_gh_smoke` (modeled on `cmd_git_local_smoke` / `cmd_clang_smoke`): build with `M3OS_WITH_GH`, boot, `pkg install gh`, authenticate, a read + a write workflow, secret-hygiene assertions, skip-with-reason when `GH_TOKEN` is absent.
5. Document the native Rust GitHub-REST fallback in `docs/archived/github-cli-roadmap.md`.
6. Bump `kernel/Cargo.toml` `0.86.3` → `0.86.4`; add the 86e row to `docs/roadmap/README.md` with a note that the umbrella learning doc is created in 86f.

## Acceptance Criteria

- `gh` builds statically (Go `1.22+`, `CGO_ENABLED=0`, `go build -trimpath -ldflags '-s -w -X internal/build.Version=<v>'`) via `cargo xtask port build gh` and seals into a `.m3pkg`; the artifact is bundled into `/usr/pkg/` **only** when `M3OS_WITH_GH` is set (default images omit it), and `pkg install gh` then `gh --version` runs inside m3OS.
- `ports/util/gh/Portfile` records the build-from-source (DEPS = 86d Go toolchain) vs prebuilt provenance decision, pins a version + SHA-256.
- With `GH_TOKEN` set, `cmd_gh_smoke` boots m3OS, installs `gh`, authenticates non-interactively (no prompt), runs a read (`gh repo view` / `gh pr list`) and a write (`gh pr create` / `gh issue create`) over 86c HTTPS, all PASS.
- `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt` is exported for Go `crypto/tls`; `~/.config/gh/hosts.yml` is mode `0600`; the token never appears on serial output and is never written to `/tmp` in plaintext (the gate greps the serial log and asserts absence).
- The gate is **skip-with-reason** (not failure) when `GH_TOKEN` is absent; it is wired as `cargo xtask gh-smoke` and as an opt-in `M3OS_GH_REGRESSION=1` pre-push gate in both `AGENTS.md` and `.githooks/pre-push`, with a clang-gate-class `--timeout`.
- The native Rust GitHub-REST fallback (raw HTTPS `GET`/`POST` to `api.github.com` with a `Bearer` PAT, read subset) is documented.
- `kernel/Cargo.toml` reads `0.86.4`; `cargo xtask check` is clean; boot banner / `uname` report `0.86.4`. `docs/roadmap/README.md` has an 86e row (Theme / Outcome / Status / Source Ref / Milestone / Tasks) noting the umbrella learning doc is created in 86f.

## Companion Task List

- [Phase 86e Task List](./tasks/86e-github-cli-tasks.md)

## How Real OS Implementations Differ

- On Linux/macOS, `gh auth login` is interactive (device-code or browser OAuth); m3OS has no browser/TTY-OAuth, so `GH_TOKEN` + non-interactive auth is the only path, and `gh auth setup-git` is what makes HTTPS `git` ops reuse the credential.
- Mainstream systems run a *dynamic* `gh` against a system Go and shared TLS; m3OS ships a *static* `gh` (`CGO_ENABLED=0`) carrying its own pure-Go `crypto/tls`, bundled behind an image feature because it is ~40 MB and rarely needed — the same opt-in posture as `M3OS_WITH_CLANG`. (Building `gh` from source needs Go `≥1.22` per `cli/cli`'s `go.mod`.)
- Distros store the token in a keyring/secret-service; m3OS has only file-at-rest secrecy (`~/.config/gh/hosts.yml` mode `0600`, never `/tmp` plaintext, redacted from serial) — the credential-at-rest limit the umbrella documents.
- **Redox OS** ships `gh`-class tooling via a C cross-build against `relibc`; m3OS's analogue is the Go cross-build sealed into a `.m3pkg` and gated behind an image feature.
- Mature `gh` benefits from hardware AES-NI in its TLS; under SIMD-off, software AES is slow, so m3OS confirms a ChaCha20-first cipher preference until the 86f AES-NI capstone.

## Deferred Until Later

- Interactive `gh auth login` (device-code / OAuth browser flow) and a keyring-backed secret store.
- Hardware-AES-accelerated `gh` TLS — that throughput pass is the 86f userspace-SIMD/AES-NI capstone.
- The umbrella learning doc `docs/86-networking-and-github.md` — owned by 86f.
- `gh` extensions, `gh codespace`, `gh copilot`, and other large subcommand families beyond the read/write workflow subset.
- Self-hosting the Go toolchain (building Go, or `gh`, inside m3OS) — umbrella-level deferral.
- IPv6 / AAAA resolution of GitHub endpoints — Phase 91.
