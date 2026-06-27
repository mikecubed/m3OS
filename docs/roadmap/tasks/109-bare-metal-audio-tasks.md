# Phase 109 — Bare-Metal Audio (HDA validation / SoundWire+SOF determination): Task List

**Status:** Planned
**Source Ref:** phase-109
**Depends on:** Phase 80 (Intel HDA Audio — `hda` driver + `driver_ipc::audio` / `audio.hw` seam) ✅, Phase 63 (Audio PCM Emission / `audio_server` mixer) ✅. Sequenced within the Phase 98 GUI-workstation re-charter after Phase 108 (HP OmniBook); governed by the Phase 98 Track A.5 bare-metal validation strategy (`docs/appendix/bare-metal-validation.md`). Track C is soft-dependent on Phase 101 (ACPI) for SoundWire/NHLT resource enumeration.
**Goal:** Get sound working on the laptops. **First determine** the Dell Tiger Lake codec path — legacy Intel HDA codec on the HD-Audio link vs Intel SoundWire links fronted by an SOF DSP — because that verdict sets the scope. **Then** either bare-metal-validate the Phase 80 `hda` driver against the laptop's real codec (Track B) or charter a from-scratch SoundWire+SOF driver sub-arc (Track C). Either way the Phase 63 `audio_server` mixer and the `driver_ipc::audio` / `"audio.hw"` seam are reused unchanged, and the phase ends with an operator-captured non-silent playback recorded as `Validated-on-HW (run N, date)`.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Determine the Dell codec path (PCI audio inventory + STATESTS codec presence + SoundWire/SOF/NHLT topology check) → record verdict | — | Planned |
| B | **Conditional (iff Track A = HDA):** bare-metal-validate the Phase 80 `hda` driver against the laptop codec → `audio_server` | A | Planned |
| C | **Conditional (iff Track A = SoundWire/SOF):** charter the SoundWire bus master + SOF DSP driver sub-arc → `audio.hw` | A, Phase 101 (ACPI) | Planned |
| D | Bare-metal non-silent-playback validation (operator-captured) + gate/runbook/README | B **or** C | Planned |

> HW-only phase: QEMU models neither the laptop's real HDA codec nor any SoundWire/SOF hardware. All on-metal acceptance items follow `docs/appendix/bare-metal-validation.md` and carry **Validated-on-HW (run N, date)** — `<machine>` — `<captured-artifact pointer>`, never a bare "Complete." Every host-testable/QEMU-testable sub-item below stays a real gate so the un-modelable remainder is as small as possible.

---

## Track A — Determine the Dell Codec Path

### A.1 — PCI audio-device inventory diagnostic

**Files:**
- new: `userspace/drivers/audio-probe/src/main.rs` (or an extension of `userspace/drivers/hda/src/main.rs` start path)
- `userspace/drivers/hda/src/main.rs` (`find_hda` reuse)

**Symbol:** `audio_pci_inventory` (new), reusing `driver_runtime::enumerate_pci_class` + `kernel_core::device_host::pci_enum::decode_class_dword`
**Why it matters:** A class-`0x04` subclass-`0x03` device is a legacy HDA controller; a subclass-`0x01` ("Multimedia audio controller") device is the Intel cAVS/SST/ACE audio **DSP** (the SoundWire+SOF host, e.g. Tiger Lake `8086:a0c8`) — which subclass(es) are present is the first determination signal.

**Acceptance:**
- [ ] Logs every class-`0x04` PCI function with its `(vendor, device, subclass, prog_if)` over serial (e.g. `AUDIO_PCI: 8086:a0c8 class=04 sub=01` / `… sub=03`).
- [ ] Distinguishes a legacy HDA controller (subclass `0x03`) from a cAVS/SST DSP function (subclass `0x01`) in the log.
- [ ] Host test in `kernel-core` (or `audio-probe`) asserts the subclass-classification mapping for representative `(class, subclass)` inputs.

### A.2 — HDA codec-presence (STATESTS) classification

**File:** `userspace/drivers/hda/src/controller.rs`
**Symbol:** `HdaController::bring_up` / the STATESTS codec-ready poll (`HdaController::wait_codecs`), the existing `Err("no codecs reported in STATESTS")` fail-closed arm
**Why it matters:** A class-`0x0403` controller that is present but reports **STATESTS == 0** is the canonical SoundWire signature — the codecs are not on the HDA serial link. Track A turns that existing fail-closed error into a *diagnosis* instead of a silent exit.

