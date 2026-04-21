# Scheduler-Fairness Investigation — Pointer

**Status:** Resolved 2026-04-21. Superseded by the post-mortem at
[`docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`](../post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md).

This file used to host the "pick up and continue" resume prompt for a
fresh Claude Code session while the SSH-wedge investigation was open.
The investigation closed when the root cause — `SCHEDULER.lock()`
being plain `spin::Mutex` reachable from `wake_task` in ISR context —
was identified and fixed. The post-mortem carries the authoritative
summary, fix, validation, and follow-up action items.

The neighbouring investigation log at
[`scheduler-fairness-regression.md`](./scheduler-fairness-regression.md)
is preserved as the historical record of the H1–H9 hypothesis search
and the failed experiments along the way. Read the post-mortem first;
dip into the regression log when you want to see how the root cause
was narrowed.

## Reproduction harness (kept for future regression testing)

```
/tmp/h9_run_once.sh <run-id>
    → Single-shot boot + ssh attempt. Produces
      /tmp/h9run<id>.{log,ssh,summary}.
/tmp/h9_batch.sh <count> <prefix>
    → Run /tmp/h9_run_once.sh count times with RUN_ID=<prefix><i>.
      Tallies class= counts at the end.
```

Classification from the `summary` file:

- `class=clean-auth-rejected` → `ssh` exit 255 + "Permission denied".
  Clean handshake, auth rejected by `BatchMode=yes`. Expected path.
- `class=clean-login` → `ssh` exit 0. Only if a real keyring matches;
  never seen in this harness because `BatchMode` skips passwords.
- `class=early-wedge` → `ssh` exit 255 + "Connection timed out during
  banner exchange". The pre-fix dominant failure.
- `class=late-wedge` → `ssh` exit 124 + "Permanently added". A
  slow-tail artefact in pre-fix samples; no longer observed.

Both scripts live outside the tree under `/tmp/`. If the class of bug
recurs, the **Action items** in the post-mortem call for moving the
harness into `tests/` or `scripts/` as a named regression.
