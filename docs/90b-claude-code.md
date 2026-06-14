# Claude Code

**Aligned Roadmap Phase:** Phase 90
**Status:** Complete
**Source Ref:** phase-90

## Overview

Phase 90b runs **Claude Code** — Anthropic's CLI coding agent — natively inside
m3OS as a content-addressed `.m3pkg`. **The delivered milestone is install +
launch + headless `claude -p` + the interactive primitives**, on *both* the
CI-viable jitless Node (Phase 89) and the JIT Node (Phase 90a). This is the
post-1.0 developer platform's integration capstone: a non-trivial modern Node
application that needs HTTPS to a live API, subprocess management, raw-mode
terminal I/O, git, and — for the interactive TUI — a JIT runtime and a
WebAssembly layout engine, exercised together rather than as isolated synthetic
probes. The headline outcome is `cargo xtask claude-smoke`, which **PASSES** on
m3OS: an always-on offline core that `pkg install claude-code` (solving
`DEPS=node` dependency-first) → `claude --version` (= `2.1.112`) → `claude
--help` → the vendored static-pie `rg --version` → the A.2
`NODE_SIGINT_OK`/`NODE_SPAWN_OK`/`NODE_RAWMODE_OK` interactive-substrate probes —
**27/27 on the jitless full-install gate** (the CI-viable default, no KVM/PKU
needed) and **27/27 on the 90a JIT-node serial core** (`M3OS_CLAUDE_JIT=1`,
KVM/PKU-gated) — plus opt-in live arms (an authenticated `claude -p` round-trip
to `api.anthropic.com`, a file/shell/git agent workflow asserted by
`cat`/`git log`). (The early "24/24" figure was an `M3OS_CLAUDE_FAST_ITER`
reuse-disk run; the interactive TUI is not part of any of these step counts.)
The **full interactive `claude` TUI does not yet run** on m3OS: a direct QMP/PPM
render test (PR-audit, 2026-06-14) on the JIT node showed the launch gets
through onboarding and JITs under the W^X-v2 PKU-guarded path, then crashes on a
userspace null-deref tied to unhandled `mremap`/`io_uring` syscalls the heavy
interactive path exercises (Phase 93 syscall-gap territory) — a tracked
follow-up (see *The A.1 WASM/TUI decision* below). The kernel bumps `0.90.0` →
`0.90.1` (Phase 90a took the `0.90.0` minor; this sub-phase takes the patch,
mirroring the 86a–f sub-phase pattern).

Phase 90b was **planned as "no kernel work"**, but the integration test surfaced
one real W^X-v2 cross-thread PKU gap — exactly the kind of finding the phase
exists to surface — and a single principled page-fault-handler fix landed as a
documented Phase 90a PKU follow-up (see *The W^X-v2 cross-thread PKU
read-recovery fix* below). It is the centerpiece of the as-built story: it
unblocked `cli.js` on *both* node variants. The rest of the runtime substrate is
delivered by Phase 89 (static Node 22, the `timerfd` event loop, the libuv
threadpool `FUTEX_CMP_REQUEUE` fix, always-on in-kernel-TCP egress) and Phase
90a (PKU-backed W^X v2 + a JIT Node variant on which `yoga.wasm` instantiates).
This phase's deliverable is packaging, environment pinning, credential handling,
the one PKU follow-up fix, and — most importantly — an honest, falsifiable
supported-workflow boundary. The one Phase 89 leftover it closes is the
explicitly deferred in-Node interactive substrate (`NODE_SIGINT_OK` and the
`NODE_SPAWN_OK`/`NODE_RAWMODE_OK` raw-mode/spawn probes), now validated in the
`claude-smoke` always-on core.

## What This Doc Covers

- The **supported-workflow decision** and the **native-binary divergence** — why
  the pin is `@anthropic-ai/claude-code@2.1.112` and not `latest`.
- The **install path** — a pre-bundled `.m3pkg` (fetch + stage host-side, seal,
  install offline from `/usr/pkg/`) and why live `npm install -g` is *not* the
  supported path.
- The **runtime dependency chain** — `claude-code` (`DEPS=node`) → the bundled
  Node runtime (jitless by default; the 90a JIT variant for the TUI) → the
  `/usr/bin/claude` launcher → the Phase 86a CA bundle.
