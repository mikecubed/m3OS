# Audio Stack Implementation (Phase 63)

**Aligned Roadmap Phase:** Phase 63
**Status:** Complete
**Source Ref:** phase-63
**Supersedes Legacy Doc:** new — Phase 63 ships the first real PCM-emission path; there is no pre-existing learning surface to supersede.

## Overview

Phase 63 turns the Phase 57 audio stack from a host-testable accounting stub into a backend that actually emits PCM to hardware. Phase 57 shipped the full IPC surface (`audio_server`, `audio_client`, `AudioControlCommand::GetStats`), the BDL state machine (`Ac97Logic`), and the pure-logic register-write helpers (`init_controller`, `open_pcm_out_stream`, `handle_pcm_out_irq`), all gated on a `MmioOps` trait so host tests could exercise them with a `FakeMmio`. The production `cfg(not(test)) Ac97Backend` was left as an accounting stub — `init` set a boolean, `submit_frames` advanced a counter, and `handle_irq` returned `IrqEvent::None`. The user could open a stream and submit frames without ever hearing a tone.

Phase 63 closes that gap by introducing a privileged port-I/O syscall pair (`SYS_DEVICE_PIO_READ` = `0x1125`, `SYS_DEVICE_PIO_WRITE` = `0x1126`), a userspace `Pio<T>` wrapper mirroring the existing `Mmio<T>` shape, and an `Ac97PioBus` adapter that implements `MmioOps` over both AC'97 BARs. The real `Ac97Backend` then chains DMA allocation, `init_controller`, and `open_pcm_out_stream` from its `init()` call; `submit_frames` copies bytes into a 16 KiB PCM ring divided into 32 equal slots, posts the next BDL entry via `Ac97Logic::submit_buffer`, and advances the LVI register via the bus. `handle_irq` reads CIV, classifies the SR bits via `handle_pcm_out_irq`, and feeds the resulting `IrqEvent` through `Ac97Logic::observe_irq` so `frames_consumed` and `underrun_count` advance in lockstep with the hardware.

To verify the path end-to-end in CI, Phase 63 switches the `audio-smoke` xtask gate to use QEMU's WAV `-audiodev` backend, runs `audio-demo` inside the smoke, parses the recorded `audio.wav` file, and asserts that at least 5% of samples have `|sample| > 100`. A new `bell-smoke` gate spawns a small `bell-test` binary from the serial shell — `bell-test` calls into the same `term::bell::AudioClientBellSink` library that `term`'s ANSI parser uses, exercising the BEL path on the real backend without needing a `kbd_server` input-injection harness.

## What This Doc Covers

- The privileged PIO syscall family and the lock-step `Pio<T>` wrapper.
- The structure of the real `Ac97Backend`: DMA layout, init sequence, IRQ handler, and submit-frames path.
- The two new smoke gates (`audio-smoke` and `bell-smoke`) that prove `frames_consumed > 0` and audible output.
- How the BEL path from Phase 57 G.6 connects through to the new backend.
- The five-byte slot stride math that makes `submit_frames` zero-allocation.

## Key Files

| File | Role |
|---|---|
| `kernel-core/src/device_host/syscalls.rs` | `SYS_DEVICE_PIO_READ` / `SYS_DEVICE_PIO_WRITE` constants + pinned tests |
| `kernel-core/src/device_host/pio_validation.rs` | Host-testable PIO validation (width ∈ {1,2,4}, BAR type, offset+width range) |
| `kernel/src/syscall/device_host.rs` | Kernel-side `sys_device_pio_read` / `sys_device_pio_write` handlers (capability check + port I/O) |
| `userspace/lib/driver_runtime/src/pio.rs` | `Pio<T>` wrapper + `PioContract` trait double for host tests |
| `userspace/audio_server/src/device.rs` | Real `Ac97Backend`, `Ac97PioBus` adapter, DMA-backed BDL + PCM ring |
| `userspace/audio_server/src/irq.rs` | `repost_silence_after_underrun` zero-fill helper |
| `userspace/audio-demo/src/main.rs` | Emits `AUDIO_DEMO:PASS` and `AUDIO_DEMO:stats consumed=… underruns=…` after drain |
| `userspace/bell-test/src/main.rs` | Drives `Bell::ring` from the serial shell; emits `BELL_TEST:PASS` when `frames_consumed > 0` |
| `userspace/audio-stats/src/main.rs` | One-shot CLI that calls `AudioClient::get_stats()` without opening a stream |
| `userspace/lib/audio_client/src/lib.rs` | `AudioStats { underrun_count, frames_submitted, frames_consumed }` mirroring wire layout |
| `xtask/src/main.rs` | `cmd_audio_smoke` (WAV non-silent gate), `cmd_bell_smoke` (BEL → frames_consumed gate), `assert_wav_non_silent` helper |

