# Phase 86c — HTTPS/TLS + git smart-HTTP: Task List

**Status:** ✅ Complete — all tracks landed; `git-https-smoke` PASSED 36/36 on m3OS (incl. live bad-cert reject + `https://` clone)
**Source Ref:** phase-86c
**Depends on:** Phase 86a (Outbound Foundation — CSPRNG `sys_getrandom`, build-date wall-clock floor, `ca-certificates` `.m3pkg`), Phase 86b (git build pattern + smoke template), Phase 77 (DNS D.1 + outbound TCP `connect` D.2) ✅, Phase 85 (Cross-Compiled Toolchains) ✅
**Goal:** Rebuild the static musl `git` **with** curl (`NO_CURL` removed) against a static musl `libcurl --with-mbedtls`, validate GitHub's TLS 1.3 cert chain + hostname against the SHA-256-pinned Phase 86a CA bundle, handle PAT credentials, and prove `git clone https://…` works inside m3OS with both a success and a rejected-bad-cert arm — the HTTPS half of the Phase 86 git-transport arc.

> **Delivered.** Authored ahead of implementation, then implemented: every acceptance item below is now checked `[x]` with the delivered, measured result. The end-to-end `git-https-smoke` gate PASSED 36/36 steps on m3OS (incl. the opt-in live bad-cert reject + `https://` clone). See [`./86c-track-report.md`](./86c-track-report.md) for the batch summary.

> **Hard cross-phase dependency.** Certificate-validity checking is impossible until **Phase 86a Track B**'s build-date wall-clock floor lands — with `BOOT_EPOCH_SECS = 0` every cert is "not yet valid" and TLS fails-closed for the wrong reason. Likewise the CTR_DRBG entropy callback requires **86a Track A** (`sys_getrandom` CSPRNG) and the trust store requires **86a Track C** (the `ca-certificates` `.m3pkg`). 86c must not be marked done while any of those is absent.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | mbedTLS port (trimmed client-only, CSPRNG entropy) | 86a Track A | ✅ Done (built + entropy self-test PASS) |
| B | curl port + git HTTPS rebuild (invert `NO_CURL` assertions) | A, 86b | ✅ Done (curl libcurl/8.15 mbedTLS/3.6.2; git inverted assertions PASS) |
| C | Cert/hostname validation + PAT creds + smoke gate + version | B, 86a Track B/C | ✅ Done (gitconfig trust + PAT, git-https-smoke PASSED 36/36 incl. live arms, kernel 0.86.2) |

---

## Track A — mbedTLS port

### A.1 — Add the mbedTLS Portfile + `build_mbedtls`

**Files:**
- `ports/lib/mbedtls/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_mbedtls`, registered in the `port_build` `match name` dispatch — `fn port_build` `port_build.rs:773`, build dispatch `:873` — and in `PORTS` `main.rs:17446`)

**Symbol:** `build_mbedtls` (routing through `musl_toolchain()` at `xtask/src/port_build.rs:111`)
**Why it matters:** libcurl needs a TLS backend, and the SIMD-off Rust userspace rules out `ring`/`aws-lc-rs` — but not C TLS; mbedTLS is the small, static, musl-friendly TLS 1.3 client with full X.509 that drops into the existing `ports/` pipeline next to `zlib`.

