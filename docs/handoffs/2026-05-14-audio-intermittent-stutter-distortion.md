---
status: resolved (2026-05-14 — `fix/wake-task-v2-precondition-race`)
priority: medium (user-visible UX regression, intermittent, reboot-recoverable)
date: 2026-05-14
component: AC'97 audio pipeline — audio_server BDL refill loop ↔ IRQ-driven wake ↔ `block_current_on_reply_v2` IPC reply path
related:
  - docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md
  - docs/handoffs/2026-05-13-reply-v2-deadline-residual-race.md
  - docs/handoffs/2026-05-13-mouse-reset-top-left-intermittent.md
  - userspace/audio_server/src/device.rs
  - userspace/audio_server/src/irq.rs
  - userspace/audio_server/src/stream.rs
  - userspace/audio-demo/src/main.rs
log: m3os-audio-crash.log, m3os-audio-crash-2.log (DOOM-playback reproductions captured 2026-05-14)
---

> **Resolved 2026-05-14.** Two user-captured DOOM-playback crash logs
> (`m3os-audio-crash.log`, `m3os-audio-crash-2.log`) confirmed
> Hypothesis A — the reply_v2 residual race firing under audio load.
> Log 2 line 835 caught the exact diagnostic signature
> (`wake_task_v2 on BlockedOnReply: task=25 caller=…:5224 has_pending_msg=false`)
> immediately before `audio_server: recv failed`. The race fired on a
> DOOM `audio_client` task blocked in `BlockedOnReply`; the audio_server
> itself crashed via the `recv_msg_with_notif` spurious-wake fallback
> returning `u64::MAX`, which `audio_server::run_io_loop`
> (`userspace/audio_server/src/irq.rs:244-256`) treats as fatal by
> design.
>
> Fix: precondition-closure refactor of `wake_task_v2` per the
> reply_v2 tracker's design sketch — `wake_task_v2_if(id, |t| t.wake_deadline == Some(expected))`
> runs the deadline check inside the same `pi_lock`+`scheduler_lock`
> critical section as the state CAS, collapsing the TOCTTOU window
> to zero. `drive_expired_wake_deadlines` switched to the new helper;
> the redundant re-validation pass outside the lock is removed.
> `log_wake_blocked_on_reply` also gained a one-shot pid+name capture
> so the next reproduction (if any) names the affected task directly
> instead of leaving "task=25" anonymous.
>
> Verification: two consecutive `cargo xtask doom-audio-smoke` PASS;
> `audio-smoke`, `smoke-test`, `regression`, `cargo xtask test` (all
> 12 kernel tests) all green.
>
> Original report follows for context.
>
> ---

# Handoff — intermittent audio stutter + distortion ("morse code" pattern)

> **Bug shape from user report.** Some boots, audio works normally. Other
> boots, all audio paths (DOOM SFX, terminal bell, audio-demo) produce
> a **stuttering + distorted output that sounds like morse code** —
> bursts of valid samples interleaved with silence or stale-buffer
> distortion. Rebooting clears the state, sometimes onto a good boot,
> sometimes onto another bad boot.
>
> The user also observed the mouse top-left-reset issue
> (`docs/handoffs/2026-05-13-mouse-reset-top-left-intermittent.md`)
> on the **same boot** as a bad audio boot. Whether the two share a
> root cause is not yet confirmed; both are tracked separately for now.

## TL;DR

The "morse code" audible signature is the classic shape of a cyclic
**BDL underrun → audio_server catches up → underrun → catches up**
loop:

1. audio_server submits N frames to the AC'97 BDL.
2. AC'97 plays them and emits a `BCIS` interrupt asking for more.
3. audio_server's IPC reply loop is stalled (scheduling latency, IPC
   reply race, IRQ delivery latency, or some combination).
4. AC'97 runs out of buffers, sets `LVBCI` or wraps to stale samples.
5. audio_server finally wakes, refills.
6. Cycle repeats — audible as periodic bursts of valid audio
   separated by silence or buzz.

On this machine three back-to-back `cargo xtask audio-smoke` runs all
PASS with 99% non-silent samples and zero underruns — so on at least
one host the basic submit→IRQ→refill path is healthy. The cause is
**timing-sensitive** (intermittent across boots, host-dependent), not
a deterministic protocol bug.

## Reproduction

User-observed pattern:

1. Boot to graphical session.
2. Trigger any audio path (audio-demo, DOOM, bell-test).
3. Symptom may or may not appear on this boot.
4. Reboot may transition to good or bad state non-deterministically.

When the symptom is present, every audio path is affected (it is not
client-specific).

**What to capture next time the bad state is observed** (in order of
priority):

1. **Full serial log** during a bad audio run (and through several
   minutes of attempts). Diagnostic instrumentation already in tree
   from the Phase 63 audio handoff is the primary signal — see
   `kernel/src/ipc/mod.rs::log_ipc_umax` and
   `kernel/src/task/scheduler.rs::{log_spurious_block_wake,
   log_wake_blocked_on_reply}`. Both have 32-hit boot budgets so
   capture early in the bad run.
