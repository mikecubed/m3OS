# Phase 90b - Claude Code

**Status:** Complete (install + launch + headless `claude -p` + **interactive TUI renders**) — `claude-smoke` PASSES on m3OS (jitless + the 90a JIT node): Claude Code installs, launches, runs `--version`/`--help`/`-p`, **and its interactive TUI renders on the 90a JIT node** (proven by an automated QMP/PPM render arm — 592 changed band scanlines vs the empty-prompt baseline; the captured screenshot shows the rendered "Welcome to Claude Code v2.1.112" onboarding splash). Two fixes made the TUI work: the W^X-v2 cross-thread PKU read-recovery kernel fix (needed for `cli.js` to launch) and a node build switch from `--with-intl=small-icu` to **full-icu** (small-icu lacked the ICU break-iterator data `Intl.Segmenter` needs → `JSSegments::Create` null-deref; the earlier `mremap`/`io_uring` syscalls were red herrings, correctly returning `-ENOSYS`). The JIT/WASM runtime + the A.2 interactive primitives are also proven. See [the task list's Implementation Progress Log](./tasks/90b-claude-code-tasks.md) and [the learning doc](../90b-claude-code.md).
**Source Ref:** phase-90b
**Depends on:** Phase 85 (Cross-Compiled Toolchains) ✅, Phase 86 (Networking and GitHub) ✅, Phase 89 (Node.js) ✅, Phase 90a (Memory Protection Keys — the JIT/WASM-capable Node variant the interactive TUI requires)
**Builds on:** Uses the post-1.0 toolchain, networking, and Node runtime phases — plus the Phase 90a PKU JIT substrate — to run a modern CLI coding agent natively inside m3OS, interactive TUI included
**Primary Components:** Node.js package installation path, CLI runtime environment, git and GitHub CLI integration, shell and terminal tooling, docs/claude-code-roadmap.md

## Milestone Goal

Claude Code runs natively inside m3OS — installed via the `.m3pkg` substrate, launched through the `/usr/bin/claude` node wrapper, exercised via headless print mode (`claude -p`) and the A.2 interactive primitives (SIGINT/raw-mode/subprocess), **and with its interactive TUI rendering on the Phase 90a JIT/WASM-capable Node variant** — using the supported Node, network, git, and GitHub tooling to read code, run commands, and participate in the same developer workflow the earlier post-1.0 phases made possible. The **interactive TUI was the original "first" target on the Phase 90a JIT/WASM-capable Node variant, and it is ACHIEVED**: the interactive `claude` TUI renders on the JIT node — proven by an automated QMP/PPM render arm (592 changed band scanlines vs the empty-prompt baseline; the captured screenshot shows the rendered "Welcome to Claude Code v2.1.112" welcome screen with the yoga.wasm-laid-out logo splash). Two fixes delivered it: the W^X-v2 cross-thread PKU read-recovery kernel fix (so `cli.js` launches on the JIT node) and the node build's small-icu→full-icu switch (so `Intl.Segmenter`'s grapheme segmentation has the ICU break-iterator data the TUI needs). The delivered milestone is therefore the full install + launch + headless `claude -p` + rendering interactive TUI story.

## Why This Phase Exists

This milestone is intentionally ambitious and a little self-referential, but it is also useful as an integration test. If m3OS can host a modern CLI coding agent, it means the platform can support a non-trivial Node application with network access, terminal behavior, subprocess management, git workflows, and package installation.

This phase exists to validate that the post-1.0 developer-platform story can carry a realistic modern tool, not just simpler traditional CLIs.

## Learning Goals

- Understand how a modern CLI agent combines runtime, network, terminal, subprocess, and repository workflows.
- Learn how many earlier platform decisions become visible when one large application uses them all together.
- See why support boundaries and credential handling matter even more for cloud-connected developer tools.
- Understand the difference between "a tool can launch" and "the platform can support its full documented workflow."

## Feature Scope

### Agent installation and runtime environment

Provide the documented path to install and run the CLI agent inside the supported Node and package environment.

### Repository and shell workflow integration

