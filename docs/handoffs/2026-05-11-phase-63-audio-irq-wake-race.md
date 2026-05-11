---
status: resolved
branch: feat/phase-63-audio-stack-implementation
last-known-good-commit: c51ada1
date: 2026-05-11
component: end-to-end audio path — driver_runtime IPC ↔ audio_server ↔ AC'97 BDL ↔ QEMU wav backend
related:
  - docs/handoffs/2026-04-25-scheduler-design-comparison.md
  - docs/63-audio-stack-implementation.md
  - docs/handoffs/2026-04-28-graphical-stack-startup.md
---

# Handoff — Phase 63 audio path

> **Doc title note**: filename still says `audio-irq-wake-race` from
> the first issue this thread tracked. The wake-race is fixed
> (`f1573bd`), the AC'97 IRQ pipeline is fixed (`58bbbc8`), the
> WAV-silent / second-open / Open-while-open / Io(-32) issues all
> closed across the 2026-05-11 sessions, and the two non-blocking
> follow-ups originally noted (kernel `ipc_call_buf == u64::MAX`
> race and scheduler watchdog false-positive) are now also resolved
> in `c51ada1` and `f2040a8` respectively. The file is kept under
> the same name so existing references don't break. See
> [Resolved follow-ups](#resolved-follow-ups) for the diagnostic
> process and fix details.

## ⚠ Status update (2026-05-11 — final, end-to-end working)

**`audio-smoke` PASSes deterministically (5/5 in the closing run),
and `audio-demo` plays audibly from `cargo xtask run` /
`cargo xtask run-gui` with no hand-running of QEMU flags.** The
loudest 1-second window of the recorded WAV reports ~99% non-silent
samples (~88K/88K @ 44.1 kHz stereo); back-to-back invocations of
`audio-demo` recover from prior-client crashes automatically.

```
[step 19] wait-pass-or-fail: guest/audio: audio-demo PASS sentinel (run #2) (30s)
[step 20] wait-line-not-matching: guest/audio: frames_consumed non-zero (run #2) (5s)
audio-smoke: PASSED (20 steps in 35s)
audio-smoke: WAV check PASSED — loudest 88200-sample window has 99% non-silent samples (87550/88200), total non-silent 158134/2527210
audio-smoke: WAV non-silent check PASSED
```

User-confirmed: audible 440 Hz tone on `cargo xtask run` (with
PipeWire pulse running on the host and a QEMU built with
`--enable-pipewire`/`--enable-pa`).

**What was wrong, and what changed.** Each of the headline fixes
landed in this session is summarised below; the per-bug breakdown is
in the two tables under [Bugs closed in earlier 2026-05-11 sessions](#bugs-closed-in-earlier-2026-05-11-sessions)
and [Bugs closed in this 2026-05-11 session](#bugs-closed-in-this-2026-05-11-session-third).

| Layer | What broke | How it's fixed now |
|---|---|---|
| `xtask::assert_wav_non_silent` | QEMU's streaming WAV writes size=0 headers; previous checker bailed before scanning the data | Tolerate size=0 (use bytes-to-EOF); replace whole-file 5% threshold with a sliding 1-second window |
| `xtask` smoke harness | FAIL-line capture truncated mid-write; `variant=` always lost | Require newline before firing on `AUDIO_DEMO:FAIL` |
| `xtask` `cmd_run` | Headless `run` never appended `-device AC97,…` | Route through `launch_qemu_with_devices_audio`; default-on, `--no-audio` opt-out |
| `xtask::detect_gui_audio_driver` | Audiodev probe silently fell back to `none` for stripped QEMU | Accept `M3OS_AUDIODEV` override; broader candidate sockets; distro-aware install hints |
| `Ac97Backend::open_stream` | Second IPC `Open` only flipped a bool — controller stayed halted and `Ac97Logic` carried stale head/tail | Reset `Ac97Logic` and re-call `open_pcm_out_stream` (BDBAR + LVI=0 + CR.RPBM) on every open |
| `audio_server::irq::dispatch_message` | Open-while-open returned `Busy`, leaving the server wedged after a crashed client | Open is now a takeover — close the lingering stream first, then open fresh. The protocol-level path is what actually fires because `LABEL_AUDIO_CMD` is constant across clients |
| `audio-demo::submit_tone` | `Io(-32)` (kernel IPC race) was a hard failure | Retry on the same schedule as `Server:WouldBlock` (200×5 ms) so the documented intermittency is absorbed |

## TL;DR (current state)

- Pre-session-1: `audio-demo` never ran — keystrokes hit `m3OS login:`.
  10 IPC/driver_runtime/audio_server bugs fixed in `58bbbc8`.
- Pre-session-2: step list PASSed, but WAV non-silent failed with
  `'data' chunk is empty (0 samples)` despite driver-side evidence
  the audio was getting through.
- Post-session-2: WAV checker rewritten; smoke harness diagnostics
  upgraded; end-to-end PASS on 8/10 runs.
- This (final) session: closed the cascade triggered by the
  `Io(-32)` IPC race. `Ac97Backend::open_stream` now re-arms the
  controller; `audio_server` treats `Open` as a takeover when a
  stream is already open; `audio-demo` retries `Io(-32)` like a
  `WouldBlock`. Audio path is end-to-end working — 5/5
  audio-smoke runs PASS.
- Post-final sessions: both originally-listed follow-ups closed.
  `drive_expired_wake_deadlines` was unpaired-waking `BlockedOnReply`
  tasks (the kernel-side source of the `Io(-32)` cascade) — fixed in
  `c51ada1` by re-validating the deadline between collection and
  wake. The watchdog false-positive on idle `audio_server` is fixed
  in `f2040a8` by skipping `BlockedOnNotif` with a live bound
  notification. The 200×5 ms `Io(-32)` retry in `audio-demo` stays
  as defence-in-depth against the residual ~1% race window. See
  [Resolved follow-ups](#resolved-follow-ups).

## Reproduction

```bash
git checkout feat/phase-63-audio-stack-implementation
git pull
# Automated gate (TCG): asserts the WAV and step list pass twice.
M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log cargo xtask audio-smoke

# Interactive on the developer's machine — needs a QEMU with
# pipewire and/or pa audio backends compiled in:
cargo xtask run        # then log in as root, run `audio-demo`
cargo xtask run-gui    # same, with a QEMU display window
```

Expected smoke output (deterministic — 5/5 in the closing run):

```
[step 19] wait-pass-or-fail: guest/audio: audio-demo PASS sentinel (run #2) (30s)
[step 20] wait-line-not-matching: guest/audio: frames_consumed non-zero (run #2) (5s)
audio-smoke: PASSED (20 steps in 35s)
audio-smoke: WAV check PASSED — loudest 88200-sample window has 99% non-silent samples (87550/88200), total non-silent 158134/2527210
audio-smoke: WAV non-silent check PASSED
```

Look for these markers in `/tmp/serial.log` to confirm the driver path
is healthy:

- `audio_server: spawned` — driver started
- `device_host.irq_subscribe pid=16 … vector=0x62 notif=0 bit=0` — INTx routed
- `AUDIO_DEMO:opened` / `AUDIO_DEMO:submitted` / `AUDIO_DEMO:drained`
- `AUDIO_DEMO:PASS`
- `AUDIO_DEMO:stats consumed=N underruns=0` with `N > 0` (typically ~88,000–91,000)

The smoke's WAV file is ~4.8 MB at 44.1 kHz stereo (QEMU's wav backend
defaults — it rate-converts from our 48 kHz codec setting). Audio
content sits in the last ~2 seconds of the file (one second per
audio-demo invocation, run back-to-back); the rest is captured boot
silence. The loudest 1-second window is the assertion target.

If a previous regression resurfaces, see "Bugs already closed" below
for the failure signature each one produced.

## Bugs closed in earlier 2026-05-11 sessions

These all fired in series — each fix uncovered the next one. None of
them were the AC'97 IRQ delivery the original handoff blamed.

| # | Layer | Symptom | Fix |
|---|-------|---------|-----|
| 1 | `xtask` smoke harness | `audio-demo` typed at `m3OS login:`, treated as a username, "Login incorrect" — demo never ran | `audio_smoke_steps()` now prefixes `boot_and_login_steps()` (xtask:6100) |
| 2 | `driver_runtime::ipc::SyscallBackend` | Hardcoded `REPLY_CAP_HANDLE = 1`, but the kernel inserts the reply cap into the receiver's first free slot. For `audio_server` (caps 0=device, 1=endpoint, 2/3=DMA, 4=notif) the reply landed at slot 5 — `transport.reply()` removed slot 1 (the endpoint), the next `recv` failed with `InvalidHandle`, and audio_server exited | `RecvFrame` now carries `reply_cap_handle`; `SyscallBackend` stashes it from `msg.data[3]` on every `recv` and uses it in `reply` (lib/driver_runtime/src/ipc/mod.rs) |
| 3 | kernel `MAX_BULK_LEN` | Capped at 65536, but audio's wire payload is `frame_header (16 B) + PCM (64 KiB) = 65552 B` — every `Open`-followed-`SubmitFrames` rejected with `u64::MAX` | Bump to 81920 (kernel/src/ipc/mod.rs) |
| 4 | `kernel-core::user_range::MAX_COPY_LEN` | Same 64 KiB ceiling at the `validate_user_range` layer — `copy_to_kernel` failed before `MAX_BULK_LEN` was checked | Bump to 96 KiB (kernel-core/src/user_range.rs) |
| 5 | `audio_server::irq::dispatch_message` | `SubmitFrames` arm was a Phase-57-D.1 stub: `let _ = len; SubmitAck { frames_consumed: 0 }`. The PCM bytes were silently dropped — the backend's `submit_frames` was **never called**, so the BDL was never programmed and the AC'97 controller had nothing to consume | `IoAction::HandleMessage` now carries the `consumed` byte count from the decoder; `run_io_loop` extracts the PCM tail (`frame.bulk[consumed..consumed+len]`) and routes it to `streams.submit(backend, stream_id, pcm)` (audio_server/src/irq.rs) |
| 6 | `audio_server::irq::run_io_loop` | Even with #5 wired, `frames_consumed` reported `0` because `streams.record_consumed` was never called — the comment in `apply_irq_event` said "the io loop reads the backend's stats snapshot and calls `record_consumed` separately" but no such call existed | New `AudioBackend::poll_frames_consumed()` trait method; `run_io_loop` queries it before encoding any reply and feeds the delta into `streams.record_consumed` |
| 7 | `audio_server::device::Ac97Backend` | No non-IRQ path to advance `tail` — under QEMU's `wav` backend the controller walks the BDL on a timer but `tail` stayed pinned, so the second `submit` deadlocked on `WouldBlock` | New `Ac97Backend::poll_completed_buffers()` reads `CIV` and runs `Ac97Logic::observe_irq(0, civ)` to drag `tail` forward; called from `submit_frames` and `poll_frames_consumed` |
| 8 | `audio-demo::submit_tone` | Submitted 64 KiB per call into a 16 KiB BDL ring (`PCM_SLOT_STRIDE × BDL_ENTRIES = 512 × 32`). Even chunked at 16 KiB, `observe_irq` always leaves the slot at the current `CIV` in-flight, producing a permanent 1-slot deficit and a `WouldBlock` loop | Cap chunk at 8 KiB (`SUBMIT_CHUNK_BYTES`); add `WouldBlock` retry with `nanosleep_for(0, 5_000_000)` capped at 200 attempts |
| 9 | `audio-demo::log_error` | Five separate `write_str` calls; the smoke harness's `AUDIO_DEMO:FAIL stage=` prefix matcher captured an empty variant suffix because the rest of the line hadn't arrived yet when QEMU was killed | Assemble the FAIL line in a stack buffer, emit it with one `write` call (audio-demo/src/main.rs) |
| 10 | `audio-demo` print order | `AUDIO_DEMO:stats` was emitted before `AUDIO_DEMO:PASS`. The smoke harness's PASS step (`WaitPassOrFail`) drains the buffer up through PASS, so the stats line was gone by the time the next step (`WaitLineNotMatching` for `consumed=`) ran — the harness reported PASSED-then-step-16-timeout | Move `log_stats` AFTER PASS (audio-demo/src/main.rs) |

Validation tools added (kept in tree):

- `kernel/src/arch/x86_64/interrupts.rs` — permanent `DEVICE_IRQ_HITS[]`
  per-vector counter + `device_irq_hits()` accessor. Lock-free
  `AtomicU64`, ISR-safe, zero overhead beyond a `fetch_add` per IRQ.
- `kernel/src/task/scheduler.rs` — when the trace-ring dump fires
  (stuck-no-waker), it now also prints non-zero device IRQ counters.
  Future regressions in the same area can be triaged with one log scan.

## Bugs closed in this 2026-05-11 session (third)

| # | Layer | Symptom | Fix |
|---|-------|---------|-----|
| 11 | `xtask::assert_wav_non_silent` | QEMU's `-audiodev wav` backend leaves the RIFF/`data` chunk-size fields at zero (streaming WAV). The previous checker read `data_size`, saw 0, and bailed with `'data' chunk is empty (0 samples)` — even though ~78K non-silent samples were sitting near the file's tail | (a) When `data_size == 0` or the declared size overruns the file, treat the rest of the file as the PCM payload. (b) Replace the whole-file 5%-non-silent threshold with a sliding 1-second window — the whole-file average was always drowned by boot silence (xtask/src/main.rs:`assert_wav_non_silent`) |
| 12 | `xtask::WaitPassOrFail` | The harness fired the moment `AUDIO_DEMO:FAIL stage=` appeared in the buffer, killing QEMU mid-write of the rest of the line. The captured suffix was truncated (`stage=submi`, `stage=submit v`, …) so the `variant=<AudioClientError>` diagnostic was permanently lost | Require a newline after the prefix before triggering — new `find_terminated_fail_line` helper, used at both call sites. Two new tests pin the terminated/unterminated cases (xtask/src/main.rs) |
| 13 | `audio-demo::error_label` | `AudioClientError::Server(_)` was reported as a flat `"Server"` — the inner `AudioError` discriminant (which distinguishes `WouldBlock` from `Internal` etc.) was unobservable from smoke output | Expand the label to `Server:Busy` / `Server:WouldBlock` / … and append `errno=<i32>` for `Io(errno)` variants — the latter immediately revealed the intermittent failure was `Io(-32)` from `ipc_call_buf` returning `u64::MAX` (audio-demo/src/main.rs) |
| 14 | `xtask` `cmd_run` | Headless `cargo xtask run` never appended `-audiodev` + `-device AC97,…` to the QEMU args. `audio_server` correctly fell back to stub mode (`audio_server: WARNING — no AC'97 device found`) and `audio-demo` reported `consumed=0`. Only `run-gui` had been audio-aware since Phase 57 H.5 | Route `cmd_run` through `launch_qemu_with_devices_audio` and add the same `--no-audio` opt-out flag. Two new tests pin headless-run audio-on / audio-off arg shapes (xtask/src/main.rs) |
| 15 | `xtask::detect_gui_audio_driver` | The audiodev probe only checked `pipewire-0` and `pulse/native` and silently fell back to `none` if neither matched. On a Linux host with PipeWire-pulse, a QEMU built without audio backends (Arch's `qemu-base` / minimal `qemu-system-x86_64` ships `Available audio drivers: none wav` only) was indistinguishable from a missing daemon | Accept `M3OS_AUDIODEV=<driver>` as an explicit override; broaden the candidate socket list (`pipewire-{0,1,2}`, `pulse/native`+`pulse/cli`); when the probe still misses, log the QEMU-supported driver list, missing drivers with distro-specific install hints, and the contents of `$XDG_RUNTIME_DIR` |
| 16 | `audio_server::Ac97Backend::open_stream` | `close_stream` halts the bus master (CR=0) and issues CR.RR (clearing CIV/LVI/SR/BDBAR on the hardware), but `open_stream` only flipped `stream_open = true` — it neither re-armed the controller (BDBAR+LVI+CR.RPBM) nor reset `Ac97Logic`'s head/tail/BDL mirror. The first `audio-demo` invocation PASSed with `consumed=89344`; the second invocation reproducibly failed with `Server:WouldBlock` after ~1 s of 200×5 ms retries because the bus master was halted and CIV/`tail` could never advance | `open_stream` now resets `self.logic = Ac97Logic::new()` and calls `open_pcm_out_stream(&self.bus, self.bdl.iova())` to re-program BDBAR → LVI=0 → CR.RPBM before returning. The audio-smoke step list now runs audio-demo **twice** so this regression class can't sneak back in (xtask/src/main.rs:`audio_smoke_steps`) |
| 17 | `audio_server::irq::dispatch_message` `Open` arm | When `audio-demo` died after `Open` but before `Close` (the `Io(-32)` intermittency aborting mid-`SubmitFrames`), the `Ac97Backend.stream_open` flag stayed `true`. Subsequent demo invocations got `OpenError(Busy)` indefinitely; the server never recovered without a restart. The earlier `ClientRegistry::force_release` takeover never fired because every audio_client user sends the same constant `LABEL_AUDIO_CMD = 0x000A_0D10_C0DE` — `frame.label`-derived `client_id` is identical across consecutive demo processes | `dispatch_message`'s `Open` arm now treats an arriving Open while the stream is already open as a takeover: close the lingering stream on the backend, then proceed with the open. The existing client_id eviction stays as a future-proofing layer for when clients diversify their IPC labels. Host test `dispatch_open_when_already_open_takes_over_and_returns_opened` pins the new semantic (audio_server/src/irq.rs) |
| 18 | `audio-demo::submit_tone` | `AudioClientError::Io(-32)` (the `audio_client` mapping for `ipc_call_buf` returning `u64::MAX` — see [Known follow-ups](#known-follow-ups)) was treated as a hard failure. Even with the server-side Open-takeover fix the next demo had to be manually re-invoked to recover from a single missed call | Retry `Io(-32)` on the same schedule as `Server:WouldBlock` (200 attempts × 5 ms backoff). The retry absorbs the documented IPC race so a single missed `ipc_call_buf` no longer aborts the run (audio-demo/src/main.rs) |

## Resolved follow-ups

Both originally-listed follow-ups closed across the 2026-05-11
diagnostic / fix sessions following the main audio-path work. The
diagnostic instrumentation that pinned each cause is left in tree
for the next regression of the same shape.

### 1. Kernel `ipc_call_buf == u64::MAX` race — fixed in `c51ada1`

**Original symptom.** `audio_client::SyscallSocket::call`:

```rust
let reply_label = syscall_lib::ipc_call_buf(
    self.endpoint, LABEL_AUDIO_CMD, LABEL_AUDIO_CMD, &combined[..total],
);
if reply_label == u64::MAX {
    return Err(AudioClientError::Io(-32)); // EPIPE-shaped
}
```

returned `u64::MAX` on roughly 10–20 % of `SubmitFrames` sends
under TCG. The audio path masked it with a 200×5 ms retry, but the
same race fired across **every** ring-3 IPC client that uses
`call_msg` — term ↔ display_server (`LABEL_CLIENT_EVENT_PULL`),
display_server ↔ kbd_server / mouse_server (`KBD_EVENT_PULL` /
`MOUSE_EVENT_PULL`), and audio-demo ↔ audio_server
(`LABEL_AUDIO_CMD`). Each fired the same kernel diagnostic
once enabled.

**Diagnostic process.** Three commits layered on instrumentation
that narrowed the cause from "somewhere in the IPC stack" to a
single line in the scheduler:

1. **`a571115`** — rate-limited `[ipc] u64::MAX diag` logs at every
   kernel site that returns the sentinel from `ipc_call_buf` /
   `ipc_send_with_bulk` / `endpoint::call_msg` /
   `endpoint::recv_msg_with_notif`. First pass showed 32/32 hits at
   `call_msg:no_reply_message` — the post-`block_current_until`
   path where `take_message` returned `None`.
2. **`13ed58a` + `e816b87`** — `[sched] spurious block wake` logs
   inside `block_current_on_reply_v2`, capturing the
   `BlockOutcome`, the local `woken` flag, and `pending_at_clear`.
   Result: 31/32 spurious cases were
   `outcome=DeadlineExpired, woken_flag=false, pending_at_clear=false`
   — meaning `wake_task_v2` had fired without `deliver_message`
   having been called first.
3. **`6d0f18a` + `297aff1` + `82226cb`** — `#[track_caller]` on
   `wake_task_v2`, filtered to log only when prev_state was
   `BlockedOnReply` AND `pending_msg` was `None` at the CAS (the
   unpaired-wake shape). 32/32 hits resolved to a single line:
   `kernel/src/task/scheduler.rs` `drive_expired_wake_deadlines`'s
   `wake_task_v2(*id)` call.

**Root cause.** `drive_expired_wake_deadlines` (scheduler.rs, ~5189
in the pre-fix tree) collected `TaskId`s with expired
`wake_deadline` under `scheduler_lock`, dropped the lock, then
iterated calling `wake_task_v2(id)` for each. Between collection
and the wake, a task could:

1. Wake naturally (its `wake_deadline` cleared, state → `Ready`).
2. Resume, process whatever woke it.
3. Re-block on a different state (e.g. `BlockedOnReply` via
   `call_msg` with no deadline).

The stale wake then fired against the **new** block state,
transitioning `BlockedOnReply → Ready` with no `pending_msg` set —
the caller's `block_current_on_reply_v2` returned
`DeadlineExpired`, `call_msg`'s `take_message` returned `None`, and
the syscall produced `u64::MAX` to userspace.

The old comment in `collect_expired_wake_deadlines` had asserted
spurious collections were harmless because `wake_task_v2` returns
`AlreadyAwake` for non-`Blocked*` states. That reasoning missed
the case where the task had **re-entered** a `Blocked*` state for
an unrelated reason between collection and the wake.

**Fix (`c51ada1`).** `collect_expired_wake_deadlines` now returns
`(TaskId, u64)` tuples carrying the deadline observed at
collection time. `drive_expired_wake_deadlines` re-acquires
`scheduler_lock` briefly before each wake and verifies the task's
current `wake_deadline` still matches the collected value. Skip if
not — the task has either woken naturally and re-blocked with a
different deadline, or has no deadline at all (the bug case).

**Residual.** A small race window remains between the re-validation
`scheduler_lock` release and `wake_task_v2`'s `pi_lock` acquire.
Empirically: pre-fix runs saw 32 `Io(-32)` events / ~50 s boot;
post-fix runs see 1. The `audio-demo` / `audio_client` retry stays
as defence in depth and now almost never fires. If the residual
becomes load-bearing, the next step is to refactor `wake_task_v2`
to accept a precondition closure evaluated under `pi_lock` so the
deadline check happens atomically with the state CAS.

**Diagnostic infrastructure kept in tree.**

- `kernel/src/ipc/mod.rs::log_ipc_umax` (32-hit boot budget,
  `IPC_UMAX_DIAG_BUDGET`) — discriminator strings cover every
  `u64::MAX` return site in IPC.
- `kernel/src/task/scheduler.rs::log_spurious_block_wake` and
  `log_wake_blocked_on_reply` (32-hit boot budgets,
  `SPURIOUS_BLOCK_WAKE_BUDGET` / `WAKE_REPLY_DIAG_BUDGET`) —
  catch any future regression of the unpaired-wake shape.

Future kernel-side IPC bugs of the same family should start by
grepping `/tmp/serial.log` for `[sched] wake_task_v2 on
BlockedOnReply` and `[ipc] u64::MAX diag:` — either is silent on a
healthy boot.

### 2. Scheduler watchdog false-positive on idle `audio_server` — fixed in `f2040a8`

**Original symptom.** With no client connected, `audio_server`'s
30 s stuck-no-waker watchdog fired and dumped a 60-KB trace ring:

```
[WARN] [sched] task pid=16 name=fork-child state=BlockedOnNotif stuck-since=30001ms (no waker registered)
[WARN] [sched] dumping trace rings (deferred from earlier signal)
=== TRACE RING DUMP (last 256 per core) ===
... (thousands of lines)
```

The server was actually fine — parked in `recv_with_capacity`
waiting on either the IPC endpoint queue or its bound notification
(vector `0x62`). The watchdog's "no waker registered" verdict
checked `task.wake_deadline.is_none() && task.state ==
BlockedOnNotif` and concluded the task was unwakeable, but
`BlockedOnNotif` for `recv_msg_with_notif` is inherently a "wake
from either side" state — the wake source is the bound notification,
not a deadline.

**Fix (`f2040a8`).** `watchdog_scan()` now extends the existing
`BlockedOnRecv && wake_deadline.is_none()` skip to also cover
`BlockedOnNotif && wake_deadline.is_none() &&
task_has_bound_notif(idx)`. The check uses a new lock-free
`crate::ipc::notification::task_has_bound_notif` helper that reads
`TCB_BOUND_NOTIF[task_sched_idx]` — the same source
`recv_msg_with_notif` consults to opt into the message-or-
notification fast path. The verdict still fires for tasks parked in
`BlockedOnNotif` **without** a bound notification (e.g. a
`notify_wait` whose signaler was lost) or with an expired
`wake_deadline`, so real lost-wake bugs surface.

This closes the false-positive for the broader class of IRQ-bound
ring-3 drivers (every driver that uses
`device_host.irq_subscribe → IrqNotification::bind_to_endpoint`),
not just `audio_server`.

## Files to read first

For a future regression of the kernel IPC `u64::MAX` race
(diagnostic infrastructure stays in tree — start here):

1. **This document.**
2. `kernel/src/ipc/mod.rs` — `log_ipc_umax` helper +
   `IPC_UMAX_DIAG_BUDGET` (32 hits/boot). Tagged at every kernel
   site that returns `u64::MAX` from `ipc_call_buf` /
   `ipc_send_with_bulk` / `endpoint::call_msg` /
   `endpoint::recv_msg_with_notif`.
3. `kernel/src/task/scheduler.rs` — `log_spurious_block_wake`
   (`SPURIOUS_BLOCK_WAKE_BUDGET`) and `log_wake_blocked_on_reply`
   (`WAKE_REPLY_DIAG_BUDGET`), each capped at 32 hits/boot.
4. `kernel/src/task/scheduler.rs::drive_expired_wake_deadlines` —
   the re-validation pattern that closed the race in `c51ada1`.
   Same pattern applies to any future collect-then-wake site.
5. `userspace/lib/audio_client/src/lib.rs:380-423` —
   `SyscallSocket::call`, the only producer of
   `AudioClientError::Io(-32)`.
6. `userspace/audio-demo/src/main.rs:205-240` — `submit_tone`'s
   per-chunk loop with the `Io(-32)` retry (now defence in depth
   rather than the primary mitigation).

For the watchdog suppression:

1. `kernel/src/task/scheduler.rs::watchdog_scan` — the
   `BlockedOnNotif && task_has_bound_notif` skip introduced in
   `f2040a8`.
2. `kernel/src/ipc/notification.rs::task_has_bound_notif` —
   lock-free read of `TCB_BOUND_NOTIF` (the same source
   `recv_msg_with_notif` consults).

For the now-resolved WAV-silent issue (kept for context):

1. `xtask/src/main.rs:assert_wav_non_silent` — windowed checker.
2. `xtask/src/main.rs:audio_smoke_qemu_args` — the QEMU args we
   pass: `-audiodev wav,id=snd0,path=…` and
   `-device AC97,audiodev=snd0,addr=0x5`. Note QEMU's wav backend
   defaults to 44.1 kHz S16 stereo regardless of our codec rate.

## Key constants (current state)

| Symbol | Value | Source |
|---|---|---|
| AC'97 PCI ID | `8086:2415` | `userspace/audio_server/src/lib.rs:4` |
| AC'97 PCI BDF | `0000:00:05.0` | `xtask/src/main.rs` AC97_QEMU_AUDIO_DEVICE_FLAGS |
| `MAX_SUBMIT_BYTES` | 64 KiB (protocol cap) | `kernel-core/src/audio/protocol.rs:57` |
| `MAX_BULK_LEN` (kernel) | **80 KiB** (was 64 KiB) | `kernel/src/ipc/mod.rs:688` |
| `MAX_COPY_LEN` (user_range) | **96 KiB** (was 64 KiB) | `kernel-core/src/user_range.rs:7` |
| `BDL_ENTRIES` | 32 | `userspace/audio_server/src/device.rs:196` |
| `PCM_SLOT_STRIDE` | 512 B | `userspace/audio_server/src/device.rs:430` |
| `DEFAULT_PCM_RING_BYTES` | 16 KiB | `userspace/audio_server/src/device.rs:203` |
| `SUBMIT_CHUNK_BYTES` (audio-demo) | 8 KiB (half ring) | `userspace/audio-demo/src/main.rs:174` |
| audio_server pid | 16 | observed |
| audio_server reply_cap_handle | dynamic (read from `msg.data[3]`) | was hardcoded 1 — see Bug #2 |

## What I tried that didn't pan out

For the post-session WAV-silent issue (now resolved):

- **Adding ring-debug register dumps in `submit_frames`** — confirmed
  CR=0x1d, BDBAR is correct, LVI advances each submit, CIV advances
  (we saw 0x01 mid-submit, 0x1f after IRQ). All evidence the device
  is consuming what we post.
- **Looking for `GLOB_CNT` writes** — there are none. Possible cause
  but ultimately not needed; the silent-WAV diagnosis was wrong.

## What's covered by tests

- 73 host-side tests pass (`cargo xtask check`)
- 10 audio-smoke step-list / WAV / wait-pass-or-fail shape tests pass
  (`cargo test -p xtask --target x86_64-unknown-linux-gnu`)
- 4 `ClientRegistry` host tests cover admit / release /
  `force_release` / rate-limited rejection
- 1 `dispatch_message` host test pins the new Open-while-open
  takeover semantic
- Sample WAV at `target/audio-smoke/audio.wav` after a passing run:
  4.85 MB, 44.1 kHz S16 stereo, ~158K total non-silent samples
  (two audio-demo invocations back-to-back); loudest 1-second
  window ~99% non-silent (~88K/88K), max abs amplitude ~8.9K of
  32.7K matches the 0.3-of-full-scale tone the demo emits

## Out of scope

- The driver_runtime `REPLY_CAP_HANDLE = 1` convention is removed in
  this session, but other servers (`mouse_server`, `kbd_server`,
  `fat_server`, `session_manager::control`) still hardcode it. Most
  work because their cap layout puts the reply cap at slot 1; only
  driver-host clients with multiple pre-existing caps were broken.
  Worth migrating those servers to read `msg.data[3]` too, but it's
  not blocking any current test.
- `bell-smoke` (a sibling test) shares the same fix surface — it
  should now also work end-to-end given the IPC/IRQ changes, but
  hasn't been re-validated this session.
- The `audio-smoke` pre-push gate is now wired (`41346ff`); CI gate
  (PR-blocking) is still open. With the closing-session reliability
  (5/5 deterministic), adding the gate to PR CI is a small
  workflow-config change.
- A fully-atomic version of the `drive_expired_wake_deadlines` fix
  (precondition closure evaluated under `pi_lock` inside
  `wake_task_v2`) would close the residual ~1% race window. The
  current re-validate-then-wake pattern is empirically enough: 32
  → 1 `Io(-32)` events per ~50 s run, and `audio-demo`'s retry
  absorbs the residual. Defer until / unless the residual fires
  in a load-bearing path.

## Done-when

- (Done) `cargo xtask audio-smoke` exits 0 deterministically — both
  the step list AND the WAV non-silent check pass on every run in
  the closing session (5/5).
- (Done) The recorded WAV at `target/audio-smoke/audio.wav` has a
  1-second window with at least 5% of samples > |100| (typically
  reports ~99% on a passing run).
- (Done) `frames_consumed > 0` reported by `audio-demo`'s stats
  line.
- (Done) AC'97 IRQ vector `0x62` fires at least once during
  audio-demo's submit phase.
- (Done) `audio-demo` plays an audible 440 Hz tone from
  `cargo xtask run` / `cargo xtask run-gui` on a developer host
  with a PipeWire/PulseAudio-capable QEMU build.
- (Done) `cargo xtask audio-smoke` wired into the pre-push hook
  (`41346ff`).
- (Done) Kernel `ipc_call_buf == u64::MAX` race resolved (`c51ada1`)
  — see [Resolved follow-ups §1](#1-kernel-ipc_call_buf--u64max-race--fixed-in-c51ada1).
- (Done) Scheduler watchdog false-positive resolved (`f2040a8`)
  — see [Resolved follow-ups §2](#2-scheduler-watchdog-false-positive-on-idle-audio_server--fixed-in-f2040a8).
