# Phase 62 Track E.2 — Pi-Lock Closeout 30-Minute Soak

**Status:** Pending — operator runs the soak after PR #146 merges (or before, in a clean checkout of `feat/phase-62-pi-lock-closeout`).
**Source Ref:** phase-62-track-E.2
**Depends on:** Phase 62 Tracks A, B, C, D, E.1 ✅ (all merged into `feat/phase-62-pi-lock-closeout`).
**Cross-references:** `docs/handoffs/57b-soak-gate.md` (canonical soak procedure inherited from Phase 57b Track H.4 / Phase 59 Track G).

## Purpose

Phase 62 Tracks B and D add small but invariant-relevant changes:

- Track B introduces `Task::with_block_state_locked_scheduler` and applies it at four sites in `kernel/src/task/scheduler.rs` (sites 892, 4108, 4120, 4319). One of those sites is the dispatch hot path (every context switch goes through it), so any defect would manifest under sustained load.
- Track D adds host-side regression tests but does not modify any kernel runtime code.

The 30-minute soak is the runtime confirmation that Track B's scheduler-internal changes do not introduce panics, deadlocks, or scheduler regressions under sustained multi-core load. Phase 62 Track E.2 reuses the canonical Phase 57b Track H.4 / Phase 59 Track G soak procedure verbatim so the result is directly comparable to any future baseline.

The same soak run also satisfies Phase 59 Track G's deliverable (populate the `docs/handoffs/57b-soak-gate.md` Result-tracking table). One run, two phase-closure obligations.

## Procedure

Run on a developer machine — not in CI; 30 minutes is too long for the default CI budget. Use a clean checkout of `feat/phase-62-pi-lock-closeout` (or `main` after merge) at the integration commit (Phase 62 closure SHA, post-merge of all Track B + Track D commits).

```bash
cargo xtask run-gui --fresh
```

Once the GUI session is up and the desktop has settled, drive synthetic load on **≥ 4 cores** for **30 minutes** wall-clock:

- IPC stress: ≥ 8 long-running clients and ≥ 4 servers exchanging bound notifications and synchronous calls in tight loops.
- Futex stress: ≥ 4 threads doing paired futex wait/wake on shared addresses.
- Notification stress: ≥ 4 producers signalling kernel `Notification` objects that ≥ 4 consumers wait on.

If the repository ships a soak harness (look under `userspace/` for a binary named `soak`, `stress`, or similar), prefer it. Otherwise spawn the shells manually inside the GUI session — `docs/handoffs/57b-soak-gate.md` § Procedure documents the exact commands.

While the soak runs, in a separate terminal tail the serial log:

```bash
tail -f target/m3os.log 2>/dev/null || journalctl -f
```

(Adjust to wherever the QEMU `-serial` redirects on this machine.)

## Pass criteria

The soak passes iff **all four** hold for the full 30 minutes:

- [ ] **Zero panics** from the user-mode-return debug assertion. The panic message contains the literal substring `preempt_count != 0 at user-mode return`. **A single occurrence fails the gate.**
- [ ] **No new `[WARN] [sched]` lines** that did not appear in the Phase 59 Track G baseline (or, if the baseline has not yet been captured, in a clean pre-Phase-62 boot log of the same workload).
- [ ] **No deadlocks.** The GUI continues to respond to input throughout (the stuck-task watchdog from Phase 57a would print warnings if a kernel task wedged for > 5 s).
- [ ] **Clean shutdown.** The session terminates cleanly via `poweroff` or `Ctrl-A x` from the QEMU monitor.

## Result tracking

After running the gate, append a row to **both** of the following tables on a follow-up branch named `docs/62e-pi-lock-soak-result`:

### `docs/handoffs/62e-pi-lock-soak.md` (this file)

| Date | Operator | Duration | Result | Notes |
|------|----------|----------|--------|-------|
|      |          |          |        |       |

### `docs/handoffs/57b-soak-gate.md` Result-tracking table (also satisfies Phase 59 Track G)

The same row format. Reference Phase 62 Track E.2 in the Notes column so the row's dual purpose is recorded.

## Failure handling

If the gate fails:

1. Capture the full serial log and any panic backtrace.
2. File a regression note in `docs/handoffs/` describing the symptom and the failing pass criterion.
3. Bisect against the Phase 62 commits (Track B at `a1d3286`, Track D at `89e38d1`, plus subsequent Track F/G commits) to identify the responsible commit.
4. Most likely culprits, ordered by audit risk:
   - **Site 4 (dispatch hot path) regression.** Site 4 is on every context switch; any wake-side race that the structural-safety NOTE failed to anticipate would manifest here first. Re-check the NOTE's claim that `wake_task_v2`'s CAS only ever transitions `Blocked* → Ready` and never `Ready → Running`.
   - **Site 1 (queue-scan) regression.** The defensive cleanup runs once per scheduling tick; a regression here would manifest as a ~100 ms-latency anomaly in cooperative yield response.
   - **Sites 2/3 (test scaffolding).** These run only under `#[cfg(test)]`; soak failures are unlikely to point here. If the soak fails on a debug-build kernel test binary path (which it shouldn't — the soak runs the production kernel), check whether the test scaffolding leaked into the release build.
   - **Helper signature drift.** The new `Task::with_block_state_locked_scheduler` is byte-identical to `Task::with_block_state` minus the lock-ordering debug assertion. Verify both helpers exist and have not converged or diverged unexpectedly.

## When to skip

Do not skip this gate. Phase 62's scheduler-internal changes (especially Site 4 on the dispatch hot path) are exactly the class of change that benefits from sustained multi-core load validation; the test suite cannot exercise the same deferred-reschedule windows.

## References

- `docs/handoffs/57b-soak-gate.md` — canonical procedure (this file inherits all four pass criteria + the Procedure / Failure-handling steps).
- `docs/handoffs/62a-pi-lock-inventory.md` — Track A inventory (per-site lock-context summary the soak would surface a defect against).
- `docs/roadmap/62-phase-57a-pi-lock-closeout.md` — Phase 62 design doc (§ Track E).
- `docs/roadmap/tasks/62-phase-57a-pi-lock-closeout-tasks.md` — Phase 62 task list (§ Track E.2).