**Acceptance:**
- [ ] When an HDA controller is present, the probe runs reset + the STATESTS poll and logs the codec bitmap (e.g. `HDA_STATESTS: 0x01` = one codec, `0x00` = none).
- [ ] A non-zero STATESTS bitmap is reported as the **HDA** signal; a present-controller-with-`0x00`-STATESTS is reported as the **SoundWire** signal (not a hard error).
- [ ] No regression to the Phase 80 QEMU path: `hda-smoke` on `-device intel-hda` still enumerates its codec and passes.

### A.3 — SoundWire master + SOF/NHLT topology check

**Files:**
- new: `userspace/drivers/audio-probe/src/topology.rs`
- `kernel/src/acpi/` (raw ACPI table lookup by signature, reusing the existing RSDP/XSDT parse)

**Symbol:** `find_nhlt` (new — locate the `NHLT` ACPI table by signature), `soundwire_master_present` (new)
**Why it matters:** The presence of an Intel SoundWire master / cAVS DSP plus an ACPI **NHLT** (Non-HD-Audio-Link-Table) describing SoundWire/SSP endpoints is the positive SoundWire+SOF signature and the table SOF needs for topology.

**Acceptance:**
- [ ] Logs whether an `NHLT` ACPI table is present and, if so, the link types it enumerates (SoundWire / SSP / DMIC).
- [ ] Logs whether a SoundWire master / cAVS-DSP PCI function is present (correlated with A.1).
- [ ] Host test parses a captured NHLT byte blob and asserts the link-type extraction (pure logic, no hardware).
- [ ] Honest scope note: full ACPI `_HID`/`_CRS` namespace enumeration is Phase 101; A.3 does the minimum raw-signature table parse.

### A.4 — Record the determination verdict (the gating decision)

**Files:**
- `scripts/hda-baremetal-validate.md` (new — results/verdict appendix)
- `docs/roadmap/README.md` (Phase 109 row + the recorded verdict)

**Symbol:** the `AUDIO_PATH:` verdict sentinel + the recorded run
**Why it matters:** The spec requires the codec path be **determined and recorded before** any driver work — it selects Track B vs Track C and the rest of the phase's scope.

**Acceptance:**
- [ ] The probe emits exactly one of `AUDIO_PATH:HDA` (class-`0x0403` controller + non-zero STATESTS) or `AUDIO_PATH:SOUNDWIRE` (HDA absent or STATESTS-empty + cAVS DSP + SoundWire NHLT) on the Dell.
- [ ] The captured PCI inventory + STATESTS bitmap + NHLT result are recorded in `scripts/hda-baremetal-validate.md` per `docs/appendix/bare-metal-validation.md`.
- [ ] **Validated-on-HW (run N, date)** — Dell Precision 5560 / Tiger Lake; evidence: the captured serial log pointer in the runbook.
- [ ] The README Phase 109 row records the verdict and which of Track B / Track C is therefore live.

---

## Track B — HDA Bare-Metal Validation (iff Track A = HDA)

> Taken only if Track A.4 records `AUDIO_PATH:HDA`. Validates the Phase 80 `hda` driver — QEMU/VFIO-only until now — on the laptop's real codec.

### B.1 — Real-codec enumeration + widget-graph dump

**File:** `userspace/drivers/hda/src/codec.rs`
**Symbol:** the widget-graph traversal (`OutputPath`, `configure_output`), `get_parameter(AUDIO_WIDGET_CAPABILITIES)`
**Why it matters:** QEMU's generic single-DAC/single-pin codec never exercised a real multi-widget Realtek graph; the driver must enumerate the laptop codec's AFG/DAC/mixer/pin NIDs correctly before any path can be chosen.

**Acceptance:**
- [ ] Boots on the Dell; serial shows the enumerated codec vendor/device ID and a widget-graph dump (NID → type → connection list).
- [ ] The dump is captured in the runbook for reference.
- [ ] **Validated-on-HW (run N, date)** — Dell; evidence: captured widget-graph log pointer.

### B.2 — Internal-speaker output-path selection (pin defaults)