## Core Implementation

### Privileged PIO syscall

`sys_device_mmio_map` rejects PIO BARs because the kernel cannot map I/O space into a user address window. AC'97 has two BARs (NAM at BAR 0, NABM at BAR 1) and both are I/O-only on real ICH silicon and in QEMU's `-device AC97` emulation. The new pair of syscalls bridges that gap: each call validates `Capability::Device` ownership of the BAR, validates that the BAR is PIO-typed (`is_pio_only() == true`), validates `width ∈ {1, 2, 4}` and `offset + width ≤ BAR_SIZE`, then issues `inb`/`inw`/`inl` (or `outb`/`outw`/`outl`) on the caller's behalf. The hot path performs no allocation and no logging.

The validation logic lives in `kernel-core/src/device_host/pio_validation.rs` so it is host-testable; the kernel handler is a thin wrapper that consults `DEVICE_HOST_REGISTRY` for capability/BAR lookups and forwards to the validated I/O helpers. Width-mismatch returns `-EINVAL`, MMIO-BAR access returns `-EINVAL`, out-of-range offset returns `-ERANGE`, missing capability returns `-EBADF`. These error codes are pinned by host tests so the wire contract cannot drift.

### `Pio<T>` wrapper and `Ac97PioBus` adapter

`Pio<T>` mirrors `Mmio<T>`: a thin generic over a typestate marker `T`, holding the device-cap handle plus the BAR index. `read_u{8,16,32}` and `write_u{8,16,32}` route through the new syscalls. `Pio::map` does not perform any probe read — the kernel validates ownership on every access, and reading some PIO registers has clear-on-read side effects, so a construction-time probe is explicitly forbidden. `Drop` is a no-op (PIO has no mapping to release).

`Ac97PioBus` holds two `Pio<()>` instances — one per AC'97 BAR — and implements the `MmioOps` trait that `init_controller`, `open_pcm_out_stream`, `close_pcm_out_stream`, and `handle_pcm_out_irq` are already generic over. The adapter dispatches strictly on the `bar` parameter; there is no shared state between the two windows. This is the seam where the production backend plugs in: the helpers compile against either `FakeMmio` (host tests) or `Ac97PioBus` (real hardware) without any code change to the helpers themselves.

### Real `Ac97Backend`

The production backend (`#[cfg(not(test))]`) now owns:

- `device: DeviceHandle` — the claimed AC'97 capability.
- `bus: Ac97PioBus` — the two-BAR PIO adapter.
- `bdl: DmaBuffer<[BufferDescriptor; BDL_ENTRIES]>` — 32-entry buffer descriptor list, allocated through `sys_device_dma_alloc`.
- `pcm_ring: DmaBuffer<[u8; DEFAULT_PCM_RING_BYTES]>` — 16 KiB PCM ring, divided into 32 equal 512-byte slots.
- `logic: Ac97Logic` — the Phase 57 BDL state machine (sole owner of `frames_submitted`, `frames_consumed`, `underrun_count`).
- `stream_open: bool` — admission gate for the `Open`/`Busy` policy; the BDL state in `logic` is the single source of truth for "stream has work pending".

`init()` chains `Ac97PioBus::new(&device)` → `DmaBuffer::allocate(BDL)` → `DmaBuffer::allocate(PCM ring)` → `init_controller(&bus)` → `open_pcm_out_stream(&bus, bdl.iova())`. Each allocation early-returns on failure, and `DmaBuffer::Drop` releases the cap on the error path (the kernel reclaims caps on process exit even if `Drop` is a no-op, but the structured release is what makes the contract clear).

`submit_frames(stream_id, bytes)`:

1. Validates `bytes.len() > 0` and `bytes.len() % PCM_SLOT_STRIDE == 0` — partial-slot submissions are not supported in Phase 63 (`InvalidArgument`).
2. For each slot's worth of bytes: checks that `Ac97Logic::submit_buffer` would accept (BDL not full; otherwise `WouldBlock`); copies the slot bytes into `pcm_ring[head × PCM_SLOT_STRIDE..]`; calls `logic.submit_buffer(bdl_iova_offset, slot_phys_addr, samples)` where `samples = slot_bytes / 2` (S16Le); writes the new `logic.lvi()` value to `BAR_NABM + nabm::PCM_OUT_BASE + nabm::LVI`.
3. Returns the total bytes copied.

