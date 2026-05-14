---
status: resolved (2026-05-14 — `fix/wake-task-v2-precondition-race`)
priority: low (tracking)
date: 2026-05-13
component: kernel scheduler — `drive_expired_wake_deadlines` ↔ `block_current_on_reply_v2` residual race
related:
  - docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md
  - docs/handoffs/2026-05-14-audio-intermittent-stutter-distortion.md
  - kernel/src/task/scheduler.rs (drive_expired_wake_deadlines, block_current_on_reply_v2, wake_task_v2, wake_task_v2_if)
log: m3os-1h.log (1-hour idle KVM run on `fix/page-fault-reentry-guard`, 2026-05-13)
---

# Handoff — residual `reply_v2:deadline_expired_no_deadline` spurious wakes

> **Resolved 2026-05-14.** The audio_server DOOM-playback crash
> (`docs/handoffs/2026-05-14-audio-intermittent-stutter-distortion.md`)
> tripped this tracker's elevation criterion #2 ("a userspace caller
> starts surfacing the retry as an observable failure"). The fix
> sketch this doc left behind — refactor `wake_task_v2` to accept a
> precondition closure evaluated atomically under `pi_lock` — landed
> on `fix/wake-task-v2-precondition-race` as `wake_task_v2_if`.
> `drive_expired_wake_deadlines` now passes
> `|t| t.wake_deadline == Some(expected_deadline)` as the precondition;
> the re-validation pass outside the lock is removed. Two consecutive
> `cargo xtask doom-audio-smoke` runs PASS; `audio-smoke`, `smoke-test`,
> `regression`, and the full 12-test `cargo xtask test` suite are all
> green. Kept open as a record of the elevation criteria pattern —
> for future "accepted residual" tracker work.
>
> Original (pre-fix) status note follows for historical context.
>
> **Status note.** This is a **known, already-characterized residual
> race** documented in the Phase 63 audio handoff
> (`docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md`), kept
> open here for tracking. The original Phase 63 fix (`c51ada1`)
> reduced the rate from ~32 hits / 50 s boot to ~1 hit / 50 s boot;
> idle steady-state rates are lower still. This doc records what we
> see today, the criteria under which we'd elevate this from
> "accepted residual" to "fix it", and the implementation sketch
> Phase 63 left behind for that fix.

## TL;DR

`block_current_on_reply_v2` (the `call_msg` reply-wait primitive)
intermittently returns the `DeadlineExpired` `BlockOutcome` even
though the caller did **not** set a deadline. When this happens, the
scheduler emits two paired diagnostics:

```
[WARN] [sched] wake_task_v2 on BlockedOnReply: task=N caller=kernel/src/task/scheduler.rs:5224 has_pending_msg=false
[WARN] [sched] spurious block wake: task=N site=reply_v2:deadline_expired_no_deadline outcome=Some(DeadlineExpired) woken_flag=false pending_at_clear=false
```

The IPC caller's higher-level path (e.g. `call_msg`) re-checks
`take_message()` and produces a benign `u64::MAX` / `EAGAIN` return —
clients retry — so this surfaces only as log noise today, not as a
user-visible bug.

**Root cause (already root-caused, Phase 63).** A microseconds-wide
TOCTTOU window between `drive_expired_wake_deadlines`'s
re-validation of `wake_deadline` under `scheduler_lock` (released
just before the call) and `wake_task_v2`'s state CAS under
`pi_lock`. A target task can:

1. Wake naturally (its deadline cleared, state → `Ready`),
2. Resume, finish whatever woke it,
3. Re-block on `BlockedOnReply` via `call_msg` with no deadline,

all within the few microseconds between the re-validation lock drop
and the CAS — and the stale wake then fires against the new state.

The Phase 63 fix closed the **dominant** window (the
collect-then-wake gap was milliseconds wide pre-fix; re-validation
shrank it to microseconds). The remaining window is the lock
hand-off itself, which is fundamental to the current lock layout.

## Observation — 1-hour idle KVM run on `fix/page-fault-reentry-guard`

Captured 2026-05-13, see `m3os-1h.log` in the repo root.

