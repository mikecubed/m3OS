# Phase 57a — Validation Gate

This document records the validation procedure for Phase 57a's scheduler
block/wake protocol rewrite.  Tests I.1, I.2, and I.4 are user-driven; results
are recorded inline below.  Test I.3 is automated; the CI procedure is included
for reference.

---

## I.1 — Real-hardware graphical stack regression

**Why this matters:**
The 2026-04-28 cursor-at-(0,0) regression is the primary user-facing acceptance
test.  Before the Phase 57a rewrite, `display_server` and `mouse_server` could
race at startup on a multi-core machine, leaving the cursor stuck at the origin.
The v2 `wake_task_v2` CAS protocol (Track D) eliminates this race.

**Procedure:**

1. On your test hardware, clean the disk image so UEFI reinitialises all
   services:
   ```
   cargo xtask clean
   ```

2. Launch the graphical session:
   ```
   cargo xtask run-gui --fresh
   ```

3. After the framebuffer console appears (the `m3os` boot banner must scroll
   past), perform the following checks within 30 seconds of the first rendered
   frame:

   a. **Cursor motion** — move the mouse.  The cursor must begin moving within
      1 second of the first mouse event.  A cursor frozen at (0, 0) is a
      regression.

   b. **Keyboard input** — type any character in the terminal.  The character
      must appear in the framebuffer terminal within 100 ms of the keypress.

   c. **Term ready** — in the QEMU serial console (or the GUI terminal), confirm
      that the log line `TERM_SMOKE:ready` appears within 10 seconds of boot.

   d. **No stuck-task warnings** — confirm that NO line matching the pattern
      `[WARN] [sched]` appears in the serial log during the first 60 seconds of
      runtime.  Use:
      ```
      cargo xtask run-gui --fresh 2>&1 | grep '\[WARN\] \[sched\]'
      ```
      Zero matches = pass.

4. Repeat steps 2–3 five times (rebooting between each run).  The placement of
   `display_server`, `kbd_server`, and `mouse_server` across APs varies between
   boots; all five runs must pass.

**Acceptance criteria:**
- [ ] 5 / 5 boots: cursor moves within 1 s of first motion event.
- [ ] 5 / 5 boots: keyboard echoes within 100 ms.
- [ ] 5 / 5 boots: `TERM_SMOKE:ready` appears in the log.
- [ ] 5 / 5 boots: zero `[WARN] [sched]` stuck-task lines in the first 60 s.

**What to capture if a run fails:**
- Full serial log of the failing boot (`cargo xtask run-gui 2>&1 | tee boot-fail.log`).
- The `[WARN] [sched]` line(s) with timestamps and the task names preceding them.
- CPU core assignment of `display_server` / `mouse_server` at the time of
  failure (look for `[INFO] [sched] dispatch task=display_server core=N` lines).
- File a bug against `kernel/src/task/scheduler.rs` (`wake_task_v2`) if the
  stuck-task is any of `display_server`, `mouse_server`, or `kbd_server`.

**Status:** ⬜ Pending user run.
**Last run:** (date — to be filled in by user)
**Result:** (pass / fail — to be filled in by user)
**Notes:** (observations — to be filled in by user)

---

## I.2 — SSH disconnect/reconnect soak

**Why this matters:**
The 2026-04-25 SSH cleanup hang was the second user-facing acceptance test.
After a client disconnected, the `sshd` session task could block forever in
`block_current_until` waiting for a reply that was never coming (because the
IPC endpoint had been torn down).  The v2 `scan_expired_wake_deadlines` path
(Track D.4) wakes timed-out blocked tasks even if no explicit `wake_task_v2`
is called.

**Procedure:**

1. Launch the full OS with e1000 networking:
   ```
   cargo xtask run --device e1000 --fresh
   ```
   Wait for `sshd: listening on 0.0.0.0:22` in the serial log.

2. From the host, run 50 consecutive SSH connect/disconnect cycles.  Use the
   provided script, or adapt it to your SSH client:
   ```bash
   for i in $(seq 1 50); do
       ssh -o StrictHostKeyChecking=no \
           -o ConnectTimeout=5 \
           -p 2222 root@localhost \
           "echo cycle $i; sleep 0.2" 2>/dev/null
       echo "cycle $i done"
   done
   ```
   Adjust the port (`-p 2222`) to match your QEMU hostfwd mapping.

