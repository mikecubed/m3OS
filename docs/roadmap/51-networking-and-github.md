# Phase 51 - Networking and GitHub

## Milestone Goal

m3OS can make HTTPS connections to the outside world. The GitHub CLI (`gh`) runs
natively, enabling pull requests, issues, and CI status checks from the command line.
git gains HTTPS remote support for clone, push, and pull to GitHub. DNS resolution
works via musl's built-in resolver.

## Learning Goals

- Understand TLS 1.3 and how certificate verification works (CA chains, root stores).
- Learn how DNS stub resolution works (UDP query to a configured nameserver).
- See how Go binaries bundle their own TLS and DNS, making them easier to deploy than
  C programs that depend on OpenSSL.

## Feature Scope

### GitHub CLI (gh)

Cross-compile `gh` with Go — this is trivially easy because Go cross-compilation is
a first-class feature:

```bash
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags="-s -w" -o gh ./cmd/gh
```

One command. ~40 MB static binary. **Go bundles its own TLS 1.3 and DNS resolver** —
no external OpenSSL, curl, or CA certificates needed. The Mozilla CA bundle is
compiled into the Go binary via `crypto/x509`.

**What works:** `gh auth login`, `gh repo clone`, `gh pr create`, `gh pr list`,
`gh issue create`, `gh run list`, `gh api`, and all other `gh` subcommands.

**xtask integration:** `build_gh()` function. Cached in `target/gh-staging/`.

### DNS Resolution

Write `/etc/resolv.conf` on the ext2 partition:
```
nameserver 10.0.2.3
```

QEMU's user-mode networking (SLIRP) provides a DNS forwarder at the gateway IP
(`10.0.2.3`). musl's built-in resolver reads this file and sends UDP DNS queries.
Go's resolver also reads this file.

**Kernel RNG upgrade:** The existing `getrandom()` syscall (318) is currently
backed by a PRNG seeded from `_rdtsc()`. For this phase, upgrade it to a CSPRNG
seeded from RDRAND/RDSEED (available on all modern x86_64 CPUs) so DNS transaction
IDs and Go's `crypto/rand` have real entropy.

### git HTTPS (Remote Operations)

Rebuild git with curl and a TLS library to enable `git clone`, `git push`, `git pull`
over HTTPS.

**Two approaches (choose one):**

1. **Rust transport helper** — write a small Rust binary using `ureq` + `rustls` that
   acts as a git remote helper (`git-remote-https`). This avoids cross-compiling
   libcurl and OpenSSL entirely. See [Rust crate acceleration](../rust-crate-acceleration.md).

2. **Traditional approach** — cross-compile libcurl + a TLS library (mbedTLS or
   OpenSSL) with musl and rebuild git with `NO_CURL=` (empty, enabling curl).

**CA certificates:** If using approach 2, bundle Mozilla's CA certificate file at
`/etc/ssl/certs/ca-certificates.crt` (~200 KB). Approach 1 (rustls + `webpki-roots`)
embeds CA certs in the binary.

### git Credential Storage

For `git push`, users need to authenticate with a personal access token:
- `credential-store`: plaintext in `~/.git-credentials` (simplest)
- `.netrc`: `machine github.com login user password ghp_...`
- Environment: `GIT_ASKPASS` or inline `https://user:token@github.com/...`

See [git roadmap](../git-roadmap.md) and [GitHub CLI roadmap](../github-cli-roadmap.md)
for full details.

## Dependencies

- **Phase 36** (Expanded Memory) — demand paging, `mprotect()` for Go runtime
- **Phase 37** (I/O Multiplexing) — `epoll` for Go's netpoller and libuv
- **Phase 38** (Filesystem Enhancements) — `/dev/null`, `/proc/self/exe`, symlinks
- **Phase 40** (Threading Primitives) — `clone(CLONE_THREAD)`, `futex` for Go runtime
- **Phase 42** (Crypto and TLS) — RustCrypto + rustls foundation
- **Phase 50** (Cross-Compiled Toolchains) — git (local) already bundled

## Acceptance Criteria

- [ ] `/etc/resolv.conf` exists with `nameserver 10.0.2.3`.
- [ ] `getrandom()` syscall works (RDRAND-seeded CSPRNG).
- [ ] `gh auth status` shows authenticated (with `GH_TOKEN` env var).
- [ ] `gh repo list --limit 5` lists repositories.
- [ ] `gh issue create --repo user/repo --title "test"` creates an issue.
- [ ] `gh pr create` creates a pull request.
- [ ] `git clone https://github.com/user/repo.git` clones a repository.
- [ ] `git push origin main` pushes with token authentication.
- [ ] DNS resolution of `github.com` succeeds.

## Deferred Items

- **SSH transport for git** — requires Phase 43 (SSH with sunset). HTTPS is sufficient.
- **Two-factor authentication** — tokens bypass 2FA.
- **gh extensions** — plugin system, not needed.
- **Signed commits (GPG)** — would need a GPG port. Deferred.