2. **Compare `audio_summary` lines from DOOM**: a good boot shows
   `frames_submitted ≈ frames_consumed` with `underruns=0`; a bad
   boot should show `underruns > 0` and possibly
   `frames_submitted ≫ frames_consumed`.
3. **`AUDIO_DEMO:stats consumed=N underruns=M`** from the audio-demo
   binary. `M > 0` confirms the IRQ-side detected underrun events.
4. **`BELL_TEST:consumed=N underruns=M`** from the bell-test binary.
   Same shape.

The relevant grep patterns when triaging a captured log:

```bash
grep -nE "u64::MAX diag|wake_task_v2 on BlockedOnReply|spurious block wake|underruns=|frames_consumed=|frames_submitted=" m3os-bad.log
```

## Hypotheses (ranked by likelihood)

### Hypothesis A — reply_v2 residual race fires more under audio load (**most likely**)

Documented in `docs/handoffs/2026-05-13-reply-v2-deadline-residual-race.md`.

The Phase 63 fix (`c51ada1` in `drive_expired_wake_deadlines`) closed
the dominant millisecond-scale window but left a microseconds-wide
TOCTTOU residual between `scheduler_lock` release in the
re-validation pass and `pi_lock` acquire in `wake_task_v2`.

In the 1-hour idle log we observed 3 hits on a single task (task=25)
— a rate of ~2.5/hour at idle. **Under continuous audio load the
rate would be much higher** because audio_server's reply path
(`call_msg` into the kernel device-host, IPC into the AC'97
backend) runs on every BDL refill — order ~50 times per second.
Each invocation has a microsecond-scale TOCTTOU window; each hit
costs at least one Ready→pick_next dispatch (the falsely-woken task
re-blocks immediately). A handful of late wakes per second would
disrupt the BDL refill cadence in exactly the morse-code shape the
user reports.

The Phase 63 handoff kept `audio_client`'s `Io(-32)` retry as
defence in depth specifically so the spurious wake stays invisible
to userspace. But the retry itself costs a round trip — and if the
retry lands again on the same race, the audible gap grows.

**What would confirm it.** A bad-boot log showing the diagnostic
budgets exhausted early (32 hits in the first minute), correlated
in time with the audible glitches.

