# Phase 57 Track A.1 — First Audio Target Choice

**Status:** Decided
**Source Ref:** phase-57
**Scope:** A.1 (this memo) — informs B.1 (`PcmFormat`), C.1 (device claim), and D.2 (controller init)
**Cross-links:**
- Phase 57 design doc — [`docs/roadmap/57-audio-and-local-session.md`](../roadmap/57-audio-and-local-session.md)
- Phase 57 audio ABI — [`docs/appendix/phase-57-audio-abi.md`](./phase-57-audio-abi.md)
- Phase 57 service topology — [`docs/57-audio-and-local-session.md`](../57-audio-and-local-session.md) (Service topology section)
- Phase 55b ring-3 driver host pattern — [`docs/55b-ring-3-driver-host.md`](../55b-ring-3-driver-host.md)
- Phase 55c bound-notification + `RecvResult` pattern — [`docs/appendix/phase-55c-net-send-shape.md`](./phase-55c-net-send-shape.md)

---

## Decision

**Phase 57's first audio target is the Intel 82801AA AC'97 controller** (PCI vendor `0x8086`, device `0x2415`), instantiated in QEMU via `-device AC97`. Rejected alternatives: Intel HDA (`-device intel-hda` + `-device hda-output`) and `virtio-sound`.

The decision is binding for Phase 57 only. A second backend (Intel HDA is the natural follow-up) lands in a later phase by adding a new `AudioBackend` impl behind the trait declared in D.2 — not by editing existing callers. The trait surface in B.3 / D.2 is intentionally narrow enough that a future HDA backend slots in without touching `kernel-core::audio::protocol` or `userspace/lib/audio_client`.

## Rationale

The Phase 57 milestone has three constraints that pull the choice toward AC'97:

1. **Smallest MMIO surface that supports a complete PCM-out path.** AC'97 exposes one mixer block (NAM — Native Audio Mixer, 64 bytes of register space) and one bus-master block (NABM — Native Audio Bus Master, 64 bytes per stream × 3 streams = 192 bytes); the entire driver-visible register surface fits in two BARs that span well under 1 KiB. Intel HDA exposes a ~16 KiB MMIO surface (CORB / RIRB ring registers, stream descriptors per channel, codec command interface, immediate-command interface, wall-clock counter); virtio-sound exposes a control vring plus per-stream tx/rx vrings whose layout is tied to the VirtIO 1.x specification. AC'97 is the smallest reasonable surface for a first cut.

2. **Simplest DMA model that is real silicon, not a paravirtualization.** AC'97 uses a Buffer Descriptor List (BDL) — a fixed-size array of 32 `BufferDescriptor` entries (each 8 bytes: a 32-bit physical address, a 16-bit sample-count, and 16 bits of flags), one per PCM frame chunk. The driver fills BDL entries, programs the BDBAR (BDL base address), the LVI (last valid index), and toggles the run/pause bit. Intel HDA uses CORB (Command Output Ring Buffer) + RIRB (Response Input Ring Buffer) for codec commands plus per-stream BDL — a richer model with codec discovery semantics that Phase 57 does not need. virtio-sound uses a vring transport whose descriptor format is well-defined but whose semantics overlap with the existing virtio-blk/virtio-net code paths; reusing the model brings no new learning value, while its real-hardware coverage is zero.

3. **Real-hardware coverage on commodity x86_64 from an era we already target.** AC'97 (Intel ICH series, 1999–2008) is one of the most widely deployed integrated audio chipsets on the kind of pre-UEFI / early-UEFI commodity hardware m3OS already supports for storage and networking. A working AC'97 driver runs on real silicon with no extra effort (most ICH4–ICH9 motherboards expose the same `0x8086:0x2415` programming model or a near-clone). HDA also has wide hardware coverage but adds codec-discovery complexity that Phase 57 should not pay for. virtio-sound has no real-hardware coverage by definition; it exists only inside hypervisors.

The rejected alternatives' tradeoffs are recorded explicitly so a later phase can revisit without re-litigating.

## Chosen target: AC'97 (Intel 82801AA, ICH-class)