- Boot to `state=running`: line 789
- Last log line: tick `4228252` (≈70 min runtime at 1 ms LAPIC period)
- `[kstack] pool ready: 542 slots × 64 KiB usable + 4 KiB guard` —
  PR #155's kstack rework active.

Occurrences (all on the **same** task=25; not identified from the
log alone, but consistent with the per-process IPC worker spawned
for one of the always-on services):

```
line 851  wake_task_v2 on BlockedOnReply: task=25 caller=…scheduler.rs:5224 has_pending_msg=false
line 852  spurious block wake: task=25 site=reply_v2:deadline_expired_no_deadline outcome=Some(DeadlineExpired) woken_flag=false pending_at_clear=false
line 1212 wake_task_v2 on BlockedOnReply: task=25 caller=…scheduler.rs:5224 has_pending_msg=false
line 2630 wake_task_v2 on BlockedOnReply: task=25 caller=…scheduler.rs:5224 has_pending_msg=false
line 2631 spurious block wake: task=25 site=reply_v2:deadline_expired_no_deadline outcome=Some(DeadlineExpired) woken_flag=false pending_at_clear=false
```

3 × `wake_task_v2 on BlockedOnReply` and 2 × `spurious block wake`
paired with them — the third wake didn't produce a paired
`spurious block wake` because the diagnostic budgets
(`SPURIOUS_BLOCK_WAKE_BUDGET` / `WAKE_REPLY_DIAG_BUDGET`, both 32
hits per boot in `kernel/src/task/scheduler.rs`) have a per-budget
race that can let one fire while the other still has capacity.

**Rate:** ~2.5 hits/hour idle, ~0.0007 Hz. Below Phase 63's
post-fix characterisation (1 hit / 50 s busy boot ≈ 0.02 Hz). Idle
steady-state is consistent with the expected window narrowing now
that no audio / display / large-IPC bursts are exercising the
reply path.

**Task=25 identity.** Not directly visible from the log; the wake
site's `caller=…scheduler.rs:5224` is `drive_expired_wake_deadlines`,
which is the only emitter of the `BlockedOnReply` shape with
`has_pending_msg=false`. The task is one of the userspace daemons
that takes a sub-second-deadline `call_msg` into a server; the
race window is shaped by which server's reply path is on the hot
loop. Likely candidates given the boot transcript: `nvme_driver`,
`vfs_server`, `net_udp`, or one of the display-stack workers.
Adding a one-shot `pid` capture into `log_wake_blocked_on_reply`
(file path already passes through `caller`) would resolve it the
next reproduction.

## Reference — the original Phase 63 analysis

Full details in `docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md`
§ 1 "audio_server `u64::MAX` from `ipc_call_buf`". Highlights:

- Reproduced as `Io(-32)` returns at the userspace `audio_client`
  retry boundary.
- 32/32 spurious cases pre-fix had
  `outcome=DeadlineExpired, woken_flag=false, pending_at_clear=false`.
- The diagnostic instrumentation in the Phase 63 handoff
  (`SPURIOUS_BLOCK_WAKE_BUDGET`, `WAKE_REPLY_DIAG_BUDGET`,
  `IPC_UMAX_DIAG_BUDGET`) is the same instrumentation that produced
  the observations in this doc — that subsystem is intentionally
  kept in tree as a regression tripwire.
- Phase 63 fix landed as `c51ada1` in `collect_expired_wake_deadlines`
  / `drive_expired_wake_deadlines`. The re-validation step closes
  the dominant millisecond-scale window; the residual microsecond
  window is what we're seeing now.

## Impact today

- **Userspace symptom:** none observed. The IPC caller's
  `take_message()` re-check returns `None`, `call_msg` produces
  `u64::MAX`, and the caller retries. Phase 63 explicitly kept the
  `audio_client` retry as defence in depth precisely so this
  residual race stays invisible to userspace.
- **Kernel symptom:** two WARN-level diagnostic lines per
  occurrence, capped by `SPURIOUS_BLOCK_WAKE_BUDGET` /
  `WAKE_REPLY_DIAG_BUDGET` at 32 hits per boot each. After the
  budget exhausts, the race continues silently.
- **Performance:** one extra Ready→pick_next dispatch per
  occurrence (the falsely-woken task immediately re-blocks).
  Negligible at the observed rate.

