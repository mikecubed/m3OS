# Phase 90b — Claude Code: Task List

**Status:** In Progress (`feat/phase-90b-claude-code`)

## Implementation Progress Log

> Live record kept during `flow:parallel-impl`. Acceptance boxes below are checked as each item lands + is validated.

**Track A — DONE (host-side spike, 2026-06-13).** Key finding that shaped the build:

- **Pin = `@anthropic-ai/claude-code@2.1.112`** — `URL=https://registry.npmjs.org/@anthropic-ai/claude-code/-/claude-code-2.1.112.tgz`, `SHA256=84379969ea53a0e5fd231a8f77debe4c7cb17dd971f4809d10d33f9aeca5de09` (~18.7 MB tarball, ~49 MB unpacked).
- **Why not `latest` (2.1.177):** the spike found that claude-code **repackaged to a native Bun/JavaScriptCore single-file binary at `2.1.113`** (the wrapper's `install.cjs` copies a per-platform ~500 MB native binary over a stub; no `cli.js`, no Node). That model does **not** use the Node runtime at all — it would invalidate the entire `DEPS=node` + Phase 89/90a JIT-Node dependency chain this phase is built on. **`2.1.112` is the last version shipping the classic `cli.js` (9.3 MB JS bundle) + `yoga.wasm` (88 KB TUI layout engine) + `vendor/ripgrep/` model** the task list assumed. Pinning it is the faithful, correct way to deliver the phase as designed; the native-binary line is a separate future port (a Bun runtime, not Node).
- **A.1 validated:** `node cli.js --version` → `2.1.112 (Claude Code)` (exit 0, 0.26 s / 178 MB RSS under host node v24); `--help` renders the full CLI. `yoga.wasm`/TUI WASM path de-risked by 90a's `node-jit-smoke` (already proves `WebAssembly.instantiate` on the m3OS JIT node).
- **B.2 (ripgrep) — no fallback port needed:** vendored `vendor/ripgrep/x64-linux/rg` is **`static-pie` linked, NO `PT_INTERP`** (6.5 MB). m3OS's ELF loader supports `ET_DYN` static-PIE (no-interpreter path, `kernel/src/mm/elf.rs`), so the vendored rg is used directly (confirmed on-OS in D.1). The B.2 `build_ripgrep` fallback stays a documented contingency, not built. The `audio-capture.node` (dynamic, optional) and `seccomp` helper (m3OS has no seccomp) vendor dirs are pruned.
- **A.2 relocation:** the SIGINT/spawn/rawmode interactive-substrate probes are added to **`node-smoke`** (jitless, CI-visible, no JIT needed) rather than the KVM-gated `claude-smoke`, so they are always-on in CI.
- **D-gate model:** the bundled node is the **90a JIT variant** (the TUI needs WASM/JIT), which aborts on a no-PKU CPU; so `claude-smoke` is **KVM/PKU-gated like `node-jit-smoke`** (SKIP-with-reason without `M3OS_KVM=1` on a PKU host) for ALL arms, not just the TUI one.

**Status (original):** Planned
**Source Ref:** phase-90b
**Depends on:** Phase 85 (Cross-Compiled Toolchains — `.m3pkg` substrate + offline `pkg` + the `DEPS=` solver) ✅, Phase 86 (Networking and GitHub — CA trust, DNS, TLS egress, `git`-over-HTTPS, the `gh` 0600-credential-seeding precedent) ✅, Phase 89 (Node.js — the static Node 22 runtime, `timerfd` event loop, always-on in-kernel-TCP egress, opt-in live HTTPS to the npm registry) ✅, Phase 90a (Memory Protection Keys — the JIT/WASM-capable node variant the interactive TUI requires; [90a tasks](./90a-memory-protection-keys-tasks.md))
**Goal:** Run Claude Code natively inside m3OS — **interactive TUI first**, on the Phase 90a JIT node variant — as a content-addressed `.m3pkg`: a pinned `@anthropic-ai/claude-code` npm tarball staged host-side into a `claude-code` port (`DEPS=node`, so the offline solver pulls the runtime), a `/usr/bin/claude` launcher that pins the supported environment (CA bundle, no auto-update, no non-essential telemetry egress), credential handling that supports **subscription use** (a host-minted `claude setup-token` OAuth token seeded at mode 0600, never crossing serial; `ANTHROPIC_API_KEY` as the API-billing alternative; the in-OS `/login` paste-flow as the documented human path once the TUI works), and a `claude-smoke` gate whose always-on core proves install + launch offline while the authenticated API round-trip, the file/shell/git agent workflow, and the TUI render proof are opt-in live arms (skip-with-reason, mirroring `gh-smoke`). Bump the kernel to `0.90.1` and ship the learning doc.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one. (Mirrors the [Phase 89](./89-nodejs-tasks.md) task-list style.) Where a task only *validates* substrate that already exists, the acceptance item says so and points at the existing symbol to reuse rather than reimplement.
>
> **Two evaluation findings shaped this plan; the second spawned Phase 90a:**
>
> 1. **The supported install path is a pre-bundled `.m3pkg`, not a live `npm install -g`.** Phase 89 D.2 proved npm *launches* and *reaches the registry* over real HTTPS, but loading npm's ~thousands of JS files over the ring-3 VFS made full `npm install` completion impractical — a per-file round-trip-latency limit that the Phase 87 bulk-I/O coalescing doesn't remove (87's win is on large sequential files; npm's workload is thousands of tiny files with per-file `open`/`stat`/`write` IPC round-trips) — and repo CI has no outbound egress anyway. So the reproducible, documented path is the same one every heavy port uses: fetch + stage host-side, seal as `.m3pkg`, install offline from `/usr/pkg/` (Track B). The live `npm install -g @anthropic-ai/claude-code` remains an opt-in real-internet arm, documented as such.
> 2. **The interactive TUI requires WASM, which requires the Phase 90a JIT variant.** Claude Code's terminal UI ships a WebAssembly layout engine (`yoga.wasm`); the Phase 89 jitless config disallows runtime WASM code generation, and m3OS forbids unguarded RWX. Rather than settling for a print-mode floor, **Phase 90a delivers PKU-backed JIT** (W^X v2 — the invariant is strengthened, not relaxed) and a JIT node variant on which WASM works; this phase consumes that variant. The jitless `claude -p` path remains the documented **fallback if 90a slips** and the always-available automation floor — not the milestone.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Feasibility spikes + runtime-substrate validation (Claude Code bundle under the 90a JIT variant host-side; SIGINT/raw-mode/subprocess on-OS) | 89, 90a-D.2 | Planned |
| B | Packaging + install path (`ports/util/claude-code`, ripgrep strategy, `M3OS_WITH_CLAUDE` bundling of the JIT node variant) | A, 85a | Planned |
| C | Launch environment + credential handling (`/usr/bin/claude` env contract; 0600 OAuth-token/key seeding; headless onboarding; in-OS `/login` story) | B | Planned |
| D | `claude-smoke` gate (always-on offline install+launch core; opt-in authenticated API + file/shell/git workflow + TUI render arms) | B, C | Planned |
| E | Docs + release closeout (learning doc, revived standalone roadmap, README rows, `0.90.1` version bump) | A–D | Planned |

---

## Track A — Feasibility Spikes + Runtime-Substrate Validation

> The runtime substrate is **delivered by Phases 89 + 90a** — static Node 22 with the `timerfd` event loop, the libuv threadpool (`FUTEX_CMP_REQUEUE`), always-on in-kernel-TCP egress, validated live HTTPS + registry reach, and (90a) PKU-backed JIT with working WASM. Track A does not touch the kernel; it validates the Claude Code bundle against that substrate host-side and closes the interactive-substrate validation Phase 89 A.2 explicitly deferred to this phase.

### A.1 — Host-side feasibility spike: the Claude Code bundle under the 90a JIT node variant

**Files:**
- the JIT-variant node binary from 90a D.2 (`build_node` with `M3OS_NODE_JIT=1` — run host-side, no QEMU) **and** the jitless binary (the fallback arm)
- a scratch unpack of the pinned `@anthropic-ai/claude-code` registry tarball (becomes the Portfile pin in B.1)

**Symbol:** the `build_node` artifacts (`xtask/src/port_build.rs:build_node`) exercised against `cli.js`; the same host-run validation trick that caught 89 B.2's `--v8-lite-mode` startup abort
**Why it matters:** this spike validates the phase's primary workflow before any OS time is spent: under the JIT variant, `yoga.wasm` should instantiate and the TUI render path should work — confirm it rather than assume it, and confirm the bundle has no *other* platform surprises. The jitless arm establishes the documented fallback (`claude -p` headless) in case 90a slips. It also measures bundle parse/cold-start cost under both configs, which sizes the Track D gate timeouts.

**Acceptance:**
- [x] The pinned tarball version + its registry SHA-256 are recorded (`2.1.112` / `84379969…de09`, become the B.1 Portfile pin), and node runs `cli.js --version` (→ `2.1.112 (Claude Code)`) and `--help` **on the host** successfully. *(Validated with host node v24; the m3OS JIT variant is exercised on-OS in D.1. The pinned version pre-dates the 2.1.113 native-binary repackaging — see Progress Log.)*
- [x] The TUI path is de-risked: `yoga.wasm` is present in the 2.1.112 bundle and 90a's `node-jit-smoke` already proves `WebAssembly.instantiate` on the m3OS JIT node; the on-OS rendered-UI proof is D.2's QMP/PPM arm. *(The "jitless fallback" arm is moot for the supported pin: 2.1.112 needs the JIT node for the TUI; jitless `claude -p` remains the documented degraded mode.)*
- [x] Cold-start cost measured host-side: `--version` 0.26 s / 178 MB RSS under host node v24. On-OS cold `cli.js` load over the slow VFS is the same class as `node-smoke`'s cold exec → Track D uses `--timeout 5400` + the `M3OS_KVM=1` recommendation (KVM is also required for the JIT node's PKU).
- [x] Bundle platform expectations enumerated: `cli.js` resolves `yoga.wasm` + `vendor/` relative to its own dir (preserved under `/usr/lib/claude-code/`); the optional `audio-capture.node` (dynamic) + `seccomp` helper degrade gracefully on m3OS and are pruned; `os.homedir()`/`/tmp`/`/dev/null` already exist on the m3OS VFS.

