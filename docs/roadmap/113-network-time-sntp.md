# Phase 113 - Network Time Synchronization (SNTP)

**Status:** Planned
**Source Ref:** phase-113
**Depends on:** Phase 34 (Real-Time Clock) ✅, Phase 16 (Network) ✅, Phase 23 (Socket API) ✅, Phase 46 (System Services / daemon model) ✅, Phase 86a (build-date wall-clock floor + CSPRNG) ✅
**Builds on:** The read-once-at-boot RTC epoch (`kernel/src/rtc.rs`) and the TSC-advanced `CLOCK_REALTIME` (`tsc_now_us`), the UDP socket surface (`syscall-lib` `socket`/`sendto`/`recvfrom`), and the `init` service-manager daemon model. Adds the **first writable path to the wall clock** the OS has ever had.
**Primary Components:** `kernel/src/rtc.rs`, `kernel/src/arch/x86_64/syscall/mod.rs`, `userspace/syscall-lib`, new `userspace/sntpd`, `userspace/init`

## Milestone Goal

The system clock corrects itself over the network. A minimal SNTP (RFC 4330 / RFC 5905
client subset) daemon queries an NTP server, computes the offset, and steps the wall clock —
so a machine whose CMOS is wrong, or which has drifted, converges to real time without a
human setting it. This closes a quietly load-bearing gap: TLS certificate validation and
`cron` scheduling both depend on a correct clock, and today the clock can only ever be as
good as the CMOS reading taken once at boot.

## Why This Phase Exists

m3OS can **read** the wall clock but has **no way to correct it**:

- `BOOT_EPOCH_SECS: AtomicU64` (`kernel/src/rtc.rs:10`) is written at exactly **one** site —
  `init_rtc()` at `rtc.rs:237`, once, at boot — after applying a build-date floor
  (`build_epoch_floor`, `rtc.rs:209`; `BUILD_EPOCH_FALLBACK`, `rtc.rs:199`). Thereafter
  `CLOCK_REALTIME` is `BOOT_EPOCH_SECS + TSC_delta` (`tsc_now_us`,
  `kernel/src/arch/x86_64/syscall/mod.rs:20541`).
- Userspace can read the clock (`GETTIMEOFDAY = 96`, `CLOCK_GETTIME = 228`; handlers
  `sys_gettimeofday`/`sys_clock_gettime` at `mod.rs:20565`/`:20595`) but there is **no**
  `settimeofday`, `clock_settime`, `adjtime`, or `adjtimex` anywhere in the tree — confirmed
  by exhaustive search. The only `*settime*` symbol is the unrelated `timerfd_settime`.
  `rebase_boot_tsc` (`kernel/src/arch/x86_64/apic.rs:395`) shifts only the **monotonic**
  origin (for S3 resume) and cannot correct wall time.
- The consequences already bite: `crond` reads `clock_gettime(CLOCK_REALTIME)`
  (`userspace/crond/src/main.rs:611`) to decide which jobs fire, and TLS verification (Phase
  86c) rejects certs when the clock is implausible — both silently degrade when the CMOS is
  wrong. `init`'s `KNOWN_CONFIGS` even carries an **orphan `ntpd.conf` entry**
  (`userspace/init/src/main.rs:205`) with no binary behind it — the slot was reserved and
  never filled.

So the phase must add two things that genuinely do not exist: a **privilege-gated clock-set
syscall** (and the `BOOT_EPOCH_SECS` writer behind it), and the **`sntpd` client** that uses
it. This is a small, self-contained subsystem — but it touches ring 0, so it is its own
phase rather than a userspace polish item.

## Learning Goals

- The SNTP/NTP packet format (RFC 4330): the 48-byte header, the four timestamps
  (originate/receive/transmit) in NTP 64-bit fixed-point seconds-since-1900, and the
  offset/round-trip-delay computation.
- The NTP↔Unix epoch conversion (the 2,208,988,800-second 1900→1970 constant) and why NTP's
  era rollover (2036) matters.
- **Step vs. slew** clock correction — why a first sync steps (jumps) the clock and a
  disciplined daemon would slew (rate-adjust) small offsets, and why m3OS starts with step.
- Why setting the clock is a **privileged** operation (a wrong clock breaks TLS, cron, file
  timestamps, and audit ordering) and must be capability/root-gated at the syscall boundary.
- The `no_std` userspace networking constraint: `syscall-lib` has **no** `getaddrinfo` (the
  resolver is musl-C-only), so a native Rust daemon speaks to a **numeric** server address —
  the tradeoffs of a hardcoded/config IP vs. linking musl for DNS.

