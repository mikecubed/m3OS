# Phase 86b — Implementation Track Report

**Workflow:** `/flow:parallel-impl`
**Integration branch:** `feat/phase-86b-ssh-git-transport`
**Base / review target:** `main`
**PR:** [#228](https://github.com/mikecubed/m3OS/pull/228)
**Concurrency cap:** 2 (`.flow/defaults.json` absent → default)
**Models:** implementer = `claude-sonnet-4.6`, reviewer = `claude-opus-4.7` (per `.claude/models.yaml`); coordinator/implementer = Opus 4.8 (this session)
**Max revision rounds per track:** 2

## Discovery brief

- **Task shape:** multi-track-batch (Tracks A/B/C from `86b-ssh-git-transport-tasks.md`), but with a **strict A→B→C pipeline** and **heavy shared-file concentration**.
- **Scout:** skipped — the task doc + design doc are a fully-scoped brief (exact files, symbols, line numbers, acceptance). The coordinator read every task site directly and ran two `Explore` fan-outs to map the `git-local-smoke`/pkg-bundling plumbing and the port-registration machinery.
- **Environment:** musl cross-compiler present (`/usr/bin/x86_64-linux-musl-gcc`); host has SSH egress (`ssh-keyscan github.com` works) but **no GitHub-registered SSH key**, so the live-network half SKIPs by design.
- **Validation commands (coordinator-owned):**
  - `cargo xtask port build dropbear` (static seal + pkgcache-hit re-run)
  - `cargo xtask check` (clippy -D warnings + rustfmt + host tests + retpoline gate)
  - `cargo xtask git-ssh-smoke --timeout 600` (boot + install + run-on-m3OS + seed round-trip)

## Orchestration decision — serialized, not parallel worktrees

The three implementation tracks are **not independent** in the sense `parallel-impl`
requires: Tracks B.1, B.2, and C.1 all edit the same 17 000-line
`xtask/src/main.rs` (the dropbear→`ssh.m3pkg` bundling block, the `known_hosts`
seeding + optional identity in `populate_ext2_files`, and `cmd_git_ssh_smoke` +
the CLI dispatch), and B.1 additionally edits `xtask/src/port_build.rs`. They also
follow a hard build→wire→test pipeline (you cannot wire the smoke before the port
seals an artifact). Per the skill's **Core Rule 1 ("if in doubt, serialize")** and
the hard-conflict risk of parallel worktree edits to one giant file, the
xtask-bound work was implemented **serially by the coordinator**. Only the
genuinely disjoint, tiny pieces — the Track A docs and the C.2 version bump — were
candidates for fan-out; their worktree+agent overhead exceeded the benefit, so
they were done inline. **Review separation was preserved** via an independent
`code-quality-reviewer` agent on the integrated diff before publication (skill
Step 6), keeping implementation and review judgment distinct.

## Tracks

| Track | Tasks | Owned files | State |
|---|---|---|---|
| A | A.1 (ADR) | `docs/roadmap/86b-ssh-git-transport.md` (ADR + matrix + interop contract), `docs/appendix/sunset-local-fork.md` (sunset client budget) | ✅ done |
| B.1 | B.1 (dropbear port) | `ports/util/dropbear/Portfile` (new), `xtask/src/port_build.rs` (`build_dropbear`, `build_dropbear_port`, recipe-id/deps/dispatch, `.tar.bz2` extract), `xtask/src/main.rs` (dropbear→`ssh.m3pkg` bundle) | ✅ done |
| B.2 | B.2 (known_hosts) | `xtask/src/main.rs` (`populate_ext2_files`: `/root/.ssh` 0700, seeded `known_hosts` 0600, optional `id_dropbear`) | ✅ done |
| C.1 | C.1 (git wiring + gate) | `xtask/src/main.rs` (`cmd_git_ssh_smoke`, `git_ssh_smoke_steps`, CLI/usage, exit code), `AGENTS.md` (gate row), `.githooks/pre-push` (gate) | ✅ done |
| C.2 | C.2 (version) | `kernel/Cargo.toml`, `Cargo.lock` | ✅ done |

## ADR outcome (Track A, recorded before B/C)

**dropbear** (scored 53 vs sunset 34). sunset is server-only today
(`Runner::new_client`/`open_client_session` have zero userspace callers) with no
`known_hosts` store — only a `CheckHostkey` callback — so it would have needed a
from-scratch async client harness + TOFU layer. dropbear is +1 C port with native
TOFU and zero new harness. The dropbear branch of B/C was implemented.

## Review + rescue history

- **Independent reviewer (`code-quality-reviewer`) on the integrated diff: APPROVE / PASS — mergeable.** No BLOCKER/MAJOR; six MINOR. Applied #1 (recipe-id distinctness test + `dropbear`/`llvm`), #3 (robust mismatch assertion via `cat` body match, not a bare grep-count), #4 (gate clone on key *readability*), #6 (AGENTS.md version bump). Skipped #2 (fail_prefix parity with every existing pkg-install smoke — reviewer recommended no action) and #5 (no action needed). `cargo xtask check` re-run clean.
- **No rescues.** The background tasks (two core smokes, two live net-mode smokes, the reviewer) all ran to completion; no stalls, nudges, or replacements.

## Validation (coordinator-owned, on the integrated branch)

| Gate | Result |
|---|---|
| `cargo xtask port build dropbear` | ✅ `produced /usr/bin/dbclient + /usr/bin/ssh (static)` → sealed `…171f4d1c….m3pkg` (653 658 bytes); stripped binary runs `Dropbear v2024.86` |
| `cargo xtask port build dropbear` (re-run) | ✅ `pkgcache hit … zero compiler invocations` |
| `cargo xtask check` | ✅ `clippy clean, formatting correct, … retpoline gate pass`; kernel compiles as **v0.86.1** |
| `cargo xtask git-ssh-smoke --timeout 600` | ✅ `PASSED core (22 steps in 150s)` — `pkg install: ssh: OK`, `pkg install: git: OK` (git reused, not rebuilt), `dbclient -V`/`ssh -V` → `Dropbear v2024.86` on m3OS, seeded GitHub ed25519 key round-trips the VFS; live clone **SKIPPED** (net=false clone=false) with the documented NOTE |
| Banner / `uname` `0.86.1` | ✅ by construction — `kernel/src/lib.rs:75`, `procfs.rs:742`, `uname` utsname all use `env!("CARGO_PKG_VERSION")`, kernel recompiled at 0.86.1 |

**Live-network path (opt-in `M3OS_GIT_SSH_NET=1`) — driven, blocker localized.**
The mismatch-reject was run against `github.com:22` over SLIRP. Doing so surfaced
(and fixed) four real defects in the opt-in path that only execution exposes —
the reviewer could not run it: dropbear prints lowercase `host key mismatch for`
(not `Host key mismatch`); `printf` is absent in m3OS (→ `echo`); the `ion` login
shell mis-parses `git@github.com` as `@`-array expansion (→ single-quote); and
dropbear's blocking `getrandom()` hangs under entropy-starved `qemu64` (→ advertise
`+rdrand,+rdseed` so the 86a CSPRNG reaches READY). With those fixed, the run
advanced all the way to: dropbear resolves the host and the **kernel TCP layer
establishes the connection** (`connection established (active)`), but dropbear's
**non-blocking** `connect()` then reports `Connect failed: unexpected failure`
against m3OS's **synchronous** `sys_connect` — precisely the non-blocking-connect
item the design doc defers. So the live host-key reject (and any `git clone`) is
gated on m3OS gaining `EINPROGRESS`/writability connect semantics; the SSH client,
TOFU seed, and `GIT_SSH_COMMAND` wiring are otherwise verified. (The default gate —
`M3OS_GIT_SSH_REGRESSION=1` without `NET` — runs the network-free core and PASSES.)

## Batch outcome

- **Merged tracks:** A, B.1, B.2, C.1, C.2 — all integrated into `feat/phase-86b-ssh-git-transport` (commit `b5dbac4` + doc follow-up).
- **Retained/abandoned:** none.
- **Integration branch:** committed + pushed; PR #228 (draft → ready after review).
- **Workflow outcome measures:**
  - `discovery-reuse`: scout skipped; two `Explore` fan-outs grounded every B/C edit site — reused across tracks.
  - `rescue-attempts`: 0.
  - `abandonment-events`: 0.
  - `re-review-loops`: see the reviewer pass below.
- **Temporary work surfaces:** none (serial coordinator implementation; no track worktrees were provisioned, per the orchestration decision).
