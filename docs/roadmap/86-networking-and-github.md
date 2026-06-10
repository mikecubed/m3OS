# Phase 86 - Networking and GitHub (Umbrella)

**Status:** Done ✅ — all six sub-phases (86a–86f) landed; umbrella aggregate kernel `0.86.5`; learning doc cut at `docs/86-networking-and-github.md` (owned by 86f per the Phase 85 → 85d precedent).
**Source Ref:** phase-86
**Depends on:** Phase 37 (I/O Multiplexing) ✅, Phase 40 (Threading) ✅, Phase 42 (Crypto Primitives) ✅, Phase 48 (Security Foundation) ✅, Phase 77 (Pre-1.0 Cleanup — DNS reply delivery D.1 + outbound TCP `connect` D.2) ✅, Phase 85 (Cross-Compiled Toolchains) ✅
**Builds on:** Extends the post-1.0 local developer platform from Phase 85 (local `git`, Python, Clang installed from `.m3pkg`) into **authenticated outbound networking** — a trustworthy CSPRNG + wall-clock + resolver foundation, then SSH and HTTPS git remotes, the Go runtime, and the GitHub CLI — without dragging any of it back into the release-critical path.
**Primary Components:** `kernel-core/src/csprng.rs` (new) + `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_getrandom`, `mmap`/`epoll`/signal syscalls), `kernel/src/rtc.rs` (wall-clock), `kernel/src/net/{tcp,udp}.rs`, `ports/{lib,util,lang}/*` (`ca-certificates`, `mbedtls`, `curl`, an `ssh` client, `go`, `gh`), `userspace/sshd` + `sunset-local` + `async-rt`, `xtask/src/{port_build,main}.rs`, the SSE-enabled Rust userspace target (`x86_64-m3os.json`), `docs/git-roadmap.md`, `docs/archived/github-cli-roadmap.md`

> **This is an umbrella phase, delivered as six sub-phases (86a–86f).** It follows the Phase 85 (Cross-Compiled Toolchains) pattern: the umbrella doc defines the theme, the scope split, and the shared crypto/transport architecture; each sub-phase has its own design doc and task list and lands its own kernel-version patch bump (`0.86.0` → `0.86.5`). There is **no separate umbrella task list** — the companion task lists are the six sub-phase lists below.

## Milestone Goal

m3OS can use authenticated outbound developer tooling for real remote workflows: randomness is cryptographically trustworthy, the wall-clock can validate certificates, DNS resolves the supported hosts, `git` can clone/fetch/push over **SSH** and over **HTTPS**, the **Go runtime** runs, and the **GitHub CLI (`gh`)** completes authenticated PR/issue/CI workflows from inside the OS — with a final hardware-AES-NI performance pass for the crypto paths.

## Why This Phase Exists

Local developer tooling (Phase 85) is powerful but isolated. The next high-leverage step is remote collaboration — and that raises the bar in ways a local-only system never had to meet:

- **Cryptographic randomness becomes load-bearing.** SSH key exchange, TLS session keys, X25519/ECDHE ephemerals, and DNS transaction IDs all depend on it. m3OS's current `sys_getrandom` expands a **non-cryptographic xorshift PRNG** (`kernel_core::prng::Prng`, documented "NOT cryptographically secure") from a single 64-bit `RDRAND ⊕ TSC` seed per call. That is adequate for nothing in this phase. Hardening it is the foundation everything else stands on.
- **Certificate validation needs a trustworthy wall-clock.** `kernel/src/rtc.rs` leaves `BOOT_EPOCH_SECS = 0` on an invalid RTC; an epoch-1970 clock makes every certificate "not yet valid" and looks like a TLS bug.
- **Two transports, two trust models.** SSH reuses in-tree audited crypto and skips the entire X.509/CA stack; HTTPS needs a TLS 1.3 client, X.509 chain validation, a CA bundle, hostname verification, and `curl`. They are independent lifts that deserve independent sub-phases.
- **The Go runtime is a real bring-up.** `gh` is a Go binary; running *any* Go program first requires clearing concrete kernel gaps (`mmap` `MAP_FIXED`, edge-triggered `epoll`, `SIGURG` preemption). That is validation/bring-up work that must be de-risked on its own before the 40 MB `gh` is bundled.

This phase exists to make that outbound workflow **deliberate, secure, and supportable**, decomposed so each capability lands and is validated independently.

## Sub-Phase Decomposition

