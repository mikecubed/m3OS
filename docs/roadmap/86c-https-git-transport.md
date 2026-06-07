# Phase 86c - HTTPS/TLS + git smart-HTTP

**Status:** Planned
**Source Ref:** phase-86c
**Depends on:** Phase 86a (Outbound Foundation — CSPRNG, wall-clock floor, `ca-certificates` `.m3pkg`), Phase 86b (git build pattern + smoke template), Phase 77 (DNS reply delivery D.1 + outbound TCP `connect` D.2) ✅, Phase 85 (Cross-Compiled Toolchains) ✅
**Builds on:** Sub-phase 86c of the Phase 86 (Networking and GitHub) umbrella — see [`./86-networking-and-github.md`](./86-networking-and-github.md). It is the heavier of the two git-transport sub-phases: where 86b reused in-tree audited crypto and skipped X.509 entirely, 86c adds the full TLS 1.3 + X.509/CA/hostname stack and rebuilds the git binary with `NO_CURL` removed.
**Primary Components:** `ports/lib/mbedtls/Portfile` (new), `ports/util/curl/Portfile` (new), `ports/util/git/Portfile`, `xtask/src/port_build.rs` (`build_mbedtls`, `build_curl`, `build_git`), `xtask/src/main.rs` (`populate_ext2_files` `/etc/gitconfig`, `cmd_git_https_smoke`, `PORTS`), the Phase 86a `ca-certificates` `.m3pkg`, the Phase 85a `.m3pkg` pipeline + dependency solver

## Milestone Goal

`git clone https://github.com/<repo>` (and `fetch`/`push`) works from inside m3OS over real TLS: a static musl `git` rebuilt **with** curl (`NO_CURL` removed) against a static musl `libcurl --with-mbedtls`, validating GitHub's certificate chain and hostname against the SHA-256-pinned Phase 86a CA bundle, with Personal Access Token (PAT) credentials over HTTPS. Both a **positive** (successful authenticated clone) and a **negative** (expired / wrong-host / self-signed certificate is rejected) case are proven by a serial regression gate.

## Why This Phase Exists

86b delivered the cheapest first secure clone (git shells out to an `ssh` binary, reusing the in-tree `crypto-lib`/`sunset` crypto and skipping X.509). HTTPS is the heavier, footgun-laden transport that most real-world `git remote add origin https://…` URLs use, and it forces m3OS to confront the part SSH skipped: a TLS 1.3 client, an X.509 chain validator, a trust store on a canonical path, and hostname verification — the classic place where a green clone hides silently-broken certificate checking.

The two m3OS constraints pin every choice here:

- **SIMD is off** in the Rust userspace target (no SSE/AES-NI), which rules out the entire `ring`/`aws-lc-rs` ecosystem and therefore every mainstream Rust TLS stack. It does **not** rule out C TLS — mbedTLS is a small, static, musl-friendly TLS 1.3 client with full X.509 that drops into the existing `ports/` pipeline next to `zlib`, and `curl --with-mbedtls` is curl's documented small-footprint backend (curl 8.15 dropped BearSSL; mbedTLS is the small-footprint recommendation).
- **Certificate validity needs a trustworthy wall-clock.** This is a hard cross-phase dependency on Phase 86a Track B's build-date floor: with `BOOT_EPOCH_SECS = 0` (the pre-86a behavior on a bad RTC), every certificate is "not yet valid" and TLS fails-closed for the wrong reason. 86c cannot validate certs until 86a's floor lands.