### A.2 — On-OS interactive-substrate validation: SIGINT, raw mode, subprocess spawn

**Files:**
- `xtask/src/main.rs` (`node_smoke_steps` at `:15197` — new always-on probe arms, or a dedicated `/usr/src/node-interactive-probe.js` fixture in `populate_ext2_files`)

**Symbol:** `process.on('SIGINT')` (the libuv self-pipe signal path — the explicit Phase 89 A.2 deferred follow-up), `process.stdin.setRawMode(true)` (termios over the PTY stack), `child_process.spawn` (libuv fork/exec)
**Why it matters:** an interactive CLI agent lives on exactly these three primitives: it traps Ctrl-C, puts the tty in raw mode, and spawns shell commands capturing stdout/stderr. Phase 89 validated none of them in-OS (its A.2 recorded the self-pipe *decision* and deliberately deferred the explicit in-Node signal assertion to "Phase 90's interactive-CLI use"). Each is a one-line probe arm riding the existing `node-smoke` boot.

**Acceptance:**
- [ ] `NODE_SIGINT_OK`: a probe registers `process.on('SIGINT')`, self-signals (`process.kill(process.pid, 'SIGINT')`), and the handler fires — proving libuv's self-pipe signal path (`pipe2` + `rt_sigaction`) end-to-end on m3OS, the deferred 89 A.2 item.
- [ ] `NODE_SPAWN_OK`: `child_process.spawn('/bin/sh', ['-c', 'echo spawned'])` captures `spawned` on stdout and a 0 exit code — the libuv fork/exec + pipe-capture path Claude Code's shell tool uses.
- [ ] `NODE_RAWMODE_OK`: `process.stdin.isTTY` is true under the m3OS terminal and `setRawMode(true)` then `setRawMode(false)` succeed without throwing (termios `ICANON`/`ECHO` toggling over the PTY stack).
- [ ] The arms are always-on in the gate that carries them (no network needed), and a failure in any one is a hard gate failure, not a skip.

