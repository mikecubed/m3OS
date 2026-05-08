# Findings: Post-mortems and Handoffs (missed by 2026-05-07 audit)

**Validation pass:** 2026-05-08

---

## Per-doc summary

### docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md

**What:** Root cause analysis and resolution of the SCHEDULER.lock ISR deadlock that produced 60-70% SSH wedge rates on `feat/phase-55b-ring-3-driver-host`.

**Phase tie:** Phase 55b (Ring-3 Driver Host); feeds directly into the IrqSafeMutex infrastructure that 57b then generalised.

**Closure status:** Resolved. Fix commit: `ac37270`. Validation commits: `2c331ec`, `fd2c044`. 75 post-fix runs at 100% clean rate. Global lock audit commit: `c519a60` (also fixed `RAW_INPUT_ROUTER` as a second ISR-unsafe lock).

**New items the audit missed:**

- The global lock audit (`c519a60`) identified `RAW_INPUT_ROUTER` as a second ISR-unsafe spinlock (held in both `keyboard_handler` ISR context and `read_raw_scancode`/`reset_raw_input_state` task context without IF masking). The audit has no record of this second fix.
- The regression harness `scripts/ssh-wedge-regression.sh` and `scripts/ssh-wedge-regression-batch.sh` were promoted in-tree as commit `0263c58`. The audit does not track this harness. The doc notes it is not yet wired into `cargo xtask test` because full-VM boots are too expensive for every-commit use — a `cargo xtask ssh-regression --count N` subcommand is a listed follow-up with no owner.
- Action item: "H6 patch (wake-ping-pong suppression in `userspace/sshd/src/session.rs`)" — this post-mortem calls it a "semantic improvement" not confirmed shipped. `findings/07` also lists this as unconfirmed. These docs corroborate each other.

**Items it confirms or contradicts from the audit:**

- Confirms findings/07 `scheduler-fairness-regression.md` resolution is correctly characterised.
- The doc's `action items` section shows the `55c-net-remote-rx-test-bug` was NOT a concern here; this is a separate track.
- The audit (findings/07) flags H9 KEX stall as open. This post-mortem does not address H9; it is scoped to the early-wedge / ISR deadlock only. No contradiction.

---

### docs/post-mortems/2026-04-22-e1000-bound-notif.md

**What:** Root cause analysis of the Phase 55c ring-3 e1000 driver main-loop deadlock that prevented SSH sessions from completing banner exchange over `--device e1000`.

**Phase tie:** Phase 55c (Ring-3 Driver Correctness Closure) Tracks A, B, E, F, plus adjacent closures R1 (EAGAIN visibility) and R2 (IOMMU BAR identity mapping).

**Closure status:** Resolved. Fix: "Phase 55c Tracks A, B, E, and F." Confirmation script: `scripts/ssh_e1000_banner_check.sh` (Track I). No single fix commit is cited; the fix spans the Phase 55c track set.

**New items the audit missed:**

- Confirms Phase 55c Track R1 and R2 closures with explicit confirmation: `RemoteNic::send_frame` EAGAIN path wired (R1), identity-mapped 4 KiB pages for each BAR inserted at `sys_device_claim` time (R2). The audit characterised these as closed but was reading from the 55c design doc only; this post-mortem is an independent corroborating primary source.
- The doc explains *why* Phase 55b's smoke test never caught this: the one-shot ICMP echo-request/reply path completes inside the TCP handshake window before the driver parks on the endpoint. This is an architectural insight about test coverage the audit did not record.

**Items it confirms or contradicts from the audit:**

- Fully consistent with findings/05 characterisation of Phase 55c. No contradictions.

---

### docs/post-mortems/2026-04-23-boot-and-vfs-startup-stalls.md

**What:** Root cause analysis of the four-layer boot failure chain after Phase 55c integration: fork-child first-dispatch starvation, init hot-path churn, VFS stale-registry routing window, and single-slot virtio-blk request path under concurrent readers.

**Phase tie:** Phase 55c integration / boot-stability follow-up. Fix commit: `c5ead6d`.

**Closure status:** Resolved. Fix set: scheduler fork-child priority boost, init hot-path reduction, dead-VFS fallback hardening, virtio-blk `REQUEST_LOCK` serialisation.

**New items the audit missed:**

- The doc's "Follow-ups" section explicitly records two items with no current owner:
  1. The virtio-blk `REQUEST_LOCK` fix intentionally serialises requesters rather than teaching the driver true multi-request bookkeeping — "a throughput ceiling, not a scalable queue model." If the ring-3 block/VFS stack grows more concurrent at boot, a future cleanup should replace the single shared request slot with per-request descriptors.
  2. The syslogd drain-loop competition is flagged as an amplifier. The audit did not record the `REQUEST_LOCK` serialisation follow-up as an open item.
- The doc confirms that `transport-level u64::MAX from vfs_service_open()` is now treated as a temporary VFS-unavailable result that falls back to kernel ext2. This is not recorded in the audit.

**Items it confirms or contradicts from the audit:**

- No direct coverage in findings/01-07. This post-mortem is entirely new to the audit.

---

### docs/post-mortems/2026-04-24-ingress-task-starvation.md

**What:** Root cause analysis of the ring-3 NIC ingress task starving PID 1's reap loop, blocking PR #118 regression gate, and description of the items-2+3 resolution (folding ingress into `net_task` via `recv_msg_nowait` + edge-triggered wake hook; driver-death detection via `cleanup_task_ipc`).

**Phase tie:** Phase 55c Track E / PR #118 residuals.

**Closure status:** Items 2 and 3 resolved (PR date 2026-04-25). Item 1 (`sys_nanosleep` busy-yield second-wake bug) explicitly deferred: "serverization-fallback continues to be flaky (~40% pass rate on the base; my changes do not fix this)."

**New items the audit missed:**

- Item 1 (nanosleep busy-yield / `block_current_unless_woken_until` second-wake bug) is still open as of this doc and is not marked closed by the companion handoff docs either. The `findings/07` audit mentions `async-rt` mutex liveness separately but the nanosleep second-wake bug is a distinct open issue not captured in findings/01-07.
- The post-mortem's "What the real fix would look like" section prescribes three approaches (items 1-3) and explicitly records that a naive `block_current_unless_woken_until` substitution causes a second-wake failure. This is the same bug described in `docs/handoffs/2026-04-25-scheduler-design-comparison.md` and `docs/handoffs/2026-04-25-pr-118-residual-issues-update.md` — these three docs form a coherent set that the audit missed entirely.
- The `serverization-fallback` test has ~40% pass rate and is documented as flaky. The audit does not record this test as a known-flaky regression.
- The doc calls out a test-gating lesson: "`e1000-restart-crash` was added, ungated in the same phase, and never observed to pass green. It has been listed as required coverage without ever having been required-passing." No action item assigned in this doc.

**Items it confirms or contradicts from the audit:**

- Findings/07 records the scheduler ISR deadlock as resolved. This doc confirms it is a prerequisite for the ingress fix but does not change its closure status.
- Findings/07 open follow-up: "fork/scheduler handoff RIP=0x4" — this doc does not address that item (different bug class).

---

### docs/post-mortems/2026-05-07-57e-preempt-full-deferred.md

