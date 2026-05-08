# Phase 59 — Validation Backlog

**Status:** Planned
**Source Ref:** phase-59
**Depends on:** Phase 58 (Documentation Reconciliation Pass) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Builds on:** All phases whose task docs carry deferred manual-QEMU validation items. Phase 59 does not add features; it runs the manual and semi-automated validation sessions that earlier phases deferred behind "run in QEMU once" annotations.
**Primary Components:** QEMU test harness (`cargo xtask test`, `cargo xtask run`), `docs/roadmap/tasks/` (source phase task docs that carry the deferred items), `docs/handoffs/57b-soak-gate.md` (soak result record), `kernel/src/arch/x86_64/tests/` (RTC automated test scaffold), `kernel/src/net/` (AF_UNIX integration test scaffold)

## Milestone Goal

Every major manual validation item deferred by Phases 10, 22b, 24, 30, 31, 32, 34, 39, 43, and 57b is either executed (QEMU session run, log captured, checkboxes flipped) or formally escalated to a documented decision record explaining why it cannot be run. For the two items that require new test code (Phase 34 RTC test, Phase 39 AF_UNIX test), the test code is written and added to the `cargo xtask test` suite. The Phase 57b 30-minute soak result is recorded in `docs/handoffs/57b-soak-gate.md`.

## Why This Phase Exists

The 2026-05-08 audit found that Phases 30, 31, and 32 — telnetd, TCC compiler bootstrap, and build-tools + sh0 POSIX features — were marked Complete with entire validation tracks unchecked. The pattern extended to Phases 43 (SSH end-to-end), 22b (ANSI parser visual), 24 (reboot persistence), and 57b (30-minute soak). In each case the shipped code very likely works; the issue is that no one ran the test to confirm it, and the task doc records the feature as unverified.

This is a credibility problem for a 1.0 release: the README cites telnet, TCC, make, sh0 POSIX extensions, and SSH as shipped capabilities. If the first time a user exercises them is after the 1.0 tag, the "Complete" status is aspirational rather than verified.

Phase 59 is bounded: run what is documented, record what was found, flip checkboxes accordingly. Where the test reveals a real failure, that failure is escalated to a bug report and an owner phase is assigned — the fix is not in scope here.

## Learning Goals

- How to run and capture QEMU serial log output for structured manual test passes.
- How to write an automated QEMU kernel test that validates hardware-facing behaviour (RTC read accuracy, AF_UNIX kernel-internal plumbing) within the `cargo xtask test` harness.
- What constitutes sufficient evidence to flip a task-doc checkbox from `[ ]` to `[x]`.

## Feature Scope

### Track A — Phase 30 Telnetd Manual Tests

Phase 30 Track F has 16 items, all annotated "manual QEMU test" and unchecked. The items include telnetd boot, login prompt, authentication, shell handoff, at least four concurrent sessions, SIGHUP-on-disconnect, and PTY freeing on reconnect. G.3 ("Phase 30 marked complete in roadmap README") is itself gated on Track F passing.

The QEMU session uses `cargo xtask run` plus a host-side `telnet localhost 2323` client. Each item is exercised, the serial log is captured as a log artifact, and the result is recorded in a handoff note.

### Track B — Phase 31 TCC Compiler Bootstrap

Phase 31 Tracks D, E, F (approximately 15 items) are all deferred. Track E — TCC self-hosting (tcc compiling tcc) — is the long pole and the headline Phase 31 claim. Track D includes basic usage (`tcc --version`, `tcc -run hello.c`). Track F includes `cargo xtask check` passing with all new Phase 31 code.

TCC self-hosting requires the TCC binary and source to be resident on the m3OS ext2 data disk; the xtask build pipeline populates this via the ports tree. The QEMU session runs with `cargo xtask run --fresh` to ensure a clean disk.

### Track C — Phase 32 Build Tools and sh0 POSIX Features

Phase 32 Tracks B (partial), C (partial), D, E, F (~20 items) are deferred. Track D adds POSIX shell features to sh0 (`for`/`if`/`$?`/`$()`) — substantial new behaviour never validated. Track E covers full GNU make build, incremental rebuild, `make clean`, and ar archive creation.

### Track D — Phase 43 SSH End-to-End

Phase 43 G.1 (password auth), G.2 (pubkey auth), and G.3 (Wireshark traffic inspection) are all unchecked manual items. SSH is a flagship capability; the three tests are the minimum evidence that it works against a standard `ssh` client. The pubkey format incompatibility (hex-encoded Ed25519, not OpenSSH wire format) is documented but not fixed here — the decision to fix or explicitly defer belongs to an owner phase.

### Track E — Phase 22b ANSI Parser Visual Tests

Phase 22b Track F has 7 items, every one marked "Deferred (manual QEMU visual test)". Includes the critical regression check P22b-T046: sh0 still works correctly in cooked-mode echo after ANSI parser changes.

### Track F — Phase 24 Reboot Persistence

P24-T043 (reboot, remount, verify persisted file survives) is unchecked. This is the single unrun test item for Phase 24. Estimated: 30 minutes in QEMU.

### Track G — Phase 57b 30-Minute Soak

Phase 57b's acceptance gate (PR #132) requires a 30-minute soak test under SMP load with `preempt_count` discipline and `IrqSafeMutex` wiring active. The soak gate doc `docs/handoffs/57b-soak-gate.md` exists with an empty result table. This track runs the soak and populates the table. The soak result doc is then cross-referenced by Phase 62 Track F.

### Track H — Phase 34 Automated RTC Test

