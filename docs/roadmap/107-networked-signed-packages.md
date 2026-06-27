# Phase 107 - Networked & Signed Package Distribution

**Status:** Planned
**Source Ref:** phase-107
**Depends on:** Phase 85a (`.m3pkg` format + offline `pkg` installer + `DEPS` solver) ✅, Phase 86c (HTTPS/TLS via `curl`+mbedTLS) ✅, Phase 42 (`crypto-lib` ed25519) ✅
**Builds on:** Adds a **network fetch** + a **signed index** on top of the Phase 85a *offline* `pkg` engine — the solve/verify/extract/DB engine (`topo_install_order`, `install_one`, `pkg_format::verify`, `/var/lib/pkg/db`) is reused 100 %. The only new code is download + index-parse + ed25519 signature-verify. The on-device TLS transport is the **Phase 86c `curl`/mbedTLS** binary, reached through the `fork`/`execve`/`waitpid` spawn seam — mbedTLS is **not** linked into the `no_std` installer. Part of the Phase 98 GUI-workstation arc (a workstation that installs software needs to fetch + verify packages from a repo).
**Primary Components:** `pkg-format/src/lib.rs` (new `index` module — `index.m3idx` parse/serialize + canonical signing bytes; the reserved-ed25519-field design intent realized), `userspace/pkg/src/main.rs` + `userspace/pkg/src/lib.rs` (new `pkg update` + networked `pkg install`, `repos.conf` parse, curl spawn seam, blob SHA-256 check; the existing `install_one`/`topo_install_order` reused unchanged), `userspace/crypto-lib/src/asymmetric.rs` (`ed25519_verify` against the baked-in pubkey), `userspace/syscall-lib/src/lib.rs` (`fork`/`execve`/`waitpid` — already present), `xtask/src/main.rs` + `xtask/src/port_build.rs` (new `cargo xtask repo-index` host tool), new `.github/workflows/build-and-publish.yml` (in a separate public `m3os-pkgs` repo)

## Milestone Goal

Prebuilt `.m3pkg` artifacts are distributed **over the network** so the on-device installer can **fetch + verify + install** them, hosted for **$0** on GitHub: GitHub Releases as the blob store + a tiny **ed25519-signed static index** (`index.m3idx`), published by a GitHub Actions pipeline. `pkg update` fetches and signature-verifies the index against a public key baked into the image; `pkg install <name>` then resolves dependencies from the index, fetches each `<key>.m3pkg` over HTTPS, SHA-256-checks it against the trusted index, and installs it through the **unchanged** Phase 85a offline path. This finally connects the landed TLS/DNS transport (86a/86c) to the offline-only `pkg` client.

## Why This Phase Exists

`pkg` today is **offline-only**: `cmd_install` reads `/usr/pkg/<name>.m3pkg` from a repo bundled into the image at build time (`userspace/pkg/src/main.rs:199`), with no HTTPS, no fetch, and no remote-repo concept. It also has **no signing or verification of provenance** — `pkg_format::verify` checks internal per-entry SHA-256 hashes for *integrity* (the artifact is not corrupt), but nothing proves the artifact came from a trusted publisher. For a GUI workstation that installs software, both gaps are blocking: you cannot grow the installed software set without rebuilding the whole image, and you cannot trust a blob pulled from the internet.

The Phase 85a doc already named the destination (its Track E.2 records "Publish `.m3pkg` artifacts as a flat static repo on GitHub Releases … mirroring the Redox model" and "the **networked** Phase 86 install … will require the reserved **ed25519** signature field to be populated and verified against a distributed public key"). Phase 86c then landed the on-device HTTPS transport (`curl`+mbedTLS). This phase joins them. GitHub Releases is **$0** for public repos (no bandwidth charges, no total-size cap, 2 GiB/asset >> the 368 MB `rust` package), which is cheaper than git-LFS (metered) and lighter on the `no_std` client than GHCR/OCI (kept as the documented escape hatch). The trust model follows Alpine/apt: **sign the index**, and let per-package integrity flow from the hashes recorded in that signed index.

