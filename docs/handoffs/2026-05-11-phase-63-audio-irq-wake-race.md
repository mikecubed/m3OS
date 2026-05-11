---
status: resolved-with-followups
branch: feat/phase-63-audio-stack-implementation
last-known-good-commit: 374ffe7
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
> closed across the 2026-05-11 sessions. The file is kept under the
> same name so existing references don't break. Two non-blocking
> follow-ups remain — see [Known follow-ups](#known-follow-ups).

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
- Two non-blocking follow-ups: the underlying kernel
  `ipc_call_buf == u64::MAX` race is masked but not fixed, and the
  scheduler's stuck-no-waker watchdog dumps a misleading trace
  ring after 30 s of audio_server idle. See [Known follow-ups](#known-follow-ups).

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

## Known follow-ups

These are non-blocking — `audio-demo` works end-to-end and
`audio-smoke` is deterministic — but worth tracking because both
sit at boundaries that other phases will touch.

### 1. Underlying `ipc_call_buf == u64::MAX` race (masked, not fixed)

`Io(-32)` is the `audio_client::SyscallSocket::call` branch:

```rust
let reply_label = syscall_lib::ipc_call_buf(
    self.endpoint, LABEL_AUDIO_CMD, LABEL_AUDIO_CMD, &combined[..total],
);
if reply_label == u64::MAX {
    return Err(AudioClientError::Io(-32)); // EPIPE-shaped
}
```

The kernel returns `u64::MAX` from `ipc_call_buf` on ~10–20% of
sends in this codebase under TCG; the user-visible failure is
absorbed by the new client-side retry, but the underlying race
still exists. Plausible causes, in rough order:

A) **audio_server still parked in `recv` from the previous `Open`
   reply** when audio-demo's first `SubmitFrames` syscall reaches
   the kernel, but in a state where the send path treats the
   endpoint as "no receiver" rather than queuing. The
   bound-notification fix in `f1573bd` covers stale wake tokens but
   not necessarily `u64::MAX` returns from `ipc_call_buf`.

B) **Bulk payload size + queue depth interaction.** Submit ships a
   `frame_header (16 B) + 8 KiB PCM = 8208 B` bulk payload. Even
   though `MAX_BULK_LEN = 80 KiB`, the kernel may reject under some
   transient send-queue / receiver-not-ready combination.

C) **driver_runtime reply-cap reuse.** Bug #2's fix in `58bbbc8`
   moved audio_server to dynamic `reply_cap_handle` from
   `msg.data[3]`. If a prior verb leaks the reply cap slot, the
   next `recv` could land the new reply cap somewhere unexpected —
   but that should surface on the server, not the client.

**Recommendation.** This work should land alongside any other
ring-3-driver client that ships through `audio_client`-shaped
syscalls (mouse/keyboard/audio/display all share the same
`SyscallBackend`):

1. **Move the `Io(-32)` retry from `audio-demo` into
   `audio_client::SyscallSocket::call`** so every client benefits
   without each binary repeating the loop. The retry budget is the
   same kind of "transient back-pressure" the `Server:WouldBlock`
   branch already encodes; cap the audio-demo loop at a smaller
   number once the client-level retry exists.
2. **Add a kernel-side reason byte to `ipc_call_buf`** instead of
   the binary `u64::MAX` sentinel. A typed errno (`-EPIPE` vs
   `-EAGAIN` vs `-ENOENT`) would narrow A vs B vs C in one log run
   and let `audio_client` retry only on truly-transient cases.
3. **Audit `audio_server`'s recv-loop transition** between `Open`'s
   `reply()` and the next `recv()`. The window where a new `call`
   from the demo could see "no receiver" is the most likely
   integration seam.

### 2. Scheduler watchdog false-positive on idle `audio_server`

When `audio_server` sits with no client connected, the scheduler's
30 s stuck-no-waker watchdog fires:

```
[WARN] [sched] task pid=16 name=fork-child state=BlockedOnNotif stuck-since=30001ms (no waker registered)
[WARN] [sched] dumping trace rings (deferred from earlier signal)
=== TRACE RING DUMP (last 256 per core) ===
... (thousands of lines)
```

The server is actually fine — it's in `recv_with_capacity` waiting
on either the IPC endpoint queue or its bound notification (vector
0x62). Both wake sources are registered. The watchdog's
"no waker registered" check reads `task.wake_deadline.is_none() &&
task.state == BlockedOnNotif` and concludes the task is
unwakeable, but `BlockedOnNotif` for `recv_msg_with_notif` is
inherently a "wake from either side" state — the wake source is
the notification subscription, not a deadline.

The dump is also user-visible as a giant block of repeating
scheduler trace lines that the user reasonably mistakes for a
crash — the m3os-irq.log thread spent a session resolving this
exact misread.

**Recommendation.** Two complementary changes:

1. **Suppress the stuck-no-waker verdict for tasks that hold a
   live `BoundNotification` subscription.** The
   `device_host.irq_subscribe` path already records the
   subscription; the watchdog can check it and skip the warning.
   Quick fix; closes the false-positive on `audio_server` and the
   class of IRQ-bound drivers more generally.
2. **Replace the trace-ring dump with a per-task one-liner.** Even
   when the warning is correct (some other lost-wake), a 60-KB
   serial dump is the wrong signal-to-noise; the relevant info is
   the task's last scheduling event and the registered wake
   sources. The full trace ring is still available via the
   user-mode dump command.

## Files to read first

In this order, for the kernel `Io(-32)` follow-up:

1. **This document.**
2. `userspace/lib/audio_client/src/lib.rs:380-423` —
   `SyscallSocket::call`, the only producer of
   `AudioClientError::Io(-32)` and `Io(-5)`. Candidate site for
   moving the retry from `audio-demo` into the shared client.
3. `userspace/audio-demo/src/main.rs:205-240` — `submit_tone`'s
   per-chunk loop with the temporary `Io(-32)` retry.
4. `kernel/src/ipc/mod.rs` — `ipc_call_buf` and the conditions
   under which it returns `u64::MAX`. Look for the "no receiver"
   / "would block on send" branches and consider exposing a typed
   reason byte.
5. `userspace/audio_server/src/irq.rs` — `run_io_loop`, the
   `recv → handle → reply → recv` cycle. The transition from
   `Open`'s reply back to `recv` is the candidate race window.

For the watchdog follow-up:

1. `kernel/src/task/scheduler.rs:5495-5600` — `WatchdogVerdict`,
   `dump_dispatch_state`, and the trace-ring dump trigger.
2. `kernel/src/ipc/notification.rs:739+` — bound-notification
   wake bookkeeping.
3. `kernel/src/ipc/endpoint.rs:657-700` — `recv_msg_with_notif`'s
   wake-source registration around the v2 block site.

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
- The Phase 63 audio-smoke gate is not yet in PR CI
  (`cargo xtask check` only). With the closing-session reliability
  (5/5 deterministic), wiring `audio-smoke` into the pre-push hook
  is a one-line config change.

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
- **Now-actionable**: wire `cargo xtask audio-smoke` into the
  pre-push hook now that pass-rate is 100%.
- **Follow-ups (non-blocking)**: see [Known follow-ups](#known-follow-ups)
  for the underlying `ipc_call_buf == u64::MAX` race and the
  scheduler watchdog false-positive.