**What:** Authoritative deferral post-mortem for Phase 57e (full kernel-mode preemption). Documents 13 bugs across 18 sessions, explains the two structural root causes (unconditional signal_reschedule on every tick; naive quantum threshold delays the wakee), records what survives (SMP discipline infrastructure, defensive race-shape closures), and what is removed (timer-driven kernel-mode preemption, `preempt-full` feature flag).

**Phase tie:** Phase 57e (Full Kernel Preemption). Resolution commits cited verbatim: `8b44442`, `549584f`, `9c39291`, `052010a`, `a1bfe17`, `eb1f13d`, `d5fad05`.

**Closure status:** Deferred 2026-05-07. Phase reduced in scope to "voluntary kernel preemption with cross-core IPI fast-path." Feature flag retired.

**New items the audit missed:**

- Two open action items from the post-mortem are not in findings/01-07:
  1. `[ ] Make real-hardware smoke a per-track gate in future phases.` No owner, no phase assigned.
  2. `[ ] Track G (Bug #9 FS-mutex fairness — Option B Arc-clone refactor)` remains open. Pickup handoff: `docs/handoffs/57e-bug9-bug10-followup.md`. See that file's section below.
  3. `[ ] Bug #10 (sporadic Doom-launch kernel-mode GPF)` remains open. Same pickup handoff.
- The post-mortem confirms the five tick-multiplier bugs are fixed by referencing Track G.3 in the "Keep: SMP discipline infrastructure" section: "G.3 Sweep stale 100 Hz tick-multiplier assumptions: 5 sites fixed." The audit's validation-pass note on findings/05 already records this as closed by PR #136 — this post-mortem corroborates that finding.
- The post-mortem explains Bug #10's single observation: faulting RIP `0x4847474947479b46` with "GHIJK" register pattern — stack-corruption signature. Not reproduced subsequently. This is the same event recorded in `docs/handoffs/57e-bug9-bug10-followup.md`.

**Items it confirms or contradicts from the audit:**

- findings/05 validation-pass note states "Phase 57e is now Deferred" and lists the surviving commits. This post-mortem is the source document for those assertions. All facts are consistent.
- findings/05 validation note states "Five `× 10` / `÷ 10` tick-multiplier bugs ... are fixed (Track G.3)." This post-mortem confirms Track G.3 as part of the 57a work that was "kept" — consistent.
- findings/05 notes four `TODO(57a-C/D)` pi_lock marker sites. This post-mortem does not address those markers directly; they predate the 57e deferral.
- findings/05 notes 57a design doc still says "Planned." The post-mortem explicitly states the 57a wake protocol (Phase 57b C.1/C.2/C.3, IrqSafeMutex F.1) is "still load-bearing" and "survives." This is consistent with the 57b batch summary which shows PR #132 as merged.

---

### docs/handoff/2026-04-28-graphical-stack-startup.md

**What:** Detailed debugging handoff for the Phase 57 graphical-stack startup regression (cursor stuck at (0,0) or kbd_server dead on core 3). Documents 8 failed placement-tweak attempts, identifies the serial-stdin feeder halt-loop as the core-3 parking mechanism, and concludes the highest-confidence root cause is the lost-wake bug class in `block_current_unless_woken*` (per `docs/handoffs/2026-04-25-scheduler-design-comparison.md`).

**Phase tie:** Phase 57 / Phase 57a (Scheduler Block/Wake Rewrite).

