# Claude Code

**Aligned Roadmap Phase:** Phase 90
**Status:** Complete
**Source Ref:** phase-90

## Overview

Phase 90b runs **Claude Code** — Anthropic's CLI coding agent — natively inside
m3OS as a content-addressed `.m3pkg`, on top of the Phase 90a JIT/WASM-capable
Node variant. This is the post-1.0 developer platform's integration capstone: a
non-trivial modern Node application that needs a JIT runtime, a WebAssembly TUI
layout engine, HTTPS to a live API, subprocess management, raw-mode terminal
I/O, and git — all of it exercised together rather than as isolated synthetic
probes. The headline outcome is `cargo xtask claude-smoke`: an always-on offline
core that `pkg install claude-code` (solving `DEPS=node` dependency-first) →
`claude --version` (= `2.1.112`) → `claude --help` → the vendored `rg --version`,
plus opt-in live arms (an authenticated `claude -p` round-trip to
`api.anthropic.com`, a file/shell/git agent workflow asserted by `cat`/`git
log`, and the interactive-TUI render proof captured via the QMP/PPM screenshot
harness). The kernel bumps `0.90.0` → `0.90.1` (Phase 90a took the `0.90.0`
minor; this sub-phase takes the patch, mirroring the 86a–f sub-phase pattern).

Phase 90b adds **no kernel work**. The runtime substrate is delivered entirely
by Phase 89 (static Node 22, the `timerfd` event loop, the libuv threadpool
`FUTEX_CMP_REQUEUE` fix, always-on in-kernel-TCP egress) and Phase 90a
(PKU-backed W^X v2 + a JIT Node variant on which `yoga.wasm` instantiates). This
phase's deliverable is packaging, environment pinning, credential handling, the
TUI render proof, and — most importantly — an honest, falsifiable
supported-workflow boundary. The one Phase 89 leftover it closes is the
explicitly deferred in-Node `SIGINT` assertion (now `NODE_SIGINT_OK`, validated
in the `claude-smoke` always-on core).

## What This Doc Covers

- The **supported-workflow decision** and the **native-binary divergence** — why
  the pin is `@anthropic-ai/claude-code@2.1.112` and not `latest`.
- The **install path** — a pre-bundled `.m3pkg` (fetch + stage host-side, seal,
  install offline from `/usr/pkg/`) and why live `npm install -g` is *not* the
  supported path.
- The **runtime dependency chain** — `claude-code` (`DEPS=node`) → the Phase 90a
  JIT Node variant → the `/usr/bin/claude` launcher → the Phase 86a CA bundle.
- The **`/usr/bin/claude` launcher env contract**, line by line, each with one
  sentence of *why*.
- The **credential posture** — subscription-first 0600 OAuth-token/key seeding,
  headless onboarding, and the in-OS `/login` paste-flow.
- The **A.1 WASM/TUI decision** and the **ripgrep static-pie finding**.
- The **supported-workflow boundary** stated falsifiably, with the explicit
  non-goals matching the design doc's Deferred section.

## Core Implementation

### The supported-workflow decision and the native-binary divergence

The single most important decision in this phase is the **version pin**.
Claude Code's `latest` line (2.1.177 at authoring time) does **not** run on the
Phase 89/90a Node runtime at all: at version **2.1.113** Anthropic repackaged
claude-code into a **native Bun/JavaScriptCore single-file binary** — a
per-platform, ~500 MB native executable that the npm wrapper's `install.cjs`
copies over a stub. That model ships **no `cli.js` and does not use the Node
runtime**, so pinning `latest` would invalidate the entire `DEPS=node` +
Phase 89/90a JIT-Node dependency chain this phase is built on (running the native
Bun binary on m3OS would be a separate future port — a Bun runtime, not Node).

**`2.1.112` is the last version shipping the classic model** the phase assumes:

- `cli.js` — a 9.3 MB (gzipped; ~13.7 MB unpacked) JavaScript bundle that runs
  on `node`, with the **WebAssembly TUI layout engine (`yoga`) embedded inside it**
  (in 2.1.112; the 1.x/2.0 bundles shipped a standalone `yoga.wasm` file). The
  embedded WASM is the reason the JIT variant is required (see below).
- `vendor/ripgrep/` — a vendored platform `rg` for the file-search tool.

The pinned tarball (Track A host-side spike, 2026-06-13):

