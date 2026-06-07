# Phase 86b — SSH Client + git over SSH: Task List

**Status:** Planned
**Source Ref:** phase-86b
**Depends on:** Phase 86a (Outbound Foundation), Phase 85b (git Local) ✅, Phase 77 (DNS reply delivery D.1 + outbound TCP `connect` D.2) ✅
**Goal:** Land a static `ssh` client (chosen by an in-phase dropbear-vs-sunset spike + ADR) with `known_hosts`/TOFU host-key verification, wire it to the **unchanged** Phase 85b `git` via `GIT_SSH_COMMAND`, and prove a shallow single-branch `git clone ... ssh://git@github.com/<repo>` succeeds end-to-end inside m3OS — the cheapest first secure remote transport, reusing in-tree audited crypto and skipping the entire X.509/CA stack. Kernel bumps to `0.86.1`.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirror the 92-vfs-bulk-io style.)

> **Spike-conditional tracks.** The Track B/C task *count* depends on the Track A spike outcome: the **dropbear** branch adds one C port and zero harness; the **sunset** branch adds a from-scratch async client harness + TOFU + bundling. Both branches are presented below; implement the one the ADR selects. Per the umbrella ([86-networking-and-github.md](../86-networking-and-github.md)), the documented recommendation is **dropbear for 86b**, with the sunset spike captured as the future all-Rust migration path.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | dropbear-vs-sunset spike + scored matrix + written ADR | 86a | Planned |
| B | Static `ssh` client `.m3pkg` + `known_hosts`/TOFU + pinned GitHub keys | A, 86a (CSPRNG + `known_hosts` path) | Planned |
| C | `GIT_SSH_COMMAND` wiring + ssh-clone smoke gate + version bump | B, 85b (git untouched) | Planned |

---

## Track A — Spike + decision (ADR)

### A.1 — dropbear-vs-sunset spike with a scored matrix + written ADR

**Files:**
- `docs/roadmap/86b-ssh-git-transport.md` (the ADR section of this design doc)
- `docs/appendix/sunset-local-fork.md` (the sunset client-harness/TOFU budget note)

**Symbol:** `sunset_local::runner::Runner::new_client` (`sunset-local/src/runner.rs:73`), `Runner::open_client_session` (`runner.rs:486`), `Runner::resume_checkhostkey` (`runner.rs:168`), `CliEvent::Hostkey(CheckHostkey)` (`sunset-local/src/event.rs:40`, struct `:148-162`, dispatch `CliEventId::Hostkey` `:182`/`:211-213`)
**Why it matters:** the Rust SSH field is ruled out by SIMD-off — russh, ssh-rs, thrussh, ssh2/libssh2-FFI all hard-depend on `ring`/`aws-lc-rs` (asm/C) or a C TLS lib; sunset is the only pure-RustCrypto fit, but it is **server-only today** (`new_client`/`open_client_session` have zero userspace callers — `userspace/sshd/src/session.rs:182`'s `run_session` wires only the server) and has **no `known_hosts`/TOFU**, only a `CheckHostkey` callback, so picking sunset materially changes the B/C task count.

**Acceptance:**
- [ ] A scored matrix exists with five axes — drop-in subprocess fit, GitHub interop, `known_hosts`/TOFU cost, binary size, crypto reuse — plus a written decision; the sunset branch's budget line explicitly accounts for a from-scratch async client harness + TOFU layer (versus dropbear's built-in equivalents).
- [ ] The interop contract is documented: KEX `curve25519-sha256`, host-sig `ssh-ed25519`, ciphers `chacha20-poly1305@openssh.com` + `aes256-ctr`, MAC `hmac-sha2-256` (satisfiable by both candidates), with a risk note that interop rests on `chacha20-poly1305@openssh.com` (dropbear has no AES-GCM; GitHub accepts chacha20).

---

## Track B — ssh client binary + TOFU

> **Conditional on A.1.** Implement the dropbear *or* the sunset branch per the ADR.

