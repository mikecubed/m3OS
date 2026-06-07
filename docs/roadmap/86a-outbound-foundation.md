# Phase 86a - Outbound Foundation (CSPRNG, Wall-clock, Resolver, CA Trust)

**Status:** Planned
**Source Ref:** phase-86a
**Depends on:** Phase 48 (Security Foundation) ✅, Phase 77 (Pre-1.0 Cleanup — DNS reply delivery D.1 + outbound TCP `connect` D.2) ✅, Phase 85a (Package & Build-Cache Infrastructure) ✅
**Builds on:** First sub-phase (86a) of the Phase 86 umbrella ([./86-networking-and-github.md](./86-networking-and-github.md)) — repairs the entropy/time/trust foundation that all of 86b–86f silently assume, without landing any transport.
**Primary Components:** `kernel-core/src/csprng.rs` (new), `kernel-core/src/prng.rs` (legacy, quarantined), `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_getrandom`, `rdrand64`, new `rdseed64`), `kernel/src/lib.rs` (`kernel_main_entry`), `kernel/src/rtc.rs` (`init_rtc`, `BOOT_EPOCH_SECS`), `kernel/src/mm/elf.rs` (AT_RANDOM), `kernel/src/net/tcp.rs` (TCP ISN), `ports/lib/ca-certificates/Portfile` (new), `xtask/src/main.rs` (`populate_ext2_files` resolv.conf, ports registry), `userspace/dns-smoke/dns-smoke.c`

## Milestone Goal

Land the trust foundation that every later Phase 86 sub-phase depends on: a real **ChaCha20 DRBG `getrandom`** (RDSEED→RDRAND→TSC seeded, flags honored, ≤256-byte atomicity preserved, the 256-byte cap removed), a **fail-closed wall-clock floor** so certificate validity can be evaluated, a **validated IPv4/A-record resolver path** (`/etc/hosts` first, then a single nameserver over the Phase 77 UDP path), and the on-disk **CA / `known_hosts` / credential conventions** plus a **SHA-256-pinned `ca-certificates` `.m3pkg`**. No SSH, no TLS, no transport — just the precondition. Kernel `0.86.0`.

## Why This Phase Exists

Phase 85 made m3OS a local developer platform; Phase 86 makes it an *authenticated outbound* one. The moment a session key, an X25519 ephemeral, a TLS handshake, or a DNS transaction ID is generated, three quiet assumptions become load-bearing — and m3OS does not currently meet any of them:

- **Randomness is non-cryptographic.** `sys_getrandom` (`kernel/src/arch/x86_64/syscall/mod.rs:15075`) expands the **xorshift64-multiply `Prng`** (`kernel-core/src/prng.rs:16`, doc'd at `kernel-core/src/prng.rs:4` as "NOT cryptographically secure") from a single 64-bit `RDRAND ⊕ TSC` seed *per call*, drops the `_flags` argument, caps the request at 256 bytes, and reseeds every 256 bytes. That is adequate for nothing SSH or TLS needs.
- **The wall-clock can read 1970.** `init_rtc` (`kernel/src/rtc.rs:198`) leaves `BOOT_EPOCH_SECS = 0` (`kernel/src/rtc.rs:10`) on an invalid RTC. An epoch-1970 clock makes every certificate `notBefore`-in-the-future (`MBEDTLS_X509_BADCERT_FUTURE`), which on dead-CMOS metal looks exactly like a TLS bug — a hidden cross-phase blocker for 86c.
- **There is no trust root.** mbedTLS ships no default trust store; an unpinned, path-mismatched bundle silently defeats verification.

Doing this as a standalone sub-phase means the hard, security-critical foundation is landed and host-tested *before* any transport is wired on top of it, so 86b/86c/86e become "wire up the tool" rather than "fight the foundation."

## Learning Goals

- Why a CSPRNG, a sane wall-clock, and a CA trust root are the *precondition* for secure outbound tooling, not an afterthought bolted on at the transport layer.
- How a Linux-`random.c`-style ChaCha20 DRBG works (entropy crediting, an `EMPTY`/`EARLY`/`READY` state machine, fast-key-erasure forward secrecy, reseed interval) and why its ARX core is SIMD-off-safe and host-testable.
- Why RDSEED (full-entropy, `CPUID.07H:EBX[18]`) is preferred over RDRAND (a CTR_DRBG output ≤128-bit), and how to condition either through a software DRBG to neutralize sources like the 2025 AMD RDSEED bias.
- How `getrandom` flag semantics (`GRND_NONBLOCK`/`GRND_INSECURE`/`GRND_RANDOM`) and the ≤256-byte single-call atomicity contract interact with real consumers (`sshd`'s no-loop `getrandom`).
- Why a fail-closed clock floor and a single canonical CA path are the difference between "TLS works" and "TLS silently doesn't verify."

## Feature Scope

### Kernel CSPRNG (Track A)

A new `kernel-core/src/csprng.rs` provides a ChaCha20 DRBG (`ChaChaDrbg`) fed by an `EntropyPool`, seeded ≥256 credited bits before it reports `READY`, with fast-key-erasure forward secrecy and a bounded reseed interval. The seed is drawn from a new `rdseed64()` (full-entropy, probed via `CPUID.07H:EBX[18]`) preferring over the existing `rdrand64()` (`kernel/src/arch/x86_64/syscall/mod.rs:5724`), with a TSC-mixed degraded path so a hypervisor lacking both still boots. `sys_getrandom` is rewritten to honor flags, source the DRBG, and drop the 256-byte cap while preserving ≤256-byte single-call atomicity. The two downstream consumers — `AT_RANDOM` (`kernel/src/mm/elf.rs:666`) and the TCP ISN (`kernel/src/net/tcp.rs:250`) — are switched from their deterministic patterns to the CSPRNG. The legacy `Prng` is quarantined so it is grep-unreachable from any crypto path.

### Wall-clock trust (Track B)

`init_rtc` gets a **build-date floor**: an invalid RTC sets `BOOT_EPOCH_SECS` to the image build date (logged) instead of `0`, so `tsc_now_us` (`kernel/src/arch/x86_64/syscall/mod.rs:15145`) and `sys_clock_gettime` (`:15199`) can never return 1970. This makes certificate `notBefore`/`notAfter` checks fail-closed-but-sane and records the first-boot insecure-skip-time decision.

### DNS config + CA-trust paths (Track C)

The IPv4/A-record resolver path is *validated* (not rewritten): `getaddrinfo(host, AF_INET)` resolves an A record over the Phase 77 `sys_recvmsg_inet` UDP path, `/etc/hosts` first then the single `/etc/resolv.conf` nameserver. AAAA/IPv6 is explicitly scoped out. A new SHA-256-pinned `ca-certificates` `.m3pkg` stages the Mozilla root bundle (curl `cacert.pem`, ~121 roots, ~200 KB) to one canonical path that 86c's `curl`/`git` agree on, and this doc fixes the on-disk trust/credential conventions.

### Version bump (Track D)

`kernel/Cargo.toml` `0.85.3` → `0.86.0` — the first patch bump of the Phase 86 umbrella sequence.

## Important Components and How They Work

### `ChaChaDrbg` / `EntropyPool` (`kernel-core/src/csprng.rs`, new)

A host-testable DRBG: a 32-byte ChaCha20 key + counter, rekeyed after each output draw (fast-key-erasure) so a captured post-draw state cannot reproduce prior output. The `EntropyPool` accumulates credited bits from the seed source and gates the `READY` transition at ≥256 bits. Because ChaCha20 is pure-integer ARX, it never touches XMM and runs identically in the kernel (`no_std`) and in `cargo test` host builds. The DRBG reseeds at a 60-second-or-output-ceiling bound (the Linux `CRNG_RESEED_INTERVAL` shape, minus the IRQ-harvest pool, which m3OS does not have yet).

### `rdseed64` / `cpu_has_rdseed` (`kernel/src/arch/x86_64/syscall/mod.rs`)

New, modeled on the existing `cpu_has_rdrand` (`:5701`) / `RDRAND_SUPPORT` (`:5718`) / `rdrand64` (`:5724`) trio: probe `CPUID.07H:EBX[18]`, cache the result in an `AtomicU8`, PAUSE-retry on `CF=0`, and credit full-entropy only when `CF=1`. Seed selection is RDSEED → RDRAND → TSC-degraded, with the chosen source and credited-bit count logged at boot.

### `sys_getrandom` (`kernel/src/arch/x86_64/syscall/mod.rs:15075`)

Rewritten to read from `ChaChaDrbg` instead of the per-call `seed_pseudorandom_state` (`:5759`) / `fill_pseudorandom_bytes` (`:5770`) / `copy_pseudorandom_to_user` (`:5795`) xorshift path. It now: maps `GRND_NONBLOCK`→`EAGAIN` iff `!ready`, serves `GRND_INSECURE` pre-`READY`, honors `GRND_RANDOM`, rejects a bad flag combo with `EINVAL`, removes the 256-byte cap, and guarantees that every ≤256-byte request returns its exact length in a single call. The userspace wrapper (`userspace/syscall-lib/src/lib.rs:3231`) already loops, but `sshd`'s backend (`userspace/sshd/src/getrandom_impl.rs:5`) requires `ret == len` in one call — so the ≤256-byte atomicity contract is non-negotiable.

### `init_rtc` build-date floor (`kernel/src/rtc.rs:198`)

On an invalid datetime, instead of leaving `BOOT_EPOCH_SECS = 0` it stores a compile-time build-date constant and logs the substitution, so `CLOCK_REALTIME` is always ≥ floor.

### `ca-certificates` Portfile (`ports/lib/ca-certificates/Portfile`, new)

A bundle-only port (no compiler invocation — it stages a verified data blob) registered in `xtask/src/main.rs` next to the `PORTS` / `BUNDLE_ONLY_PORTS` registry (`:17446` / `:17541`, populated by `populate_phase_69d_ports` at `:17445`). It pins the upstream `cacert.pem` by SHA-256 and stages it to `/etc/ssl/certs/ca-certificates.crt`.

## How This Builds on Earlier Phases

- Extends **Phase 48**'s security/entropy posture, which was only nominal — `kernel-core/src/prng.rs` self-documents as not cryptographically secure — by replacing the xorshift expansion under `getrandom` with a vetted ChaCha20 DRBG.
- Reuses **Phase 77**'s DNS reply delivery (`sys_recvmsg_inet`) and outbound TCP `connect` (`sys_connect` → `tcp::connect`) as the resolver/transport substrate; 86a only *validates* the A-record path and hardens the ISN (`kernel/src/net/tcp.rs:250`) that Phase 77 left as `tick_count()`.
- Reuses the **Phase 85a** `.m3pkg` packaging substrate + offline installer to ship `ca-certificates` as a content-addressed, SHA-256-pinned bundle on the standard ports path.
- Replaces the deterministic `AT_RANDOM` fill (`kernel/src/mm/elf.rs:666`, `(0xAB ^ i).wrapping_add(i)`) — which gave every binary identical stack canaries/ASLR — with live CSPRNG bytes.

## Implementation Outline

1. Add `kernel-core/src/csprng.rs` (`ChaChaDrbg` + `EntropyPool`, `EMPTY`/`EARLY`/`READY` states, fast-key-erasure, reseed bound) with host tests; quarantine the legacy `Prng`.
2. Add `rdseed64`/`cpu_has_rdseed` and the RDSEED→RDRAND→TSC seed selector with a boot log line naming the source + credited bits.
3. Seed the DRBG in `kernel_main_entry` (`kernel/src/lib.rs:71`) synchronously right after `mm::init` and before `init_task`; audit and move/accept the pre-seed consumers (canary, ASLR slide, TCP ISN, DNS txid).
4. Rewrite `sys_getrandom` to source the DRBG, honor `GRND_*` flags, drop the 256-byte cap, and preserve ≤256-byte single-call atomicity; reseed at the 60-second-or-output-ceiling bound.
5. Switch `AT_RANDOM` and the TCP ISN to the CSPRNG (RFC 6528 ISN mixing).
6. Add the `init_rtc` build-date floor + fail-closed contract.
7. Validate `getaddrinfo(github.com, AF_INET)` over the Phase 77 path (`/etc/hosts`-first), document the resolver limits, and add the SHA-256-pinned `ca-certificates` port to one canonical path.
8. Document the on-disk CA/`known_hosts`/credential conventions and the one-time weak-secret rotation note; bump kernel `0.86.0`.

## Acceptance Criteria

- A `kernel-core` host test proves the DRBG reports `READY` only after ≥256 credited bits, that `GRND_NONBLOCK`→`EAGAIN` iff `!ready` while `GRND_INSECURE` serves pre-`READY`, that fast-key-erasure prevents a recovered post-draw state from reproducing prior output, and that 1 MiB of output passes monobit + chi-square — and the legacy xorshift `Prng` is grep-unreachable from any csprng path.
- `rdseed64` probes `CPUID.07H:EBX[18]`, caches an `AtomicU8`, PAUSE-retries on `CF=0`, credits full-entropy only on `CF=1`; the boot log emits the seed source (`rdseed`|`rdrand`|`degraded`) + credited-bit count, and the degraded path still boots (no deadlock).
- The DRBG is `READY` synchronously after `mm::init` and before `init_task`, asserted by boot-log ordering; an audit note enumerates every pre-seed consumer (stack canary, ASLR slide `kernel/src/mm/elf.rs`, TCP ISN `kernel/src/net/tcp.rs:250`, DNS txid) as moved-after-seed or accepted-degraded.
- `sys_getrandom`: `GRND_RANDOM` honored, bad flag combo → `EINVAL`, every ≤256-byte call returns the exact length in one call, >256-byte succeeds (cap removed), reseed honored at the 60-second-or-output-ceiling bound.
- `AT_RANDOM` is 16 live CSPRNG bytes per load (two processes observe different canaries); the TCP ISN mixes CSPRNG per RFC 6528 (two connections get non-sequential, non-`tick_count` ISNs).
- A forced-bad-RTC smoke confirms `CLOCK_REALTIME` ≥ the build-date floor and that `tsc_now_us`/`sys_clock_gettime` never return 1970; the first-boot insecure-skip-time decision is logged.
- `getaddrinfo("github.com", AF_INET)` resolves an A record over `sys_recvmsg_inet` and `dns-smoke` reports **PASS** (not SKIP), traced to `open(/etc/resolv.conf)` + `/etc/hosts`-first.
- `ca-certificates` stages `cacert.pem` to exactly `/etc/ssl/certs/ca-certificates.crt` (the single canonical path 86c's `curl --with-ca-bundle` will agree with); this doc records CA = `/etc/ssl/certs/ca-certificates.crt`, `known_hosts` = `~/.ssh/known_hosts` (+ `/etc/ssh` seed), credentials = `~/.git-credentials` + `~/.netrc`.
- `kernel/Cargo.toml` reads `0.86.0` (+ `Cargo.lock`); `cargo xtask check` is clean; the boot banner / `uname` report `0.86.0`.

## Companion Task List

- [Phase 86a Task List](./tasks/86a-outbound-foundation-tasks.md)

## How Real OS Implementations Differ

- **Linux `random.c`** is the CSPRNG reference: fast-key-erasure ChaCha20, `POOL_READY_BITS = 256`, `CRNG_RESEED_INTERVAL = 60*HZ`, an `EMPTY`/`EARLY`/`READY` state machine, and a real entropy pool with interrupt harvesting (`add_interrupt_randomness`). m3OS adopts the ChaCha20 + reseed-interval shape but has **no entropy pool / IRQ harvest yet** — it relies on RDSEED/RDRAND at the seed point.
- **Redox `randd`** seeds a `ChaCha20Rng` once from 32 RDRAND bytes and prints "NOT SECURE" on a zero seed; m3OS deliberately goes **stronger** — RDSEED-preferred, and it should refuse keygen rather than seed silently insecure.
- **Intel DRNG** distinguishes RDRAND (a ≤128-bit CTR_DRBG output) from RDSEED (full-entropy); m3OS conditions whichever it gets through its own DRBG, which also neutralizes the 2025 AMD RDSEED bias (AMD-SB-7055).
- **Redox `relibc` `netdb`/`lookup.rs`** resolves `/etc/hosts` first then a UDP `nameserver:53` with no daemon — the same shape m3OS validates here.
- **curl `cacert.pem`** is the ~200 KB SHA-256-pinned Mozilla bundle this phase packages; distributions instead ship a system trust store managed by `update-ca-certificates`.

## Deferred Until Later

- IPv6 / AAAA / dual-stack resolution — Phase 89.
- DNS caching, search domains, EDNS0, DNSSEC, and DNS-over-TCP fallback.
- An entropy pool with interrupt harvesting (`add_interrupt_randomness`-style) — RDSEED/RDRAND at the seed point suffices for now.
- Rotating already-persisted weak secrets generated under the old PRNG (the `sshd` Ed25519 host key at `userspace/sshd/src/host_key.rs:43`, `passwd`/`shadow` salts) — this phase documents a one-time rotation step + updates the `crypto-lib/src/random.rs` / `kernel-core/src/prng.rs:4` disclaimers, but does not auto-rotate.
- All transport (SSH 86b, HTTPS/TLS 86c), the Go runtime (86d), `gh` (86e), and the userspace SIMD / AES-NI capstone (86f).