## Fix — already designed in Phase 63 handoff

From `docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md`
("Residual"):

> If the residual becomes load-bearing, the next step is to refactor
> `wake_task_v2` to accept a precondition closure evaluated under
> `pi_lock` so the deadline check happens atomically with the state
> CAS.

Implementation sketch:

1. Introduce `wake_task_v2_if(id: TaskId, precondition: impl FnOnce(&Task) -> bool) -> WakeOutcome`.
2. Implementation acquires `pi_lock`, runs `precondition(task)`,
   and only proceeds to the state CAS if `precondition` returns
   `true`. Closure runs inside the same critical section as the
   CAS — no intervening release.
3. `drive_expired_wake_deadlines` passes
   `|t| t.wake_deadline == Some(expected_deadline)` as the
   precondition. The current `task.wake_deadline` is observed
   while the same lock the wake itself takes is held; the race
   collapses to zero.
4. Existing `wake_task_v2` keeps its current shape (predicate
   always true). Phase 63 audio handoff's contract is preserved.

Estimated diff: ~40 LoC inside scheduler.rs, no public API changes
beyond the new helper. Risk: low — the closure runs under a lock
already held; no new lock-ordering exposure.

## Elevation criteria — when this becomes "fix it now"

Track this rate over time. Elevation triggers (any one):

1. Per-hour idle rate climbs above **20 hits/hour** sustained,
   indicating a regression in the re-validation path or a new
   IPC pattern that exercises the residual window heavily.
2. A userspace caller starts surfacing the retry as an observable
   failure (the Phase 63 `audio_client` retry was deliberately kept
   precisely to prevent this — if a new caller is added without
   the retry pattern and starts logging `Io(-32)` / `u64::MAX`,
   that caller either needs the retry OR this race needs the
   precondition-closure fix).
3. `SPURIOUS_BLOCK_WAKE_BUDGET` exhausts within the boot phase on
   any production-shaped boot, meaning the budget is too tight or
   the rate is higher than this doc records.
4. Phase 65+ work introduces a sub-millisecond reply-loop pattern
   (e.g. real-time audio, kernel-userspace control loops at high
   rates) where even microsecond-scale spurious wakes start
   measurably perturbing scheduling.

Until then this is **accepted residual** — the diagnostic in tree
is the tripwire.

## What the next session should do

If picking this up:

1. **Read the Phase 63 handoff first** — full root-cause analysis
   and the diagnostic-budget rationale are there, not duplicated
   here.
2. **Capture task=25's identity** before redesigning. Add a one-shot
   `log::warn!` in `log_wake_blocked_on_reply` that records
   `task.pid` + `task.name` for the first occurrence per boot,
   then run the reproduction recipe in this doc. That alone is
   ~5 LoC and tells us which userspace daemon owns the affected
   reply path.
3. **Implement `wake_task_v2_if`** as sketched above. Pure-logic
   change to scheduler.rs; testable in `cargo test -p kernel-core`
   if mirrored there (or via the `tests/load_balance_smp.rs`-style
   integration test surface).
4. **Verify** with a 30-min idle KVM run + the busy-boot recipe
   from the Phase 63 audio handoff (`cargo xtask doom-audio-smoke`
   is a known busy-IPC stressor). Expect zero
   `reply_v2:deadline_expired_no_deadline` hits post-fix.

## References

| Resource | Where |
|---|---|
| Original analysis | `docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md` § 1 |
| Diagnostic emitters | `kernel/src/task/scheduler.rs::log_wake_blocked_on_reply` (line 3381), `log_spurious_block_wake` (line 3405) |
| Wake site | `kernel/src/task/scheduler.rs::drive_expired_wake_deadlines` (line 5204), specifically line 5224 |
| Re-validation fix | commit `c51ada1` in `collect_expired_wake_deadlines` |
| Reply primitive | `kernel/src/task/scheduler.rs::block_current_on_reply_v2` (line 3285) |
| Caller | `kernel/src/ipc/endpoint.rs::call_msg` |
| Diagnostic budgets | `SPURIOUS_BLOCK_WAKE_BUDGET` / `WAKE_REPLY_DIAG_BUDGET` (both `AtomicU32::new(32)`, scheduler.rs lines 3370–3379) |