**File:** `userspace/drivers/hda/src/codec.rs`
**Symbol:** `decode_pin_default`, `VERB_GET_CONFIG_DEFAULT`, the output-pin selection
**Why it matters:** A real laptop has multiple pin complexes (internal speaker, headphone jack, mic); the driver must select the **internal speaker** from the BIOS-programmed pin defaults — the Phase 80c real-hardware concern QEMU's single pin never tested.

**Acceptance:**
- [ ] The driver selects a pin whose `GET_CONFIG_DEFAULT` marks it an internal/fixed speaker (default-device + connectivity), not a disconnected jack, and logs the chosen NID + decoded pin-default.
- [ ] If no usable analog speaker pin is found, the driver logs the full pin-default table rather than failing silently.
- [ ] **Validated-on-HW (run N, date)** — Dell; evidence: the chosen-pin log line.

### B.3 — Realtek amp enable on metal (EAPD / GPIO-EAPD / COEF)

**File:** `userspace/drivers/hda/src/codec.rs`
**Symbol:** `configure_output`, `VERB_SET_EAPD_BTLENABLE`, the GPIO-driven-EAPD fallback + optional vendor COEF write
**Why it matters:** QEMU does not model the "silent until the external amplifier is powered" trap; a real ALC892/ALC1220 board stays silent unless EAPD (and often GPIO-driven EAPD / a vendor COEF write) is issued along the whole path.

**Acceptance:**
- [ ] The EAPD + GPIO-EAPD (+ COEF if needed) amp-enable sequence is issued on the real codec and logged.
- [ ] Every amp along the selected path (mixer/selector input amps + the output/pin amp) is unmuted and powered (`SET_POWER_STATE` D0), confirmed by the produced audible output in D.1.
- [ ] **Validated-on-HW (run N, date)** — Dell; evidence: the amp-enable log + the D.1 audible-output capture.

### B.4 — Stream-to-completion on the real controller

**File:** `userspace/drivers/hda/src/stream.rs`
**Symbol:** `OutputStream::submit` / `poll_consumed` (`SDnLPIB`), `HdaController::handle_irq` (BCIS)
**Why it matters:** The stream DMA engine must actually advance on real silicon — `SDnLPIB` increasing / a BCIS interrupt firing — for any sound to be produced; this is the bring-up half QEMU's deterministic model could not falsify.

**Acceptance:**
- [ ] During playback, `SDnLPIB` advances (non-zero `frames_consumed` from `poll_consumed`) and/or a BCIS interrupt is observed and cleared on the real controller.
- [ ] No interrupt storm (the level-triggered INTx is de-asserted on each completion, per the existing `handle_irq` poll path).
- [ ] **Validated-on-HW (run N, date)** — Dell; evidence: the `frames_consumed`/BCIS log lines.

### B.5 — `audio_server` end-to-end through the real driver

**File:** `userspace/audio_server/src/proxy.rs`
**Symbol:** `AudioProxyBackend`, `DRIVER_SERVICE_NAME` (`"audio.hw"`), the `Ack`/`WouldBlock` flow control
**Why it matters:** The Phase 63 mixer must drive the real HDA driver over the unchanged `driver_ipc::audio` seam — proving the microkernel policy/mechanism split holds on bare metal with zero policy-layer change.

**Acceptance:**
- [ ] `audio_server` discovers `"audio.hw"` (the real `hda` driver), opens a stream, and mixes a non-silent buffer through it — no change to `AudioProxyBackend`.
- [ ] The driver↔server link survives a driver restart (reconnect-and-reopen), confirmed by re-establishing the stream after a forced `hda` re-exec.
- [ ] **Validated-on-HW (run N, date)** — Dell; evidence: the end-to-end playback capture in D.1.

---

## Track C — SoundWire + SOF Driver Charter (iff Track A = SoundWire/SOF)

> Taken only if Track A.4 records `AUDIO_PATH:SOUNDWIRE`. There is **zero** SoundWire/SOF code in the tree, so this track **charters a sub-arc** (likely Phases 109a/109b or a follow-on phase) — Phase 109 proper lands the determination + the charter, and the playback acceptance (Track D) moves to the terminal sub-phase. Linux `drivers/soundwire/` + `sound/soc/sof/` are facts-only references (GPL → register/sequence/IPC-format facts re-expressed in Rust; there is no BSD equivalent). SOF firmware is the Intel-redistributable blob, bundled like the mt792x Wi-Fi firmware.