---

## Track B — Packaging + Install Path

### B.1 — `ports/util/claude-code/Portfile` + `build_claude_code` (npm-tarball fetch-and-stage, `DEPS=node`)

**Files:**
- `ports/util/claude-code/Portfile` (new — pinned `@anthropic-ai/claude-code` version + registry-tarball SHA-256 from A.1, `CATEGORY=util`, `DEPS=node`)
- `xtask/src/port_build.rs` (new `fn build_claude_code`; dispatch arm in `fn port_build` at `:1236`; `port_deps` arm `:774` → `"claude-code" => &["node"]`; `build_recipe_id` arm `:339` + the distinctness-test list; `compute_port_key_inner` `:861`; `BUILDABLE_PORTS` `:1083`)

**Symbol:** `build_claude_code` (a fetch-and-stage recipe — no compiler; modeled on `build_go`'s download-a-prebuilt-artifact shape, not the musl-gcc compile path)
**Why it matters:** this is the phase's "supported install path" made reproducible: the npm tarball is fetched once host-side with a pinned SHA, unpacked into the stage as `/usr/lib/claude-code/` (the package's `node_modules/@anthropic-ai/claude-code` payload), a `/usr/bin/claude` launcher is staged (its env contract is C.1), and the whole thing seals as a content-addressed `.m3pkg` whose `DEPS=node` makes the in-OS solver auto-install the Phase 89 runtime — the same dependency-first proof `pkg install tmux`→`libevent` established in 85a. No live network inside the guest is needed to install.

