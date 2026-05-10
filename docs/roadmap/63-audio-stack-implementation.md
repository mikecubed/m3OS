# Phase 63 - Audio Stack Implementation

**Status:** Planned
**Source Ref:** phase-63
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Builds on:** Replaces the Phase 57 `Ac97Backend` accounting-only stub with real PIO + DMA register writes that drive the existing Phase 57 D.2 helpers (`init_controller`, `open_pcm_out_stream`, `Ac97Logic`, `handle_pcm_out_irq`); adds a privileged PIO syscall family because Phase 55b's device-host surface is MMIO-only and AC'97's BARs are I/O-space; switches QEMU `-audiodev` from the stub `none` backend to PulseAudio under `run-gui` and to a WAV file under `audio-smoke`; extends the smoke gate from "conf loaded" to "frames consumed advanced AND the recorded WAV is non-silent".
**Primary Components:** `kernel-core/src/device_host/syscalls.rs`, `kernel/src/syscall/device_host.rs`, `userspace/lib/driver_runtime/src/pio.rs` (new), `userspace/audio_server/src/device.rs`, `xtask/src/main.rs`

## Milestone Goal

A user running `cargo xtask run-gui` can launch `/bin/audio-demo` from the `term` prompt and hear an audible 1-second 440 Hz tone on the host audio device, and can type `printf '\x07'` to hear the terminal bell. `cargo xtask audio-smoke` boots headless, runs `audio-demo` over the serial console, asserts `frames_consumed` advances via the existing `AudioControlCommand::GetStats` verb, and verifies the QEMU-recorded WAV is non-silent. The Phase 57 single-client EBUSY policy is preserved, and Phase 57 docs carry closure notes that point at this phase.

## Why This Phase Exists

Phase 57 shipped a fully wired audio stack — `audio_server` claims the AC'97 BDF, maps Phase 55a `DmaBuffer` resources, subscribes to the IRQ via Phase 55c bound notifications, and implements a single-client / single-stream registry with full host-test coverage of every state transition — except for the four lines that would actually move bytes through the AC'97 controller. The `cfg(not(test)) Ac97Backend` impl in `userspace/audio_server/src/device.rs:559-664` is an accounting stub: `init` just sets a flag, `submit_frames` advances `frames_submitted` without touching DMA, `handle_irq` returns `IrqEvent::None`. The Phase 57 H.1 `audio-smoke` gate was deliberately scoped to "conf-loaded" because the path beyond it could not yet be exercised.

Two structural blockers explain why D.2 stopped at the pure-helper layer:

1. **AC'97 BARs are I/O-space.** `kernel-core/src/device_host/audio_class.rs::AC97_BAR_LAYOUT.is_pio_only() == true`. The Phase 55b `sys_device_mmio_map` syscall filters PIO BARs and returns an error for them; there is no `sys_device_pio_*` syscall and no userspace `Pio<T>` wrapper. Without a way to emit `inb`/`outb` from ring 3 the existing pure helpers (which take an `MmioOps` seam) had nothing real to plug into.
2. **QEMU's `-audiodev` was wired to `none`.** `xtask/src/main.rs:50` pins the audio backend to `none,id=snd0`. Even with a perfect driver, every byte the AC'97 device receives is discarded. There is no audible output and no way for a smoke gate to verify "audio actually played."

Phase 63 closes both gaps without re-doing any of Phase 57's pure-logic work. The pure helpers, BDL ring math, and IRQ classification all stay; only the production `Ac97Backend` shell, the QEMU launchers, and the smoke harness change.

## Learning Goals

- See how a privileged port-I/O syscall is the minimum addition needed to host a PIO-only device driver in ring 3 (vs. memory-mapped drivers that ride Phase 55b unchanged).
- Understand the path from a userspace `submit_frames` byte buffer through the Phase 55a `DmaBuffer<T>` IOVA, the AC'97 Buffer Descriptor List, the LVI/CIV register pair, and back through an IRQ-driven completion counter.
- Learn how a smoke gate composes two independent assertions (a software counter `GetStats` plus a hardware-side WAV recording) so a regression in one path cannot silently mask the other.

## Feature Scope

### Privileged PIO syscall family

Add `SYS_DEVICE_PIO_READ` and `SYS_DEVICE_PIO_WRITE` to the Phase 55b device-host syscall block. The kernel validates that the caller holds a `Capability::Device` for the named BAR, that the BAR is PIO-classified, and that the offset+width fits the BAR's reported size; only then issues `inb/inw/inl` or `outb/outw/outl` on the caller's behalf. Userspace gets a thin `Pio<T>` wrapper in `driver_runtime` matching the shape of the existing `Mmio<T>`.