- **Version:** `@anthropic-ai/claude-code@2.1.112`
- **URL:** `https://registry.npmjs.org/@anthropic-ai/claude-code/-/claude-code-2.1.112.tgz`
- **SHA256:** `84379969ea53a0e5fd231a8f77debe4c7cb17dd971f4809d10d33f9aeca5de09`
  (~18.7 MB tarball, ~49 MB unpacked)

Pinning 2.1.112 is the faithful, correct way to deliver the phase as designed.
The host spike validated it: `node cli.js --version` → `2.1.112 (Claude Code)`
(exit 0, 0.26 s / 178 MB RSS under host node v24), and `--help` renders the full
CLI. Running the native Bun binary (2.1.113+) on m3OS is explicit future /
out-of-scope work.

### The install path: a pre-bundled `.m3pkg`, not live `npm install -g`

The supported, reproducible install path is the same one every heavy port uses:
**fetch + stage the npm tarball host-side, seal it as a content-addressed
`.m3pkg`, and install offline from `/usr/pkg/`** (Track B). `build_claude_code`
downloads the pinned registry tarball SHA-verified, stages the package payload
under `<stage>/usr/lib/claude-code/`, stages the `/usr/bin/claude` launcher, and
seals a `target/pkgcache/<key>.m3pkg` whose `DEPS=node` makes the in-OS solver
auto-install the Phase 89/90a runtime — the same dependency-first proof
`pkg install tmux`→`libevent` established in Phase 85a. No live network inside
the guest is needed to install.

Live `npm install -g @anthropic-ai/claude-code` is deliberately **not** the
supported path. Phase 89 D.2 proved npm *launches* and *reaches the registry*
over real HTTPS, but loading npm's ~thousands of tiny JS files over the
~200 KB/s ring-3 VFS made full `npm install` completion impractical — a per-file
`open`/`stat`/`write` round-trip-latency limit the Phase 87 bulk-I/O coalescing
does not remove (87's win is on large sequential files; npm's workload is the
opposite shape) — and repo CI has no outbound egress anyway. Live
`npm install -g` survives only as a **documented opt-in real-internet arm**, not
the supported path.

### The runtime dependency chain

```
claude-code (.m3pkg, DEPS=node)
  └─> node (.m3pkg)  ← the Phase 90a JIT Node variant (NOT the jitless Phase 89 default)
        └─> /usr/bin/claude launcher
              └─> /etc/ssl/certs/ca-certificates.crt  ← the Phase 86a Mozilla CA bundle
```

The bundled `node` is specifically the **Phase 90a JIT variant** (V8 JIT + WASM
under PKU-backed W^X v2), because the interactive TUI's `yoga.wasm` cannot run
on the Phase 89 **jitless** node — jitless V8 allocates zero runtime executable
memory and so cannot instantiate WebAssembly. The `M3OS_WITH_CLAUDE` image block
therefore bundles **both** `.m3pkg`s (claude-code *and* the JIT node variant) so
the offline solver can resolve `DEPS=node` in-guest against the right runtime.
Because the JIT variant requires PKU, the `claude-smoke` gate is **KVM/PKU-gated
exactly like `node-jit-smoke`** (SKIP-with-reason without `M3OS_KVM=1` on a PKU
host) — for *all* arms, not just the TUI one: on a no-PKU CPU the JIT node aborts
at its first code-space commit (it does not degrade to jitless).

### The `/usr/bin/claude` launcher: the pinned supported environment

Claude Code's defaults assume a mainstream Linux. The launcher (`exec
/usr/bin/node /usr/lib/claude-code/cli.js "$@"` behind a set of env exports) is
where the supported configuration is *pinned* rather than hoped for — every env
line is a documented support-boundary decision. It relies on the Phase 89 `#!`
shebang/exec support and the `DEPS=node` runtime at `/usr/bin/node`; the install
layout is relocated under `/usr/lib/claude-code/`, and `cli.js` resolves the
`vendor/` tools relative to its own dir (the WASM TUI engine is embedded in
`cli.js`).

| Launcher line | Why |
|---|---|
| `export NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-certificates.crt` | Node's bundled OpenSSL validates `api.anthropic.com` against the Phase 86a Mozilla CA bundle — m3OS has no system trust-store discovery, so the path is pinned explicitly. |
| `export DISABLE_AUTOUPDATER=1` | The sealed `.m3pkg` is the only supported delivery; auto-update over the VFS is impractical and would version-drift the artifact away from the pinned, sealed content. |
| `export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` | Suppresses telemetry / Statsig / Sentry egress attempts — dead weight and a confusing failure mode on a box with no default outbound egress. |
| `exec /usr/bin/node /usr/lib/claude-code/cli.js "$@"` | Runs the bundled `cli.js` on the `DEPS=node` runtime via the Phase 89 shebang/exec chain; the relocated `/usr/lib/claude-code/` layout keeps the `vendor/` tools resolvable (the WASM TUI engine is embedded in `cli.js`). |