**Acceptance:**
- [ ] `ports/util/claude-code/Portfile` pins the A.1 version + SHA-256, `CATEGORY=util`, `DEPS=node` — and `port_deps` agrees with `"claude-code" => &["node"]`.
- [ ] `build_claude_code` downloads the pinned registry tarball (SHA-verified), stages the package payload under `<stage>/usr/lib/claude-code/` and the launcher at `<stage>/usr/bin/claude`, and seals a `target/pkgcache/<key>.m3pkg`; a second `cargo xtask port build claude-code` is a pure pkgcache hit (`PKGCACHE: hit`, zero fetches).
- [ ] `build_recipe_id("claude-code")` is non-empty + distinct (added to the distinctness unit test); the content key folds the pinned tarball SHA so a version bump can never serve a stale hit.
- [ ] `"claude-code"` is in `BUILDABLE_PORTS` (so `port build all` + `port list` see it), and `cargo xtask check` stays green (the xtask host tests cover the new Portfile/key wiring).

### B.2 — ripgrep strategy: vendored binary audit + static-musl fallback

**Files:**
- `xtask/src/port_build.rs` (`build_claude_code` — the vendored-`rg` audit + staging decision)
- `ports/util/ripgrep/Portfile` + `fn build_ripgrep` (new — **only if** the audit fails; a `cargo build --release --target x86_64-unknown-linux-musl` cross of ripgrep, the first Rust-built port)

**Symbol:** the `readelf -l` `PT_INTERP` check (the same fully-static proof `assert_node_layout` uses) applied to the bundle's `vendor/ripgrep/x64-linux/rg`; the `USE_BUILTIN_RIPGREP=0` env switch in the C.1 launcher
**Why it matters:** Claude Code's file-search tool shells out to a *vendored platform ripgrep binary*. If that vendored `rg` is dynamically linked against glibc it cannot run on m3OS (the custom `ld-musl` has no real `libc.so`) and every search silently degrades or fails. The audit is one `readelf` host-side; the fallback (a static-musl `rg` on `PATH` + `USE_BUILTIN_RIPGREP=0` in the launcher) is cheap because ripgrep is a pure-Rust crate and the repo already has a Rust toolchain.

**Acceptance:**
- [ ] The vendored `rg` is audited host-side: `readelf -l` shows no `PT_INTERP` **and** it executes under the m3OS-equivalent constraint (static), **or** the audit failure is recorded and the fallback path is taken — the decision is written into `docs/90b-claude-code.md`.
- [ ] If the fallback is taken: `ports/util/ripgrep` builds a fully-static musl `rg` (no `PT_INTERP`), it is added to the `claude-code` `DEPS=` chain (or staged into the same `.m3pkg`), and the C.1 launcher exports `USE_BUILTIN_RIPGREP=0` so Claude Code uses the `PATH` `rg`.
- [ ] On m3OS, `rg --version` (whichever binary is the supported one) prints its version over serial — asserted in the Track D always-on core, so a search-tool regression is a gate failure, not a silent degradation.

### B.3 — `M3OS_WITH_CLAUDE` opt-in image bundling

**File:** `xtask/src/main.rs` (`fn populate_phase_69d_ports` — a new `M3OS_WITH_CLAUDE` env-gated bundle block modeled on the `M3OS_WITH_NODE` block at `:21027`)
**Symbol:** the `M3OS_WITH_CLAUDE` guard in `populate_phase_69d_ports`; the Track D gate sets `std::env::set_var("M3OS_WITH_CLAUDE", "1")` (mirroring `cmd_node_smoke` at `:15036`)
**Why it matters:** the `claude-code` artifact plus its `DEPS=node` runtime together exceed ~130 MB of `/usr/pkg/` payload; like clang/gh/node it must be gated out of default images so routine `cargo xtask image`/`run` stays lean, and the bundle block must ship **both** `.m3pkg`s (claude-code *and* node) so the offline solver can actually resolve `DEPS=node` in-guest.

**Acceptance:**
- [ ] With `M3OS_WITH_CLAUDE` unset, the default image contains no `claude-code.m3pkg` and no behavior change (the block is a no-op; image size unaffected).
- [ ] With `M3OS_WITH_CLAUDE=1`, the block `pkg_format::verify`s and bundles `usr/pkg/claude-code.m3pkg` + `usr/pkg/claude-code.meta` (`VERSION=<pin> DEPS=node`) **and** ensures the node `.m3pkg`/`.meta` are bundled too — **the 90a JIT variant** (the artifact the TUI requires), so `pkg install claude-code` solves `DEPS=node` offline against the right runtime; the jitless artifact remains the default-image story and the documented fallback bundle if 90a slips.
- [ ] If the sealed artifact is absent the block fails fast with an actionable `cargo xtask port build claude-code` message (mirroring the node/clang blocks), rather than building a broken image.

---

## Track C — Launch Environment + Credential Handling

### C.1 — `/usr/bin/claude` launcher: the pinned supported environment

**Files:**
- `xtask/src/port_build.rs` (`build_claude_code` — the staged launcher script content)
- `docs/90b-claude-code.md` (records the env contract — see E.1)