### Real `Ac97Backend` over PIO + DMA

`Ac97Backend` is rewritten to own a `DmaBuffer` for the BDL, a `DmaBuffer` for the PCM ring, an `Ac97PioBus` adapter that holds two `Pio<()>` instances (NAM at BAR0, NABM at BAR1) and dispatches `MmioOps` calls by `bar` parameter, and an `Ac97Logic` for the ring-state math. `init` calls `init_controller(&bus)` then `open_pcm_out_stream(&bus, bdl_iova)`; `submit_frames` copies bytes into the next free PCM-ring slot, calls `Ac97Logic::submit_buffer`, and writes the new LVI through PIO; `handle_irq` reads CIV/SR through PIO, calls `handle_pcm_out_irq` and `Ac97Logic::observe_irq`, and returns the typed event.

### Underrun recovery

`apply_irq_event` already records `Ac97Logic::observe_irq` underruns into the registry stats. Phase 63 extends the io loop so an underrun with an empty software ring zero-fills one BDL slot and reposts it, preventing the engine from staying halted past a missed deadline.

### QEMU `-audiodev` selection

`run-gui` defaults to `pa,id=snd0` (PulseAudio) on Linux hosts so frames become audible on the host. `audio-smoke` selects `wav,id=snd0,path=<smoke_dir>/audio.wav` so CI gets a deterministic, hardware-independent recording. Both modes keep the existing `-device AC97,audiodev=snd0,addr=0x5` flag verbatim — only the `audiodev` backend changes.

### `audio-smoke` asserts hardware consumed frames

The smoke gate runs `audio-demo` over the serial console, waits for `AUDIO_DEMO:PASS`, parses an `AUDIO_DEMO:stats consumed=<N> underruns=<M>` line that the demo emits before exit (sourced from the existing `GetStats` verb), and finally opens the WAV file and asserts ≥5% of samples are above the silence threshold. Three independent failure modes — driver dropped frames, IPC stats verb broken, QEMU audio path broken — produce three distinct error messages.

### Audible-bell smoke

A new `bell-smoke` step writes `printf '\x07'\n` to the serial console after `term` is registered and asserts `frames_consumed` advances via `GetStats` within 200 ms. The bell wiring (`screen.rs:185` BEL → `RenderCommand::Bell` → `main.rs:212` → `bell.rs::AudioClientBellSink`) is already complete from Phase 57 G.6 and is not modified by Phase 63.

### Multi-client policy unchanged

The Phase 57 D.5 `ClientRegistry` (single-client, BUSY-on-second-connect, rate-limited reject log, 13 host tests) is unchanged. The PIO/DMA rewrite never touches `client.rs`.

## Important Components and How They Work

### `kernel-core/src/device_host/syscalls.rs`

After this phase: declares `SYS_DEVICE_PIO_READ = 0x1125` and `SYS_DEVICE_PIO_WRITE = 0x1126`, with `DEVICE_HOST_LAST` updated to the new top. The `pin_constants` test in this file is extended to cover the two new numbers without renumbering the prior block.

### `kernel/src/syscall/device_host.rs`

After this phase: implements `sys_device_pio_read` / `sys_device_pio_write`. Both syscalls validate the caller's `Capability::Device`, look up the device's BAR layout from the existing claim slot, reject MMIO BARs (`-EINVAL`), check the offset+width is in range (`-ERANGE`), and dispatch to `x86_64::instructions::port::Port` for the actual `inb`/`outb`. No allocation; no logging on the hot path.

### `userspace/lib/driver_runtime/src/pio.rs` (new)

After this phase: declares `Pio<T>`, mirroring the shape of `Mmio<T>` but dispatching reads/writes through the new syscalls instead of dereferencing a mapped pointer. Constructed via `Pio::<T>::map(&DeviceHandle, bar_index)`; method surface is `read_u{8,16,32}` / `write_u{8,16,32}`. `Drop` is a no-op (no MMIO mapping; the `DeviceHandle` owns the underlying lifetime).

### `userspace/audio_server/src/device.rs`

After this phase: the `cfg(not(test)) Ac97Backend` owns `device: DeviceHandle`, `bus: Ac97PioBus`, `bdl: DmaBuffer<[BufferDescriptor; BDL_ENTRIES]>`, `pcm_ring: DmaBuffer<[u8; DEFAULT_PCM_RING_BYTES]>`, `logic: Ac97Logic`, `stream_open: bool`. `init` chains `init_controller(&bus)` → `open_pcm_out_stream(&bus, bdl.iova())`; `submit_frames` copies bytes into the next free PCM-ring slot, calls `logic.submit_buffer`, and writes the new LVI through PIO; `handle_irq` calls `handle_pcm_out_irq(&bus, ring_was_empty)` plus `logic.observe_irq(sr, civ)`. The four standalone accounting counters (`frames_submitted`, `frames_consumed`, `underrun_count`, `initialised`) are removed — `Ac97Logic` is the single source of truth.

