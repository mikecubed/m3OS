# Phase 86a — Outbound Foundation (CSPRNG, Wall-clock, Resolver, CA Trust): Task List

**Status:** In Progress (`feat/phase-86a-outbound-foundation`)
**Source Ref:** phase-86a
**Depends on:** Phase 48 (Security Foundation) ✅, Phase 77 (Pre-1.0 Cleanup — DNS reply delivery D.1 + outbound TCP `connect` D.2) ✅, Phase 85a (Package & Build-Cache Infrastructure) ✅
**Goal:** Land the entropy/time/trust foundation that all of Phase 86's transports (86b SSH, 86c HTTPS, 86e `gh`) silently depend on — a ChaCha20 DRBG `getrandom` (RDSEED→RDRAND→TSC seeded, flags honored, ≤256-byte atomicity preserved, the 256-byte cap removed), a fail-closed wall-clock floor so certificates can be validated, a validated IPv4/A-record resolver path, and the on-disk CA/`known_hosts`/credential conventions plus a SHA-256-pinned `ca-certificates` `.m3pkg` — with **no transport** landed here. Kernel `0.86.0`.

> **Authored ahead of implementation.** Every acceptance item below is intentionally unchecked `[ ]`; it records the planned, measurable result, not a delivered one.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Kernel CSPRNG (ChaCha20 DRBG, RDSEED seeding, early seed, `getrandom` rewrite, downstream consumers) | — | Planned |
| B | Wall-clock trust (build-date floor + fail-closed contract) | — | Planned |
| C | DNS config validation + CA-trust paths (`ca-certificates` `.m3pkg`, canonical trust/credential paths) | — | Planned |
| D | Version bump `0.85.3` → `0.86.0` | A, B, C | Planned |

---

## Track A — Kernel CSPRNG

### A.1 — ChaCha20 DRBG + entropy pool in `kernel-core`

**Files:**
- `kernel-core/src/csprng.rs` (new)
- `kernel-core/src/prng.rs` (legacy `Prng::new` at `:16`, "NOT cryptographically secure" doc at `:4` — quarantined)

**Symbol:** `ChaChaDrbg`, `EntropyPool`
**Why it matters:** SSH/TLS session keys and X25519 ephemerals all ride on `getrandom`; ChaCha20's pure-integer ARX core is SIMD-off-safe and host-testable, so the whole crypto stack can rest on a vetted, unit-tested DRBG instead of the legacy xorshift expansion.

**Acceptance:**
- [ ] A `kernel-core` host test asserts ≥256 credited bits are required before the DRBG reports `READY`; `GRND_NONBLOCK`→`EAGAIN` iff `!ready`; `GRND_INSECURE` serves output pre-`READY`.
- [ ] A `kernel-core` host test proves fast-key-erasure: a recovered post-draw state cannot reproduce prior output; 1 MiB of DRBG output passes monobit + chi-square.
- [ ] The legacy xorshift `Prng` is grep-unreachable from any csprng path (`grep` confirms no `prng::Prng` reference under the `getrandom`/csprng call graph).

### A.2 — `rdseed64` + RDSEED probe + RDSEED→RDRAND→TSC fallback

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (modeled on `cpu_has_rdrand` `:5701`, `RDRAND_SUPPORT` `:5718`, `rdrand64` `:5724`)
**Symbol:** `rdseed64`, `cpu_has_rdseed`
**Why it matters:** There is no RDSEED in the tree today; RDSEED is full-entropy (`CPUID.07H:EBX[18]`) versus RDRAND's ≤128-bit CTR_DRBG output, and the 2025 AMD RDSEED bias (AMD-SB-7055) means whichever source is used must be conditioned through the DRBG.

**Acceptance:**
- [ ] `rdseed64` probes `CPUID.07H:EBX[18]`, caches the result in an `AtomicU8`, PAUSE-retries on `CF=0`, and credits full-entropy only on `CF=1`.
- [ ] The boot log emits the seed source (`rdseed` | `rdrand` | `degraded`) and the credited-bit count; the degraded path (neither RDSEED nor RDRAND) still boots with no deadlock.

### A.3 — Seed the DRBG early in `kernel_main_entry` + audit the unseeded window

**File:** `kernel/src/lib.rs` (`kernel_main_entry` at `:71`; seed right after `mm::init`)
**Symbol:** `kernel_main_entry`
**Why it matters:** Stack canaries, `AT_RANDOM`, the TCP ISN, and DNS transaction IDs are all drawn before any current seed point, so the seed must move ahead of every consumer or each consumer must be explicitly accepted as degraded.