**Symbol:** the `/usr/bin/claude` launcher (`exec node /usr/lib/claude-code/cli.js "$@"` behind the env exports); the kernel `#!` shebang support landed in Phase 89
**Why it matters:** Claude Code's defaults assume a mainstream Linux: it auto-updates itself via npm (impractical over the VFS and version-drifts the sealed `.m3pkg`), emits non-essential telemetry/error-reporting traffic (dead weight and a confusing failure mode on a box with no default egress), and discovers TLS roots from the system store (m3OS's is the Phase 86a bundle at a fixed path). The launcher is where the supported configuration is *pinned* rather than hoped for — every env line is a documented support-boundary decision.

**Acceptance:**
- [ ] The launcher exports `NODE_EXTRA_CA_CERTS=/etc/ssl/certs/ca-certificates.crt` (the Phase 86a bundle — Node's bundled OpenSSL validates `api.anthropic.com` against it), `DISABLE_AUTOUPDATER=1` (the sealed `.m3pkg` is the only supported delivery), `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` (no telemetry/Statsig/Sentry egress attempts), and the B.2 ripgrep switch if taken — then `exec`s the bundled `cli.js` under `/usr/bin/node`.
- [ ] `claude --version` on m3OS prints the pinned version through the launcher (proving the shebang/exec chain + relocated install layout), asserted in the Track D core.
- [ ] Every launcher env line is documented in `docs/90b-claude-code.md` with one sentence of *why* — the explicit support-boundary record the phase doc requires.

### C.2 — Credential handling: 0600 subscription-token/key seeding + headless onboarding + the in-OS `/login` story

**Files:**
- `xtask/src/main.rs` (`cmd_claude_smoke` — credential seeding via **dedicated** `M3OS_CLAUDE_TOKEN` (subscription OAuth token) / `M3OS_CLAUDE_KEY` (API key) env vars read by `populate_ext2_files`, modeled on the `GH_TOKEN` seeding in `cmd_gh_smoke` at `:15403`; onboarding-state seeding for `/root/.claude.json`)
- `docs/90b-claude-code.md` (the credential posture — see E.1)

**Symbol:** the `M3OS_CLAUDE_TOKEN`/`M3OS_CLAUDE_KEY` → `/root/.claude/` mode-0600 seeding path; `CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY` exported in-guest; the pre-seeded `/root/.claude.json` onboarding/trust state
**Why it matters:** this is the phase doc's *critical, non-deferrable* credential item, and **subscription use is first-class**: a Pro/Max user mints a long-lived OAuth token once on the host with `claude setup-token` (the browser dance happens there) and it is seeded onto m3OS exactly like the `gh` precedent (86e) — staged at mode 0600 under a dedicated env name (never the user's ambient variable, so it can't bake into a routine image build), the **value never crosses the serial console** (only paths do), and absence means skip-with-reason, never failure. `ANTHROPIC_API_KEY` is the API-billing alternative through the same seeding path. The interactive in-OS `/login` paste-flow (the TUI displays a URL; the user visits it on any browser-equipped device and pastes the code back into the m3OS terminal) is the documented *human* path once the TUI works — it cannot be the gate path because it is interactive by design.

**Acceptance:**
- [ ] With `M3OS_CLAUDE_TOKEN=<token>` set, the gate seeds the token at mode `0600` under `/root/.claude/` and exports `CLAUDE_CODE_OAUTH_TOKEN` in-guest from that file — subscription-backed headless use, value never in any serial send/expect string, any log, or the repo/CI (asserted by construction, mirroring `gh-smoke`). `M3OS_CLAUDE_KEY` → `ANTHROPIC_API_KEY` works identically as the API-billing alternative; the gate prefers the token when both are set and records which mode authenticated.
- [ ] `/root/.claude.json` is pre-seeded with completed-onboarding/trust state so a headless `claude -p` invocation proceeds with no interactive first-run prompt; the seeded fields are enumerated in the docs.
- [ ] An in-guest `ls -l` asserts the credential file is `-rw-------` (the same permission assertion `gh-smoke` makes on `hosts.yml`).
- [ ] The in-OS `/login` paste-flow is documented as the human path (and, if the TUI lands per 90a, manually validated once and recorded — not gate-automated); the docs state the full posture: credentials live only in 0600 files under `/root/.claude/`, never serial, never CI; multi-user/enterprise credential stories are out of scope (Deferred).

---

## Track D — `claude-smoke` Gate

### D.1 — Always-on offline core: image → boot → `pkg install claude-code` → launch

**Files:**
- `xtask/src/main.rs` (new `fn cmd_claude_smoke` + `fn claude_smoke_steps`, modeled on `cmd_node_smoke` at `:14987`/`node_smoke_steps` at `:15197`; a new `SMOKE_EXIT_CLAUDE_SMOKE_FAILED` const after `SMOKE_EXIT_NODE_SMOKE_FAILED = 82` at `:257`)
- `AGENTS.md` (opt-in regression row `M3OS_CLAUDE_REGRESSION=1`, appended after the `node-smoke` row)
- `.githooks/pre-push` (an `M3OS_CLAUDE_REGRESSION` block after the node block at `:569`)

**Symbol:** `cmd_claude_smoke`, `claude_smoke_steps`
**Why it matters:** the phase has no value if the agent cannot be set up reproducibly — this core proves the entire offline story with zero network and zero secrets: the `M3OS_WITH_CLAUDE` image bundles both `.m3pkg`s (claude-code + the 90a JIT node variant), the in-OS solver resolves `DEPS=node` dependency-first, and the launcher chain (`/usr/bin/claude` → shebang → static node → `cli.js`) runs to a version print. It reuses the serial `SmokeStep` DSL, `boot_and_login_steps`, and the heavy-install `WaitPassOrFail` pattern exactly as the node/clang gates do, and honors `M3OS_KVM=1` + a fast-iter reuse-disk mode (the cold cli.js parse over the VFS is the same class of slow as node-smoke; KVM also exposes the host PKU the JIT variant needs).

**Acceptance:**
- [ ] The gate sets `M3OS_WITH_CLAUDE=1`, builds the image, boots `0.90.1`, and `pkg install claude-code` succeeds — with the solver auto-installing `node` first (asserted in output: the dependency-first install order), the end-to-end `DEPS=` proof.
- [ ] `claude --version` over serial prints the pinned version (`CLAUDE_VERSION_OK`-class assertion) and `claude --help` exits 0 — install + launcher + runtime, fully offline.
- [ ] The B.2 ripgrep assertion (`rg --version`) and the A.2 interactive-substrate arms ride this gate if not already carried by `node-smoke` — all always-on.
- [ ] Wired opt-in: the `AGENTS.md` regression row documents `M3OS_CLAUDE_REGRESSION=1` (+ the `M3OS_CLAUDE_NET`/`M3OS_CLAUDE_KEY` parentheticals), `.githooks/pre-push` runs `cargo xtask claude-smoke --timeout 5400` when set, and absent the build prerequisites (host toolchain for the node dep) the gate prints `SKIP (reason: …)` and returns success.

### D.2 — Opt-in live arms: authenticated API round-trip + the file/shell/git agent workflow + the TUI render proof

**Files:**
- `xtask/src/main.rs` (`cmd_claude_smoke` — `attempt_net` gating on `M3OS_CLAUDE_NET=1`, modeled on the `M3OS_NODE_NET` gating; `+rdrand,+rdseed` CPU flags when live, like the TLS gates; the C.2 credential seeding on `M3OS_CLAUDE_TOKEN`/`M3OS_CLAUDE_KEY`)
- `xtask/src/qmp.rs` + `xtask/src/ppm.rs` (the existing headless-framebuffer harness — the TUI render arm reuses `QmpClient::screendump` + the PPM row-occupancy analysis, the `less-render-probe` pattern)
- `AGENTS.md` (the row parenthetical documenting the env vars)

**Symbol:** the `attempt_net` arms in `claude_smoke_steps`; non-interactive `claude -p` as the scriptable workflow driver; `QmpClient::screendump` for the TUI proof
**Why it matters:** this is the phase's actual milestone — the agent *does work* on m3OS: a prompt goes out over real HTTPS to `api.anthropic.com` (Node's bundled OpenSSL + the 86a CA bundle + c-ares DNS), a response comes back, the agent's tools touch the real OS (write a file, run a shell command, make a git commit with the 85b/86c git), **and the interactive TUI visibly renders** — which a serial sentinel can never prove (the serial harness is blind to graphical/TUI output; only a framebuffer screenshot is falsifiable evidence). Like every networked gate it is opt-in (real egress + a real secret can never be CI-bound) and **skip-with-reason** when unconfigured — the always-on D.1 core is what CI sees.

