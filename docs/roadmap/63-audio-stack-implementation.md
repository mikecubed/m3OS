# Phase 63 - Audio Stack Implementation

**Status:** Planned
**Source Ref:** phase-63
**Depends on:** Phase 57 (Audio and Local Session) ✅, Phase 55a (IOMMU Substrate) ✅, Phase 55b (Ring-3 Driver Host) ✅, Phase 55c (Ring-3 Driver Correctness Closure) ✅
**Builds on:** Replaces the Phase 57 `Ac97Backend` accounting-only stub with real AC'97 NABM register writes; extends the `audio-smoke` xtask gate from config-load verification to frame-consumption verification
**Primary Components:** userspace/audio_server, kernel-core/audio, xtask audio-smoke gate, userspace/term (bell path)

## Milestone Goal

`audio_server` emits real PCM frames to the AC'97 hardware Buffer Descriptor List. The `audio-smoke` xtask gate asserts frame consumption by reading the AC'97 BDL position counter (or a kernel-side accounting counter), not merely that the service starts. A BEL byte in `term` produces an audible tone through the full stack. The multi-client EBUSY policy from Phase 57 is preserved.

## Why This Phase Exists

Phase 57 was declared Complete with an `Ac97Backend` that performs accounting only — logging frame submissions and advancing internal counters without writing to AC'97 NABM registers. The audio path is structurally present (device claim, MMIO map, DMA allocation, IRQ subscription, single-client arbitration), but no PCM reaches hardware. The `audio-smoke` xtask gate checked that `audio_server.conf` loads, which means the gate passed without ever touching the actual audio path.

This phase exists to close the gap between "the audio architecture is wired" and "sound comes out". Without real NABM register writes the audio subsystem is equivalent to `fat_server`'s ENOSYS stubs: a service that runs and replies while doing none of its advertised work.

## Learning Goals

- Understand how AC'97 bus-master (NABM) registers control DMA buffer delivery.
- Learn how the Buffer Descriptor List (BDL) interacts with position counters for flow control.
- See how a userspace ring-buffer maps onto hardware DMA descriptors.
- Understand how an xtask smoke gate can sample a hardware-side counter rather than a software-side proxy.

## Feature Scope

### AC'97 NABM register driver writes

Set up the BDL with correct entry count, last-valid-index, and IOC bits. Write the BDL base address and control registers to start the PCM-out DMA engine. Handle the BDL wrap (circular-buffer mode). Read the Current Index of Last Buffer (CIV) and Last Buffer Index (LVI) registers to track hardware consumption.

### Ring buffer and DMA staging in `audio_server`

Connect the existing Phase 57 PCM ring buffer to the DMA page(s) allocated via `sys_device_dma_alloc`. Copy client-submitted frames into the DMA buffer and advance LVI to expose them to hardware. Handle underrun: pause the DMA engine, zero-fill one descriptor, restart.

### PCM timing and frame delivery

Verify that frames reach hardware within the AC'97 clock budget (48 kHz, 16-bit stereo = 192 kB/s). The IRQ handler reads the AC'97 status register, clears the completion bit, and wakes the writer side. Frames must not accumulate latency beyond one BDL entry period.

### `audio-smoke` gate asserts frame consumption

Extend the `cargo xtask audio-smoke` gate to read either the AC'97 CIV register (via a kernel debug interface) or a kernel-side `frames_consumed` counter that `audio_server` increments when hardware IRQ fires. The gate must fail if the counter does not advance during the smoke window.

### Audible-bell smoke

A BEL byte (`\x07`) written to `term` must cause the bell callback to submit a short tone through `audio_client`. The `audio-smoke` gate includes a sub-test that sends a BEL and verifies PCM frame consumption within 200 ms.

### Multi-client policy preserved

A second `audio_client` connection returns `-EBUSY` immediately, as established in Phase 57. No change to the arbitration logic.

## Important Components and How They Work

### `userspace/audio_server/src/device.rs`

Contains the AC'97 MMIO init sequence. After this phase: writes the BDL base address to the NABM PCM-out registers, sets LVI, asserts the RUN bit, and reads CIV in the IRQ handler to retire completed descriptors. Before this phase: none of these register writes existed.

### `userspace/audio_server/src/stream.rs`

