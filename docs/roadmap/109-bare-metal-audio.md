# Phase 109 - Bare-Metal Audio (HDA validation / SoundWire+SOF determination)

**Status:** Planned
**Source Ref:** phase-109
**Depends on:** Phase 80 (Intel HDA Audio — out-of-process ring-3 `hda` driver behind `driver_ipc::audio`) ✅, Phase 63 (Audio PCM Emission / `audio_server` mixer) ✅
**Builds on:** Validates the Phase 80 Intel HDA driver on **real laptop silicon** (it was only QEMU/`intel-hda` + VFIO-validated, never bare-metal on the Dell), **or** — if the laptop routes audio over Intel SoundWire + SOF DSP rather than a legacy HDA codec — charters a brand-new SoundWire+SOF driver. Either path reuses the Phase 63 `audio_server` mixer and the Phase 80 `driver_ipc::audio` / `audio.hw` seam unchanged. Sequenced within the Phase 98 GUI-workstation re-charter after Phase 108 (HP OmniBook bring-up) and governed by the Phase 98 Track A.5 bare-metal validation strategy (`docs/appendix/bare-metal-validation.md`).
**Primary Components:** `userspace/drivers/hda/src/main.rs` (`find_hda` — the class-`0x0403` PCI scan), `userspace/drivers/hda/src/controller.rs` (`HdaController::bring_up` / the STATESTS codec-ready poll — the determination signal), `kernel-core/src/hda/{ids,amd}.rs` (`hda_pci_match`, `is_amd_hda_controller`), `userspace/audio_server/src/proxy.rs` (`AudioProxyBackend` / `audio.hw` discovery — reused either way), `userspace/drivers/hda/src/codec.rs` (`configure_output`, `decode_pin_default`, Realtek EAPD/GPIO amp-enable), **new:** an `audio-probe` determination diagnostic, **new (conditional, Track C):** `userspace/drivers/sndw` (SoundWire bus master) + `userspace/drivers/sof` (SOF DSP), **new:** `scripts/hda-baremetal-validate.md` (generalized from `scripts/ure-vfio-validate.md`)

## Milestone Goal

Sound plays on the laptops. The phase **first determines** how the Dell Tiger Lake machine actually routes audio — a legacy Intel HDA codec on the HD-Audio serial link (which the Phase 80 `hda` driver already knows how to drive), or Intel **SoundWire** links fronted by an **SOF** (Sound Open Firmware) DSP (which no code in the tree touches today) — because that determination sets the entire scope. It **then** either bare-metal-validates the Phase 80 HDA driver against the laptop's real codec (fixing whatever binding/pin-default/amp-enable issues real silicon surfaces) or charters a SoundWire+SOF driver as its own sub-arc. The visible outcome is an **operator-captured non-silent playback** through `audio_server` on the reference laptop, recorded as `Validated-on-HW (run N, date)`.

## Why This Phase Exists

A workstation with no sound is a real gap, and m3OS has never produced audio on the target laptop. The Phase 80 HDA driver was validated only against QEMU's `-device intel-hda` + `-device hda-duplex` model and via VFIO — never on the physical Dell. **Worse, the Phase 80 driver may not bind at all.** Modern Tiger Lake (and later) laptops frequently move analog audio off the legacy HD-Audio link onto **SoundWire** (a MIPI serial bus), with the codecs driven by firmware running on the on-die audio DSP through the **SOF** stack. On such a machine the legacy HDA controller is either firmware-disabled (so `find_hda()` finds nothing) or present but **reports no codecs in STATESTS** (the codecs hang off SoundWire, not the HDA link) — and `HdaController::bring_up` already fails closed in exactly that case with `"no codecs reported in STATESTS"`. There is **zero** SoundWire or SOF code in the tree, so if the laptop uses that path this is a large from-scratch driver, not a validation pass. The determination must come first because it decides whether Phase 109 is a one-PR validation or a multi-PR new-driver sub-arc.

## Learning Goals