**Acceptance:**
- [ ] **API round-trip (`M3OS_CLAUDE_NET=1` + a C.2 credential):** `claude -p 'Reply with exactly CLAUDE_API_OK and nothing else'` completes a full TLS 1.3 handshake + cert-chain validation against `api.anthropic.com` and prints `CLAUDE_API_OK` over serial — the authenticated network path end-to-end, subscription-token mode preferred (the `claude -p` + `CLAUDE_CODE_OAUTH_TOKEN` combination is itself an assertion that headless subscription auth works).
- [ ] **File/shell/git workflow (same arm):** in a pre-seeded git repo on the data disk, a scripted `claude -p` invocation (tool permissions pre-granted via the C.2 seeded settings or explicit `--allowedTools`) creates a named file with known content, runs a shell command, and makes a git commit — asserted *outside* the agent by `cat` of the file and `git log --oneline` showing the new commit. The assertion trusts the filesystem and git, never the model's own claim of success.
- [ ] **TUI render proof (same arm, the 90a payoff):** an interactive `claude` launch in the m3OS terminal is driven via QMP `send_key` and captured via `screendump`; the PPM analysis asserts a populated TUI (multi-row non-black text occupancy, frame-to-frame change on keystroke) — the `less-render-probe` pattern, not a launch sentinel. If 90a has not landed, this arm prints `SKIP (reason: requires the 90a JIT node variant)` and the jitless `-p` arms above still run — the documented fallback, exercised.
- [ ] **Skip-with-reason:** with `M3OS_CLAUDE_NET`/`M3OS_CLAUDE_TOKEN`/`M3OS_CLAUDE_KEY` unset the gate prints a NOTE naming exactly what was skipped and why (real egress to `api.anthropic.com:443` + a secret), and exits success — mirroring `gh-smoke`/`git-https-smoke`.