3. After all 50 cycles complete, confirm:
   a. The QEMU guest is still responsive — type a command in the serial console
      and confirm a response arrives within 5 seconds.
   b. No `[WARN] [sched]` stuck-task lines appeared during the soak.
   c. No `[WARN] [sched] cpu-hog` line with `ran > 200 ms` appeared.

4. Optional extended soak: run 200 cycles with 100 ms sleep between each.
   The scheduler watchdog (`G.1`) fires at 5-second intervals; any task stuck
   longer than 5 seconds will produce a `[WARN] [sched]` line.

**Acceptance criteria:**
- [ ] 50 consecutive SSH cycles complete without a scheduler hang.
- [ ] Serial console remains responsive after all 50 cycles.
- [ ] Zero `[WARN] [sched]` stuck-task lines during the soak.
- [ ] Zero `[WARN] [sched] cpu-hog` lines with `ran` field > 200 ms.

**What `[WARN] [sched]` lines look like:**
```
[WARN] [sched] stuck task: pid=42 name=sshd state=BlockedOnReply deadline=Some(38000) now=45000
[WARN] [sched] cpu-hog: pid=7 name=vfs_server ran=312ms slice=100ms
```
Any such line during the soak is a failure.  Capture:
- The full line including `pid=`, `name=`, `state=`, and timestamp.
- The last 200 lines of serial output before and after the line.
- File a bug against `kernel/src/task/scheduler.rs` (`scan_expired_wake_deadlines`
  or `wake_task_v2`) depending on whether the task is stuck-in-block or
  stuck-running.

**Status:** ⬜ Pending user run.
**Last run:** (date — to be filled in by user)
**Result:** (pass / fail — to be filled in by user)
**Notes:** (observations — to be filled in by user)

---

## I.3 — Multi-core in-QEMU fuzz

**Why this matters:**
Property-based host tests (`sched_fuzz_multicore`) exercise the v2 model in
isolation.  This test exercises the same `kernel_core::sched_model` state
machine compiled for the `x86_64-unknown-none` kernel target, confirming that
the QEMU build target produces correct results.

**Two-layer approach:**

| Layer | Command | Duration | Coverage |
|---|---|---|---|
| Model-level property fuzz | `cargo test -p kernel-core ... -- sched_fuzz_multicore` | < 30 s | 5 000 random 16-task × 32-action rounds |
| In-QEMU smoke | `cargo xtask test --test sched_fuzz` | < 60 s | 4 deterministic 16-task scenarios |

**Procedure (model-level):**
```bash
cargo test -p kernel-core --target x86_64-unknown-linux-gnu \
    -- sched_fuzz_multicore
```
Expected output: all tests pass, zero failures.

For the full 5-minute-equivalent depth:
```bash
PROPTEST_CASES=100000 cargo test -p kernel-core \
    --target x86_64-unknown-linux-gnu \
    -- sched_fuzz_multicore
```

**Procedure (in-QEMU):**
```bash
cargo xtask test --test sched_fuzz
```
Default QEMU timeout is 60 s.  Extend for a longer run:
```bash
cargo xtask test --test sched_fuzz --timeout 120
```

**Acceptance criteria:**
- [ ] Model-level: all `sched_fuzz_multicore` tests pass with 0 failures in 5 000 cases.
- [ ] In-QEMU: `cargo xtask test --test sched_fuzz` exits with QEMU code 0x21 (success).
- [ ] No hang, no panic, no `[WARN] [sched]` stuck-task line, no `cpu-hog` line with `ran > 200 ms`.

**Current status:** ✅ Model-level 5 000-case run passes (see CI).
In-QEMU smoke: ⬜ pending full QEMU build in CI.

**What to look for in QEMU output:**
- `Test sched_fuzz: PASSED` printed by xtask.
- QEMU exit code `0x21` (xtask maps ISA debug exit value 0x10 → OS exit 0x21).
- Failure indicators: `Test sched_fuzz: FAILED`, QEMU exit code `0x23`, timeout.

---

## I.4 — Long-soak (idle + load, 60 minutes)