### C.1 — SoundWire bus master bring-up scope

**File:** new: `userspace/drivers/sndw/src/main.rs`
**Symbol:** `sndw_master_init` (new — link power-up, clock/frame shape, peripheral enumeration)
**Why it matters:** The codecs live on MIPI SoundWire links, not the HDA serial link; nothing in the tree drives a SoundWire master, so this is the new bus the codec hangs off.

**Acceptance:**
- [ ] A sub-phase design + task doc is chartered for the SoundWire master (link enumeration, clock/frame config, peripheral discovery) with its own template-conformant acceptance.
- [ ] The master is a ring-3 device-host client (`sys_device_claim` / MMIO map / DMA / IRQ), mirroring the `hda`/`e1000` shape.
- [ ] Host-tested scope: the SoundWire enumeration-register codec is pure logic and gated in CI; the live link is HW-only.

### C.2 — SOF DSP firmware load + IPC mailbox scope

**File:** new: `userspace/drivers/sof/src/main.rs`
**Symbol:** `sof_fw_load` (new — signed firmware blob into the cAVS/ACE DSP), `sof_ipc_send`/`sof_ipc_recv` (new — the DSP mailbox protocol)
**Why it matters:** SoundWire codecs are driven by firmware running on the audio DSP; the host must load the signed SOF firmware and speak the IPC mailbox protocol — a substantial from-scratch effort with no in-tree analog.

**Acceptance:**
- [ ] A sub-phase charter covers firmware staging (bundled Intel-redistributable blob), DSP boot/firmware-load sequence, and the IPC mailbox protocol, with its own acceptance.
- [ ] Host-tested scope: the IPC message encode/decode + firmware-header parse are pure logic and gated in CI.
- [ ] License provenance recorded: SOF/SoundWire are Linux-facts-only re-expressions; the firmware blob's redistribution terms are documented in the port.

### C.3 — PCM stream over the DSP → `audio.hw`

**Files:**
- new: `userspace/drivers/sof/src/stream.rs`
- `userspace/audio_server/src/proxy.rs` (reused unchanged)

**Symbol:** the `driver_ipc::audio` server loop + `ipc_register_service(.., "audio.hw")`
**Why it matters:** The new driver must terminate at the **same** `"audio.hw"` seam the `hda`/`ac97` drivers use, so the Phase 63 `audio_server` mixer is reused with no change — the whole point of the Phase 80 split.

**Acceptance:**
- [ ] The sub-phase charter has the SoundWire+SOF driver register `"audio.hw"` and serve `driver_ipc::audio` (`QueryCaps`/`OpenStream`/`SubmitFrames`/`Drain`/`CloseStream`), identical to the `hda` server loop.
- [ ] No change to `AudioProxyBackend` or the mixer is required by the charter (verified against the existing `proxy.rs` contract).

### C.4 — Sub-phase split + acceptance

**Files:**
- `docs/roadmap/README.md` (sub-phase rows + mermaid nodes)
- new: `docs/roadmap/109a-…` / `109b-…` design + task docs (as the split dictates)

**Symbol:** the chartered sub-phase schedule
**Why it matters:** The spec explicitly notes a SoundWire+SOF driver is large and "may itself split into sub-phases" — the charter must record that split with measurable per-sub-phase acceptance rather than promising a single PR.

**Acceptance:**
- [ ] The SoundWire+SOF work is split into sub-phases (master / SOF firmware+IPC / PCM stream), each with a template-conformant design + task doc and concrete acceptance.
- [ ] README rows + mermaid nodes added for the sub-phases, depending on Phase 109 (determination) and Phase 101 (ACPI).
- [ ] The terminal sub-phase carries the Track D operator-captured playback acceptance.

---

## Track D — Bare-Metal Non-Silent-Playback Validation

> Runs after Track B (HDA) or the terminal Track C sub-phase (SoundWire/SOF). The audio analog of `hda-smoke`'s non-silent-WAV assertion — but **operator-captured**, since there is no QEMU `wavcapture` audiodev on metal.

### D.1 — Operator-captured non-silent playback

