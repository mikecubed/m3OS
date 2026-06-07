# Phase 86c — HTTPS/TLS + git smart-HTTP: Task List

**Status:** Planned
**Source Ref:** phase-86c
**Depends on:** Phase 86a (Outbound Foundation — CSPRNG `sys_getrandom`, build-date wall-clock floor, `ca-certificates` `.m3pkg`), Phase 86b (git build pattern + smoke template), Phase 77 (DNS D.1 + outbound TCP `connect` D.2) ✅, Phase 85 (Cross-Compiled Toolchains) ✅
**Goal:** Rebuild the static musl `git` **with** curl (`NO_CURL` removed) against a static musl `libcurl --with-mbedtls`, validate GitHub's TLS 1.3 cert chain + hostname against the SHA-256-pinned Phase 86a CA bundle, handle PAT credentials, and prove `git clone https://…` works inside m3OS with both a success and a rejected-bad-cert arm — the HTTPS half of the Phase 86 git-transport arc.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirror the 92-vfs-bulk-io style.)

> **Hard cross-phase dependency.** Certificate-validity checking is impossible until **Phase 86a Track B**'s build-date wall-clock floor lands — with `BOOT_EPOCH_SECS = 0` every cert is "not yet valid" and TLS fails-closed for the wrong reason. Likewise the CTR_DRBG entropy callback requires **86a Track A** (`sys_getrandom` CSPRNG) and the trust store requires **86a Track C** (the `ca-certificates` `.m3pkg`). 86c must not be marked done while any of those is absent.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | mbedTLS port (trimmed client-only, CSPRNG entropy) | 86a Track A | Planned |
| B | curl port + git HTTPS rebuild (invert `NO_CURL` assertions) | A, 86b | Planned |
| C | Cert/hostname validation + PAT creds + smoke gate + version | B, 86a Track B/C | Planned |

---

## Track A — mbedTLS port

### A.1 — Add the mbedTLS Portfile + `build_mbedtls`

**Files:**
- `ports/lib/mbedtls/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_mbedtls`, registered in the `port_build` `match name` dispatch — `fn port_build` `port_build.rs:773`, build dispatch `:873` — and in `PORTS` `main.rs:17446`)

**Symbol:** `build_mbedtls` (routing through `musl_toolchain()` at `xtask/src/port_build.rs:111`)
**Why it matters:** libcurl needs a TLS backend, and the SIMD-off Rust userspace rules out `ring`/`aws-lc-rs` — but not C TLS; mbedTLS is the small, static, musl-friendly TLS 1.3 client with full X.509 that drops into the existing `ports/` pipeline next to `zlib`.

**Acceptance:**
- [ ] `ports/lib/mbedtls/Portfile` pins mbedTLS **≥3.6.1** + SHA-256; `build_mbedtls` routes through `musl_toolchain()` + `musl_extra_ldflags_joined()` + `--host=x86_64-linux-musl` per the AGENTS.md port rules and is registered in `PORTS` (`xtask/src/main.rs:17446`) + the `port_build` dispatch (`xtask/src/port_build.rs:873`).
- [ ] The build produces a **static** library with the trimmed client-only config: `MBEDTLS_SSL_CLI_C` on, `MBEDTLS_SSL_SRV_C` off, DTLS off, and `MBEDTLS_CHACHAPOLY_C` + ECDHE-ECDSA-P256 (mbedTLS 3.5+ p256-m) + ECDHE-RSA + `MBEDTLS_X509_CRT_PARSE_C` + `MBEDTLS_PEM_PARSE_C` on. Trimmed footprint is on the order of 45–300 KB.

### A.2 — Wire CTR_DRBG entropy to the 86a CSPRNG

**File:** `xtask/src/port_build.rs` (`build_mbedtls` — the mbedTLS `MBEDTLS_ENTROPY_HARDWARE_ALT` / entropy-source override) + `ports/lib/mbedtls/Portfile`
**Symbol:** the mbedTLS entropy callback bound to `sys_getrandom`
**Why it matters:** TLS session keys and X25519/ECDHE ephemerals must come from the Phase 86a CSPRNG, not a file-I/O entropy path that m3OS cannot serve; a non-crypto seed makes the whole handshake predictable.

**Acceptance:**
- [ ] mbedTLS's CTR_DRBG entropy source is the Phase 86a `sys_getrandom` CSPRNG (e.g. via a hardware-entropy `mbedtls_hardware_poll` shim), with **no** `/dev/urandom`/file-I/O entropy path compiled in.
- [ ] A test feeds N bytes through the mbedTLS entropy callback and asserts (a) it returns exactly the requested length and (b) two successive N-byte draws differ (non-constant), with the callback resolving to `sys_getrandom` and **no** `/dev/urandom`/file-I/O entropy path linked.

