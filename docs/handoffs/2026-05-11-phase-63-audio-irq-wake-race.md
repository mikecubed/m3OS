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
> the AC'97 IRQ pipeline is fixed (this session), and the file is kept
> under the same name so existing references don't break. The active
> investigation now is the QEMU `-audiodev wav` backend recording an
> empty data chunk despite our driver advancing `frames_consumed`.

## ⚠ Status update (2026-05-11 — second session)

**`audio-smoke` step list now PASSES end-to-end.** `audio-demo` runs
through `Open → SubmitFrames → Drain → GetStats → Close → PASS`,
audio_server programs the AC'97 BDL ring, the controller actually
consumes the buffers (`CIV` advances, vector `0x62` fires), and
`frames_consumed` reads back at ~88,000–91,000 across runs.

**The only remaining failure is the WAV non-silent assertion**:
QEMU's `-audiodev wav` backend creates the file with a valid RIFF
header but writes a zero-length `data` chunk. The smoke command
returns `SMOKE_EXIT_WAV_SILENT` (= 63).

```
[step 16] wait-line-not-matching: guest/audio: frames_consumed non-zero (5s)
audio-smoke: PASSED (16 steps in 33s)
audio-smoke: WAV non-silent check FAILED
WAV file /…/target/audio-smoke/audio.wav 'data' chunk is empty (0 samples)
exit=63
```

The driver path is healthy. The remaining issue is a QEMU AC'97 ↔ wav
backend integration question.

## TL;DR (current state)

- Pre-session: `audio-demo` never ran — keystrokes hit `m3OS login:`.
  audio_server parked in `BlockedOnNotif` waiting for a client that
  never connected. The handoff blamed the AC'97 IRQ pipeline; the real
  set of bugs was much wider.
