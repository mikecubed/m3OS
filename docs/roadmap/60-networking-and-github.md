# Phase 60 - Networking and GitHub

**Status:** Planned
**Source Ref:** phase-60
**Depends on:** Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅, Phase 42 (Crypto Primitives) ✅, Phase 47 (Security Foundation) ✅, Phase 59 (Cross-Compiled Toolchains) ✅
**Builds on:** Extends the post-1.0 developer platform from local toolchains into authenticated outbound networking, DNS resolution, git remote workflows, and GitHub CLI use
**Primary Components:** userspace network tooling, getrandom()/entropy path, GitHub CLI integration, git transport support, docs/github-cli-roadmap.md, docs/git-roadmap.md

## Milestone Goal

m3OS can use authenticated outbound network tooling for real developer workflows: DNS works, HTTPS is trustworthy enough for the supported use cases, git can speak to remotes, and the GitHub CLI runs inside the OS.

## Why This Phase Exists

Local developer tooling is powerful but still isolated. Once git, Python, and Clang exist locally, the next natural step is to make the system useful for remote collaboration. That brings in DNS, HTTPS trust, authenticated CLI workflows, and a stricter dependence on the repaired randomness and security story.

This phase exists to make that outbound developer workflow deliberate and supportable.

## Learning Goals

- Understand how DNS resolution, HTTPS, and authenticated developer tooling build on the earlier security and networking layers.
- Learn why outbound tooling raises the bar for entropy, certificate validation, and credential handling.
- See how post-1.0 growth phases still depend on strong release and security discipline.
- Understand how to stage network-facing developer tools without pretending the whole system is a general-purpose internet workstation.

## Feature Scope

### DNS and outbound name resolution

Provide the documented resolver path and configuration needed by the supported developer tools. The phase should define what "working DNS" means for the supported environment.

### HTTPS and certificate trust for developer tooling

Make the supported transport path for GitHub CLI, git remotes, and other outbound developer workflows explicit and trustworthy enough for the post-1.0 promise.

### git remote workflows

Extend the local git baseline from Phase 59 to remote clone, fetch, push, and related workflows on the supported services.

### GitHub CLI integration

Bundle and validate the GitHub CLI path used for repository, issue, PR, and CI interactions inside m3OS.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Strong randomness and trust roots from earlier phases | Outbound auth and HTTPS depend on them |
| DNS that works for the supported environment | Remote workflows fail without it |
| One documented git remote path and GitHub CLI path | They are the whole point of the phase |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Security baseline | Phase 47's entropy and default-security repairs are complete and trusted | Pull missing RNG or credential-handling work into this phase |
| Tooling baseline | Phase 59 local git and other developer tools are working reliably | Add the missing local-tool cleanup before remote workflows |
| Network/runtime baseline | The supported networking and threading substrate can carry the chosen tools | Add the missing runtime or resolver support instead of assuming it |
| Support-boundary baseline | The project has documented what remote workflows it actually supports | Add the missing support-matrix updates before closing |

## Important Components and How They Work

### Resolver and trust configuration

This phase should define where DNS configuration lives, how certificates or trust roots are handled, and what security assumptions the supported tools rely on.

### git remote transport path

Remote git support is where local toolchains, auth, and network transport meet. The project should document the chosen transport strategy clearly enough that future developer workflows build on it without guesswork.

### GitHub CLI workflow integration

The GitHub CLI is a useful test because it exercises authenticated HTTPS, API access, and a realistic modern developer workflow end-to-end.

## How This Builds on Earlier Phases

- Builds on Phase 59's local toolchain story by extending it into real collaboration workflows.
- Depends on Phase 47 because network-facing developer tools raise the bar for entropy and credentials.
- Reuses earlier network, crypto, threading, and I/O groundwork without pulling those phases back into the release-critical path.

## Implementation Outline

1. Define the supported resolver and HTTPS trust configuration for the phase.
2. Choose and implement the supported git remote transport path.
3. Bundle and validate the GitHub CLI workflow.
4. Test authenticated remote workflows inside the supported environment.
5. Update support docs and post-1.0 roadmap notes to match the shipped behavior.

## Learning Documentation Requirement

- Create `docs/60-networking-and-github.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the resolver path, HTTPS trust model, git remote integration, and GitHub CLI workflow.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/23-socket-api.md`, `docs/git-roadmap.md`, `docs/github-cli-roadmap.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update any security or networking docs that describe entropy, trust roots, or outbound network policy.
- Update post-1.0 evaluation notes if the supported remote workflow meaningfully changes the platform story.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.60.0`.

## Acceptance Criteria

- The supported resolver configuration works for the documented outbound environment.
- The chosen HTTPS trust path is documented and used by the supported developer tools.
- git can perform the documented remote workflows inside m3OS.
- The GitHub CLI can complete the documented authenticated workflows inside m3OS.
- The phase docs clearly describe the supported remote workflows and their limits.

## Companion Task List

- Phase 60 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature systems support far more network tools, trust stores, background services, and credential helpers than m3OS should assume here.
- The goal is not to duplicate a modern Linux workstation; it is to support a small, credible remote developer workflow.
- Strong trust boundaries matter even more once the system starts handling real authenticated network traffic.

## Deferred Until Later

- Full workstation-grade browser and GUI networking stack
- Rich credential-helper ecosystems
- Large language-specific package managers that depend on heavier runtimes
- Broad general internet-client expectations beyond the supported developer tools