**Acceptance:**
- [ ] The DRBG is `READY` synchronously after `mm::init` and before `init_task` (`kernel/src/lib.rs:373`), asserted by boot-log ordering.
- [ ] An audit note enumerates the pre-seed consumers (stack canary, ASLR slide `kernel/src/mm/elf.rs`, TCP ISN `kernel/src/net/tcp.rs:250`, DNS txid) each as moved-after-seed or accepted-degraded.
- [ ] If both RDSEED and RDRAND are absent (some hypervisors), a degraded-but-progressing `INSECURE` fallback contract avoids a boot deadlock (boot reaches the login prompt).

### A.4 — Rewrite `sys_getrandom`: honor flags, drop the 256-byte cap, source the DRBG

**File:** `kernel/src/arch/x86_64/syscall/mod.rs` (`sys_getrandom` at `:15075`; old path `seed_pseudorandom_state` `:5759`, `fill_pseudorandom_bytes` `:5770`, `copy_pseudorandom_to_user` `:5795`; `GETRANDOM` const = 318 at `:1418`)
**Symbol:** `sys_getrandom`
**Why it matters:** Today `_flags` is dropped, the request is capped at 256 bytes, and a fresh xorshift seed is drawn per call; the userspace wrapper (`userspace/syscall-lib/src/lib.rs:3231`) loops, but `sshd`'s backend (`userspace/sshd/src/getrandom_impl.rs:5`) requires `ret == len` in a single call, so ≤256-byte atomicity must be preserved.

**Acceptance:**
- [ ] `GRND_NONBLOCK`→`EAGAIN` iff `!ready`; `GRND_INSECURE` serves pre-`READY`; `GRND_RANDOM` honored; a bad flag combo returns `EINVAL`.
- [ ] Every ≤256-byte call returns the exact requested length in one call (atomicity preserved for `sshd`); a >256-byte call succeeds (cap removed); reseed occurs at a 60-second-or-output-ceiling bound.

### A.5 — Fill `AT_RANDOM` + randomize the TCP ISN from the CSPRNG

**Files:**
- `kernel/src/mm/elf.rs` (AT_RANDOM fill at `:666`, currently deterministic `(0xAB ^ i).wrapping_add(i)`; `AT_RANDOM` = 25)
- `kernel/src/net/tcp.rs` (ISN at `:250`, currently `snd_nxt = tick_count()`)

**Symbol:** the `AT_RANDOM` seed write; `TcpConn` ISN
**Why it matters:** A deterministic `AT_RANDOM` gives every binary identical stack canaries / ASLR, and a `tick_count()`-derived ISN is hijackable; both are the live downstream consumers that prove the CSPRNG is actually wired in.

**Acceptance:**
- [ ] `AT_RANDOM` is 16 live CSPRNG bytes per load; two processes observe different stack canaries.
- [ ] The TCP ISN mixes CSPRNG output per RFC 6528; two connections get non-sequential, non-`tick_count` ISNs.

---

## Track B — Wall-clock trust

### B.1 — Build-date-floor fallback for `BOOT_EPOCH` + fail-closed contract

**File:** `kernel/src/rtc.rs` (`init_rtc` at `:198`; `BOOT_EPOCH_SECS` at `:10`; consumers `tsc_now_us` at `kernel/src/arch/x86_64/syscall/mod.rs:15145`, `sys_clock_gettime` at `:15199`)
**Symbol:** `init_rtc`, `BOOT_EPOCH_SECS`
**Why it matters:** `init_rtc` leaves `BOOT_EPOCH_SECS = 0` on a bad RTC → epoch 1970 → every certificate `notBefore`-in-future → `MBEDTLS_X509_BADCERT_FUTURE`; on dead-CMOS metal this looks like a network/TLS bug and is a hidden blocker for 86c.

**Acceptance:**
- [ ] An invalid RTC sets `BOOT_EPOCH_SECS` to a build-date floor (not `0`), logged; `tsc_now_us` / `sys_clock_gettime` never return 1970.
- [ ] A forced-bad-RTC smoke confirms `CLOCK_REALTIME` ≥ the floor; the first-boot insecure-skip-time decision is recorded in the log.

---

## Track C — DNS config + CA-trust paths

### C.1 — Validate `resolv.conf` + `/etc/hosts` path; scope AAAA out

**Files:**
- `xtask/src/main.rs` (`populate_ext2_files` at `:15586`; `resolv_conf_content = "nameserver 10.0.2.3\noptions timeout:5 attempts:3"` at `:15623`)
- `userspace/dns-smoke/dns-smoke.c` (`getaddrinfo("github.com", …)` at `:26`)