### B.1 — Build the chosen ssh client as a static `.m3pkg`

**File (dropbear branch):** `ports/util/dropbear/Portfile` (new) + `build_dropbear` in `xtask/src/port_build.rs`, routed through `musl_toolchain()` (`port_build.rs:111`) + `musl_extra_ldflags_joined()` (`port_build.rs:105`), registered in `PORTS` and the `port_build` `match name` dispatch.
**File (sunset branch):** `userspace/ssh/` (new Rust client harness) linking `crypto-lib` + driving `userspace/async-rt/src/reactor.rs:23`'s `Reactor`.

**Symbol:** `build_dropbear` (dropbear branch) **or** the `userspace/ssh` client harness over `Runner::new_client` + `Runner::open_client_session` (sunset branch)
**Why it matters:** dropbear's `libtomcrypt` software crypto must be built with **assembly disabled** (SIMD-off), and the sunset path must build the client harness **from scratch** because only the server is wired today; either way the artifact is a static binary on `PATH`.

**Acceptance:**
- [ ] The chosen `ssh` client builds **static** and seals into a `target/pkgcache/<key>.m3pkg`; it is bundled into `/usr/pkg/` and `pkg install ssh` lays it into `/usr` and succeeds.
- [ ] `ssh -T git@github.com` returns the GitHub banner and exit code 1.
- [ ] dropbear branch: `dbclient -p 443 -i <key> -y -T git@ssh.github.com` connects to `ssh.github.com:443`, accepts the host key non-interactively (`-y`), uses the given identity (`-i`), and the `git@` restricted-shell banner returns with exit code 1. sunset branch: the harness drives `new_client` + `open_client_session` via the `async-rt` `Reactor` and surfaces `CliEvent::Hostkey` to the app.

### B.2 — known_hosts TOFU + pin GitHub host keys as seed data

**File:** `userspace/ssh/src/known_hosts.rs` (sunset branch TOFU consumer) **or** a `dbclient` `known_hosts` seeded via `xtask/src/main.rs`'s `populate_ext2_files` (`main.rs:15586`) (dropbear branch)
**Symbol:** the `known_hosts` TOFU consumer (accept-on-first-use + mismatch-reject + `0600` file I/O)
**Why it matters:** the host key must be **data with a rotation path** (GitHub rotated its RSA key in 2023); the dropbear `dbclient` does TOFU natively, while the sunset path surfaces the key via `CheckHostkey` but the **app** must do TOFU + file I/O.

**Acceptance:**
- [ ] `github.com` **and** `ssh.github.com` `ssh-ed25519` entries are seeded as data (`SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU`).
- [ ] A **mismatched** host key is **REJECTED** (negative test asserts the clone aborts and the bad key is not written).
- [ ] The `known_hosts` format (`host ssh-ed25519 base64`, file mode `0600`) round-trips the slow ring-3 VFS (written then re-read on a subsequent connection), and the key-rotation procedure is documented.

---

## Track C — git wiring + smoke + version

### C.1 — Wire git via `GIT_SSH_COMMAND` + an ssh-clone smoke gate

**Files:**
- `xtask/src/main.rs` (`cmd_git_ssh_smoke`, modeled on `cmd_git_local_smoke` at `main.rs:13584`; `/etc/gitconfig` + `known_hosts` staging via `populate_ext2_files` at `main.rs:15586`)
- `AGENTS.md` (opt-in gate row, `M3OS_GIT_SSH_REGRESSION=1`)
- `.githooks/pre-push` (gate wiring)

**Symbol:** `cmd_git_ssh_smoke`
**Why it matters:** the Phase 85b `git` is sufficient **unchanged** — `build_git` (`xtask/src/port_build.rs:1427`) is untouched and the server-side pack helpers it prunes (`git-upload-pack`/`git-receive-pack`/`git-upload-archive`, `port_build.rs:1535`+) stay pruned (they run on the *remote*); a bare `GIT_SSH='dbclient -y'` fails on positional args, and scp-like syntax cannot carry `:443`, so the wiring must use `GIT_SSH_COMMAND` and an `ssh://` URL or config alias.