`claude --version` on m3OS prints `2.1.112` *through this launcher*, proving the
shebang/exec chain + relocated install layout end-to-end — asserted in the
Track D always-on core.

### Credential posture: subscription-first, 0600, never on the wire

Subscription use is **first-class**, mirroring the Phase 86e `gh` `GH_TOKEN`
precedent exactly. A Pro/Max user mints a long-lived OAuth token **on the host**
with `claude setup-token` (the browser dance happens there, off m3OS), and the
gate seeds it via the **dedicated** `M3OS_CLAUDE_TOKEN` env var — never the
user's ambient variable, so it can't bake into a routine image build. The token
is written to a **mode-0600** file under `/root/.claude/` and exported in-guest
as `CLAUDE_CODE_OAUTH_TOKEN`. The token **value never crosses the serial
console** (only file paths do), never lands in any log, and never lives in
repo/CI — secret hygiene is by construction, not by review. An in-guest `ls -l`
asserts the credential file is `-rw-------` (the same assertion `gh-smoke` makes
on `hosts.yml`).

`M3OS_CLAUDE_KEY` → `ANTHROPIC_API_KEY` is the **API-billing alternative**
through the same seeding path; when both are set the gate prefers the token and
records which mode authenticated. Absence of both means **skip-with-reason**,
never failure.

`/root/.claude.json` is pre-seeded with completed-onboarding / trust state so a
headless `claude -p` invocation proceeds with **no interactive first-run
prompt**. The in-OS **`/login` paste-flow** (the TUI shows a URL; the user visits
it on any browser-equipped device and pastes the returned code back into the
m3OS terminal) is the documented **human** path once the TUI works — it cannot be
the gate path because it is interactive by design (manually validated once if the
TUI lands, not gate-automated). Multi-user / enterprise credential stories are
out of scope (see Deferred).

### The A.1 WASM/TUI decision

Claude Code's terminal UI ships `yoga.wasm`, a WebAssembly layout engine. The
Phase 89 jitless V8 config disallows runtime WASM code generation (it allocates
zero runtime executable memory), and m3OS forbids unguarded RWX. Rather than
settling for a print-mode floor, **Phase 90a delivers PKU-backed JIT** — the W^X
invariant is *strengthened* to v2 (W+X is permitted only via the PKU-guarded
`pkey_mprotect` path under a write-deny key), not relaxed — and a JIT Node
variant on which WASM works. Phase 90b consumes that variant. The TUI path is
already de-risked: 90a's `node-jit-smoke` proves `WebAssembly.instantiate` runs
on the m3OS JIT node, and 2.1.112's embedded `yoga` WASM engine is the same
capability class. The
on-OS rendered-UI proof is the Track D QMP/PPM screenshot arm (a serial sentinel
is blind to TUI rendering; only a framebuffer screenshot is falsifiable
evidence). The jitless `claude -p` path remains the documented degraded fallback
if 90a slips — not the milestone.

### The ripgrep static-pie finding (no port needed)

Claude Code's file-search tool shells out to a *vendored platform ripgrep
binary* at `vendor/ripgrep/x64-linux/rg`. The Track A/B audit (`readelf -l`)
found it is **static-pie linked with NO `PT_INTERP`** (~6.5 MB). m3OS's ELF
loader supports `ET_DYN` static-PIE via its no-interpreter path
(`kernel/src/mm/elf.rs`), so the vendored `rg` runs **directly** — no separate
ripgrep port was needed. The `build_ripgrep` static-musl fallback stays a
documented contingency, not built. The optional vendored native bits
(`audio-capture.node`, a dynamic addon; the `seccomp` helper, for which m3OS has
no seccomp) are pruned or degrade gracefully. `rg --version` is asserted in the
Track D always-on core, so a search-tool regression is a gate failure, not a
silent degradation.

### The interactive substrate (Phase 89 A.2, validated in `claude-smoke`)

