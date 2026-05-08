# Phase 59 — Validation Backlog: Task List

**Status:** Planned
**Source Ref:** phase-59
**Depends on:** Phase 58 (Documentation Reconciliation Pass) ✅, Phase 57e (Full Kernel Preemption — Deferred 2026-05-07) ✅
**Goal:** Run every major manual QEMU validation session deferred by Phases 10, 22b, 24, 30, 31, 32, 34, 39, 43, and 57b; write two new automated tests (Phase 34 RTC, Phase 39 AF_UNIX); record the Phase 57b 30-minute soak result; and flip source-phase task-doc checkboxes with log-artifact citations.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Phase 30 telnetd — 16 manual QEMU items | — | Planned |
| B | Phase 31 TCC — Tracks D/E/F (~15 items), including self-hosting | — | Planned |
| C | Phase 32 build tools + sh0 POSIX — Tracks B/C/D/E/F (~20 items) | — | Planned |
| D | Phase 43 SSH end-to-end — G.1 password, G.2 pubkey, G.3 Wireshark | — | Planned |
| E | Phase 22b ANSI parser visual — Track F 7 items | — | Planned |
| F | Phase 24 reboot persistence — P24-T043 | — | Planned |
| G | Phase 57b 30-minute soak — populate `57b-soak-gate.md` | — | Planned |
| H | Phase 34 RTC automated test — write + run E.2 | — | Planned |
| I | Phase 39 AF_UNIX integration test — write + run J.1 | — | Planned |
| J | Phase 10 real-hardware Secure Boot — hardware-availability decision | — | Planned |
| K | Documentation and Release | A B C D E F G H I J | Planned |

---

## Track A — Phase 30 Telnetd Manual Tests

### A.1 — Run Phase 30 Track F validation session

**Files:**
- `docs/roadmap/tasks/30-telnet-server-tasks.md`
- `docs/handoffs/59a-telnetd-validation.md` (new log artifact)

**Symbol:** Track F checkbox items F.1–F.16
**Why it matters:** 16 items including telnetd boot, login prompt, auth → shell, ≥4 concurrent sessions, SIGHUP on disconnect, and PTY freeing on reconnect. All unchecked for a phase marked Complete. SSH depends on PTY plumbing proven by this phase.

**Acceptance:**
- [ ] `cargo xtask run` executed; `telnet localhost 2323` connection established from host.
- [ ] Serial log captured to `docs/handoffs/59a-telnetd-validation.md` with timestamps.
- [ ] Each of Phase 30 Track F's 16 items marked `[x]` with a log line citation, or marked `[ ] — Bug #NN: <description>` if the item fails.
- [ ] Phase 30 G.3 ("Phase 30 marked complete in roadmap README") flipped to `[x]` if all 16 pass.
- [ ] Phase 30 design doc receives a one-paragraph closure note citing this phase.

---

## Track B — Phase 31 TCC Compiler Bootstrap

### B.1 — Run Phase 31 Tracks D, E, F validation

**Files:**
- `docs/roadmap/tasks/31-compiler-bootstrap-tasks.md`
- `docs/handoffs/59b-tcc-validation.md` (new log artifact)

**Symbol:** Track D (`tcc --version`, `tcc -run hello.c`), Track E (TCC self-hosting), Track F (`cargo xtask check`)
**Why it matters:** TCC self-hosting is the headline Phase 31 claim. No validation of self-hosting has been run since the phase shipped.

**Acceptance:**
- [ ] `cargo xtask run --fresh` executed; TCC binary found on ext2 data disk.
- [ ] `tcc --version` produces a version line — Track D item verified.
- [ ] `tcc -run hello.c` compiles and runs hello.c — Track D item verified.
- [ ] Track E self-hosting: `tcc tcc.c` (or equivalent) runs to completion without kernel panic — result recorded (pass or specific failure).
- [ ] Track F: `cargo xtask check` passes with all Phase 31 code present — verified.
- [ ] Serial log captured to `docs/handoffs/59b-tcc-validation.md`.
- [ ] Phase 31 task-doc Track D, E, F checkboxes flipped with log citations or explicit deferral.
- [ ] Phase 31 design doc receives a closure note.

---

## Track C — Phase 32 Build Tools and sh0 POSIX Features

### C.1 — Run Phase 32 Tracks B/C/D/E/F validation

**Files:**
- `docs/roadmap/tasks/32-build-tools-tasks.md`
- `docs/handoffs/59c-buildtools-validation.md` (new log artifact)

**Symbol:** sh0 `for`/`if`/`$?`/`$()` extensions (Track D), GNU make full build + incremental + clean + ar (Track E), `cargo xtask check` (Track F)
**Why it matters:** sh0 POSIX feature additions are substantial new sh0 behaviour never validated. `make` is the build system used by TCC and ports; its correctness is foundational.