**Fix.** Already designed in the reply_v2 tracker
(PR #157 / `docs/handoffs/2026-05-13-reply-v2-deadline-residual-race.md`):
refactor `wake_task_v2` to accept a precondition closure evaluated
atomically under `pi_lock` with the state CAS. ~40 LoC.

This hypothesis would also explain the simultaneous mouse-reset
observation if `task=25` is `display_server`'s reply worker (since
display_server's input dispatcher uses the same `call_msg` path).
Identifying `task=25`'s pid + name is the prerequisite step the
reply_v2 tracker already flagged as ~5 LoC.

### Hypothesis B — scheduler latency spike from a different source

`stale-ready` warnings in the 1-hour idle log show task-readiness
latency >100 ms intermittently. At 48 kHz / 16-bit / stereo, a
typical 4 KiB BDL buffer is ~21 ms of audio; AC'97's 32-entry
ring has ~670 ms of headroom in theory but only if audio_server
keeps the ring fed. **One 100 ms scheduler stall** eats a quarter of
the typical 32 KiB working-set audio_server keeps queued.

Sources of 100 ms scheduler stalls observed in tree:

- Sub-millisecond preempt-bracket pile-up under specific IPC
  workloads (Phase 57e Bug #12).
- `tlb_shootdown_range` waiting on a slow IPI ack from a stalled
  core (less likely with the per-core send fix from PR #156 now
  in place).
- `dump_trace_rings` from any kernel diagnostic dump — but this
  fires only on crash, so not for a healthy boot.

**What would confirm it.** Audible glitches that don't correlate
with reply_v2 diagnostic spikes — and visible `stale-ready` bursts
in the log instead.

### Hypothesis C — AC'97 IRQ delivery delayed

If IRQ 5 / 9 / 10 / 11 (AC'97 typically lands on the legacy ISA
IRQ overrides routed via the IOAPIC) is masked or delayed for
windows of 20+ ms, audio_server doesn't get its BCIS / LVBCI wakes
in time. The Phase 63 work fixed a major IRQ-pipeline bug
(`58bbbc8`), but residual IRQ delivery latency under contention is
plausible.

**What would confirm it.** Trace-ring entries showing IRQ entry
gaps >20 ms on the AC'97 vector during a bad run.

### Hypothesis D — BDL refill submitting too-few frames per cycle

If `streams.submit` consistently submits only 1–2 buffers per IRQ
(instead of refilling the ring up to a high-water mark), normal
scheduling jitter eats into the headroom faster than the BDL can
absorb. This is a tuning issue, not a correctness one.

**What would confirm it.** A bad-boot log with `underruns > 0` and
relatively low `frames_submitted` vs the buffer capacity.

### Hypothesis E — Shared root cause with the mouse top-left reset (**speculative**)

The user observed both bugs on the same boot. If both subsystems
share a common scheduling / IPC latency source, fixing that source
would fix both. Most parsimonious shared candidate: the reply_v2
residual race firing more under load (covers Hypotheses A above).

**What would confirm it.** A bad-boot log showing both the
`reply_v2:deadline_expired_no_deadline` diagnostic AND mouse
top-left snaps in the same time window.

## Code surface — what to read first

1. **`userspace/audio_server/src/device.rs:285` — `classify_sr`**.
   Decodes the AC'97 status register into `IrqEvent`. `Underrun`
   shape is `FIFOE && ring_was_empty`; `FifoError` is the hard-bug
   shape; `LastValidIndex` is the "BDL ran out, please repost"
   shape.
2. **`userspace/audio_server/src/irq.rs`** — `run_io_loop` and
   `apply_irq_event`. Where audio_server reacts to the AC'97 IRQ,
   advances `frames_consumed` via `poll_frames_consumed`, and
   refills the BDL.
3. **`userspace/audio_server/src/stream.rs::submit`** — the
   producer-side accounting. Tracks `frames_submitted` and
   handles `WouldBlock` when the BDL is full.
4. **`kernel/src/task/scheduler.rs::block_current_on_reply_v2`
   (line 3285)** — the reply primitive whose residual race is the
   most likely culprit per Hypothesis A.
5. **`docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md`** —
   full Phase 63 root-cause analysis of the previous audio
   regressions. Many of the diagnostic patterns documented there
   apply directly here.

## Recommended investigation order

1. **Capture a bad-boot serial log.** Reproduce the symptom and
   redirect QEMU's serial output to a file
   (`cargo xtask run --kvm > /tmp/audio-bad.log 2>&1`), exercise the
   audio paths through the failure window, kill QEMU, and grep for
   the four signatures listed in [Reproduction](#reproduction).
   The single log either confirms or rules out Hypothesis A in one
   observation.
2. **If Hypothesis A confirmed**: implement the precondition-closure
   refactor of `wake_task_v2` already sketched in the reply_v2
   tracker. Also tackle the task=25 identity capture (~5 LoC) so
   we can confirm whether task=25 is the audio_server worker
   (cross-reference with the Hypothesis E shared-cause question).
3. **In parallel**: add a structured `[audio]` diagnostic from
   audio_server when `underrun_count` increments
   (`userspace/audio_server/src/device.rs:597` already tracks the
   counter; just needs a `syscall_lib::write_str` emission tied
   to the increment).
4. **If Hypothesis A is ruled out**: pivot to Hypothesis B
   (scheduler latency) — instrument trace-ring entries around
   audio_server's `submit` / `poll_frames_consumed` calls to find
   what's actually delaying the task.

## What this is NOT

- **Not the kernel-pipe-table corruption from PR #155.** That was
  resolved by the kstack guard-page rework; the 2.5-hour idle run
  on `fix/page-fault-reentry-guard` was clean. This audio issue
  manifests under audio load, not at idle.
- **Not a `cargo xtask audio-smoke` regression on the test gate.**
  The gate passes on this machine across multiple consecutive
  runs. The user's symptom is timing-sensitive intermittent, not
  a deterministic protocol break.
- **Not (yet) confirmed to share a root cause with the mouse
  top-left reset** (`docs/handoffs/2026-05-13-mouse-reset-top-left-intermittent.md`).
  Co-occurrence on the same boot is suggestive but not conclusive
  until we have a log capturing both glitches in the same time
  window with diagnostic correlation.

## References

| Resource | Where |
|---|---|
| Phase 63 audio root-cause history | `docs/handoffs/2026-05-11-phase-63-audio-irq-wake-race.md` |
| reply_v2 residual race tracker | `docs/handoffs/2026-05-13-reply-v2-deadline-residual-race.md` |
| Mouse top-left handoff | `docs/handoffs/2026-05-13-mouse-reset-top-left-intermittent.md` |
| AC'97 IRQ classification | `userspace/audio_server/src/device.rs::classify_sr` (line 285) |
| audio_server IO loop | `userspace/audio_server/src/irq.rs::run_io_loop` |
| Stream submit accounting | `userspace/audio_server/src/stream.rs` |
| Reply primitive | `kernel/src/task/scheduler.rs::block_current_on_reply_v2` (line 3285) |
| Wake-deadline driver | `kernel/src/task/scheduler.rs::drive_expired_wake_deadlines` (line 5204) |
| `audio_client` retry (DiD) | `userspace/audio_client/src/lib.rs` |
| Diagnostic budgets | `kernel/src/task/scheduler.rs::{SPURIOUS_BLOCK_WAKE_BUDGET, WAKE_REPLY_DIAG_BUDGET}`, `kernel/src/ipc/mod.rs::IPC_UMAX_DIAG_BUDGET` (all 32-hit per boot) |
| audio-smoke gate | `cargo xtask audio-smoke` (PASSED ×3 on the dev machine 2026-05-14) |