| Attribute | Value |
|---|---|
| PCI Vendor / Device ID | `0x8086` / `0x2415` (Intel 82801AA AC'97 Audio) |
| QEMU device argument | `-device AC97` |
| Required QEMU audiodev | `-audiodev pa,id=snd0` (host PulseAudio sink) for `run-gui` audible output, OR `-audiodev none,id=snd0` for headless smoke harness — the device itself is identical, only the backend changes |
| Real-hardware coverage | Intel ICH0 / ICH / ICH2 / ICH3 / ICH4 / ICH5 / ICH6 / ICH7 / ICH9 motherboards (1999–2008), VIA AC'97-clone southbridges of the same era, SiS / nForce AC'97 implementations. Estimated coverage on commodity pre-2010 x86_64 hardware: >70% |
| Approx. MMIO surface | NAM BAR0 ≈ 64 bytes (mixer registers, single linear block); NABM BAR1 ≈ 192 bytes (3 streams × 64 bytes — PCM-out, PCM-in, MIC; Phase 57 uses PCM-out only) |
| BAR layout | BAR0 (NAM, mixer, I/O space — 256-byte alignment); BAR1 (NABM, bus master, I/O space — 64-byte alignment). Both are I/O-space BARs in real ICH silicon; QEMU's `AC97` device emulates the same I/O-space layout |
| DMA model | Buffer Descriptor List (BDL) — array of up to 32 `BufferDescriptor` entries, each 8 bytes (4 bytes physical address + 2 bytes sample count + 2 bytes flags). Driver programs the BDBAR (BDL base address) once, advances the LVI (last valid index) per stream submission, and observes the CIV (current index value) on IRQ |
| Driver-allocated buffer pages | Phase 55a `DmaBuffer<T>` (existing primitive) — one BDL page (single 4 KiB frame holds all 32 BDL entries with room to spare) plus one PCM-data ring (sized 4 KiB ≤ N ≤ 64 KiB per the audio resource bounds in the task list). Both go through `sys_device_dma_alloc` so the IOMMU domain established at claim time covers them |
| IRQ shape | Single PCI legacy interrupt (or MSI under modern QEMU) routed through the device's IRQ line. The status register (PI / PO / MC global, and BDBAR-relative LVBCI / BCIS / FIFO-error in the per-stream block) tells the driver which condition fired. Phase 55c `IrqNotification::bind_to_endpoint` delivers the wake to `audio_server`'s io loop; the IRQ handler is the kernel's MSI ISR doing only "set a notification bit, EOI" — no work in interrupt context, per the existing IRQ-safety invariants |
| Sample formats supported in Phase 57 | 16-bit signed little-endian, 2-channel (stereo), at one of the AC'97-supported rates (48000 Hz fixed for the first cut; the variable-rate AC'97 extension `VRA` is left disabled for Phase 57). This is the exact set `PcmFormat` (B.1) enumerates — no speculative variants |
| Userspace API the target implies | `open(format, layout, rate) -> stream` → `submit_frames(&[u8])` → `drain()` → `close()`. The `submit_frames` path appends bytes to the PCM ring; `drain` blocks until `frames_consumed >= frames_submitted` (with a timeout); `close` halts the BDL run bit and releases the slot |

The userspace API the target implies feeds directly into A.3's pure-IPC ABI shape and into B.3's `ClientMessage` / `ServerMessage` codec.

### Where AC'97 specifics live in the codebase

- `userspace/audio_server/src/device.rs` (D.2) — owns the AC'97 register layout, BDL programming, and the FakeMmio-driven unit tests that lock the reset → BDBAR-program → LVI-write → run-bit-toggle sequence.
- `kernel-core/src/audio/format.rs` (B.1) — enumerates exactly the PCM formats AC'97 supports in Phase 57: `S16LE` × `Stereo` × `48000 Hz` only.
- `etc/services.d/audio_server.conf` (D.1, D.6) — declares the supervised driver's restart policy and dependency chain.
- `kernel/src/device_host/mod.rs` (C.1) — recognizes `0x8086:0x2415` as a valid claim target alongside the existing NVMe and e1000 IDs; no new syscall is introduced.

## Rejected alternatives and their tradeoffs

### Intel HDA (rejected for Phase 57; first follow-up candidate)