## Feature Scope

### Track A — Writable wall clock: `settimeofday`-class syscall

Add the missing kernel surface:

- A `set_wall_epoch(secs, nsec)` writer in `kernel/src/rtc.rs` that stores a new base into
  `BOOT_EPOCH_SECS` **and** re-anchors the TSC origin so `tsc_now_us` stays continuous across
  the step (i.e. record the TSC at the moment of the set, so post-set reads = new_base +
  TSC_since_set). No CMOS write-back in this cut (the RTC hardware stays read-only; the
  correction lives in the kernel epoch).
- A new syscall — `sys_clock_settime(clk_id, tp_ptr)` (Linux number `227`) and/or
  `sys_settimeofday` — that validates the caller is privileged (root/uid 0 or a dedicated
  capability), reads the userspace `timespec`, sanity-clamps against the build-date floor
  (never accept a time *before* the image was built — the Phase 86a anti-rollback invariant),
  and calls the writer. Reject `CLOCK_MONOTONIC` targets with `EINVAL`.
- `syscall-lib` wrappers `clock_settime`/`settimeofday`.

### Track B — `sntpd` daemon + SNTP protocol

A minimal, `no_std` SNTP client daemon:

- Encode a mode-3 (client) SNTP request, `sendto` it over `socket(AF_INET, SOCK_DGRAM)` to
  the configured server, `recvfrom` the mode-4 reply (with a bounded timeout + retry), and
  compute the offset per RFC 4330: `offset = ((T2 − T1) + (T3 − T4)) / 2`.
- Sanity-gate the reply (stratum in `1..=15`, non-zero transmit timestamp, LI ≠ alarm,
  round-trip delay under a ceiling) before stepping; then `clock_settime(CLOCK_REALTIME, …)`.
- Run once at boot (oneshot) then periodically (a long interval, e.g. 1 h) — either as a
  restart-`never` oneshot re-armed by `crond`, or a `type=daemon` sleeper loop. Server
  address + poll interval come from `/etc/sntpd.conf` (a numeric IP by default —
  `pool.ntp.org` needs the musl resolver, which a `no_std` daemon lacks; see Deferred).
- Log each sync (server, offset applied, new time) via the existing syslog path.

### Track C — Wiring + gate

- Register `sntpd` through the four userspace-binary points (Cargo member, `bins[]`, ramdisk
  `BIN_ENTRIES`, service `.conf`) — and **fill the orphan `ntpd.conf`/`sntpd.conf` slot** in
  `init`'s `KNOWN_CONFIGS`.
- A CI smoke that stands up a **local UDP SNTP responder** (in the test harness, on
  loopback/SLIRP) returning a known transmit timestamp, boots with the clock deliberately
  wrong, runs `sntpd`, and asserts `clock_gettime(CLOCK_REALTIME)` converged to the injected
  time (± a small delta). An opt-in live arm points at a real public NTP server.

## Important Components and How They Work

### The clock-set writer and TSC re-anchor (Track A)

Today `tsc_now_us` computes `sec = BOOT_EPOCH_SECS + elapsed_tsc / tsc_per_ms`
(`mod.rs:20541`), where `elapsed_tsc` is measured from `boot_tsc()` (`apic.rs:384`). A naive
`BOOT_EPOCH_SECS.store(new)` would double-count the elapsed TSC. The writer therefore records
the current TSC as a new "epoch-anchor TSC" alongside the new base, and `tsc_now_us` computes
the delta from that anchor — so the clock steps exactly to the requested value and continues
monotonically from there. This is the wall-clock analog of the `rebase_boot_tsc` monotonic
re-anchor already used on S3 resume, kept deliberately separate so a wall-clock step never
perturbs `CLOCK_MONOTONIC`.

### The privilege gate and anti-rollback clamp (Track A)

`sys_clock_settime` is the first syscall that can move the wall clock, so it fails closed:
non-root callers get `EPERM`; a requested time earlier than the build-date floor
(`build_epoch_floor`, `rtc.rs:209`) is rejected, preserving the Phase 86a invariant that the
clock never rolls behind the image build (which would let an attacker revive expired certs).
The `sntpd` daemon runs as root (or holds the dedicated capability) so it — and effectively
only it, plus a root `date -s` — can step time.

### The `no_std` SNTP client and the DNS constraint (Track B)

