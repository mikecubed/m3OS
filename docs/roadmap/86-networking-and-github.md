# Phase 86 - Networking and GitHub

**Status:** Planned
**Source Ref:** phase-86
**Depends on:** Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅, Phase 42 (Crypto Primitives) ✅, Phase 48 (Security Foundation) ✅, Phase 85 (Cross-Compiled Toolchains)
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

Extend the local git baseline from Phase 85 to remote clone, fetch, push, and related workflows on the supported services.

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
| Security baseline | Phase 48's entropy and default-security repairs are complete and trusted | Pull missing RNG or credential-handling work into this phase |
| Tooling baseline | Phase 85 local git and other developer tools are working reliably | Add the missing local-tool cleanup before remote workflows |
| Network/runtime baseline | The supported networking and threading substrate can carry the chosen tools | Add the missing runtime or resolver support instead of assuming it |
| Support-boundary baseline | The project has documented what remote workflows it actually supports | Add the missing support-matrix updates before closing |

## Important Components and How They Work

### Resolver and trust configuration

This phase should define where DNS configuration lives, how certificates or trust roots are handled, and what security assumptions the supported tools rely on.

### git remote transport path

Remote git support is where local toolchains, auth, and network transport meet. The project should document the chosen transport strategy clearly enough that future developer workflows build on it without guesswork. **A concrete, source-verified transport strategy is captured below in [Pre-Planning Findings](#pre-planning-findings-2026-05-29--secure-transport-track) — read it before scoping this section.**

### GitHub CLI workflow integration

The GitHub CLI is a useful test because it exercises authenticated HTTPS, API access, and a realistic modern developer workflow end-to-end.

## Pre-Planning Findings (2026-05-29) — secure-transport track

Source-verified during the Phase 77 review cycle. Captured here so the transport
strategy is not re-discovered from scratch when this phase is scoped.

### What already exists (foundation is ~70% there)

| Building block | Status |
|---|---|
| DNS forward resolution (`getaddrinfo`/A records, IPv4/UDP) | ✅ Phase 77 D.1 — the kernel reply-delivery gap (musl drains replies via `recvmsg`, not `recvfrom`) was closed by `sys_recvmsg_inet` (commit `8303990`). No caching / search-domains / AAAA / DNSSEC yet. |
| Outbound TCP `connect()` active-open + RFC 6298 retransmit + flow control + 64 conns | ✅ Phase 77 D.2 + `sys_connect` → `tcp::connect` |
| `zlib` (packfile inflate) | ✅ `ports/lib/zlib` |
| `sunset` — full SSH **client**+server, `no_std` | ✅ pulls X25519, Ed25519, AES, ChaCha20-Poly1305, SHA-2, HMAC, RSA |
| musl cross-toolchain + `ports/` build infra | ✅ |

### Recommended strategy: SSH-first, then HTTPS

The genuinely *minimal secure* first clone is **SSH, not HTTPS**, because it reuses the
in-tree audited `sunset` crypto and skips the entire X.509/CA/HTTP stack.

| Milestone | Work | Reuses | Effort / risk |
|---|---|---|---|
| **M0** prove outbound INET | smoke test: `connect()` to a SLIRP-forwarded host, exchange bytes | DNS, TCP connect | S / low (mostly verification) |
| **M1** `ssh` client binary | build a userspace `ssh` from `sunset` (client role exists): connect → X25519 KEX → host-key TOFU → `known_hosts` → pubkey auth → session channel → exec + stdio pipe | sunset crypto/KEX/auth | S–M / **low** (audited crypto) — reusable beyond git |
| **M2** git over SSH (the elegant shortcut) | port upstream git against musl with **`NO_CURL NO_OPENSSL`** (links `zlib`); git's SSH transport shells out to the `ssh` binary and runs `git-upload-pack` remotely — **we write no git-protocol/packfile code** | upstream git, zlib, M1 | M–L / med (build config; packfile is git's own code) — *first working secure `git clone`* |
| **M3** HTTPS (the real lift, last) | TLS 1.3 client (one suite) + **X.509 chain validation + CA bundle + hostname verify** + HTTP/1.1 (chunked) + git smart-HTTP; rebuild git with curl+TLS | sunset primitives, git http transport | **L / med–high** — X.509/CA-trust correctness is the classic footgun |

### TLS library decision (for M3)

