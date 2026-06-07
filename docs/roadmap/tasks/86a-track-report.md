# Phase 86a — Parallel Implementation Track Report

**Workflow:** `/flow:parallel-impl`
**Integration branch:** `feat/phase-86a-outbound-foundation`
**Base / review target:** `main`
**Concurrency cap:** 2 (`.flow/defaults.json` absent → default)
**Models:** implementer = `claude-sonnet-4.6`, reviewer = `claude-opus-4.7` (per `.claude/models.yaml`)
**Max revision rounds per track:** 2

## Discovery brief

- **Task shape:** multi-track-batch (4 tracks A–D from `86a-outbound-foundation-tasks.md`).
- **Scout:** skipped — the task doc is a fully-scoped brief (exact files, symbols, line numbers, acceptance). Skip reason recorded per skill skip-condition. Coordinator read every task site directly to ground the briefs.
- **Validation commands (integration, coordinator-owned):**
  - `cargo xtask check` (clippy -D warnings + rustfmt + host tests incl. new csprng tests)
  - `cargo xtask test` (QEMU kernel tests)
  - `cargo xtask smoke-test` + `regression`
  - `M3OS_DNS_REGRESSION=1` dns-smoke gate (PASS, not SKIP)
  - forced-bad-RTC smoke (CLOCK_REALTIME ≥ build-date floor)
- **Track file-set disjointness (verified):** A (kernel-core csprng/prng/lib, syscall/mod.rs, kernel/lib.rs, mm/elf.rs, net/tcp.rs, crypto-lib/random.rs), B (kernel/build.rs, kernel/src/rtc.rs), C (xtask/main.rs, xtask/port_build.rs, ports/lib/ca-certificates/Portfile, userspace/dns-smoke). No shared files → merge order A→C→B→D is conflict-free.
- **Environment:** musl cross-compiler present (`/usr/bin/x86_64-linux-musl-gcc`); host network available (cacert.pem fetch + QEMU SLIRP DNS).

## Tracks

| Track | Tasks | Owned files | Branch | Worktree | State |
|---|---|---|---|---|---|
| A | A.1–A.5 (CSPRNG) | kernel-core/src/{csprng.rs,lib.rs,prng.rs}, kernel/src/arch/x86_64/syscall/mod.rs, kernel/src/lib.rs, kernel/src/mm/elf.rs, kernel/src/net/tcp.rs, userspace/crypto-lib/src/random.rs | `wt/phase-86a-track-a` | `../ostest-wt-track-a` | ✅ merged (e0938a1) |
| C | C.1–C.2 (DNS + CA) | xtask/src/main.rs, xtask/src/port_build.rs, ports/lib/ca-certificates/Portfile, docs/roadmap/86a-outbound-foundation.md (resolver/trust notes); dns-smoke.c already AF_INET-correct (unchanged) | `wt/phase-86a-track-c` | `../ostest-wt-track-c` | ✅ merged (ca77e35) |
| B | B.1 (wall-clock floor) | kernel/build.rs, kernel/src/rtc.rs, kernel-core/src/time.rs (pure floor helper) | `wt/phase-86a-track-b` | `../ostest-wt-track-b` | ✅ merged (cde45bf) |
| D | D.1 (version bump) | kernel/Cargo.toml, Cargo.lock | (coordinator, on integration branch) | — | ✅ done (557b062) |

## Execution plan

1. Wave 1 (parallel, cap=2): Track A + Track C implementer agents in external worktrees.
2. Review each completed track (reviewer agent on diff) → revise (≤2 rounds) → merge into integration branch.
3. Wave 2: Track B (freed slot) → review → merge.
4. Track D (coordinator) on integration branch.
5. Full integration validation, update task-doc checkboxes, update PR, batch summary.

## Rescue history

- **No rescues.** All four tracks' implementer agents returned final results; none stalled, none required a nudge or replacement.
- **Track A — 1 revision round** (round 1 of 2): the independent reviewer (opus) flagged the TCP ISN PRF as an additive (invertible) mix — observing one ISN could solve for the global secret. Re-scoped to `tcp.rs` + `csprng.rs`; replaced with a one-way **SipHash-2-4** keyed PRF over the 4-tuple + a 128-bit per-boot secret, host-tested against official SipHash vectors. Converged in one round. (Two MINOR nits fixed in the same round: timer-comment accuracy; `getrandom` secure-fill invariant doc + `debug_assert`.)
- Tracks B, C: reviewer **APPROVE**, no revision needed.

## Validation (coordinator-owned, on the integrated branch)

| Gate | Result |
|---|---|
| `cargo xtask check` (clippy -D warnings + rustfmt + all host tests incl. 16 csprng tests w/ SipHash vectors + 6 time-floor tests + xtask Portfile) | ✅ PASS |
| `cargo xtask test` (kernel QEMU suite) | ✅ 12/12 |
| `cargo xtask smoke-test` + `M3OS_DNS_REGRESSION=1` | ✅ 22 steps; `SMOKE:dns-smoke:PASS` |
| `cargo xtask regression` | ✅ 11/11 |
| Manual boot `+rdseed` | ✅ `[csprng] seeded source=rdseed credited_bits=256 state=READY` |
| Manual boot default `qemu64` (no RDSEED/RDRAND) | ✅ degraded EARLY, boots to login (no deadlock) |
| Manual boot `-rtc base=2000-01-01` | ✅ `[rtc] clock floor applied: BOOT_EPOCH_SECS=1780800303 … not 1970` |
| `cargo xtask port build ca-certificates` | ✅ `verified cacert.pem (sha 86a1f33…)` → staged `etc/ssl/certs/ca-certificates.crt` → sealed `.m3pkg` |
| Boot banner | ✅ `[m3os] Hello from kernel! v0.86.0` |

## Batch outcome

- **Merged tracks:** A, B, C, D — all four integrated into `feat/phase-86a-outbound-foundation`.
- **Retained/abandoned:** none.
- **Integration branch:** committed + pushed; PR #227 (transitioned from draft to ready).
- **Workflow outcome measures:**
  - `discovery-reuse`: scout skipped (task doc was a fully-scoped brief); coordinator read every task site to ground the briefs — reused by all tracks.
  - `rescue-attempts`: 0.
  - `abandonment-events`: 0.
  - `re-review-loops`: Track A = 1 (ISN PRF hardening); B = 0; C = 0.
- **Temporary work surfaces:** the three external worktrees (`../ostest-wt-track-{a,b,c}`) were removed after merge.
