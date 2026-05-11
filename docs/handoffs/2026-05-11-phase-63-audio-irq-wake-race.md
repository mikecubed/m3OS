---
status: open
branch: feat/phase-63-audio-stack-implementation
last-known-good-commit: HEAD-of-feat/phase-63-audio-stack-implementation
date: 2026-05-11
component: end-to-end audio path — driver_runtime IPC ↔ audio_server ↔ AC'97 BDL ↔ QEMU wav backend
related:
  - docs/handoffs/2026-04-25-scheduler-design-comparison.md
  - docs/63-audio-stack-implementation.md
  - docs/handoffs/2026-04-28-graphical-stack-startup.md
---

# Handoff — Phase 63 audio path

> **Doc title note**: filename still says `audio-irq-wake-race` from the
> first issue this thread tracked. The wake-race is fixed (`f1573bd`),
> the AC'97 IRQ pipeline is fixed (`58bbbc8`), the WAV-silent issue is
> fixed (this session), and the file is kept under the same name so
> existing references don't break. The remaining concern is a low-rate
> intermittent IPC failure on the first `SubmitFrames` call.

## ⚠ Status update (2026-05-11 — third session)

**`audio-smoke` now PASSES end-to-end on most runs.** The step list
reaches `AUDIO_DEMO:PASS` with `frames_consumed > 0`, and the WAV
non-silent assertion now succeeds — the loudest 1-second window shows
~88% non-silent samples (~78K/88K @ 44.1 kHz stereo).

```
[step 16] wait-line-not-matching: guest/audio: frames_consumed non-zero (5s)
audio-smoke: PASSED (16 steps in 33s)
audio-smoke: WAV check PASSED — loudest 88200-sample window has 88% non-silent samples (78010/88200), total non-silent 78010/2426858
audio-smoke: WAV non-silent check PASSED
```

**Why the previous "empty data chunk" diagnosis was wrong.** QEMU's
`-audiodev wav` backend writes a *streaming* WAV: it leaves the RIFF
and `data` chunk-size fields at zero because the final length isn't
known when the file is opened. The actual PCM appears at the *end* of
the file — boot phase is captured as silence. The old checker read
the declared size, saw 0, and bailed before scanning the bytes that
were really on disk. A `hexdump` of the file confirmed the tone is
present, near the tail, at the expected ~0.3 amplitude (max abs
~8.9K of 32.7K i16 range).

**Remaining intermittent failure (~10–20% of runs).** The first
`audio-demo` `SubmitFrames` call sometimes returns
`AudioClientError::Io(-32)` (`EPIPE`-shaped), which is the audio_client
mapping for `ipc_call_buf` returning `u64::MAX`. The new FAIL line:

```
audio-demo failed at stage: submit variant=Io errno=-32
```

When this fires, the step list fails at step 15 and the WAV check
never runs. The smoke harness now waits for a newline-terminated FAIL
line before killing QEMU so the `variant=` and `errno=` fields are
always preserved. See **What's still flaky** below for the
hypotheses.

## TL;DR (current state)

- Pre-session-1: `audio-demo` never ran — keystrokes hit `m3OS login:`.
  10 IPC/driver_runtime/audio_server bugs fixed in `58bbbc8`.
- Pre-session-2 (this session): step list PASSed, but WAV non-silent
  failed with `'data' chunk is empty (0 samples)`. Driver-side
  evidence (CIV advancing, IRQs firing, `frames_consumed ~= 88K`)
  said the audio was getting through.
- Post-session-2: WAV checker rewritten to (a) tolerate
  `data_size == 0` headers by treating "rest of file" as the data
  payload, and (b) report the loudest sliding-window non-silent
  percentage instead of a whole-file average (which was always
  drowned by boot-phase silence). End-to-end PASS confirmed on 8/10
  runs.
- Open: ~10–20% intermittent `Io(-32)` from the first submit's
  `ipc_call_buf`. Not blocking on the WAV check; blocks the step
  list. New diagnostic (`errno=`) added — see follow-up plan.

## Reproduction

```bash
git checkout feat/phase-63-audio-stack-implementation
git pull
M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log cargo xtask audio-smoke
```

Expected output on a clean run (~80–90% of runs):

```
[step 15] wait-pass-or-fail: guest/audio: audio-demo PASS sentinel (30s)
[step 16] wait-line-not-matching: guest/audio: frames_consumed non-zero (5s)
audio-smoke: PASSED (16 steps in 33s)
audio-smoke: WAV check PASSED — loudest 88200-sample window has 88% non-silent samples (78010/88200), total non-silent 78010/2426858
audio-smoke: WAV non-silent check PASSED
```

Expected output on an intermittent submit failure (~10–20% of runs):

```
[step 15] wait-pass-or-fail: guest/audio: audio-demo PASS sentinel (30s)
audio-demo failed at stage: submit variant=Io errno=-32
(step 15 — guest/audio: audio-demo PASS sentinel)
```

Look for these markers in `/tmp/serial.log` to confirm the driver path
is healthy:

- `audio_server: spawned` — driver started
- `device_host.irq_subscribe pid=16 … vector=0x62 notif=0 bit=0` — INTx routed
- `AUDIO_DEMO:opened` / `AUDIO_DEMO:submitted` / `AUDIO_DEMO:drained`
- `AUDIO_DEMO:PASS`
- `AUDIO_DEMO:stats consumed=N underruns=0` with `N > 0` (typically ~88,000–91,000)

The WAV file should be ~4.8 MB at 44.1 kHz stereo (QEMU's wav backend
defaults — it rate-converts from our 48 kHz codec setting). Audio
content sits in the last ~1 second of the file; the rest is captured
boot silence.

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

