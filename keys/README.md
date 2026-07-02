# Trust keys

- `m3os-pkgs.pub` — the **public** ed25519 key (32 raw bytes) that verifies
  the networked package repo index (`index.m3idx`), Phase 107. Baked into
  every image at `/etc/pkg/keys/m3os-pkgs.pub`; `pkg update` verifies the
  signed index against it fail-closed.

**Never commit a private signing seed here.** The matching seed lives
off-repo at `~/.m3os/m3os-pkgs-signing-key` on the build host and is set as
the `M3OS_PKG_SIGNING_KEY_HEX` GitHub Actions secret in the `m3os-pkgs`
publish repo (see `docs/appendix/m3os-pkgs/README.md`). Rotating the key
requires a new image (single baked trust root — a documented Phase 107
deferral). Regenerate a keypair with
`cargo xtask repo-index --gen-key <path>`.