**Acceptance:**
- [ ] sh0 `for x in a b c; do echo $x; done` executes correctly.
- [ ] sh0 `if [ $? -eq 0 ]; then echo ok; fi` executes correctly.
- [ ] sh0 command substitution `$(echo hello)` expands correctly.
- [ ] `make -f <test-makefile>` completes a full build run.
- [ ] Incremental make (second run with no changes) produces "Nothing to be done".
- [ ] `make clean` removes targets.
- [ ] `ar rc libtest.a obj.o` creates an archive.
- [ ] Serial log captured to `docs/handoffs/59c-buildtools-validation.md`.
- [ ] Phase 32 task-doc Track B/C/D/E/F checkboxes flipped with log citations.
- [ ] Phase 32 design doc receives a closure note.

---

## Track D — Phase 43 SSH End-to-End

### D.1 — Run Phase 43 G.1 password auth, G.2 pubkey auth, G.3 traffic inspection

**Files:**
- `docs/roadmap/tasks/43-ssh-server-tasks.md`
- `docs/handoffs/59d-ssh-validation.md` (new log artifact)

**Symbol:** G.1, G.2, G.3 checkbox items
**Why it matters:** SSH is a flagship m3OS capability cited in AGENTS.md. No end-to-end test has been run against a standard client.

**Acceptance:**
- [ ] `cargo xtask run` executed with SSHD configured; host-side `ssh -o StrictHostKeyChecking=no user@localhost -p 2222` connects.
- [ ] G.1 password authentication: login with valid password succeeds; login with invalid password rejected.
- [ ] G.2 pubkey authentication: add a test pubkey to authorized keys (in m3OS hex format); `ssh -i <privkey>` authenticates.
- [ ] G.3 traffic capture: `tcpdump`/Wireshark (or `tshark`) on the QEMU tap interface confirms SSH wire traffic is present and encrypted.
- [ ] Log artifact captured to `docs/handoffs/59d-ssh-validation.md`.
- [ ] Phase 43 G.1, G.2, G.3 flipped to `[x]` with citations, or `[ ] — Bug #NN` if failed.
- [ ] Phase 43 design doc receives a closure note.
- [ ] OpenSSH pubkey format incompatibility explicitly documented as `[ ] — Deferred: post-1.0 (see Phase 43 C.12)` in the task doc if not fixed.

---

## Track E — Phase 22b ANSI Parser Visual Tests

### E.1 — Run Phase 22b Track F validation (7 visual items)

**Files:**
- `docs/roadmap/tasks/22b-ansi-escape-tasks.md`
- `docs/handoffs/59e-ansi-parser-validation.md` (new log artifact)

**Symbol:** Track F items P22b-T040 through P22b-T046 (the 7 "Deferred (manual QEMU visual test)" rows) — note: this task doc uses the legacy **pipe-table** format, not checkboxes. The implementer updates each row's `Status` cell from `Deferred (manual QEMU visual test)` to `Done (Phase 59 Track E.1, log: docs/handoffs/59e-ansi-parser-validation.md)` rather than flipping `[ ]` → `[x]`. Phase 58 Track B.3 deferred the format conversion of legacy task docs (other than Phase 16) to post-1.0.
**Why it matters:** P22b-T046 is the regression check for whether the ANSI parser changes broke sh0's cooked-mode echo and line discipline. It was deferred despite being the most basic correctness check.

**Acceptance:**
- [ ] `cargo xtask run` executed; sh0 interactive session started.
- [ ] P22b-T046: sh0 accepts input characters, echoes them, and processes line discipline (backspace, Ctrl-C) correctly.
- [ ] At least 5 of the 7 Track F visual items (P22b-T040–P22b-T046) verified against `cargo xtask run` session output.
- [ ] Serial/framebuffer log artifact captured to `docs/handoffs/59e-ansi-parser-validation.md`.
- [ ] Phase 22b Track F pipe-table `Status` cells updated for each verified row (not `[x]` checkboxes — see Symbol field above).
- [ ] Phase 22b design doc closure note added.

---

## Track F — Phase 24 Reboot Persistence

### F.1 — Run P24-T043 reboot-persistence test

**Files:**
- `docs/roadmap/tasks/24-persistent-storage-tasks.md`
- `docs/handoffs/59f-persistence-validation.md` (new log artifact)

**Symbol:** P24-T043 (and optionally P24-T044 — host-side visibility check via `losetup -P` + `mount`, also currently deferred). Note: this task doc uses the legacy **pipe-table** format, not checkboxes. The implementer updates the row's `Status` cell from `deferred (requires interactive QEMU)` to `Done (Phase 59 Track F.1, log: docs/handoffs/59f-persistence-validation.md)` rather than flipping `[ ]` → `[x]`.
**Why it matters:** Proves that writes to the ext2 data disk survive a QEMU reboot — the core Phase 24 persistence claim.

