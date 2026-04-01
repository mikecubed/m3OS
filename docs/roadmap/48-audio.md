# Phase 48 - Audio Output

## Milestone Goal

The OS can play sound. A sound card driver (Intel HD Audio or AC'97, emulated by QEMU)
outputs PCM audio to the host's speakers. Userspace programs write audio samples to
a device file or via a dedicated syscall. DOOM plays sound effects and music.

## Learning Goals

- Understand how digital audio works: sample rate, bit depth, channels, PCM buffers.
- Learn how a DMA-based sound card operates: ring buffers, buffer descriptors, interrupts.
- See how audio mixing works when multiple sources play simultaneously.
- Understand the latency vs throughput trade-off in audio buffer sizing.

## Feature Scope

### Sound Card Driver

QEMU supports several audio backends. The best targets:

**Option A: Intel HD Audio (HDA) — recommended**
- QEMU flag: `-device intel-hda -device hda-duplex` (or `-audiodev` options).
- PCI device, discoverable via PCI enumeration (already in Phase 15).
- DMA-based: kernel sets up buffer descriptor lists, hardware reads PCM data via DMA.
- Supports 16-bit stereo 44.1/48 kHz — CD quality.

**Option B: AC'97 (Intel ICH)**
- QEMU flag: `-device AC97` or `-device ich9-intel-hda`.
- Simpler than HDA but same DMA model.
- Well-documented, widely ported in hobby OS projects.

**Option C: PC Speaker (minimal)**
- Already accessible via port 0x42/0x43/0x61 (PIT channel 2).
- 1-bit audio only — square wave beeps.
- No mixing, terrible quality, but zero driver complexity.
- Good for a first pass before implementing a real sound card.

### Audio Device Interface

**`/dev/dsp` or `/dev/audio` device file:**
- `open("/dev/dsp", O_WRONLY)` — open audio output.
- `write(fd, pcm_buffer, nbytes)` — write signed 16-bit PCM samples.
- `ioctl(fd, SNDCTL_DSP_SPEED, &rate)` — set sample rate.
- `ioctl(fd, SNDCTL_DSP_CHANNELS, &channels)` — set mono/stereo.
- `ioctl(fd, SNDCTL_DSP_SETFMT, &format)` — set sample format.

This follows the OSS (Open Sound System) API, which DOOM and many retro programs use
natively.

### Audio Mixing (Stretch Goal)

When multiple programs play sound simultaneously:
- Sum PCM samples from all open audio fds.
- Clip to prevent overflow.
- Output mixed buffer to hardware.

For initial implementation, single-client is sufficient.

### DMA Buffer Management

The sound card reads audio data from memory via DMA:
1. Kernel allocates a ring buffer of PCM data (e.g., 64 KB, split into 32 fragments).
2. Sets up buffer descriptor list (BDL) pointing to each fragment.
3. Programs the sound card to cycle through the BDL.
4. Interrupt fires when each fragment is consumed.
5. Kernel copies userspace PCM data into the next available fragment.

### DOOM Audio Integration

DOOM's audio system:
- Sound effects: short PCM samples (8-bit, 11 kHz) from the WAD file.
- Music: MUS format (simplified MIDI) — would need a software synthesizer or stub.
- doomgeneric exposes audio callbacks; the platform layer feeds PCM to `/dev/dsp`.

**Minimal approach:** Sound effects only (no music). Convert 8-bit 11 kHz to 16-bit
44.1 kHz via simple upsampling.

## Prerequisites

| Phase | Why needed |
|---|---|
| Phase 15 (Hardware Discovery) | PCI enumeration to find the sound card |
| Phase 3 (Interrupts) | DMA completion interrupt handling |

## Implementation Outline

1. Start with PC speaker beep — validate audio output path.
2. Detect HDA or AC'97 device via PCI enumeration.
3. Initialize the sound card: reset, configure output stream, set up BDL.
4. Implement DMA ring buffer management.
5. Implement `/dev/dsp` device with `open`/`write`/`ioctl`.
6. Write a test program that plays a sine wave.
7. Integrate with DOOM platform layer for sound effects.
8. Optionally implement basic mixing for multiple audio sources.

## Acceptance Criteria

- A test program plays a sine wave audible through QEMU's audio output.
- `write()` to `/dev/dsp` outputs PCM audio at the configured sample rate.
- ioctl sets sample rate and format correctly.
- DOOM sound effects play during gameplay (gunshots, doors, enemies).
- Audio does not glitch or underrun during normal gameplay.
- Closing the audio device stops playback cleanly.
- All existing tests pass without regression.

## Companion Task List

- Phase 48 Task List — *not yet created*

## How Real OS Implementations Differ

Real systems have a deep audio stack:
- **ALSA** (Advanced Linux Sound Architecture) — kernel driver + userspace library.
- **PulseAudio / PipeWire** — userspace sound servers for mixing, routing, effects.
- **Jack** — low-latency audio for professional use.
- **Hardware mixing** — modern sound cards mix in hardware (our approach mixes in software).
- **Resampling** — automatic sample rate conversion between sources and output.
- **Bluetooth audio** — A2DP sink/source profiles.
- **USB audio** — class-compliant USB audio devices.

Our approach is closest to OSS (Open Sound System), the original Unix audio API.
Single-client, direct PCM output, no mixing server.

## Deferred Until Later

- PulseAudio / PipeWire-style sound server
- MIDI / software synthesizer (for DOOM music)
- USB audio class driver
- Bluetooth audio
- Audio recording (microphone input)
- Hardware mixing
- Sample rate conversion
- Multiple simultaneous audio clients
