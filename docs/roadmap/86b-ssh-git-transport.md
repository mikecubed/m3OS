# Phase 86b - SSH Client + git over SSH

**Status:** Planned
**Source Ref:** phase-86b
**Depends on:** Phase 86a (Outbound Foundation), Phase 85b (git Local) ✅, Phase 77 (DNS reply delivery D.1 + outbound TCP `connect` D.2) ✅
**Builds on:** Sub-phase 86b of the [Phase 86 umbrella](./86-networking-and-github.md). Reuses the Phase 85b local `git` binary **unchanged** and the Phase 85a `.m3pkg` substrate; consumes the Phase 86a CSPRNG (SSH ephemeral X25519), `known_hosts` path convention, and resolver/wall-clock foundation. This is the cheapest first secure transport because SSH reuses in-tree audited crypto and skips the entire X.509/CA stack.
**Primary Components:** the chosen static `ssh` client (`ports/util/dropbear/Portfile` + `build_dropbear` **or** `userspace/ssh/` reusing `crypto-lib` + `userspace/async-rt`'s `Reactor` + `sunset-local`), `userspace/ssh/src/known_hosts.rs` (or a seeded `dbclient` `known_hosts`), `xtask/src/main.rs` (`cmd_git_ssh_smoke`, `/etc/gitconfig` + `known_hosts` staging in `populate_ext2_files`), `ports/util/git` (Phase 85b, **untouched**), `docs/appendix/sunset-local-fork.md`

## Milestone Goal

`git clone --depth 1 --single-branch ssh://git@github.com/<owner>/<repo>` succeeds inside m3OS — the packfile arrives and `HEAD` checks out — with **zero changes to the git binary**. git's SSH transport shells out to a static `ssh` client on `PATH` (wired through `GIT_SSH_COMMAND`), so m3OS supplies the transport while upstream git speaks the protocol and moves the bytes. The `ssh` client is chosen by an in-phase **dropbear-vs-sunset spike + ADR**, ships as a `.m3pkg`, and performs `known_hosts`/TOFU host-key verification against GitHub's pinned ed25519 keys. Kernel bumps to **0.86.1**.

## Why This Phase Exists

After the Phase 86a trust foundation lands (a real CSPRNG, a fail-closed wall-clock, the resolver, and the on-disk `known_hosts`/credential conventions), the next step is a real authenticated remote clone — and SSH is the **minimal secure** way to get one. git's SSH transport is "git speaks the protocol, an external program moves the bytes": git fork/execs an `ssh` binary, runs `git-upload-pack` on the remote, and handles the packfile itself. So m3OS writes **no git-protocol/packfile code and rebuilds nothing** — the Phase 85b `git` is already sufficient. The only new artifact is a static `ssh` client.

The client choice is not obvious, which is why this sub-phase opens with a spike and an ADR rather than a foregone conclusion. The Rust SSH field is almost entirely ruled out by m3OS's SIMD-off constraint: **russh, ssh-rs, thrussh, and ssh2/libssh2-FFI all hard-depend on `ring`/`aws-lc-rs`** (asm/C crypto) or on a C TLS library, none of which build on the no-SSE Rust userspace target. The two genuine candidates are **dropbear** (`dbclient`, mature C, self-contained `libtomcrypt` software crypto, `known_hosts`/TOFU built in, ~110–200 KB, same port class as the existing `git`/Python/Clang C ports) and **`sunset`** (the only pure-RustCrypto SSH that fits — it reuses `crypto-lib`, fed by the 86a CSPRNG, and the `async-rt` reactor). The decisive axis is host-key verification cost: **sunset is server-only today** (`Runner::new_client`/`open_client_session` have zero userspace callers — `userspace/sshd/src/session.rs:182`'s `run_session` wires only the server) and has **no `known_hosts`/TOFU** — only a `CheckHostkey` callback — so the sunset branch must budget a from-scratch async client harness *and* a TOFU layer. The umbrella's documented recommendation is dropbear for 86b with the sunset spike captured as the future all-Rust migration path; this sub-phase makes that decision concrete and reversible.

This phase exists to validate the entire outbound transport path at low risk (no X.509 footguns) before the heavier HTTPS/TLS lift in [86c](./86c-https-git-transport.md).

## Learning Goals