`Ac97PioBus` is a small adapter living in the same file: holds two `Pio<()>` instances and implements `MmioOps` by dispatching on the `bar` parameter. It is the seam where the existing `init_controller<M: MmioOps>` etc. plug into real hardware.

### `userspace/audio_server/src/irq.rs`

After this phase: `apply_irq_event` keeps its existing `IrqEvent::Underrun` → `record_underrun` mapping, and the production io loop adds a one-call zero-frame repost so the BDL re-arms after underrun. No change to `dispatch_message` / `encode_outcome` — the existing `AudioControlCommand::GetStats` verb already returns the data the smoke gate needs.

### `xtask/src/main.rs`

After this phase: the `AC97_QEMU_AUDIO_FLAGS` constant splits into `AC97_QEMU_AUDIO_FLAGS_GUI` (PulseAudio on Linux, `none` elsewhere with a warning) and `AC97_QEMU_AUDIO_FLAGS_HEADLESS` (WAV file). `cmd_run_gui` defaults to GUI flags with `--no-audio` opt-out; `cmd_audio_smoke` always selects the WAV variant. `audio_smoke_steps` is extended to: spawn `audio-demo`, wait for `AUDIO_DEMO:PASS`, wait for `AUDIO_DEMO:stats consumed=[1-9]\d*`, send a BEL byte after `term` is up, sample `GetStats` again, and then assert the recorded WAV is non-silent in `cmd_audio_smoke`'s post-QEMU step.

### `userspace/audio-demo/src/main.rs`

After this phase: emits one new line before exit — `AUDIO_DEMO:stats consumed=<N> underruns=<M>` — sourced from a `GetStats` request after `drain` succeeds. No change to the tone-generation, open, submit, or close paths.

## How This Builds on Earlier Phases