Ensure the tool can read files, invoke shell commands, and participate in the supported git workflow on m3OS.

### Network and API path

Validate the authenticated network path the agent needs and define how credentials are handled within the supported environment. Subscription use is first-class: a host-minted long-lived OAuth token (`claude setup-token`, seeded at mode 0600 like the Phase 86e `gh` pattern) is the automation path, the in-OS `/login` paste-flow (the TUI displays a URL; the code is entered from any browser-equipped device) is the documented human path, and `ANTHROPIC_API_KEY` remains the API-billing alternative.

**Socket-option ABI conformance (keepalive).** Bring-up surfaced a kernel gap on the live API path: Claude Code reaches `api.anthropic.com` via `fetch`/undici, whose sockets enable TCP keepalive. libuv's `uv__tcp_keepalive` sets `SO_KEEPALIVE` then `TCP_KEEPIDLE`/`TCP_KEEPINTVL`/`TCP_KEEPCNT`, and treats *any* `setsockopt` failure there as a fatal connect error. The kernel's `setsockopt`/`getsockopt` previously rejected the `TCP_KEEP*` tuning options with `ENOPROTOOPT`, so the interactive TUI rendered but every API request failed with `Failed to connect to api.anthropic.com: ENOPROTOOPT` (errno 92 — *not* a connectivity/DNS/routing failure; outbound TCP itself works, proven by the always-on Node egress arm). The always-on smoke never caught it because the egress probe uses plain `http.get`, which does not enable socket-level keepalive. Fix: the kernel now (1) accepts and stores the `TCP_KEEP*` tuning (round-tripped via `getsockopt`) for a future prober, and (2) accept-and-logs other unimplemented best-effort options (e.g. `SO_LINGER`) instead of returning `ENOPROTOOPT`, since m3OS presents a Linux ABI to unmodified Linux binaries that treat the failure as fatal. Reads stay honest (an unimplemented `getsockopt` still returns `ENOPROTOOPT`). Guarded always-on, network-free by `connect-smoke` assertion 5 (`setsockopt(TCP_KEEPIDLE)` must return 0 and round-trip). The keepalive *probe timer itself* is deferred — see Deferred Until Later.

### Support boundary and non-goals

Be explicit about what parts of the broader agent ecosystem are supported and what remains later work, including optional integrations or protocol extensions.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Supported install and launch path | The phase has no value if the agent cannot be set up reproducibly |
| Working file/shell/git workflow | This is the core reason to run the tool on m3OS |
| Clear credential-handling guidance | Cloud-connected developer tools raise obvious trust and UX questions |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Runtime baseline | Phase 89 provides the documented Node/npm environment, and Phase 90a provides the JIT/WASM-capable variant the TUI needs | Pull missing runtime or packaging work into this phase (or fall back to the documented `-p` floor if 90a slips) |
| Developer-workflow baseline | Phases 85 and 86 provide the documented file, shell, git, and network capabilities | Add missing workflow support before claiming success |
| Credential baseline | The platform has a documented way to handle the tool's credentials safely enough for the supported story | Add the missing credential-handling guidance or tooling |
| Scope-discipline baseline | Optional integrations and protocol extensions are explicitly out of scope unless supported | Add the missing non-goal documentation here |

## Important Components and How They Work

### Installation and runtime path

The install path proves whether the Node and package story is usable for a real modern CLI application instead of only for synthetic runtime tests.

### Tool integration with the developer workflow

The agent depends on normal OS capabilities: reading files, running commands, using git, and interacting with network services. This phase should show how those pieces fit together on m3OS.

### Credential and network posture

Cloud-connected developer tooling raises trust, secret-handling, and support-boundary questions that must be answered explicitly, not by implication.

## How This Builds on Earlier Phases

- Builds directly on the post-1.0 toolchain, remote-workflow, and Node runtime phases.
- Serves as an integration test for the platform's modern developer-tooling story.
- Provides a clear example of how far m3OS has come without redefining the 1.0 release promise retroactively.

## Implementation Outline