`handle_irq()` reads `nabm::CIV` via the bus, computes `ring_was_empty = logic.head == logic.tail` **before** the SR read (because `handle_pcm_out_irq` writes SR back to acknowledge W1C bits), calls `handle_pcm_out_irq(&bus, ring_was_empty)`, then feeds the classified `IrqEvent` and CIV through `logic.observe_irq`. On `IrqEvent::FifoError` the function returns `Err(AudioError::Internal)` so the io loop surfaces the error to the open client.

When `IrqEvent::Underrun` fires and the software ring is empty, the io loop calls `repost_silence_after_underrun` (in `irq.rs`), which submits one slot's worth of zero bytes from the const `SILENCE_FRAME` buffer. The underrun stat is bumped exactly once per event by `apply_irq_event` in `irq.rs`; the repost helper does not touch the counter.

### Smoke gates

`audio-smoke` now boots QEMU with `-audiodev wav,id=snd0,path=<smoke_dir>/audio.wav` (the deterministic CI backend), runs `audio-demo\n` over serial after `audio_server` registers, waits for `AUDIO_DEMO:PASS` (with `AUDIO_DEMO:FAIL stage=...` surfaced via the new `SmokeStep::WaitPassOrFail` variant), then waits for `AUDIO_DEMO:stats consumed=<N>` and rejects any line containing `consumed=0 ` via `SmokeStep::WaitLineNotMatching`. After QEMU exits, `assert_wav_non_silent` parses the recorded WAV's RIFF/WAVE header, walks the data chunk as i16 samples, and requires at least 5% of samples to have `|sample| > 100`. Three distinct exit codes route regressions: `SMOKE_EXIT_AUDIO_DEMO_FAILED = 60`, `SMOKE_EXIT_WAV_SILENT = 63`, `SMOKE_EXIT_BELL_SMOKE_FAILED = 64`.

`bell-smoke` boots the same QEMU configuration, waits for `term`'s `TERM_SMOKE:prompt-ready` sentinel, sends `bell-test\n` over serial, and waits for `BELL_TEST:PASS` (which `bell-test` emits only after confirming `frames_consumed > 0` via `AudioClient::get_stats()`). The `bell-test` binary uses the same `term::bell::AudioClientBellSink` library that `term`'s ANSI parser invokes when it sees a BEL byte — so the smoke exercises the production library code path even though the BEL is not delivered via the user's keyboard.

## Related Roadmap Docs

- [Phase 63 Design](./roadmap/63-audio-stack-implementation.md)
- [Phase 63 Tasks](./roadmap/tasks/63-audio-stack-implementation-tasks.md)
- [Phase 57 Audio + Local Session](./roadmap/57-audio-and-local-session.md) (predecessor; carries the Phase 63 closure note)
- [Phase 57 Audio + Local Session Tasks](./roadmap/tasks/57-audio-and-local-session-tasks.md)

## Manual Smoke Checklist

The headless `cargo xtask audio-smoke` and `cargo xtask bell-smoke` gates provide CI-deterministic coverage. The steps below confirm human-audible output on the host audio device — the final proof that the AC'97 backend emits real PCM rather than just advancing software counters.

### Step 1 — Audible 440 Hz tone via `audio-demo`

1. Run `cargo xtask run-gui` (default audio: PulseAudio on Linux, `none` elsewhere).
2. Wait for the `term` graphical terminal prompt to appear.
3. Type `/bin/audio-demo` and press Enter.
4. **Expected:** A 1-second 440 Hz tone is audible on the host audio device. Serial output shows `AUDIO_DEMO:PASS`.

### Step 2 — Audible BEL beep via `printf '\x07'`

1. In the same `term` session from Step 1 (or re-launch with `cargo xtask run-gui`).
2. Type `printf '\x07'` and press Enter.
3. **Expected:** A short audible beep (~30 ms at 880 Hz, per Phase 57 G.6 `BELL_TONE_FREQ_HZ` / `BELL_DURATION_MS`) is heard on the host audio device.

### Failure interpretation

| Symptom | Likely cause |
|---|---|
| No tone from `audio-demo`, `AUDIO_DEMO:PASS` absent | `audio_server` stub mode (AC'97 device not present in QEMU) |
| Tone inaudible but `AUDIO_DEMO:PASS` printed | Host audio sink not configured (check `pa` vs `none` backend) |
| BEL step silent, `frames_consumed` did not advance | Bell wiring not reaching `Ac97Backend`; check `term::bell::AudioClientBellSink` |
| `cargo xtask run-gui` no audio at all | `--no-audio` was passed, or check QEMU `-audiodev` line in the build log |