| Attribute | Value |
|---|---|
| QEMU argument | `-device intel-hda` + `-device hda-output,audiodev=snd0` (controller + codec separately) |
| Real-hardware coverage | Most x86_64 silicon from ~2007 forward (ICH9 onward, every modern Intel platform). Higher than AC'97 on **modern** hardware; lower on the era m3OS already targets via NVMe / e1000 |
| MMIO surface | ~16 KiB total: CORB ring registers, RIRB ring registers, ICH (immediate-command host) registers, wall-clock counter, per-stream descriptor blocks (input streams × 4, output streams × 4, bidirectional × 2) |
| DMA model | CORB (commands to codec) + RIRB (responses from codec) for codec discovery and control; per-stream BDL (same shape as AC'97) for sample data |
| IRQ shape | Single MSI vector multiplexed across stream completion, controller status, and codec response. Adds CORB/RIRB drain logic to the io loop |
| Why rejected for Phase 57 | The codec-discovery handshake (CORB write → RIRB read → parse Vendor/Device/Subsystem/Function-Group/Audio-Widget tree) is real complexity that Phase 57 doesn't need to learn and that QEMU emulates only partially. AC'97 has no codec discovery — the mixer block is fixed-layout. Re-using the same codec discovery code on real silicon later is a known unknown. Phase 57's milestone goal is "audible PCM out plus a session," not "complete HDA stack" |

### virtio-sound (rejected for Phase 57; possible follow-up if hosts standardize on it)

| Attribute | Value |
|---|---|
| QEMU argument | `-device virtio-sound-pci,audiodev=snd0` (QEMU 8.1+) |
| Real-hardware coverage | Zero. virtio-sound is a paravirtual device; no physical NIC ships with it |
| MMIO surface | Standard virtio-pci: common config + device-specific config + 3+ vrings (control, event, tx, rx). Sizes follow virtio 1.x layout |
| DMA model | virtqueue descriptors — same model `virtio-blk` and `virtio-net` already use in the kernel today |
| IRQ shape | Per-vring MSI-X (or shared legacy IRQ in fallback). Vring completion fires the bound notification |
| Why rejected for Phase 57 | Excellent for QEMU regression coverage and would arguably be the simplest path because virtio infrastructure already exists in the kernel — but Phase 57's audio milestone is also about teaching the difference between a paravirtual transport and a real DMA-capable controller. Adopting virtio-sound first risks the audio path becoming "virtio plumbing" rather than "audio hardware learning." Picking AC'97 first matches the Phase 55 hardware-substrate ethos of preferring real silicon programming models |

## QEMU argument changes for `cargo xtask run-gui`

The current `qemu_args_with_devices_resolved` (in `xtask/src/main.rs`, around line 1740) already emits `-audiodev none,id=noaudio` under `QemuDisplayMode::Gui` and `pcspk-audiodev=noaudio` on the `-machine` flag (around line 1858). Phase 57's xtask change is a **single-flag delta** in three places, intentionally minimal:

1. **Add `-device AC97` after the audio backend is chosen.** When `--device audio` is passed (a new `DeviceSet` field added in Track H), `qemu_args_with_devices_resolved` emits `-device AC97,audiodev=snd0` alongside the existing `-audiodev` line. The `id=` of the existing `-audiodev` line changes from `noaudio` to `snd0` so the AC'97 device can reference it.
2. **Switch the audiodev backend from `none,id=noaudio` to `pa,id=snd0`** under `--gui` when `--device audio` is set, so the host hears audio through PulseAudio. Headless audio smoke (`cargo xtask audio-smoke`) keeps `none,id=snd0` and validates the driver-level "frames consumed" counter advances rather than asserting on audible output.
3. **`pcspk-audiodev=noaudio` on the `-machine` flag stays as-is** — the PC speaker is independent of AC'97 and still binds to the null backend so an unrelated SeaBIOS beep doesn't crash QEMU.

The smallest-impact patch: gate the new arguments on a `DeviceSet::audio` field defaulting to `false`, so existing `cargo xtask run` and `cargo xtask run-gui` invocations are byte-identical. Track H's manual smoke checklist documents the explicit `--device audio` opt-in.

Worked example. With the new flag the GUI launcher emits, in addition to the current Phase 55/56 args:

```
... -display sdl -audiodev pa,id=snd0 -device AC97,audiodev=snd0 ...
```

The headless smoke launcher emits:

```
... -display none -audiodev none,id=snd0 -device AC97,audiodev=snd0 ...
```

## Userspace API implication (feeds A.3 and B.3)

The chosen target's BDL semantics naturally express as a four-verb client surface:

```
open(PcmFormat, ChannelLayout, SampleRate) -> StreamId
submit_frames(StreamId, &[u8]) -> bytes_accepted (or -EAGAIN if the ring is full)
drain(StreamId) -> () (blocks until all submitted frames have been consumed by the device, with timeout)
close(StreamId) -> ()
```

A.3 turns this into either a `sys_audio_*` syscall block or a pure-IPC contract on `audio_server`'s endpoint. B.3 codifies the wire format. D.2 implements the AC'97-side state machine that backs each verb.

## Connection to Phase 55b's ring-3 driver host pattern

Per A.5, `audio_server` is a Phase 55b-style ring-3 supervised driver. It claims `0x8086:0x2415` via `sys_device_claim`, maps NAM (BAR0) and NABM (BAR1) via `sys_device_mmio_map`, allocates the BDL page and PCM ring through `sys_device_dma_alloc` (so they ride the IOMMU domain established at claim time per Phase 55c R2), and binds the audio IRQ to a `Notification` object via `sys_device_irq_subscribe`. The kernel learns no AC'97 specifics — it learns only "this PCI ID is a valid claim target," and Phase 55c's BAR identity-coverage assertion runs unchanged.

## Acceptance trace

This memo discharges A.1's acceptance:

- [x] Names the chosen target verbatim (Intel 82801AA AC'97, `0x8086:0x2415`) and the rejected alternatives (Intel HDA, virtio-sound) with their tradeoffs recorded.
- [x] Records (a) QEMU device argument (`-device AC97`), (b) real-hardware coverage (>70% of pre-2010 commodity x86_64), (c) approx register/MMIO surface size (NAM ≈ 64 B, NABM ≈ 192 B, total well under 1 KiB), (d) DMA model (BDL — 32-entry Buffer Descriptor List of 8 B entries), (e) IRQ shape (single PCI legacy or MSI vector with status-register multiplexing).
- [x] Names the userspace API the target implies (`open` / `submit_frames` / `drain` / `close`) so B.3 can codify the wire format.
- [x] Cross-links the Phase 57 design doc and Phase 55b's ring-3 driver host pattern (Phase 55c bound-notification mechanics).
- [x] Records the smallest-impact `cargo xtask run-gui` flag change: add `-device AC97,audiodev=snd0`, switch `id=` from `noaudio` to `snd0`, and switch the backend to `pa` for audible output (or keep `none` for headless smoke). Gated behind a new `DeviceSet::audio` opt-in so existing invocations are unchanged.