The pure-Rust rustls path is explicitly **deferred**, not chosen-against-and-revisited: `rustls-rustcrypto` is marked do-not-use-in-production and std-leaning, `crypto-lib` (`userspace/crypto-lib/Cargo.toml:11-19`) ships no `p256`/`ecdsa` (GitHub's leaf is ECDSA P-256, live-confirmed), `Cargo.lock` carries no `rustls`/`webpki`/`p256`/`ecdsa`, and it would not serve the C git binary anyway. The missing-crates lift is recorded as the concrete reason.

## Learning Goals

- Why HTTPS git is a much larger trust surface than SSH — a full X.509 chain, a CA bundle, and hostname verification, versus SSH's `known_hosts` TOFU over audited in-tree crypto.
- How git smart-HTTP actually works: `GET info/refs?service=git-upload-pack` returns `application/x-git-upload-pack-advertisement` (a 5-byte pkt-line magic), then `POST git-upload-pack` returns a side-band-64k packfile result; a `text/plain` reply triggers the dumb-HTTP fallback m3OS does not support.
- Why hostname verification is **separate** from chain validation (`mbedtls_ssl_set_hostname` / curl `SSL_VERIFYHOST=2`) and why a negative certificate-rejection test is mandatory.
- Why the SIMD-off constraint forces a C TLS stack and a ChaCha20-Poly1305 cipher preference (no hardware AES-NI until Phase 86f → soft AES-GCM is slow and cache-timing-exposed; GitHub offers `TLS_CHACHA20_POLY1305_SHA256`).
- How a multi-package dependency chain (`zlib → mbedtls → curl → ca-certificates → git`) flows through the Phase 85a `.m3pkg` topological solver, including a data-only `ca-certificates` package.

## Feature Scope

### Area A — mbedTLS port

A new static musl `ports/lib/mbedtls` with a trimmed, client-only configuration: `MBEDTLS_SSL_CLI_C` on, `MBEDTLS_SSL_SRV_C` off, DTLS off, and just the cipher/X.509 surface GitHub needs — ChaCha20-Poly1305, ECDHE-ECDSA-P256, ECDHE-RSA, `MBEDTLS_X509_CRT_PARSE_C`, and `MBEDTLS_PEM_PARSE_C`. The trim keeps the library on the order of 45–300 KB. The CTR_DRBG entropy callback is wired to the Phase 86a `sys_getrandom` CSPRNG, **not** to any file-I/O entropy path.

### Area B — curl + git HTTPS rebuild

A new static musl `ports/util/curl` built `--with-mbedtls --with-ca-bundle=<86a path>` (HTTP/HTTPS only), plus a rebuild of git **with** curl that **inverts** the Phase 85b absence-assertions. The Phase 85b assertions HARD-FAIL the build if any curl/OpenSSL linkage is present; 86c reverses them to **require** `curl_easy_perform`/`SSL_CTX_new` and adds the curl dependency to git's `DEPS` (transitively pulling mbedtls + ca-certificates). The git Portfile's `DEPS` encodes the dependency-first order `zlib → mbedtls → curl → ca-certificates → git`. The server-side pack helpers (`git-upload-pack`/`git-receive-pack`/`git-upload-archive`) **stay pruned** — un-pruning them would be wrong, as they are the server half of the protocol.

### Area C — Cert validation, credentials, smoke, version

X.509 chain + hostname verification (CAINFO path agreement across git and curl), PAT credential handling (GitHub rejects password auth), a `git-https-smoke` gate with both a success and a rejected-bad-cert arm, and the `0.86.1 → 0.86.2` kernel version bump.

## Important Components and How They Work

### `build_mbedtls` in `port_build.rs`

A new port `build_*` function following the AGENTS.md musl-toolchain rules — `musl_toolchain()` (`xtask/src/port_build.rs:111`), `musl_extra_ldflags_joined()`, `--host=x86_64-linux-musl` where the build uses autotools/CMake configuration — registered in `PORTS` (`xtask/src/main.rs:17446`) and the `port_build` `match name` dispatch (`fn port_build` at `xtask/src/port_build.rs:773`, build dispatch at `:873`). It builds mbedTLS ≥3.6.1 static, with the trimmed client-only `mbedtls_config.h`, and overrides the entropy source so CTR_DRBG seeds from `sys_getrandom`. mbedTLS 3.5+ ships p256-m, the tiny constant-time P-256 implementation, which validates GitHub's ECDSA leaf without any assembly.

### `build_curl` in `port_build.rs`

A new `build_curl` that links the staged mbedtls and zlib. `--with-mbedtls` selects the small-footprint TLS backend; `--with-ca-bundle=/etc/ssl/certs/ca-certificates.crt` makes curl's compiled-in default CAINFO match the Phase 86a path that git also uses. Disabling the unused protocols keeps the binary small (HTTP/HTTPS only). curl is what git's smart-HTTP transport shells the bytes through.

### `build_git` rebuild + inverted assertions in `port_build.rs`

`build_git` (`xtask/src/port_build.rs:1427`) currently sets `NO_CURL=1`/`NO_OPENSSL=1` (lines 1458–1459) and **hard-fails** if any curl helper is present — the curl-helper-present check (`~line 1566`), then `curl_easy_perform` (1574) and `SSL_CTX_new` (1580) symbol checks. 86c removes `NO_CURL` and inverts those into **presence** requirements: `git-remote-https`/`git-http-fetch` must exist, and the binary must contain `curl_easy_perform`/`SSL_CTX_new`. Critically, **the inversion and the curl `DEPS` addition must land together** — otherwise `build_git` hard-fails the moment curl helpers appear. The server-side pack-helper prune (`~line 1535`, the `bin/git-upload-pack`/`git-receive-pack`/`git-upload-archive` removal) is **correct and stays**; a doc note records that un-pruning them would expose a server m3OS does not run.

### `/etc/gitconfig` + credentials (xtask staging)

The bundled `/etc/gitconfig` is staged by `populate_ext2_files` (`xtask/src/main.rs:15586`, gitconfig block at `15625`). 86c extends it with `http.sslVerify=true`, `http.sslCAInfo=/etc/ssl/certs/ca-certificates.crt` (matching `GIT_SSL_CAINFO` and curl's `--with-ca-bundle`), and the PAT credential mechanism (`credential.helper store` reading `~/.git-credentials`, or `~/.netrc`). GitHub rejects password auth, so the credential is a PAT used as the password; tokens are kept out of serial output.

### `cmd_git_https_smoke` (xtask gate)

A new serial gate modeled on `cmd_git_local_smoke` (`xtask/src/main.rs:13584`). It has two mandatory arms: (1) a positive clone whose `info/refs` response is validated by the `application/x-git-upload-pack-advertisement` Content-Type plus the 5-byte pkt-line magic; (2) a negative arm where an expired / wrong-host / self-signed certificate is **rejected**. The negative arm needs no secret and is the proof that verification is not silently disabled.

## How This Builds on Earlier Phases

- Consumes the **Phase 86a** CSPRNG (CTR_DRBG entropy), the **build-date wall-clock floor** (cert validity — a hard blocker), and the SHA-256-pinned **`ca-certificates` `.m3pkg`** on the canonical `/etc/ssl/certs/ca-certificates.crt` path.
- Reuses the **Phase 86b** git build pattern and serial smoke-gate template; 86b proved the outbound path over SSH, 86c adds the HTTPS arm.
- Reuses **Phase 77**'s DNS A-record resolution (`sys_recvmsg_inet`) and outbound TCP `connect` (`sys_connect` → `tcp::connect`) — the transport curl rides on.
- Rides the **Phase 85a** `.m3pkg` pipeline + offline dependency solver, extended to a five-package topological chain including a data-only package.
- Reuses **Phase 85b**'s `ports/lib/zlib` (packfile inflate) and the git port scaffolding, inverting only its `NO_CURL` posture.

## Implementation Outline

1. Add `ports/lib/mbedtls/Portfile` (pinned ≥3.6.1 + SHA-256) and `build_mbedtls` with the trimmed client-only config + the `sys_getrandom` CTR_DRBG entropy callback; register in `PORTS` + dispatch.
2. Add `ports/util/curl/Portfile` and `build_curl` (`--with-mbedtls --with-ca-bundle`, HTTP/HTTPS only) linking staged mbedtls + zlib.
3. Rebuild git with curl: remove `NO_CURL`, invert the absence-assertions to presence-requirements, and add `curl` to git's `DEPS` — all in one change so `build_git` never hard-fails mid-flight; keep the server-side pack-helper prune.
4. Extend `/etc/gitconfig` with `sslVerify`/`sslCAInfo` + PAT credentials; write the trust-model doc section.
5. Add `cmd_git_https_smoke` with a positive clone arm and a negative cert-rejection arm; wire the opt-in pre-push regression.
6. Bump `kernel/Cargo.toml` `0.86.1 → 0.86.2`; `cargo xtask check` clean; banner reports `0.86.2`.

## Acceptance Criteria

- `build_mbedtls` produces a static mbedTLS ≥3.6.1 with `MBEDTLS_SSL_CLI_C` on / `MBEDTLS_SSL_SRV_C` off, ChaCha20-Poly1305 + ECDHE-ECDSA-P256 + ECDHE-RSA + `MBEDTLS_X509_CRT_PARSE_C` + `MBEDTLS_PEM_PARSE_C` on, DTLS off, and its CTR_DRBG entropy callback bound to `sys_getrandom` (no file-I/O entropy path).
- `build_curl` produces a static `libcurl --with-mbedtls --with-ca-bundle=/etc/ssl/certs/ca-certificates.crt` (HTTP/HTTPS only); an HTTPS GET to a controllable endpoint succeeds with SNI + `SSL_VERIFYHOST=2`.
- A curl-enabled git variant has `git-remote-https` + `git-http-fetch` present; the inverted assertions **require** `curl_easy_perform`/`SSL_CTX_new`; git's `DEPS` gains `curl` (transitively mbedtls + ca-certificates) in the topo order `zlib → mbedtls → curl → ca-certificates → git`; the server-side pack helpers remain pruned.
- `/etc/gitconfig` sets `http.sslVerify=true` and `http.sslCAInfo=/etc/ssl/certs/ca-certificates.crt`; the PAT mechanism is configured and documented; tokens are redacted from serial.
- `git-https-smoke` (env `M3OS_GIT_HTTPS_REGRESSION=1`): (1) a clone succeeds with `info/refs` validated by `application/x-git-upload-pack-advertisement` Content-Type + 5-byte pkt-line magic; (2) expired / wrong-host / self-signed certificates are **rejected**.
- `kernel/Cargo.toml` reads `0.86.2`; `cargo xtask check` is clean; the boot banner reports `0.86.2`.

## Companion Task List

- [Phase 86c Task List](./tasks/86c-https-git-transport-tasks.md)

## How Real OS Implementations Differ

- **git smart-HTTP, fully.** Mature systems support both smart-HTTP (`GET info/refs?service=git-upload-pack` → `application/x-git-upload-pack-advertisement` 5-byte magic; `POST git-upload-pack` → side-band-64k result) and the legacy **dumb** HTTP fallback (`text/plain` advertisement, `git-http-fetch` walking loose objects). 86c supports only smart-HTTP; the dumb path's `git-http-fetch` is the pruned half.
- **GitHub TLS, live-probed.** GitHub serves TLS 1.3 with an ECDSA P-256 leaf chaining Sectigo → USERTrust ECC, offers `TLS_CHACHA20_POLY1305_SHA256`, and **requires SNI**. Without hardware AES-NI, m3OS prefers ChaCha20-Poly1305 over soft AES-GCM — the opposite default of an AES-NI host.
- **Backends.** curl 8.15 dropped BearSSL; distributions ship curl against OpenSSL/GnuTLS, while m3OS takes the small-footprint mbedTLS route (the same C-TLS pattern Redox uses with C OpenSSL).
- **Credentials.** GitHub rejects password-over-HTTPS; auth is a PAT via `credential.helper store` / `~/.netrc` / `GIT_ASKPASS`, with `GIT_SSL_CAINFO` pointing at the trust store — the model 86c adopts.
- **Revocation.** Production stacks do OCSP/CRL revocation, session resumption/tickets, and IPv6/dual-stack; 86c defers all of those.

## Deferred Until Later

- A pure-Rust rustls + RustCrypto TLS path — explicitly deferred; the blocking lift is the missing `p256`/`ecdsa`/`rustls`/`webpki` crates (none in `Cargo.lock`) plus `rustls-rustcrypto`'s do-not-use-in-production + std-leaning posture, and it would not serve the C git binary regardless.
- The legacy dumb-HTTP transport (`git-http-fetch` over loose objects) — only smart-HTTP is supported.
- TLS revocation (OCSP/CRL), session resumption/tickets, and client certificates.
- IPv6 / AAAA HTTPS endpoints (the stack is IPv4 / A-record only — Phase 89).
- Hardware-AES-NI-accelerated TLS crypto (Phase 86f); 86c runs on software ChaCha20-Poly1305.
- Networked `pkg install` over HTTPS + ed25519 package signing (unblocked by 86c, tracked separately).