- Reuses the entire Phase 57 D.2 pure-logic surface (`init_controller`, `open_pcm_out_stream`, `close_pcm_out_stream`, `handle_pcm_out_irq`, `Ac97Logic`, `classify_sr`, `sr_ack_value`, `cr_*_value`).
- Reuses the Phase 57 D.3 `StreamRegistry` and D.5 `ClientRegistry` unchanged.
- Reuses the Phase 57 D.4 `irq::run_io_loop`, `dispatch_message`, `encode_outcome`, and `apply_irq_event` with one additive change (underrun-zero-fill repost).
- Reuses the Phase 57 G.6 `term::bell::Bell` + `AudioClientBellSink` unchanged — the bell becomes audible the moment the backend stops dropping frames.
- Reuses the Phase 55a `DmaBuffer<T>` for the BDL and the PCM ring with no kernel change.
- Reuses the Phase 55b `DeviceHandle` claim path and the BAR coverage assertion (which already classifies AC'97 as PIO-only and skips IOMMU coverage for it).
- Reuses the Phase 55c `IrqNotification::bind_to_endpoint` IRQ-multiplex pattern unchanged.
- Extends the Phase 55b syscall block by two numbers; nothing else in `kernel-core/src/device_host` changes.

## Implementation Outline

TDD-first: write host-side tests for the new PIO syscall (input validation, width bounds, MMIO-BAR rejection) before any kernel impl, and write a contract-shim test for `Pio<T>` before the production type. Then wire `Ac97Backend` to the existing pure helpers — every behavioral invariant for the helpers themselves is already host-tested in `device.rs::tests`, so the rewrite is mostly mechanical glue. Only after the gates above pass does the QEMU smoke harness change so a stub regression cannot pass the new assertion.

1. Add `SYS_DEVICE_PIO_READ`/`SYS_DEVICE_PIO_WRITE` numbers in `kernel-core::device_host::syscalls`; extend the pin tests.
2. Implement the kernel-side syscalls with full validation; cover with kernel-core unit tests for accept/reject paths.
3. Add `Pio<T>` in `driver_runtime`; implement against a `PioContract` trait double for host tests; re-export from `lib.rs`.
4. Add `Ac97PioBus` in `userspace/audio_server/src/device.rs` and an `MmioOps` impl for it.
5. Replace the `cfg(not(test)) Ac97Backend` definition with the real one; chain `init_controller` + `open_pcm_out_stream` from `init`; route `submit_frames` and `handle_irq` through the new fields. Existing host tests in `device.rs::tests` (which use `FakeMmio`) must still pass without modification.
6. Extend `apply_irq_event` callers with the underrun-zero-fill repost.
7. Split `AC97_QEMU_AUDIO_FLAGS` into GUI/headless variants; rewire `cmd_run_gui` and `cmd_audio_smoke`.
8. Extend `audio-demo` to print `AUDIO_DEMO:stats ...`; extend `audio_smoke_steps` for the new assertion; add the WAV non-silence check; add the BEL sub-step.
9. Add Phase 57 closure notes to `docs/roadmap/57-audio-and-local-session.md` and `docs/roadmap/tasks/57-audio-and-local-session-tasks.md`.
10. Bump the kernel version to `0.63.0`; update `AGENTS.md` and `docs/roadmap/README.md`.

## Acceptance Criteria

- `cargo xtask audio-smoke` passes with the new `consumed=` assertion plus the WAV non-silence check; the same gate fails when run against a scratch revert of the Track A rewrite (confirmed by reverting and re-running).
- `cargo xtask run-gui` plays an audible 440 Hz tone when `/bin/audio-demo` is invoked from `term`, and an audible beep when the user types `printf '\x07'`.
- A second `audio_client` open returns `-EBUSY`; no regression from Phase 57.
- Phase 57 design and task docs carry Phase 63 closure notes referencing the new assertion.
- `kernel/Cargo.toml` is at `0.63.0`; `AGENTS.md` and `docs/roadmap/README.md` reflect the bump.

## Companion Task List

- [Phase 63 Task List](./tasks/63-audio-stack-implementation-tasks.md)

## Manual Smoke Checklist

> **Note (Phase 63 Track E.2):** This checklist will be migrated into
> `docs/63-audio-stack-implementation.md` when Track H creates that document.
> It is placed here as a findable placeholder in the meantime.

The headless `cargo xtask audio-smoke` and `cargo xtask bell-smoke` gates
provide CI-deterministic coverage. The steps below confirm human-audible
output on the host audio device — the final proof that the AC'97 backend
emits real PCM rather than just advancing software counters.

### Step 1 — Audible 440 Hz tone via `audio-demo`

1. Run `cargo xtask run-gui` (default audio: PulseAudio on Linux, `none`
   elsewhere).
2. Wait for the `term` graphical terminal prompt to appear.
3. Type `/bin/audio-demo` and press Enter.
4. **Expected:** A 1-second 440 Hz tone is audible on the host audio device.
   Serial output shows `AUDIO_DEMO:PASS`.

### Step 2 — Audible BEL beep via `printf '\x07'`

1. In the same `term` session from Step 1 (or re-launch with
   `cargo xtask run-gui`).
2. Type `printf '\x07'` and press Enter.
3. **Expected:** A short audible beep (~30 ms at 880 Hz, per Phase 57 G.6
   `BELL_TONE_FREQ_HZ` / `BELL_DURATION_MS`) is heard on the host audio
   device.

### Failure interpretation

| Symptom | Likely cause |
|---|---|
| No tone from `audio-demo`, `AUDIO_DEMO:PASS` absent | `audio_server` stub mode (AC'97 device not present in QEMU) |
| Tone inaudible but `AUDIO_DEMO:PASS` printed | Host audio sink not configured (check `pa` vs `none` backend) |
| BEL step silent, `AUDIO_STATS:FAIL:consumed=0` | Bell wiring not reaching `Ac97Backend`; check `term::bell::AudioClientBellSink` |
| `cargo xtask run-gui` no audio at all | Missing `--no-audio` was not passed; check QEMU `-audiodev` in build log |

## How Real OS Implementations Differ

- Linux PulseAudio and PipeWire implement ALSA-level ring-buffer management; the kernel ALSA layer handles BDL setup and position reporting through a well-defined `snd_pcm_ops` interface, and userspace daemons multiplex many clients via a mixer.
- AC'97 is a legacy interface; modern systems use Intel HDA with a more complex command/response ring (CORB/RIRB) and a codec discovery handshake. m3OS picks AC'97 to keep the first audio learning surface as small as possible; HDA is the natural follow-up.
- Production audio servers report latency in nanoseconds, implement dynamic BDL sizing, and offer per-client volume control. m3OS uses a fixed 32-entry BDL and a 16 KiB PCM ring.

## Deferred Until Later

- Multi-client mixing and sample-rate conversion
- Intel HDA (Intel High Definition Audio) driver
- Audio capture (record) path
- ALSA-compatible IPC protocol
- Latency reporting to clients
- Per-client volume control
- Variable-rate audio (the AC'97 VRA bit is set during init but the rate is pinned at 48 kHz)