- Post-session: 10 bugs across the IPC/driver_runtime/audio_server
  stack fixed. AC'97 IRQs DO fire. Backend reports `frames_consumed >
  0`. Smoke step list PASSes.
- Open: WAV recording is empty. Our driver thinks it shipped 88K+
  samples through the BDL, QEMU's wav backend disagrees. Either QEMU
  is silently dropping the audio, or our PCM bytes never make it onto
  the AC'97 codec data path even though the BDL is being walked.

## Reproduction

```bash
git checkout feat/phase-63-audio-stack-implementation
git pull
M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log cargo xtask audio-smoke
```

Expected output (current):

```
[step 15] wait-pass-or-fail: guest/audio: audio-demo PASS sentinel (30s)
[step 16] wait-line-not-matching: guest/audio: frames_consumed non-zero (5s)
audio-smoke: PASSED (16 steps in 33s)
audio-smoke: WAV non-silent check FAILED
WAV file /home/.../target/audio-smoke/audio.wav 'data' chunk is empty (0 samples)
```

Look for these markers in `/tmp/serial.log` to confirm the driver path
is healthy:

- `audio_server: spawned` — driver started
- `device_host.irq_subscribe pid=16 … vector=0x62 notif=0 bit=0` — INTx routed
- `AUDIO_DEMO:opened` / `AUDIO_DEMO:submitted` / `AUDIO_DEMO:drained`
- `AUDIO_DEMO:PASS`
- `AUDIO_DEMO:stats consumed=N underruns=0` with `N > 0` (typically ~88,000–91,000)

If a previous regression resurfaces, see "Bugs already closed" below
for the failure signature each one produced.

## Bugs closed in the 2026-05-11 session

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

## What's actually still broken — empty WAV under QEMU AC'97 + wav

After the driver-side close-out, the failure mode is:

- audio_server's `Ac97Logic` reports `frames_consumed = 88,832`
  (varies run-to-run between ~87,800 and ~91,400 — consistent with one
  ring's worth of slot-completion counts)
- AC'97 vector `0x62` fires (we observed it in early-session diag logs)
- Hardware `CIV` advances past every BDL slot we post (`PICB` cycles
  through the 256-sample range each time)
- BDL phys_addr fields are correct (32-bit IOVAs in low 4 GiB,
  validated by `check_iova_fits_u32`)
- BUT: `target/audio-smoke/audio.wav` ends with a valid RIFF header
  followed by a zero-length `data` chunk

Either:

A) **QEMU's wav backend isn't actually wired to the AC'97 codec
   output path under our config.** Possible: a missing `-audiodev`
   flag combination, an AC'97 model option we're not setting, or
   a QEMU version where `AC97 + wav` was broken upstream.

B) **Our codec is mute or programmed at a rate the wav backend
   discards.** The driver writes `MASTER_VOLUME = 0x0202` and
   `PCM_OUT_VOLUME = 0x0202` (≈ -3 dB, mute clear) and 48 kHz to
   `PCM_FRONT_DAC_RATE` after enabling VRA — looks correct, but the
   wav-side actually-getting-samples question is open.

C) **The BDL slots get walked but our PCM data isn't being read.**
   Could be a DMA-coherency / IOMMU translation issue: the BDL
   `phys_addr` is a 32-bit IOVA but the device-host's
   `dma_alloc.identity` log line shows the IOVA and the physical
   address are the same, so this seems unlikely.

D) **AC'97's CIV advances on a free-running timer regardless of
   sample availability**, so our `frames_consumed` is fool's gold
   and the codec was never actually pumping anything — but that
   contradicts the IRQ firing on completion.

## Concrete next-step plan

Pick the cheapest experiment first.

### Hypothesis A — `-audiodev wav` is misconfigured / unsupported

1. **Check QEMU version for known AC97+wav bugs.**
   `qemu-system-x86_64 --version`. Search QEMU changelog for AC97
   regressions around the installed version.
2. **Try `-audiodev pa` against PipeWire.** The user confirmed
   PipeWire-only (no PulseAudio daemon). PipeWire ships
   `pipewire-pulse` as a separate package on most distros; if it's
   installed, `pa` may transparently route through PipeWire. On a
   bare PipeWire install without `pipewire-pulse`, this won't help —
   but it's a one-flag test.
3. **Try `-audiodev sdl` or `-audiodev alsa`** if available, just to
   confirm the AC'97 ↔ audiodev path works for SOME backend. If
   nothing produces samples, the bug is QEMU-side AC'97 emulation.
4. **Switch to `-device intel-hda` + `-device hda-output`** instead
   of AC97 — newer device, better-tested in QEMU. This is a larger
   change because the driver class is different, but isolates whether
   the issue is AC97-specific.

### Hypothesis B/C — driver isn't actually shipping audible PCM

5. **Dump a few BDL entries + the first few PCM bytes after submit.**
   Confirm they aren't zero. `audio-demo`'s sine is non-silent by
   construction, but a stride/copy bug could be replacing them with
   zeros.
6. **Disable `submit_frames_inner`'s ring-copy and write a static
   non-zero pattern to every PCM ring slot.** If WAV stays silent,
   the issue is downstream of the ring. If WAV gets data, the issue
   is in the BDL ↔ ring linkage.
7. **Read `GLOB_CNT` (NABM 0x2C) and ensure GIE / cold-reset bits
   are set the way QEMU expects.** We never write `GLOB_CNT`; the
   default may or may not be enough. The `GLOB_STA` we read post-open
   shows `0x100` (PCR — Primary Codec Ready), so the codec is past
   reset, but `GIE` (bit 0 of `GLOB_CNT`) is a separate question.

### Hypothesis D — observe-irq overcounts

8. **Compare `frames_consumed` against `(SAMPLE_RATE × duration)`.**
   `audio-demo` ships ~1 second of stereo S16Le at 48 kHz =
   `48000 × 2 = 96000` samples. Reported `frames_consumed` is ~88K.
   Order-of-magnitude correct, suggests we're not over-counting.
   But: the unit might be "samples" vs "stereo frames" — clarify
   in `Ac97Logic::observe_irq`.

### Step-by-step recommended order

1. Run audio-smoke once with `M3OS_SMOKE_SERIAL_DUMP=/tmp/serial.log
   cargo xtask audio-smoke 2>&1 | tee /tmp/smoke.log` and confirm
   the current state matches this doc.
2. Check QEMU version: `qemu-system-x86_64 --version`.
3. Try the `pa` audiodev experiment (one-line change in
   `xtask/src/main.rs:audio_smoke_qemu_args`).
4. If `pa` produces audible samples, the bug is `wav`-specific and
   we should consider switching the smoke to capture via `pa`-into-
   `parec` or similar, OR file a QEMU bug.
5. If `pa` is also silent, the bug is AC'97-side. Move to
   Hypothesis B: dump BDL + ring contents post-submit.

## Files to read first

In this order, for the empty-WAV bug:

1. **This document.**
2. `userspace/audio_server/src/device.rs:355-410` — `init_controller`
   and `open_pcm_out_stream`. NAM mixer writes (volume, VRA, DAC
   rate) and NABM bus-master writes (BDBAR, LVI, CR.RPBM).
3. `userspace/audio_server/src/device.rs:495-560` — `submit_frames_inner`,
   the BDL programming and PCM ring-copy hot path.
4. `userspace/audio_server/src/device.rs:840-880` — `poll_completed_buffers`
   (the CIV-based fallback for non-IRQ environments) and the
   `submit_frames` + `poll_frames_consumed` trait impls.
5. `xtask/src/main.rs:6151-6177` — `audio_smoke_qemu_args`. The QEMU
   args we pass: `-audiodev wav,id=snd0,path=…` and
   `-device AC97,audiodev=snd0,addr=0x5`.
6. `xtask/src/main.rs:6303-6418` — `assert_wav_non_silent`. The
   format-validity + 5%-non-silent-samples check that's currently
   failing.

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

For the post-session WAV-silent issue (the active investigation):

- **Adding ring-debug register dumps in `submit_frames`** — confirmed
  CR=0x1d, BDBAR is correct, LVI advances each submit, CIV advances
  (we saw 0x01 mid-submit, 0x1f after IRQ). All evidence the device
  is consuming what we post.
- **Looking for `GLOB_CNT` writes** — there are none. Possible cause
  but not yet tested.

## What's covered by tests

- 73 host-side tests pass (`cargo xtask check`)
- All 6 audio-smoke step-list shape tests pass
  (`cargo test -p xtask --target x86_64-unknown-linux-gnu audio_smoke`)
- 3 consecutive `audio-smoke` runs all hit `frames_consumed` between
  87,808 and 91,392 — driver-side consistency confirmed

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

- `cargo xtask audio-smoke` exits 0 — both the step list AND the
  WAV non-silent check pass.
- The recorded WAV file at `target/audio-smoke/audio.wav` has at
  least 5% of samples with `|sample| > 100`.
- (Already true) `frames_consumed > 0` reported by `audio-demo`'s
  stats line.
- (Already true) AC'97 IRQ vector `0x62` fires at least once during
  audio-demo's submit phase.