---

## Track B — curl + git HTTPS rebuild

### B.1 — Add the curl Portfile + `build_curl` (`--with-mbedtls`)

**Files:**
- `ports/util/curl/Portfile` (new)
- `xtask/src/port_build.rs` (new `build_curl`, registered in `PORTS` + the `port_build` dispatch)

**Symbol:** `build_curl` (routing through `musl_toolchain()` at `xtask/src/port_build.rs:111`)
**Why it matters:** `curl --with-mbedtls` is curl's documented small-footprint TLS backend (curl 8.15 dropped BearSSL), and `--with-ca-bundle` must compile in the **same** CAINFO path the Phase 86a bundle stages so git and curl agree.

**Acceptance:**
- [ ] `build_curl` builds a **static** `libcurl` (HTTP/HTTPS only — other protocols disabled), linking the staged mbedtls + zlib; the curl `Portfile` `DEPS` and git's `DEPS` together encode the dependency-first order `zlib → mbedtls → curl → ca-certificates → git`.
- [ ] curl is configured `--with-mbedtls --with-ca-bundle=/etc/ssl/certs/ca-certificates.crt` (matching the Phase 86a path); an HTTPS GET to a controllable endpoint succeeds with SNI sent and `SSL_VERIFYHOST=2`.

### B.2 — Rebuild git WITH curl + invert the `NO_CURL` assertions

**Files:**
- `xtask/src/port_build.rs` (`build_git` at `:1427`; `NO_CURL=1`/`NO_OPENSSL=1` knobs at `:1458`–`1459`; the curl-helper-present HARD-FAIL at `~:1566`; the `curl_easy_perform` symbol check at `:1574`; the `SSL_CTX_new` symbol check at `:1580`; the server-side pack-helper prune at `~:1535`)
- `ports/util/git/Portfile` (`DEPS=zlib` at `:5`)

**Symbol:** `build_git`
**Why it matters:** `build_git` currently **hard-fails** if any curl/OpenSSL linkage is present, so removing `NO_CURL` and adding the curl dependency must land **together**; the server-side prune is correct and must survive the inversion.

**Acceptance:**
- [ ] `NO_CURL=1` (and `NO_OPENSSL=1` if OpenSSL is not adopted) is removed from the git `make` invocation; the curl-helper-present check (`~:1566`) and the `curl_easy_perform` (`:1574`) / `SSL_CTX_new` (`:1580`) symbol checks are **inverted** to REQUIRE those symbols/helpers, so a curl-enabled git variant builds with `git-remote-https` + `git-http-fetch` **present**.
- [ ] `ports/util/git/Portfile` `DEPS` (`:5`) gains `curl` (transitively pulling mbedtls + ca-certificates); the inversion and the `DEPS` addition land in the **same** change so `build_git` never hard-fails mid-flight.
- [ ] The server-side `git-upload-pack`/`git-receive-pack`/`git-upload-archive` prune (`~:1535`) **remains**; a code/doc note records that un-pruning them would be wrong (they are the server half of the protocol m3OS does not run).

---

## Track C — Cert validation, credentials, smoke, version

