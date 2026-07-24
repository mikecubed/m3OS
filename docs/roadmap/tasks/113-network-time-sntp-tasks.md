# Phase 113 — Network Time Synchronization (SNTP): Task List

**Status:** Planned
**Source Ref:** phase-113
**Depends on:** Phase 34 (Real-Time Clock) ✅, Phase 16 (Network) ✅, Phase 23 (Socket API) ✅, Phase 46 (System Services) ✅, Phase 86a (build-date wall-clock floor) ✅
**Goal:** Add the first writable path to the wall clock — a root-gated `settimeofday`-class syscall + a `BOOT_EPOCH_SECS` writer with a TSC re-anchor and a build-date anti-rollback clamp — then ship a minimal `no_std` `sntpd` that queries an NTP server over UDP and steps `CLOCK_REALTIME`, wired through init's service model (filling the orphan `ntpd.conf` slot).

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Writable wall clock: `clock_settime`/`settimeofday` syscall + epoch writer | — | Planned |
| B | `sntpd` daemon + SNTP packet codec | A | Planned |
| C | Daemon wiring (4 points + `KNOWN_CONFIGS`) + CI gate | A, B | Planned |

Track A is the kernel half (the syscall no clock-correcting daemon can exist without); B/C are
userspace. Land A first — B depends on the wrapper.

---

## Track A — Writable wall clock

### A.1 — `set_wall_epoch` writer + TSC re-anchor

**File:** `kernel/src/rtc.rs`
**Symbols:** `BOOT_EPOCH_SECS` (`rtc.rs:10`, single writer today at `rtc.rs:237`), `build_epoch_floor` (`rtc.rs:209`); `tsc_now_us` (`kernel/src/arch/x86_64/syscall/mod.rs:20541`), `boot_tsc`/`rebase_boot_tsc` (`kernel/src/arch/x86_64/apic.rs:384`/`:395`)
**Why it matters:** `BOOT_EPOCH_SECS` is written exactly once (boot); a correct step must re-anchor the TSC origin so `tsc_now_us` (which adds `elapsed_tsc`) does not double-count.

**Acceptance:**
- [ ] `set_wall_epoch(secs, nsec)` stores the new base **and** records the TSC-at-set so post-set `tsc_now_us` = new_base + TSC_since_set (continuous, monotonic across the step).
- [ ] `CLOCK_MONOTONIC` is provably unperturbed by a wall-clock step (the monotonic origin, `rebase_boot_tsc`, is untouched).
- [ ] No CMOS write-back in this cut (RTC hardware stays read-only); documented as deferred.

### A.2 — `sys_clock_settime` / `sys_settimeofday` (root-gated, floor-clamped)

**File:** `kernel/src/arch/x86_64/syscall/mod.rs`
**Symbols:** existing read side `sys_gettimeofday`/`sys_clock_gettime` (`mod.rs:20565`/`:20595`), constants `GETTIMEOFDAY = 96`/`CLOCK_GETTIME = 228` (`mod.rs:1633`/`:1636`); new `CLOCK_SETTIME = 227`
**Why it matters:** Setting the clock is privileged — a wrong/backward clock breaks TLS, cron, and audit ordering; the syscall must fail closed and honor the Phase 86a anti-rollback floor.

**Acceptance:**
- [ ] `sys_clock_settime(clk_id, tp_ptr)` reads the userspace `timespec`, rejects `CLOCK_MONOTONIC` (`EINVAL`), rejects non-root callers (`EPERM`), and rejects a time earlier than `build_epoch_floor()` (anti-rollback) before calling `set_wall_epoch`.
- [ ] Dispatch arm added next to the read syscalls; a `sys_settimeofday` (number `164`) alias may share the validator.
- [ ] Host tests in `kernel-core` cover the NTP-64 ↔ Unix conversion and the offset/round-trip-delay math the daemon uses (pure logic, no hardware).

### A.3 — `syscall-lib` wrappers

**File:** `userspace/syscall-lib/src/lib.rs`
**Symbols:** existing readers `clock_gettime` (`lib.rs:3272`), `gettimeofday` (`lib.rs:3284`), `SYS_CLOCK_GETTIME = 228` (`lib.rs:1414`)
**Why it matters:** `sntpd` (and a future `date -s`) need a typed setter.

**Acceptance:**
- [ ] `clock_settime(clk_id, secs, nsec) -> isize` and/or `settimeofday(secs, usec) -> isize` wrappers with `SYS_CLOCK_SETTIME`/`SYS_SETTIMEOFDAY` constants.

---

## Track B — `sntpd` daemon

### B.1 — SNTP packet codec (host-tested)

**Files:** `kernel-core/src/` (new `sntp.rs`, host-testable) **or** `userspace/sntpd/src/sntp.rs`
**Symbols:** `encode_request` / `parse_reply` / `compute_offset`
**Why it matters:** The RFC 4330 48-byte header + NTP 64-bit fixed-point timestamps + the offset formula are pure logic; host-test them off-device.