The PCM stream state machine. After this phase: `submit_frames` copies client data into the DMA region and advances LVI; `handle_irq` reads CIV, frees retired descriptors, and wakes blocked clients. Before this phase: `submit_frames` advanced an in-process counter only.

### `kernel-core/audio/counters.rs` (new)

A small `FrameCounter` struct that `audio_server` increments on each hardware-IRQ-confirmed frame batch. Readable through a debug IPC verb. The `audio-smoke` xtask gate samples this counter before and after the smoke window and asserts the delta is non-zero.

### `cargo xtask audio-smoke`

Extended to: (1) boot the kernel under QEMU with the AC'97 device present, (2) wait for `audio_server` to register, (3) submit a test tone via `audio-demo`, (4) sample the `FrameCounter` over a 500 ms window, and (5) assert the counter advanced. Failure message names the counter value and expected minimum.

## How This Builds on Earlier Phases

- Reuses Phase 55a `DmaBuffer<T>` for the BDL page and PCM ring with no kernel changes.
- Reuses Phase 55b `sys_device_mmio_map` and `sys_device_irq_subscribe` exactly as claimed in Phase 57.
- Reuses Phase 55c bound-notification IRQ loop shape (`RecvResult::Notification { bits }` branch) established in Phase 57 Track D.
- Extends Phase 57's single-client audio architecture — the only changes are to `device.rs` and `stream.rs`; the capability model, IPC protocol, and client library are unchanged.

## Implementation Outline

Follow a TDD-first order: write the `kernel-core` host-side tests for `FrameCounter`, BDL ring accounting, and wraparound before touching `device.rs` or `stream.rs`. Once those tests pass on the host, implement the NABM register writes and wire the IRQ handler. Only then extend the QEMU `audio-smoke` gate to assert frame consumption — this ordering ensures every behavioral invariant is machine-checked before hardware integration.

1. Write host-side tests in `kernel-core/audio/` for `FrameCounter` semantics, BDL ring-buffer accounting, and CIV wraparound — all must pass before any `device.rs` change.
2. Audit the Phase 55b MMIO map for BAR1 (NABM block); verify bus address and size match QEMU AC'97 register map.
3. Implement BDL setup in `device.rs`: allocate one DMA page, fill 32 BDL entries pointing into the PCM ring, set IOC on the last entry, write NABM `PCM_OUT_BDBAR`.
4. Write `stream.rs` `start_dma()`, `advance_lvi()`, `retire_completed(civ)`, and `handle_underrun()`.
5. Add `kernel-core/audio/counters.rs` `FrameCounter`; wire into `audio_server` IRQ handler.
6. Extend `audio-smoke` xtask gate to sample `FrameCounter` and assert advancement.
7. Add audible-bell path in `term`: BEL byte invokes `audio_client::bell()` which submits a 440 Hz, 50 ms tone.
8. Add bell sub-test to `audio-smoke`.
9. Update Phase 57 design doc with a closure note referencing this phase.

## Acceptance Criteria

- `cargo xtask audio-smoke` passes with the `FrameCounter` delta assertion active; the gate fails when run against the stub implementation (confirmed by reverting the NABM writes and observing gate failure).
- A `term` BEL byte triggers PCM frame consumption visible in the smoke gate within 200 ms.
- QEMU AC'97 output can be recorded (using `-audiodev wav`) and contains non-silent samples during the smoke window.
- A second `audio_client` open returns `-EBUSY`; no regression from Phase 57.
- Phase 57 design doc carries a closure note referencing Phase 63 for the PCM implementation.

## Companion Task List

- [Phase 63 Task List](./tasks/63-audio-stack-implementation-tasks.md)

## How Real OS Implementations Differ

- Linux PulseAudio and PipeWire perform ALSA-level ring-buffer management; the kernel ALSA layer handles BDL setup and position reporting through a well-defined `snd_pcm_ops` interface.
- AC'97 is a legacy interface; modern systems use Intel HDA with a more complex command buffer and codec discovery model.
- Production audio servers track latency in nanoseconds and implement dynamic BDL sizing; m3OS uses a fixed 32-entry BDL for simplicity.

## Deferred Until Later

- Multi-client mixing and sample-rate conversion
- HDA (Intel High Definition Audio) driver
- Audio capture (record) path
- ALSA-compatible IPC protocol
- Latency reporting to clients
- Per-client volume control