- Why SSH is the cheapest first secure transport — it reuses in-tree audited crypto (X25519/Ed25519/ChaCha20-Poly1305) and skips the entire X.509/CA/hostname/revocation stack that 86c must build.
- How git remote transports work as "git speaks the protocol, an external program moves the bytes" — `GIT_SSH_COMMAND` fork/execs `ssh`, which runs `git-upload-pack` remotely, so the OS supplies transport, not protocol.
- How `known_hosts`/TOFU host-key trust works, why GitHub's host keys are pinned-as-rotatable-data (the RSA key rotated in 2023), and why a mismatched host key **must** be rejected.
- How the SIMD-off constraint collapses the Rust SSH choice space to a single pure-RustCrypto candidate, and how to weigh a mature C drop-in (dropbear) against an all-Rust greenfield harness (sunset) in a written ADR.

## Feature Scope

### Area A — ssh-client spike + ADR

A scored decision matrix (drop-in subprocess fit, GitHub interop, `known_hosts`/TOFU cost, binary size, crypto reuse) plus a written ADR recorded in this design doc and the `docs/appendix/sunset-local-fork.md` note. The ADR documents the interop contract both candidates satisfy — KEX `curve25519-sha256`, host-sig `ssh-ed25519`, ciphers `chacha20-poly1305@openssh.com` + `aes256-ctr`, MAC `hmac-sha2-256` — with the explicit risk note that interop rests on `chacha20-poly1305@openssh.com` (dropbear has no AES-GCM; GitHub accepts chacha20). The B/C task counts are **conditional on the spike outcome**: dropbear adds one C port and zero harness; sunset adds a from-scratch client harness, TOFU, and bundling.

### Area B — Static `ssh` client + `known_hosts`/TOFU

Build the chosen client as a static `.m3pkg` landing in `/usr/pkg/`, installable with `pkg install ssh`. **dropbear branch:** a `build_dropbear` port routed through the shared musl-toolchain plumbing, with `libtomcrypt` assembly disabled (SIMD-off), exposing `dbclient -y/-i/-p` and built-in `known_hosts`/TOFU. **sunset branch:** a Rust client harness in `userspace/ssh/` driving `Runner::new_client` + `open_client_session` over the `async-rt` `Reactor`, surfacing `CliEvent::Hostkey` so the app does TOFU + file I/O. GitHub's host keys are seeded as rotatable on-disk **data** (`github.com` and `ssh.github.com` ed25519 entries), and a **mismatched host key is rejected** (mandatory negative test).

### Area C — git wiring + smoke + version

Wire git to the client via `GIT_SSH_COMMAND` (never a bare `GIT_SSH`, which fails on argument handling) plus a bundled `/etc/gitconfig`, then validate a shallow single-branch `git clone` over an `ssh://` URL with an opt-in serial smoke gate. Bump the kernel to `0.86.1`.

## Important Components and How They Work

### The spike + ADR (`docs/appendix/sunset-local-fork.md`)

The decision is grounded in source: `sunset-local/src/runner.rs:73` (`new_client`) and `:486` (`open_client_session`) exist but have **zero userspace callers** — `userspace/sshd/src/session.rs:182` (`run_session`) is the only consumer and it wires the *server* path. Host-key verification in sunset is surfaced as `CliEvent::Hostkey(CheckHostkey)` (`sunset-local/src/event.rs:40`, struct at `:148-162`, dispatched via `CliEventId::Hostkey` at `:182`/`:211-213`); `CheckHostkey::accept`/`reject` resume via `Runner::resume_checkhostkey` (`runner.rs:168`). There is no `known_hosts` store — the callback is the entire mechanism. The ADR weighs this from-scratch harness+TOFU cost against dropbear's mature, built-in equivalent and records the decision.

### The static `ssh` client

**dropbear:** `build_dropbear` in `xtask/src/port_build.rs` follows the AGENTS.md port rules — `musl_toolchain()` (`port_build.rs:111`) for `CC`/`AR`/`RANLIB`, `musl_extra_ldflags_joined()` (`port_build.rs:105`) for the static LDFLAGS, `--host=x86_64-linux-musl`, with `libtomcrypt` built assembly-disabled so the software crypto stays SIMD-off-safe. **sunset:** a new `userspace/ssh/` binary linking `crypto-lib` (X25519/Ed25519/ChaCha20-Poly1305/SHA-2/HMAC, fed by the 86a CSPRNG) and driving the connection through `userspace/async-rt/src/reactor.rs:23`'s `Reactor`. Either way the artifact is a static binary on `PATH`, sealed into a `.m3pkg` and installed offline.

### `known_hosts` TOFU consumer

