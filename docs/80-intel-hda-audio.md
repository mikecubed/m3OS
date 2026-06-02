# Phase 80 — Intel HDA Audio + out-of-process audio drivers (Learning Doc)

**Status:** Complete
**Source Ref:** phase-80
**Depends on:** Phase 55b (Ring-3 Driver Host), Phase 57 (Audio Stack), Phase 63 (Audio PCM Emission), Phase 67 (IOMMU Substrate), Phase 74 (IPC Capability Grants)
**Builds on:** the in-process `audio_server`-owns-AC'97 model — promoting audio to the out-of-process ring-3 driver model used by `e1000`/`nvme`/`xhci`, then adding an Intel HD Audio controller + generic codec driver behind that seam.
**Primary Components:** `kernel-core/src/driver_ipc/audio.rs` (protocol), `userspace/lib/driver_runtime/src/audio_pcm.rs` (`sys_shm` PCM transport) + `ipc/audio.rs` (wire glue), `userspace/audio_server/src/proxy.rs` (`AudioProxyBackend`), `userspace/drivers/ac97/` (extracted AC'97), `userspace/drivers/hda/{controller,corb,codec,stream}.rs` (HDA driver), `kernel-core/src/hda/{regs,verb,widget,fmt,irq,ids,realtek}.rs` (host-tested decode), `kernel/src/syscall/device_host.rs` (audio-class INTx routing) + `kernel/src/arch/x86_64/syscall/mod.rs` (SHM-mapper reclaim), the `hda-smoke` gate.

## Milestone Goal

m3OS produces sound through the **correct microkernel decomposition**: the audio hardware lives in an isolated, IOMMU-protected, individually-restartable ring-3 driver (`ac97` or `hda`), while `audio_server` owns only mixing and policy. Intel HDA — the audio silicon on essentially every x86 board since ~2008 — is supported via a generic widget-graph driver; QEMU's `intel-hda` + `hda-duplex` codec is driven end-to-end to non-silent output.

## Why This Phase Exists

Two problems, one phase. (1) The AC'97-only path was hard-gated to the QEMU device ID `0x8086:0x2415`, so real hardware (HDA) produced no audio. (2) `audio_server` *was* the AC'97 driver — it claimed the PCI device, owned DMA, fielded the IRQ, **and** ran the mixer. Mechanism (hardware) and policy (mixing) were fused in one process — the last driver violating the Phase 55b ring-3 driver-host decomposition every other driver (`e1000`/`nvme`/`xhci`) already followed. Phase 80 splits them, exactly as Redox does (`ihdad` hardware driver vs. `audiod` mixer) and Linux does (`snd-hda-intel` vs. ALSA/PipeWire).

A key consequence done correctly: **no kernel changes are required for the seam itself.** `RemoteNic`/`RemoteBlockDevice` live in ring 0 only because their consumers (TCP/IP, VFS) are in-kernel. Audio's consumer — `audio_server` — is already in ring 3, so it talks to the driver process **directly over userspace IPC**; a kernel `RemoteAudio` facade would be ring-0 bloat and violate the userspace-first rule. This phase deliberately adds none.

## Learning Goals

- The microkernel separation of **mechanism (device driver) from policy (mixer/server)**, and why a NIC/block driver needs a kernel facade while an audio driver does not — the difference is whether the consumer lives in ring 0 or ring 3.
- How **bulk PCM crosses a process boundary without an IPC payload memcpy** — and why a streaming audio seam is a *producer/consumer shared ring* (`sys_shm`), not a one-shot ownership *move* (`sys_page_grant_*`).
- How HDA decomposes into a generic host controller + a per-codec **widget graph** the driver enumerates and connects, vs. AC'97 which fused the two.
- Why the CORB/RIRB DMA engines and each stream's `SDnCTL` must be explicitly **RUN-enabled** after setup or nothing moves.
- Why a ring-3 HDA driver under an IOMMU programs **IOVA** (from its own `DmaBuffer<T>`) into the CORB/RIRB/BDL base registers — the single biggest difference from the Redox `ihdad` reference, which writes host-physical addresses.
- The Realtek "silent until the external amplifier is powered" trap (EAPD verb / GPIO-driven EAPD / vendor COEF) and the generic trap that *every amp along the path* must be unmuted and powered.

## Feature Scope

### The seam: `driver_ipc::audio` + the shared PCM ring

The control channel (`kernel-core/src/driver_ipc/audio.rs`) carries small fixed tag-dispatched messages: `QueryCaps`/`OpenStream`/`SubmitFrames`/`Drain`/`CloseStream` → `Caps`/`StreamOpened`/`Ack{frames_consumed}`/`WouldBlock`/`Ok`/`Err`. Sample data never travels inline — the enum has no `&[u8]`/`Vec<u8>` field (grep-verifiable, asserted by a host test). `audio_server`'s `AudioProxyBackend` implements the unchanged `AudioBackend` trait by forwarding each call over this protocol; `WouldBlock` round-trips back to `AudioError::WouldBlock`, preserving the all-or-nothing client submit contract. The mixer, client registry, and io loop are unchanged.

#### Transport: why `sys_shm`, not per-submission `page_grant`

The phase plan first specified a per-submission `sys_page_grant_*`. Tracing the code showed that is the wrong primitive for a ~100 Hz period loop: `sys_page_grant_*` is a single-use **move** — `send` unmaps the sender's pages, `recv` maps them at a *fresh* VA and consumes the grant, with **no release path**. Re-granting every period would churn both address spaces and leak frames/VA. And the rationale "matches the in-tree audio path" did not hold — the `audio_client`→`audio_server` path actually uses inline IPC bulk, not grants. A streaming seam is a producer/consumer ring, which is what every real audio stack uses (PulseAudio/PipeWire mmap, CoreAudio, Windows WASAPI). So the transport (`driver_runtime::audio_pcm`) uses the **persistent `sys_shm` shared ring** the design doc sanctions: created+mapped once at stream open, reused for every `SubmitFrames` (the message carries `offset`/`len`; `grant_handle` names the shared region's `shm_id`), refcounted teardown on `CloseStream`/exit. This keeps bulk PCM out of the IPC body (the AGENTS.md rule's intent) without the move-primitive's churn.

#### The driver still copies into its own IOMMU-domain buffer

A `sys_shm`-mapped region lands in the driver's *CPU* page table, not its VT-d/AMD-Vi domain. So the driver (`ac97`/`hda`) **copies** each window into its own `sys_device_dma_alloc` `DmaBuffer` (a real IOVA in its IOMMU domain) and programs *that* into the controller — never the shared region's address. IOMMU isolation is preserved with no new kernel primitive, exactly as the in-process `Ac97Backend` did. True zero-copy (entering granted/shared frames into the driver's IOMMU domain) is deferred (design-doc "Deferred Until Later").

### The HDA host controller

`userspace/drivers/hda/` matches the device by **PCI class `0x040300`** (vendor-agnostic — gating on one vendor ID is the AC'97 mistake) plus an AMD/Intel device-ID table; maps BAR0; decodes `GCAP` into the OSS/ISS/BSS stream counts so the chosen output stream-descriptor index is valid. Bring-up: `GCTL.CRST` reset (clear → poll read-0 → set → poll read-1), the **STATESTS codec-ready poll** before any verb is issued (issuing a verb early returns garbage — the #1 first-driver pitfall), then the CORB/RIRB rings.

#### CORB/RIRB and the immediate-command fallback

HDA replaces AC'97's register-poke model with a pair of DMA rings: CORB (host→codec, 32-bit verbs) and RIRB (codec→host, 64-bit responses). The driver allocates each as a `DmaBuffer` and programs its **IOVA** into `CORBLBASE`/`RIRBLBASE` — the Redox `ihdad` reference writes host-physical addresses and has no IOMMU; m3OS must write the IOMMU-mapped device address. Bring-up sizes both to 256 entries, resets the pointers (`CORBWP=0`, `RIRBWP.WPRST`, the `CORBRP.CORBRPRST` set→read-1→clear→read-0 handshake), then **RUN-enables last** (`CORBCTL.CORBRUN`, `RIRBCTL.RIRBDMAEN`) — without the RUN bits no verb transfers. A single-verb immediate-command path (ICOI/IRII/ICS) is the reliability fallback (Redox branches per-emulator).

#### Widget graph and the output stream

A codec is a graph of NID widgets. `codec.rs` walks root → Audio Function Group → widgets, reading `AUDIO_WIDGET_CAPABILITIES` (type), connection lists, and `GET_CONFIG_DEFAULT` pin defaults — **zero quirk tables**, trusting the BIOS pin config like Redox `ihdad`/Linux `hda_generic.c`. It selects an analog output pin (Speaker > HP > Line-Out), finds a path back to a DAC, then configures every widget on the path: `SET_POWER_STATE` D0, `SET_AMP_GAIN_MUTE` unmute (output **and** input amps — a muted intermediate amp or un-powered widget anywhere leaves the output silent even with DAC+pin correct), `SET_PIN_WIDGET_CONTROL` out-enable + `SET_EAPD_BTLENABLE`, and on the converter `SET_CHANNEL_STREAMID` + `SET_STREAM_FORMAT`. The output stream descriptor (`0x80 + n*0x20`) is configured by cycling `SDnCTL.SRST`, programming the BDL IOVA / `SDnCBL` / `SDnLVI` / `SDnFMT` (BASE/MULT−1/DIV−1/bits/channels), writing the 4-bit stream tag into `SDnCTL[23:20]` (it **must** match the tag sent to the converter or the DAC ignores the stream), then setting `SDnCTL.RUN` **last**. `SDnLPIB` polling tracks the consumed position (DMA position buffer deferred — Redox does the same).

### Interrupts (C.3)

`INTCTL` (GIE + per-stream IE) is armed and a handler decodes `INTSTS` + clears `SDnSTS.BCIS` (host-tested in `kernel_core::hda::irq`). The device-host IRQ allocator was extended to route **audio class `0x04` through INTx** (like the Phase 79 Ethernet fix) because the ring-3 HDA driver drives the legacy `INTCTL`/`SDnSTS` model with no MSI-X cause routing — a kernel-enabled MSI-X vector would stay silent. The IRQ subscription succeeds, INTCTL is armed, and under QEMU's `intel-hda` the live BCIS notification **does** reach the ring-3 driver — the `hda-smoke` gate hard-waits on the driver's `stream IRQ (BCIS cleared)` log line (emitted only from the notification path), so a green gate proves end-to-end IRQ delivery, not just polling. Per the deferred-DPB decision `SDnLPIB` polling remains the authoritative *position* path; the driver also clears BCIS on that poll path so that even if a notification is missed the armed level-triggered INTx can never storm.

### Realtek codec config (Track E)

`kernel-core/src/hda/realtek.rs` (host-tested) encodes the verb sequences a real Realtek ALC892/ALC1220 needs that QEMU's generic codec does not: `SET_EAPD_BTLENABLE` (the external speaker/headphone amp defaults **OFF** — a basic pin-enable alone is silent on real boards), a GPIO-driven EAPD fallback (`SET_GPIO_{DIRECTION,MASK,DATA}`), an optional vendor COEF hook (`SET_COEF_INDEX`/`SET_PROC_COEF`, default no-op — board-specific COEF tables are out of scope), jack-presence-aware output selection (`GET_PIN_SENSE` skips an unplugged HP pin), and the `SET_AMP_GAIN_MUTE` payload encoder for volume/mute. The driver applies these only when the codec vendor is Realtek (`0x10EC`); QEMU's generic codec skips them, so `hda-smoke` is unaffected. Real validation is hardware-only (Track F).

### Failure handling / lifetime

`AudioProxyBackend` presents a **stable facing stream id** to `audio_server`'s registry and maps it to the driver-allocated id internally; on reconnect the driver assigns a fresh id while the facing id is unchanged, so in-flight references stay valid. On `BrokenPipe` (endpoint gone, or a `DriverRestarting`/`DeviceAbsent` reply) the proxy re-discovers `audio.hw`, re-opens the stream, and retries (host-tested). The kernel gained `release_shm_mappings_for_pid` (called at process exit) to decref a dying process's SHM **mapper** references — previously exit dropped only the *creator* ref, so a crashed audio driver's `shm_map` incref pinned the PCM ring's frames forever across restarts.

## How This Builds on Earlier Phases

- **Replaces** the Phase 57 in-process audio-driver shape with the Phase 55b out-of-process ring-3 driver model; `audio_server` is demoted from "driver + server" to "server."
- **Reuses** the Phase 55b device-host syscalls (`sys_device_claim`/`_mmio_map`/`_dma_alloc`/`_irq_subscribe`) verbatim — the HDA/AC'97 drivers are ordinary device-host clients like `e1000`.
- **Reuses** Phase 67's `DmaBuffer<T>` (IOVA into CORB/RIRB/BDL bases) and Phase 74's IPC + the `sys_shm` primitive `display_server` uses for surface buffers.
- **Mirrors** `kernel-core/src/driver_ipc/{block,net}.rs` for the protocol and `e1000`'s `ipc_register_service` + `RemoteNic`'s restart-recovery pattern.

## Implementation Outline

1. **80a**: `driver_ipc::audio` + `sys_shm` PCM transport + glue; `audio_server` → policy/mixer + `AudioProxyBackend` + reconnect; extract AC'97 to `userspace/drivers/ac97/` + four-place wiring; kernel SHM-mapper reclaim; prove `audio-smoke`/`bell-smoke`/`doom-audio-smoke` pass out-of-process.
2. **80b**: HDA controller (`GCAP`/reset/STATESTS/CORB/RIRB RUN-enable, IOVA + immediate-command fallback), widget-graph enumeration + path config, output stream (BDL/`SDnFMT`/`SDnCTL`), interrupts + audio-class INTx routing, the driver-side `audio.hw` server + HDA-first probe, the `hda-smoke` gate.
3. **80c**: Realtek EAPD/GPIO/COEF + jack-presence output selection + volume/mute (host-tested); real-hardware bring-up doc/script; kernel `0.80.0` bump + this doc + README/AGENTS.

## Acceptance Criteria

- After 80a, `audio-smoke`/`bell-smoke`/`doom-audio-smoke` pass with AC'97 **out-of-process** and `audio_server` owning no hardware. ✅
- `hda-smoke` passes on `-device intel-hda -device hda-duplex`: controller claimed, codec enumerated, CORB/RIRB RUN-enabled, stream driven to non-silent WAV. ✅
- No kernel `RemoteAudio` facade. ✅
- Kernel `0.80.0`; this learning doc exists and conforms to the template. ✅

## Companion Task List

- [Phase 80 Task List](./roadmap/tasks/80-intel-hda-audio-tasks.md) · [Design Doc](./roadmap/80-intel-hda-audio.md)

## How Real OS Implementations Differ

- **Redox** factors audio identically (`ihdad` hardware driver + `audiod` mixer), but `ihdad` writes **host-physical addresses** into the controller (no IOMMU) — m3OS programs IOMMU-mapped IOVA from its own `DmaBuffer<T>`, the single biggest difference.
- **Linux** `snd-hda-intel` is one universal controller driver with codec parsers as separate modules and `hda_generic.c` as the BIOS-pin auto-parser, carrying thousands of OEM `SND_PCI_QUIRK` fixups; m3OS ships **zero** quirks plus the minimal Realtek EAPD/GPIO/COEF.
- Real OSes route through PulseAudio/PipeWire/CoreAudio with per-stream volume, sample-rate conversion, and routing graphs; m3OS at 1.0 has the Phase 63a userspace mixer and a fixed 48 kHz / 2 ch / 16-bit format.
- HDMI/DisplayPort audio needs GPU↔audio ELD coordination; m3OS has no GPU driver and deliberately binds the analog codec over an HDMI/DP-only codec on a multi-codec board.

## Deferred Until Later

- **Zero-copy PCM DMA across the seam** (entering shared frames into the driver's IOMMU domain) — the shipped design copies into the driver's own DMA buffer.
- **Format/rate negotiation** — fixed 48 kHz / 2 ch / 16-bit; `QueryCaps` returns a fixed descriptor.
- **Live HDA interrupt-driven completion** under QEMU — the IRQ is armed + handled but delivery is gated; `SDnLPIB` polling is the working path.
- HDMI/DisplayPort audio, USB audio, microphone/line-in capture, HDA power management (D3/runtime suspend), the DMA position buffer (`DPLBASE`/`DPUBASE`), and per-destination multi-codec routing.