## What's still flaky — `Io(-32)` on first `SubmitFrames` (~10–20%)

The remaining gap is an intermittent IPC failure. Across 10 back-to-back
runs in this session: 8 PASS end-to-end, 2 fail with
`audio-demo failed at stage: submit variant=Io errno=-32`.

`Io(-32)` is the `audio_client::SyscallSocket::call` branch:

```rust
let reply_label = syscall_lib::ipc_call_buf(
    self.endpoint, LABEL_AUDIO_CMD, LABEL_AUDIO_CMD, &combined[..total],
);
if reply_label == u64::MAX {
    return Err(AudioClientError::Io(-32)); // EPIPE-shaped
}
```

`ipc_call_buf == u64::MAX` is the kernel's "send failed" sentinel —
the call never reached audio_server. The most plausible candidates,
in rough order:

A) **audio_server still parked in `recv` from the previous
   `Open` reply** when audio-demo's first `SubmitFrames` syscall
   reaches the kernel, but in a state where the send path treats the
   endpoint as "no receiver" rather than queuing. The bound-notification
   fix in `f1573bd` covers stale wake tokens but not necessarily
   `u64::MAX` returns from `ipc_call_buf`.

B) **Bulk payload size + queue depth interaction.** Submit ships a
   `frame_header (16 B) + 8 KiB PCM = 8208 B` bulk payload. Even
   though `MAX_BULK_LEN = 80 KiB`, the kernel may reject under some
   transient send-queue / receiver-not-ready combination.

C) **driver_runtime reply-cap reuse.** The Bug #2 fix in
   `58bbbc8` moved audio_server to dynamic `reply_cap_handle` from
   `msg.data[3]`. If a prior verb leaks the reply cap slot, the next
   `recv` could land the new reply cap somewhere unexpected — but
   that should surface in audio_server, not in the *client* side
   ipc_call.

### Diagnostic next steps

1. **Capture a serial log of an `Io(-32)` run.** Run the smoke in a
   loop until it surfaces, then look for:
   - The last audio_server line before the demo failure — was it
     mid-`recv` from `Open`?
   - Any `[WARN] [ipc]` or `state-not-ready` entries near the
     `AUDIO_DEMO:opened` line that signal a send-side reject?
   - The kernel-side IPC trace ring if it dumped (the
     `stuck-no-waker` path also dumps device IRQ counters now).
2. **Add a kernel-side reason byte for `ipc_call`'s `u64::MAX`
   return.** Today the syscall is a binary success/fail. A typed
   error (`-EPIPE` vs `-EAGAIN` vs `-ENOENT`) would narrow A vs B vs C.
3. **Audit audio_server's recv-loop transition** between `Open`'s
   `reply()` and the next `recv()`. The window where a new `call`
   from the demo could see "no receiver" is the most likely
   integration seam.
4. **Audit client-side retry**. If the kernel's `u64::MAX` is
   semantically "would block on send", the audio-demo could treat
   `Io(-32)` as a `WouldBlock`-equivalent and retry up to N times.
   The `WouldBlock` retry path at audio-demo/src/main.rs:222
   already exists for the server-side WouldBlock case; extending it
   to `Io(-32)` is a 5-line change that would also confirm the
   diagnosis (if the retry succeeds, this is a recv-window race).

## Files to read first

In this order, for the residual `Io(-32)` intermittency:

1. **This document.**
2. `userspace/lib/audio_client/src/lib.rs:380-423` — `SyscallSocket::call`,
   the only producer of `AudioClientError::Io(-32)` and `Io(-5)`.
3. `userspace/audio-demo/src/main.rs:204-240` — `submit_tone`'s
   per-chunk loop and the `WouldBlock` retry. Candidate for
   extending the retry to `Io(-32)`.
4. `kernel/src/ipc/mod.rs` — `ipc_call_buf` and the conditions
   under which it returns `u64::MAX`. Look for the "no receiver"
   / "would block on send" branches and consider exposing a
   typed reason byte.
5. `userspace/audio_server/src/irq.rs` — `run_io_loop`, the
   `recv → handle → reply → recv` cycle. The transition from
   `Open`'s reply back to `recv` is the candidate race window.

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
- 9 audio-smoke step-list / WAV / wait-pass-or-fail shape tests pass
  (`cargo test -p xtask --target x86_64-unknown-linux-gnu`)
- Sample WAV at `target/audio-smoke/audio.wav` after a passing run:
  4.85 MB, 44.1 kHz S16 stereo, ~78K non-silent samples in the
  loudest 1-second window (max abs amplitude ~8.9K of 32.7K, matches
  the 0.3-of-full-scale tone the demo emits)

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
- The Phase 63 audio-smoke gate is not in PR CI (`cargo xtask check`
  only). After WAV-silent is closed, consider wiring `audio-smoke`
  into the pre-push hook so this regression class is caught.

## Done-when

- (Already true on a passing run) `cargo xtask audio-smoke` exits 0 —
  both the step list AND the WAV non-silent check pass.
- (Already true) The recorded WAV at `target/audio-smoke/audio.wav`
  has a 1-second window with at least 5% of samples > |100| (this
  session typically reports ~88%).
- (Already true) `frames_consumed > 0` reported by `audio-demo`'s
  stats line.
- (Already true) AC'97 IRQ vector `0x62` fires at least once during
  audio-demo's submit phase.
- **Still open**: `cargo xtask audio-smoke` is reliable
  enough to wire into the pre-push hook. Today: 80–90% pass rate.
  Closing the `Io(-32)` intermittency above gets us to ≥99% — a
  level that justifies CI-gating.
