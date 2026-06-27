# Phase 107 — Networked & Signed Package Distribution: Task List

**Status:** Planned
**Source Ref:** phase-107
**Depends on:** Phase 85a (`.m3pkg` format + offline `pkg` installer + `DEPS` solver) ✅, Phase 86c (HTTPS/TLS via `curl`+mbedTLS) ✅, Phase 42 (`crypto-lib` ed25519) ✅
**Goal:** Distribute prebuilt `.m3pkg` artifacts over the network so the on-device installer can fetch + verify + install them, hosted for $0 on GitHub (Releases as the blob store + a tiny ed25519-signed `index.m3idx`, published by a GitHub Actions pipeline). Add a network fetch + a signed index on top of the Phase 85a offline `pkg` engine — the solve/verify/extract/DB engine is reused 100 %; the only new code is download + index-parse + ed25519 verify. Reuse the Phase 86c on-device `curl`/mbedTLS via the `fork`/`execve`/`waitpid` spawn seam (mbedTLS is **not** linked into the installer).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Signed repo index `index.m3idx` format (new `pkg-format::index` module) + ed25519 trust model + baked-in public key + `repos.conf` | Phase 85a, Phase 42 | Planned |
| B | On-device networked `pkg` verbs — `pkg update` (curl fetch + ed25519-verify) + networked `pkg install` (resolve from index → fetch → SHA-256-check → unchanged `install_one`) + curl spawn seam | A, Phase 86c | Planned |
| C | CI publish pipeline — new `cargo xtask repo-index` host tool + `build-and-publish.yml` (cache `pkgcache/`, `port build all`, emit+sign index, `gh release upload`) in a public `m3os-pkgs` repo | A | Planned |
| D | Validation — host tests (index/sig/solver) + `pkg-net-smoke` gate (SLIRP-served CI-deterministic core + opt-in live HTTPS arm) + `AGENTS.md`/README docs | A, B, C | Planned |

---

## Track A — Signed Repo Index + Trust Model

### A.1 — `index.m3idx` format module

**File:** `pkg-format/src/lib.rs` (new `pub mod index`)
**Symbol:** `index::IndexEntry`, `index::serialize_index`, `index::parse_index`
**Why it matters:** The index is the single trust root — both the host publisher (`xtask repo-index`) and the on-device client (`pkg`) must parse/serialize it identically, and signing requires a **deterministic** byte serialization, so this format has exactly one shared implementation. It also keeps the `.m3pkg` byte layout untouched (the signature lives on the index, not in each package header).

**Acceptance:**
- [ ] `IndexEntry { name, version, key, size, sha256, url, deps: Vec<String> }` defined; APKINDEX-style flat text with single-letter tags (`N:`/`V:`/`K:`/`S:`/`C:`/`U:`/`D:`) and a header (`M:m3idx1`, `A:x86_64`), records separated by blank lines.
- [ ] `serialize_index` emits records **sorted by name** so output is byte-deterministic for a given entry set.
- [ ] `parse_index` round-trips `serialize_index` output exactly; an unknown future tag line is ignored (forward-compat), a missing required field errors.
- [ ] Host test in `cargo xtask check`: a multi-record round-trip + a determinism assertion (re-serializing a reordered input yields identical bytes).

### A.2 — Index canonical signing bytes + blob hashing

**File:** `pkg-format/src/lib.rs` (`index` module)
**Symbol:** the `C:`-field hashing path reusing `pkg_format::sha256::digest` + `to_hex`
**Why it matters:** Each record's `C:` field is the SHA-256 of the **whole `.m3pkg` blob** — this is the value the on-device client checks a fetched blob against, so package authenticity flows from the signed index. Reusing the existing in-crate `sha256` keeps a single hash implementation (the one already pinned by `content_hash_matches_known_sha256`).

**Acceptance:**
- [ ] The bytes signed/verified are exactly the `serialize_index` output (no separate canonicalization step); documented in the module header.
- [ ] `C:` is computed with `pkg_format::sha256::digest` over the blob bytes and rendered with `to_hex` (lowercase hex, 64 chars).
- [ ] Host test: an entry's `C:` for a known blob matches the standalone `pkg_format::content_hash` of the same bytes.

### A.3 — Baked-in public key + `repos.conf` on the image

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files`)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS` if a config gate is needed)
- new in-repo committed key file (e.g. `pkg-format/keys/m3os-pkgs.pub` or `ports/keys/`)