**Acceptance:**
- [ ] `M3OS_GIT_SSH_REGRESSION=1` boots m3OS, `pkg install`s `git` + `ssh`, and `git clone --depth 1 --single-branch ssh://git@github.com/<owner>/<repo>` succeeds — the packfile arrives and `HEAD` checks out (both serial-asserted).
- [ ] The clone uses an `ssh://` URL or a `~/.ssh/config` alias (never scp-like for non-22 ports) and is driven through `GIT_SSH_COMMAND` (never a bare `GIT_SSH`).
- [ ] The gate runs at a long `--timeout` (clang-gate class, e.g. 5400 s) because the packfile transfer over the ~200 KB/s ring-3 VFS is slow, and clones a tiny repo with `--depth 1 --single-branch` to bound the packfile.
- [ ] The gate confirms **no git rebuild** occurred (the bundled Phase 85b `git` `.m3pkg` is reused) and **skips-with-reason** when the SSH key/creds are absent (mirroring `tls-smoke`/`dns-smoke` PASS-vs-SKIP).
- [ ] `cargo xtask git-ssh-smoke` exists and is wired into both `AGENTS.md`'s pre-push gate table and `.githooks/pre-push` behind `M3OS_GIT_SSH_REGRESSION=1`.

### C.2 — Bump kernel crate `0.86.0` → `0.86.1`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.86.1"` (line 3, currently `0.85.3`)
**Why it matters:** the 86b cut is the second Phase 86 sub-phase (mirrors 85b `0.85.1`); the per-sub-phase patch bump tracks the umbrella's `0.86.0` → `0.86.5` sequence.

**Acceptance:**
- [ ] `kernel/Cargo.toml` line 3 reads `version = "0.86.1"` (with `Cargo.lock` updated).
- [ ] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests + retpoline gate).
- [ ] Boot banner / `uname` report `0.86.1` (`env!("CARGO_PKG_VERSION")`).

---

## Documentation Notes

- **What changed relative to Phase 85b.** Phase 85b's `git` binary is reused **unchanged** — `build_git` (`xtask/src/port_build.rs:1427`) is not touched and its pruned server-side pack helpers (`port_build.rs:1535`+) stay pruned; the only new artifacts are the static `ssh` client `.m3pkg` and the `known_hosts`/`GIT_SSH_COMMAND` wiring. HTTPS/curl/TLS remain absent ([86c](./86c-https-git-transport.md)).
- **Spike-conditional task count.** Record the ADR outcome before implementing Track B/C — dropbear is +1 C port with native TOFU; sunset is +a from-scratch client harness (`userspace/ssh/`) + a `known_hosts.rs` TOFU layer + bundling.
- **Host keys are rotatable data, not compiled-in.** Seed `github.com` + `ssh.github.com` ed25519 entries; the mandatory mismatch negative test (B.2) is what proves verification is real and not a green-but-broken pass.
- **Use `GIT_SSH_COMMAND` + `ssh://`/config alias, never scp-like for `:443`.** scp-like remotes cannot carry a non-22 port, and `GIT_SSH` is invoked positionally.
- **Clone-cost caveat.** The packfile over the ~200 KB/s VFS plus the `sys_connect` 3 s synchronous-connect cap and the no-TCP-reassembly drop path (`kernel/src/net/tcp.rs:20`) make a lossy SLIRP clone a known risk; `--depth 1 --single-branch` on a tiny repo plus a clang-class `--timeout` is the mitigation.
- **Prefer exact symbols** — `Runner::new_client`/`open_client_session`/`resume_checkhostkey`, `CliEvent::Hostkey`, `build_git`, `cmd_git_local_smoke`, `populate_ext2_files`, `musl_toolchain` — over generic descriptions.