**Acceptance:**
- [ ] `cargo xtask run` started; a file written to `/data/` (or equivalent persistent path).
- [ ] QEMU rebooted (or `sys_reboot` called); system comes back up without `--fresh`.
- [ ] Written file found intact on remount — verified via `cat` or `ls` in sh0.
- [ ] Log captured to `docs/handoffs/59f-persistence-validation.md`.
- [ ] P24-T043 pipe-table `Status` cell updated (not `[x]` — see Symbol field above).
- [ ] Phase 24 design doc receives a closure note.

---

## Track G — Phase 57b 30-Minute Soak

### G.1 — Run the soak and populate `57b-soak-gate.md`

**File:** `docs/handoffs/57b-soak-gate.md`
**Symbol:** "Result tracking" table (existing columns: `Date | Operator | Duration | Result | Notes`).
**Why it matters:** PR #132 acceptance gate requires this soak. The table is currently empty. Phase 62 Track F cross-references this result when updating Phase 57b's design doc.

**Acceptance:**
- [ ] `cargo xtask run-gui --fresh` (per the soak-gate doc's documented procedure) running for 30 minutes wall-clock time, with synthetic IPC + futex + notification load on ≥4 cores.
- [ ] Serial log monitored against the four pass criteria the soak-gate doc enumerates (zero `preempt_count != 0 at user-mode return` panics; no new `[WARN] [sched]` lines vs. pre-57b baseline; no deadlocks; clean shutdown).
- [ ] `docs/handoffs/57b-soak-gate.md` Result-tracking table row appended: `Date`, `Operator`, `Duration` (= 30m), `Result` (Pass / Fail), `Notes` (QEMU command line if non-default, observed panics or "none", reference to log artifact). The Notes column carries the additional detail (start/end times, QEMU config, panic count) so the existing 5-column schema does not need to change.
- [ ] If the soak fails: kernel panic captured in log per the soak-gate doc's "Failure handling" section, bug report filed, Result = Fail.
- [ ] Phase 57b task doc receives a cross-reference to the soak gate doc.

---

## Track H — Phase 34 Automated RTC Test

### H.1 — Write and integrate the RTC accuracy automated test

**Files:**
- `kernel/tests/rtc_accuracy.rs` (new test file — kernel integration tests live in the top-level `kernel/tests/` directory; existing peers: `bound_recv.rs`, `preempt_latency.rs`, `preempt_user_stress.rs`, `preempt_voluntary.rs`, `sched_fuzz.rs`, `xsave_avx.rs`)
- `xtask/src/main.rs` (`build_test_binaries` discovers new files in `kernel/tests/` automatically; no list edit required, but verify the test boots through `qemu_test_args_with_devices`)
- `docs/roadmap/tasks/34-real-time-clock-tasks.md`

**Symbol:** `test_rtc_read_accuracy` (new), E.2 checkbox item
**Why it matters:** Phase 34 E.2 is an unchecked item requiring an automated QEMU test. Without automation, RTC correctness is only established by a one-time manual run.

**Acceptance:**
- [ ] Test binary reads CMOS RTC time at boot, waits ~1 s (using TSC or APIC timer), reads again; asserts elapsed time is within ±200 ms.
- [ ] `cargo xtask test --test rtc_accuracy` passes (QEMU exits with code `0x10` per `QEMU_EXIT_SUCCESS` in `xtask/src/main.rs`).
- [ ] Phase 34 task-doc E.2 checkbox flipped to `[x]` with the test binary name as citation.
- [ ] Phase 34 design doc closure note updated.

---

## Track I — Phase 39 AF_UNIX Integration Test

### I.1 — Write and integrate the AF_UNIX integration test

**Files:**
- `kernel/tests/unix_socket.rs` (new test file — matches the file path Phase 39 J.1 already cites; kernel integration tests live in `kernel/tests/`, not `kernel/src/net/tests/`)
- `xtask/src/main.rs` (test binary discovered automatically by `build_test_binaries` from `kernel/tests/`)
- `docs/roadmap/tasks/39-unix-domain-sockets-tasks.md`

**Symbol:** `unix_socket_test` (per Phase 39 J.1 spec); supersedes the J.1 deferral note that records "existing test harness uses kernel-level `#[test_case]` only; userspace test binary is built and embedded in initrd for manual/smoke testing". Reference implementation already lives at `userspace/unix-socket-test/src/main.rs` (covers `socketpair`, named stream + datagram, `accept`/`connect`/`send`/`recv`, `shutdown`); the kernel-resident test binary should mirror its coverage using `kernel-core` pure-logic helpers where possible.
**Why it matters:** Phase 39 J.1 is currently `[x]` with a deferral note rather than a real automated test. Phase 59 closes the deferral.

**Acceptance:**
- [ ] Test spawns a server task listening on a UNIX domain socket and a client task that connects, sends a fixed payload, and receives an echo. Coverage equivalent to `userspace/unix-socket-test/src/main.rs`'s test set (or that test is invoked from the integration test as a userspace child).
- [ ] `cargo xtask test --test unix_socket` passes (QEMU exits with `0x10`).
- [ ] Phase 39 task-doc J.1 deferral notes replaced with a positive citation to the new `kernel/tests/unix_socket.rs` (the existing `[x] (deferred — …)` items become `[x] (verified Phase 59 Track I.1, kernel/tests/unix_socket.rs)`).
- [ ] Phase 39 design doc closure note updated.

---

## Track J — Phase 10 Real-Hardware Secure Boot Decision

### J.1 — Document hardware availability and update Phase 10 C.3

**File:** `docs/roadmap/tasks/10-secure-boot-tasks.md`
**Symbol:** C.3 checkbox item
**Why it matters:** The real-hardware Secure Boot test requires hardware with Secure Boot support and the ability to enroll custom keys. This may not be available in the lab.

**Acceptance:**
- [ ] If hardware is available: C.3 run, serial boot log captured, checkbox flipped to `[x]`.
- [ ] If hardware is unavailable: C.3 converted to `[ ] — Deferred: post-1.0 (hardware not available in lab as of 2026-05-08)` with an explanation in Phase 10's task doc.
- [ ] Phase 10 design doc receives a one-line note matching whichever outcome was chosen.

---

---

## Track K — Documentation and Release

### K.1 — Create the aligned legacy learning doc

**File:** `docs/59-validation-backlog.md`
**Symbol:** new file
**Why it matters:** The doc-template "aligned legacy learning doc" form gives a learner-friendly companion to the design + task docs. Every shipped phase has one (or has a deliberate exception). This file is created from the template in `docs/appendix/doc-templates.md` § "Template: aligned legacy learning doc".

**Acceptance:**
- [ ] `docs/59-validation-backlog.md` exists, follows the template (Aligned Roadmap Phase, Status, Source Ref, Supersedes Legacy Doc / new — all present)
- [ ] Overview paragraph is learner-friendly and explains the phase outcome in plain language
- [ ] "What This Doc Covers" lists 3+ concrete topics
- [ ] "Core Implementation" is written for a learner who has not read the design or task doc
- [ ] "Key Files" table cites the actual files this phase touches
- [ ] "How This Phase Differs From Later Validation Work" (or analogous heading specific to this phase) is filled in
- [ ] "Related Roadmap Docs" links the design and task docs

### K.2 — Bump kernel version to 0.59.0

**Files:**
- `kernel/Cargo.toml`
- `Cargo.lock`
- `AGENTS.md`
- `docs/roadmap/README.md` (any version annotations)

**Symbol:** `version` field in `kernel/Cargo.toml` `[package]` section
**Why it matters:** Phase closure is signalled by a kernel version bump per project convention. Each new phase moves the project from `0.<previous>.x` to `0.<NN>.0`. The `AGENTS.md` "Kernel v0.X.Y" reference must move with it (per audit Red Flag — `AGENTS.md` was found stale at `v0.51.0` during the 2026-05-08 audit).

**Acceptance:**
- [ ] `kernel/Cargo.toml` `version = "0.59.0"`
- [ ] `Cargo.lock` regenerated (`cargo generate-lockfile` or similar)
- [ ] `AGENTS.md` "Kernel v0.59.0" reference updated
- [ ] `cargo xtask check` passes after the bump
- [ ] Git tag suggestion: `v0.59.0` (tag at phase merge, not at task-checkbox tick)

---

## Documentation Notes

- Each track's log artifact should be a new file in `docs/handoffs/` named with the `59X-` prefix so it is clearly associated with this phase.
- When flipping checkboxes in source-phase task docs, always add a parenthetical citation: `[x] <item text> (verified Phase 59 Track A, log: docs/handoffs/59a-telnetd-validation.md)`.
- If a validation run discovers a real failure, create a GitHub issue or `docs/post-mortems/` entry before marking the track complete. Do not flip checkboxes to `[x]` for items that actually failed.
- Tracks B and C (TCC self-hosting, make + sh0) are the longest-running and most likely to reveal real gaps. Run them after the quicker tracks to establish a success baseline first.
- Track G (57b soak) is time-bounded by wall clock; start it running in the background and work other tracks concurrently.