**Acceptance:**
- [ ] `encode_request()` builds a mode-3 client packet; `parse_reply(buf)` extracts stratum, LI, and the receive/transmit timestamps; `compute_offset(t1,t2,t3,t4)` implements `((T2−T1)+(T3−T4))/2` and the round-trip delay.
- [ ] NTP↔Unix epoch conversion (the 2,208,988,800 constant) is host-tested against known vectors.
- [ ] Sanity predicates (`stratum in 1..=15`, LI ≠ alarm, non-zero transmit ts, delay ≤ ceiling) are unit-tested.

### B.2 — `sntpd` main loop

**File:** `userspace/sntpd/src/main.rs` (new)
**Symbols:** socket surface `socket`/`sendto`/`recvfrom` (`syscall-lib/src/lib.rs:2365`/`:2424`/`:2454`), `SockaddrIn::new` (`lib.rs:1557`); template `userspace/syslogd/src/main.rs`; `udp-smoke` reference (`userspace/udp-smoke/src/main.rs`)
**Why it matters:** A `no_std` Rust daemon has **no** `getaddrinfo` (DNS is musl-C only), so it must target a numeric server IP and drive UDP directly.

**Acceptance:**
- [ ] `socket(AF_INET, SOCK_DGRAM, 0)` → `sendto` request → bounded `recvfrom` (timeout + bounded retries) → sanity-gate → `clock_settime(CLOCK_REALTIME, …)`; each successful sync logged (server, offset, new time) via syslog.
- [ ] Server IP + poll interval read from `/etc/sntpd.conf` (numeric IP default); missing/invalid config fails closed (no set) with a log line.
- [ ] Runs once at boot then periodically; `CLOCK_MONOTONIC`-unperturbed step confirmed.

---

## Track C — Wiring + gate

### C.1 — Four registration points + `KNOWN_CONFIGS` slot

**Files:** root `Cargo.toml` (`members`), `xtask/src/main.rs` (`bins[]` at `main.rs:2107`, `build_userspace_bins` at `:2099`), `kernel/src/fs/ramdisk.rs` (`generated_initrd_asset!` const + `BIN_ENTRIES` at `ramdisk.rs:547`), `userspace/init/src/main.rs` (`KNOWN_CONFIGS` at `:190`, orphan `ntpd.conf` at `:205`), `xtask/src/main.rs` (`populate_ext2_files` conf literal + staging, `:31986`)
**Why it matters:** Missing any point means the binary isn't built, isn't embedded, or isn't launched; the `ntpd.conf` slot in `KNOWN_CONFIGS` is already reserved but backs nothing.

**Acceptance:**
- [ ] `sntpd` added as a workspace member, a `bins[]` tuple (`needs_alloc` per its deps), an ELF const + `BIN_ENTRIES` tuple, and an `/etc/services.d/sntpd.conf` (`type=oneshot` or `daemon`, `restart=on-failure`, `user=0`) staged in `populate_ext2_files` + matched in `KNOWN_CONFIGS` (reuse/replace the orphan `ntpd.conf` entry). `cargo xtask clean` to recreate the disk.

### C.2 — `sntp-smoke` gate (local responder + opt-in live)

**Files:** `xtask/src/main.rs` (new `cmd_sntp_smoke` + a harness UDP SNTP responder), `.githooks/pre-push` (`M3OS_SNTP_REGRESSION`), `AGENTS.md` + `docs/appendix/regression-gates.md`
**Why it matters:** A deterministic local responder proves the offset math + step without depending on the public internet; the live arm proves the real path.

**Acceptance:**
- [ ] The harness stands up a UDP responder (loopback/SLIRP) returning a known transmit timestamp; the guest boots with a deliberately skewed clock, runs `sntpd`, and `clock_gettime(CLOCK_REALTIME)` converges to the injected time within a small delta. Non-root set → `EPERM`; below-floor → rejected.
- [ ] Opt-in `M3OS_SNTP_LIVE` arm queries a real public NTP server (skip-with-reason in CI, like `git-https-smoke`'s live arm).
- [ ] Gate row added to `AGENTS.md` + `regression-gates.md`.

---

## Documentation Notes

- This phase adds the **first** writable wall-clock path; frame the syscall as a substrate
  correction (the RTC has been read-only since Phase 34), not just a feature.
- Emphasize the anti-rollback clamp reuses the Phase 86a build-date floor — a set that would
  move the clock *backward* past the image build is refused.
- Note `crond` (`clock_gettime` at `crond/src/main.rs:611`) and TLS (Phase 86c) are the
  concrete beneficiaries — both silently degrade on a wrong clock today.
- The orphan `ntpd.conf` entry in `KNOWN_CONFIGS` (`init/src/main.rs:205`) is finally backed.
- Prefer exact symbols; `mod.rs` syscall line numbers drift — reference `sys_clock_gettime`
  / `tsc_now_us` over raw offsets.