- **The W^X-v2 cross-thread PKU read-recovery fix** — the one kernel change the
  integration test forced, landed as a 90a PKU follow-up; what unblocked
  `cli.js` on both node variants.
- The **`/usr/bin/claude` `#!/usr/bin/env node` launcher** (a CJS wrapper that
  pins the supported env *in-process* and runs `cli.js` via dynamic `import()` —
  *not* a `#!/bin/sh` script, because m3OS's `/bin/sh` is `ion`), each pinned
  env var with one sentence of *why*.
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
  └─> node (.m3pkg)  ← jitless (Phase 89) by default; the 90a JIT variant under M3OS_CLAUDE_JIT=1
        └─> /usr/bin/claude launcher  (#!/usr/bin/env node CJS wrapper, in-process import())
              └─> /etc/ssl/certs/ca-certificates.crt  ← the Phase 86a Mozilla CA bundle
```

The default bundle is the **Phase 89 jitless node**. `cli.js`'s entire CLI
surface — `--version`, `--help`, headless `claude -p`, the vendored search tool,
the A.2 interactive primitives — runs on it: jitless V8 uses the Ignition
interpreter and allocates *zero* runtime executable memory, so it never touches
the PKU/W^X JIT path, and the always-on core is therefore **CI-viable under
plain TCG** (no KVM, no PKU). The **interactive TUI** needs runtime WASM
(`yoga.wasm`, embedded in `cli.js` in 2.1.112), which requires the **Phase 90a
JIT variant** (V8 JIT + WASM under PKU-backed W^X v2); jitless V8 cannot
instantiate WebAssembly. `M3OS_CLAUDE_JIT=1` selects that variant, and because it
requires PKU that arm is **KVM/PKU-gated exactly like `node-jit-smoke`**
(SKIP-with-reason without `M3OS_KVM=1` on a PKU host — on a no-PKU CPU the JIT
node aborts at its first code-space commit, it does not degrade to jitless). Note
that the JIT/WASM runtime working is necessary but **not sufficient** for the
full interactive TUI: that path still crashes on unhandled `mremap`/`io_uring`
syscalls (Phase 93 syscall-gap territory — see *The A.1 WASM/TUI decision*). The
`M3OS_WITH_CLAUDE` image block bundles **both**
`.m3pkg`s (claude-code *and* the node variant being tested) so the offline
solver can resolve `DEPS=node` in-guest against the right runtime.

### The `/usr/bin/claude` launcher: a `#!/usr/bin/env node` CJS wrapper

The launcher is where the supported configuration is *pinned* rather than hoped
for — but it is emphatically **not** a `#!/bin/sh` shell script. The first gate
runs surfaced exactly why: m3OS's `/bin/sh` is **`ion`**, which — unlike POSIX
`sh` — intercepts `--version` and prints its own banner instead of running the
shebang script body (and the alternative built-in shell `sh0` ignores `argv`
entirely). A `#!/bin/sh` launcher that tried to `exec node cli.js "$@"` therefore
never reached `cli.js` for `claude --version`. **The one interpreter m3OS runs
correctly with flag arguments is node itself** — the `#!/usr/bin/env node` path
that npm's own bin shims ride.

So `/usr/bin/claude` is a **`#!/usr/bin/env node` CommonJS wrapper**: it pins the
supported environment **in-process** (`process.env.…`) and then runs `cli.js`
via a dynamic `import()` — a **single node process, no fork/exec of a second
node**. It relies on the Phase 89 `#!` shebang support to launch node, and on
node resolving its own argv with flags (the path npm rides). The install layout
is relocated under `/usr/lib/claude-code/`, and `cli.js` resolves the `vendor/`
tools relative to its own dir (the WASM TUI engine is embedded in `cli.js`).

| Pinned in the wrapper | Why |
|---|---|
| `process.env.NODE_EXTRA_CA_CERTS = '/etc/ssl/certs/ca-certificates.crt'` | Node's bundled OpenSSL validates `api.anthropic.com` against the Phase 86a Mozilla CA bundle — m3OS has no system trust-store discovery, so the path is pinned explicitly. Set in-process; node reads it lazily when the root store is first built, which covers the opt-in TLS arm. |
| `process.env.DISABLE_AUTOUPDATER = '1'` | The sealed `.m3pkg` is the only supported delivery; auto-update over the VFS is impractical and would version-drift the artifact away from the pinned, sealed content. |
| `process.env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = '1'` | Suppresses telemetry / Statsig / Sentry egress attempts — dead weight and a confusing failure mode on a box with no default outbound egress. |
| `await import('/usr/lib/claude-code/cli.js')` | Runs the bundled `cli.js` **in the same node process** (no second fork/exec); the relocated `/usr/lib/claude-code/` layout keeps the `vendor/` tools resolvable (the WASM TUI engine is embedded in `cli.js`). |

`claude --version` on m3OS prints `2.1.112` *through this wrapper*, proving the
shebang→node→in-process-`import()` chain + relocated install layout end-to-end —
asserted in the Track D always-on core.

### The W^X-v2 cross-thread PKU read-recovery fix (the one kernel change)

Phase 90b was planned with **no kernel work**: the runtime was supposed to be
fully delivered by Phases 89 + 90a, leaving only packaging. The integration test
falsified that assumption in the best possible way — it surfaced a real
W^X-v2 cross-thread PKU gap that 90a's synthetic probes never exercised. This is
exactly what an integration phase exists to find, and the roadmap had even
pre-flagged it as the "SMP-PKU follow-up." A single principled
page-fault-handler fix closes it; it lands here as a documented **Phase 90a PKU
follow-up**.

**The bug.** A real-world Node process (`cli.js`) allocates a write-deny
protection key for its V8 code space, then spawns worker/background threads
(`clone_thread` ×5). **PKRU is per-thread.** A sibling thread that was created
*before* the key existed inherits a PKRU in which that key is access-disabled, so
when it DATA-reads the now-pkey-tagged **executable** V8 code page, the CPU
raises a `PROTECTION_KEY` page fault:

```
userspace page fault: pid=… err=(PROTECTION_VIOLATION | USER_MODE | PROTECTION_KEY) rip=0x1b898b1 — process killed
```

m3OS kills the faulting process, so `claude --version` crashed before `cli.js`
finished booting. (The fault header was buried in the trace-ring dump; an
`M3OS_SERIAL_LOG` full-serial tee captured it.) This is *not* a JIT-only failure
— it reproduces identically on the jitless node and the JIT node, because both
run the real multi-threaded `cli.js`.

**The fix.** The W^X-v2 invariant only needs **writes** gated per-thread-window;
**read + execute of guarded code is process-wide** (every thread of a process
must be able to run its own code pages). So on a `PROTECTION_KEY` **read** fault
against a *present*, *executable* page carrying a non-zero protection key, the
page-fault handler now grants the faulting thread read access — it clears that
key's access-disable bit in the thread's *live* PKRU (the context-switch XSAVE
persists it across switches) and retries the instruction. Crucially:

- **Writes stay gated.** `CAUSED_BY_WRITE` faults are *excluded* from the grant,
  so the W^X-v2 write-deny window is intact — W^X is not weakened.
- **Data-isolation pages are never granted.** The grant requires the page to be
  **executable**; non-executable access-deny *data* pages (the PKU data
  isolation that `pku-smoke` exercises) fall through to the kill path unchanged.

This fix unblocks `cli.js` on **both** the jitless node (where it's the actual
blocker — jitless still spawns the same threads and tags the same code pages) and
the JIT node. See `kernel/src/arch/x86_64/interrupts.rs` (`page_fault_handler` +
a new `leaf_pte_flag_bits` helper) and `kernel/src/arch/x86_64/pkru.rs` (the new
`grant_read_access`). The earlier "no kernel work" framing is corrected to: one
documented page-fault-handler fix landed as a 90a PKU follow-up.

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
m3OS terminal) is the documented **human** path once the interactive TUI runs —
it cannot be the gate path because it is interactive by design (and the full
interactive TUI does not yet run; see *The A.1 WASM/TUI decision* for the tracked
syscall-coverage follow-up). Multi-user / enterprise credential stories are
out of scope (see Deferred).

### The A.1 WASM/TUI decision

Claude Code's terminal UI ships `yoga.wasm`, a WebAssembly layout engine. The
Phase 89 jitless V8 config disallows runtime WASM code generation (it allocates
zero runtime executable memory), and m3OS forbids unguarded RWX. Rather than
settling for a print-mode floor, **Phase 90a delivers PKU-backed JIT** — the W^X
invariant is *strengthened* to v2 (W+X is permitted only via the PKU-guarded
`pkey_mprotect` path under a write-deny key), not relaxed — and a JIT Node
variant on which WASM works. Phase 90b consumes that variant under
`M3OS_CLAUDE_JIT=1`, and the **JIT/WASM runtime the TUI depends on is proven**:
90a's `node-jit-smoke` proves V8 TurboFan optimization and
`WebAssembly.Instance` run on the m3OS JIT node, the W^X-v2 cross-thread PKU fix
above unblocks `cli.js` *launch* there too, and the A.2 raw-mode/SIGINT/spawn
primitives the TUI lives on all pass. The jitless `claude -p` path is the
always-available automation floor and the CI-viable default — not a degraded
fallback, but a first-class part of the delivered scope.

**The full interactive `claude` TUI does not yet run on m3OS — this is a tracked
follow-up, and it is a *syscall-coverage* gap, not a JIT/PKU gap.** A direct
QMP/PPM framebuffer render test (PR-audit, 2026-06-14) on the 90a JIT node showed
the interactive `claude` launch gets through first-run onboarding (writes
`/root/.claude.json`), JIT-compiles under the W^X-v2 PKU-guarded path (`[wx]
v2-guarded W+X mapping` is logged — so the JIT/PKU substrate works), and spawns a
ripgrep subprocess — then **crashes with a userspace null-pointer dereference**
(`addr=0x0`, `rip=0x1e3464c`) right after the ripgrep `SIGCHLD`, with `unhandled
syscall 25` (`mremap`), `425` (`io_uring_setup`), and `125` (`capget`) logged
just before. `mremap` is an **explicit Phase 93 deferred item** ("close the
syscall gaps a dynamic libc needs (`mremap`, …)"). So the JIT/PKU layer is *not*
the blocker — V8 committed code under the guarded path before the crash — the
heavy interactive path simply exercises syscalls m3OS has not yet implemented.

The interactive-TUI **visual render is therefore a needed, tracked follow-up**,
not a mere manual-screenshot step. Once the `mremap`/`io_uring` syscall gaps
close (Phase 93 territory), the visual proof becomes a **one-line wire-up of the
existing `htop-render-probe` QMP/PPM harness** (`xtask/src/qmp.rs` +
`xtask/src/ppm.rs`): drive an interactive `claude` launch via QMP `send_key`,
`screendump`, and assert a populated TUI (multi-row non-black occupancy +
frame-to-frame change on keystroke). A serial sentinel is blind to TUI rendering,
so only a framebuffer screenshot is falsifiable evidence the screen *shows* the
UI — but that step is gated behind the syscall work, not merely a deferred
screenshot.

### The ripgrep static-pie finding (no port needed)

Claude Code's file-search tool shells out to a *vendored platform ripgrep
binary* at `vendor/ripgrep/x64-linux/rg`. The Track A/B audit (`readelf -l`)
found it is **static-pie linked with NO `PT_INTERP`** (~6.5 MB). m3OS's ELF
loader supports `ET_DYN` static-PIE via its no-interpreter path
(`kernel/src/mm/elf.rs`), so the vendored `rg` runs **directly** — no separate
ripgrep port was needed. **This is now confirmed on-OS:** `rg --version` runs
under m3OS in the `claude-smoke` always-on core. The `build_ripgrep` static-musl
fallback stays a documented contingency, not built. The optional vendored native
bits (`audio-capture.node`, a dynamic addon; the `seccomp` helper, for which
m3OS has no seccomp) are pruned or degrade gracefully. Because `rg --version` is
asserted in the always-on core, a search-tool regression is a gate failure, not
a silent degradation.

### The interactive substrate (Phase 89 A.2, validated in `claude-smoke`)

An interactive CLI agent lives on three primitives Phase 89 deferred to this
phase: trapping Ctrl-C, putting the tty in raw mode, and spawning shell commands.
Each is a one-line always-on probe arm riding the `claude-smoke` always-on core
(the task list explicitly permits the A.2 arms to ride this gate, and they ride
`claude-smoke` — **not** `node-smoke`). The probes need no JIT, so they run on
the jitless default core, which is **CI-viable under plain TCG** (no KVM/PKU) —
and they **PASS**, closing the Phase 89 A.2 deferral:

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
proves the entire setup story with zero network and zero secrets, and it
**PASSES on m3OS**: `M3OS_WITH_CLAUDE=1` bundles both `.m3pkg`s, the in-OS solver
resolves `DEPS=node` dependency-first, the launcher chain (`/usr/bin/claude` →
shebang → node → in-process `import()` of `cli.js`) runs to `claude --version` =
`2.1.112` and `claude --help` exits 0, the vendored static-pie `rg --version`
prints, and the A.2 `NODE_SIGINT_OK`/`NODE_SPAWN_OK`/`NODE_RAWMODE_OK` probes
pass — **27/27 on the jitless full-install gate** (the early "24/24" figure was
an `M3OS_CLAUDE_FAST_ITER` reuse-disk run).

**The default bundle is the jitless node, so this core is CI-viable under plain
TCG** — no KVM, no PKU. Jitless V8 never touches the W^X/PKU JIT path, so the
gate is *not* KVM-gated by default (KVM is only a speed knob). Setting
`M3OS_CLAUDE_JIT=1` selects the **90a JIT node** (the runtime-WASM variant the
interactive TUI needs); *that* arm IS KVM/PKU-gated exactly like `node-jit-smoke`
(SKIP-with-reason without `M3OS_KVM=1` on a PKU host, since the JIT node requires
real PKU), and on it the `claude-smoke` serial core PASSES **27/27** (the
`--version`/`--help`/`-p` + `rg` + A.2 set — the same step count as the jitless
full-install gate; the interactive TUI is not part of these counts). The gate
runs at
`--timeout 5400` (the ~130 MB install + cold `cli.js` parse over the slow VFS —
far faster under KVM). Absent the build prerequisites (host C++ toolchain for the
node dep) it prints `SKIP (reason: …)` and returns success.

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

Skip-with-reason when unconfigured (real egress to `api.anthropic.com:443` + a
secret can never be CI-bound) — the always-on core is what CI sees, mirroring
`gh-smoke` / `git-https-smoke`.

**The full interactive `claude` TUI does not yet run, and its visual render is a
tracked follow-up — not merely a manual screenshot.** What *is* proven
automatically: the JIT/WASM runtime the TUI needs (90a's `node-jit-smoke` —
TurboFan optimization + `WebAssembly.Instance`), the W^X-v2 PKU fix above
(unblocking `cli.js` *launch* on the JIT node), and the A.2
raw-mode/SIGINT/spawn primitives the TUI lives on (all passing in the
`claude-smoke` core). What is **not** working is the full interactive launch
itself: a direct QMP/PPM render test (PR-audit, 2026-06-14) on the JIT node
showed it gets through onboarding and JITs under the guarded path, then crashes
on a userspace null-deref tied to unhandled `mremap`/`io_uring` syscalls the
heavy interactive path exercises (Phase 93 syscall-gap territory). Once those
syscall gaps close, the visual proof is a **one-line wire-up of the existing
`htop-render-probe` QMP/PPM harness** (`xtask/src/qmp.rs` + `xtask/src/ppm.rs`):
an interactive `claude` launch driven via QMP `send_key`, captured via
`screendump`, with the PPM analysis asserting a populated TUI (multi-row
non-black text occupancy + frame-to-frame change on keystroke), the
`less-render-probe`/`htop-render-probe` pattern. The serial sentinel is blind to
TUI rendering, so a framebuffer screenshot is the falsifiable evidence — but it
is gated behind the syscall work, not a deferred manual capstone.

## Key Files

| File | Purpose |
|---|---|
| `ports/util/claude-code/Portfile` | Pinned `@anthropic-ai/claude-code@2.1.112` + registry-tarball SHA-256, `CATEGORY=util`, `DEPS=node` |
| `xtask/src/port_build.rs` | `fn build_claude_code` — fetch-and-stage the pinned npm tarball, stage `/usr/lib/claude-code/` + the `/usr/bin/claude` launcher, seal the `.m3pkg`; the vendored-`rg` `readelf -l` static-pie audit |
| `xtask/src/main.rs` | `fn cmd_claude_smoke` / `fn claude_smoke_steps` — serial DSL gate; the `M3OS_WITH_CLAUDE` bundle block in `populate_phase_69d_ports`; the `M3OS_CLAUDE_TOKEN`/`M3OS_CLAUDE_KEY` 0600 credential seeding |
| `xtask/src/qmp.rs`, `xtask/src/ppm.rs` | The headless-framebuffer harness (`QmpClient::screendump` + PPM row-occupancy analysis) reused by `htop-render-probe`; the reusable plumbing for the **tracked** TUI visual-render follow-up — a one-line wire-up once the `mremap`/`io_uring` syscall gaps close (the full interactive `claude` TUI does not yet run) |
| `kernel/src/arch/x86_64/interrupts.rs` | `page_fault_handler` + the new `leaf_pte_flag_bits` helper — the W^X-v2 cross-thread PKU read-recovery fix (grant read access on a `PROTECTION_KEY` read fault against a present executable pkey-tagged page; writes stay gated, non-executable data pages excluded) |
| `kernel/src/arch/x86_64/pkru.rs` | The new `grant_read_access` — clears the key's access-disable bit in the faulting thread's live PKRU (context-switch XSAVE persists it) |
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
- The agent runs on **both** node variants. The jitless node (Phase 89) runs the
  full CLI — `--version`/`--help`/`-p`, search, the A.2 primitives — so the
  always-on core is **CI-viable under plain TCG**. The interactive TUI needs
  `yoga.wasm`, which needs the **Phase 90a JIT variant** (`M3OS_CLAUDE_JIT=1`);
  on a no-PKU CPU the JIT node aborts (it does not degrade to jitless), so *that
  arm* — not the whole gate — is KVM/PKU-gated. The JIT/WASM runtime is proven,
  but the **full interactive TUI does not yet run** — its launch crashes on
  unhandled `mremap`/`io_uring` syscalls the heavy interactive path exercises
  (Phase 93 syscall-gap territory), a tracked follow-up.
- Credentials are the **seeded host-minted OAuth token / API key** plus the
  in-OS `/login` paste-flow whose browser step happens on another device. There
  is no in-OS browser, no MCP / IDE integration, and no multi-user credential
  story — mature hosted-agent ecosystems support far broader integrations than
  m3OS should assume. The real value here is a platform-integration proof point,
  not a claim that m3OS is a full hosted AI workspace.

### The supported-workflow boundary (falsifiable)

With the documented steps a user can reproduce, on m3OS:

1. `M3OS_WITH_CLAUDE=1 cargo xtask image` bundles `claude-code` + the node
   variant under test (jitless by default; the 90a JIT node under
   `M3OS_CLAUDE_JIT=1`).
2. Boot `0.90.1`; `pkg install claude-code` auto-installs `node` first
   (dependency-first solver order, asserted in the gate output).
3. `claude --version` prints `2.1.112`, `claude --help` exits 0, the vendored
   `rg --version` runs, and the A.2 SIGINT/spawn/raw-mode probes pass — fully
   offline, on plain TCG (no KVM/PKU needed for the jitless core;
   27/27 full-install).
4. On a KVM/PKU host with `M3OS_CLAUDE_JIT=1`, the same serial core PASSES on the
   JIT node (27/27) — runtime WASM + the W^X-v2 PKU read-recovery fix unblock
   `cli.js` *launch* on the JIT variant.
5. With a seeded `M3OS_CLAUDE_TOKEN` (or `M3OS_CLAUDE_KEY`) and
   `M3OS_CLAUDE_NET=1`: `claude -p` round-trips `api.anthropic.com`
   (`CLAUDE_API_OK`), and a scripted agent workflow uses the Write tool to create
   `/root/claude-work.txt` with known content — proven *outside* the agent by
   `cat` (`WORKFLOW_FILE_OK`), not the model's own claim. (The as-built arm
   asserts the file write; a shell-command / `git commit` + `git log` assertion is
   a straightforward extension of the same opt-in arm, not yet wired.)
6. The **full interactive `claude` TUI does not yet run** — its launch crashes on
   a userspace null-deref tied to unhandled `mremap`/`io_uring` syscalls the
   heavy interactive path exercises (Phase 93 syscall-gap territory), *not* a
   JIT/PKU gap. Its *visual* render is therefore a **tracked follow-up** that
   becomes a one-line wire-up of the existing `htop-render-probe` QMP/PPM harness
   once those syscall gaps close.

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