---

## Track E — Documentation + Release Closeout

### E.1 — Create the Phase 90b learning doc

**Files:**
- `docs/90b-claude-code.md` (new — aligned learning-doc template at `docs/appendix/doc-templates.md:167`–`214`, modeled on `docs/89-nodejs.md`)
- `docs/README.md` (link it in the `### Phase-Aligned Learning Docs` table after the Phase 89 row)

**Symbol:** the aligned learning-doc header block (`**Aligned Roadmap Phase:** Phase 90` / `**Status:** …` / `**Source Ref:** phase-90`)
**Why it matters:** the phase doc's Learning Documentation Requirement names this file explicitly. It is also where the phase's honesty lives: the install path (pre-bundled `.m3pkg`, and *why* live npm is not the supported path), the runtime dependency chain (node → launcher env → CA bundle), the file/shell/git integration, the credential posture, the A.1 WASM/TUI decision, and the **exact supported workflow** vs. the explicit non-goals (auto-update, telemetry, browser OAuth, MCP/optional integrations, multi-user credentials, GUI integration).

**Acceptance:**
- [ ] `docs/90b-claude-code.md` exists with all aligned-template sections (Overview, What This Doc Covers, Core Implementation, Key Files, the differs-from-later-work section, Related Roadmap Docs, Deferred or Later-Phase Topics) and records the A.1 supported-workflow decision, the C.1 env contract line-by-line, and the C.2 credential posture.
- [ ] It is linked from `docs/README.md`'s learning-docs table and cross-links the Phase 90a/90b design + task docs.
- [ ] The supported-workflow boundary is stated falsifiably (what a user can reproduce with the documented steps) and the non-goals list matches the design doc's Deferred section.

### E.2 — Revive the standalone Claude Code roadmap

**Files:**
- `docs/claude-code-roadmap.md` (new — revived from `docs/archived/claude-code-roadmap.md`)
- `docs/README.md` (the Standalone Roadmaps row at `:101` — repoint from `./archived/claude-code-roadmap.md` to `./claude-code-roadmap.md`)

**Symbol:** the `> Revived YYYY-MM-DD for **Phases 90a/90b — PKU JIT + Claude Code**.` blockquote prelude (the exact precedent: `docs/nodejs-roadmap.md`, revived in 89 E.2)
**Why it matters:** the design doc's Primary Components and Related Documentation name `docs/claude-code-roadmap.md`, but the file currently exists only under `docs/archived/` — the evaluation found this gap. The archived copy's requirements table is also stale (it lists git as "Not available" and TLS/DNS as missing; all landed in 85b/86); reviving means reconciling with the as-built world — including the 90a PKU/JIT story — not just copying.

**Acceptance:**
- [ ] `docs/claude-code-roadmap.md` exists, opening with the `> Revived …` blockquote, with the requirements table reconciled to as-built reality (git ✅ 85b/86c, TLS/DNS/CA ✅ 86a/86c, Node ✅ 89, the W^X→PKU JIT story → 90a, npm-over-VFS limitation → `.m3pkg` install path) and the dependency diagrams updated to the real phase numbers.
- [ ] `docs/README.md:101` points at `./claude-code-roadmap.md` with a "revived for Phases 90a/90b" note, no longer the archived path.

### E.3 — Update the roadmap README row, the design doc, and the AGENTS.md inventory

**Files:**
- `docs/roadmap/README.md` (the Phase 90b row — Tasks cell links this doc; Status + Primary Outcome sharpen on landing; the Mermaid post-1.0 graph shows 89 → 90a → 90b)
- `docs/roadmap/90b-claude-code.md` (Companion Task List → link this doc)
- `AGENTS.md` (regression-table row for `claude-smoke`; the capability inventory)