An interactive CLI agent lives on three primitives Phase 89 deferred to this
phase: trapping Ctrl-C, putting the tty in raw mode, and spawning shell commands.
Each is a one-line always-on probe arm riding the `claude-smoke` always-on core
(the task list explicitly permits the A.2 arms to ride this gate; the probes
themselves need no JIT, so they share the boot that bundles the agent — the one
caveat is that `claude-smoke` is KVM/PKU-gated, so they run on the dev/PKU host
rather than in CI):

- `NODE_SIGINT_OK` — `process.on('SIGINT')` fires on a self-signal, proving
  libuv's self-pipe signal path (`pipe2` + `rt_sigaction`) end-to-end — the
  explicit Phase 89 A.2 deferred item.
- `NODE_SPAWN_OK` — `child_process.spawn('/bin/sh', ['-c', 'echo spawned'])`
  captures stdout with a 0 exit code — the libuv fork/exec + pipe-capture path
  the shell tool uses.
- `NODE_RAWMODE_OK` — `process.stdin.setRawMode(true/false)` toggles termios
  `ICANON`/`ECHO` over the PTY stack without throwing.

### The `claude-smoke` gate

`cargo xtask claude-smoke` (Track D) has an **always-on offline core** that
proves the entire setup story with zero network and zero secrets:
`M3OS_WITH_CLAUDE=1` bundles both `.m3pkg`s, the in-OS solver resolves
`DEPS=node` dependency-first, the launcher chain (`/usr/bin/claude` → shebang →
node → `cli.js`) runs to `claude --version` = `2.1.112` and `claude --help`
exits 0, and the vendored `rg --version` prints. Because the bundled node is the
JIT variant, the gate is KVM/PKU-gated (SKIP-with-reason without `M3OS_KVM=1` on
a PKU host) and runs at `--timeout 5400` (the ~130 MB install + cold `cli.js`
parse over the slow VFS — far faster under KVM, which is also where the JIT
node's PKU comes from). Absent the build prerequisites (host C++ toolchain for
the node dep) the gate prints `SKIP (reason: …)` and returns success.

The **opt-in live arms** (`M3OS_CLAUDE_NET=1` + a `M3OS_CLAUDE_TOKEN` /
`M3OS_CLAUDE_KEY`) are the actual milestone — the agent *does work*:

- **`CLAUDE_API_OK`** — `claude -p 'Reply with exactly CLAUDE_API_OK and nothing
  else'` completes a full TLS 1.3 handshake + cert-chain validation against
  `api.anthropic.com` (Node's bundled OpenSSL + the 86a CA bundle + c-ares DNS)
  and prints the sentinel; subscription-token mode preferred.
- **File/shell/git workflow** — a scripted `claude -p` creates a named file with
  known content, runs a shell command, and makes a git commit, asserted
  **outside** the agent by `cat` of the file and `git log --oneline` (the
  assertion trusts the filesystem and git, never the model's own claim).
- **TUI render proof** — an interactive `claude` launch driven via QMP
  `send_key` and captured via `screendump`; the PPM analysis asserts a populated
  TUI (multi-row non-black text occupancy + frame-to-frame change on keystroke),
  the `less-render-probe` pattern — the 90a payoff.

Skip-with-reason when unconfigured (real egress to `api.anthropic.com:443` + a
secret can never be CI-bound) — the always-on D.1 core is what CI sees, mirroring
`gh-smoke` / `git-https-smoke`.

## Key Files

| File | Purpose |
|---|---|
| `ports/util/claude-code/Portfile` | Pinned `@anthropic-ai/claude-code@2.1.112` + registry-tarball SHA-256, `CATEGORY=util`, `DEPS=node` |
| `xtask/src/port_build.rs` | `fn build_claude_code` — fetch-and-stage the pinned npm tarball, stage `/usr/lib/claude-code/` + the `/usr/bin/claude` launcher, seal the `.m3pkg`; the vendored-`rg` `readelf -l` static-pie audit |
| `xtask/src/main.rs` | `fn cmd_claude_smoke` / `fn claude_smoke_steps` — serial DSL gate; the `M3OS_WITH_CLAUDE` bundle block in `populate_phase_69d_ports`; the `M3OS_CLAUDE_TOKEN`/`M3OS_CLAUDE_KEY` 0600 credential seeding |
| `xtask/src/qmp.rs`, `xtask/src/ppm.rs` | The headless-framebuffer harness reused for the TUI render arm (`QmpClient::screendump` + PPM row-occupancy analysis) |
| `kernel/src/mm/elf.rs` | The `ET_DYN` static-PIE (no `PT_INTERP`) loader path that runs the vendored static-pie `rg` directly |
| `kernel/Cargo.toml` | `version = "0.90.1"` (Phase 90a took `0.90.0`; this sub-phase takes the patch) |
| `docs/claude-code-roadmap.md` | Standalone per-tool narrative (revived from archive in E.2) |
| `docs/roadmap/90b-claude-code.md` | Phase design doc |
| `docs/roadmap/tasks/90b-claude-code-tasks.md` | Per-track task list with acceptance items |

## How This Phase Differs From Later and Real Agent Work

- Phase 90b pins **2.1.112** (the last `cli.js` + `yoga.wasm` + `vendor/ripgrep/`
  Node-runtime version). The 2.1.113+ **native Bun/JavaScriptCore single-file
  binary** distribution is a separate future port (a Bun runtime, not Node) and
  is out of scope here.
- The supported install is a **pre-bundled `.m3pkg`**, not live `npm install -g`.
  Live npm survives only as an opt-in real-internet arm — a VFS-throughput limit
  on npm's thousands-of-tiny-files workload, not a TLS or registry gap.
- The agent runs on the **Phase 90a JIT Node variant**; the Phase 89 jitless node
  cannot run `yoga.wasm`. On a no-PKU CPU the JIT node aborts (it does not
  degrade to jitless), so the whole gate is KVM/PKU-gated.
- Credentials are the **seeded host-minted OAuth token / API key** plus the
  in-OS `/login` paste-flow whose browser step happens on another device. There
  is no in-OS browser, no MCP / IDE integration, and no multi-user credential
  story — mature hosted-agent ecosystems support far broader integrations than
  m3OS should assume. The real value here is a platform-integration proof point,
  not a claim that m3OS is a full hosted AI workspace.

### The supported-workflow boundary (falsifiable)

With the documented steps a user can reproduce, on m3OS:

1. `M3OS_WITH_CLAUDE=1 cargo xtask image` bundles `claude-code` + the JIT node.
2. Boot `0.90.1`; `pkg install claude-code` auto-installs `node` first
   (dependency-first solver order, asserted in the gate output).
3. `claude --version` prints `2.1.112` and `claude --help` exits 0 — fully
   offline.
4. With a seeded `M3OS_CLAUDE_TOKEN` (or `M3OS_CLAUDE_KEY`) and `M3OS_CLAUDE_NET=1`
   on a KVM/PKU host: `claude -p` round-trips `api.anthropic.com`, a scripted
   agent workflow creates a file / runs a shell command / makes a git commit
   (proven by `cat` + `git log`, not the model's claim), and the interactive TUI
   visibly renders (proven by a framebuffer screenshot, not a launch sentinel).

Everything outside that list is a non-goal (below).

## Related Roadmap Docs

- [Phase 90b design doc](./roadmap/90b-claude-code.md)
- [Phase 90b task list](./roadmap/tasks/90b-claude-code-tasks.md)
- [Phase 90a — Memory Protection Keys (PKU)](./roadmap/90a-memory-protection-keys.md) — the W^X v2 / JIT Node substrate the interactive TUI depends on
- [Phase 89 — Node.js](./89-nodejs.md) — the static Node runtime, the `npm`/TLS path, and the deferred interactive-substrate items closed here
- [Claude Code standalone roadmap](./claude-code-roadmap.md) — per-tool porting narrative with Mermaid dependency diagrams (revived for Phases 90a/90b)

## Deferred or Later-Phase Topics

- **Auto-update** — `DISABLE_AUTOUPDATER=1`; the sealed `.m3pkg` is the delivery.
- **Non-essential telemetry** — disabled via
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`; no Statsig/Sentry egress.
- **In-OS browser** — none; auth is the seeded OAuth token / API key plus the
  `/login` paste-flow whose browser step happens on another device.
- **MCP servers / IDE integrations / optional protocol extensions** — out of
  scope (the surrounding ecosystem grows faster than the OS).
- **Multi-user / enterprise credential-management stories** — out of scope;
  credentials live only in 0600 files under `/root/.claude/`.
- **Rich GUI integration** for the agent — out of scope.
- **Offline / local-model alternatives** beyond the documented cloud-backed
  path — out of scope.
- **The native Bun-binary distribution (2.1.113+)** — a separate future port (a
  Bun runtime, not Node); out of scope for this phase.
- **Live `npm install -g`** — implemented as an opt-in real-internet arm only;
  the sealed `.m3pkg` is the supported install path (a VFS-throughput limit, not
  a network gap).
