# Publishing the networked package repo (Phase 107 Track C)

The on-device `pkg update` / networked `pkg install` (Phase 107) consume a
**signed static index + content-addressed blobs** hosted for $0 on GitHub
Releases in a separate public repo. This directory holds the pieces that
belong to that repo; nothing here runs in m3OS CI.

## One-time setup (owner actions)

1. **Keypair** — already generated via `cargo xtask repo-index --gen-key`:
   - public key: committed at [`keys/m3os-pkgs.pub`](../../../keys/m3os-pkgs.pub)
     (32 raw bytes), baked into every image at `/etc/pkg/keys/m3os-pkgs.pub`.
   - private seed: **not in this repo** — stored at
     `~/.m3os/m3os-pkgs-signing-key` on the build host. Add its single
     64-hex line as the Actions secret `M3OS_PKG_SIGNING_KEY_HEX` in the
     `m3os-pkgs` repo. Rotating the key requires a new image (single baked
     trust root — the charter's documented deferral).
2. **Public repo** — create `mikecubed/m3os-pkgs`, copy
   [`build-and-publish.yml`](./build-and-publish.yml) to
   `.github/workflows/`, and create the rolling release once:
   `gh release create repo-x86_64 --title "m3OS x86_64 package repo" --notes rolling`.
3. **Default repo URL** — `/etc/pkg/repos.conf` already points at
   `https://github.com/mikecubed/m3os-pkgs/releases/download/repo-x86_64`.

## Publishing flow

`build-and-publish.yml`: restore the content-addressed `target/pkgcache/`
cache → `cargo xtask port build all` (unchanged keys skip) →
`cargo xtask repo-index` (emit + ed25519-sign `index.m3idx`) →
`gh release upload --clobber` of the changed blobs + index + signature.

Local dry-run: `cargo xtask repo-index` with no `M3OS_PKG_SIGNING_KEY`
emits an unsigned index to `target/repo/` (useless for devices — `pkg
update` is fail-closed — but good for inspecting the emitted records).

## Validation

- CI-deterministic: `cargo xtask pkg-net-smoke` (no internet — a host HTTP
  thread serves a per-run-signed index over SLIRP `guestfwd`; asserts
  update-verify, tamper-reject, networked install with the index-`D:`
  solver + SHA-256 check, and bad-blob reject). `M3OS_PKG_NET_REGRESSION=1`
  runs it from the pre-push hook.
- Live arm (after the repo exists): boot `cargo xtask run`, then
  `pkg install curl && pkg update && pkg install <name>` against the real
  GitHub URL — record per `M3OS_PKG_NET=1` (opt-in, skip-with-reason).
