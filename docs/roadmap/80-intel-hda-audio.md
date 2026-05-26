# Phase 80 - Intel HDA Audio (+ Realtek codec family)

**Status:** Planned
**Source Ref:** phase-80
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 57 (Audio Stack) ✅, Phase 63 (Audio PCM Emission) ✅, Phase 67 (IOMMU Substrate) ✅
**Builds on:** Extends `audio_server`'s existing AC'97-only backend (Intel 82801AA `0x8086:0x2415`) with the Intel HDA controller + Realtek ALC codec family — the audio silicon that has shipped on essentially every Intel and AMD board since ~2008
**Primary Components:** `userspace/drivers/hda/` (new), `userspace/audio_server/src/device.rs` (gains an HDA `AudioBackend` variant alongside the existing AC'97), `kernel/initrd/etc/services.d/audio_server.conf` (probes HDA first, falls back to AC'97 only on QEMU)

## Milestone Goal

m3OS produces sound on a real laptop or desktop without falling back to QEMU's AC'97 emulation. The supported codec set at the end of this phase: Realtek ALC888 / ALC892 / ALC1220 (covers most consumer boards from 2010 onward including the dev laptop's `0x1022:0x15e3` AMD HDA controller paired with a Realtek codec).

## Why This Phase Exists

Phase 74a §3 makes the AC'97-only audio story explicit: every modern x86 board since ~2008 ships HDA instead of AC'97. The current `audio_server` backend is hard-gated to `0x8086:0x2415` (the QEMU emulation device ID), so on real hardware audio simply does not start. The Phase 57 device-selection layer was already designed for a second backend ("phase add a second backend (e.g., HDA after AC'97)..." per source comment) — this phase makes good on that hook.

## Learning Goals

- Understand how HDA decomposes into a generic host controller + per-codec configuration, unlike AC'97 which fused the two
- See how a stream descriptor on HDA owns a BDL (Buffer Descriptor List), conceptually identical to AC'97's BDL — Phase 63's PCM emission code translates almost directly
- Learn how Realtek codecs use NID (Node ID) widgets that the driver must enumerate and connect (PCM source → mixer → output amp → pin complex)
- Understand the relationship between codec vendor IDs and pin defaults (subsystem ID + Codec ID identifies the laptop OEM's wiring)
- See why HDMI/DisplayPort audio is deferred — it requires per-controller GPU coordination

## Feature Scope

### Track A — HDA host controller

- **A.1** — PCI probe for Intel HDA (class `0x040300`) and AMD HDA (`0x1022:0x15e3` and friends). Map BAR0.
- **A.2** — Reset controller. Allocate CORB (Command Output Ring Buffer) + RIRB (Response Input Ring Buffer) using `DmaBuffer<T>` per Phase 67.
- **A.3** — Codec enumeration via CORB/RIRB verbs. Build per-codec NID widget graph.

### Track B — Stream descriptors

- **B.1** — Allocate one output stream descriptor. Build BDL (Buffer Descriptor List) — same shape as AC'97's BDL from Phase 63.
- **B.2** — Configure stream format (sample rate, channel count, bit depth). Default: 48 kHz / 2 ch / 16-bit, matching `audio_server`'s existing default.
- **B.3** — Start stream via SDnCTL. IRQ on buffer completion drives the existing `Ac97PioBus`-equivalent feeding logic.

### Track C — Realtek codec configuration

- **C.1** — ALC888/892/1220 widget graph parsing. Connect PCM stream → mixer → green/headphone pin complex.
- **C.2** — Pin-default verbs from the BIOS-programmed PinConfig — this is how the codec knows which physical jack is the front-headphone output vs. rear line-out vs. internal speaker.
- **C.3** — Volume / mute control via codec amp widgets.

### Track D — `audio_server` integration

- **D.1** — `userspace/audio_server/src/device.rs` gains an `HdaBackend` variant of the `AudioBackend` enum. The Phase 57 device-selection layer probes HDA first; only the QEMU `0x8086:0x2415` PCI ID falls through to AC'97.
- **D.2** — The Phase 63 `audio-smoke` and Phase 63a `doom-audio-smoke` gates continue to pass — they don't care which backend produced the samples.

## Important Components and How They Work

### CORB / RIRB

HDA replaces AC'97's "write to a register, read a response" model with a pair of DMA rings: CORB (host → codec) carries verbs, RIRB (codec → host) carries responses. Each verb is a 32-bit command targeting one of the codec's NIDs. This decouples codec configuration from MMIO bandwidth and makes multi-codec systems tractable.

### Widget graph

A codec is a graph of NIDs. The host issues `GET_PARAMETERS(AUDIO_WIDGET_CAPABILITIES)` on every NID to learn its type (DAC, ADC, mixer, selector, pin complex). The driver chooses a path from a PCM stream source to a pin complex matching the desired physical output (front headphone, internal speaker), then issues `SET_CONNECTION_SELECT` / `SET_AMP_GAIN_MUTE` verbs to wire the path up.

### BDL and the stream engine

The BDL is a list of `(physical_address, length, IOC_bit)` entries; the controller walks the list and DMAs the buffers into the stream. When the IOC bit is set on a buffer, the controller fires an interrupt. Phase 63's `Ac97PioBus` already implements this loop for AC'97 — Phase 80's HDA driver wires the same `audio_server` feeding logic to a slightly different ring layout.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives.
- Reuses Phase 67's `DmaBuffer<T>` for CORB / RIRB / BDL allocation.
- Reuses Phase 63's `Ac97PioBus`-style feeding loop with a different backend struct.
- Slots into the Phase 57 `AudioBackend` enum point exactly where the Phase 57 source comment promised.

## Implementation Outline

1. Bring up the HDA host controller against QEMU's `-device intel-hda` + `-device hda-duplex` so the existing audio-smoke gate keeps passing.
2. Implement codec enumeration; print the widget graph for a Realtek codec.
3. Wire up output stream with hardcoded path (PCM → output amp → headphone pin) for the QEMU codec.
4. Add Realtek-specific pin-default parsing so a real laptop selects the right physical output.
5. Integrate with `audio_server`; verify `audio-smoke` + `bell-smoke` + `doom-audio-smoke` all pass under HDA.
6. Real-hardware validation on the dev laptop — confirm Realtek codec output through the laptop's internal speaker.
7. Bump kernel to `0.80.0`.

## Acceptance Criteria

- `cargo xtask audio-smoke` passes on `-device intel-hda` (and on `-device intel-hda-mid` if QEMU supports the modern variant).
- `cargo xtask bell-smoke` produces an audible BEL through the HDA backend.
- `cargo xtask doom-audio-smoke` passes under HDA (DOOM SFX + Tier 2a synth music both audible).
- On the dev laptop: `audio-smoke` produces non-silent output through the internal speaker.
- No regression in AC'97 — the QEMU AC'97 path still works for the legacy smoke run.
- Kernel bumped to `0.80.0`.

## Companion Task List

- [Phase 80 Task List](./tasks/80-intel-hda-audio-tasks.md) — to be authored when implementation planning begins.

## How Real OS Implementations Differ

- Linux's `snd-hda-intel` driver has thousands of OEM-specific pin-default quirks; m3OS ships zero quirks and trusts what the firmware programmed.
- Real OSes route audio through PulseAudio / PipeWire / CoreAudio with per-stream volume, sample-rate conversion, latency negotiation, and routing graphs. m3OS at 1.0 has the Phase 63a userspace mixer and that's it.
- HDMI / DisplayPort audio in Linux requires the GPU driver to coordinate hot-plug events with the audio driver via ELD (EDID-Like Data). m3OS at 1.0 has no GPU driver and therefore no HDMI audio path.
- USB audio class — deferred.

## Deferred Until Later

- HDMI / DisplayPort audio (needs GPU driver coordination)
- USB audio (needs Phase 78 USB stack + a new class driver)
- Microphone / line-in capture path
- Power management (HDA D3 / runtime suspend)
- Multi-stream / multi-codec output routing
- Sample-rate conversion in `audio_server` (today: forces 48 kHz / 2 ch / 16-bit)