**Symbol:** the README Status/Tasks cells; the AGENTS.md "Package management" toolchain bullet
**Why it matters:** `docs/roadmap/README.md` is the authoritative phase index and AGENTS.md the always-loaded inventory. Per the AGENTS.md keep-it-small policy, Claude Code is delivered through the *existing* capability classes (the `.m3pkg` substrate + the Node runtime), so it **folds into the existing toolchain bullet** as a clause — it does not get a new capability bullet unless review concludes a hosted CLI agent is genuinely a new class. (90a's W^X v2/PKU clause folds into the CPU-hardening bullet via its own E.2.)

**Acceptance:**
- [ ] The Phase 90b README row's Tasks cell links this doc (done at authoring time); Status flips `Planned` → `Complete` (+ Primary Outcome sharpened to the as-built result) only on landing.
- [ ] `docs/roadmap/90b-claude-code.md`'s Companion Task List section links `./tasks/90b-claude-code-tasks.md`.
- [ ] AGENTS.md gains the `claude-smoke` / `M3OS_CLAUDE_REGRESSION=1` regression-table row, the toolchain bullet folds in Claude Code as a clause, and no new capability bullet is added (per the maintenance policy).

### E.4 — Bump kernel crate `0.90.0` → `0.90.1`

**File:** `kernel/Cargo.toml`
**Symbol:** `[package] version = "0.90.1"` (Phase 90a takes the `0.90.0` minor; this sub-phase takes the patch, mirroring how the 86a–f sub-phases shared the 0.86.x line)
**Why it matters:** the bump is how the landing is recorded in the boot banner and `uname` (both derive from `env!("CARGO_PKG_VERSION")`), and the `claude-smoke` boot banner asserting `0.90.1` is the cheap proof the cut shipped — exactly the 89 E.4 pattern.

**Acceptance:**
- [ ] `kernel/Cargo.toml:3` reads `version = "0.90.1"` (+ `Cargo.lock` updated), and `AGENTS.md` line 7 reads `kernel **v0.90.1**`.
- [ ] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + host tests incl. the new `build_recipe_id` distinctness entry); exit 0.
- [ ] The `claude-smoke` boot banner / `uname` reports `0.90.1` (rides the D.1 run).

---

## Documentation Notes

- **What changed relative to the previous phase.** Phase 89 delivered the runtime and Phase 90a the JIT/WASM-capable variant; Phase 90b adds **no kernel work at all** (Track A is validation-only) — the deliverable is packaging, environment pinning, credential handling, the TUI proof, and an honest supported-workflow boundary. The one Phase 89 leftover this phase closes is the explicitly deferred A.2 in-Node `SIGINT` assertion (here A.2's `NODE_SIGINT_OK`).
- **The install-path reframing replaces the older plan.** The archived standalone roadmap's "Phase E: `npm install -g @anthropic-ai/claude-code`" is **replaced** by the sealed-`.m3pkg` path: Phase 89 proved live npm reaches the registry but cannot practically complete an install over the ~200 KB/s ring-3 VFS under jitless V8. Live `npm install -g` survives only as a documented opt-in real-internet arm, not the supported path.
- **Honesty / explicit non-goals.** No auto-update (`DISABLE_AUTOUPDATER=1` — the `.m3pkg` is the delivery), no non-essential telemetry egress, no in-OS browser (auth is the seeded OAuth token / API key, plus the `/login` paste-flow whose browser step happens on another device), no MCP servers / IDE integrations / optional protocol extensions, no multi-user credential story, no GUI integration, no local-model alternative. The interactive TUI is the milestone **on the 90a JIT variant**; on jitless configurations the supported workflow degrades to non-interactive `claude -p`, and the docs say so plainly.
- **Secret hygiene is by construction, not by review.** The `M3OS_CLAUDE_KEY` seeding copies the `gh-smoke` pattern verbatim: dedicated env name (never ambient), mode-0600 file on the data disk, value never in a serial string, skip-with-reason when absent. Any deviation from that pattern is a review blocker.
- **Prefer exact targets.** Reference `build_claude_code`, `cmd_claude_smoke`, the `M3OS_WITH_CLAUDE` block in `populate_phase_69d_ports`, and the launcher env lines by name — not "the port" or "the gate".
- **Cross-links.** Companion design doc: [Phase 90b — Claude Code](../90b-claude-code.md). JIT substrate: [Phase 90a — Memory Protection Keys](./90a-memory-protection-keys-tasks.md) (the node variant the TUI depends on). Runtime predecessor: [Phase 89 — Node.js](./89-nodejs-tasks.md) (Track D's npm/TLS path and the deferred interactive-substrate items). Packaging substrate: [Phase 85a](./85a-package-infrastructure-tasks.md). Credential precedent: [Phase 86e — GitHub CLI](./86e-github-cli-tasks.md) (`GH_TOKEN` 0600 seeding). Standalone narrative: `docs/claude-code-roadmap.md` (revived in E.2).
