# Phase 57b — Soak-Gate Procedure (Track H.4)

**Status:** Pending — run after PR #132 merges to mark Phase 57b fully closed.
**Source Ref:** phase-57b-track-H.4
**Depends on:** Phase 57b Tracks A through H.3 merged on `main`.

## Purpose

Phase 57b is a no-op refactor: every kernel-side spinlock callsite now
cycles `preempt_count` exactly once on acquire and release, and every
user-mode-return path debug-asserts the count is zero. The soak gate is
the runtime confirmation that the refactor is genuinely no-op — that is,
that the new discipline does not introduce panics or scheduler regressions
under sustained multi-core load.

## Procedure

Run on a developer machine (not CI — 30 minutes is too long for the
default CI budget) against a clean checkout of `main` with the merged
57b commits.

```bash
cargo xtask run-gui --fresh
```

Once the GUI session is up and the desktop has settled, drive synthetic
load on 4 cores for **30 minutes**:

- IPC stress: spawn ≥ 8 long-running clients and ≥ 4 servers exchanging
  bound notifications and synchronous calls in tight loops.
- Futex stress: spawn ≥ 4 threads doing paired futex wait/wake on
  shared addresses.
- Notification stress: ≥ 4 producers signalling kernel
  `Notification` objects that ≥ 4 consumers wait on.

If the repository ships a soak harness (look under `userspace/` for a
binary named `soak`, `stress`, or similar), use it. Otherwise spawn the
shells manually inside the GUI session.

While the soak runs, in a separate terminal tail the serial log:

```bash
tail -f target/m3os.log 2>/dev/null || journalctl -f
```

(Adjust to wherever the QEMU `-serial` redirects on this machine.)

## Pass criteria

The soak passes iff **all** of the following hold for the full 30 minutes:

- [ ] **Zero panics** from the user-mode-return debug assertion. The
  panic message contains the literal substring `preempt_count != 0 at
  user-mode return`. A single occurrence fails the gate.
- [ ] **No new `[WARN] [sched]` lines** that did not appear pre-57b.
  Compare against a baseline boot log captured immediately before the
  57b merge (`git log --before="<merge-date>" -1 main`).
- [ ] **No deadlocks**. The GUI continues to respond to input (the
  stuck-task watchdog from Phase 57a would print warnings if a kernel
  task wedged for >5s).
- [ ] **No corrupted scheduler state**. The session terminates cleanly
  on shutdown.

## Result tracking

After running the gate, fill in the table below in this file (commit
the update on a follow-up branch named `docs/57b-soak-gate-result`):

| Date | Operator | Duration | Result | Notes |
|------|----------|----------|--------|-------|
|      |          |          |        |       |

If the gate passes, update `docs/roadmap/README.md` Phase 57b row from
`Complete pending soak` to `Complete` and close any tracking issue.

## Failure handling

If the gate fails:

1. Capture the full serial log and any panic backtrace.
2. File a regression in `docs/handoffs/` describing the symptom.
3. Bisect against the merged 57b waves (A → H) to identify the
   responsible track.
4. The most likely causes (ordered by audit-defined risk):
   - A Track G migration mis-classified an IRQ-shared lock as
     task-only — re-check the audit row in
     `docs/handoffs/57b-spinlock-callsite-audit.md` and the migration
     commit's helper-call placement.
   - A Track G callsite raised `preempt_count` on a path that doesn't
     pass through a user-mode return (rare but possible for
     long-running kernel tasks). The Phase 57a stuck-task watchdog
     would surface this first.
   - The `IrqSafeGuard` field declaration order was disturbed,
     breaking Track F's drop ordering — verify
     `kernel/src/task/scheduler.rs` has fields `guard` →
     `_restore` → `_preempt`.

## When to skip

Do not skip this gate. Phase 57b is foundational for 57d (voluntary
preemption) and 57e (full kernel preemption); a latent bug here will
surface as a kernel deadlock the moment 57d's preemption begins firing
inside a held lock.