| Sub | Theme | Primary Outcome | Depends on | Kernel |
|---|---|---|---|---|
| **86a** | Outbound Foundation | A real **ChaCha20 DRBG** `getrandom` (RDSEED→RDRAND seeded, flags honored), a fail-closed **wall-clock** floor for cert validity, the IPv4/A-record **resolver** + `/etc/hosts` path, and on-disk **CA / known_hosts / credential** conventions + a SHA-256-pinned `ca-certificates` `.m3pkg`. No transport yet. | 77, 85 | `0.86.0` |
| **86b** | SSH + git-over-SSH | A static **`ssh` client** (chosen by an in-phase **dropbear-vs-sunset spike + ADR**) with `known_hosts`/TOFU, wired through `GIT_SSH_COMMAND` → first **secure `git clone`/fetch/push** over SSH, with **zero changes to the git binary**. | 86a | `0.86.1` |
| **86c** | HTTPS/TLS + git smart-HTTP | **mbedTLS + curl** ports (SIMD-off-safe C crypto) + git rebuilt with `NO_CURL` removed + X.509 chain/hostname verification against the 86a CA bundle + PAT credentials → **`git clone`/push over HTTPS**. | 86a (+ 86b build pattern) | `0.86.2` |
| **86d** | Go-runtime gate | Clear the two hard kernel blockers (`mmap` `MAP_FIXED`/`PROT_NONE` reservations; edge-triggered `EPOLLET`+`EPOLLRDHUP`) and the soft one (`SIGURG` preemption), then ship `ports/lang/go` and prove a static Go binary runs (goroutine + plaintext HTTP). | 86a | `0.86.3` |
| **86e** | GitHub CLI (`gh`) + native fallback | Cross-built **`gh`** (`.m3pkg` behind an `M3OS_WITH_GH` image feature), `GH_TOKEN` auth + `gh auth setup-git` reusing the 86c machinery, authenticated PR/issue/CI workflows; a documented **native Rust GitHub-REST fallback** if the Go path stalls. | 86c, 86d | `0.86.4` |
| **86f** | Userspace SIMD / AES-NI capstone | An **SSE/AES-enabled Rust userspace target**, the finished signal-frame FPU path, `_start` RSP-alignment verification, full re-validation, and **hardware-AES-NI-accelerated** crypto for the SSH/TLS paths. Owns the umbrella learning doc + capability cut. | 86c (correctness first) | `0.86.5` |

**Ordering rationale.** 86a is the trust foundation and must land first — every transport silently depends on a real CSPRNG, a sane clock, and a CA bundle. 86b is the **cheapest first secure clone** (SSH reuses in-tree crypto and skips X.509 entirely), so it validates the outbound path at low risk. 86c is the heavier HTTPS/TLS lift (X.509/CA correctness is the classic footgun). 86d and 86e form the GitHub-CLI branch; per `docs/research/simd-enablement.md`, 86f is a **perf optimization, not a prerequisite**, sequenced last with the full re-validation it demands. The whole family is **post-1.0 growth** — the kernel stays phase-tracked (`0.86.x`), never SemVer `1.0.0` (the Phase 83 posture).

**Branch decoupling.** After 86a, the two arcs are largely independent: the git-transport arc (86b SSH, then 86c HTTPS) and the GitHub-CLI arc (86d Go runtime, then 86e `gh`). `gh` carries its **own** Go `crypto/tls`, so it does not depend on the m3OS mbedTLS stack — only on 86a (entropy/DNS/clock/CA bundle) and 86d (the runtime). The result is that m3OS deliberately carries **two TLS implementations**: mbedTLS for `git`, and Go `crypto/tls` for `gh`.

## Shared Secure-Transport & Crypto Architecture

This is the architecture every sub-phase consumes; the per-sub-phase detail lives in the six sub-phase docs.

### Randomness, time, and trust roots (86a — the foundation)