Phase 34 E.2 requires an automated QEMU RTC accuracy test. This track writes the test code in the `cargo xtask test` scaffold, runs it, and flips E.2's checkbox.

### Track I — Phase 39 AF_UNIX Integration Test

Phase 39 J.1 requires an automated AF_UNIX integration test covering at minimum `SOCK_STREAM` connect/send/recv across two processes. This track writes and runs the test.

### Track J — Phase 10 Real-Hardware Secure Boot

Phase 10 C.3 (real-hardware Secure Boot boot with EFI binary signed by project key) is conditionally deferred on hardware availability. This track documents a hardware-availability decision: if appropriate hardware is available in the lab, run the test and flip C.3; if not, add an explicit deferral note with an owner (post-1.0) to Phase 10's task doc.

## Important Components and How They Work

### QEMU Serial Log Capture

All manual QEMU tests produce a log artifact by running `cargo xtask run` with serial redirected to a file: `cargo xtask run 2>&1 | tee /tmp/phase-NN-test.log`. The log is quoted in the handoff note and referenced in the checkbox citation.

### `cargo xtask test` Harness

Automated tests use the ISA debug-exit convention: write `0x10` to port `0xf4` for success, `0x11` for failure. The RTC test (H) and AF_UNIX test (I) each become a named test binary added to the `bins` array in `xtask/src/main.rs` with `needs_alloc = false` (kernel-resident tests) or added to `kernel-core` host tests.

### `docs/handoffs/57b-soak-gate.md`

The file exists but has an empty result table. Track G populates: start time, end time, QEMU SMP configuration, observed panics (if any), max observed preemption latency (from serial log), and a pass/fail verdict.

## How This Builds on Earlier Phases

- Closes the validation deficits that Phases 10, 22b, 24, 30, 31, 32, 34, 39, 43, and 57b deferred.
- Depends on Phase 58 having reconciled the task-doc format so that the checkboxes being flipped here are in the correct `[x]`/`[ ]` format.
- Provides the soak result that Phase 62 Track F references when updating Phase 57b's design doc.

## Implementation Outline

Phase 59 is the manual-validation tier of the test pyramid: host-side `kernel-core` unit tests are the wide automated base; `cargo xtask test` QEMU smoke tests are the middle tier; and the QEMU interactive sessions in Tracks A–J are the narrow top requiring human judgment. Keeping track of which tier each item belongs to prevents re-running automated tests as manual ones and vice versa.

1. Run Track G (57b soak) first — it is time-bounded (30 minutes) and its result unblocks Phase 62 Track F.
2. Run Track F (Phase 24 reboot persistence) — 30 minutes, single-item, quick win.
3. Run Track E (Phase 22b ANSI parser) — 1 hour, visual tests, confirm sh0 regression.
4. Run Track A (Phase 30 telnetd) — 2–3 hours, 16 items, host-side telnet client.
5. Run Track D (Phase 43 SSH) — 2–3 hours, 3 items, host-side `ssh` client.
6. Write and run Track H (Phase 34 RTC automated test). For Track H and I, write the failing automated test first in `kernel-core` (host side) before wiring the QEMU harness entry — the host test is the specification that the QEMU run must satisfy.
7. Write and run Track I (Phase 39 AF_UNIX automated test).
8. Run Track B (Phase 31 TCC) — 4–6 hours, including self-hosting attempt.
9. Run Track C (Phase 32 build tools + sh0) — 4–6 hours.
10. Document Track J hardware-availability decision for Phase 10 C.3.
11. For each track: flip source-phase task-doc checkboxes, add a closure note to the source-phase design doc.

## Acceptance Criteria

- Tracks A, B, C, D, E, F: QEMU session run, serial log artifact captured in a handoff note, source-phase task-doc checkboxes updated.
- Track G: `docs/handoffs/57b-soak-gate.md` result table populated with a pass or fail verdict and supporting log reference.
- Track H: an automated RTC accuracy test exists in the `cargo xtask test` suite and passes.
- Track I: an automated AF_UNIX integration test exists in the `cargo xtask test` suite and passes.
- Track J: Phase 10 C.3 is either `[x]` with a hardware test log, or `[ ] — Deferred: post-1.0 (no suitable hardware in lab)` with the decision recorded.
- Every source-phase design doc whose validation track was completed receives a one-paragraph closure note added to its "Acceptance Criteria" section.
- Any test failure discovered during a track is captured as a bug report (GitHub issue or doc in `docs/post-mortems/`) with an assigned owner phase before the track is marked complete.

## Companion Task List

- [Phase 59 Task List](./tasks/59-validation-backlog-tasks.md)

## How Real OS Implementations Differ

- Production OS projects (Linux distributions, FreeBSD releases) have CI infrastructure that runs regression and integration tests automatically on every commit. m3OS does not; this phase is the manual equivalent of a release-candidate integration-test pass.
- Real regression suites capture machine-parseable artifacts (JUnit XML, TAP output) rather than serial log text. Phases 43c and later add this infrastructure for kernel tests; this phase uses the simpler log-capture pattern for the remaining manual items.
- Formal validation of a network protocol (SSH, telnet) against a standards body test suite is normal for production implementations. m3OS's equivalent is an interop test against a standard client binary, which is sufficient for a learning-OS 1.0 bar.

## Deferred Until Later

- Fixing any failures discovered during the validation runs — those are escalated to owner phases.
- Automating the remaining manual QEMU tests (telnetd concurrent sessions, TCC self-hosting) — post-1.0.
- Phase 43 pubkey format compatibility (OpenSSH wire format) — explicitly deferred pending an owner phase decision.
- Phase 10 real-hardware Secure Boot test if hardware is unavailable — post-1.0.