**Symbol:** `populate_ext2_files` resolv.conf staging; the `getaddrinfo` path
**Why it matters:** `AF_INET6` is unrecognized in `sys_socket` and stock musl emits AAAA queries unless scoped; the stock musl resolver is UDP-only with no TCP fallback / EDNS0, so many-A/CNAME responses can flake — validating the IPv4-only path with documented limits prevents a silent dependency for 86b/86c.

**Acceptance:**
- [ ] `getaddrinfo("github.com", AF_INET)` resolves an A record over `sys_recvmsg_inet` and `dns-smoke` reports **PASS** (not SKIP), traced to `open(/etc/resolv.conf)` + `/etc/hosts`-checked-first.
- [ ] A DEFERRED note in the design doc documents the resolver limits: UDP-only, no EDNS0/DNSSEC, AAAA stubbed (IPv6 → Phase 89), no caching, `/etc/hosts` checked first.

### C.2 — SHA-256-pinned `ca-certificates` `.m3pkg` + canonical trust paths

**Files:**
- `ports/lib/ca-certificates/Portfile` (new)
- `xtask/src/main.rs` (`PORTS` registry at `:17446`, `BUNDLE_ONLY_PORTS` at `:17541`, `populate_phase_69d_ports` at `:17445`)

**Symbol:** the `ca-certificates` Portfile
**Why it matters:** mbedTLS has no default trust store and an unverified bundle defeats trust; a refreshable SHA-256-pinned bundle (~200 KB, ~121 Mozilla roots, curl `cacert.pem`) staged to one canonical path is the trust root 86c's `curl`/`git` will validate against.

**Acceptance:**
- [ ] `ca-certificates` stages `cacert.pem` to exactly **one** canonical path `/etc/ssl/certs/ca-certificates.crt`, pinned by SHA-256 in the Portfile, that 86c's `curl --with-ca-bundle` agrees with; it is registered as bundle-only (no compiler invocation).
- [ ] This doc fixes the trust/credential paths: CA = `/etc/ssl/certs/ca-certificates.crt`, `known_hosts` = `~/.ssh/known_hosts` (+ `/etc/ssh` seed), credentials = `~/.git-credentials` + `~/.netrc`.

---

## Track D — Version

### D.1 — Bump kernel crate `0.85.3` → `0.86.0`

**File:** `kernel/Cargo.toml` (`[package] version` at line 3, currently `0.85.3`)
**Symbol:** `[package] version = "0.86.0"`
**Why it matters:** 86a is the first sub-phase of the Phase 86 umbrella and lands the umbrella's opening kernel-version patch bump (`0.86.0` → `0.86.5` across 86a–86f).

**Acceptance:**
- [ ] `kernel/Cargo.toml` line 3 reads `version = "0.86.0"` (+ `Cargo.lock` updated).
- [ ] `cargo xtask check` is clean (clippy `-D warnings` + rustfmt + all host tests incl. the new `kernel-core` csprng tests + the retpoline gate).
- [ ] The boot banner / `uname` report `0.86.0` (`env!("CARGO_PKG_VERSION")`).

---

## Documentation Notes

- **No transport here.** SSH (86b, [`86b-ssh-git-transport-tasks.md`](./86b-ssh-git-transport-tasks.md)) and HTTPS/TLS (86c, [`86c-https-git-transport-tasks.md`](./86c-https-git-transport-tasks.md)) both depend on this foundation; 86a deliberately lands only the CSPRNG, the wall-clock floor, the resolver validation, and the CA bundle.
- **The RNG upgrade does not rotate already-persisted weak secrets.** The `sshd` Ed25519 host key (`userspace/sshd/src/host_key.rs:43`, `/etc/ssh/ssh_host_ed25519_key`) and `passwd`/`shadow` salts were generated under the weak PRNG; call out a one-time manual rotation step. Update the disclaimers in `userspace/crypto-lib/src/random.rs` and `kernel-core/src/prng.rs:4` once the CSPRNG lands.
- **Entropy atomicity is a hard contract.** `sshd`'s `getrandom` backend (`userspace/sshd/src/getrandom_impl.rs`) does not loop and requires `ret == len`; the ≤256-byte single-call atomicity in A.4 must be preserved even though the cap is removed.
- **IPv6/AAAA is explicitly out** (Phase 89). The resolver stays IPv4-only, UDP-only, no caching, `/etc/hosts`-first.
- **One canonical CA path.** Match Debian's `/etc/ssl/certs/ca-certificates.crt`; every later consumer (86c `curl`/`git`) must agree on it. Track CA-bundle provenance/staleness as refreshable data, not a frozen artifact.
- **Prefer exact symbols.** Reference `ChaChaDrbg`, `rdseed64`, `sys_getrandom`, `init_rtc`/`BOOT_EPOCH_SECS`, and the AT_RANDOM/TCP-ISN sites — not "the randomness code."
