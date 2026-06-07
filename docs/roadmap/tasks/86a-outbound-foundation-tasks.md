# Phase 86a — Outbound Foundation (CSPRNG, Wall-clock, Resolver, CA Trust): Task List

**Status:** Complete (`feat/phase-86a-outbound-foundation`, PR #227) — kernel `0.86.0`
**Source Ref:** phase-86a
**Depends on:** Phase 48 (Security Foundation) ✅, Phase 77 (Pre-1.0 Cleanup — DNS reply delivery D.1 + outbound TCP `connect` D.2) ✅, Phase 85a (Package & Build-Cache Infrastructure) ✅
**Goal:** Land the entropy/time/trust foundation that all of Phase 86's transports (86b SSH, 86c HTTPS, 86e `gh`) silently depend on — a ChaCha20 DRBG `getrandom` (RDSEED→RDRAND→TSC seeded, flags honored, ≤256-byte atomicity preserved, the 256-byte cap removed), a fail-closed wall-clock floor so certificates can be validated, a validated IPv4/A-record resolver path, and the on-disk CA/`known_hosts`/credential conventions plus a SHA-256-pinned `ca-certificates` `.m3pkg` — with **no transport** landed here. Kernel `0.86.0`.

> **Implemented via `/flow:parallel-impl`** (Tracks A + C in parallel, then B, then D). See [`86a-track-report.md`](./86a-track-report.md) for the track/batch record. Every acceptance item below is checked `[x]` with the verification that established it.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel CSPRNG (ChaCha20 DRBG, RDSEED seeding, early seed, `getrandom` rewrite, downstream consumers) | — | ✅ Merged |
| B | Wall-clock trust (build-date floor + fail-closed contract) | — | ✅ Merged |
| C | DNS config validation + CA-trust paths (`ca-certificates` `.m3pkg`, canonical trust/credential paths) | — | ✅ Merged |
| D | Version bump `0.85.3` → `0.86.0` | A, B, C | ✅ Merged |

---

## Track A — Kernel CSPRNG

### A.1 — ChaCha20 DRBG + entropy pool in `kernel-core`

**Files:**
- `kernel-core/src/csprng.rs` (new)
- `kernel-core/src/prng.rs` (legacy `Prng::new` at `:16`, "NOT cryptographically secure" doc at `:4` — quarantined)

**Symbol:** `ChaChaDrbg`, `EntropyPool`
**Why it matters:** SSH/TLS session keys and X25519 ephemerals all ride on `getrandom`; ChaCha20's pure-integer ARX core is SIMD-off-safe and host-testable, so the whole crypto stack can rest on a vetted, unit-tested DRBG instead of the legacy xorshift expansion.

**Acceptance:**
- [x] A `kernel-core` host test asserts ≥256 credited bits are required before the DRBG reports `READY`; `GRND_NONBLOCK`→`EAGAIN` iff `!ready`; `GRND_INSECURE` serves output pre-`READY`.
- [x] A `kernel-core` host test proves fast-key-erasure: a recovered post-draw state cannot reproduce prior output; 1 MiB of DRBG output passes monobit + chi-square.
- [x] The legacy xorshift `Prng` is grep-unreachable from any csprng path (it survives only on the `/dev/urandom` read path, which the design doc explicitly permits; `getrandom`/AT_RANDOM/ISN use only `kernel_core::csprng`).

> **Verified:** 16 csprng host tests pass under `cargo xtask check` (incl. the READY-gate, fast-key-erasure, and 1 MiB monobit+chi-square tests). The ChaCha20 core was independently validated against the RFC 7539 §2.3.2 test vector. A boot with `+rdseed` logs `[csprng] seeded source=rdseed credited_bits=256 state=READY`.

### A.2 — `rdseed64` + RDSEED probe + RDSEED→RDRAND→TSC fallback

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (modeled on `cpu_has_rdrand` `:5701`, `RDRAND_SUPPORT` `:5718`, `rdrand64` `:5724`)
**Symbol:** `rdseed64`, `cpu_has_rdseed`
**Why it matters:** There is no RDSEED in the tree today; RDSEED is full-entropy (`CPUID.07H:EBX[18]`) versus RDRAND's ≤128-bit CTR_DRBG output, and the 2025 AMD RDSEED bias (AMD-SB-7055) means whichever source is used must be conditioned through the DRBG.

**Acceptance:**
- [x] `rdseed64` probes `CPUID.07H:EBX[18]`, caches the result in an `AtomicU8`, PAUSE-retries on `CF=0`, and credits full-entropy only on `CF=1`.
- [x] The boot log emits the seed source (`rdseed` | `rdrand` | `degraded`) and the credited-bit count; the degraded path (neither RDSEED nor RDRAND) still boots with no deadlock.

> **Verified:** boot on `qemu64,+rdseed` → `source=rdseed credited_bits=256 state=READY`; boot on default `qemu64` (no RDSEED/RDRAND) → `source=degraded credited_bits=0 state=EARLY` and the smoke suite still reaches login (no deadlock).

### A.3 — Seed the DRBG early in `kernel_main_entry` + audit the unseeded window

**File:** `kernel/src/lib.rs` (`kernel_main_entry`; seed right after `mm::init`/`kstack::init`)
**Symbol:** `kernel_main_entry`
**Why it matters:** Stack canaries, `AT_RANDOM`, the TCP ISN, and DNS transaction IDs are all drawn before any current seed point, so the seed must move ahead of every consumer or each consumer must be explicitly accepted as degraded.

**Acceptance:**
- [x] The DRBG is `READY` synchronously after `mm::init` and before `init_task`, asserted by boot-log ordering.
- [x] An audit note enumerates the pre-seed consumers (stack canary, ASLR slide `kernel/src/mm/elf.rs`, TCP ISN `kernel/src/net/tcp.rs`, DNS txid) each as moved-after-seed or accepted-degraded.
- [x] If both RDSEED and RDRAND are absent (some hypervisors), a degraded-but-progressing `INSECURE` fallback contract avoids a boot deadlock (boot reaches the login prompt).

> **Verified:** boot serial shows `[csprng] seeded …` (line ~45) immediately after the banner and before `[init] loading /sbin/init` (line ~161). The pre-seed audit lives in the `kernel_main_entry` comment block. Degraded boot (default `qemu64`) reaches login and passes 22 smoke steps.

### A.4 — Rewrite `sys_getrandom`: honor flags, drop the 256-byte cap, source the DRBG

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_getrandom`; `GETRANDOM` const = 318 at `:1418`)
**Symbol:** `sys_getrandom`
**Why it matters:** Today `_flags` is dropped, the request is capped at 256 bytes, and a fresh xorshift seed is drawn per call; the userspace wrapper loops, but `sshd`'s backend (`userspace/sshd/src/getrandom_impl.rs:5`) requires `ret == len` in a single call, so ≤256-byte atomicity must be preserved.

**Acceptance:**
- [x] `GRND_NONBLOCK`→`EAGAIN` iff `!ready`; `GRND_INSECURE` serves pre-`READY`; `GRND_RANDOM` honored; a bad flag combo returns `EINVAL`.
- [x] Every ≤256-byte call returns the exact requested length in one call (atomicity preserved for `sshd`); a >256-byte call succeeds (cap removed); reseed occurs at a 60-second-or-output-ceiling bound.

> **Verified:** the rewrite validates flags (unknown bits → `EINVAL`, `GRND_INSECURE|GRND_RANDOM` → `EINVAL`), the chunked `getrandom_fill_user` always returns the full requested length (no short count, no cap), and `maybe_reseed_csprng` enforces the 1 MiB-or-60 s bound. The independent review confirmed the atomicity contract and that no path holds the DRBG lock re-entrantly. `tls-smoke` (PT_TLS) and `dns-smoke` (musl resolver) both PASS over the rewritten path.

### A.5 — Fill `AT_RANDOM` + randomize the TCP ISN from the CSPRNG

**Files:**
- `kernel/src/mm/elf.rs` (AT_RANDOM fill, previously deterministic `(0xAB ^ i).wrapping_add(i)`)
- `kernel/src/net/tcp.rs` (ISN, previously `snd_nxt = tick_count()`)

**Symbol:** the `AT_RANDOM` seed write; `TcpConn` ISN
**Why it matters:** A deterministic `AT_RANDOM` gives every binary identical stack canaries / ASLR, and a `tick_count()`-derived ISN is hijackable; both are the live downstream consumers that prove the CSPRNG is actually wired in.

**Acceptance:**
- [x] `AT_RANDOM` is 16 live CSPRNG bytes per load (with `fill_insecure` fallback on a degraded boot so exec never fails); two processes observe different stack canaries.
- [x] The TCP ISN mixes CSPRNG output per RFC 6528 (a one-way **SipHash-2-4** keyed PRF over the 4-tuple + a 128-bit per-boot CSPRNG secret) at both the active `connect` and the passive `Listen` site; two connections get non-sequential, non-`tick_count` ISNs.

> **Verified:** AT_RANDOM draws from `global_fill`/`global_fill_insecure`. The ISN PRF was hardened from an additive mix to SipHash-2-4 (round-1 revision) — the SipHash implementation is host-tested against the official SipHash-2-4 test vectors (`siphash24(0x0706…00, 0x0f0e…08, "") == 0x726fdb47dd0e0e31`, etc.). So one observed ISN no longer leaks the per-boot secret.

---

## Track B — Wall-clock trust

### B.1 — Build-date-floor fallback for `BOOT_EPOCH` + fail-closed contract

**Files:** `kernel/build.rs` (emits `M3OS_BUILD_EPOCH`), `kernel/src/rtc.rs` (`init_rtc`, `BOOT_EPOCH_SECS`), `kernel-core/src/time.rs` (pure `apply_clock_floor` helper)
**Symbol:** `init_rtc`, `BOOT_EPOCH_SECS`, `apply_clock_floor`
**Why it matters:** `init_rtc` leaves `BOOT_EPOCH_SECS = 0` on a bad RTC → epoch 1970 → every certificate `notBefore`-in-future → `MBEDTLS_X509_BADCERT_FUTURE`; on dead-CMOS metal this looks like a network/TLS bug and is a hidden blocker for 86c.

**Acceptance:**
- [x] An invalid (or behind-floor) RTC sets `BOOT_EPOCH_SECS` to a build-date floor (not `0`), logged; `tsc_now_us` / `sys_clock_gettime` (which read `BOOT_EPOCH_SECS`) never return 1970.
- [x] A forced-early-RTC smoke confirms `CLOCK_REALTIME` ≥ the floor; the first-boot insecure-skip-time decision is recorded in the log.

> **Verified:** floor logic is host-tested in `kernel-core::time::apply_clock_floor` (invalid → floor; early < floor → floor; recent ≥ floor → unchanged; boundary). A boot with `-rtc base=2000-01-01` logs `[WARN] [rtc] clock floor applied: BOOT_EPOCH_SECS=1780800303 (RTC invalid or behind build-date floor); certificate time checks may be skipped on first boot` — clamped to the build epoch (~2026-06-07), **not 1970**. The `M3OS_BUILD_EPOCH` is emitted by `build.rs` (honoring `SOURCE_DATE_EPOCH`) and consumed via `option_env!` in `rtc.rs`, with a `2026-06-01` fallback.

---

## Track C — DNS config + CA-trust paths

### C.1 — Validate `resolv.conf` + `/etc/hosts` path; scope AAAA out

**Files:** `xtask/src/main.rs` (`populate_ext2_files` — `/etc/hosts` staging + existing `/etc/resolv.conf`), `userspace/dns-smoke/dns-smoke.c` (`getaddrinfo("github.com", AF_INET, …)`)
**Symbol:** `populate_ext2_files` resolv.conf/hosts staging; the `getaddrinfo` path
**Why it matters:** `AF_INET6` is unrecognized in `sys_socket` and stock musl emits AAAA queries unless scoped; the stock musl resolver is UDP-only with no TCP fallback / EDNS0, so validating the IPv4-only path with documented limits prevents a silent dependency for 86b/86c.

**Acceptance:**
- [x] `getaddrinfo("github.com", AF_INET)` resolves an A record over `sys_recvmsg_inet` and `dns-smoke` reports **PASS** (not SKIP), traced to `open(/etc/resolv.conf)` + `/etc/hosts`-checked-first.
- [x] A DEFERRED note in the design doc documents the resolver limits: UDP-only, no EDNS0/DNSSEC, AAAA stubbed (IPv6 → Phase 89), no caching, `/etc/hosts` checked first.

> **Verified:** `/etc/hosts` is now staged into the ext2 data disk (`127.0.0.1 localhost` + `::1 localhost`) mirroring the resolv.conf staging. `smoke-test` run with `M3OS_DNS_REGRESSION=1` (which **requires** PASS, fails on SKIP) → step 14 matched `SMOKE:dns-smoke:PASS`. The resolver-limits DEFERRED note is in `docs/roadmap/86a-outbound-foundation.md`.

### C.2 — SHA-256-pinned `ca-certificates` `.m3pkg` + canonical trust paths

**Files:** `ports/lib/ca-certificates/Portfile` (new), `xtask/src/port_build.rs` (`build_ca_certificates` + dispatch), `xtask/src/main.rs` (`BUNDLE_ONLY_PORTS`)
**Symbol:** the `ca-certificates` Portfile
**Why it matters:** mbedTLS has no default trust store and an unverified bundle defeats trust; a refreshable SHA-256-pinned bundle (~190 KB, curl `cacert.pem`) staged to one canonical path is the trust root 86c's `curl`/`git` will validate against.

**Acceptance:**
- [x] `ca-certificates` stages `cacert.pem` to exactly **one** canonical path `/etc/ssl/certs/ca-certificates.crt`, pinned by SHA-256 in the Portfile, that 86c's `curl --with-ca-bundle` agrees with; it is registered as bundle-only (no compiler invocation).
- [x] This doc fixes the trust/credential paths: CA = `/etc/ssl/certs/ca-certificates.crt`, `known_hosts` = `~/.ssh/known_hosts` (+ `/etc/ssh` seed), credentials = `~/.git-credentials` + `~/.netrc`.

> **Verified:** `cargo xtask port build ca-certificates` downloads cacert.pem, prints `verified cacert.pem (sha 86a1f3366afac7c6)` (the Portfile pin; the build aborts on mismatch), stages to `etc/ssl/certs/ca-certificates.crt`, and seals a valid `.m3pkg` (a re-run is a pure pkgcache hit). Image builds log `ports: bundled ca-certificates.m3pkg (bundle-only) into /usr/pkg`. Trust/credential conventions recorded in the design doc.

---

## Track D — Version

### D.1 — Bump kernel crate `0.85.3` → `0.86.0`

**File:** `kernel/Cargo.toml` (`[package] version`)
**Symbol:** `[package] version = "0.86.0"`
**Why it matters:** 86a is the first sub-phase of the Phase 86 umbrella and lands the umbrella's opening kernel-version patch bump (`0.86.0` → `0.86.5` across 86a–86f).

**Acceptance:**
- [x] `kernel/Cargo.toml` reads `version = "0.86.0"` (+ `Cargo.lock` updated).
- [x] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + all host tests incl. the new `kernel-core` csprng + time-floor tests + the retpoline gate).
- [x] The boot banner / `uname` report `0.86.0` (`env!("CARGO_PKG_VERSION")`).

> **Verified:** `cargo xtask check` → "check passed: clippy clean, formatting correct, … host tests pass; retpoline indirect-branch gate pass"; build log shows `Compiling kernel v0.86.0`; boot serial shows `[m3os] Hello from kernel! v0.86.0`.

---

## Documentation Notes

- **No transport here.** SSH (86b) and HTTPS/TLS (86c) both depend on this foundation; 86a deliberately lands only the CSPRNG, the wall-clock floor, the resolver validation, and the CA bundle.
- **The RNG upgrade does not rotate already-persisted weak secrets.** The `sshd` Ed25519 host key (`/etc/ssh/ssh_host_ed25519_key`) and `passwd`/`shadow` salts were generated under the weak PRNG; the disclaimers in `userspace/crypto-lib/src/random.rs` and `kernel-core/src/prng.rs` were updated to call out the one-time manual rotation step.
- **Entropy atomicity is a hard contract.** `sshd`'s `getrandom` backend does not loop and requires `ret == len`; the ≤256-byte single-call atomicity in A.4 is preserved even though the cap is removed.
- **`/dev/urandom` still serves the legacy xorshift `Prng`** (spec-permitted) — userspace crypto that reads `/dev/urandom` directly rather than `getrandom` does not yet benefit from the DRBG. Tracked as a follow-up.
- **IPv6/AAAA is explicitly out** (Phase 89). The resolver stays IPv4-only, UDP-only, no caching, `/etc/hosts`-first.
- **One canonical CA path.** Match Debian's `/etc/ssl/certs/ca-certificates.crt`; every later consumer (86c `curl`/`git`) must agree on it. Track CA-bundle provenance/staleness as refreshable data.