**Symbol:** image-staged `/etc/pkg/keys/m3os-pkgs.pub` (32 raw bytes) + `/etc/pkg/repos.conf`
**Why it matters:** The public key is the trust anchor; it must be committed in-repo (auditable) **and** baked into the image so `pkg update` can verify offline-of-the-repo. `repos.conf` is where the client learns the base URL(s) to fetch from.

**Acceptance:**
- [ ] `/etc/pkg/keys/m3os-pkgs.pub` (the 32-byte ed25519 public key) is staged into the ext2 image; the same key is committed in-repo.
- [ ] `/etc/pkg/repos.conf` is staged with one base URL per line (`#` comments), defaulting to the published GitHub Releases / gh-pages base for the `repo/x86_64` rolling tag.
- [ ] `cargo xtask clean` + rebuild places both files; a boot `cat /etc/pkg/repos.conf` shows the configured base URL.

---

## Track B — On-Device Networked `pkg` Verbs

### B.1 — `curl` spawn seam (`fetch_url`)

**File:** `userspace/pkg/src/main.rs`
**Symbol:** `fetch_url` (new) over `syscall_lib::{fork, execve, waitpid}`
**Why it matters:** The `no_std` installer must reach HTTPS **without linking mbedTLS** (it would not codegen on the soft-float target and would bloat a kernel-adjacent binary). Spawning the existing Phase 86c `curl` over the `fork`/`execve`/`waitpid` boundary (`syscall-lib/src/lib.rs:1939/1945/1970`) is the same seam `git` uses.