## Learning Goals

- How a binary distribution establishes a **trust root by signing the index, not every package**: the ed25519-signed `index.m3idx` is the only thing verified against a key; each package's authenticity then follows transitively from the SHA-256 recorded in that trusted index (Alpine `APKINDEX`, Debian `InRelease` → `Packages` hashes). Compare with per-artifact signing (Arch, Redox `pkgar`).
- How **content-addressed storage** lets a CI pipeline rebuild only changed packages: the Phase 85a content key (`pkg_format::compute_package_key`) names the artifact `<key>.m3pkg`, so `actions/cache` of `target/pkgcache/` plus a key-stable URL means an unchanged port is never re-uploaded.
- How a `no_std` installer fetches over an untrusted transport **without linking a TLS stack**: it spawns the existing `curl` binary via `fork`/`execve`/`waitpid` (the same process boundary `git` uses), keeping mbedTLS out of the kernel-adjacent installer entirely.
- How a detached signature over a **deterministically serialized** index makes sign-on-host and verify-on-device agree byte-for-byte, and why canonical ordering (sort by name) is a correctness requirement, not a nicety.

## Feature Scope

### Track A — The signed repo index `index.m3idx` + trust model

A new `index` module in `pkg-format` defines an **APKINDEX-style flat-text** index: a short header (`M:m3idx1`, `A:x86_64`) then one record per package using single-letter field tags — `N:` name, `V:` version, `K:` content-key, `S:` size in bytes, `C:` SHA-256 hex of the whole `.m3pkg`, `U:<key>.m3pkg` relative URL, `D:` space-separated direct deps — records separated by blank lines, **sorted by name** so serialization is deterministic. The index is **ed25519-signed** with a **detached** `index.m3idx.sig` (raw 64-byte signature over the exact index bytes). The private key is a GitHub Actions secret; the public key is committed in-repo **and baked into the image** at `/etc/pkg/keys/m3os-pkgs.pub` (32 raw bytes). The index is the **trust root**; package integrity flows from its `C:` hashes. This realizes the design intent of `pkg_format`'s reserved signature field (`SIGNATURE_LEN`, the zeroed `Manifest.signature`) without changing the `.m3pkg` byte layout — the signature lives on the *index*, not in each package header.

### Track B — On-device networked `pkg` verbs

New verbs in the `no_std` `userspace/pkg`:

- **`pkg update`** — read repo base URLs from `/etc/pkg/repos.conf`, spawn `curl` to GET `<base>/index.m3idx` and `<base>/index.m3idx.sig`, then `ed25519_verify` the signature over the fetched index bytes against the baked-in pubkey (`crypto-lib::asymmetric::ed25519_verify`). On success, cache the verified index to `/var/lib/pkg/index.m3idx`; on signature failure, **reject and keep the previous index** (fail-closed).
- **`pkg install <name>`** over the network — parse the cached trusted index, build the dep map from the index `D:` fields, run the **existing** `topo_install_order`, then for each name in order: look up its index record, `curl -L` fetch `<base>/<key>.m3pkg` to `/usr/pkg/<name>.m3pkg`, compute its SHA-256 and compare to the index `C:` field (**reject on mismatch**), then call the **unchanged** `install_one`. Because the fetched blob lands at exactly the path `install_one` already reads, the offline extract/verify/DB path is reused verbatim.

Repos live in `/etc/pkg/repos.conf` (one base URL per line, `#` comments). The installer **never links mbedTLS** — TLS is entirely inside the spawned `curl`.

### Track C — The CI publish pipeline ($0 on a public repo)

