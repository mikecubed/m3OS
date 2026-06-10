# Networking and GitHub (Phase 86)

**Aligned Roadmap Phase:** Phase 86 (sub-phases 86a–86f)
**Status:** Complete
**Source Ref:** phase-86
**Supersedes Legacy Doc:** new

## Overview

Phase 86 turns m3OS from a **local developer platform** into an **authenticated
outbound** one — a system where `git clone https://github.com/…` validates a
real TLS certificate chain, `gh pr list` authenticates against the GitHub API,
and symmetric-crypto throughput is hardware-accelerated. It ships as six
sequenced sub-phases: **86a** (the trust foundation), **86b** (SSH transport),
**86c** (HTTPS/TLS), **86d** (Go runtime), **86e** (GitHub CLI), and **86f**
(userspace SIMD / AES-NI capstone). The family closes at kernel `0.86.5`.

The central lesson is **why the trust foundation has to come first**. Every
transport — SSH key exchange, TLS handshake, X25519 ephemeral, DNS transaction
ID — silently assumes three things: randomness is cryptographically
trustworthy, the wall clock can evaluate certificate validity, and there is a
canonical CA bundle on a path both tools agree on. Phase 86a lands all three
before any transport is wired, so 86b–86f become "wire up the tool" rather than
"fight the foundation."

The second lesson is **why SSH is the cheaper first secure transport**. git's
SSH path shells out to a static `ssh` binary; git itself speaks the protocol and
moves the bytes. m3OS writes no git-protocol code and rebuilds nothing — just
adds the transport client. SSH reuses in-tree audited crypto (X25519/Ed25519/
ChaCha20-Poly1305) and skips the entire X.509/CA/hostname stack that HTTPS
demands. That difference is enough to split the two git transports into separate
sub-phases.

The third lesson is **the SIMD-off constraint and when it lifts**. m3OS ran with
a soft-float userspace since inception — a consequence of sharing one build
target with the kernel, whose IRQ/exception handlers must never touch XMM. This
posture ruled out the whole `ring`/`aws-lc-rs` crypto ecosystem and drove every
transport decision in 86b–86e: dropbear for SSH, mbedTLS+curl for HTTPS, Go's
own `crypto/tls` for `gh`. It also made software ChaCha20-Poly1305 the
preferred TLS cipher (no hardware AES). 86f resolves this by splitting the
userspace build target from the kernel's — a build-system change, not a kernel
project, because the per-task XSAVE save/restore machinery (Phase 57e/60) was
already live and paying its cost on every context switch.

## What This Doc Covers

- **The trust foundation (86a)** — ChaCha20 DRBG `getrandom`, fail-closed
  wall-clock, DNS path validation, and the `ca-certificates` package.
- **SSH transport (86b)** — the dropbear-vs-sunset ADR, `known_hosts`/TOFU, and
  the `GIT_SSH` wiring model.
- **HTTPS/TLS (86c)** — mbedTLS + curl, X.509 chain + hostname verification,
  the negative cert-rejection test, and why ChaCha20-Poly1305 was preferred.
- **SSH vs HTTPS trust models** — contrasting `known_hosts` TOFU against
  X.509/CA chain validation.
- **The Go runtime (86d)** — the three concrete kernel blockers (`mmap`
  `MAP_FIXED`, edge-triggered `epoll`, `SIGURG`/`tgkill`) and how a managed
  runtime stresses a young kernel differently than a C program.
- **GitHub CLI (86e)** — the `gh` packaging, `GH_TOKEN` auth, `gh auth
  setup-git` credential-helper wiring, and m3OS's two coexisting TLS stacks.
- **Userspace SIMD / AES-NI (86f)** — the principled soft-float-kernel /
  hard-float-userspace split, the signal-frame FPU completion, `_start`
  alignment, the AES-NI throughput payoff (27× measured), and the deliberate
  deviations.