### C.1 — X.509 chain + hostname verify, CAINFO path, PAT credentials

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files` `/etc/gitconfig` staging at `:15586`, gitconfig block at `:15625`)
- the Phase 86c design doc trust-model section ([`../86c-https-git-transport.md`](../86c-https-git-transport.md))

**Symbol:** `/etc/gitconfig` (`http.sslVerify` / `http.sslCAInfo` ↔ `GIT_SSL_CAINFO`); PAT via `credential.helper store` (`~/.git-credentials`) / `~/.netrc`
**Why it matters:** hostname verification is **separate** from chain validation (`mbedtls_ssl_set_hostname` / `SSL_VERIFYHOST=2`), GitHub rejects password auth (PAT only), and the CAINFO default must agree across git and curl or trust silently breaks.

**Acceptance:**
- [ ] `/etc/gitconfig` sets `http.sslVerify=true` and `http.sslCAInfo=/etc/ssl/certs/ca-certificates.crt` (matching curl's `--with-ca-bundle` and `GIT_SSL_CAINFO`); the PAT mechanism (`credential.helper store` or `~/.netrc`) is configured and documented.
- [ ] A trust-model doc section records: ChaCha20-Poly1305-first (no AES-NI until 86f), the CAINFO override knobs, the DEFERRED set (no OCSP/CRL, tickets off, IPv4/A-records only), that PAT tokens are redacted from serial, and the plaintext-credential-at-rest tradeoff.

### C.2 — `git-https-smoke`: success clone AND rejected-bad-cert

**Files:**
- `xtask/src/main.rs` (`cmd_git_https_smoke`, modeled on `cmd_git_local_smoke` at `:13584`)
- `AGENTS.md` (opt-in gate row, `M3OS_GIT_HTTPS_REGRESSION=1`)
- `.githooks/pre-push`

**Symbol:** `cmd_git_https_smoke`
**Why it matters:** it is trivial to ship a green clone while certificate verification is silently disabled; the negative case is mandatory and — unlike the positive clone — needs no secret.

**Acceptance:**
- [ ] `M3OS_GIT_HTTPS_REGRESSION=1` runs `cargo xtask git-https-smoke`, which (1) performs a successful clone whose `info/refs` response is validated by `Content-Type: application/x-git-upload-pack-advertisement` **and** the 5-byte pkt-line magic, and (2) confirms an expired / wrong-host / self-signed certificate is **REJECTED** (clone fails closed).
- [ ] The gate uses a long `--timeout` (curl/mbedtls/git rebuild + the slow ~200 KB/s ring-3 VFS clone — clang-gate class, e.g. ≥1800s); a clone smoke uses `--depth 1 --single-branch` of a tiny repo to bound transfer.
- [ ] The endpoint design is documented: a SLIRP-localhost-TLS server (with deliberately expired/wrong-host/self-signed certs) drives the **negative** arm with no secret, and an anonymous read-only public repo (or a PAT-gated repo) drives the **positive** arm; the gate is wired into both `AGENTS.md` and `.githooks/pre-push`.

### C.3 — Bump kernel crate `0.86.1` → `0.86.2`

**File:** `kernel/Cargo.toml` (`[package] version`, line 3 — currently `0.85.3`; 86a→`0.86.0`, 86b→`0.86.1`, 86c→`0.86.2`)
**Symbol:** `[package] version = "0.86.2"`
**Why it matters:** 86c is the third Phase 86 sub-phase and lands its own `0.86.x` patch bump per the umbrella version sequence.

**Acceptance:**
- [ ] `kernel/Cargo.toml` line 3 reads `version = "0.86.2"` (+ `Cargo.lock`); `cargo xtask check` is clean (clippy + rustfmt + all host tests + retpoline gate); the boot banner / `uname` report `0.86.2`.

---

## Documentation Notes

- **Pure-Rust TLS is a trap, deferred deliberately.** `userspace/crypto-lib/Cargo.toml:11-19` ships no `p256`/`ecdsa`, and `Cargo.lock` has no `rustls`/`webpki`/`p256`/`ecdsa`; GitHub's leaf is ECDSA P-256 (live-confirmed). Record the missing-crates lift + `rustls-rustcrypto`'s do-not-use-in-production/std-leaning posture as the explicit reason rustls is out of scope, not an oversight.
- **Wall-clock is a hard dependency.** Fail-closed cert validity depends on Phase 86a Track B's build-date floor; without it `git-https-smoke` would fail for the wrong reason (1970 < `notBefore`). Do not close 86c against a `BOOT_EPOCH_SECS = 0` clock.
- **Hostname verify is separate from chain.** Keep `mbedtls_ssl_set_hostname` / `SSL_VERIFYHOST=2` and the mandatory negative arm distinct from chain-trust; a passing chain with no hostname check is still broken.
- **Do not un-prune the server pack helpers.** The `~:1535` prune of `git-upload-pack`/`git-receive-pack`/`git-upload-archive` is correct; 86c is a client, and un-pruning them would ship a server m3OS does not run.
- **Solver topo-order.** Confirm the Phase 85a dependency solver resolves `zlib → mbedtls → curl → ca-certificates → git` correctly, including the **data-only** `ca-certificates` package (no binaries, just the PEM bundle).
- **Invert + add-dep together.** `build_git` hard-fails at `~:1566` the moment curl helpers appear, so the assertion inversion and the curl `DEPS` addition are a single atomic change — never split across commits.
- **Cross-links.** This task list is the companion to [`../86c-https-git-transport.md`](../86c-https-git-transport.md); the SSH sibling is [`./86b-ssh-git-transport-tasks.md`](./86b-ssh-git-transport-tasks.md) and the foundation is [`./86a-outbound-foundation-tasks.md`](./86a-outbound-foundation-tasks.md).