A `build-and-publish.yml` in a separate public `m3os-pkgs` repo: restore `actions/cache` of `target/pkgcache/` (content-addressed → only changed-key ports rebuild), run `cargo xtask port build all`, then a **new `cargo xtask repo-index` host tool** that walks the built `target/pkgcache/*.m3pkg` set, reads each port's NAME/VERSION/DEPS, computes SIZE + SHA-256, emits `index.m3idx`, and **signs** it (ed25519 private key from a GHA secret env var) producing `index.m3idx.sig`. Then `gh release upload --clobber` pushes the new `<key>.m3pkg` blobs + `index.m3idx` + `.sig` to a **rolling per-arch release tag** (`repo/x86_64`), with an optional `gh-pages` mirror of the index for a stable URL.

### Track D — Validation

- Host tests (CI-deterministic, no QEMU): `index.m3idx` parse/serialize round-trip + deterministic ordering (`pkg-format`), ed25519 sign→verify + tamper-reject (`crypto-lib` already has `test_ed25519_tampered_message`; add an index-level tamper test), `topo_install_order` fed from index `D:` fields (`userspace/pkg` lib tests), and `xtask repo-index` emit+sign+self-verify.
- A `pkg-net-smoke` integration gate (new): the **CI-deterministic core** serves a freshly-signed index + a couple of `.m3pkg` blobs from a **host server over SLIRP plaintext HTTP** (the `10.0.2.100:80` guestfwd pattern `node-smoke`/`go-runtime-smoke` use — no real internet), then on-device `pkg update` verifies, a **tampered index is rejected**, `pkg install <leaf>` resolves+fetches+SHA-checks+installs, and a **bad-blob hash is rejected**. The **live HTTPS arm** (`M3OS_PKG_NET=1`) fetches from the real GitHub Releases / gh-pages URL and is **opt-in / skip-with-reason** (mirroring `git-https-smoke`'s `M3OS_GIT_HTTPS_NET`).

## Important Components and How They Work

### `pkg-format/src/lib.rs` — the `index` module (new)

Adds `IndexEntry { name, version, key, size, sha256, url, deps }`, `serialize_index(&[IndexEntry]) -> String` (sorted by name, deterministic), and `parse_index(&str) -> Result<Vec<IndexEntry>, Error>`. The bytes that get signed are simply the serialized index text — there is no separate canonicalization step, which is why the serializer must be deterministic. Reuses the existing in-crate `pkg_format::sha256::digest` for the `C:` blob hashes and `to_hex`. Pure logic, host-tested, no `unsafe`, no crypto dependency (the ed25519 verify lives in the caller). Shared by both the host `xtask repo-index` (emit) and the on-device `pkg` (parse) so the format has exactly one implementation.

### `userspace/pkg/src/main.rs` — networked verbs + curl spawn seam (new)

`cmd_update` and the networked branch of `cmd_install` are new; `install_one` (`:199`), `collect_deps` (`:184`), `read_file_bytes` (`:787`), and the DB helpers (`db_read`/`db_write`/`db_update`) are unchanged. A new `fetch_url(url, dest_path) -> bool` helper builds an argv (`["curl", "-fsSL", "-o", dest, url]`), `fork()`s, `execve()`s `/usr/bin/curl` in the child, and `waitpid()`s in the parent, returning success on exit status 0 (`fork`/`execve`/`waitpid` at `syscall-lib/src/lib.rs:1939/1945/1970`). The signature verify calls `crypto-lib::asymmetric::ed25519_verify(&pubkey, &index_bytes, &sig)`. No TLS code in this binary.

### `userspace/crypto-lib/src/asymmetric.rs` — verification (existing, reused)

`ed25519_verify` (`:25`) and `ed25519_verifying_key_from_bytes` (`:47`) are already present and RFC-8032-tested (`test_ed25519_rfc8032_test1`/`test2`, `test_ed25519_tampered_message`). The on-device `pkg` loads the 32-byte pubkey from `/etc/pkg/keys/m3os-pkgs.pub`, reconstructs the `VerifyingKey`, and verifies the detached signature. The host `xtask repo-index` reuses `ed25519_sign` (`:18`) for the publish side.

### `xtask repo-index` — the host publish tool (new)

A new subcommand in the `xtask/src/main.rs` dispatch (alongside `Some("port") => …` at `:1417`) → `cmd_repo_index`. It enumerates `target/pkgcache/*.m3pkg`, derives each entry's `K:`/`U:` from the content key (`port_build::pkgcache_artifact_path` / `pkg_format::compute_package_key`), reads `N:`/`V:`/`D:` from the port's Portfile/`.meta`, computes `S:`/`C:` from the blob bytes, calls `pkg_format::index::serialize_index`, and signs with an ed25519 key read from `M3OS_PKG_SIGNING_KEY` (a path or hex seed; absent → emit unsigned + warn, for local dry-runs). Host-tested: emit → parse round-trip + self-verify of the produced signature.

## How This Builds on Earlier Phases

- **Reuses the Phase 85a engine unchanged** — `topo_install_order`, `parse_meta`, `install_one`, `pkg_format::{parse,verify}`, and the `/var/lib/pkg/db` layer all carry over; networked install only changes *where the blob comes from* (curl → `/usr/pkg/<name>.m3pkg`) and adds a pre-extract SHA-256 check against the signed index.
- **Reuses the Phase 86c HTTPS transport** via the spawn seam — the same `curl`+mbedTLS binary `git` shells out to, reached by `fork`/`execve`/`waitpid`. mbedTLS is deliberately **not** linked into the `no_std` installer (it would not codegen on the soft-float target and would bloat a kernel-adjacent binary).
- **Reuses the Phase 42 `crypto-lib` ed25519** (`asymmetric.rs`) — already RFC-8032-tested; this phase adds no new crypto primitive, only a new caller.
- **Realizes the Phase 85a reserved-signature design intent** — the `pkg_format` header's reserved `SIGNATURE_LEN`/`Manifest.signature` field stays zeroed (the `.m3pkg` byte layout is unchanged); the trust signature instead lives on the *index*, which is the Alpine/apt model and the cheaper one for content-addressed blobs.
- **Sits in the Phase 98 GUI-workstation arc** — chartered as the phase that lets the workstation grow its software set after install; depends only on already-landed substrate, so it is CI-able (host tests + SLIRP-served gate) with an opt-in live arm.

## Implementation Outline

1. **Track A** — add the `index` module to `pkg-format` (`IndexEntry`, `serialize_index`, `parse_index`); host-test round-trip + deterministic ordering. Define the on-image key path `/etc/pkg/keys/m3os-pkgs.pub` and the repos file `/etc/pkg/repos.conf`; commit the public key in-repo and stage both into the ext2 image (`populate_ext2_files`).
2. **Track B** — add `fetch_url` (curl spawn via `fork`/`execve`/`waitpid`) and `cmd_update` (fetch index + `.sig` → `ed25519_verify` → cache to `/var/lib/pkg/index.m3idx`, fail-closed) to `userspace/pkg`; add the networked branch to `cmd_install` (resolve from index `D:` via `topo_install_order`, fetch each `<key>.m3pkg`, SHA-256-check vs index `C:`, call unchanged `install_one`); parse `/etc/pkg/repos.conf`.
3. **Track C** — implement `cargo xtask repo-index` (`cmd_repo_index`): walk `target/pkgcache/*.m3pkg`, emit + sign `index.m3idx`; write `build-and-publish.yml` (cache `target/pkgcache/`, `port build all`, `repo-index`, `gh release upload --clobber repo/x86_64`, optional gh-pages mirror) for the public `m3os-pkgs` repo.
4. **Track D** — host tests for index parse/sign/verify/solver; the `pkg-net-smoke` gate (SLIRP-served CI-deterministic core: update-verify, tamper-reject, install, bad-blob-reject; opt-in `M3OS_PKG_NET` live HTTPS arm); add the `M3OS_PKG_NET_REGRESSION` row to the `AGENTS.md` gate table + the Phase 107 README row.

## Acceptance Criteria

- `pkg-format::index::{serialize_index, parse_index}` round-trips a multi-record index with deterministic (name-sorted) output; host-tested in `cargo xtask check`.
- `pkg update` fetches `index.m3idx` + `index.m3idx.sig` and ed25519-verifies the index against the baked-in `/etc/pkg/keys/m3os-pkgs.pub`; a **tampered index** (any flipped byte in either the index or the signature) is rejected and the previous cached index is retained — asserted by `pkg-net-smoke`'s CI-deterministic arm and by a `crypto-lib`/`pkg-format` host tamper test.
- `pkg install <pkg>` over the network resolves deps from the index `D:` fields via the unchanged `topo_install_order`, fetches each `<key>.m3pkg`, **SHA-256-checks it against the signed index `C:` field**, and installs via the unchanged `install_one`; `pkg list` then shows it.
- A **bad blob hash** (fetched bytes whose SHA-256 ≠ the index `C:`) is rejected before extraction — asserted by `pkg-net-smoke`.
- `cargo xtask repo-index` emits a signed `index.m3idx` + `index.m3idx.sig` from `target/pkgcache/*.m3pkg`, and a self-verify (parse + `ed25519_verify`) passes — host-tested.
- `build-and-publish.yml` produces a signed index + content-addressed assets on a public repo at **$0** (GitHub Releases blob store + optional gh-pages index mirror), rebuilding only changed-key ports via the `target/pkgcache/` cache.
- The live HTTPS fetch arm (`M3OS_PKG_NET=1`) PASSES against the real published repo and is **skip-with-reason** when unset; the index-parse + sig-verify + solver are CI-deterministic (host tests + SLIRP-served boot arm). The `M3OS_PKG_NET_REGRESSION` row is documented in `AGENTS.md`.

## Companion Task List

- [Phase 107 Task List](./tasks/107-networked-signed-packages-tasks.md)

## How Real OS Implementations Differ

- **Alpine `apk`** signs a gzipped `APKINDEX.tar.gz` with an RSA (now also ed25519-capable) key and lets per-package integrity flow from the index hashes — exactly the model here, but Alpine ships a directory of trusted keys (`/etc/apk/keys/`) and supports multiple mirrors with failover; this phase bakes in a **single** key and one repo.
- **Debian `apt`** signs a `Release`/`InRelease` file (GPG, a web-of-trust keyring) that carries SHA-256 hashes of the `Packages` lists, which in turn hash each `.deb` — a two-level hash chain; `index.m3idx` collapses this to one level (the signed index directly hashes each `.m3pkg`).
- **Arch `pacman`** and **Redox `pkgar`** sign **every package artifact** (pacman: detached `.sig` per `.pkg.tar`; pkgar: ed25519 with a 136-byte remote header enabling *partial* fetch). m3OS signs only the index, trading per-artifact signatures for a single trust root and full-blob fetch (no partial download).
- Production managers ship **mirror lists, repo priorities, delta/zsync downloads, parallel fetch, key rotation, and TUF-style root-of-trust rotation**; this phase ships one repo, one key, full-artifact fetch, and a single rolling release tag.
- Real CDNs front the blob store; here GitHub Releases *is* the CDN, chosen because it is $0 for public repos with no bandwidth cap.

## Deferred Until Later

- **Multiple repos / mirror failover / repo priority** — `repos.conf` parses a list, but failover, signed-by-different-keys-per-repo, and priority ordering are deferred.
- **Key rotation / multiple signing keys / TUF-style root rotation** — a single baked-in key; rotating it requires a new image today.
- **Binary delta / zsync / partial-fetch packages** — full-artifact fetch only (the 85a deferral stands; the Redox `pkgar` 136-byte-header partial fetch is not adopted).
- **Transactional / atomic install + rollback** — installs stay per-file (the 85a posture).
- **GHCR / OCI-registry transport** — the documented escape hatch if GitHub Releases is ever unsuitable; heavier on the `no_std` client, so deferred.
- **`pkg search` / package descriptions / richer index metadata** — the index carries only resolution + integrity fields.
- **Resumable / parallel downloads** — one `curl` per blob, sequential.
- **A `cargo`/source-package networked path** — networked binary packages only; building from source over the network is out of scope.