- **Where the family stops** — the DNS/IPv6 deferrals, the `ring`/`aws-lc-rs`
  constraint, and what remains for Phases 87/89/91.

## Core Implementation

### The trust foundation (86a) — CSPRNG, clock, CA bundle

Before 86a, `sys_getrandom` expanded a non-cryptographic xorshift PRNG from a
single `RDRAND ⊕ TSC` seed per call — adequate for nothing SSH or TLS needs.
`BOOT_EPOCH_SECS` was `0` on an invalid RTC, so every certificate looked
"not yet valid." There was no canonical CA bundle path.

86a replaces all three:

**CSPRNG.** A new `kernel-core/src/csprng.rs` provides a `ChaChaDrbg` fed by
an `EntropyPool`. RDSEED (full-entropy, `CPUID.07H:EBX[18]`) is preferred over
RDRAND (a CTR_DRBG output) with a TSC-degraded fallback so a hypervisor lacking
both still boots. The DRBG fast-key-erases after each output draw (forward
secrecy), gates `READY` at ≥256 credited bits, and reseeds at a 60-second-or-
output-ceiling bound. ChaCha20 is pure-integer ARX — SIMD-off-safe and host-
testable. `sys_getrandom` is rewritten to honor `GRND_NONBLOCK`/`GRND_INSECURE`/
`GRND_RANDOM`, drop the 256-byte cap, and preserve ≤256-byte single-call
atomicity (load-bearing for `sshd`'s no-loop `getrandom` consumer). `AT_RANDOM`
and the TCP ISN are switched from deterministic patterns to the CSPRNG. The
legacy `Prng` (`kernel-core/src/prng.rs`) is deleted — no kernel path can fall
back to a non-cryptographic PRNG.

**Wall-clock.** `init_rtc` sets `BOOT_EPOCH_SECS` to a build-date floor on an
invalid RTC instead of `0`, so `CLOCK_REALTIME` never returns 1970.
`MBEDTLS_X509_BADCERT_FUTURE` on dead-CMOS metal no longer looks like a TLS bug.

**CA bundle.** A new `ca-certificates` Portfile stages the Mozilla root bundle
(curl `cacert.pem`, ~121 roots, ~200 KB) to exactly `/etc/ssl/certs/
ca-certificates.crt` — the single canonical path 86c's curl and mbedTLS agree
on. This is a bundle-only port (no compiler invocation), registered in xtask's
`BUNDLE_ONLY_PORTS` list. The on-disk conventions fixed in 86a — CA at
`/etc/ssl/certs/ca-certificates.crt`, SSH known hosts at `~/.ssh/known_hosts`,
git credentials at `~/.git-credentials` + `~/.netrc` — are consumed unchanged
by every later sub-phase.

### SSH transport (86b) — the cheapest first secure clone

git's SSH transport fork/execs a static `ssh` binary on `PATH` and runs
`git-upload-pack` on the remote; git speaks the protocol and moves the bytes.
m3OS writes no git-protocol code and the Phase 85b `git` binary is untouched.

**The client choice — a spike + ADR.** The Rust SSH field is almost entirely
ruled out by the SIMD-off constraint: russh, ssh-rs, thrussh, and ssh2/libssh2-
FFI all hard-depend on `ring`/`aws-lc-rs` (asm/C crypto) or a C TLS library.
The two genuine candidates are:

- **dropbear `dbclient` (C):** mature, battle-tested, blocking (ideal for the
  `GIT_SSH` subprocess model), self-contained `libtomcrypt` software crypto
  (assembly disabled so no SIMD), built-in `known_hosts`/TOFU, ~110–200 KB.
  Same port class as git/Python/Clang.
- **sunset (Rust):** the only pure-RustCrypto SSH option that fits m3OS —
  reuses `crypto-lib` (fed by the 86a CSPRNG) and the `async-rt` reactor. But
  `Runner::new_client`/`open_client_session` have zero userspace callers — only
  the server path is wired — and there is no `known_hosts`/TOFU implementation.
  The sunset branch must budget a from-scratch async client harness + TOFU layer.

The ADR's decisive axis is `known_hosts`/TOFU cost. The result: **dropbear for
86b, sunset spike captured as the future all-Rust migration path**.

**Trust model: `known_hosts` TOFU.** The host key is data with a rotation path.
GitHub's `github.com` and `ssh.github.com` ed25519 entries are pre-seeded as
rotatable on-disk data. A mismatched host key is rejected by mandatory negative
test. The TOFU model is straightforward: accept-on-first-use, write the key,
reject on any later mismatch — no CA, no X.509.

**Non-blocking `connect`.** The Phase 86b bring-up also implemented non-blocking
TCP `connect` (`EINPROGRESS`/poll-`POLLOUT`/`getsockopt(SO_ERROR)`) which
dropbear requires; proven by `connect-smoke` in the main smoke flow.

### HTTPS/TLS (86c) — the heavier trust surface

86c forces the parts SSH skipped: a TLS 1.3 client, X.509 chain validation, a
CA bundle on a canonical path, hostname verification, and HTTP/1.1 smart-HTTP.
The classic footgun is a green clone hiding silently-broken certificate
verification, which is why a **mandatory negative test** (a self-signed cert is
rejected) is as important as the positive case.

**TLS library choice.** SIMD-off rules out the entire `ring`/`aws-lc-rs`
ecosystem — but not C TLS. mbedTLS is a small, static, musl-friendly TLS 1.3
client with full X.509 that drops into the `ports/` pipeline, and
`curl --with-mbedtls` is curl's small-footprint backend. The pure-Rust rustls
path is explicitly deferred: `rustls-rustcrypto` is not production-ready,
`crypto-lib` lacks `p256`/`ecdsa` (GitHub's leaf is ECDSA P-256,
live-confirmed), and it would not serve the C git binary.

**ChaCha20-Poly1305 cipher preference.** Until 86f, the userspace target is
soft-float so AES-GCM is slow and cache-timing-exposed. GitHub offers
`TLS_CHACHA20_POLY1305_SHA256`, so 86c configures mbedTLS to prefer the ARX
suite — a natural fit for the SIMD-off period.

**git rebuild.** 86c removes `NO_CURL` from `build_git` and inverts the Phase
85b absence-assertions: `git-remote-https` must exist, `curl_multi_perform` +
`mbedtls_ssl_handshake` must be present, `SSL_CTX_new` must be absent (the
backend is mbedTLS-via-curl, not OpenSSL). The `DEPS` chain —
`zlib → mbedtls → ca-certificates → curl → git` — flows through the Phase 85a
topological solver unchanged.

**git smart-HTTP.** `GET info/refs?service=git-upload-pack` returns an
`application/x-git-upload-pack-advertisement` (5-byte pkt-line magic), then
`POST git-upload-pack` returns a side-band-64k packfile result. The smoke
asserts the Content-Type and pkt-line magic, then a live clone of
`octocat/Hello-World` completes.

### SSH vs HTTPS trust models — a direct comparison

| Property | SSH (`known_hosts`) | HTTPS (X.509/CA) |
|---|---|---|
| Trust anchor | First-seen host key (TOFU) | Pre-installed CA bundle |
| What is verified | Server's public key identity | Server's certificate chain + hostname |
| Key rotation | New host key → manual update | CA re-issues cert (transparent to user) |
| Attack surface | BGP/DNS hijack ≤ first connection | Compromised CA, cert misissuance |
| m3OS setup cost | Seed GitHub keys as rotatable data | Ship 200 KB `ca-certificates.crt` |
| Negative test | Mismatch-reject at KEX | Bad-cert REJECT (`self-signed.badssl.com`) |

Neither model is strictly superior. SSH's TOFU model means a hijack before the
first connection is undetected. HTTPS's CA model means a compromised root CA
can issue fraudulent certificates for any domain. m3OS ships both and lets the
user choose.

### The Go runtime (86d) — managed runtimes stress a young kernel

Running *any* Go binary is a bring-up task because the Go runtime is a managed
runtime that uses the kernel in ways a C program never does. Three concrete gaps
had to be cleared:

**Hard blocker 1 — `mmap` `MAP_FIXED` + `PROT_NONE` reservations.** Go's
allocator reserves 64 MB arenas `PROT_NONE` then commits them `PROT_RW`
`MAP_FIXED` at the exact same address. Before 86d, `sys_linux_mmap` discarded
the address hint and masked `MAP_FIXED`, so the first arena landed at the wrong
address and Go aborted. The fix honors `MAP_FIXED` at the requested address and
records `PROT_NONE` VMAs in the VMA tree without committing frames.

**Hard blocker 2 — edge-triggered `epoll`.** Go's netpoll registers every fd
with `EPOLLIN | EPOLLOUT | EPOLLRDHUP | EPOLLET`. Before 86d, m3OS `epoll` was
level-triggered only — `EPOLLET` was silently ignored — so Go's netpoll busy-
looped or hung. Edge state must be per-interest (not per-fd) because the same fd
can sit in multiple epoll sets; `EPOLLRDHUP` must fire on peer half-close.

**Soft blocker — `SIGURG` / `tgkill` async preemption.** Go uses
`tgkill(tid, SIGURG)` to preempt goroutines and stop the world for GC.
`SIGURG` was undefined; `tgkill` did not exist. 86d wires `SIGURG`(23),
`tgkill`(234), and the `SA_SIGINFO` `ucontext` (so `doSigPreempt` can read the
interrupted RIP at `ucontext+0xa8` and rewrite the goroutine PC to
`asyncPreempt`). Signal delivery happens at syscall-return — not timer-IRQ-
return — which is sufficient for the I/O-bound goroutine + GC stop-the-world
case (`go-runtime-smoke`'s channel rendezvous and plaintext HTTP GET). A pure
compute-bound goroutine with no syscall between safepoints would not be
async-preempted; closing that gap requires timer-IRQ-return delivery, deferred.

The bulk of the Go runtime substrate was already present:
`clone(CLONE_THREAD|…)`, `futex`, `arch_prctl(ARCH_SET_FS)`, `gettid`,
`/proc/self/exe`, `clock_gettime`, `sched_getaffinity`. 86d is targeted
bring-up, not greenfield.

### GitHub CLI (86e) — packaging, auth, and two TLS stacks

`gh` is a ~55 MB Go binary (`CGO_ENABLED=0`) carrying its own `crypto/tls`.
It runs on the 86d runtime, ships behind an `M3OS_WITH_GH` image feature
(mirroring `M3OS_WITH_CLANG`), and authenticates non-interactively via
`GH_TOKEN`. The token lives at mode `0600` under `~/.config/gh/`; it never
crosses the serial line or `/tmp`.

`gh auth setup-git` registers `gh` as a git credential helper. When git then
performs an HTTPS operation, it shells out to `gh` for the credential; the
*transport* is still the 86c curl + mbedTLS + PAT path. 86e supplies the
credential; 86c moves the bytes. This is the credential-helper handshake that
makes the two stacks cooperate.

m3OS deliberately carries **two TLS stacks**: mbedTLS for `git`/`curl`, and
Go's `crypto/tls` for `gh`. Both consume the same 86a CA bundle — mbedTLS via
`--with-ca-bundle`, Go via `SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`.
The clock and entropy foundation is also shared.

### Userspace SIMD / AES-NI (86f) — principled split, build-system change

**Why this is a build-system change and not a kernel project.** The Phase 57e/60
XSAVE machinery — `enable_xsave_state()` (`kernel/src/arch/x86_64/cpuid.rs`,
`XSAVE_FEATURE_MASK = 0x7`), `save_fpu_state`/`restore_fpu_state` (`xsaveopt64`/
`xsave64`) around every `switch_context` in `kernel/src/task/scheduler.rs` — was
already live. The XMM/YMM register file was being saved and restored on every
context switch, even though no code used those registers. The per-switch cost was
already paid for a benefit no one collected. Enabling SIMD is primarily adding
the *consumers*.

**The principled split.** `x86_64-m3os.json` — formerly vestigial, carrying an
explicit `-mmx,-sse,-sse2,…,-avx,-avx2,+soft-float` list — is repurposed to a
hardware-float Rust userspace target with `+sse,+sse2,+aes` and the
`+soft-float` feature removed. `xtask`'s `build_userspace_bins` userspace
`--target` invocations are pointed at this target (the dynamic linker
`ld-musl` deliberately stays on `x86_64-unknown-none` — it must remain
PIE/`ET_DYN`, and the loader has no need for SSE). The **kernel** stays on the
built-in `x86_64-unknown-none` (`-sse`, `+soft-float`) unchanged. The two are
deliberately decoupled:

- Ring 0 stays soft-float: IRQ/exception handlers never emit XMM, no FPU save
  is needed in interrupt entry, and the existing task-boundary XSAVE save/
  restore stays sufficient.
- Ring 3 (Rust userspace) gains `+sse,+sse2,+aes`: the `aes` crate's runtime
  AES-NI autodetection via `cpufeatures` can now select the hardware backend
  (XMM/AES-NI codegen is permitted), and the whole Rust userspace tree gets
  SSE2 register availability for free.

**Signal-frame FPU.** The signal frame already reserved an `fpstate` slot in
`kernel/src/signal.rs`. 86f completes the path: save the task's FPU state into
the slot on signal delivery, restore it on `sigreturn`, so an SSE-using signal
handler cannot corrupt the interrupted context's XMM registers.

**Entry-stack alignment.** SSE `movaps`/register spills fault on misalignment.
`setup_abi_stack_with_envp` (`kernel/src/mm/elf.rs`) is verified to land the
initial RSP + auxv 16-byte aligned at `_start`, satisfying the SysV psABI
guarantee. musl `_start` realigns, but the kernel-supplied stack must already be
aligned for any early SSE-spilling path.

**The AES-NI payoff — measured.** A host A/B microbenchmark over a fixed
AES-256-CTR payload at a fixed iteration count:

| Build | Throughput |
|---|---|
| Soft-float (forced-soft fixsliced, no XMM) | 203 MiB/s |
| Hardware-float + AES-NI (`xmm` + `aesenc`/`aesenclast`) | 5,459 MiB/s |

**Ratio: ≈27×.** The acceptance threshold is ≥2×, chosen as a conservative
floor that would be unmistakably reached even under QEMU/TCG distortion (where
virtual AES-NI may be slower than bare metal). On real silicon the win is
closer to 27×. The 2× factor is justified as the minimum falsifiable boundary,
not a prediction of real-silicon throughput.

`objdump -d` of the rebuilt `crypto-lib`-consuming binary shows `aesenc`/
`aesenclast` rather than the table-driven software S-box, confirming the `aes`
crate's `cpufeatures` runtime detection engaged once the hardware-float target
permitted XMM codegen.

**Deliberate deviations.** The ldso (`ld-musl-x86_64.so.1`) stays on
`x86_64-unknown-none` — the dynamic linker must remain PIE/`ET_DYN` (the
m3os target is `position-independent-executables: false`) and the loader has
no need for SSE. Userspace ELFs built with `x86_64-m3os.json` are
`relocation-model: static` → ET_EXEC (not PIE), consistent with the Phase
44/85 `no_std` userspace pattern. `+avx` is deferred conservatively to bound
the rebuild's blast radius (the Phase 57e/60 XSAVE substrate already saves
YMM state; only AVX-512 would require bumping
`XSAVE_FEATURE_MASK`/`XSAVE_AREA_SIZE` and the XCR0 mask). The target's
`"os"` field must stay `"none"` — a non-`"none"` value flips `target_os` and
silently compiles `driver_runtime`'s `cfg(target_os = "none")` device-host
syscall wrappers to their host-test fallbacks (found the hard way: the
vestigial `"os": "m3os"` blinded every ring-3 PCI driver until the
`e1000-restart-crash` regression arm caught it). Finally, compile-time `+aes`
makes the `aes` crate's `cpufeatures` check constant-fold to "available", so
SSE-userspace m3OS now assumes AES-NI hardware (any post-2010 x86_64; QEMU
TCG runs gained `+aes` in the shared `-cpu` flags since `qemu64` does not
advertise it).

**What enabling SSE does NOT unlock.** Enabling SSE/AES does not make
`ring`/`aws-lc-rs` build on m3OS. Those crates fail due to their asm/C build
scripts and hosted-target assumptions, independent of the SSE flag. So 86f does
not expand the Rust crypto-crate field — the 86b SSH client decision (dropbear)
is unaffected, and 86c's mbedTLS choice remains the correct HTTPS backend.

**The C ports are already SSE2.** git, Python, and Clang (Phase 85) are ordinary
SSE2 musl binaries built with `CFLAGS=-O2` — no `-mno-sse`. 86f's novelty is
the *Rust* userspace target and the AES-NI backend, not basic SSE.

## Key Files

| File | Purpose |
|---|---|
| `kernel-core/src/csprng.rs` | `ChaChaDrbg` + `EntropyPool` — the 86a CSPRNG; host-tested |
| `kernel/src/arch/x86_64/cpuid.rs` | `enable_xsave_state()` — hardware XSAVE enable (Phase 57e/60); `rdseed64` / `cpu_has_rdseed` (86a) |
| `kernel/src/task/scheduler.rs` | `save_fpu_state` / `restore_fpu_state` — per-switch XSAVE around `switch_context` (Phase 57e/60) |
| `kernel/src/signal.rs` | Signal-frame `fpstate` slot — reserved earlier, populated/restored by 86f |
| `kernel/src/mm/elf.rs` | `setup_abi_stack_with_envp` — initial RSP + auxv 16-byte alignment (86f) |
| `kernel/src/rtc.rs` | `init_rtc` build-date floor (86a) |
| `kernel/src/arch/x86_64/syscall/mod.rs` | `sys_getrandom` (86a rewrite), `sys_linux_mmap` `MAP_FIXED` (86d), edge-triggered `epoll` (86d), `tgkill`/`SIGURG` (86d) |
| `ports/lib/ca-certificates/Portfile` | SHA-256-pinned Mozilla CA bundle to `/etc/ssl/certs/ca-certificates.crt` |
| `ports/util/dropbear/Portfile` | Static `ssh` client (86b) |
| `ports/lib/mbedtls/Portfile` | Trimmed client-only mbedTLS 3.6.x (86c) |
| `ports/util/curl/Portfile` | `libcurl --with-mbedtls` (86c) |
| `ports/util/git/Portfile` | `git` rebuilt with `NO_CURL` removed (86c; 85b was local-only) |
| `ports/lang/go/Portfile` | Static `CGO_ENABLED=0` Go 1.24 runtime (86d) |
| `ports/util/gh/Portfile` | Static `gh` 2.82.1, bundled behind `M3OS_WITH_GH` (86e) |
| `x86_64-m3os.json` | Repurposed hardware-float Rust userspace target (86f) |
| `userspace/crypto-lib/` | `aes`/`chacha20poly1305` workspace deps; AES-NI runtime-autodetected by `cpufeatures` (86f) |
| `xtask/src/port_build.rs` | `build_dropbear`, `build_mbedtls`, `build_curl`, `build_git`, `build_go`, `build_gh` |
| `xtask/src/main.rs` | `cmd_git_ssh_smoke`, `cmd_git_https_smoke`, `cmd_go_runtime_smoke`, `cmd_gh_smoke`, `cmd_userspace_simd_smoke`; image feature gates (`M3OS_WITH_GH`) |

## How the Phase 86 Family Differs From Later Networking Work

- **DNS is minimal** (UDP-only, IPv4-only, `/etc/hosts`-first, no caching,
  no DNSSEC) — Phase 89 (IPv6/DHCPv6) is the next DNS/resolver work.
- **`ring`/`aws-lc-rs` are not unlocked by 86f** — those need asm/C build
  scripts + hosted-target assumptions. The Rust crypto stack on m3OS is limited
  to `rustcrypto`-family crates, supplemented by C crypto in the ports tree.
- **No revocation (OCSP/CRL)** — TLS validation checks chain and hostname only;
  revocation is a future hardening item.
- **Python TLS/DNS/`pip`/`asyncio`** remain deferred within Phase 86 (no
  `ssl` module shipped).
- **HTTPS-over-Go** (the Go `crypto/tls` stack calling GitHub) is proven in 86e
  via `gh`; Go programs writing their own HTTPS clients ride that same path.
- **AVX / in-kernel SIMD** — `+avx` deferred (requires `XSAVE_FEATURE_MASK`/
  `XSAVE_AREA_SIZE` bump + XCR0 mask). In-kernel SSE (fast memcpy or in-kernel
  crypto) would require `kernel_fpu_begin`/`kernel_fpu_end`-style guards or
  IRQ-prologue FPU save — the kernel deliberately stays soft-float.

## Related Roadmap Docs

- [Phase 86 umbrella design doc](./roadmap/86-networking-and-github.md) —
  theme, sub-phase decomposition, shared crypto/transport architecture
- [Phase 86a — Outbound Foundation](./roadmap/86a-outbound-foundation.md)
- [Phase 86a Task List](./roadmap/tasks/86a-outbound-foundation-tasks.md)
- [Phase 86b — SSH + git over SSH](./roadmap/86b-ssh-git-transport.md)
- [Phase 86b Task List](./roadmap/tasks/86b-ssh-git-transport-tasks.md)
- [Phase 86c — HTTPS/TLS + git smart-HTTP](./roadmap/86c-https-git-transport.md)
- [Phase 86c Task List](./roadmap/tasks/86c-https-git-transport-tasks.md)
- [Phase 86d — Go-Runtime Gate](./roadmap/86d-go-runtime.md)
- [Phase 86d Task List](./roadmap/tasks/86d-go-runtime-tasks.md)
- [Phase 86e — GitHub CLI + Native Fallback](./roadmap/86e-github-cli.md)
- [Phase 86e Task List](./roadmap/tasks/86e-github-cli-tasks.md)
- [Phase 86f — Userspace SIMD / AES-NI Capstone](./roadmap/86f-userspace-simd.md)
- [Phase 86f Task List](./roadmap/tasks/86f-userspace-simd-tasks.md)
- [SIMD Enablement Research](./research/simd-enablement.md) — the 86f source doc

## Deferred or Later-Phase Topics

- Full-stack browser and GUI networking
- IPv6 / AAAA / dual-stack resolution — Phase 89
- DNS caching, search domains, EDNS0, DNSSEC, DNS-over-TCP fallback
- TLS revocation (OCSP/CRL), session resumption/tickets, client certificates
- Networked `pkg install`/`update` over HTTPS + ed25519 package signing
  (unblocked by 86a/86c, tracked separately)
- Python TLS/DNS/`pip`/`asyncio` — deferred within Phase 86 /
  Phase 91 (`ctypes`/`dlopen`)
- `ring`/`aws-lc-rs` crate support (blocked by asm/C build + hosted-target
  assumptions, independent of the 86f SSE flag)
- AVX / in-kernel SIMD — requires `XSAVE_FEATURE_MASK`/`XSAVE_AREA_SIZE` bump
- Broader runtime stacks — Node.js is Phase 87, Claude Code is Phase 88