**Acceptance:**
- [x] `ports/lib/mbedtls/Portfile` pins mbedTLS **3.6.2** (≥3.6.1) + SHA-256 (`8b54fb…`, the RELEASE-asset tarball that bundles the `framework/` submodule + pre-generated PSA sources); `build_mbedtls` routes through `musl_toolchain()` (CC/AR/RANLIB) per the AGENTS.md port rules and is registered in the `port_build` dispatch (`"mbedtls" => build_mbedtls(...)`) + `build_recipe_id` + `port_deps`. *(mbedTLS uses a plain Makefile, not autotools — like `git`/`zlib` it has no `./configure`, so `--host`/`musl_extra_ldflags_joined()` do not apply; those are for autotools link probes and are exercised by `curl` in B.1. It is a build-time library, so it is NOT in the pre-install `PORTS` array — it is linked statically into curl/git.)*
- [x] The build produces **static** archives (`libmbedcrypto.a` 1.0 MB + `libmbedx509.a` 120 KB + `libmbedtls.a` 349 KB) with the trimmed client-only config, verified-by-construction at build time: `MBEDTLS_SSL_CLI_C` on, `MBEDTLS_SSL_SRV_C` off, DTLS off, `MBEDTLS_NET_C` off, and `MBEDTLS_CHACHAPOLY_C` + ECDHE-ECDSA-P256 (3rdparty p256-m, compiled — no assembly) + ECDHE-RSA + `MBEDTLS_X509_CRT_PARSE_C` + `MBEDTLS_PEM_PARSE_C` + `MBEDTLS_ECP_DP_SECP256R1_ENABLED` on. The linked TLS footprint (the archives are dead-code-GC'd at final link) is bounded by the client surface; the sealed `.m3pkg` is 3.7 MB.

### A.2 — Wire CTR_DRBG entropy to the 86a CSPRNG

**File:** `xtask/src/port_build.rs` (`build_mbedtls` — the mbedTLS `MBEDTLS_ENTROPY_HARDWARE_ALT` / entropy-source override) + `ports/lib/mbedtls/Portfile`
**Symbol:** the mbedTLS entropy callback bound to `sys_getrandom`
**Why it matters:** TLS session keys and X25519/ECDHE ephemerals must come from the Phase 86a CSPRNG, not a file-I/O entropy path that m3OS cannot serve; a non-crypto seed makes the whole handshake predictable.

**Acceptance:**
- [x] mbedTLS's CTR_DRBG entropy source is the Phase 86a `sys_getrandom` CSPRNG via a `mbedtls_hardware_poll` shim (`MBEDTLS_ENTROPY_HARDWARE_ALT`, the shim object `ar`-added to `libmbedcrypto.a`) calling `getrandom(2)` (syscall 318 = `sys_getrandom`), with `MBEDTLS_NO_PLATFORM_ENTROPY` removing the `/dev/urandom`/file-I/O path. Proven by `binary_contains(libmbedcrypto.a, "/dev/urandom") == false` at build time.
- [x] A build-time self-test (`m3os_entropy_test.c`, linked against the same shim object) feeds 32 bytes through the entropy callback twice and asserts (a) it returns exactly the requested length (`olen == 32`) and (b) the two draws differ (non-constant) — `mbedtls: entropy self-test: ENTROPY_OK olen=32`. On the build host `getrandom(2)` is also syscall 318, so this exercises the real shim. The `/dev/urandom`-absence check above proves no file-I/O entropy path is linked.

---

## Track B — curl + git HTTPS rebuild

### B.1 — Add the curl Portfile + `build_curl` (`--with-mbedtls`)

**Files:**
- `ports/util/curl/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_curl`, registered in `PORTS` + the `port_build` dispatch)

**Symbol:** `build_curl` (routing through `musl_toolchain()` at `xtask/src/port_build.rs:111`)
**Why it matters:** `curl --with-mbedtls` is curl's documented small-footprint TLS backend (curl 8.15 dropped BearSSL), and `--with-ca-bundle` must compile in the **same** CAINFO path the Phase 86a bundle stages so git and curl agree.