`syscall-lib` exposes `socket`/`bind`/`connect`/`sendto`/`recvfrom` (`lib.rs:2365`–`2454`)
and `SockaddrIn` (`lib.rs:1544`) — everything a UDP client needs — but **no** `getaddrinfo`
(there is no native Rust resolver; DNS is musl-C only). So `sntpd` targets a **numeric** NTP
server IP from config (mirroring `udp-smoke`'s literal `REMOTE_IP`). Resolving a hostname
like `pool.ntp.org` would require either linking musl (as the `dns-smoke` C binary does) or a
small native resolver — both deferred. The daemon's shape follows `syslogd`
(`userspace/syslogd/src/main.rs`): create a socket, then an outer loop of request → bounded
`recvfrom` → apply/sleep.

## How This Builds on Earlier Phases

- **Extends Phase 34** by adding the write path the RTC never had — the clock becomes
  correctable, not just readable.
- **Reuses the Phase 86a** build-date wall-clock floor as the anti-rollback clamp on the new
  set path, and the CSPRNG for any query nonce/jitter.
- **Reuses the Phase 16/23** UDP socket surface unchanged (`sntpd` is an ordinary client).
- **Reuses the Phase 46** service-manager daemon model and finally backs the orphaned
  `ntpd.conf` slot in `init`'s `KNOWN_CONFIGS`.
- **Directly benefits Phase 86c** (TLS cert validity) and `crond` (`clock_gettime`-driven
  scheduling), which silently degrade on a wrong clock today.

## Implementation Outline

1. **Track A:** add `set_wall_epoch` + TSC re-anchor to `rtc.rs`; add `sys_clock_settime`
   (root-gated, floor-clamped) + dispatch arm + `syscall-lib` wrappers; host-test the
   NTP↔Unix and offset math in `kernel-core`.
2. **Track B:** write `userspace/sntpd` — SNTP packet codec (host-tested), UDP query/reply
   loop, sanity gates, `clock_settime` step, syslog line, config parse.
3. **Track C:** wire the four registration points + `KNOWN_CONFIGS` slot; add the local-responder
   CI smoke (`M3OS_SNTP_REGRESSION`) + opt-in live arm; document the gate.

## Acceptance Criteria

- **Track A:** host tests cover NTP 64-bit ↔ Unix conversion and the offset/delay formula; a
  QEMU smoke sets the clock via the new syscall from root and reads it back changed, and
  confirms a non-root `clock_settime` returns `EPERM` and a below-floor time is rejected.
- **Track B/C:** with a harness UDP responder returning a known transmit timestamp and the
  guest clock deliberately skewed, `sntpd` steps `CLOCK_REALTIME` to within a small delta of
  the injected time; the sync is logged; `CLOCK_MONOTONIC` is unperturbed across the step.
  Gate `sntp-smoke` (`M3OS_SNTP_REGRESSION=1`); an opt-in `M3OS_SNTP_LIVE` arm hits a real
  public NTP server (skip-with-reason in CI, mirroring `git-https-smoke`'s live arm).
- The clock-set syscall is **root/capability-gated**; the production posture is documented
  next to the Phase 86a anti-rollback floor.

## Companion Task List

- [Phase 113 Task List](./tasks/113-network-time-sntp-tasks.md)

## How Real OS Implementations Differ

- **`ntpd`/`chrony`** *discipline* the clock: they slew small offsets (adjust the tick rate
  via `adjtimex`) instead of stepping, track multiple servers with a selection/clustering
  algorithm, estimate drift, and persist a drift file. m3OS starts with **single-server
  step** only — correct, but jumpy, and with no drift modeling.
- Production daemons resolve `pool.ntp.org` via DNS and rotate servers; m3OS uses a numeric
  server IP because the native `no_std` client has no resolver.
- Linux gates time-set with `CAP_SYS_TIME` and supports `clock_settime`/`adjtimex` with
  leap-second handling and `CLOCK_TAI`; m3OS ships a root-gated `clock_settime` step with a
  build-date floor and no leap-second/TAI support.
- Real systems also **write back** the corrected time to the CMOS RTC (`hwclock --systohc`);
  m3OS keeps the RTC hardware read-only and holds the correction in the kernel epoch only, so
  it is lost across a power cycle (re-synced on next boot).
- NTP proper authenticates (symmetric key / NTS); the SNTP subset here is unauthenticated.

## Deferred Until Later

- Clock **discipline** (slew/`adjtime`, drift estimation, a `chrony`-class algorithm) and
  multi-server selection.
- DNS resolution of NTP pool hostnames (needs a native resolver or a musl link).
- CMOS RTC write-back (`hwclock --systohc` analog) so the correction survives a power cycle.
- NTP authentication (symmetric key / NTS) and leap-second / `CLOCK_TAI` handling.
- A `date -s` / `timedatectl`-style admin CLI for manual set (the syscall makes it trivial).
