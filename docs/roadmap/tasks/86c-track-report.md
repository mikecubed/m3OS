# Phase 86c — Implementation Track Report

**Workflow:** `/flow:parallel-impl`
**Integration branch:** `feat/phase-86c-https-git-transport`
**Base / review target:** `main`
**PR:** [#229](https://github.com/mikecubed/m3OS/pull/229)
**Concurrency cap:** 2 (`.flow/defaults.json` absent → default)
**Models:** implementer = `claude-sonnet-4.6`, reviewer = `claude-opus-4.7` (per `.claude/models.yaml`); coordinator/implementer = Opus 4.8 (this session)
**Max revision rounds per track:** 2

## Discovery brief

- **Task shape:** multi-track-batch (Tracks A/B/C from `86c-https-git-transport-tasks.md`), but with a **strict A→B→C dependency pipeline** and **heavy shared-file concentration** (all three tracks edit `xtask/src/port_build.rs` and/or `xtask/src/main.rs`).
- **Scout:** skipped — the task + design docs are a fully-scoped brief (exact files, symbols, acceptance). The coordinator read every task site directly and ran one `Explore` fan-out to map the large `main.rs` smoke-gate + pkg-bundling plumbing.
- **Environment:** musl cross-compiler present (`/usr/bin/x86_64-linux-musl-gcc`, verified to produce working static no-PT_INTERP binaries); cmake + ninja present; host has HTTPS egress (downloaded the mbedTLS + curl tarballs and reaches `github.com`). `getrandom(2)` (syscall 318) works on the host, so the mbedTLS entropy self-test exercises the real shim.
- **Validation commands (coordinator-owned):**
  - `cargo xtask port build mbedtls` / `curl` / `git` (the `zlib → mbedtls → ca-certificates → curl → git` chain — static seals + cache-key self-invalidation)
  - `cargo xtask check` (clippy -D warnings + rustfmt + 158 host tests + retpoline gate)
  - `cargo test -p xtask` (the `port_deps` + recipe-id distinctness tests)
  - `M3OS_GIT_HTTPS_NET=1 cargo xtask git-https-smoke --timeout 5400` (boot + 27 MB chain install + on-device curl+mbedTLS + bad-cert reject + live HTTPS clone)

## Orchestration decision — serialized, not parallel worktrees

The three tracks are **not independent** in the sense `parallel-impl` requires: Track B can't start until Track A's mbedTLS archives stage, Track C can't wire the smoke until Track B's git links curl, and **A/B/C all edit the same two files** (`xtask/src/port_build.rs`'s `build_*`/`build_recipe_id`/`port_deps`/`compute_port_key`, and `xtask/src/main.rs`'s gitconfig/bundling/smoke-gate). Parallel worktrees would conflict on every commit. Per the skill's **Core Rule 1 ("serialize anything that touches the same tight code region")**, the work was implemented **serially by the coordinator** on one feature branch, committing + pushing per track. **Review separation was preserved** via independent `code-quality-reviewer` agents launched on each completed track's diff (skill Step 6), keeping implementation and review judgment distinct.

## Tracks

| Track | Tasks | Owned files | Commit | State |
|---|---|---|---|---|
| A | A.1 + A.2 (mbedTLS port + CSPRNG entropy) | `ports/lib/mbedtls/Portfile` (new), `xtask/src/port_build.rs` (`build_mbedtls`, entropy shim + self-test, `mbedtls_config_enabled`, recipe-id/deps/dispatch) | `926d282` | ✅ done |
| B.1 | B.1 (curl port) | `ports/util/curl/Portfile` (new), `xtask/src/port_build.rs` (`build_curl`, `curl_static_link_line`, recipe-id/deps/dispatch) | `cb5eee6` | ✅ done |
| B.2 | B.2 (git HTTPS rebuild) | `xtask/src/port_build.rs` (`build_git` invert + curl link, `compute_port_key` recursion), `ports/util/git/Portfile` (DEPS) | `cb5eee6` | ✅ done |
| C.1 | C.1 (gitconfig trust + PAT + bundling + trust-model doc) | `xtask/src/main.rs` (`/etc/gitconfig`, `BUNDLE_ONLY_PORTS`), `docs/roadmap/86c-https-git-transport.md` (Trust Model), `port_build.rs` (curl→ca-certificates dep, `build_git_port` chain) | `b767668` | ✅ done |
| C.2 | C.2 (`git-https-smoke` gate) | `xtask/src/main.rs` (`cmd_git_https_smoke`, `git_https_smoke_steps`, exit code 78, CLI/usage), `AGENTS.md` (gate row), `.githooks/pre-push` (gate + raised git-local/ssh timeouts) | `b767668` | ✅ done |
| C.3 | C.3 (version) | `kernel/Cargo.toml`, `Cargo.lock`, `AGENTS.md` | `b767668` | ✅ done |

## Key engineering decisions

- **mbedTLS config via `scripts/config.py`, not a hand-written header.** Starting from the shipped default (a known-good, `check_config`-passing config) and `unset`ting the server/DTLS/NET surface + `set`ting the two entropy macros guarantees self-consistency and keeps every listed cipher/X.509 requirement satisfied. The trim is verified-by-construction at build time (on/off assertions on the installed `mbedtls_config.h`). The **same** in-place-edited header is installed so curl/git compile against the exact config the archives were built with (no struct-size ABI skew — guarded again in `build_curl`).
- **Entropy is `sys_getrandom`-only, verified by construction.** `MBEDTLS_ENTROPY_HARDWARE_ALT` + a `mbedtls_hardware_poll` shim (`getrandom(2)` = syscall 318) + `MBEDTLS_NO_PLATFORM_ENTROPY`. A build-time C self-test (linked against the shipped shim object, run on the host where syscall 318 also = getrandom) asserts full-length + non-constant draws; `binary_contains(libmbedcrypto.a, "/dev/urandom") == false` proves no file-I/O path linked.
- **`NO_OPENSSL` stays; the literal task text was corrected.** The task asked to "require `SSL_CTX_new`", but the chosen backend is mbedTLS-via-curl, so that OpenSSL symbol is intentionally **absent**. The positive TLS proof is `mbedtls_ssl_handshake` (present) + `curl_multi_perform` (present), with `SSL_CTX_new` asserted **absent** — documented in the task doc.
- **The symbol check targets the helper, not `bin/git`.** With `SKIP_DASHED_BUILT_INS`, curl is linked only into `libexec/git-core/git-remote-http`, never the monolithic `git` binary; `build_git` runs its assertions on the unstripped staged helper (before `seal_package` strips).
- **`compute_port_key` made recursive.** Phase 86c introduces the first transitive dep chain (`git → curl → {zlib, mbedtls, ca-certificates}`); the key computation now folds in each dep's full transitive identity. Backward-compatible: for the pre-86c leaf deps the recursion is byte-identical, so warm caches stay valid.
- **`ca-certificates` is curl's runtime dep.** The CA bundle (the trust store) must be installed at runtime, so curl `DEPS` lists `ca-certificates`; the solver installs `zlib → mbedtls → ca-certificates → curl → git` from `/usr/pkg`.
- **Fully-static curl CLI needs libtool `-all-static`.** A bare `-static` only selects static *libtool* libraries; `-all-static` (passed only at `make`, since gcc rejects it in configure link probes) produces the fully-static binary.

## Review + rescue history

- **Track A reviewer (`code-quality-reviewer`): no blockers.** Applied the two cheap correctness items — entropy shim fails closed on `getrandom()==0` (no infinite spin) and idempotent mbedTLS header install (remove-before-`cp` + double-nest guard). The "mock the short-read loop" suggestion was addressed by hardening the C shim itself rather than testing a Rust mirror of C code (documented).
- **Track B reviewer (`code-quality-reviewer`): one flagged item, resolved.** The reviewer flagged the `curl_easy_perform` presence assertion. Root cause was subtler than reported (the assertion runs pre-strip, so it actually passed), but the underlying point was valid — git 2.44 never *calls* `curl_easy_perform` (it uses the curl **multi** interface), so it was only a symtab artifact. Switched to `curl_multi_perform` (a symbol git actually calls — strip-robust, semantically meaningful). Re-verified: the `git` rebuild asserts `curl_multi_perform + mbedtls_ssl_handshake present, SSL_CTX_new absent`. The reviewer separately confirmed the static link order, the recursion's acyclicity + warm-cache preservation, the libtool `-all-static` idiom, and the curl configure flag set are all sound.
- **Track C / integration reviewer (`code-quality-reviewer`): no blockers, 1 major + minors, all applied.** M1 — corrected the now-false "git reused UNCHANGED / not rebuilt" claims in `cmd_git_ssh_smoke` + AGENTS.md + the git Portfile (as of 86c `build_git_port` builds the HTTPS-capable git; the SSH transport is orthogonal). m1 — corrected the install topo order to `zlib → mbedtls → ca-certificates → curl → git` everywhere. m2 — curl Portfile `DEPS` gains `ca-certificates`. m4 — raised the git-local-smoke pre-push global to 2400 (> the 1800 install-step ceiling). The reviewer verified the echo-match safety, solver/bundling coherence, the version bump, and the gitconfig/shell-quoting were all sound. (m3 — bare no-`--timeout` invocation clamps to the 240s default — left as-is: every wired invocation passes an explicit `--timeout`.)
- **No rescues.** All background tasks (three port-chain builds, three reviewers, two end-to-end smoke runs) ran to completion; no stalls, nudges, or replacements. The first smoke run surfaced the `head -2`/`-c` bug, which was fixed and the rerun passed all 36 steps.

## Validation outcomes

- `cargo xtask check` — clippy clean, rustfmt, **158 xtask host tests**, retpoline gate **PASS** (re-run by the pre-commit hook on each of the three commits). ✅
- Port chain — `zlib → mbedtls → ca-certificates → curl → git` builds clean with the new content keys:
  - **mbedTLS:** static `libmbedcrypto.a`/`x509.a`/`tls.a`; entropy self-test `ENTROPY_OK olen=32`; no `/dev/urandom` in the archive. ✅
  - **curl:** static `libcurl.a` + static CLI; `curl 8.15.0 … libcurl/8.15.0 mbedTLS/3.6.2 zlib/1.3.1`; embedded CAINFO `/etc/ssl/certs/ca-certificates.crt`. ✅
  - **git:** `git-remote-https + curl_multi_perform + mbedtls_ssl_handshake present, SSL_CTX_new absent, server pack helpers pruned`. ✅
- `git-https-smoke` (`M3OS_GIT_HTTPS_NET=1 --timeout 5400`) — **PASSED, 36/36 steps in 904s** (full end-to-end on m3OS, including the live network arms). On-device proof:
  - `pkg install git: OK` — the solver resolved + installed the whole `zlib → mbedtls → ca-certificates → curl → git` chain (~27 MB) from `/usr/pkg`.
  - `curl 8.15.0 (x86_64-pc-linux-musl) libcurl/8.15.0 mbedTLS/3.6.2 zlib/1.3.1` — the static cross-built curl+mbedTLS **runs on m3OS**.
  - `http.sslCAInfo` = `/etc/ssl/certs/ca-certificates.crt`, `http.sslVerify` = `true`, and the installed CA bundle's `Bundle of CA Root Certificates` header is on disk.
  - **NEGATIVE arm:** `git clone https://self-signed.badssl.com/...` was **REJECTED** (TLS failed closed at chain validation — `certificate`/`Certificate` matched).
  - **POSITIVE arm:** the `info/refs` response Content-Type was `application/x-git-upload-pack-advertisement` and its pkt-line body began `# service=git-upload-pack`; then a real `git clone https://github.com/octocat/Hello-World.git` reached `Receiving objects` and checked out HEAD (`Hello World` from the README).
  - **Bug caught by the gate (then fixed):** m3OS's `head` supports only `-n N`; the original `head -2` / `head -c 30` printed a usage error and timed the gate out. Fixed to `head -n 2` / `head -n 1` (commit `89368be`). A clean demonstration that the on-device gate exercises real m3OS tooling, not just the build.

## Workflow outcome measures

- **discovery-reuse:** N/A (single coordinator; no separate scout brief consumed by sub-agents).
- **rescue-attempts:** 0.
- **abandonment-events:** 0.
- **re-review-loops:** 0 per track (each track reviewed once; findings applied without a second review round).