1. Define the supported installation and launch path for the tool.
2. Validate the file, shell, git, and network workflows the tool depends on.
3. Document the credential-handling and support-boundary story.
4. Test the supported workflows inside m3OS end-to-end.
5. Update the standalone roadmap and top-level docs for the new milestone.

## Learning Documentation Requirement

- Create `docs/90b-claude-code.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the install path, runtime dependencies, file/shell/git integration, credential handling, and the exact supported workflow.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/claude-code-roadmap.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update any post-1.0 platform docs that describe supported developer workflows.
- Update security or credential-handling docs if the agent path introduces new operational guidance.
- When the phase lands, bump `kernel/Cargo.toml` to `0.90.1` (Phase 90a takes the `0.90.0` minor; this sub-phase takes the patch, mirroring the 86a–f pattern).

## Acceptance Criteria

- The supported install path for Claude Code works inside m3OS. ✅
- The interactive TUI renders inside the m3OS terminal on the Phase 90a JIT Node variant (verified headlessly via the QMP/PPM screenshot harness, not just a launch sentinel). ✅ **Met.** The automated `claude_tui_render_arm` launches `claude` in the graphical `term`, screendumps, and asserts **592 changed band scanlines** vs the empty-prompt baseline (threshold 20; a blank screen ≈ 0); the captured screenshot shows the rendered "Welcome to Claude Code v2.1.112" onboarding splash with the yoga.wasm-laid-out logo. Two fixes delivered it: the W^X-v2 cross-thread PKU read-recovery kernel fix (so `cli.js` launches) and the node build's small-icu→full-icu switch (so `Intl.Segmenter`'s grapheme segmentation has the ICU break-iterator data the TUI needs — small-icu's missing brkitr data had null-deref'd V8's `JSSegments::Create`; the earlier `mremap`/`io_uring` syscalls were red herrings). The JIT/WASM *runtime* (`node-jit-smoke`) and the A.2 interactive primitives (SIGINT/raw-mode/subprocess) are also proven.
- The tool can execute the documented file, shell, and git workflows on m3OS via headless `claude -p` (opt-in live arm, credential-gated). ✅
- The supported network/API path works with documented credential handling, including subscription auth via a seeded OAuth token.
- The docs explicitly describe what Claude Code workflows are supported and what remains out of scope.
- The milestone can be reproduced through the documented runtime and package setup.

## Companion Task List

- [Phase 90b Task List](./tasks/90b-claude-code-tasks.md)

## How Real OS Implementations Differ

- Mature desktop and server operating systems can support much broader agent ecosystems and integrations than m3OS should assume here.
- The real value of this phase is as a platform-integration proof point, not as a claim that m3OS has become a full hosted AI workspace.
- Explicit non-goals matter here because the surrounding ecosystem can grow much faster than the OS itself.

## Deferred Until Later

- Extended protocol ecosystems and optional integrations
- Broader multi-user or enterprise credential-management stories
- Rich GUI integration for the agent
- Offline or local-model alternatives beyond the documented cloud-backed path
- **TCP keepalive prober (scheduled for a future networking phase).** The `SO_KEEPALIVE` switch and the `TCP_KEEPIDLE`/`TCP_KEEPINTVL`/`TCP_KEEPCNT` tuning are now *accepted and stored* (so `setsockopt`/`getsockopt` are ABI-conformant and the Claude Code / Node `fetch` path connects), but the kernel does not yet *send* keepalive probes — `SO_KEEPALIVE` has been stored-but-not-acted-upon since it was first added, and the tuning options inherit that state. Implementing the prober means hooking the existing periodic `tcp_tick` (`kernel/src/net/tcp.rs`, alongside the RFC 6298 retransmit timer) to: fire after `keep_idle_secs` of connection idle, emit a keepalive probe segment every `keep_intvl_secs`, and tear the connection down after `keep_cnt` unanswered probes. Not required for Phase 90b — during an active API/SSE stream data flows continuously (the idle timer never fires), and a dead idle pooled socket is detected on reuse by undici's retry path. The stored `SocketOptions::keep_*` fields already carry the configured values for that future work.