- Understand the two competing modern x86 audio topologies: **legacy HD-Audio** (a controller + codecs on the HDA serial link, enumerated via STATESTS) versus **SoundWire + SOF** (codecs on MIPI SoundWire links, driven by firmware on the audio DSP), and the firmware-era reasons laptops migrated to the latter.
- Learn the falsifiable **signatures** that distinguish the two on real hardware without a service manual: an HDA controller absent or present-with-empty-STATESTS, a separate cAVS/ACE audio-DSP PCI function (class `0x0401`, e.g. Tiger Lake `8086:a0c8`), and an ACPI **NHLT** (Non-HD-Audio-Link-Table) describing SoundWire/SSP endpoints.
- See why a microkernel's mixer/policy split (Phase 80) pays off here: `audio_server` and the `driver_ipc::audio` / `audio.hw` seam are **identical** whether the backend is HDA or SoundWire+SOF — the policy layer is bus-agnostic and is reused with no change on either path.
- Understand the shape of a SoundWire+SOF driver: a SoundWire bus master (the Cadence/Intel MIPI controller), signed DSP **firmware load**, an **IPC mailbox** protocol to the DSP, a **topology** description, and a PCM stream that flows DSP→codec — and why this is genuinely a multi-sub-phase effort, not a feature toggle.
- Apply the Phase 98 bare-metal validation protocol to an **un-QEMU-modelable** audio path: a serial-sentinel + operator-captured-audible-output convention substituting for the `hda-smoke` non-silent-WAV assertion that only QEMU's `wavcapture` audiodev can make.

## Feature Scope

### Track A — Determine the Dell codec path (the gating decision)