- **CSPRNG.** Replace the xorshift `getrandom` with a Linux-`random.c`-style **ChaCha20 DRBG** (`kernel-core/src/csprng.rs`), seeded ≥256 credited bits from **RDSEED** (full-entropy) preferring over **RDRAND**, fast-key-erasure forward secrecy, reseeded on an interval, seeded **early** (right after `mm::init`, before `init_task`), with `GRND_RANDOM`/`GRND_NONBLOCK`/`GRND_INSECURE` honored and the 256-byte cap removed (but ≤256-byte atomicity preserved, because `sshd`'s `getrandom` consumer does not loop). ChaCha20's ARX core is pure-integer — **SIMD-off-safe** and host-testable. The legacy `Prng` is quarantined.
- **Downstream entropy consumers.** `AT_RANDOM` (currently a deterministic `0xAB ^ i` pattern → identical stack canaries/ASLR across binaries) and the **TCP ISN** (currently `tick_count()` → hijackable) are switched to the CSPRNG.
- **Wall-clock.** `init_rtc` gets a **build-date floor** instead of `0` on a bad RTC, so cert `notBefore`/`notAfter` checks fail-closed-but-sane rather than rejecting every certificate as future-dated.
- **Resolver + trust paths.** The IPv4/A-record resolver path (`/etc/hosts` first, then a single-nameserver `/etc/resolv.conf` over the Phase 77 `sys_recvmsg_inet` UDP path) is validated; AAAA/IPv6 is explicitly scoped out (the stack is IPv4-only — Phase 89). A SHA-256-pinned **`ca-certificates` `.m3pkg`** stages the Mozilla root bundle to one canonical path (`/etc/ssl/certs/ca-certificates.crt`) that both `git`/`curl` and any other consumer agree on.

### SSH transport (86b) — the elegant first secure clone

git's SSH transport **shells out to an `ssh` binary** (`GIT_SSH_COMMAND`) and runs `git-upload-pack`/`git-receive-pack` on the remote; git speaks the protocol and moves the packfile itself, so **we write no git-protocol/packfile code and rebuild nothing**. The only new artifact is a static `ssh` client.

The client is chosen by an **in-phase spike + ADR**, because the field is narrow and the trade-off is real:

- **`dropbear` `dbclient` (C).** Mature, battle-tested, GitHub-interop-confirmed, blocking (ideal for the `GIT_SSH` subprocess model), self-contained `libtomcrypt` software crypto (build with assembly disabled), `known_hosts`/TOFU built in, ~100–200 KB. Same port class as the existing `git`/Python/Clang C ports.
- **`sunset` (Rust, vendored as `sunset-local`).** The **only** pure-Rust SSH option that fits m3OS, because the SIMD-off constraint rules out the entire `ring`/`aws-lc-rs` ecosystem — **russh, ssh-rs, thrussh, ssh2/libssh2-FFI are all ruled out** (each hard-depends on `ring`/`aws-lc-rs` asm/C crypto, or on a C TLS lib). sunset reuses `crypto-lib` (fed by the 86a CSPRNG) and the `async-rt` reactor, but today it is **server-only** (`new_client`/`open_client_session` have zero callers) and has **no `known_hosts`/TOFU** — only a `CheckHostkey` callback — so the sunset branch must budget a from-scratch client harness + TOFU.

The ADR's decisive axis is `known_hosts`/TOFU cost; the documented recommendation is **dropbear for 86b, with the sunset spike captured as the future all-Rust migration path**.

### HTTPS/TLS transport (86c) — the real lift

- **TLS library: mbedTLS + curl (C).** SIMD-off rules out Rust `ring`/`aws-lc-rs`; it does **not** rule out C TLS (Redox ships C OpenSSL the same way). mbedTLS is a small, static, musl-friendly C TLS 1.3 client with full X.509 that drops into the `ports/` pipeline next to `zlib`, and `curl --with-mbedtls` is the documented small-footprint backend. The **pure-Rust rustls path is explicitly deferred**: `rustls-rustcrypto` is marked do-not-use-in-production and std-leaning, `crypto-lib` lacks `p256`/`ecdsa` (GitHub's leaf is ECDSA P-256, live-confirmed), and it would not serve the git binary anyway.
- **No AES-NI yet → prefer ChaCha20-Poly1305.** Until 86f, software AES is slow and cache-timing-exposed; GitHub offers `TLS_CHACHA20_POLY1305_SHA256`, so the client prefers the ARX suite.
- **git rebuild.** The 86c change to git is removing `NO_CURL` and **inverting** the Phase 85b absence-assertions (require `curl_easy_perform`/`SSL_CTX_new`); the server-side pack helpers stay pruned. Smart-HTTP uses `GET info/refs?service=git-upload-pack` → `POST git-upload-pack`; the smoke must assert the `application/x-git-upload-pack-advertisement` Content-Type + 5-byte pkt-line magic and include a **negative** (expired/wrong-host/self-signed) case.

### Go runtime (86d) — concrete kernel blockers

Running a static (`CGO_ENABLED=0`) Go binary is gated on three specific gaps, surfaced by source-grounding:

- **Blocker 1 — `mmap` `MAP_FIXED` + `PROT_NONE` reservations.** Go's allocator reserves arenas `PROT_NONE` then commits them `PROT_RW` `MAP_FIXED` *at the same address*; `sys_linux_mmap` currently discards the address hint and masks `MAP_FIXED`.
- **Blocker 2 — edge-triggered `epoll`.** Go's netpoll registers `EPOLLET`+`EPOLLRDHUP`; m3OS `epoll` is **level-triggered only** (the `EPOLLET` flag is silently ignored), so Go busy-loops or hangs.
- **Soft — async preemption.** `SIGURG` is undefined and signals are delivered only at syscall-return; Go uses `tgkill(SIGURG)` for goroutine preemption + GC stop-the-world. 86d's as-built: async preemption is left **enabled** and `SIGURG` is delivered at **syscall-return** (opportunistic — covers I/O-bound goroutines + GC stop-the-world); the timer-IRQ-return delivery path that would also preempt a syscall-free compute loop is deferred.

Most of the runtime substrate already exists (`clone(CLONE_THREAD|…)`, `futex`, `arch_prctl ARCH_SET_FS`, `/proc/self/exe`, `clock_gettime`, `sched_getaffinity`), so this is targeted bring-up, not greenfield.

### Userspace SIMD / AES-NI (86f — perf capstone)

Per `docs/research/simd-enablement.md`: the expensive kernel work (per-task XSAVE save/restore of x87+SSE+AVX, `CR4.OSXSAVE` + `XCR0=0x7`) is **already done and running**. Enabling SIMD is a build-system change (an SSE/AES-enabled **Rust userspace** target) + finishing the signal-frame FPU path + verifying `_start` RSP alignment + full re-validation. It is a **throughput** win (hardware AES-NI for the `aes` crate; faster ChaCha20/Poly1305), **not** a prerequisite — TLS works on software crypto, and userspace SSE2 already functions (the C ports are ordinary SSE2 musl binaries). It does **not** unlock `ring`/`aws-lc-rs` (those fail on their asm/C build + hosted-target assumptions, independent of the SSE flag), so the 86b SSH decision is unaffected. The kernel stays soft-float.

## Learning Goals

- Understand how a trustworthy CSPRNG, a sane wall-clock, and a CA trust root are the *precondition* for any secure outbound tooling — not an afterthought.
- Learn why SSH is the cheapest first secure transport (reuses audited crypto, no X.509) and HTTPS is the heavier, footgun-laden one (chain + hostname + CA + revocation).
- See how `git` remote transports work as "git speaks the protocol, an external program moves the bytes," so the OS supplies transport, not protocol.
- Learn the concrete OS-runtime requirements of the Go runtime (`MAP_FIXED`, edge-triggered `epoll`, `SIGURG` preemption) and why a managed-runtime binary stresses a young kernel differently than a C program.
- Understand how the SIMD-off constraint shapes the entire Rust crypto-crate choice space, and how/when enabling userspace SIMD changes it.

## Feature Scope

### DNS, entropy, and trust foundation (86a)

The CSPRNG, wall-clock floor, resolver/`/etc/hosts` validation, and the on-disk CA/`known_hosts`/credential conventions + `ca-certificates` package. Detailed in the 86a doc.

### git over SSH (86b)

A static `ssh` client (spike-chosen), `known_hosts`/TOFU seeded with GitHub's pinned host keys (treated as rotatable data), and `GIT_SSH_COMMAND` wiring to `git clone`/fetch/push over SSH — no git-binary changes.

### git over HTTPS (86c)

mbedTLS + curl ports, git rebuilt with curl + TLS, X.509 chain + hostname verification against the CA bundle, and PAT credential handling — `git clone`/push over HTTPS.

### Go runtime (86d)

The `mmap`/`epoll`/signal kernel work plus `ports/lang/go`, validated by a static Go binary doing a goroutine rendezvous and a plaintext HTTP GET.

### GitHub CLI (86e)

`gh` packaged behind an image feature, `GH_TOKEN` auth + credential-helper registration reusing 86c, authenticated PR/issue/CI workflows, and a documented native GitHub-REST fallback.

### Userspace SIMD / AES-NI (86f)

The SSE/AES-enabled Rust userspace target, signal-frame FPU completion, alignment verification, full re-validation, and hardware-accelerated SSH/TLS crypto.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| A real CSPRNG (86a) | SSH/TLS session keys, X25519/ECDHE ephemerals, and DNS txids built on a non-crypto PRNG are predictable; outbound auth is not trustworthy without it |
| A fail-closed wall-clock floor (86a) | Without it, every certificate is "not yet valid" and HTTPS cannot validate — a hidden cross-phase blocker for 86c |
| A SHA-256-pinned CA bundle on one canonical path (86a) | An unverified or path-mismatched trust store silently defeats TLS trust |
| One documented git remote path (86b SSH **or** 86c HTTPS) | A working secure clone is the whole point of the transport arc |
| Clearing the Go hard blockers (86d) | `gh` (86e) cannot run at all until `MAP_FIXED` + edge-`epoll` work |
| A negative TLS test (86c) | It is trivial to ship a green clone while certificate verification is silently broken |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Entropy baseline | `getrandom` is a vetted CSPRNG seeded from hardware entropy; the legacy xorshift is unreachable from any crypto path | Pull the CSPRNG work into 86a |
| Time baseline | `CLOCK_REALTIME` never returns 1970; cert validity can be checked | Land the 86a wall-clock floor before 86c |
| Transport baseline | At least one of SSH (86b) / HTTPS (86c) performs a real authenticated clone/push | Add the missing transport before closing |
| Runtime baseline | The Go runtime runs a static binary end-to-end (86d) before `gh` is bundled | Add the missing `mmap`/`epoll`/signal work to 86d |
| Trust-boundary baseline | The supported remote workflows, their limits (UDP-only DNS, IPv4-only, plaintext credential-at-rest), and the secret-handling story are documented | Add the missing support-matrix + security notes |

## Important Components and How They Work

### The 86a trust foundation

The keystone: a CSPRNG, a sane clock, a resolver, and a CA bundle. Everything downstream silently assumes these; landing them first turns later sub-phases from "fight the foundation" into "wire up the tool."

### Two transports, two trust models

SSH (86b) and HTTPS (86c) reach the same hosts by completely different means — one reusing in-tree audited crypto with `known_hosts` trust, the other a full X.509/CA/hostname stack. Keeping them as separate sub-phases keeps each trust model legible.

### The Go runtime as a distinct consumer

The GitHub-CLI arc (86d/86e) stresses the kernel through a managed runtime (`MAP_FIXED` arenas, edge-`epoll` netpoll, `SIGURG` preemption, its own `crypto/tls`), which is why it is de-risked as its own gate before the heavy `gh` artifact.

## How This Builds on Earlier Phases

- Builds on **Phase 85**'s local toolchains and `.m3pkg` substrate — the new `ca-certificates`/`mbedtls`/`curl`/`ssh`/`go`/`gh` artifacts all ride the same packaging path, and the deferred **networked `pkg` fetch** is unblocked by 86a/86c.
- Builds on **Phase 77**'s DNS reply delivery (`sys_recvmsg_inet`) and outbound TCP `connect`, and on the **Phase 48** security/entropy posture — which 86a now repairs where it was only nominal.
- Builds on **Phase 57e/60**'s FPU/XSAVE machinery, which makes 86f a build-system change rather than a kernel project.
- Reuses earlier network, crypto (`crypto-lib`/`sunset`), threading, and I/O groundwork without pulling those phases back into the release-critical path.

## Implementation Outline

1. **86a** — CSPRNG + wall-clock floor + resolver/`/etc/hosts` validation + `ca-certificates` `.m3pkg` + on-disk trust/credential conventions.
2. **86b** — ssh-client spike + ADR, build the chosen client + `known_hosts`/TOFU, wire `GIT_SSH_COMMAND`, validate `git clone` over SSH.
3. **86c** — mbedTLS + curl ports, rebuild git with curl/TLS, X.509 + hostname verification + PAT creds, validate `git clone`/push over HTTPS (with a negative case).
4. **86d** — `mmap` `MAP_FIXED` + edge-`epoll` + `SIGURG`/`tgkill`/`sched_yield`, ship `ports/lang/go`, prove a static Go binary runs.
5. **86e** — package `gh` behind an image feature, wire `GH_TOKEN` + credential helper, validate PR/issue/CI workflows, document the native fallback.
6. **86f** — SSE/AES-enabled Rust userspace target + signal-frame FPU + alignment + full re-validation + AES-NI crypto; cut the umbrella learning doc + capability inventory.

## Learning Documentation Requirement

- Create the umbrella learning doc `docs/86-networking-and-github.md` (one doc for the family, per the Phase 85 precedent) using the aligned learning-doc template in `docs/appendix/doc-templates.md`. **Owned by 86f** (the last sub-phase).
- Explain the CSPRNG/wall-clock/trust foundation, the SSH vs HTTPS trust models, the Go-runtime requirements, the GitHub CLI workflow, and the SIMD/AES-NI payoff.
- Link the learning doc from `docs/README.md` when 86f lands.

## Related Documentation and Version Updates

- Update `docs/23-socket-api.md`, `docs/git-roadmap.md` (Stage 2 lands here), `docs/archived/github-cli-roadmap.md`, `docs/research/simd-enablement.md` (mark the userspace-SSE track as scheduled in 86f), `docs/README.md`, and `docs/roadmap/README.md` (the umbrella + 86a–f rows).
- Update any security/networking docs that describe entropy, trust roots, the wall-clock, or outbound network policy — especially `kernel-core/src/prng.rs` / `crypto-lib/src/random.rs` disclaimers once the CSPRNG lands.
- Each sub-phase bumps `kernel/Cargo.toml` to its `0.86.x` version when it lands (86a `0.86.0` → 86f `0.86.5`); the umbrella aggregate is `0.86.5`.

## Acceptance Criteria

The umbrella is complete **iff all six sub-phase acceptance sets (86a–86f) pass** — each bullet below is proven by its sub-phase's gate, not re-tested at the umbrella level.

- `getrandom` is a vetted CSPRNG (host-tested statistical + forward-secrecy properties), seeded from RDSEED/RDRAND early in boot, with the legacy xorshift unreachable from any crypto path.
- The wall-clock never reports 1970; certificate validity can be evaluated.
- DNS resolves the supported hosts (A records over the Phase 77 path) and the resolver path is documented (UDP-only, IPv4-only, `/etc/hosts`-first).
- `git` performs the documented remote workflows over **SSH** (86b) and over **HTTPS** (86c), including a negative certificate-rejection test.
- A static Go binary runs end-to-end (86d), and `gh` completes the documented authenticated workflows (86e), with a documented native fallback.
- Userspace SIMD / hardware AES-NI is enabled and re-validated (86f).
- The phase docs describe the supported remote workflows, their limits, and the credential-/secret-handling story.

## Companion Task List

- [Phase 86a Task List](./tasks/86a-outbound-foundation-tasks.md)
- [Phase 86b Task List](./tasks/86b-ssh-git-transport-tasks.md)
- [Phase 86c Task List](./tasks/86c-https-git-transport-tasks.md)
- [Phase 86d Task List](./tasks/86d-go-runtime-tasks.md)
- [Phase 86e Task List](./tasks/86e-github-cli-tasks.md)
- [Phase 86f Task List](./tasks/86f-userspace-simd-tasks.md)

## How Real OS Implementations Differ

- **Redox OS** is the closest analogue: a `relibc` UDP resolver (`/etc/hosts` first, then `nameserver:53` — no daemon), a `randd` ChaCha20 CSPRNG seeded once from RDRAND, **C OpenSSL** for TLS (git/curl HTTPS the same C path m3OS takes with mbedTLS), the `redox-ssh` pure-Rust client/server (the precedent for the sunset path), and `pkgar`'s ed25519+BLAKE3 signed packages (the model for signing the networked `.m3pkg` fetch). m3OS deliberately goes **stronger than Redox on entropy** (RDSEED-preferred, refuse-keygen-on-no-entropy rather than a silent insecure seed).
- **Linux `random.c`** is the CSPRNG reference (fast-key-erasure ChaCha20, a real entropy pool with interrupt harvesting, `EMPTY`/`EARLY`/`READY` states); m3OS adopts the ChaCha20 + reseed-interval shape without the IRQ-harvest pool initially.
- Mature systems carry many more transports, trust stores, credential helpers, revocation (OCSP/CRL), background services, and a full IPv6 dual stack than m3OS should attempt here.
- The goal is a small, credible, **secure** remote developer workflow — not a general-purpose internet workstation.

## Inherited Follow-ups from Earlier Phases

- **Phase 55c follow-through for userspace `EAGAIN` visibility** — Phase 86 treats the userspace-visible `EAGAIN`/restart contract on `sys_net_send`/`sendto` as baseline and builds on it rather than reopening ownership.
- **Phase 85a networked `pkg` fetch** — the optional remote `.m3pkg` repo (DNS + HTTPS + `/etc/pkg.d/` registration, plus ed25519 package signing per `pkgar`) is unblocked once 86a/86c land; it is tracked but not required to close Phase 86.

## Pre-Planning Findings (2026-05-29) — secure-transport track

Source-verified during the Phase 77 review cycle; retained as the original planning artifact. The decisions it informed have now been made: the phase is split into 86a–86f, the SSH client is decided by an in-phase spike (the broader Rust SSH field — russh/ssh-rs/thrussh — is ruled out by the SIMD-off constraint), and the CSPRNG hardening it gestured at is the non-deferrable 86a foundation.

### What already exists (foundation is ~70% there)

| Building block | Status |
|---|---|
| DNS forward resolution (`getaddrinfo`/A records, IPv4/UDP) | ✅ Phase 77 D.1 — kernel reply delivery via `sys_recvmsg_inet`. No caching / search-domains / AAAA / DNSSEC. |
| Outbound TCP `connect()` active-open + RFC 6298 retransmit + flow control | ✅ Phase 77 D.2 + `sys_connect` → `tcp::connect` (3 s synchronous-connect cap; no TCP reassembly) |
| `zlib` (packfile inflate) | ✅ `ports/lib/zlib` |
| `sunset` — SSH **server** (`userspace/sshd`), `no_std`, pure RustCrypto | ✅ vendored as `sunset-local`; pulls X25519, Ed25519, AES, ChaCha20-Poly1305, SHA-2, HMAC |
| musl cross-toolchain + `ports/` build infra | ✅ |

### Recommended strategy: SSH-first, then HTTPS

The genuinely *minimal secure* first clone is **SSH, not HTTPS**, because it reuses the in-tree audited `sunset`/`crypto-lib` crypto and skips the entire X.509/CA/HTTP stack. (Realized as the 86b → 86c ordering.)

| Milestone | Work | Reuses | Mapped to |
|---|---|---|---|
| **M0** prove outbound INET | smoke: `connect()` to a SLIRP-forwarded host, exchange bytes | DNS, TCP connect | 86a |
| **M1** `ssh` client binary | connect → X25519 KEX → host-key TOFU → `known_hosts` → pubkey auth → exec channel | sunset/crypto-lib **or** dropbear | 86b |
| **M2** git over SSH | git's SSH transport shells out to the `ssh` binary and runs `git-upload-pack` remotely — no git-protocol code written | upstream git, zlib, M1 | 86b |
| **M3** HTTPS | TLS 1.3 client + X.509 chain + CA bundle + hostname verify + HTTP/1.1 + git smart-HTTP; rebuild git with curl+TLS | mbedTLS, git http transport | 86c |

### TLS library decision (for 86c)

Do **not** hand-roll TLS. Two m3OS constraints pin the choice: **SIMD is off** (rules out `ring`/`aws-lc-rs` — pure-Rust software backends work, proven by `sunset`), and **cert-expiry validation needs a trustworthy wall-clock** (the 86a floor). **mbedTLS + curl** (C, software crypto, full X.509, drops into the existing musl `ports/` pipeline next to `zlib`) is the choice for the git binary; the rustls + `rustls-rustcrypto` + `rustls-webpki` + `webpki-roots` path is the "correct Rust" alternative but is experimental/std-leaning and is explicitly deferred.

### Phasing note

This belongs in Phase 86 (with the local git binary in Phase 85), **not** as a "Phase 77b" — HTTPS/TLS + git is a post-1.0 headline capability, explicitly deferred behind the Phase 83 release gate.

## Deferred Until Later

- Full workstation-grade browser and GUI networking stack
- Rich credential-helper ecosystems beyond a single documented mechanism
- IPv6 / AAAA / dual-stack resolution — **Phase 89**
- DNS caching, search domains, EDNS0, DNSSEC, and DNS-over-TCP fallback
- TLS revocation (OCSP/CRL), session resumption/tickets, and client certificates
- Networked `pkg install`/`update` over HTTPS + ed25519 package signing (unblocked here, tracked separately)
- Broader language/runtime stacks beyond git/Go (`gh`) — Node.js is Phase 87
- Self-hosting the Go toolchain inside m3OS (building Go on m3OS)