**Acceptance:**
- [ ] `fetch_url(url: &str, dest: &[u8]) -> bool` builds argv `["curl", "-fsSL", "-o", dest, url]`, `fork()`s, `execve()`s `/usr/bin/curl` in the child, and `waitpid()`s in the parent; returns `true` only on child exit status 0.
- [ ] No mbedTLS / TLS symbols are linked into the `pkg` binary (verified by an `nm`/build-deps check — `pkg`'s dependency set is unchanged except `crypto-lib`).
- [ ] A failed fetch (non-zero curl exit, e.g. 404/connection refused) returns `false` and surfaces an actionable error, not a hang.

### B.2 — `pkg update` (fetch + ed25519-verify the index)

**File:** `userspace/pkg/src/main.rs`
**Symbol:** `cmd_update` (new), `crypto_lib::asymmetric::ed25519_verify`
**Why it matters:** `pkg update` is the act that establishes trust — it pulls the index + detached signature and verifies the signature against the baked-in key before any package is ever fetched. Fail-closed (keep the old index on a bad signature) is the security-critical behavior.

**Acceptance:**
- [ ] `pkg update` reads base URLs from `/etc/pkg/repos.conf`, `fetch_url`s `<base>/index.m3idx` and `<base>/index.m3idx.sig` to a temp path.
- [ ] Loads `/etc/pkg/keys/m3os-pkgs.pub` (32 bytes) → `ed25519_verifying_key_from_bytes`, reads the 64-byte detached sig, and calls `ed25519_verify(&vk, &index_bytes, &sig)`.
- [ ] On verify success: the index is cached to `/var/lib/pkg/index.m3idx` and `pkg update` reports the record count.
- [ ] On verify failure (flipped byte in index **or** sig): the fetched index is **discarded**, the previously cached `/var/lib/pkg/index.m3idx` is retained, a clear rejection is printed, and exit code is non-zero.

### B.3 — Networked `pkg install` (resolve → fetch → SHA-check → install)

**Files:**
- `userspace/pkg/src/main.rs` (`cmd_install` networked branch)
- `userspace/pkg/src/lib.rs` (reused `topo_install_order`, `parse_meta`/index dep map)

**Symbol:** `cmd_install` (extended), reused `topo_install_order` (`lib.rs:330`), reused `install_one` (`main.rs:199`)
**Why it matters:** This is the payoff — and it must reuse the offline engine unchanged. Resolution comes from the **signed index's `D:` fields**, the blob lands at exactly the path `install_one` already reads (`/usr/pkg/<name>.m3pkg`), and the only new safety step is the SHA-256 check against the trusted index before extraction.

**Acceptance:**
- [ ] When a cached trusted index exists, `cmd_install` builds the dep map from index `D:` fields and runs the **unchanged** `topo_install_order`, omitting already-installed packages (`/var/lib/pkg/db`).
- [ ] For each name in dependency-first order: look up its index record, `fetch_url` `<base>/<key>.m3pkg` → `/usr/pkg/<name>.m3pkg`, compute SHA-256 of the fetched bytes, and compare to the index `C:` field.
- [ ] On SHA-256 match: call the **unchanged** `install_one`; `pkg list` afterward shows the package + its deps.
- [ ] On SHA-256 mismatch: the blob is rejected **before** extraction, the file is removed, install aborts non-zero.
- [ ] With no network / no cached index, `cmd_install` falls back to the existing offline `/usr/pkg/<name>.m3pkg` path (no regression to the Phase 85a `pkg-smoke` behavior).

### B.4 — `repos.conf` parser

**File:** `userspace/pkg/src/lib.rs`
**Symbol:** `parse_repos_conf` (new, host-tested pure logic)
**Why it matters:** Keeping the config parse in the host-testable `lib.rs` (like `parse_meta`/`db_parse`) means the multi-line, comment-handling logic is covered by `cargo xtask check` rather than only exercised in QEMU.

**Acceptance:**
- [ ] `parse_repos_conf(&str) -> Vec<String>` returns base URLs in file order, skipping blank lines and `#` comments, trimming whitespace.
- [ ] Host test covers comments, blank lines, and trailing-slash normalization (so `<base>/index.m3idx` is well-formed).

---

## Track C — CI Publish Pipeline

### C.1 — `cargo xtask repo-index` host tool

**Files:**
- `xtask/src/main.rs` (dispatch arm next to `Some("port") => …` at `:1417`; new `cmd_repo_index`)
- `xtask/src/port_build.rs` (reused `pkgcache_artifact_path` `:1110`, `compute_package_key`)

**Symbol:** `cmd_repo_index`
**Why it matters:** This is the host side of the trust model — it turns the content-addressed `target/pkgcache/*.m3pkg` set into a signed `index.m3idx`. Sharing `pkg_format::index::serialize_index` with the client guarantees byte-identical format.

**Acceptance:**
- [ ] `cargo xtask repo-index` walks `target/pkgcache/*.m3pkg`, deriving each entry's `K:`/`U:<key>.m3pkg` from the content key and `N:`/`V:`/`D:` from the port Portfile/`.meta`, and `S:`/`C:` from the blob bytes.
- [ ] Calls `pkg_format::index::serialize_index`, then signs the bytes with an ed25519 key read from `M3OS_PKG_SIGNING_KEY` (path or hex seed) via `crypto_lib::asymmetric::ed25519_sign`, writing `index.m3idx` + `index.m3idx.sig`.
- [ ] Absent `M3OS_PKG_SIGNING_KEY`, emits an **unsigned** index with a warning (local dry-run), never silently signs with a default key.
- [ ] Host test: emit → `parse_index` round-trip + `ed25519_verify` of the produced signature with the matching public key passes.

### C.2 — `build-and-publish.yml` (the m3os-pkgs CI flow)

**File:** new `.github/workflows/build-and-publish.yml` (in the separate public `m3os-pkgs` repo)
**Symbol:** the workflow
**Why it matters:** This is what makes distribution **$0** and incremental — `actions/cache` of the content-addressed `target/pkgcache/` means only changed-key ports rebuild, and GitHub Releases is the free, uncapped blob store.

**Acceptance:**
- [ ] Restores `actions/cache` keyed on `target/pkgcache/` so unchanged-key ports are not rebuilt or re-uploaded.
- [ ] Runs `cargo xtask port build all` then `cargo xtask repo-index` with the ed25519 private key from a GitHub Actions **secret** (never echoed to logs).
- [ ] `gh release upload --clobber repo/x86_64 <key>.m3pkg … index.m3idx index.m3idx.sig` pushes new/changed assets to the rolling per-arch tag; an optional `gh-pages` step mirrors the index for a stable URL.
- [ ] Documented: the secret name, the rolling-tag convention, and the public-repo $0 hosting rationale (vs git-LFS metered / GHCR escape hatch).

---

## Track D — Validation

### D.1 — Host tests (CI-deterministic)

**Files:**
- `pkg-format/src/lib.rs` (index round-trip + tamper)
- `userspace/pkg/src/lib.rs` (`topo_install_order` from index deps, `parse_repos_conf`)
- `userspace/crypto-lib/src/asymmetric.rs` (existing `test_ed25519_tampered_message`)

**Symbol:** the `#[test]` modules
**Why it matters:** The index-parse + sig-verify + solver are the CI-deterministic core the spec mandates — they must be proven without QEMU or network so a regression is caught in `cargo xtask check`.

**Acceptance:**
- [ ] Index serialize/parse round-trip + determinism test passes (`pkg-format`).
- [ ] An **index-level tamper test**: a one-byte mutation of a signed index's bytes makes `ed25519_verify` return `false` (built on the existing `crypto-lib` tamper coverage).
- [ ] `topo_install_order` fed a dep map built from index `D:` fields produces the same dependency-first order as the Phase 85a `.meta`-fed path (no solver change).
- [ ] All run under `cargo xtask check` (the existing `pkg-format` / `pkg` / `crypto-lib` host-test set).

### D.2 — `pkg-net-smoke` gate

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_pkg_net_smoke` (new) + `M3OS_PKG_NET_REGRESSION`
**Why it matters:** Proves the whole on-device chain — `pkg update` verify, tamper-reject, networked `pkg install`, bad-blob-reject — end to end, deterministically in CI (no real internet) via a SLIRP-served host index, with the real-HTTPS arm opt-in.

**Acceptance:**
- [ ] **CI-deterministic core:** the gate freshly signs an `index.m3idx` over a couple of small `.m3pkg` blobs (e.g. `libevent` + a leaf), serves them from a **host HTTP server over SLIRP** (the `10.0.2.100:80` guestfwd pattern used by `node-smoke`/`go-runtime-smoke`), points `repos.conf` at it, boots, and asserts: `pkg update` verifies (sentinel `PKG_UPDATE_OK <n>`), a **served tampered index is rejected** (`PKG_UPDATE_REJECT`), `pkg install <leaf>` resolves+fetches+SHA-checks+installs (`pkg list` shows it), and a **served bad-blob (wrong bytes) is rejected** before extraction (`PKG_BLOB_REJECT`).
- [ ] Fails fast via `WaitPassOrFail` on any `:FAIL` / a silent-accept of a tampered index or bad blob.
- [ ] **Opt-in live arm** `M3OS_PKG_NET=1`: fetch + verify against the real published GitHub Releases / gh-pages URL; **skip-with-reason** when unset (mirroring `git-https-smoke`'s `M3OS_GIT_HTTPS_NET`).
- [ ] Runs at a timeout sized for the cold install over the slow VFS (`--timeout 900`+).

### D.3 — Gate + roadmap documentation

**Files:**
- `AGENTS.md` (pre-push opt-in gate table)
- `docs/roadmap/README.md` (Phase 107 row + mermaid node)

**Symbol:** the `M3OS_PKG_NET_REGRESSION` row; the Phase 107 summary row
**Why it matters:** Keeps the gate discoverable and the roadmap accurate per the documentation policy.

**Acceptance:**
- [ ] `M3OS_PKG_NET_REGRESSION=1` row added to the `AGENTS.md` gate table with the same skip-vs-pass wording as the `git-https-smoke` row (CI-deterministic core always-on; live HTTPS arm opt-in via `M3OS_PKG_NET`).
- [ ] `docs/roadmap/README.md` has the Phase 107 table row and a mermaid node depending on Phase 85a + 86c + 42 (`P85a --> P107`, `P86c --> P107`, `P42 --> P107`).

---

## Documentation Notes

- The Phase 85a engine is reused **unchanged** — `topo_install_order`, `install_one`, `pkg_format::{parse,verify}`, and the `/var/lib/pkg/db` layer. The only new on-device code is `fetch_url` (curl spawn), `cmd_update`, the networked `cmd_install` branch, `parse_repos_conf`, and the ed25519 verify call. Record that no offline behavior regressed (Phase 85a `pkg-smoke` stays green).
- The trust signature lives on the **index**, not in each `.m3pkg` header — the reserved `pkg_format` `SIGNATURE_LEN`/`Manifest.signature` field stays zeroed and the `.m3pkg` byte layout is untouched. This realizes the 85a Track E.2 "reserved ed25519 field … networked Phase 86 install" design intent via the Alpine/apt sign-the-index model.
- mbedTLS is **never linked** into `pkg`; TLS is entirely inside the spawned Phase 86c `curl`. Keep this property asserted (B.1) so a future refactor cannot quietly pull a TLS stack into the installer.
- `pkg-format::index` is the single shared format implementation for both `xtask repo-index` (emit) and on-device `pkg` (parse); a format change must update one module and re-run the host round-trip test.
- The hosting plan ($0 GitHub Releases + signed index, gh-pages mirror) is the Redox-model destination the Phase 85a doc named; GHCR/OCI is the documented escape hatch (Deferred Until Later), not adopted.
- Prefer exact files/symbols over directories when these land; update this list's checkboxes as tracks complete.