Do **not** hand-roll TLS. Two m3OS constraints pin the choice:

1. **SIMD is off** (`+soft-float`, no SSE/AES-NI) → rules out `ring` / `aws-lc-rs` (asm/C/SIMD).
   Pure-Rust RustCrypto software backends work here (proven by `sunset`). See
   [`docs/research/simd-enablement.md`](../research/simd-enablement.md) — enabling SIMD is a
   tracked future perf option, **not** a TLS prerequisite (its payoff is hardware AES-NI throughput).
2. **Cert-expiry validation needs a trustworthy wall-clock.** `kernel/src/rtc.rs` provides
   `BOOT_EPOCH_SECS` but falls back to 0 on an invalid RTC; a current-time source
   (`BOOT_EPOCH + monotonic`) is a prerequisite for *any* TLS option.

| Option | Fit | Catch |
|---|---|---|
| **mbedTLS + curl** (C ports) | **Best for the real git binary** — full X.509, portable C crypto (no SIMD dep), drops into the existing musl `ports/` pipeline next to `zlib` | C, not Rust — but git's HTTPS expects curl + a C TLS lib anyway |
| **rustls + `rustls-rustcrypto` + `rustls-webpki` + `webpki-roots`** | "Correct" Rust path; `rustls-webpki` does chain validation, `webpki-roots` is the CA set | `rustls-rustcrypto` is experimental + still std-leaning (no_std WIP) |
| **`embedded-tls`** | Lightest `no_std` TLS 1.3 client on RustCrypto | its cert verifier is **std-only** today — pair with `rustls-webpki` yourself |

### Phasing note

This belongs here (Phase 86, with the git binary in Phase 85), **not** as a "Phase 77b" —
HTTPS/TLS + git is a post-1.0 headline capability, explicitly deferred behind the Phase 83
release gate. The git binary build (`NO_CURL NO_OPENSSL` + ssh transport) is Phase 85's
concern; the transport (ssh client, then HTTPS) is this phase's.

## How This Builds on Earlier Phases

- Builds on Phase 85's local toolchain story by extending it into real collaboration workflows.
- Depends on Phase 48 because network-facing developer tools raise the bar for entropy and credentials.
- Reuses earlier network, crypto, threading, and I/O groundwork without pulling those phases back into the release-critical path.

## Implementation Outline

1. Define the supported resolver and HTTPS trust configuration for the phase.
2. Choose and implement the supported git remote transport path.
3. Bundle and validate the GitHub CLI workflow.
4. Test authenticated remote workflows inside the supported environment.
5. Update support docs and post-1.0 roadmap notes to match the shipped behavior.

## Learning Documentation Requirement

- Create `docs/86-networking-and-github.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the resolver path, HTTPS trust model, git remote integration, and GitHub CLI workflow.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/23-socket-api.md`, `docs/git-roadmap.md`, `docs/github-cli-roadmap.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update any security or networking docs that describe entropy, trust roots, or outbound network policy.
- Update post-1.0 evaluation notes if the supported remote workflow meaningfully changes the platform story.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to the next post-1.0 minor version.

## Acceptance Criteria

- The supported resolver configuration works for the documented outbound environment.
- The chosen HTTPS trust path is documented and used by the supported developer tools.
- git can perform the documented remote workflows inside m3OS.
- The GitHub CLI can complete the documented authenticated workflows inside m3OS.
- The phase docs clearly describe the supported remote workflows and their limits.

## Companion Task List

- Phase 86 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature systems support far more network tools, trust stores, background services, and credential helpers than m3OS should assume here.
- The goal is not to duplicate a modern Linux workstation; it is to support a small, credible remote developer workflow.
- Strong trust boundaries matter even more once the system starts handling real authenticated network traffic.

## Inherited Follow-ups from Earlier Phases

- **Phase 55c follow-through for userspace `EAGAIN` visibility** — Phase 55c pulled the old Phase 55b `sys_net_send` / `sendto()` restart-surfacing gap forward because the pre-1.0 ring-3 driver story now depends on it. Phase 86 should treat that userspace-visible `EAGAIN` contract as baseline behavior and build future networking work on top of it rather than reopening ownership.

## Deferred Until Later

- Full workstation-grade browser and GUI networking stack
- Rich credential-helper ecosystems
- Large language-specific package managers that depend on heavier runtimes
- Broad general internet-client expectations beyond the supported developer tools
