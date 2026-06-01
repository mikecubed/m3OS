# Phase 80 - Intel HDA Audio (+ Realtek codec family)

**Status:** Planned
**Source Ref:** phase-80
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 57 (Audio Stack) ✅, Phase 63 (Audio PCM Emission) ✅, Phase 63a (DOOM Audio Wiring) ✅, Phase 67 (IOMMU Substrate) ✅, Phase 74 (IPC Capability Grants) ✅
**Builds on:** Promotes audio from the legacy *in-process* model — where `audio_server` itself claims the AC'97 PCI device and pokes hardware — to the mature **out-of-process ring-3 driver model** used by `e1000`/`nvme`/`xhci`. `audio_server` becomes a pure policy/mixer server; the hardware moves into isolated driver processes. The existing in-process `Ac97Backend` (Intel 82801AA `0x8086:0x2415`) is *extracted* onto the new seam, and the Intel HDA controller + Realtek ALC codec family — the audio silicon shipped on essentially every Intel and AMD board since ~2008 — is added as a second driver behind the same seam.
**Primary Components:** `userspace/drivers/hda/` (new ring-3 driver process), `userspace/drivers/ac97/` (new — AC'97 extracted out-of-process), `kernel-core/src/driver_ipc/audio.rs` (new — audio driver IPC protocol, sibling of `driver_ipc::block`/`driver_ipc::net`), `userspace/audio_server/` (refactored to policy/mixer + an IPC proxy implementing the existing `AudioBackend` trait), `kernel/initrd/etc/services.d/{hda,ac97}.conf`

## Milestone Goal

m3OS produces sound on a real laptop or desktop without falling back to QEMU's AC'97 emulation, and it does so through the **correct microkernel decomposition**: the audio hardware lives in an isolated, IOMMU-protected, individually-restartable ring-3 driver, while `audio_server` owns only mixing and policy. The supported codec set at the end of this phase: Realtek ALC888 / ALC892 / ALC1220 (covers most consumer boards from 2010 onward, including the dev laptop's AMD HDA controller `0x1022:0x15e3` paired with a Realtek codec).

## Why This Phase Exists

Two problems, one phase.

**1. AC'97-only is a dead end on real hardware.** Phase 74a §3 makes the AC'97-only audio story explicit: every modern x86 board since ~2008 ships HDA instead of AC'97. The current audio path is hard-gated to `0x8086:0x2415` (the QEMU emulation device ID), so on real hardware audio simply does not start.

**2. The audio subsystem is the last driver that violates the microkernel decomposition.** `audio_server` today *is* the AC'97 driver: it claims the PCI device (`sys_device_claim`), maps registers, owns DMA, fields the device IRQ, **and** runs the mixer, multiplexes client streams, and arbitrates volume policy. Mechanism (hardware) and policy (mixing) are fused in one process. Every driver m3OS has built since the Phase 55b ring-3 driver-host model — `e1000`, `nvme`, `xhci` — is instead a *separate* process that owns only its device and exposes a narrow IPC surface; audio is the outlier because it predates that model (Phase 57). The same separation is what Redox does (`ihdad` hardware driver vs. the `audiod` mixer daemon) and what Linux does (`snd-hda-intel` controller/codec vs. ALSA/PipeWire policy). This phase makes audio match the rest of the system instead of cloning the legacy shape into a brand-new HDA driver "because it was already set up that way."

A key consequence of doing it correctly: **no kernel changes are required for the seam itself.** `RemoteNic`/`RemoteBlockDevice` live in ring 0 only because their *consumers* (the TCP/IP stack, the VFS) are in-kernel and need a bridge to reach a ring-3 driver. The audio consumer — `audio_server` — is already in userspace, so it connects to the driver process **directly over userspace IPC**. A kernel `RemoteAudio` facade would be pure ring-0 bloat and would violate the userspace-first rule; this phase deliberately does **not** add one. (The one optional kernel touch is the deferred zero-copy IOMMU path in 80c — see Deferred Until Later — which the shipped design does not require.)

## Learning Goals

- Understand the microkernel separation of **mechanism (device driver) from policy (mixer/server)**, and why bundling them — as the legacy `audio_server` does — is a layering violation that hurts fault isolation and least privilege.
- See why an audio driver needs **no kernel facade** while a NIC/block driver does: the difference is whether the consumer lives in ring 0 (kernel facade required) or ring 3 (direct userspace IPC).
- Learn how **bulk PCM crosses a process boundary by capability grant, not by IPC payload** (per the IPC rule "bulk data: page capability grants, never IPC payloads"), and the difference between a *single-use ownership-transfer* page-grant (`sys_page_grant_*`, Phase 74) and a *persistent shared mapping* (`sys_shm_*`, `kernel/src/mm/shm.rs`) — and why the driver still copies each submission into its own IOMMU-domain DMA buffer until the zero-copy path is wired.
- Understand how HDA decomposes into a generic host controller + per-codec configuration, unlike AC'97 which fused the two.
- See how an HDA stream descriptor owns a BDL (Buffer Descriptor List), conceptually identical to AC'97's BDL — Phase 63's PCM emission logic translates almost directly — and why the controller's CORB/RIRB DMA engines (and each stream's `SDnCTL`) must be explicitly **RUN-enabled** after setup or nothing moves.
- Learn how codecs are a graph of NID (Node ID) widgets the driver must enumerate and connect (PCM source → mixer → output amp → pin complex), and how the BIOS-programmed pin-default config identifies the physical jack/speaker without board-specific quirks.
- Understand the Realtek "silent until the external amplifier is powered" trap (EAPD verb, GPIO-driven EAPD, vendor COEF writes) and the related generic trap that every amp *along the path* (mixer/selector input amps, not just the terminal output amp) must be unmuted and powered (`SET_POWER_STATE` D0).
- See why a ring-3 HDA driver under an IOMMU programs **IOMMU device addresses (IOVA)** from its own `DmaBuffer<T>` (`sys_device_dma_alloc`) into the CORB/RIRB/BDL base registers — and why a page-granted buffer from another process is *not* automatically in the driver's IOMMU domain (the Redox `ihdad` reference writes host-physical addresses and has no IOMMU at all).

## Feature Scope

This phase is large enough to land as three sub-phases. They are independent PRs in order; 80a establishes and de-risks the seam, 80b adds the HDA hardware against QEMU, 80c covers Realtek/real-hardware/release. The companion task list groups its tracks under these sub-phases.

### 80a — Audio driver-host seam (AC'97 extraction)

The architecture change, validated against the *known-good, QEMU-testable* AC'97 device before any real HDA hardware is touched.

- A new `driver_ipc::audio` protocol (sibling of `driver_ipc::block`/`driver_ipc::net`) defines the driver-facing verbs: `QueryCaps`, `OpenStream`(format/rate/layout) → stream-id, `SubmitFrames`, `Drain`, `CloseStream`, plus a completion/IRQ notification. The `SubmitFrames` response distinguishes `Ack{frames_consumed}` / `WouldBlock` (ring full, retry) / `Err`, so the existing all-or-nothing client contract (`AudioError::WouldBlock`) is preserved across the IPC seam.
- **Bulk PCM crosses the boundary by grant, not IPC payload.** Each `SubmitFrames` hands the driver a freshly page-granted PCM buffer (Phase 74 `sys_page_grant_*`, a single-use ownership transfer) — the audio analog of NVMe's per-request `payload_grant` and e1000's per-frame bulk grant, and exactly what `audio_server`/`display_server` already do today. The message carries only the grant handle + offset/length, never samples. (A persistent shared ring is also possible with the existing `sys_shm_*` primitive that `display_server` uses for surface buffers; the per-submission grant is chosen for 80a because it matches the in-tree audio path and needs no lifetime negotiation.)
- **The driver copies each submitted buffer into its own IOMMU-domain DMA buffer before programming the controller.** A page-granted buffer is mapped into the driver's *CPU* page table but is **not** entered into the driver's VT-d/AMD-Vi domain (Phase 74's `iommu_remap_grant` is a documented no-op for the receiver-holds-a-device-claim case). So the driver allocates its hardware ring via its own `sys_device_dma_alloc` (a real IOVA in its domain) and copies the submitted frames in — preserving IOMMU isolation with no new kernel primitive. True zero-copy is a deferred 80c follow-up (see Deferred Until Later).
- `audio_server` is refactored into a policy/mixer server. Its existing `AudioBackend` trait stays as the *internal* abstraction; a new `AudioProxyBackend` implements the trait by forwarding each call over the new IPC protocol to a driver process discovered by service name. The `audio_mixer` / `audio_client` / `audio_client_ffi` crates are already backend-agnostic, so DOOM, the bell, and `audio-demo` do not change.
- The existing `Ac97Backend` + `Ac97Logic` + `Ac97PioBus` logic moves into a new `userspace/drivers/ac97/` process that registers an `"audio.hw"`-style service (the same `ipc_register_service` mechanism `e1000` uses for `"net.nic"`). AC'97 keeps its own `DmaBuffer` and copies submitted frames in exactly as the in-process backend does today — so 80a de-risks the **control protocol + four-place wiring + service discovery + crash recovery**, which is the point.

### 80b — Intel HDA host controller + generic codec (QEMU-testable)

The HDA hardware, exercised end-to-end against QEMU's `-device intel-hda` + `-device hda-duplex`.

- `userspace/drivers/hda/` is a new ring-3 driver process. PCI probe matches **class `0x040300`** (vendor-agnostic) plus the AMD HDA IDs (`0x1022:0x15e3` and friends); maps BAR0; decodes `GCAP` (BAR0+`0x00`) into the output/input/bidirectional stream counts (OSS/ISS/BSS) so the chosen output-stream index is valid.
- Controller reset (`GCTL.CRST`), then the **STATESTS codec-ready poll** before any verb is issued.
- CORB (host→codec) + RIRB (codec→host) DMA rings via `DmaBuffer<T>`, programming the **IOVA** into `CORBLBASE`/`RIRBLBASE`. Sizing (`CORBSIZE`/`RIRBSIZE`), pointer reset (`CORBWP`=0, `RIRBWP` `WPRST`), and — critically — **DMA-engine RUN-enable** (`CORBCTL.CORBRUN`, `RIRBCTL.RIRBDMAEN`) last, or no verb ever transfers. An **immediate-command (ICOI/IRII/ICS) fallback** path is included — Redox `ihdad` branches per-emulator because CORB/RIRB vs. immediate-command reliability differs.
- Codec enumeration + **generic widget-graph traversal** (root → AFG → widgets), reading `AUDIO_WIDGET_CAPABILITIES`, connection lists, and `GET_CONFIG_DEFAULT` pin defaults; selecting an output path from a usable pin (headphone/speaker) back to a DAC. When `STATESTS` reports multiple codecs, the driver prefers the codec whose AFG exposes an analog output pin over an HDMI/DP-only codec (a real-hardware concern — see Important Components). This is codec-*agnostic* and is the largest single chunk of 80b.
- One output stream descriptor + BDL (128-byte aligned, IOC per entry, `SDnCBL`/`SDnLVI` consistent), `SDnFMT` encoded (BASE/MULT−1/DIV−1/bits/channels). The per-stream engine is **reset (`SDnCTL.SRST`) then RUN-started (`SDnCTL.RUN`)**, with the 4-bit stream tag written into `SDnCTL[23:20]`. The stream tag **and** format are programmed on **both** the stream descriptor *and* the codec converter (`SET_CHANNEL_STREAMID` + `SET_STREAM_FORMAT`). Every widget on the path is powered (`SET_POWER_STATE` D0) and its amps unmuted (`SET_AMP_GAIN_MUTE` on input amps of mixers/selectors *and* the output/pin amp), pin out-enabled (`SET_PIN_WIDGET_CONTROL`) + EAPD (`SET_EAPD_BTLENABLE`).
- `INTCTL`/`INTSTS` global interrupt plumbing + per-stream `BCIS` clear; single-MSI with INTx fallback (checking the device-host IRQ allocator's behavior for audio-class devices — Phase 79 found it forced INTx only for Ethernet-class).
- `audio_server` probes HDA first; only the QEMU `0x8086:0x2415` PCI ID falls through to the extracted AC'97 driver.

### 80c — Realtek codec config + real hardware + release

- Realtek ALC888/892/1220 widget-graph specifics; pin-default parsing so a real laptop selects the right physical output (internal speaker vs. headphone).
- The Realtek "silent until amp powered" handling: EAPD verb, **GPIO-driven EAPD** fallback (`SET_GPIO_{DIRECTION,MASK,DATA}`), and an optional vendor **COEF** write hook (`SET_COEF_INDEX`/`SET_PROC_COEF`).
- Volume / mute via codec amp widgets (`SET_AMP_GAIN_MUTE`).
- Real-hardware validation on the dev laptop (hardware-only, like the Phase 79 Realtek tracks); kernel bump to `0.80.0`; learning doc `docs/80-intel-hda-audio.md`; README row + AGENTS.md opt-in gate row.

## Important Components and How They Work

### `driver_ipc::audio` + the PCM transport

The new seam. The control channel carries small fixed messages (`QueryCaps`/`OpenStream`/`SubmitFrames`/`Drain`/`CloseStream` + completion), modeled on `kernel-core/src/driver_ipc/block.rs` for NVMe. Sample data does **not** travel in those messages: each `SubmitFrames` references a single-use page-granted PCM buffer (Phase 74) by handle + offset/length — the audio analog of NVMe's per-request `payload_grant`. The driver maps the grant into its CPU address space, **copies** the frames into its own `sys_device_dma_alloc` hardware ring (which carries a valid IOVA in the driver's IOMMU domain), and DMAs from there. The granted pages are reclaimed on receiver-side teardown. The `SubmitFrames` reply is `Ack{frames_consumed}` / `WouldBlock` / `Err`, which `AudioProxyBackend` maps back to the existing `AudioError::WouldBlock` so the mixer's flow control is unchanged.

### `audio_server` as a policy/mixer server

After 80a, `audio_server` no longer touches hardware. It keeps the `audio_mixer` (32-channel DMX→S16LE), the client registry, and stream multiplexing, and it talks to exactly one driver process at a time over `driver_ipc::audio`. Backend selection (HDA-first, AC'97 fallback) becomes "which driver service did we find," implemented behind the unchanged `AudioBackend` trait via `AudioProxyBackend`.

### Failure handling and lifetime

The phase's headline value — an *individually-restartable* ring-3 audio driver — only holds if the seam has an explicit ownership/lifetime story:

- **Grant reclamation on driver crash.** Phase 74's frame-allocator hook *is* wired (`frame_allocator.rs` consults `page_grant::is_frame_granted` and refuses to free pinned frames), but `PageGrant` has no `Drop` and there is no per-PID grant-teardown path — pins are released only when the receiver `consume()`s the grant. So a driver that crashes *before* consuming a `SubmitFrames` grant leaks pinned frames permanently. The plan must verify (and, if necessary, add) kernel reclamation of a dead driver's outstanding grants on abnormal exit.
- **Reconnect on `session_manager` restart.** `audio_server`'s startup path falls through to `run_stub_loop` when no driver is present, but there is no *mid-session* reconnect. On driver death, `audio_server` must re-discover `audio.hw`, re-establish (re-grant) the transport, and reset stream state — mirroring `RemoteNic`'s `RESTART_SUSPECTED`/`DriverRestarting` re-register semantics (`kernel/src/net/remote.rs`).
- **Stream-id ownership.** Stream ids are driver-allocated and invalidated on reconnect; in-flight `SubmitFrames` offsets against a vanished grant are dropped, and `audio_server` re-opens streams after a restart.

### CORB / RIRB and the immediate-command path

HDA replaces AC'97's "write a register, read a response" model with a pair of DMA rings: CORB carries 32-bit verbs (host→codec), RIRB carries responses (codec→host). Each verb targets one codec NID. Bring-up is: program `CORBLBASE`/`CORBUBASE`/`RIRBLBASE`/`RIRBUBASE` with the ring IOVAs, set `CORBSIZE`/`RIRBSIZE` (256 entries), reset the pointers (`CORBWP`=0, `RIRBWP.WPRST`, the `CORBRP.CORBRPRST` set→read-1→clear→read-0 handshake), then **RUN-enable last** (`CORBCTL.CORBRUN`, `RIRBCTL.RIRBDMAEN`) — without the RUN bits no verb transfers. A separate immediate-command interface (ICOI/IRII/ICS registers) issues a single verb synchronously and is the reliable fallback when an emulator's ring path misbehaves.

### Widget graph and pin defaults

A codec is a graph of NIDs. The driver issues `GET_PARAMETER(AUDIO_WIDGET_CAPABILITIES)` on every NID to learn its type (DAC, ADC, mixer, selector, pin complex), reads connection lists, and follows a path from a PCM-stream DAC to a pin complex matching the desired physical output. `GET_CONFIG_DEFAULT` returns the BIOS-programmed 32-bit pin config (default device, port connectivity, location, color, sequence, association) — this is how the driver tells an internal speaker from a rear green line-out **without** any board-specific quirk table. On a multi-codec board (e.g. a GPU HDMI/DP codec alongside the analog Realtek codec) the driver selects the codec exposing an analog output pin; per-destination multi-codec *routing* is deferred.

### BDL and the stream engine

The BDL is a list of `(IOVA address, length, IOC_bit)` entries; the controller walks the list and DMAs the buffers into the stream, firing an interrupt when an IOC-marked buffer completes. The output stream descriptor (block `0x80 + n*0x20`) is configured by cycling `SDnCTL.SRST` (reset), programming `SDnBDPL/U`, `SDnCBL`, `SDnLVI`, `SDnFMT`, writing the stream tag into `SDnCTL[23:20]`, then setting `SDnCTL.RUN` last so the DMA engine starts and `SDnLPIB` advances. Phase 63's `Ac97Logic` already implements this feeding loop for AC'97 — the HDA driver wires the same loop to a slightly different ring layout, fed from the copied-in PCM data.

## How This Builds on Earlier Phases

- **Replaces** the Phase 57 in-process audio-driver shape with the Phase 55b out-of-process ring-3 driver model; `audio_server` is demoted from "driver + server" to "server."
- **Reuses** the Phase 55b ring-3 driver-host syscalls (`sys_device_claim` / `sys_device_mmio_map` / `sys_device_dma_alloc` / `sys_device_irq_subscribe`) verbatim — the HDA and extracted-AC'97 drivers are ordinary device-host clients like `e1000`.
- **Reuses** Phase 67's `DmaBuffer<T>` for CORB / RIRB / BDL allocation, programming IOVA into the controller's base registers.
- **Reuses** Phase 74's page-capability grants for per-submission bulk PCM transfer between `audio_server` and the driver (the `sys_shm_*` persistent-mapping primitive is the alternative, used by `display_server`).
- **Reuses** Phase 63's `Ac97Logic`-style feeding loop, now living inside the extracted `ac97` driver and re-expressed for HDA.
- **Mirrors** `kernel-core/src/driver_ipc/block.rs` (the NVMe block protocol) and `driver_ipc::net` (the per-frame grant) when defining `driver_ipc::audio`, and `e1000`'s `ipc_register_service("net.nic")` registration + `RemoteNic`'s restart-recovery pattern.

## Implementation Outline

1. **80a**: define `driver_ipc::audio` (with the `Ack`/`WouldBlock`/`Err` submit reply) + the per-submission page-grant PCM transport; refactor `audio_server` to a policy/mixer server with `AudioProxyBackend` + mid-session reconnect; extract `Ac97Backend`/`Ac97Logic`/`Ac97PioBus` into `userspace/drivers/ac97/` (keeping its own DmaBuffer + copy); wire the new driver in the four places; add the failure-handling/crash-recovery story; prove `audio-smoke` + `bell-smoke` + `doom-audio-smoke` all still pass out-of-process.
2. **80b**: bring up the HDA host controller against QEMU `-device intel-hda` + `-device hda-duplex`: `GCAP` decode, reset + STATESTS poll, CORB/RIRB sizing + pointer reset + **RUN-enable**, IOVA-programmed rings (+ immediate-command fallback).
3. **80b**: implement codec enumeration + generic widget-graph traversal (+ analog-codec selection); print the widget graph; select an output path for the QEMU codec.
4. **80b**: wire up the output stream — BDL, `SDnFMT`, **`SDnCTL.SRST` reset + stream-tag + `SDnCTL.RUN` start**, stream-tag/format on descriptor *and* converter, per-path power-up + amp unmute + pin out-enable + EAPD — interrupts (`BCIS` clear), and the driver-side `driver_ipc::audio` server loop; make `audio_server` probe HDA first; add the `hda-smoke` gate.
5. **80c**: add Realtek-specific pin-default parsing + EAPD/GPIO-EAPD/COEF amp enable + volume/mute amps so a real laptop selects and drives the right physical output.
6. **80c**: real-hardware validation on the dev laptop (Realtek codec through the internal speaker); bump kernel to `0.80.0`; author `docs/80-intel-hda-audio.md`; update README + AGENTS.md gate table.

## Acceptance Criteria

- After 80a, `cargo xtask audio-smoke`, `cargo xtask bell-smoke`, and `cargo xtask doom-audio-smoke` all pass with AC'97 running **out-of-process** in `userspace/drivers/ac97/` and `audio_server` owning no hardware (verified by `audio_server` no longer calling `sys_device_claim`).
- Killing the audio driver mid-stream and letting `session_manager` restart it: `audio_server` re-establishes the transport and audio resumes, with the dead grant's frames reclaimed (no permanent pin leak).
- `cargo xtask hda-smoke` passes on `-device intel-hda` (+ `-device hda-duplex`): the `hda` driver claims the device, enumerates the codec, RUN-enables CORB/RIRB, and drives a stream to completion (IOC/BCIS interrupt observed or `SDnLPIB` advances).
- `cargo xtask audio-smoke` / `bell-smoke` / `doom-audio-smoke` pass under the HDA backend (DOOM SFX + Tier 2a synth music both audible).
- On the dev laptop: `audio-smoke` produces non-silent output through the internal speaker via the Realtek codec (hardware-only, operator-validated, behind `M3OS_HDA_REGRESSION`).
- No regression in AC'97 — the QEMU AC'97 path still works through the extracted driver for the legacy smoke run.
- No kernel `RemoteAudio` facade is added (the driver↔`audio_server` link is pure userspace IPC).
- Kernel bumped to `0.80.0`; learning doc `docs/80-intel-hda-audio.md` exists and conforms to the design-doc template.

## Companion Task List

- [Phase 80 Task List](./tasks/80-intel-hda-audio-tasks.md)

## How Real OS Implementations Differ

- **Redox** factors audio exactly the way this phase does: `ihdad` is the HDA *hardware* driver (exposes the `audiohw` scheme), and `audiod` is a *separate* mixer/policy daemon. But Redox's `ihdad` is a pure generic-parser driver with **zero codec quirks** and writes **host-physical addresses** straight into the controller's base registers — it does not route HDA DMA through an IOMMU. m3OS instead programs IOMMU-mapped IOVA from its own `DmaBuffer<T>`, which is the single biggest implementation difference from the Redox reference.
- **Linux** `snd-hda-intel` is one universal controller driver; codec parsers are separate modules (`patch_realtek.c`, …) keyed by codec ID, with `hda_generic.c` as the BIOS-pin-config auto-parser. It carries thousands of OEM-specific `SND_PCI_QUIRK` pin-default fixups keyed on subsystem ID; m3OS ships **zero** quirks and trusts what the firmware programmed (plus the minimal EAPD/GPIO/COEF amp-enable Realtek needs).
- Real OSes route audio through PulseAudio / PipeWire / CoreAudio with per-stream volume, sample-rate conversion, latency negotiation, and routing graphs. m3OS at 1.0 has the Phase 63a userspace mixer and that's it.
- HDMI / DisplayPort audio in Linux requires the GPU driver to coordinate hot-plug events with the audio driver via ELD (EDID-Like Data). m3OS at 1.0 has no GPU driver and therefore no HDMI audio path — and on a multi-codec board it deliberately binds the analog codec, not the GPU's HDMI/DP codec.
- USB audio class — deferred (would slot in as another driver behind the same `driver_ipc::audio` seam).

## Deferred Until Later

- **Zero-copy PCM DMA across the seam.** The shipped design copies each submitted buffer into the driver's own IOMMU-domain DMA buffer. True zero-copy (the driver DMAs directly out of `audio_server`'s pages) requires either a new shared-mapping primitive or hardening Phase 74's `iommu_remap_grant` to enter granted frames into the receiver's VT-d/AMD-Vi domain when the receiver holds a device claim — which also needs a read-only "does this PID own a device?" predicate on the device-host registry. Deferred; not required for 1.0.
- **Format/rate negotiation.** `audio_server` forces 48 kHz / 2 ch / 16-bit; `QueryCaps` returns a fixed descriptor and the driver asserts the forced format is within the codec's reported `SUPPORTED_PCM_RATES`/`SUPPORTED_STREAM_FORMATS` (fail-fast), but runtime resampling/negotiation is deferred.
- HDMI / DisplayPort audio (needs GPU driver coordination)
- USB audio (needs the Phase 78 USB stack + a new class driver, behind the same audio seam)
- Microphone / line-in capture path
- Power management (HDA D3 / runtime suspend)
- Multi-stream output and per-destination multi-codec routing (single-codec selection among many *is* in scope for real hardware)
- DMA position buffer (`DPLBASE`/`DPUBASE`) — `SDnLPIB` polling is sufficient for the first driver