The host key is **data with a rotation path**. dropbear's `dbclient` carries TOFU + `known_hosts` natively; the sunset branch implements TOFU in `userspace/ssh/src/known_hosts.rs` — parsing/writing the `host ssh-ed25519 base64` format (mode `0600`) over the slow ring-3 VFS, accepting-on-first-use and **rejecting on mismatch**. GitHub's `github.com`/`ssh.github.com` ed25519 entries are pre-seeded (`SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU`) so the first real clone does not depend on an interactive prompt the smoke cannot answer.

### git wiring (no git changes)

`GIT_SSH_COMMAND` points git at the installed client; a bundled `/etc/gitconfig` (staged via `xtask/src/main.rs`'s `populate_ext2_files` at `:15586`) carries the commit identity and any `core.sshCommand`/`~/.ssh/config` alias. **The Phase 85b `build_git` (`xtask/src/port_build.rs:1427`) is untouched** — in particular the server-side pack helpers it prunes (`git-upload-pack`/`git-receive-pack`/`git-upload-archive`, `port_build.rs:1535`+) stay pruned, because those run on the *remote*, not the client. A clone uses the remote's `git-upload-pack` over the SSH channel; m3OS supplies only the client transport.

## How This Builds on Earlier Phases

- Reuses the **Phase 85b** local `git` binary with **zero rebuilds** — `build_git` is untouched and HTTPS stays absent (that is 86c).
- Consumes the **Phase 86a** CSPRNG for the SSH ephemeral X25519 (a weak SSH ephemeral is as fatal as a weak TLS key), the 86a `known_hosts` path convention (Track C of 86a), and the resolver + wall-clock foundation.
- Builds on **Phase 77**'s outbound TCP `connect` (`sys_connect` → `tcp::connect`, 3 s synchronous-connect cap) and DNS A-record resolution for reaching `github.com:22` / `ssh.github.com:443`.
- Rides the **Phase 85a** `.m3pkg` packaging + offline `pkg install` substrate for the new `ssh` artifact, and the AGENTS.md musl-toolchain port plumbing for the dropbear branch.

## Implementation Outline

1. **Spike + ADR (Track A).** Score dropbear vs sunset on the five axes; write the decision into this doc + `docs/appendix/sunset-local-fork.md`; document the KEX/host-sig/cipher/MAC interop contract with the chacha20-poly1305 risk note.
2. **Build the chosen client (Track B.1).** dropbear: add `ports/util/dropbear/Portfile` + `build_dropbear` (libtomcrypt asm-off, musl plumbing), register in `PORTS`/dispatch. sunset: build a `userspace/ssh/` harness over `new_client`/`open_client_session` + the `async-rt` `Reactor`. Seal the `.m3pkg`.
3. **TOFU + pinned host keys (Track B.2).** Seed `github.com` + `ssh.github.com` ed25519 entries as data; implement/verify TOFU accept-on-first-use and mismatch-reject; document rotation.
4. **git wiring + smoke (Track C.1).** Stage `/etc/gitconfig` + `known_hosts`; wire `GIT_SSH_COMMAND`; add `cmd_git_ssh_smoke` (modeled on `cmd_git_local_smoke`, `xtask/src/main.rs:13584`); clone a tiny repo `--depth 1 --single-branch` over `ssh://`.
5. **Version bump (Track C.2).** `kernel/Cargo.toml` `0.86.0` → `0.86.1`; `cargo xtask check` clean; banner/`uname` report `0.86.1`.

## Acceptance Criteria

- A scored dropbear-vs-sunset matrix (drop-in subprocess fit, GitHub interop, `known_hosts`/TOFU cost, binary size, crypto reuse) and a written ADR exist; the interop contract (KEX `curve25519-sha256`, host-sig `ssh-ed25519`, ciphers `chacha20-poly1305@openssh.com`+`aes256-ctr`, MAC `hmac-sha2-256`) is documented with the chacha20-poly1305 risk note.
- The chosen `ssh` client builds **static**, lands as a `.m3pkg` in `/usr/pkg/`, and `pkg install ssh` succeeds; `ssh -T git@github.com` returns the GitHub banner and exit code 1.
- `github.com` **and** `ssh.github.com` `ssh-ed25519` entries are seeded as data (`SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU`); a **mismatched host key is REJECTED** (negative test); the `known_hosts` format (`host ssh-ed25519 base64`, mode `0600`) round-trips the slow VFS and rotation is documented.
- `git clone --depth 1 --single-branch ssh://git@github.com/<owner>/<repo>` succeeds inside m3OS — the packfile arrives and `HEAD` checks out (serial-asserted) — over an `ssh://` URL or `~/.ssh/config` alias (never scp-like for non-22 ports), via `GIT_SSH_COMMAND`, with the Phase 85b git binary **not rebuilt**.
- `cargo xtask git-ssh-smoke` exists and is wired as an opt-in `M3OS_GIT_SSH_REGRESSION=1` pre-push gate (in `AGENTS.md` + `.githooks/pre-push`), skipping-with-reason when SSH key/creds are absent (mirroring `tls-smoke`/`dns-smoke` PASS-vs-SKIP).
- `kernel/Cargo.toml` reads `0.86.1`; `cargo xtask check` is clean; boot banner / `uname` report `0.86.1`.

## Companion Task List

- [Phase 86b Task List](./tasks/86b-ssh-git-transport-tasks.md)

## Architecture Decision Record (ADR): dropbear vs sunset (Track A)

**Status:** Accepted — **dropbear** (`dbclient`) is the 86b SSH client. The
all-Rust `sunset` client is captured as the future migration path (see
[`docs/appendix/sunset-local-fork.md`](../appendix/sunset-local-fork.md) §"A
client harness budget").

**Context.** git's SSH transport is "git speaks the protocol, an external program
moves the bytes": git fork/execs an `ssh` binary (via `GIT_SSH_COMMAND`), so 86b
needs exactly one new artifact — a static `ssh` client — and rebuilds no git code.
The SIMD-off Rust userspace target rules out almost the entire Rust SSH field:
russh, ssh-rs, thrussh, and ssh2/libssh2-FFI all hard-depend on `ring`/`aws-lc-rs`
(asm/C crypto) or a C TLS library, none of which build on the no-SSE Rust
userspace target. The two genuine candidates are **dropbear** (`dbclient`, mature
C, self-contained `libtomcrypt` software crypto, built-in `known_hosts`/TOFU) and
**sunset** (the only pure-RustCrypto SSH engine that fits — but **server-only
today**: `Runner::new_client`/`open_client_session` (`sunset-local/src/runner.rs:73`/`:486`)
have zero userspace callers, and host-key trust is only a `CheckHostkey` callback
(`event.rs:40`) with **no `known_hosts` store**).

**Scored matrix** (1 = poor … 5 = excellent; weight in parentheses):

| Axis (weight) | dropbear | sunset | Notes |
|---|---:|---:|---|
| Drop-in subprocess fit (×3) | 5 | 2 | dropbear *is* an `ssh`-shaped binary git fork/execs unmodified; sunset is a library needing a from-scratch `userspace/ssh/` async harness over the `async-rt` `Reactor`. |
| GitHub interop (×3) | 5 | 4 | Both satisfy the suite below; dropbear's interop is field-proven against GitHub, sunset's client path is unexercised. |
| `known_hosts`/TOFU cost (×3) | 5 | 1 | dropbear has accept-on-first-use + reject-on-mismatch + the file format **built in**; sunset surfaces only `CheckHostkey` — the app must write the entire TOFU + `0600` file-I/O layer (`known_hosts.rs`). |
| Binary size (×1) | 4 | 3 | dropbear static `dbclient` ≈ 300 KB stripped (proven: 382 KB unstripped → sealed `.m3pkg` 653 KB for the two-name copy); a Rust harness + crypto-lib is comparable-to-larger. |
| Crypto reuse / audit surface (×2) | 3 | 5 | sunset reuses in-tree audited `crypto-lib` (the 86a CSPRNG feeds it); dropbear ships its own vetted `libtomcrypt` software crypto (a second, self-contained crypto stack). |
| **Weighted total (max 60)** | **53** | **34** | dropbear wins decisively on the three ×3 axes that dominate 86b's cost. |

**Decision.** Ship **dropbear** for 86b. It is +1 C port and **zero** new
harness/TOFU code — it slots into the existing musl-toolchain port plumbing
(`build_dropbear`, routed through `musl_toolchain()`/`musl_extra_ldflags_joined()`)
exactly like `git`/Python/Clang, and its built-in `known_hosts` TOFU is the whole
of Track B.2 on the consumer side. The **sunset budget**, by contrast, is a
from-scratch async client harness (`userspace/ssh/` over `new_client` +
`open_client_session`) **plus** a `known_hosts.rs` TOFU + file-I/O layer **plus**
bundling — materially more code for 86b's "cheapest first secure transport" goal.
The decision is reversible: sunset remains the documented all-Rust migration path
once its client side and a TOFU store exist.

**Interop contract** (satisfied by both candidates; dropbear's defaults already
enable all of it, verified in the linked `chachapoly.o`/`ed25519.o`/`curve25519.o`
objects):

- **KEX:** `curve25519-sha256` (X25519 ephemeral, fed by the 86a CSPRNG).
- **Host signature:** `ssh-ed25519` (GitHub's pinned ed25519 key).
- **Ciphers:** `chacha20-poly1305@openssh.com` (primary) + `aes256-ctr` (fallback).
- **MAC:** `hmac-sha2-256` (implicit AEAD MAC for chacha20-poly1305; explicit for aes256-ctr).

**Risk note.** Interop rests on `chacha20-poly1305@openssh.com`: **dropbear has no
AES-GCM**, and GitHub does not offer `aes*-ctr`+ETM in every config — but GitHub
**does** accept `chacha20-poly1305@openssh.com`, which dropbear enables by default,
so the single shared cipher carries the connection. AES-NI/AES-GCM acceleration of
this path is deferred to [Phase 86f](./86-networking-and-github.md). If GitHub ever
drops chacha20-poly1305 for clients, this contract must be revisited (the fallback
`aes256-ctr` + `hmac-sha2-256` is the documented hedge).

## How Real OS Implementations Differ

- git's SSH transport is a fork/exec shell-out with a fixed precedence: `GIT_SSH_COMMAND` > `core.sshCommand` > `GIT_SSH` > the builtin `ssh` — and `GIT_SSH` is invoked positionally, so a bare `GIT_SSH='dbclient -y'` with embedded args fails (use `GIT_SSH_COMMAND`).
- scp-like remotes (`git@github.com:owner/repo`) **cannot carry a non-22 port** — reaching `ssh.github.com:443` requires an `ssh://` URL or a `~/.ssh/config` alias.
- GitHub's SSH endpoint is a **restricted shell**: `ssh -T git@github.com` greets and exits 1; it is pubkey-only ed25519, on port 22 or `ssh.github.com:443`.
- GitHub host keys are **pinned as data** (ed25519/ecdsa/rsa fingerprints; the RSA key rotated in 2023), so a static system must treat its known-good keys as rotatable, not compiled-in.
- dropbear's `dbclient` has built-in `known_hosts` TOFU plus `-y/-i/-J/-p`, at ~110–200 KB; **Redox's `redox-ssh`** is the pure-Rust client+server precedent for the sunset path.
- Mature clients negotiate AES-GCM, certificates, agents, and many KEX/cipher suites; m3OS pins a single interoperable suite (`chacha20-poly1305@openssh.com`) and a single host-key algorithm.

## Deferred Until Later

- HTTPS / TLS / smart-HTTP git transport (mbedTLS + curl, X.509) — [Phase 86c](./86c-https-git-transport.md).
- The all-Rust sunset client migration (replacing a dropbear default), agent forwarding, `ssh-agent`, ProxyJump, and certificate-based host keys.
- SSH push (`git-receive-pack` on the remote) beyond clone/fetch validation, and multi-host `~/.ssh/config` ecosystems.
- AES-GCM / hardware-AES-NI acceleration of the SSH crypto path — [Phase 86f](./86-networking-and-github.md).
- Non-blocking-connect and TCP reassembly hardening (`sys_connect`'s 3 s cap; `kernel/src/net/tcp.rs:20` drops out-of-order payload) — a lossy-SLIRP clone risk noted but not fixed here. **Empirically confirmed as the one remaining blocker for the live SSH path:** the `M3OS_GIT_SSH_NET=1` mismatch-reject test was driven against `github.com:22` over QEMU SLIRP (with `+rdrand,+rdseed` credited so dropbear's blocking `getrandom()` reaches READY, and `git@github.com` single-quoted past `ion`'s `@`-array expansion). dropbear resolves the host and the **kernel TCP layer establishes the connection** (`connection established (active)`), but dropbear — which issues a **non-blocking** `connect()` and waits on socket writability — then reports `Connect failed: unexpected failure` against m3OS's **synchronous** `sys_connect`. So the live host-key reject (and any clone) is gated on m3OS gaining non-blocking-connect (`EINPROGRESS` + writability) semantics, exactly this deferred item; the SSH client, TOFU seed, and `GIT_SSH_COMMAND` wiring are otherwise verified.