**Files:**
- `userspace/audio_server/src/main.rs` (the playback path)
- `scripts/hda-baremetal-validate.md` (results appendix)

**Symbol:** the `"audio.hw"`-ready → stream-opened → `frames_consumed > 0` sentinel chain + the operator audible-output step
**Why it matters:** This is the phase's headline proof; the serial sentinel chain is the falsifiable log-captured half and the operator's ear covers the un-modelable half, per the Phase 98 protocol.

**Acceptance:**
- [ ] Serial log shows `"audio.hw"` ready → stream opened → `frames_consumed > 0` (`SDnLPIB`/DSP position advancing) during playback.
- [ ] An operator confirms audible non-silent output through the internal speaker; the run is recorded with a dated capture pointer (and a panel/speaker photo if used) per `docs/appendix/bare-metal-validation.md`.
- [ ] **Validated-on-HW (run N, date)** — `<Dell or OmniBook>`; evidence: the captured serial chain + operator-confirmation pointer in the runbook.

### D.2 — `hda-smoke` bare-metal arm + gate/AGENTS/README

**Files:**
- `xtask/src/main.rs` (`cmd_hda_smoke` — extend with the bare-metal/skip-with-reason arm)
- `AGENTS.md` (`M3OS_HDA_REGRESSION` row)
- `docs/roadmap/README.md` (Phase 109 row + mermaid node)

**Symbol:** `cmd_hda_smoke`, `M3OS_HDA_REGRESSION`
**Why it matters:** Keeps the gate discoverable and the un-modelable datapath self-documenting — present and skip-with-reason on QEMU, flipping to a recorded HW run on the laptop (mirroring `tls-smoke`/`wifi-smoke`/`ure-smoke`).

**Acceptance:**
- [ ] `hda-smoke` keeps its QEMU `-device intel-hda` arm green and gains a documented bare-metal arm that skips-with-reason when no real codec is present.
- [ ] `AGENTS.md` `M3OS_HDA_REGRESSION` row notes the Phase 109 bare-metal laptop arm + the determination diagnostic.
- [ ] `docs/roadmap/README.md` has the Phase 109 row (status, the recorded `AUDIO_PATH:` verdict) and a mermaid node depending on Phase 80, Phase 63, Phase 98, and Phase 108 (`P80 --> P109`, `P63 --> P109`, `P108 --> P109`).

### D.3 — Record the validation run

**File:** `scripts/hda-baremetal-validate.md` (results appendix)
**Symbol:** the recorded `Validated-on-HW` run
**Why it matters:** The Phase 98 evidence convention — a recorded physical run with a captured-artifact pointer — is what makes the HW phase's status trustworthy instead of a bare "Complete."

**Acceptance:**
- [ ] A dated entry records: the machine, the determined codec path, the sentinel chain captured, and the operator confirmation.
- [ ] The README + this task doc Status carry **Validated-on-HW (run N, date)** rather than `Complete`.
- [ ] Pre-network logs captured over AMT SOL / `usb-logsink`; post-network over the network sink — both referenced in the runbook.

---

## Documentation Notes

- Track A is the gating decision and **must land first** — its `AUDIO_PATH:` verdict selects whether Track B (HDA validation, one PR) or Track C (SoundWire+SOF, a chartered sub-arc) is live. Record the verdict in the README before opening B/C work.
- The Phase 63 `audio_server` mixer and the Phase 80 `driver_ipc::audio` / `"audio.hw"` seam are reused **unchanged** on both paths — record that the policy/mechanism split proved bus-agnostic, the same way Phase 96 recorded the `RemoteNic` facade proving bus-agnostic for USB.
- If Track C is taken, keep `scripts/hda-baremetal-validate.md` and the `sndw`/`sof` provenance notes Linux-facts-only (no BSD `sof`/`soundwire` exists), mirroring the mt792x driver's `mt76`-citation convention and the SOF firmware-redistribution note.
- This is a HW-only phase under the Phase 98 Track A.5 strategy: every on-metal acceptance uses **Validated-on-HW (run N, date)** with a captured-artifact pointer; CI carries the host-tested classification logic + the skip-with-reason `hda-smoke` arm.
- Prefer the exact files/symbols above over directories; tick checkboxes and append the `run N` evidence pointers as tracks complete on the reference machines.