The scope-setting track; it runs first and its verdict selects Track B or Track C. A small `audio-probe` diagnostic (or an extension of the existing `hda` driver's start path) logs, over the boot serial/log sink:

- **The PCI audio inventory** — every class `0x04` function and its subclass: subclass `0x03` is a legacy HDA controller, subclass `0x01` ("Multimedia audio controller") is the Intel cAVS/SST/ACE audio **DSP** (the SoundWire+SOF host, e.g. TGL `8086:a0c8`). Reuses `driver_runtime::enumerate_pci_class` and `kernel_core::device_host::pci_enum::decode_class_dword` (already used by `find_hda`).
- **HDA codec presence** — if a class-`0x0403` controller exists, run the Phase 80 reset + **STATESTS codec-ready poll** (`HdaController::bring_up`) and log the codec bitmap. A controller present with **STATESTS == 0** (`"no codecs reported in STATESTS"`) is the canonical SoundWire signature: the codecs are not on the HDA link.
- **SoundWire / SOF topology** — log whether an Intel SoundWire master / cAVS DSP function is present and whether the ACPI **NHLT** table (located by signature in the XSDT — the kernel already parses ACPI tables for the RSDP/MADT/DMAR) describes SoundWire/SSP audio endpoints, the table SOF requires for topology.

The track ends by **recording the verdict** (`HDA` vs `SoundWire/SOF`) in the runbook and the README, with the captured PCI/STATESTS/NHLT evidence — exactly the determination the spec requires before any driver work.

### Track B — HDA bare-metal validation (taken **iff** Track A reports HDA)

Bring the Phase 80 `hda` driver up against the laptop's real codec, end-to-end through `audio_server`, fixing whatever real silicon surfaces that QEMU's generic codec did not:

- Codec enumeration + **widget-graph dump** on the real codec (`codec.rs` traversal) — confirm the AFG/DAC/mixer/pin NIDs.
- **Output-path selection** from the BIOS-programmed pin defaults (`decode_pin_default` / `GET_CONFIG_DEFAULT`) so the **internal speaker** (not a disconnected jack) is chosen — the Phase 80c real-hardware concern that QEMU's single-pin codec never exercised.
- **Realtek amp enable on metal** — the EAPD verb + GPIO-driven-EAPD fallback + optional vendor COEF write (`configure_output`, `VERB_SET_EAPD_BTLENABLE`) that QEMU does not need but a real ALC892/ALC1220 board does ("silent until the external amplifier is powered").
- **Stream to completion** — `SDnLPIB` advance / BCIS interrupt observed on the real controller; the **AMD snoop quirk** (`ati_snoop_rmw`) is irrelevant on the Intel Dell but the OmniBook (Phase 108 sibling) exercises it.
- `audio_server` plays a non-silent buffer through the real driver via the unchanged `AudioProxyBackend` / `audio.hw` path.

### Track C — SoundWire + SOF driver charter (taken **iff** Track A reports SoundWire/SOF)

A from-scratch driver. Because there is no in-tree code and the surface is large, Track C **charters a sub-arc** (likely Phases 109a/109b or a follow-on phase) rather than promising a single PR. Scope to be split:

- **SoundWire bus master** (`userspace/drivers/sndw`, new) — the Intel/Cadence MIPI SoundWire master controller: link power-up, clock/frame shape, peripheral (codec) enumeration over the SoundWire enumeration registers. Reference: Linux `drivers/soundwire/` (GPL → register/sequence facts only; re-expressed).
- **SOF DSP** (`userspace/drivers/sof`, new) — load the signed SOF **firmware blob** into the cAVS/ACE DSP, the **IPC** (mailbox) protocol to the DSP, a parsed **topology**, and stream setup. Reference: Linux `sound/soc/sof/` (facts only). The SOF firmware binaries are Intel-redistributable and would be bundled like the mt792x Wi-Fi firmware.
- **`audio.hw` registration** — the SoundWire+SOF driver registers the **same** `driver_ipc::audio` / `"audio.hw"` service the `hda`/`ac97` drivers register, so `audio_server` is unchanged.
- This track is **soft-dependent on Phase 101 (ACPI)** — the SoundWire link controller's resources and the NHLT topology are described in ACPI; without the Phase 101 namespace/`_CRS` enumeration, Track C does the minimum raw-table parse Track A established and defers full enumeration.

### Track D — Bare-metal non-silent-playback validation (operator-captured)

The headline proof, analogous to `hda-smoke`'s non-silent-WAV assertion but **operator-captured** because there is no QEMU `wavcapture` audiodev on metal:

- A serial **sentinel chain** — `audio.hw` ready → stream opened → `frames_consumed` advancing (`SDnLPIB`/DSP position) — proves the datapath ran (the falsifiable, log-captured half).
- An **operator-confirmed audible output** through the internal speaker (the un-modelable half), recorded per the Phase 98 protocol with a dated capture pointer.
- The existing `hda-smoke` / `M3OS_HDA_REGRESSION` gate gains a bare-metal arm and skip-with-reason on QEMU; the determination diagnostic and the runbook (`scripts/hda-baremetal-validate.md`) are committed.

## Important Components and How They Work

### `find_hda` + the STATESTS poll — the determination engine

`userspace/drivers/hda/src/main.rs::find_hda` already enumerates class `0x04` / subclass `0x03` and confirms each candidate with the host-tested `kernel_core::hda::ids::hda_pci_match`. Track A extends this from a binary find/no-find into a **classifier**: it also surfaces subclass-`0x01` audio-DSP functions, and on any HDA controller it runs `HdaController::bring_up`'s reset + STATESTS poll and reports the codec bitmap. The existing fail-closed `Err("no codecs reported in STATESTS")` path (`controller.rs`) is precisely the SoundWire signature — Track A turns that error into a *diagnosis* rather than a silent exit.

### `audio_server` / `AudioProxyBackend` — the bus-agnostic policy layer (unchanged)

`userspace/audio_server/src/proxy.rs` discovers a driver by the `"audio.hw"` service name (`DRIVER_SERVICE_NAME`) and forwards mixer output over `driver_ipc::audio` with `Ack`/`WouldBlock`/`Err` flow control and reconnect-on-restart. Phase 109 reuses this **verbatim** on both paths: whether the backend is the validated HDA driver or a new SoundWire+SOF driver, it registers `"audio.hw"` and the Phase 63 mixer is none the wiser. This is the Phase 80 microkernel split paying off — no policy-layer change on either branch.

### The SoundWire+SOF driver (new, conditional)

If Track A reports SoundWire/SOF, the new `sndw` + `sof` drivers are ring-3 device-host clients exactly like `hda` (`sys_device_claim` / MMIO map / DMA alloc / IRQ subscribe), but the hardware model is entirely different: a SoundWire bus master enumerates codecs over a MIPI serial link, and an SOF DSP runs loaded firmware that the host drives over an IPC mailbox. They terminate at the same `"audio.hw"` seam. This is the single largest implementation difference from anything in the tree and the reason Track C is a chartered sub-arc, not a one-PR deliverable.

### Bare-metal validation tooling

`scripts/hda-baremetal-validate.md` (new, generalized from `scripts/ure-vfio-validate.md`) documents the AMT Serial-over-LAN pre-network capture, the `usb-logsink` boot.log, and the network sink — the Phase 96 / Phase 98 capture toolkit — plus the audio-specific "operator confirms audible output" step. Onboard audio is not USB-attachable, so `--usb-passthrough` does not apply; the loop is image → `dd` → UEFI boot → captured serial + ear.

## How This Builds on Earlier Phases

- **Validates Phase 80** — the out-of-process `hda` driver, QEMU/VFIO-only until now, is exercised on real Dell silicon (Track B), closing the Phase 80c "real-hardware validation on the dev laptop" item that was deferred as hardware-only.
- **Reuses Phase 63 / Phase 80's `audio_server` + `driver_ipc::audio` seam** unchanged — the `AudioProxyBackend` / `"audio.hw"` discovery + `Ack`/`WouldBlock` flow control + reconnect logic carry over on both the HDA and SoundWire/SOF paths.
- **Follows the Phase 98 re-charter** — sequenced after Phase 108 (HP OmniBook bring-up) and governed by the Phase 98 Track A.5 bare-metal validation strategy, carrying the `Validated-on-HW (run N, date)` status convention instead of a bare `Complete`.
- **Reuses the Phase 96 bring-up workflow** — AMT SOL capture, `usb-logsink` boot.log, and the network log sink; the audio runbook is a sibling of `scripts/ure-vfio-validate.md`.
- **Soft-coordinates with Phase 101 (ACPI)** — the SoundWire/SOF path (Track C) leans on Phase 101's `_HID`/`_CRS`/NHLT enumeration for the SoundWire link controller's resources; Track A's raw-table NHLT probe is the bridge until then.

## Implementation Outline

1. **Track A** — add the `audio-probe` determination diagnostic (PCI class `0x04` inventory + subclass split, the STATESTS codec-presence classification reusing `HdaController::bring_up`, and the NHLT/SoundWire-master topology check); boot it on the Dell and **record the verdict** (HDA vs SoundWire/SOF) with captured evidence in `scripts/hda-baremetal-validate.md` + the README.
2. **Branch on the verdict.** If **HDA → Track B**: dump the real codec's widget graph, fix output-path/pin-default selection for the internal speaker, apply the Realtek EAPD/GPIO/COEF amp enable on metal, drive a stream to completion, and play through `audio_server`. If **SoundWire/SOF → Track C**: charter the `sndw` + `sof` sub-arc (split into sub-phases with their own design+task docs and acceptance), targeting the same `"audio.hw"` seam.
3. **Track D** — assert the serial sentinel chain (`audio.hw` ready → stream opened → `frames_consumed` advancing) and the operator-captured audible output; extend `hda-smoke` / `M3OS_HDA_REGRESSION` with the bare-metal arm + skip-with-reason; write the runbook; record the `Validated-on-HW (run N, date)` run.

## Acceptance Criteria

- **The codec path is determined and recorded.** Track A's `audio-probe` boots on the Dell and emits a verdict — `AUDIO_PATH:HDA` (a class-`0x0403` controller with a non-zero STATESTS codec bitmap) or `AUDIO_PATH:SOUNDWIRE` (HDA absent or STATESTS-empty + a cAVS/SST DSP function + an NHLT describing SoundWire/SSP) — captured per `docs/appendix/bare-metal-validation.md` and recorded in `scripts/hda-baremetal-validate.md` and the README. *(Validated-on-HW (run N, date) — Dell Precision 5560 / Tiger Lake.)*
- **Audio plays through `audio_server` on the laptop** with an operator-captured non-silent output: the serial log shows the `"audio.hw"`-ready → stream-opened → `frames_consumed > 0` sentinel chain, and an operator confirms audible playback through the internal speaker, recorded with a dated capture pointer. *(Validated-on-HW (run N, date).)*
- **If the path is HDA:** the Phase 80 `hda` driver enumerates the real codec, selects the internal-speaker pin from `GET_CONFIG_DEFAULT`, applies the Realtek EAPD/amp-enable sequence, and `SDnLPIB`/BCIS shows the stream advancing on metal — no QEMU model involved.
- **If the path is SoundWire/SOF:** the driver scope is split into a charted sub-arc (`sndw` + `sof`, new), each sub-phase with a template-conformant design + task doc and its own acceptance (SoundWire master enumeration; SOF firmware load + IPC; PCM stream → `"audio.hw"`); Phase 109 itself records the determination + the charter, and the playback acceptance moves to the terminal sub-phase.
- **CI surface maximized:** the `audio-probe` PCI/STATESTS classification and any NHLT/SoundWire/SOF codec logic are host-tested in `kernel-core`; the `hda-smoke` / `M3OS_HDA_REGRESSION` gate carries the bare-metal arm and **skips-with-reason** on QEMU (mirroring `tls-smoke`/`wifi-smoke`/`ure-smoke`).

## Companion Task List

- [Phase 109 Task List](./tasks/109-bare-metal-audio-tasks.md)

## How Real OS Implementations Differ

- **Linux** carries both stacks side by side: `snd-hda-intel` for legacy HD-Audio and the SOF stack (`sound/soc/sof/`) + `drivers/soundwire/` for the DSP/SoundWire path, selecting between them from ACPI/NHLT + DMI quirk tables at boot. m3OS makes the same selection but with a deliberate determination probe and a single bundled firmware, not a quirk database.
- **SOF firmware** on Linux is fetched from `linux-firmware` (`sof-tgl.ri`, `sof-tgl.ldc`, topology `.tplg`) and loaded by the kernel; m3OS would bundle the Intel-redistributable blob the way mt792x bundles its Wi-Fi firmware. There is **no BSD `sof`/`soundwire` driver** to re-express, so Track C is Linux-facts-only (register layouts, IPC message formats, firmware-load sequence) re-expressed in Rust.
- Real OSes expose audio through PipeWire/PulseAudio/CoreAudio with routing graphs, per-stream volume, and sample-rate conversion; m3OS has the Phase 63 fixed-format (48 kHz / 2 ch / 16-bit) mixer and nothing more.
- Production bring-up uses a DSP firmware-trace console, the SOF `ipc-test`/`sof-logger` tooling, and a hardware logic analyzer on the SoundWire link; m3OS substitutes serial sentinels + an operator's ear per the Phase 98 protocol because QEMU models none of this.

## Deferred Until Later

- **The SoundWire+SOF datapath itself, if Track A selects it** — chartered here, executed as the Track C sub-arc's own PRs; only the determination + charter land in Phase 109 proper.
- **Microphone / line-in capture** over either path — output-only, matching the Phase 80 deferral.
- **DSP power management** (SOF D0ix/runtime suspend) and **SoundWire clock-stop/bank-switch** power states.
- **HDMI/DisplayPort audio** (needs a GPU driver to coordinate ELD) — unchanged Phase 80 deferral.
- **Format/rate negotiation** beyond the forced 48 kHz / 2 ch / 16-bit — unchanged Phase 80 deferral.
- **Multi-codec routing** and the AMD-side audio path on the OmniBook beyond the existing `ati_snoop_rmw` quirk (the Phase 108 sibling carries AMD-specific bring-up).