**Closure status:** Open at doc date (2026-04-28). The lost-wake protocol bug was the Phase 57a motivation; that rewrite was completed per the 57a-batch-summary (PR #129). The residual issues in this doc (cursor-stuck symptom, kbd_server-on-AP-3) were addressed by Phase 57a tracks H.1 (`serial_stdin_feeder_task` → notification-based wait) and the v2 wake protocol.

**New items the audit missed:**

- This doc is the primary source for the `switching_out` / `wake_after_switch` bug class documentation, which the audit referenced only indirectly. It contains concrete file references and step-by-step reproduction guidance.
- The doc records 8 attempted fixes and their failure modes — useful forensic context for any future regression. Not in the audit.
- The doc identifies two bugs in `kernel/src/arch/x86_64/syscall/mod.rs`:
  1. `sys_poll` 10× multiplier bug: `(timeout_i as u64).div_ceil(10)` should be `(timeout_i as u64)`. So `poll(2000)` returns after 200 ms, not 2 s.
  2. `cpu-hog` log message 10× multiplier: `ran_ticks * 10` should be `ran_ticks`. Both are in the findings/05 audit's tick-multiplier table as open items at that time; the 57a-batch-summary confirms they were fixed by Track G.3.
- Four bugs in "Open issues NOT directly on the path" are listed:
  1. `audio_server` exits without registering `audio.cmd` when no AC'97 hardware → session_manager triggers text-fallback. The audit/57a-batch-summary Track H.2 records this as fixed (`557bbd5`).
  2. `sys_poll` 10× multiplier — fixed by G.3.
  3. `syslogd` cpu-hog ~500 ms at a stretch — addressed by 57a Track H.3.
  4. Serial-stdin feeder IRQ design fragility — addressed by 57a Track H.1.
  All four were fixed in Phase 57a. The doc pre-dates that work; at doc-write time they were open. They are now closed.

**Items it confirms or contradicts from the audit:**

- Confirms the `switching_out`/`wake_after_switch` protocol bug analysis that the audit referenced from the scheduler-design-comparison doc.
- The `parks_scheduler` Task flag from commit `e1eb5e7` is described as reverted. This revert is consistent with what findings/05 describes as "cooperative-scheduling starvation" remaining after 57a.
- This doc cites `docs/handoffs/2026-04-25-scheduler-design-comparison.md` as the root-cause document — that doc is also in the missed set (see below).

---

### docs/handoffs/2026-04-25-pr-118-residual-issues.md

**What:** Handoff for two PR #118 blockers: (1) SSH disconnect hang where sshd-child silently stops executing mid-syscall-loop, and (2) `serverization-fallback` flakiness from `sys_nanosleep` busy-yield.

**Phase tie:** Phase 55c (PR #118) residuals.

**Closure status:** Open. Both issues remain unresolved at doc date. Issue 2 ("sys_nanosleep busy-yield") is Item 1 from the 2026-04-24 post-mortem — still open. Issue 1 (SSH disconnect hang) is distinct from the earlier ISR deadlock.

**New items the audit missed:**

- Issue 1 (SSH disconnect hang) is not in findings/01-07. The doc describes the hang as occurring _between_ a successful `state.borrow()` call and the first instruction of `cleanup` — a task that ends up Blocked or descheduled with no scheduler warning, no cpu-hog, no fault. The hypotheses include: PENDING_SWITCH_OUT/wake_after_switch interaction (same family as Issue 2), async-rt executor stall.
- The acceptance criteria for Issue 1 include adding a regression test `ssh-disconnect` to `cargo xtask regression`. Not done at doc time; status unknown post-Phase-57a.
- The doc lists three script-based SSH disconnect reproduction scripts: `scripts/ssh_full_session_test.sh`, `scripts/ssh_session_exit_test.sh`. Not mentioned in the audit.
- The `block_current_unless_woken_until` second-wake failure is described in precise terms: the `wake_after_switch` flag is consumed only by the dispatch loop's switch-out handler for the *current* task being switched out. If `scan_expired_wake_deadlines` sets `wake_after_switch=true` for a task that is NOT currently switching out, the flag never fires. This is the specific invariant violation that Phase 57a's v2 protocol fixed by deleting `switching_out`/`wake_after_switch` entirely.

**Items it confirms or contradicts from the audit:**

- Findings/07 lists "H9 KEX stall" as open. This doc addresses a different hang (SSH disconnect, not KEX stall). These are separate bugs.
- Findings/07 lists `async-rt` mutex liveness as open. This doc's Issue 1 mentions the async-rt executor as a candidate; the two open issues are related but distinct.

---

### docs/handoffs/2026-04-25-pr-118-residual-issues-update.md

**What:** Second-pass investigation update for PR #118 residuals. Confirms cleanup NOOP still hangs (cleanup is not the root cause). Second-pass instrumentation localises the SSH hang: sshd-child stops executing between `@A2` write and first `cleanup` instruction — total kernel silence for 15 s, no cpu-hog, no stale-ready, no signal.

**Phase tie:** Phase 55c / PR #118 residuals (same as above).

**Closure status:** Open. One production change landed in this pass: `cleanup` function hardened with SIGHUP → 500 ms grace → SIGKILL escalation. `TIOCSCTTY` ioctl also patched to set `slave_fg_pgid`. Issue remains unresolved.

**New items the audit missed:**

- The TIOCSCTTY correctness fix: `kernel/src/arch/x86_64/syscall/mod.rs` TIOCSCTTY handler now sets `pair.slave_fg_pgid = proc.pgid` (Linux 4.6+ behaviour). Without this, `close_master` SIGHUP-to-fg-pgrp delivery was a no-op. This is a correctness fix independent of the scheduler issue. Not in any audit finding.
- `kernel/src/pty.rs`: new `set_slave_fg_pgid(id, pgid)` helper exposed for the TIOCSCTTY path. Not in the audit.
- Two `log::debug!` → `log::info!` promotions for `[signal] [pX] killed/stopped by signal Y` — makes signal delivery visible in default logs. Not in the audit.
- The subagent's proposed counter-mirroring fix for `ACTIVE_WAKE_DEADLINES` (always-decrement-old then always-increment-new) is documented as a low-risk hardening worth landing first. Status: uncommitted at doc date. Unknown if landed in Phase 57a.
- The update confirms the "second-wake silently fails" symptom reproduces with `block_current_unless_woken_until`. This is a second independent reproduction of the nanosleep second-wake bug, corroborating the post-mortem.

**Items it confirms or contradicts from the audit:**

- The TIOCSCTTY fix was not in findings/01-07. This is a new kernel correctness item.
- Findings/07 open item: "Silent EAGAIN data loss in PTY write direction 1" — not addressed by this doc (different PTY path). No contradiction.

---

### docs/handoffs/2026-04-25-scheduler-design-comparison.md

**What:** Research note comparing Linux's `hrtimer_nanosleep` / `try_to_wake_up` (single state word + barrier + CAS) to m3OS's `block_current_unless_woken_inner` (three observable flags + deferred-enqueue hand-off). Identifies the specific invariant Linux maintains that m3OS violates. Provides a concrete pseudocode sketch of the v2 protocol and recommends it as a dedicated phase.

**Phase tie:** Phase 57a (direct motivation document).

**Closure status:** The design recommendation was adopted: Phase 57a's v2 protocol implements exactly the Linux pattern described here (pi_lock, single state word, CAS wakes, deleted `switching_out`/`wake_after_switch`). The doc is a research artifact; the work it motivated is complete.

**New items the audit missed:**

- This is the primary design document that motivated Phase 57a. The audit does not cite it as a source document, though findings/05 references the "lost-wake protocol" in passing.
- The doc contains a concrete identification of the re-block scenario: "new block does NOT clear `wake_after_switch` in current code" — a specific invariant violation that Phase 57a's v2 protocol eliminates. This mechanism is more precisely characterised here than in any roadmap doc.
- The doc also contains a PTY/TIOCSCTTY correctness finding from a Redox subagent: ion's liner uses `into_raw_mode()` to disable ICANON; the TIOCSCTTY handler must clear ICANON on the slave. This feeds directly into the TIOCSCTTY fix in the pr-118-residual-issues-update doc.

**Items it confirms or contradicts from the audit:**

- Confirms that Phase 57a's motivation was correctly characterised in findings/05. No contradictions.
- The per-task spinlock intermediate step ("smaller than a full rewrite, gives most of the benefit") maps directly to Phase 57a Track B (per-task `pi_lock`). Phase 57a implemented the full rewrite, not just the intermediate — which is the correct call per this doc's recommendation.

---

### docs/handoffs/2026-05-04-virtio-input-migration.md

**What:** Plan-only handoff for migrating from PS/2 to virtio-input to fix the QEMU i8042 kbd-priority arbitration bug (held key freezes mouse cursor). Covers four phases: kernel virtio-input driver, xtask QEMU args, strip diagnostic scaffolding, fallback validation.

**Phase tie:** Phase 57d follow-up / separate engineering track, no phase number assigned.

**Closure status:** Plan only. No implementation work started at doc date (branch `feat/57d-voluntary-preemption` at `ae45431`). Unknown if subsequently implemented.

**New items the audit missed:**

- The virtio-input migration is not mentioned in any audit finding. This is a entirely new open engineering item.
- The doc confirms commit `d2f6978` added `sys_ps2_diag_counter` syscall (`0x101E`) and diagnostic counters (`MOUSE_BYTES_SEEN`, `IRQ1_ENTRIES`, etc.) — diagnostic scaffolding that should be stripped after virtio-input is verified. If the migration was never done, these diagnostic artifacts remain in the codebase.
- The "Phase 3 — strip diagnostic scaffolding" section identifies specific files to clean: `kernel/src/arch/x86_64/ps2.rs` (5 counters), `kernel/src/arch/x86_64/syscall/mod.rs` (PS2_DIAG_COUNTER syscall), `userspace/syscall-lib/src/lib.rs`, `userspace/display_server/src/main.rs`.
- Open questions recorded: modern vs legacy virtio split, Linux-keycode → PS/2 translation table size, EV_REL clamping for 9-bit signed dx/dy.

**Items it confirms or contradicts from the audit:**

- No coverage in findings/01-07. Entirely new.

---

### docs/handoffs/57a-batch-summary.md

**What:** Parallel-implementation batch summary for Phase 57a. Records all 18 tracks, 64 commits, merge SHAs, 9 post-merge follow-up fixes (7 in v2 protocol family + 2 cooperative-starvation fixes), and validation results.

**Phase tie:** Phase 57a (Scheduler Block/Wake Protocol Rewrite). PR #129.

**Closure status:** Complete per this doc. All 18 tracks merged. PR #129 marked "ready for review." Kernel version bumped to 0.57.1.

**New items the audit missed:**

- The audit (findings/05) records Phase 57a design doc status as "Planned." This batch summary confirms Phase 57a is **Complete** — PR #129 merged, all tracks landed, 1368 lib tests pass. The "Planned" status in the design doc is a bookkeeping gap, confirmed here.
- Three unresolved user-driven follow-ups at doc date: I.1 (real-hardware graphical regression), I.2 (SSH disconnect/reconnect 50-cycle soak), I.4 (60-minute long-soak). These are procedural, not implementation gaps.
- Track G.3 ("Sweep stale 100 Hz tick-multiplier assumptions: 5 sites fixed") is confirmed complete. Fixes the five `× 10` / `÷ 10` sites identified in the 2026-04-28 graphical-stack-startup handoff and findings/05. The batch summary confirms the fix is in `merged in Wave 2`.
- Track H.2 (audio_server stub when no AC'97) — commit `557bbd5`. Fixes the session_manager text-fallback issue. Track H.1 (serial_stdin_feeder_task → notification wait) — fixes kbd_server-on-AP3. Both confirmed complete.
- Track H.3 (syslogd cpu-hog: dual root cause + drain-chunk fix) — commit `5a79866`. Confirmed complete.
- The cooperative-starvation post-merge fixes (`dbcfa74` virtio_blk park instead of busy-spin; `cafdaac` sys_poll park on registered_any OR deadline_tick) were required because Phase 57a fixed the v1 lost-wake protocol but m3OS was still cooperative — any busy-wait in the kernel syscall path became a denial-of-service. These are pre-Phase-57b correctness fixes. Not in audit.
- Pre-existing test failure noted: `kernel::net::remote::tests::drain_rx_queue_removes_malformed_frames_after_deferred_queueing` was present before any 57a work — the 55c-net-remote-rx-test-bug. Audit records this as open; this doc confirms it predates 57a and was fixed as part of Phase 57b Track G (see `fix(net::remote)` in 57b-batch-summary).

**Items it confirms or contradicts from the audit:**

- Confirms findings/05 assessment that Phase 57a work is done despite the "Planned" design doc status. The batch summary is the definitive closure artifact.
- The validation note in findings/05 states "Five `× 10` / `÷ 10` tick-multiplier bugs ... are fixed (Track G.3)." This batch summary confirms G.3 merged in Wave 2. Consistent.
- I.1 validation gate (real-hardware graphical regression) at doc date: "⚠️ Tested 2026-04-29 — FAIL: cursor stuck, no graphical terminal." Root cause: cooperative-scheduling starvation (addressed by the two post-merge fixes above + Phase 57b/57d). This is the `57a-validation-gate.md` result (see below).

---

### docs/handoffs/57a-discovery-brief.md

**What:** Discovery brief for Phase 57a parallel implementation. Records scope, relevant files, task boundaries (I.1 hardware test excluded), validation commands, wave dependency ordering, comparison baseline.

**Phase tie:** Phase 57a.

**Closure status:** Artifact; no closure status. Used as input by all track agents.

**New items the audit missed:**

- Confirms that `switching_out`, `wake_after_switch`, `PENDING_SWITCH_OUT[core]` are the deletion targets. The post-merge `git grep` result in the batch summary shows zero hits — confirming complete deletion.
- Lists `userspace/syslogd/src/main.rs:141-216` and `userspace/audio_server/src/main.rs:67` as Track H bug-fix targets. Both fixed per batch summary.

**Items it confirms or contradicts from the audit:** No contradictions. Reference artifact.

---

### docs/handoffs/57a-validation-gate.md

**What:** Validation gate procedure for Phase 57a. Records I.1 (real-hardware), I.2 (SSH soak), I.3 (multi-core model fuzz), I.4 (long soak) procedures and results.

**Phase tie:** Phase 57a.

**Closure status:** Mixed.
- I.1: **FAIL** as of 2026-04-29. "Boot reaches text-fallback (session_manager retry budget exhausted waiting for `term` to register); framebuffer shows the kernel console; cursor movement does not reach the framebuffer compositor." Root cause: cooperative-scheduling starvation, not lost-wake (57a fixed those). Proper fix: Phase 57b (preemption).
- I.2, I.4: **Pending user run** (unfilled boxes).
- I.3 model-level: **✅ passes** (5000 proptest cases).

**New items the audit missed:**

- I.2 (SSH disconnect/reconnect 50-cycle soak) and I.4 (60-minute long-soak) are both marked "Pending user run" with no filled results. These are acceptance criteria for "Phase 57a is considered complete when all five rows above show ✅." With I.1 failing and I.2/I.4 pending, Phase 57a's own validation gate is not fully closed despite all tracks merging.
- The I.1 result note says the failure is "upstream of the rewrite" — cooperative-scheduling starvation. This is consistent with the Phase 57a batch summary's post-merge fix notes.

**Items it confirms or contradicts from the audit:**

- findings/05 states Phase 57a design doc shows "Planned" but work is done. This gate doc shows the implementation is complete but the validation closure (I.2/I.4) is pending. Consistent with the overall "implementation complete, validation incomplete" picture.

---

### docs/handoffs/57b-batch-summary.md

**What:** Parallel-implementation batch summary for Phase 57b. Records all 11 waves, 65 locks migrated, validations, and unresolved follow-ups.

**Phase tie:** Phase 57b (Preemption Foundation). PR #132.

**Closure status:** "Complete pending soak." PR #132 merged. All tracks (A–H) merged. Kernel version 0.57.2.

**New items the audit missed:**

- The `55c-net-remote-rx-test-bug.md` follow-up was fixed in Phase 57b scope: three RX-path tests corrected (`encode_net_send` → `encode_net_rx_notify`), `drain_rx_queue_removes_malformed_frames_after_deferred_queueing` shrunk, stale assertion in `link_event_recovers_restart_suspected_slot_with_live_endpoint` removed. The audit listed this as open; it was **closed in Phase 57b**. This is a finding contradiction.
- Three explicit unresolved follow-ups:
  1. 30-minute soak gate (H.4) — pending, documented at `docs/handoffs/57b-soak-gate.md`.
  2. Phase 57b row in README — currently "Complete pending soak"; update after soak passes.
  3. `MagazineDepot` locks remain plain `spin::Mutex` (host-test-only classification); migration owned by Phase 57e at `kernel/src/mm/slab.rs`.
- The `IrqSafeGuard` field declaration order is explicitly documented as "load-bearing": `guard` drops first (releases spinlock), `_restore` drops second (re-enables interrupts). A future reviewer disturbing this order breaks Track F. Not in audit.

**Items it confirms or contradicts from the audit:**

- findings/05 flags "Soak status unknown" for Phase 57b. The soak gate doc (`57b-soak-gate.md`) confirms the soak has NOT been run — the result table is empty (see below). This **confirms** the finding that soak status is unknown.
- The `55c-net-remote-rx-test-bug.md` open item in findings/05 and findings/07 is **CLOSED by Phase 57b**. This contradicts those findings' current open status for that item.

---

### docs/handoffs/57b-soak-gate.md

**What:** Phase 57b soak-gate procedure (Track H.4). Defines the 30-minute soak protocol, pass criteria, and result-tracking table.

**Phase tie:** Phase 57b.

**Closure status:** **Pending**. The result-tracking table at the bottom of the doc is empty — no date, no operator, no duration, no result. This is the definitive evidence that the Phase 57b 30-minute soak has never been run.

**New items the audit missed:**

- The result table being empty is the primary new finding. Phase 57b's own documentation says it is not fully closed until the soak runs.
- This doc is the soak-result document the audit said was missing for Phase 57b. It exists and explicitly records no results — the soak has not been performed.
- The doc says: "Do not skip this gate. Phase 57b is foundational for 57d (voluntary preemption) and 57e (full kernel preemption); a latent bug here will surface as a kernel deadlock the moment 57d's preemption begins firing inside a held lock."

**Items it confirms or contradicts from the audit:**

- findings/05: "Soak status unknown: The 'pending soak' qualifier has no corresponding closure document." This doc is the closure procedure document; it shows no results. The audit was correct that no result exists.
- **Phase 57b soak-result document status: FOUND (exists), result: NOT RUN (empty result table).**

---

### docs/handoffs/57c-busy-wait-audit.md

**What:** Phase 57c Track A audit catalogue. Classifies all 19 spin sites in `kernel/src/`: 0 convert (all already converted in 57a), 15 annotate (hardware/IPI-bounded), 4 leave (already documented from 57a).

**Phase tie:** Phase 57c (Kernel Busy-Wait Audit and Conversion).

**Closure status:** Complete. This is the durable audit artefact.

**New items the audit missed:**

- The audit (findings/05) listed the bounded-spin sites by file:line. This doc confirms those exact sites (with minor line-number drift). No gaps found — the audit's spin-site table matches this catalogue.
- `sys_nanosleep` < 1 ms TSC busy-spin is explicitly classified as **leave** (C.7): "Sub-millisecond spin is correct: a yield costs ~10 ms which is 10× the sleep duration. The 1 ms upper bound is enforced by the caller's branch condition." This is the definitive ruling on the < 1 ms branch. The ≥ 1 ms branch was converted to `block_current_until` in Phase 57a Track F.5.
- The `on_cpu` wait spin at `scheduler.rs:2699` is classified as **leave** (C.7): "The `on_cpu` flag is cleared with memory ordering guarantees on the next context switch out; converting to block+wake here is unsafe." This is the `wake_task_v2` cross-core spin; its correctness justification is here.

**Items it confirms or contradicts from the audit:**

- findings/05 listed the busy-wait sites. This catalogue is fully consistent. No contradictions.

---

### docs/handoffs/57c-validation-gate.md

**What:** Phase 57c validation gate. Records all primary acceptance criteria as met (✅) and secondary (E.1–E.3, real-hardware) as "Pending user validation."

**Phase tie:** Phase 57c.

**Closure status:** Primary criteria complete; secondary (real-hardware) pending.

**New items the audit missed:**

- E.1 (real-hardware graphical-stack regression), E.2 (30+30 min soak), E.3 (SSH disconnect/reconnect soak) are all "Pending user validation" with no filled results. These are the same gaps carried from Phase 57a's I.1/I.2/I.4.
- Kernel version bumped to 0.57.3 (confirmed by this doc). Not explicitly noted in audit.

**Items it confirms or contradicts from the audit:**

- findings/05 lists Phase 57c as Complete. This gate doc confirms primary criteria are met. Consistent.

---

### docs/handoffs/57d-graphical-boot-debugging.md

**What:** Extended debugging handoff for Phase 57d graphical boot. 16 entries covering: syscall-return preemption fix, virtio-blk scheduler-blocking single-flight request slot, Ion TLS fix, VFS readiness race, cooperative waitpid replacement, `ipc_wait_service` + `BlockedOnService`, prompt-ready gating, reply-cap encoding fix, terminal renderer stale-cell fix.

**Phase tie:** Phase 57d (Voluntary Preemption) post-merge debugging.

**Closure status:** Many items resolved; two leads still open at doc date:
1. Burst-time `CommitSurface` protocol failures (display_server client protocol violation).
2. Remaining virtio write timeout (`type=1`, sector 2072, owner pid 19) — not a graphical boot blocker but root cause unknown.

**New items the audit missed:**

- The following kernel-level fixes are in this doc but not in findings/01-07:
  - `ipc_wait_service` (`0x1115`) syscall and `BlockedOnService` scheduler state — new blocking service-readiness primitive. Not in audit.
  - `block_current_until` now disables preemption from the blocked-state write through `switch_context` — closes the preemptive mark-blocked-but-not-switched window. Not in audit.
  - Reply-wait flag registered on task before `BlockedOnReply` parking; `deliver_message`/`try_deliver_message` set it before `wake_task_v2`. Closes reply-before-park lost-wake race. Not in audit.
  - `Message::with_reply_cap_handle()` centralises reply-cap handle encoding; `call_msg`, `recv_msg`, `recv_msg_nowait`, `recv_msg_with_notif` all use it. VFS `vfs_server` hardcoded cap-slot-1 bug fixed. Not in audit.
  - PTY master/slave blocking-read `yield_now()` loops replaced with PTY wait-queue registration plus `block_current_until`. Not in audit.
- Two open issues from this doc that are new to the audit:
  1. Burst-time `CommitSurface` protocol violations — "display_server: client protocol violation; dropping message." Pattern: DamageSurface succeeds, following CommitSurface decoded as fatal under output bursts. Root cause unknown.
  2. Single-flight virtio-blk write timeout at sector 2072 still appears (Ion filesystem write). Not a current blocker but unresolved.
- The `term.prompt-ready` service and the syslogd/sshd gating on it are new architectural items not in the audit. `syslogd` and `sshd` now block on `term.prompt-ready` before boot-time persistent log/`/etc/ssh` setup.

**Items it confirms or contradicts from the audit:**

- findings/05 flags Phase 57d as "Planned" in design doc. This handoff doc shows extensive post-merge debugging and a substantial set of landed fixes — confirming 57d is implemented despite the Planned doc status.
- The `session_manager` F.4 `stop()` stubs are documented here as "LOGGING-ONLY stubs, no actual teardown" (consistent with the graphical-stack-startup handoff). Not addressed by any fix in this doc — still a stub.

---

### docs/handoffs/57e-bug9-bug10-followup.md

**What:** Open follow-up handoff for Bug #9 (IrqSafeMutex guard outliving `block_current_until`, causing preempt_count leak in FS-volume read paths) and Bug #10 (sporadic Doom-launch kernel-mode GPF at faulting RIP `0x4847474947479b46`).

**Phase tie:** Phase 57e Track G follow-ups.

**Closure status:** Both open.
- Bug #9: post-deferral severity adjusted to medium-low (no operational impact under voluntary preemption, but `[preempt] count=N at user-mode return — clamping to 0 (Bug #9 mitigation)` warnings still fire). Option B (Arc-clone) is the documented fix path.
- Bug #10: single observation, not reproduced; "watch-list" protocol once 50+ Doom launches clean.

**New items the audit missed:**

- Bug #9 (preempt_count leak in `FAT32_VOLUME`/`EXT2_VOLUME` read paths) is entirely new to the audit. The mechanism: any `IrqSafeMutex` guard outliving a `block_current_until` call leaves a net `+1` in `preempt_count` after the wake-protocol's `preempt_enable` runs. Worst case: `FAT32_VOLUME`/`EXT2_VOLUME` held across `kernel_read_fd_at` → `virtio_blk::do_request`. The clamp warning fires at user-mode return.
- Bug #9 fix approach (Option B): wrap volumes in `Arc`, clone the Arc inside the lock, drop guard before calling read methods. ~25 callsites identified. The step-by-step refactor is documented in precise detail including which sites to change and which to leave (TMPFS, `FAT32_PERMISSIONS`).
- Bug #10: confirmed single observation during Option A (`spin::Mutex` swap experiment) under preempt-full. The "GHIJK" register pattern is the ASCII hallmark of stack-corruption/wild-call. Post-deferral, `Option A` is not in the tree (FS mutexes reverted to `IrqSafeMutex`), so Bug #10's most likely root cause (preempt-full mid-critical-section preemption of a now-`spin::Mutex` holder) cannot recur under voluntary mode. But an independent latent stack-corruption cannot be ruled out without 50+ Doom launches on real hardware.
- `cargo xtask soak --duration 30m --max-runs 10` emits `target/soak/run-<ts>/soak-result.md`. This is a new soak harness command not mentioned in the audit.

**Items it confirms or contradicts from the audit:**

- findings/05 mentions "Four `TODO(57a-C/D)` pi_lock markers" — this handoff does not address those markers; they are different from the Bug #9 preempt_count issue.
- findings/05 mentions the phase 57e deferral and that "SMP discipline infrastructure survives." This doc confirms the preempt_count/IrqSafeMutex infrastructure is retained but notes the preempt_count *leak* in FS-volume paths is a distinct unresolved issue.

---

### docs/handoffs/57e-dispatch-reentrancy.md

**What:** Phase 57e Track A.2 audit of dispatch path reentrancy windows. Classifies each window in `pick_next`/`dispatch` by IF state and `preempt_count` safety. All windows are safe under `PREEMPT_FULL` as implemented.

**Phase tie:** Phase 57e.

**Closure status:** Landed alongside the 57e implementation branch. All windows classified as safe (three mechanisms: preempt_count > 0, IF = 0, or benign-preemption case).

**New items the audit missed:**

- The benign-preemption window post-`pick_next` / pre-`switch_context` is documented: if a kernel-mode preemption fires here, the chosen task is still in the Ready run queue (enqueue is idempotent). The double-pick pattern in trace logs is benign. Not in audit.
- Track J (XSAVE migration) — `fxsave64`/`fxrstor64` → `xsave64`/`xrstor64` — is referenced in this doc. The XSAVE migration was planned for 57e but is now moot given the 57e deferral. Two `#[ignore]` YMM-survives-yield stubs remain in `kernel/tests/xsave_avx.rs`. Not in audit.

**Items it confirms or contradicts from the audit:**

- findings/05 describes the 57e deferral. This doc is historical context from before the deferral decision. No contradictions.

---

### docs/handoffs/57e-kernel-preempt-audit.md

**What:** Phase 57e Tracks A.1, B.1, B.2, B.3. Second pass over the 57c busy-wait and 57b spinlock catalogues for PREEMPT_FULL safety. Adds `preempt_disable`/`preempt_enable` wrappers at 8 previously placeholder-comment-only sites. Per-CPU access pattern audit: 0 `needs-wrap` sites found.

**Phase tie:** Phase 57e.

**Closure status:** Landed alongside the 57e implementation branch. Given the 57e deferral and `preempt-full` removal, the Track B.2 wrappers added here are now either redundant (the `preempt-full` paths they protect are removed) or benign (they don't hurt under voluntary).

**New items the audit missed:**

- Track B.3 per-CPU access audit: 73 `per_core()` callsites; 0 `needs-wrap` found. The closest cases are `wrapped-already` via `scheduler_lock()` acquisition or IRQ context. The audit methodology (stored + used across statements + not already wrapped + core-specific) is documented for future reviewers. Not in audit.
- The B.2 wrappers added to `smp/ipi.rs:43-55`, `smp/boot.rs:267-283`, `apic.rs:434-447`, `iommu/amd.rs:329-355`, `iommu/intel.rs:241-260`, `intel.rs:362-380`, `intel.rs:382-402`, `rtc.rs:81-123`, `scheduler.rs:3200-3209`. These added `preempt_disable`/`preempt_enable` around hardware-wait spins. Given the preempt-full removal, these are now either removed (if inside a `cfg(feature="preempt-full")` block) or kept as defensive (if unconditional). The post-mortem's "Remove" list included `check_and_preempt_kernel` and associated wrappers. Whether these B.2 wrappers were inside those `cfg` gates needs verification if a future preemption phase lands.

**Items it confirms or contradicts from the audit:**

- findings/05 characterises the 57c annotation sites. This doc's per-CPU audit (B.3) adds new confidence that no untracked per-CPU access patterns exist. Consistent with but not contradicting the audit.

---

### docs/handoffs/57e-preempt-full-boot-crash.md

**What:** 57e Boot Crash Handoff — documents Bugs #1-#5 (early bring-up crashes) through the root-cause identification and resolution of Bug #2 (recursive `preempt_to_scheduler_kernel` with `sched_rsp == 0`), plus Bug #3 (per-core syscall snapshot aliased under mid-syscall preemption — fork-child user GPR corruption), Bug #4 (per-core `syscall_user_rsp` aliasing), Bug #5 (execve CR3 race).

**Phase tie:** Phase 57e (Bugs #1–5 history).

**Closure status:** All five bugs fixed in this session. Final state: `M3OS_KERNEL_FEATURES=preempt-full cargo xtask run` (60 s) — zero userspace page faults, all 17 services start cleanly. Given the 57e deferral, these fixes are in a branch that no longer ships `preempt-full`; their production relevance is the `TaskSyscallSnapshot` per-task GPR snapshot refactor (which is preempt-model-independent and addresses a real design flaw).

**New items the audit missed:**

- Bug #3's root cause (per-core `syscall_user_*` slots in `PerCoreData` aliased across tasks under mid-syscall kernel preemption) is a design flaw that exists independently of `preempt-full`. Under voluntary preemption a mid-syscall preemption cannot occur — but this explains *why* the per-core snapshot design is fragile. The fix (`TaskSyscallSnapshot` per-task struct) is correct regardless of preemption model. Not in audit.
- Bug #5 (execve CR3 race: `set_current_user_return` must happen BEFORE `Cr3::write`, not after) is a preempt-full-specific race but represents a latent ordering assumption. Not in audit.
- The `preempt-full` smoke-test timeout issue documented here: per-step timeouts in the smoke fixture (auth=10s, tcc-version=30s) are tuned for voluntary mode pace and don't tolerate preempt-full overhead. This is a tooling gap, not a correctness issue. Not in audit.
- PR #136 is referenced as the phase-57e implementation branch. The audit's findings/05 validation note says "PR #136 (`ad7d9b2`) squash-merged." This handoff confirms PR #136 is the correct reference.

**Items it confirms or contradicts from the audit:**

- findings/05 validation note references PR #136 and the preempt-full deferral. This handoff is pre-deferral but records the same bugs and fixes. Consistent.

---

### docs/handoffs/57e-preempt-full-userspace-hangs.md (not directly read — 2669 lines)

**What:** 18-session bug-analysis log for Phase 57e. Preserved as historical reference. The post-mortem and the `57e-bug9-bug10-followup.md` handoff extract the actionable items from it.

**Phase tie:** Phase 57e.

**Closure status:** Historical reference document. Per the post-mortem action items, a header was added pointing to the post-mortem and noting the outcome. The body is preserved for any future Option 3 (`cond_resched`) attempt.

**New items the audit missed:**

- Not directly read per task instructions (partially summarised in findings/05). The post-mortem's action item `[x] Add a header to docs/handoffs/57e-preempt-full-userspace-hangs.md pointing to this post-mortem` was done. The audit's findings/05 characterisation of the 57e deferral is corroborated by all the other docs in this set.

**Items it confirms or contradicts from the audit:** The post-mortem and bug9-bug10-followup provide sufficient coverage. No new contradictions identified.

---

### docs/handoffs/57a-scheduler-rewrite-call-sites.md

**What:** Phase 57a Track A.1 complete inventory of all `block_current_unless_woken*` and `wake_task`/`scan_expired_wake_deadlines` callsites. Maps each to a Track F migration sub-task. Status: Complete.

**Phase tie:** Phase 57a.

**Closure status:** Complete. All sites migrated (Track F.7 deletion of v1 functions confirms no sites remain).

**New items the audit missed:**

- `sys_nanosleep` note: at audit time `sys_nanosleep` did NOT call any block_current primitive — the ≥ 5 ms branch was a TSC busy-spin with `yield_now()`. Track F.5 migrated the ≥ 1 ms branch to `block_current_until`. This is consistent with the 57c audit's "leave" ruling for < 1 ms.

**Items it confirms or contradicts from the audit:** Reference artifact. No contradictions.

---

### docs/handoffs/57a-scheduler-rewrite-v1-transitions.md

**What:** Phase 57a Track A.2 v1 block/wake transition table. Documents the lost-wake cells and correct cells in the v1 protocol. Regression test contract for v2 rewrite.

**Phase tie:** Phase 57a.

**Closure status:** Complete. Used as test matrix for Track A.5 (host tests).

**Items it confirms or contradicts from the audit:** Reference artifact. The specific v1 race conditions documented here match the scheduler-design-comparison doc analysis. Consistent.

---

### docs/handoffs/57a-scheduler-rewrite-v2-transitions.md

**What:** Phase 57a Track A.3 v2 block/wake transition table. Spec for Track A.4's `apply_event` function, Tracks C and D's new primitives, Tracks E and F's field removals and call-site migrations.

**Phase tie:** Phase 57a.

**Closure status:** Complete. All v2 cells implemented; v1 fields deleted.

**Items it confirms or contradicts from the audit:** Reference artifact. No contradictions.

---

### docs/handoffs/57b-spinlock-callsite-audit.md

**What:** Phase 57b Track A.1 audit of all spinlock callsites in `kernel/src/` and `kernel-core/src/`. 65 lock declarations classified across four categories: already-irqsafe, convert-to-irqsafe, explicit-preempt-and-cli, host-test-only. Maps to Track G owners (G.1–G.9).

**Phase tie:** Phase 57b.

**Closure status:** Complete. 65 locks migrated.

**New items the audit missed:**

- `WaitQueue::waiters` (discovered during G.7) was a 66th lock found during implementation — not in the initial audit count of 64. Total migrated: 65. This is a minor reconciliation.
- The classification "explicit-preempt-and-cli" documents that `without_interrupts()` alone is NOT sufficient for ISR-shared locks — both explicit `preempt_disable`/`preempt_enable` AND `without_interrupts` are required. The current `DRIVER.lock` wrappers in virtio_net and virtio_blk already follow this pattern; the audit formalises it as a classification rule.

**Items it confirms or contradicts from the audit:** Reference artifact. No contradictions.

---

### docs/debug/54-followups.md

**What:** Phase 54 follow-up routing summary. Two remaining long-term backlog items: (1) `MOUNT_OP_LOCK` should be replaced with a yielding primitive; (2) scheduler diagnostic thresholds (stale-ready 500 ms, cpu-hog 200 ms) should be tuned with baseline data.

**Phase tie:** Phase 54 (Deep Serverization).

**Closure status:** Open. Items 1 and 2 have no owners. Items 3-7 from the original list are routed or complete.

**New items the audit missed:**

- Item 1 (`MOUNT_OP_LOCK`: `spin::Mutex<()>` at `kernel/src/arch/x86_64/syscall/mod.rs:94`): the fix note says "after PR #108, the lock is only held around the mount/umount mutation itself — path resolution runs outside it — so 'sleep while holding spinlock' is no longer reachable." The remaining concern is two cores racing on the lock still busy-spin in ring 0. No owner, not hot in practice.
- Item 2 (scheduler diagnostic thresholds): "If it becomes noise over day-to-day use, raise to 50 ticks (≈ 500 ms) so only genuine hangs fire." With Phase 57a's pi_lock / v2 protocol landing, these thresholds may now fire more cleanly on real issues. But the recommended change is "no code change unless the noise becomes a problem."
- The audit cites `54-followups.md` several times but never read it. The items it flags are genuinely minor backlog items that do not materially affect the audit findings. The `MOUNT_OP_LOCK` busy-spin is a bounded, rare, non-hot-path spin; it was explicitly acknowledged as acceptable.

**Items it confirms or contradicts from the audit:**

- The audit (findings/07) mentions `copy_to_user` SMP TLB audit and other Phase 52 items. `54-followups.md` does not address those items (different phase). No contradiction.
- findings/04 would cover Phase 54 (Deep Serverization). The two items here (MOUNT_OP_LOCK, threshold tuning) are low-priority backlog consistent with that characterisation.

---

## Cross-cutting findings

### Audit residuals these docs CLOSE (with closure refs)

| Audit finding (findings/NN) | Closed by | Closure ref |
|---|---|---|
| `55c-net-remote-rx-test-bug.md` open (findings/05, findings/07) | Phase 57b Track G `fix(net::remote)` | 57b-batch-summary: "Three RX-path tests corrected ... `drain_rx_queue_removes_malformed_frames_after_deferred_queueing` shrunk ... stale assertion removed." |
| Phase 57a design doc "Planned" status (findings/05) | Implementation complete per 57a-batch-summary | PR #129; "64 commits total on the feature branch"; all 18 tracks merged; 1368 lib tests pass |
| Five 100 Hz tick-multiplier bugs open (findings/05) | Phase 57a Track G.3 | 57a-batch-summary: "G.3 Sweep stale 100 Hz tick-multiplier assumptions: 5 sites fixed" (Wave 2 merge) |
| audio_server exits without registering audio.cmd (2026-04-28 graphical-stack handoff, confirmed open) | Phase 57a Track H.2 | 57a-batch-summary: commit `557bbd5` "audio_server stub when no AC'97" |
| serial_stdin_feeder_task halt-loop parking core 3 (2026-04-28 graphical-stack handoff) | Phase 57a Track H.1 | 57a-batch-summary: "serial_stdin_feeder_task → notification wait ... Fixes kbd_server-on-AP3" |
| syslogd cpu-hog ~500 ms (2026-04-28 graphical-stack handoff, findings/07) | Phase 57a Track H.3 | 57a-batch-summary: commit `5a79866` "syslogd cpu-hog: dual root cause + drain-chunk fix" |

### Audit residuals these docs CONFIRM as still open

| Residual | Confirming doc(s) | Status |
|---|---|---|
| Phase 57b 30-minute soak not run | 57b-soak-gate.md (empty result table), 57b-batch-summary.md ("unresolved follow-ups: 30-minute soak gate") | Still open |
| `sys_nanosleep` long-sleep second-wake bug (nanosleep busy-yield / `block_current_unless_woken_until`) | 2026-04-24-ingress-task-starvation.md (Item 1 "still open"), 2026-04-25-pr-118-residual-issues.md (Issue 2), 2026-04-25-pr-118-residual-issues-update.md | Still open; the v2 wake protocol (Phase 57a) eliminates the lost-wake class but whether the ≥ 5 ms nanosleep branch now correctly uses `block_current_until` without second-wake failures requires verification |
| Phase 57a I.2 (SSH disconnect/reconnect 50-cycle soak) pending | 57a-validation-gate.md (⬜ Pending user run) | Still open |
| Phase 57a I.4 (60-minute long soak) pending | 57a-validation-gate.md (⬜ Pending user run) | Still open |
| Phase 57c E.1/E.2/E.3 (real-hardware validation) pending | 57c-validation-gate.md | Still open |
| Four `TODO(57a-C/D)` pi_lock markers (findings/05) | No handoff doc addresses these | Still open |
| SSH disconnect hang (sshd-child silently stops executing) | 2026-04-25-pr-118-residual-issues.md, 2026-04-25-pr-118-residual-issues-update.md | Status after Phase 57a's `scan_expired_wake_deadlines` migration (Track D.4) is unknown; the underlying scheduler issue was the root cause but no doc confirms the hang is resolved |
| Bug #9 IrqSafeMutex preempt_count leak in FS-volume read paths | 57e-bug9-bug10-followup.md | Open; `[preempt] count=N at user-mode return — clamping to 0 (Bug #9 mitigation)` warnings still fire |
| Bug #10 sporadic Doom-launch kernel-mode GPF | 57e-bug9-bug10-followup.md | Open; single observation, not reproduced; watch-list |
| `serverization-fallback` ~40% flakiness | 2026-04-24-ingress-task-starvation.md, 2026-04-25-pr-118-residual-issues.md | Status post-Phase-57a unknown |
| `session_manager` F.4 stop() stubs are logging-only (no actual service teardown) | 2026-04-28-graphical-stack-startup.md, 57d-graphical-boot-debugging.md | Still open |
| virtio-blk single-flight write timeout at sector 2072 | 57d-graphical-boot-debugging.md | Still open; not a current graphical boot blocker but root cause unknown |
| Burst-time `CommitSurface` protocol failures under output bursts | 57d-graphical-boot-debugging.md | Still open |

### NEW gaps not captured anywhere in findings/01-07

1. **`TIOCSCTTY` correctness fix** — `slave_fg_pgid` not set in the TIOCSCTTY ioctl handler before Phase 55c; `close_master` SIGHUP-to-fg-pgrp delivery was a no-op. Fixed in `2026-04-25-pr-118-residual-issues-update.md` commit (no SHA cited). `set_slave_fg_pgid` helper added to `kernel/src/pty.rs`. Not in any finding.

2. **`RAW_INPUT_ROUTER` ISR-unsafe lock** — second spinlock found unsafe (in both ISR and task context without IF masking) during the Phase 55b global lock audit (`c519a60`). Fixed in the same commit. Not in the audit's finding of the SCHEDULER.lock issue.

3. **`cargo xtask ssh-regression --count N` follow-up** — `scripts/ssh-wedge-regression-batch.sh` promoted in-tree but not wired into xtask. A dedicated xtask subcommand is a listed follow-up with no owner.

4. **`ipc_wait_service` syscall (0x1115) and `BlockedOnService` scheduler state** — new blocking service-readiness primitive added in Phase 57d graphical-boot debugging. Clients can wait for a named service to register without receiving a callable capability. Not in audit.

5. **Reply-wait flag on task before `BlockedOnReply`** — `deliver_message`/`try_deliver_message` now set this flag before `wake_task_v2`. Closes reply-before-park lost-wake race. Not in audit.

6. **`Message::with_reply_cap_handle()` and VFS hardcoded cap-slot-1 bug** — VFS `vfs_server` was replying through hardcoded cap slot 1; fixed by centralising reply-cap handle encoding into `Message::with_reply_cap_handle()` applied in `call_msg`/`recv_msg`/`recv_msg_nowait`/`recv_msg_with_notif`. Not in audit.

7. **`term.prompt-ready` service gate** — `syslogd` and `sshd` now block on `term.prompt-ready` before boot-time persistent log/`/etc/ssh` setup. Architecture change not in audit.

8. **Bug #9: `IrqSafeMutex` preempt_count leak in FS-volume read paths** — `FAT32_VOLUME`/`EXT2_VOLUME` guards outlive `block_current_until` calls, leaving net `+1` in `preempt_count` per blocking disk read. Clamp warning: `[preempt] count=N at user-mode return — clamping to 0 (Bug #9 mitigation)`. Option B (Arc-clone) fix is documented but not landed. Not in audit.

9. **PTY master/slave blocking-read `yield_now()` loops replaced with wait-queue registration** — `block_current_until` used instead of yield loops. Not in audit.

10. **`block_current_until` now disables preemption from blocked-state write through `switch_context`** — closes the preemptive mark-blocked-but-not-switched window. Not in audit.

11. **virtio-input migration plan** — planned migration from PS/2 to virtio-input to fix QEMU i8042 kbd-priority arbitration; documented in `docs/handoffs/2026-05-04-virtio-input-migration.md`; no implementation started. Diagnostic scaffolding (`sys_ps2_diag_counter` syscall 0x101E, `MOUSE_BYTES_SEEN`/`IRQ1_ENTRIES` counters) added in commits `d2f6978` and `7b305c7` — should be stripped once virtio-input is verified. Not in audit.

12. **SSH disconnect hang (sshd-child silently stops executing mid-syscall-loop)** — distinct from the ISR deadlock. After `@A2` write succeeds, sshd-child enters Blocked or descheduled state with no scheduler warning. Diagnosed as the `switching_out`/`wake_after_switch` lost-wake family; the Phase 57a v2 protocol was the intended fix. Whether the v2 fix resolved this specific instance is unconfirmed. Not in audit.

13. **`exec_path.starts_with("/drivers/")` authorization model is path-prefix-only** — no capability-table depth. Listed in findings/07 but source is `pr-116-review.md`. `54-followups.md` has no routing for this; it predates Phase 55b. Finding is in the audit but not connected to a resolution path.

14. **`MOUNT_OP_LOCK` busy-spin backlog item** — `spin::Mutex<()>` at `kernel/src/arch/x86_64/syscall/mod.rs:94` causes ring-0 busy-spin on mount/umount contention. Low priority, no owner. `54-followups.md` item 1.

15. **TaskSyscallSnapshot per-task GPR snapshot (Bug #3 fix)** — per-core `syscall_user_*` slots in `PerCoreData` aliased across tasks under mid-syscall kernel preemption. Fixed by moving `TaskSyscallSnapshot` into `Task` struct. The design flaw exists independently of preempt-full; it would manifest if any future preemption model allows mid-syscall kernel preemption. Landed on 57e branch. Whether it survived the preempt-full removal depends on whether the fix was inside a `cfg(preempt-full)` gate. Not in audit.

---

### Phase 57b soak-result document — FOUND, result: NOT RUN

The document `docs/handoffs/57b-soak-gate.md` is the soak-result document the audit said was missing. It exists. Its result-tracking table is empty — no date, no operator, no duration, no result filled in. **The Phase 57b 30-minute soak has never been run.** The audit correctly identified that no soak result document existed; this finding updates that to: the document exists but contains no results.

The doc states explicitly: "Do not skip this gate. Phase 57b is foundational for 57d (voluntary preemption) and 57e (full kernel preemption); a latent bug here will surface as a kernel deadlock the moment 57d's preemption begins firing inside a held lock."

Given that Phase 57d was implemented (per the 57d-graphical-boot-debugging handoff showing extensive landed fixes) and Phase 57e was attempted and then deferred, and neither step produced a deadlock attributed to Phase 57b's preempt_count discipline, the soak is likely to pass if run — but it has not been run and the gate is formally still open.