**Why this matters:**
Lost-wake bugs are timing-dependent.  A 60-minute soak at realistic workload
gives statistical confidence that the bug is not merely shifted to a longer
inter-event window.  The Phase 57a rewrite eliminates the race at the
state-machine level; this test verifies that no other scheduling path reintroduces it.

**Procedure:**

1. Clean and launch the full OS (4 vCPUs for SMP coverage):
   ```
   cargo xtask run-gui --fresh
   ```
   Confirm 4 cores boot: look for `AP core 1/2/3 ready` in the serial log.

2. **Idle phase (30 minutes)** — leave the OS running with no user interaction.
   Monitor the serial console for `[WARN] [sched]` lines:
   ```bash
   cargo xtask run-gui --fresh 2>&1 | tee soak-idle.log &
   sleep 1800
   grep '\[WARN\] \[sched\]' soak-idle.log | wc -l
   ```
   Zero matches = pass for the idle phase.

3. **Load phase (30 minutes)** — apply synthetic load by running the built-in
   benchmark utilities from the guest shell:
   ```
   # In the guest serial console or SSH session:
   /bin/ping 10.0.2.2 &      # network load (if e1000 device present)
   /bin/cat /dev/zero > /dev/null &   # VFS + IPC load
   ```
   Or use the `stress` xtask if available:
   ```bash
   cargo xtask stress --test bound_recv --iterations 10000
   ```
   Monitor for `[WARN] [sched]` lines and `cpu-hog` lines throughout.

4. After 60 minutes, check:
   a. Zero `[WARN] [sched]` stuck-task lines in `soak-idle.log`.
   b. Zero `[WARN] [sched] cpu-hog` lines with `ran > 200 ms` in either phase.
   c. The OS is still responsive: type a command in the serial console and
      confirm a response within 5 seconds.

**Acceptance criteria:**
- [ ] 30-minute idle phase: zero `[WARN] [sched]` stuck-task lines.
- [ ] 30-minute load phase: zero `[WARN] [sched]` stuck-task lines.
- [ ] Neither phase produces a `[WARN] [sched] cpu-hog` line with `ran > 200 ms`.
- [ ] OS remains responsive at the end of the 60-minute soak.

**What `cpu-hog` lines look like and what they mean:**
```
[WARN] [sched] cpu-hog: pid=12 name=term ran=312ms slice=100ms
```
A `ran` value > 200 ms means a task ran continuously for > 200 ms without
yielding.  Values under 200 ms are normal scheduling jitter; values above
indicate either a busy-poll loop that should use `block_current_until` or a
scheduling primitive that failed to preempt.  Note: the kernel timer interrupt
fires at 100 Hz; a task that misses two consecutive timer ticks (> 20 ms) before
a voluntary yield will log a cpu-hog warning.  The 200 ms threshold is the
Phase 57a acceptance threshold.

**What to capture if the soak fails:**
- The first `[WARN] [sched]` or `cpu-hog` line with its timestamp.
- The 100 lines of serial output before and after the warning.
- Which phase (idle vs load) produced the first failure.
- `uptime` or tick count at the time of failure (look for `[tick=NNNNNN]` log
  prefix).
- File a bug against `kernel/src/task/scheduler.rs` with the log excerpt.

**Status:** ⬜ Pending user run.
**Last run:** (date — to be filled in by user)
**Idle phase result:** (pass / fail — to be filled in by user)
**Load phase result:** (pass / fail — to be filled in by user)
**Notes:** (observations — to be filled in by user)

---

## Summary

| Test | Type | Automated? | Status |
|---|---|---|---|
| I.1 — Real-hardware graphical regression | Procedural, 5× repeat | No | ⬜ Pending |
| I.2 — SSH disconnect/reconnect soak | Procedural, 50 cycles | Semi (script provided) | ⬜ Pending |
| I.3 — Multi-core model fuzz (5 000 cases) | `cargo test -p kernel-core` | Yes | ✅ Passes |
| I.3 — Multi-core in-QEMU smoke | `cargo xtask test --test sched_fuzz` | Yes | ⬜ Pending QEMU run |
| I.4 — Long-soak (60 min idle + load) | Procedural | No | ⬜ Pending |

Phase 57a is considered complete when all five rows above show ✅.
