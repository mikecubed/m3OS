# Phase 61 - Node.js

**Status:** Planned
**Source Ref:** phase-61
**Depends on:** Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅, Phase 42 (Crypto Primitives) ✅, Phase 59 (Cross-Compiled Toolchains) ✅, Phase 60 (Networking and GitHub) ✅
**Builds on:** Extends the post-1.0 developer platform into a heavier managed runtime with JIT, async I/O, and package-management expectations that stress more of the system than the earlier toolchain phases
**Primary Components:** Node.js runtime build pipeline, libuv integration expectations, V8 memory behavior, npm packaging path, docs/nodejs-roadmap.md

## Milestone Goal

Node.js runs natively inside m3OS with the documented level of runtime and networking support, and npm can install the packages needed by the later CLI-agent phase.

## Why This Phase Exists

Node.js is a useful stress test because it brings together many of the system capabilities that lighter CLIs can avoid: dynamic memory behavior, event loops, timers, threads, TLS-heavy networking, and package installation. If m3OS can support a disciplined Node.js story, it has crossed another meaningful threshold as a developer platform.

This phase exists to make that heavy-runtime step explicit instead of sneaking it in through later tools.

## Learning Goals

- Understand how JIT-heavy or managed runtimes stress memory, mapping, and execution permissions differently from simpler binaries.
- Learn how libuv-style async I/O builds on the earlier epoll and threading work.
- See why package installation and runtime support boundaries matter for post-1.0 growth.
- Understand which parts of Node.js support are essential for the later CLI-agent milestone and which can still wait.

## Feature Scope

### Node.js runtime bring-up

Cross-compile and validate a Node.js runtime configuration that matches the supported system capabilities and avoids unsupported extras until the platform is ready.

### Local runtime features

Make the documented local runtime behavior work first: filesystem access, timers, console, process info, and the basic event loop.

### Networked runtime features and npm

Add the supported networking and package-management path needed by the later CLI-agent milestone. This is the point where Node.js becomes more than a local curiosity.

### Runtime support boundary

Be explicit about what parts of the larger Node ecosystem are still outside scope, such as native addons, richer inspector tooling, or heavy package assumptions.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Stable Node runtime for the documented use cases | Otherwise the phase has no platform value |
| npm or equivalent package path for the later CLI-agent phase | The next phase depends on it |
| Clear support boundary for unsupported runtime features | Node growth can sprawl quickly without explicit limits |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Toolchain and network baseline | Phases 59 and 60 are already stable enough to support a heavier runtime | Pull missing toolchain or network prerequisites into this phase |
| Memory/runtime baseline | The supported memory and execution-permission behavior can carry the chosen Node configuration | Add the missing runtime support or narrow the scope explicitly |
| Package-path baseline | The project has a documented installation path for global or system-level packages | Add the missing npm/install-layout work before closing |
| Scope-discipline baseline | The phase explicitly defines what parts of the Node ecosystem are supported | Add the missing support-boundary docs here |

## Important Components and How They Work

### Runtime build and configuration

The build configuration determines how much of Node.js the system tries to support. This phase should choose a configuration that fits the actual platform instead of assuming a full Linux userspace.

### Event loop and async integration

Node.js depends on the system's timers, async I/O, and thread support in a way that simpler CLIs do not. The phase should document that dependency clearly.

### npm and package path

The runtime only becomes strategically useful once the package path needed for later tools is supported and documented.

## How This Builds on Earlier Phases

- Builds on Phase 59's toolchain packaging and Phase 60's outbound network/trust path.
- Stresses the earlier memory, I/O, threading, and crypto work in a more demanding runtime.
- Prepares the final CLI-agent milestone by making npm and a supported Node environment available inside m3OS.

## Implementation Outline

1. Choose the supported Node.js configuration and document the non-goals.
2. Bring up the local runtime behavior first.
3. Add the documented networking and npm path needed for later CLI-agent work.
4. Validate the runtime against the supported use cases.
5. Update docs and standalone Node roadmap material to match the official milestone.

## Learning Documentation Requirement

- Create `docs/61-nodejs.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the chosen Node configuration, runtime expectations, event-loop integration, npm path, and explicit non-goals.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/nodejs-roadmap.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update any runtime, memory, or package-layout docs that the chosen Node configuration depends on.
- Update post-1.0 evaluation framing if Node support changes the platform story materially.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.61.0`.

## Acceptance Criteria

- Node.js runs the documented local runtime workloads inside m3OS.
- The documented networking-dependent Node workloads and package-install path also work.
- npm or the chosen equivalent package path is usable for the later CLI-agent milestone.
- The phase docs clearly describe what Node support exists and what remains unsupported.
- The runtime configuration and install layout are reproducible through the documented build flow.

## Companion Task List

- Phase 61 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature systems support far broader Node workflows, native addons, inspection tooling, and ecosystem assumptions than m3OS needs here.
- The goal is not to replicate every Linux/npm expectation; it is to support the subset needed by the planned post-1.0 platform story.
- Support boundaries matter especially with large runtimes because ecosystem assumptions expand faster than kernel capability.

## Deferred Until Later

- Native addon and `node-gyp` support
- Full developer-inspector integration
- Rich multi-runtime JS workflows beyond the documented support set
- Broader desktop-oriented Node application ecosystems