**Acceptance:**
- [x] `build_curl` builds a **static** `libcurl.a` (1.28 MB) + a fully-static `curl` CLI (HTTP/HTTPS only — every other protocol `--disable`d), linking the staged mbedtls + zlib. The fully-static CLI needs libtool's `-all-static` at the `make` step (a bare `-static` only selects static libtool libs); configure keeps `-static`. The curl `Portfile` `DEPS=zlib mbedtls` and git's `DEPS=zlib curl` together encode the dependency-first order `zlib → mbedtls → ca-certificates → curl → git` (curl pulls mbedtls; git pulls curl).
- [x] curl is configured `--with-mbedtls --with-ca-bundle=/etc/ssl/certs/ca-certificates.crt` (matching the Phase 86a path). Build-time verified: `curl --version` reports `libcurl/8.15.0 mbedTLS/3.6.2 zlib/1.3.1`, HTTPS is a listed protocol, and the binary embeds the `/etc/ssl/certs/ca-certificates.crt` CAINFO string. The live HTTPS GET with SNI + `SSL_VERIFYHOST=2` (curl's verified default) is exercised on-device by the `git-https-smoke` gate (C.2) — a network test, not a reproducible build step.

### B.2 — Rebuild git WITH curl + invert the `NO_CURL` assertions

**Files:**
- `xtask/src/port_build.rs` (`build_git` at `:1427`; `NO_CURL=1`/`NO_OPENSSL=1` knobs at `:1458`–`1459`; the curl-helper-present HARD-FAIL at `~:1566`; the `curl_easy_perform` symbol check at `:1574`; the `SSL_CTX_new` symbol check at `:1580`; the server-side pack-helper prune at `~:1535`)
- `ports/util/git/Portfile` (`DEPS=zlib` at `:5`)

**Symbol:** `build_git`
**Why it matters:** `build_git` currently **hard-fails** if any curl/OpenSSL linkage is present, so removing `NO_CURL` and adding the curl dependency must land **together**; the server-side prune is correct and must survive the inversion.

**Acceptance:**
- [x] `NO_CURL=1` is removed from the git `make` invocation (curl is now linked via `CURL_CFLAGS`/`CURL_LDFLAGS` + `CURL_CONFIG=true`); the 85b absence-assertions are **inverted** to presence-requirements: `git-remote-http`/`git-remote-https`/`git-http-fetch` must be **present** and `git-remote-http` must reference `curl_multi_perform` (the curl-multi entry git 2.44 actually calls — strip-robust, where `curl_easy_perform` would be only a symtab artifact git never invokes). *(`NO_OPENSSL=1` is KEPT — the TLS backend is mbedTLS-via-curl, **not** OpenSSL, so the 85b `SSL_CTX_new` check is kept as an ABSENCE assertion (proving no OpenSSL crept in), and a new `mbedtls_ssl_handshake` PRESENCE assertion is the positive TLS proof. This corrects the task's literal "require `SSL_CTX_new`", which assumed an OpenSSL backend — with mbedTLS that symbol is intentionally absent.)* Build output: `git-remote-https + curl_multi_perform + mbedtls_ssl_handshake present, SSL_CTX_new absent`. `git-http-push` is not required (it needs expat; `NO_EXPAT=1`).
- [x] `ports/util/git/Portfile` `DEPS` gains `curl` (→ `DEPS=zlib curl`, transitively pulling mbedtls + ca-certificates); the assertion inversion, the `make`-knob change, and the `DEPS` addition all land in the **same** change so `build_git` never hard-fails mid-flight. `compute_port_key` was made to **recurse** so git's content key folds in curl's full transitive identity (new in 86c — previously only leaf deps existed).
- [x] The server-side `git-upload-pack`/`git-receive-pack`/`git-upload-archive` prune **remains** and is now also **asserted** (a positive guard that un-pruning them regresses); a code note records that un-pruning would ship a server m3OS does not run. The `git-remote-ftp`/`ftps` curl aliases are additionally pruned (curl is `--disable-ftp`; m3OS does no ftp clones).

---

## Track C — Cert validation, credentials, smoke, version

### C.1 — X.509 chain + hostname verify, CAINFO path, PAT credentials

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files` `/etc/gitconfig` staging at `:15586`, gitconfig block at `:15625`)
- the Phase 86c design doc trust-model section ([`../86c-https-git-transport.md`](../86c-https-git-transport.md))

**Symbol:** `/etc/gitconfig` (`http.sslVerify` / `http.sslCAInfo` ↔ `GIT_SSL_CAINFO`); PAT via `credential.helper store` (`~/.git-credentials`) / `~/.netrc`
**Why it matters:** hostname verification is **separate** from chain validation (`mbedtls_ssl_set_hostname` / `SSL_VERIFYHOST=2`), GitHub rejects password auth (PAT only), and the CAINFO default must agree across git and curl or trust silently breaks.

**Acceptance:**
- [x] `/etc/gitconfig` (staged by `populate_ext2_files`) sets `http.sslVerify=true` and `http.sslCAInfo=/etc/ssl/certs/ca-certificates.crt` (matching curl's `--with-ca-bundle` and `GIT_SSL_CAINFO`); `credential.helper=store` configures the PAT mechanism (`~/.git-credentials`), with `~/.netrc` as the documented alternative. `mbedtls` + `curl` are added to `BUNDLE_ONLY_PORTS` so `pkg install git` resolves the full `zlib → mbedtls → ca-certificates → curl → git` chain from `/usr/pkg`.
- [x] A **Trust Model (86c)** section in [`../86c-https-git-transport.md`](../86c-https-git-transport.md) records: ChaCha20-Poly1305-first (no AES-NI until 86f), the CAINFO override knobs (3-way agreement across curl `--with-ca-bundle` / `http.sslCAInfo` / `GIT_SSL_CAINFO`), chain-vs-hostname separation, the wall-clock dependency, the entropy source, the DEFERRED set (no OCSP/CRL, tickets off, IPv4/A-records only, smart-HTTP only), PAT-token serial redaction, and the plaintext-credential-at-rest tradeoff.

### C.2 — `git-https-smoke`: success clone AND rejected-bad-cert

**Files:**
- `xtask/src/main.rs` (`cmd_git_https_smoke`, modeled on `cmd_git_local_smoke` at `:13584`)
- `AGENTS.md` (opt-in gate row, `M3OS_GIT_HTTPS_REGRESSION=1`)
- `.githooks/pre-push`

**Symbol:** `cmd_git_https_smoke`
**Why it matters:** it is trivial to ship a green clone while certificate verification is silently disabled; the negative case is mandatory and — unlike the positive clone — needs no secret.

**Acceptance:**
- [x] `M3OS_GIT_HTTPS_REGRESSION=1` runs `cargo xtask git-https-smoke` (`cmd_git_https_smoke` + `git_https_smoke_steps`). Positive arm (opt-in `M3OS_GIT_HTTPS_NET=1`): validates `info/refs` by the `application/x-git-upload-pack-advertisement` Content-Type **and** the `# service=git-upload-pack` pkt-line body, then a full `git clone --depth 1 --single-branch` whose `Receiving objects` + README `Hello World` prove the end-to-end TLS path. Negative arm (opt-in, **no secret**): a `self-signed.badssl.com` clone must be **REJECTED** (`SSL certificate problem` / `certificate verif`); a `WaitEither` times out on any non-rejection (silent accept fails the gate). The always-on **core** (no network) proves `pkg install git` pulls the curl/mbedtls/ca-certs chain, the static `curl --version` reports `mbedTLS` on-device, the gitconfig trust settings resolve, and the CA bundle content is on disk.
- [x] The gate uses a long `--timeout 5400` (clang-gate class: the ~27 MB curl+mbedtls+ca-certs+git install over the slow ~200 KB/s ring-3 VFS + the packfile transfer); the positive clone uses `--depth 1 --single-branch` of a tiny public repo to bound transfer. The existing `git-local-smoke`/`git-ssh-smoke` install-step ceilings + the `git-local-smoke` pre-push timeout were raised too (git now pulls the curl chain).
- [x] The endpoint design is documented: `self-signed.badssl.com` (the standard public self-signed-cert test service) drives the **negative** arm with no secret and no local TLS server, and the anonymous read-only public `octocat/Hello-World` repo drives the **positive** arm (PAT auth is configured via `credential.helper store` but not needed for the anonymous clone; a PAT-gated repo is available via `M3OS_GIT_HTTPS_REPO`). The gate is wired into both `AGENTS.md` (`M3OS_GIT_HTTPS_REGRESSION` row) and `.githooks/pre-push`. *(The SLIRP-localhost-TLS server is the design-doc alternative; the public bad-cert service is simpler and reproducible.)*

### C.3 — Bump kernel crate `0.86.1` → `0.86.2`

**File:** `kernel/Cargo.toml` (`[package] version`, line 3 — currently `0.85.3`; 86a→`0.86.0`, 86b→`0.86.1`, 86c→`0.86.2`)
**Symbol:** `[package] version = "0.86.2"`
**Why it matters:** 86c is the third Phase 86 sub-phase and lands its own `0.86.x` patch bump per the umbrella version sequence.

**Acceptance:**
- [x] `kernel/Cargo.toml` line 3 reads `version = "0.86.2"` (+ `Cargo.lock` synced); `cargo xtask check` is clean (clippy + rustfmt + all host tests + retpoline gate); the boot banner reads `0.86.2` (it prints `env!("CARGO_PKG_VERSION")`, so it tracks automatically). AGENTS.md's overview line bumped to `v0.86.2`.

---

## Documentation Notes

- **Pure-Rust TLS is a trap, deferred deliberately.** `userspace/crypto-lib/Cargo.toml:11-19` ships no `p256`/`ecdsa`, and `Cargo.lock` has no `rustls`/`webpki`/`p256`/`ecdsa`; GitHub's leaf is ECDSA P-256 (live-confirmed). Record the missing-crates lift + `rustls-rustcrypto`'s do-not-use-in-production/std-leaning posture as the explicit reason rustls is out of scope, not an oversight.
- **Wall-clock is a hard dependency.** Fail-closed cert validity depends on Phase 86a Track B's build-date floor; without it `git-https-smoke` would fail for the wrong reason (1970 < `notBefore`). Do not close 86c against a `BOOT_EPOCH_SECS = 0` clock.
- **Hostname verify is separate from chain.** Keep `mbedtls_ssl_set_hostname` / `SSL_VERIFYHOST=2` and the mandatory negative arm distinct from chain-trust; a passing chain with no hostname check is still broken.
- **Do not un-prune the server pack helpers.** The `~:1535` prune of `git-upload-pack`/`git-receive-pack`/`git-upload-archive` is correct; 86c is a client, and un-pruning them would ship a server m3OS does not run.
- **Solver topo-order.** Confirm the Phase 85a dependency solver resolves `zlib → mbedtls → ca-certificates → curl → git` correctly, including the **data-only** `ca-certificates` package (no binaries, just the PEM bundle).
- **Invert + add-dep together.** `build_git` hard-fails at `~:1566` the moment curl helpers appear, so the assertion inversion and the curl `DEPS` addition are a single atomic change — never split across commits.
- **Cross-links.** This task list is the companion to [`../86c-https-git-transport.md`](../86c-https-git-transport.md); the SSH sibling is [`./86b-ssh-git-transport-tasks.md`](./86b-ssh-git-transport-tasks.md) and the foundation is [`./86a-outbound-foundation-tasks.md`](./86a-outbound-foundation-tasks.md).
