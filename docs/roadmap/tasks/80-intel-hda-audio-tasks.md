# Phase 80 — Intel HDA Audio (+ Realtek codec family): Task List

**Status:** Planned
**Source Ref:** phase-80
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 57 (Audio Stack) ✅, Phase 63 (Audio PCM Emission) ✅, Phase 63a (DOOM Audio Wiring) ✅, Phase 67 (IOMMU Substrate) ✅, Phase 74 (IPC Capability Grants) ✅
**Goal:** Move audio from the legacy *in-process* model (`audio_server` owns the AC'97 hardware) to the mature *out-of-process* ring-3 driver model used by `e1000`/`nvme`/`xhci`, then add an Intel HDA controller + Realtek ALC codec driver behind that same seam. `audio_server` becomes a pure policy/mixer server; the AC'97 hardware is *extracted* into `userspace/drivers/ac97/` to de-risk the seam against a QEMU-testable device before HDA hardware is touched; the HDA driver lands as `userspace/drivers/hda/`. No kernel `RemoteAudio` facade is added — the consumer (`audio_server`) is already in userspace, so the driver↔server link is direct userspace IPC over a new `driver_ipc::audio` protocol. Bulk PCM crosses the boundary by per-submission page-grant (Phase 74), and the driver copies each submission into its own `sys_device_dma_alloc` IOMMU-domain buffer (true zero-copy is a deferred follow-up — see A.3). Closeout bumps the kernel to `0.80.0` and adds the learning doc.

## Track Layout

| Sub-phase | Track | Scope | Dependencies | Status |
|---|---|---|---|---|
| 80a | A | Audio IPC seam: `driver_ipc::audio` protocol + per-submission PCM grant transport + `audio_server` policy refactor + reconnect/lifetime + AC'97 extraction | Phase 74 (page grants) | Planned |
| 80b | B | HDA host controller: crate + PCI claim + `GCAP` + BAR0 + reset + STATESTS + CORB/RIRB sizing/reset/**RUN-enable** (IOVA) | A (protocol) | Planned |
| 80b | C | HDA codec + stream engine: widget-graph enumeration + analog-codec selection + BDL/`SDnFMT` + `SDnCTL` SRST/RUN + per-path power-up/amp-unmute/pin/EAPD + interrupts | B | Planned |
| 80b | D | HDA integration: driver-side `driver_ipc::audio` server + HDA-first probe + `hda-smoke` gate | C, A.4 | Planned |
| 80c | E | Realtek ALC888/892/1220 amp-enable (EAPD/GPIO/COEF) + pin-default output selection + volume/mute | C.1, C.2 | Planned |
| 80c | F | Real-hardware bring-up on the dev laptop (hardware-only) | E | Planned |
| 80c | G | Release closeout: kernel `0.80.0` bump + learning doc + README/AGENTS gate | A–F landed | Planned |

> **Ordering note.** **80a (Track A) lands first and alone** — it changes the audio architecture using the *known-good, QEMU-testable* AC'97 device, so a regression is caught by the existing `audio-smoke`/`bell-smoke`/`doom-audio-smoke` gates with zero new-hardware risk. **80b (Tracks B–D)** adds the HDA hardware against QEMU `-device intel-hda` and depends on A's protocol (the HDA driver implements the *driver side* of `driver_ipc::audio` that the extracted AC'97 driver proved out). **80c (Tracks E–G)** is Realtek-specific config, real-hardware validation, and release closeout. Each sub-phase is a separate PR. Host-testable logic (verb encoding, `SDnFMT` packing, pin-default decode, BDL math, ring-pointer math, protocol message codec) lives in `kernel-core` so it is exercised by `cargo xtask check` without QEMU — mirroring how Phase 79 put its `nic_ids`/`r8169` host tests in `kernel-core` (the `net_ring` engine lives in `userspace/lib/driver_runtime/`).

> **Track Layout note.** The leading **Sub-phase** column is a deliberate extension of the canonical `Track | Scope | Dependencies | Status` template columns, added because this phase is splittable into 80a/80b/80c.

---

## Track A — Audio IPC seam + AC'97 extraction (80a)

### A.1 — Define the `driver_ipc::audio` protocol

**File:** `kernel-core/src/driver_ipc/audio.rs` (new; model on `kernel-core/src/driver_ipc/block.rs` and `driver_ipc::net`)
**Symbol:** `enum AudioRequest` / `enum AudioResponse` / `enum AudioDriverError` + a `PcmFormat`/`SampleRate`/`ChannelLayout` mirror of the `audio_server::device` types; an `encode`/`decode` pair (host-testable, `no_std`)
**Why it matters:** this is the narrow contract that replaces the in-process `AudioBackend` method calls with IPC verbs; defining it in `kernel-core` (not the kernel) keeps it host-testable and shared by both `audio_server` (client) and the `ac97`/`hda` drivers (servers), exactly as `driver_ipc::block` is shared by the VFS and the `nvme` driver.

**Acceptance:**
- [x] `AudioRequest` covers `QueryCaps`, `OpenStream { format, rate, layout } -> stream_id`, `SubmitFrames { stream_id, grant_handle, offset, len }`, `Drain { stream_id }`, and `CloseStream { stream_id }`. *(Landed: `kernel-core/src/driver_ipc/audio.rs::AudioRequest`.)*
- [x] `AudioResponse` distinguishes the `SubmitFrames` outcomes `Ack { frames_consumed }` / `WouldBlock` (ring full — retry) / `Err(AudioDriverError)`, plus `StreamOpened(stream_id)` and a `Caps` descriptor; a host test round-trips **every** variant — including `WouldBlock` — through `encode`→`decode` byte-for-byte (`kernel_core::driver_ipc::audio::tests::request_roundtrip`, `response_roundtrip`, `would_block_roundtrip`). *(9 host tests pass.)*
- [x] The PCM payload is **not** an inline field of any message — `SubmitFrames` references a grant handle + offset/length into the granted region (A.2), enforced by the absence of any `&[u8]`/`Vec<u8>` sample field in the request enum (grep-verifiable). *(Asserted by `submit_frames_has_no_inline_sample_field`.)*
- [x] `QueryCaps`→`Caps` returns a fixed 48 kHz / 2 ch / 16-bit descriptor for 1.0 (rate negotiation is deferred — see Documentation Notes); the driver validates this against the codec's reported `SUPPORTED_PCM_RATES`/`SUPPORTED_STREAM_FORMATS` in C.1. *(`caps_v1()`; codec validation lands in C.1.)*

### A.2 — Per-submission PCM grant transport + driver-side copy

**Files:**
- `userspace/lib/driver_runtime/src/audio_pcm.rs` (new)
- `kernel-core/src/driver_ipc/audio.rs` (grant-offset/length validation helpers)

**Symbol:** `PcmRing` (sender) + `PcmReceiver::recv_and_copy` (driver-side: maps the shared region, copies the window into the driver's own `DmaBuffer` allocated via `sys_device_dma_alloc`, real IOVA in the driver's IOMMU domain); `copy_window` is the pure host-tested core.
**Why it matters:** AGENTS.md mandates "bulk data: page capability grants, never IPC payloads." Bulk PCM crosses the seam in a page-capability-backed shared region, never in the IPC body. The driver **copies** each window into its own `sys_device_dma_alloc` buffer (real IOVA in its IOMMU domain) so IOMMU isolation is preserved with no new kernel primitive, exactly as the in-process `Ac97Backend` does (`submit_frames_inner`).

> **Transport-decision note (landed):** The literal A.2 wording specified a *per-submission* `sys_page_grant_*`. During implementation that primitive proved architecturally wrong for a ~100 Hz streaming loop: `sys_page_grant_*` is a single-use *move* (it unmaps the sender's pages and maps them at a fresh receiver VA with **no release path**), so re-granting every audio period churns both address spaces and leaks frames/VA, and the "matches the in-tree audio path" rationale does not hold — the in-tree `audio_client`→`audio_server` path actually uses inline IPC bulk, not grants. A streaming seam is a producer/consumer ring, which is what every real audio stack (PulseAudio/PipeWire mmap, CoreAudio, WASAPI) uses. We therefore use the **persistent `sys_shm_*` shared ring** the design doc explicitly sanctions (Documentation Notes; `kernel/src/mm/shm.rs`, the `display_server` primitive): map once at stream open, reuse every `SubmitFrames` (carry `offset`/`len`), refcounted teardown on `CloseStream`/exit. The A.1 protocol is unchanged — `grant_handle` now names the shared region (`shm_id`). This satisfies every functional acceptance item below (bulk out of IPC body; driver copies into its own IOMMU-domain DMA buffer; no leak) at lower kernel risk.

**Acceptance:**
- [x] Host test validates window offset/length bounds (a `SubmitFrames` offset+len always lands inside the shared region; out-of-range/zero/overflow rejected) — `audio_pcm::tests::submission_bounds` (+ `kernel_core::driver_ipc::audio::tests::submission_bounds`).
- [ ] The driver allocates its hardware PCM buffer via its **own** `sys_device_dma_alloc` and programs that buffer's **IOVA** into the controller (HDA BDL / AC'97 BDL); the shared region is never programmed as a device address (grep + log assertion). *(Lands with the ac97/hda driver in A.5/D.1.)*
- [x] No sample byte is ever copied into an `AudioRequest` (verified by A.1's enum shape).
- [ ] The shared region is released after stream close so frames are not leaked (refcounted shm teardown; ties to A.6). *(Lands with the driver in A.5/D.1.)*

### A.3 — (Stretch / deferred) Zero-copy: enter granted PCM frames into the driver's IOMMU domain

**Files:**
- `kernel/src/ipc/page_grant.rs` (`iommu_remap_grant`)
- `kernel/src/syscall/device_host.rs` (new read-only "does this PID own a device?" predicate)

**Symbol:** harden `iommu_remap_grant` to install per-domain IOVA mappings when the receiver holds ≥1 device claim; add `device_host::pid_owns_device(pid) -> bool`
**Why it matters:** removes the per-submission copy in A.2 so the driver DMAs directly out of `audio_server`'s pages. **This is explicitly out of scope for the shipped 1.0 design** — the copy path (A.2) is correct and sufficient. Listed so the optimization and its kernel prerequisites are recorded rather than rediscovered.

**Acceptance:**
- [ ] (Deferred — not required to land Phase 80.) If implemented: a granted PCM buffer received by a device-owning driver is entered into that driver's IOMMU domain and the A.2 copy is elided; a host test exercises `pid_owns_device`; the IOMMU-fault ISR shows no spurious faults for the granted range.

### A.4 — Refactor `audio_server` into a policy/mixer server + `AudioProxyBackend` + reconnect

**Files:**
- `userspace/audio_server/src/device.rs` (the `AudioBackend` trait stays; a new proxy impl is added)
- `userspace/audio_server/src/main.rs` (stop claiming hardware; discover + connect to the driver service; mid-session reconnect)
- `userspace/audio_server/src/proxy.rs` (new)

**Symbol:** `struct AudioProxyBackend` implementing the existing `AudioBackend` trait (defined as a trait at `userspace/audio_server/src/device.rs`) by forwarding `init`/`open_stream`/`submit_frames`/`drain`/`close_stream`/`handle_irq` over `driver_ipc::audio` to a driver endpoint discovered by service name (`audio.hw`); `submit_frames` maps the `WouldBlock` response back to `AudioError::WouldBlock`
**Why it matters:** keeps `audio_server`'s internal abstraction (the trait, the mixer, the client registry) intact while removing all hardware access from the server process; the `audio_mixer`/`audio_client`/`audio_client_ffi` crates stay untouched, so DOOM/bell/`audio-demo` are unaffected.

**Acceptance:**
- [ ] `audio_server` no longer calls `sys_device_claim`/`sys_device_mmio_map`/`sys_device_dma_alloc`/`sys_device_irq_subscribe` (verified by `grep` returning zero hits in `userspace/audio_server/`).
- [ ] `audio_server` still registers `audio.cmd` (`SERVICE_NAME`) for clients and now also resolves `audio.hw` for its backend; if no driver service is found it falls through to the existing `run_stub_loop` so `session_manager`'s `await_ready("audio_server")` still succeeds.
- [ ] `AudioProxyBackend::submit_frames` returns `AudioError::WouldBlock` when the driver replies `WouldBlock`, preserving the existing all-or-nothing client contract (host test against a mock driver endpoint drives one open→submit(WouldBlock)→submit(Ack)→drain→close cycle and asserts the emitted `AudioRequest` sequence + the `WouldBlock` mapping).
- [ ] Mid-session reconnect: when the driver endpoint goes away, `audio_server` re-discovers `audio.hw`, re-opens its streams, and resumes (ties to A.6).

### A.5 — Extract AC'97 into `userspace/drivers/ac97/` + four-place wiring

**Files:**
- `userspace/drivers/ac97/src/{main,lib}.rs` (new; move `Ac97Backend`/`Ac97Logic`/`Ac97PioBus` + `BufferDescriptor` BDL logic out of `userspace/audio_server/src/device.rs`)
- `Cargo.toml` (`members`)
- `xtask/src/main.rs` (`build_userspace_bins` bins array; `--features os-binary` map; `populate_ext2_files` confs)
- `kernel/src/fs/ramdisk.rs` (`static AC97_DRIVER_ELF` + `DRIVERS_ENTRIES`)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)
- `kernel/initrd/etc/services.d/ac97.conf` (via `populate_ext2_files` + `KNOWN_CONFIGS`)

**Symbol:** `program_main` for the `ac97_driver` binary (model on `userspace/drivers/e1000/src/main.rs::program_main`); registers `ipc_register_service(ep, "audio.hw")`; the device claim moves here from `audio_server`. The extracted driver **keeps its own `DmaBuffer` and copies submitted frames into it** (AC'97 is a 32-bit-IOVA PIO device — `Ac97PioBus`, `check_iova_fits_u32` rejects >4 GiB — so it cannot DMA from arbitrary granted pages; the copy is the existing behavior, unchanged).
**Why it matters:** proves the new out-of-process **control protocol + four-place wiring + service discovery + crash recovery** against the QEMU AC'97 device that the existing smoke gates already cover, before any HDA hardware exists. (It does **not** prove zero-copy DMA — AC'97 copies into its own buffer, as does the default HDA path; zero-copy is A.3, deferred.) Missing any of the four wiring places means the driver is not built, not embedded, or not found at runtime (per AGENTS.md "Adding a New Userspace Binary").

**Acceptance:**
- [ ] `userspace/drivers/ac97/` is a workspace member with an `os-binary`-gated `[[bin]]` and a `lib` target for host tests, matching the `e1000_driver` crate shape.
- [ ] After `cargo xtask clean && cargo xtask run`, init logs `driver.registered name=ac97_driver` and `audio_server` connects to `audio.hw`; the four-place wiring (members, xtask `build_userspace_bins` bins + os-binary map + `populate_ext2_files` conf, ramdisk ELF + entry, `KNOWN_CONFIGS`) is present.
- [ ] `cargo xtask check` passes with the new crate (clippy `-D warnings` + rustfmt + host tests).

### A.6 — Failure handling & lifetime (crash recovery, grant reclamation, stream-id ownership)

**Files:**
- `userspace/audio_server/src/main.rs` / `proxy.rs` (reconnect path)
- `kernel/src/ipc/page_grant.rs` (verify/add reclamation of a dead PID's outstanding grants)
- `docs/roadmap/80-intel-hda-audio.md` ("Failure handling and lifetime" subsection — already added)

**Symbol:** the reconnect path mirroring `RemoteNic`'s `RESTART_SUSPECTED`/`DriverRestarting` re-register (`kernel/src/net/remote.rs`); a kernel teardown that reclaims a crashed driver's pinned grant frames
**Why it matters:** the phase's headline value is an *individually-restartable* driver. The frame-allocator hook **is** wired (`kernel/src/mm/frame_allocator.rs` consults `page_grant::is_frame_granted` and refuses to free pinned frames), but `PageGrant` has **no `Drop`** and there is no per-PID grant-teardown — pins release only on `consume()`. So a driver that crashes before consuming a `SubmitFrames` grant leaks pinned frames *permanently* (the allocator actively refuses to free them). This must be closed for restartability to be real.

**Acceptance:**
- [ ] Stream-id ownership is defined: driver-allocated, invalidated on reconnect; in-flight `SubmitFrames` offsets against a vanished grant are dropped.
- [ ] Kill the `ac97`/`hda` driver mid-stream; `session_manager` restarts it; `audio_server` re-discovers `audio.hw`, re-opens streams, and audio resumes.
- [ ] The dead grant's frames are reclaimed (no permanent pin leak) — verified by a kernel/host test or a before/after free-frame count across a forced driver exit; if reclamation requires a new kernel teardown path (no `PageGrant` `Drop` / per-PID grant-drain exists today), that change is part of this task.

### A.7 — 80a regression: audio path passes out-of-process

**Files:**
- `xtask/src/main.rs` (`cmd_audio_smoke`, `cmd_bell_smoke`, `cmd_doom_audio_smoke` — no logic change expected; the AC'97 QEMU device flags stay)
**Symbol:** the existing `audio_smoke_steps` / `bell_smoke_steps` / `doom_audio_smoke_steps` sentinels (`AUDIO_DEMO:PASS`, `BELL_TEST:PASS`)
**Why it matters:** the entire point of doing the architecture change against AC'97 first is that the existing gates are the regression net.

**Acceptance:**
- [ ] `cargo xtask audio-smoke` passes (`AUDIO_DEMO:PASS` + non-zero `AUDIO_DEMO:stats consumed=`) with AC'97 running in `userspace/drivers/ac97/`.
- [ ] `cargo xtask bell-smoke` passes (`BELL_TEST:PASS`) through the extracted driver.
- [ ] `cargo xtask doom-audio-smoke` passes through the extracted driver.

---

## Track B — HDA host controller bring-up (80b)

### B.1 — `userspace/drivers/hda/` crate + PCI claim + `GCAP` + BAR0 + four-place wiring

**Files:**
- `userspace/drivers/hda/src/{main,init,io,lib}.rs` (new; model on `userspace/drivers/e1000/`)
- `kernel-core/src/hda/mod.rs` + `kernel-core/src/hda/ids.rs` + `kernel-core/src/hda/regs.rs` (new; host-testable PCI-match + register/GCAP decode)
- the four wiring places (as in A.5) for an `hda_driver` binary + `hda.conf`

**Symbol:** `program_main`; `hda_pci_match` matching **class `0x040300`** (vendor-agnostic) plus an `HDA_DEVICE_IDS` set including AMD `0x1022:0x15e3`; BAR0 MMIO map via `sys_device_mmio_map`; `decode_gcap` extracting OSS/ISS/BSS from `GCAP` (BAR0+`0x00`)
**Why it matters:** HDA is identified primarily by PCI class, not a single device ID — gating on one vendor ID (the AC'97 mistake) would miss most real controllers; decoding `GCAP` gives the output-stream count so the chosen stream-descriptor index `n` is valid (stream descriptors live at `0x80 + n*0x20`).

**Acceptance:**
- [ ] Host test asserts `hda_pci_match` accepts class `0x040300` regardless of vendor and the explicit AMD `0x1022:0x15e3` ID, and rejects the AC'97 `0x8086:0x2415` (`kernel_core::hda::ids::tests::matches_class_040300_and_amd`, `rejects_ac97`).
- [ ] Host test: `decode_gcap(...)` returns the OSS/ISS/BSS stream counts; the chosen output-stream index is `< OSS` (`kernel_core::hda::regs::tests::gcap_decode`).
- [ ] Under `cargo xtask run` with `-device intel-hda`, the driver claims the device, maps BAR0, and emits an `HDA_SMOKE:server:READY` sentinel before its event loop.
- [ ] Four-place wiring present; `cargo xtask check` passes.

### B.2 — Controller reset + STATESTS codec-ready poll

**File:** `userspace/drivers/hda/src/init.rs` + `kernel-core/src/hda/regs.rs`
**Symbol:** `HdaController::reset` — clear `GCTL.CRST` (BAR0+`0x08` bit0) → poll until read-0 → set `CRST` → poll until read-1 → delay → poll `STATESTS` (BAR0+`0x0E`) until non-zero with a bounded bailout; `codecs_from_statests`
**Why it matters:** issuing a verb before the codec-ready poll returns garbage — this is the #1 first-driver pitfall (Redox clears `STATESTS=0x7FFF` before reset, then polls after).

**Acceptance:**
- [ ] Host test: `codecs_from_statests(0b0000_0101)` returns codec addresses `{0, 2}`; the reset predicate reports "ready" only after `CRST` reads back 1 (`kernel_core::hda::regs::tests::statests_decode`, `reset_predicate`).
- [ ] Under `-device intel-hda`, serial logs the discovered codec count (≥1) after reset.

### B.3 — CORB/RIRB rings (IOVA) + sizing/reset/**RUN-enable** + immediate-command fallback

**Files:**
- `userspace/drivers/hda/src/corb.rs` (new)
- `kernel-core/src/hda/verb.rs` (new; verb dword encoding + ring-pointer math)

**Symbol:** `CorbRirb` allocating a `DmaBuffer<T>` and programming the **IOVA** into `CORBLBASE`/`CORBUBASE` (`0x40`/`0x44`) and `RIRBLBASE`/`RIRBUBASE` (`0x50`/`0x54`); `CORBSIZE` (`0x4E`)/`RIRBSIZE` (`0x5E`) set to 256 entries (low 2 bits `0b10`); `CORBWP` (`0x48`)=0; `RIRBWP` (`0x58`) `WPRST` (bit15); the `CORBRP` (`0x4A`) `CORBRPRST` (bit15) set→read-1→clear→read-0 handshake; **RUN-enable last** — `CORBCTL` (`0x4C`) `CORBRUN` (bit1) + `RIRBCTL` (`0x5C`) `RIRBDMAEN` (bit1); `encode_verb12`/`encode_verb4`; `ImmediateCmd` using `ICOI`/`IRII`/`ICS` (`0x60`/`0x64`/`0x68`)
**Why it matters:** this is the single biggest difference from the Redox reference — Redox writes host-physical addresses, m3OS must write the IOMMU-mapped device address from `DmaBuffer<T>`; and **without `CORBRUN`/`RIRBDMAEN` set, no verb ever transfers** over the rings (the immediate-command path is the reliability fallback Redox needs per-emulator).

**Acceptance:**
- [ ] Host test: `encode_verb12(codec=1, nid=0x02, verb=0xF00, payload=0x09) == 0x102F0009`; `encode_verb4(codec, nid, 0x2, fmt)` packs the 4-bit-verb (bits 19:16) + 16-bit-payload form; the `CORBRPRST` handshake predicate (set→read-1→clear→read-0) is modeled (`kernel_core::hda::verb::tests`).
- [ ] After ring bring-up, `CORBCTL.CORBRUN` and `RIRBCTL.RIRBDMAEN` both **read back 1** before the first verb is issued (driver assertion + serial log).
- [ ] Under `-device intel-hda`, a `GET_PARAMETER(VENDOR_ID)` issued via CORB/RIRB returns a non-zero vendor:device pair; the same query via the immediate-command path returns the identical value.
- [ ] The address written to `CORBLBASE`/`RIRBLBASE` is the `DmaBuffer` IOVA (`dma.iova()`), asserted equal to the register write — not the user VA.

---

## Track C — HDA codec + stream engine (80b)

### C.1 — Codec enumeration + generic widget-graph traversal + pin-default decode + path select + analog-codec selection

**Files:**
- `userspace/drivers/hda/src/codec.rs` (new)
- `kernel-core/src/hda/widget.rs` (new; host-testable widget-cap + pin-default decode + path search)

**Symbol:** `enumerate_widgets` (root → AFG via `GET_PARAMETER` `NODE_COUNT 0x04` / `FUNCTION_TYPE 0x05`; per-widget `AUDIO_WIDGET_CAPABILITIES 0x09`, `CONNLIST_LEN 0x0E`, `GET_CONNECTION_LIST 0xF02`, `GET_CONFIG_DEFAULT 0xF1C`, `SUPPORTED_PCM_RATES 0x0A` / `SUPPORTED_STREAM_FORMATS 0x0B`); `decode_pin_default`; `find_path_to_dac`; `select_codec` (prefer a codec whose AFG exposes an analog output pin over an HDMI/DP-only codec)
**Why it matters:** the generic, codec-*agnostic* widget parser is the largest single chunk of the HDA driver and is what lets m3OS select an output path from BIOS pin config with **zero quirk tables** (the Redox `ihdad` / Linux `hda_generic.c` approach). On a multi-codec board (GPU HDMI codec + analog Realtek codec) a naive "first codec" pick can bind the silent HDMI codec.

**Acceptance:**
- [ ] Host test decodes a `GET_CONFIG_DEFAULT` value into `{default_device, port_connectivity, location, color, sequence, association}` and classifies a "fixed internal speaker" vs a "rear green line-out jack" correctly (`kernel_core::hda::widget::tests::pin_default_classify`).
- [ ] Host test: `widget_type(caps)` extracts bits23:20 → `{DAC, ADC, Mixer, Selector, PinComplex, ...}`; `find_path_to_dac` over a synthetic graph (pin → selector → DAC) returns the DAC NID; `select_codec` over a synthetic {HDMI-only, analog} codec pair returns the analog codec (`widget::tests::path_to_dac`, `codec_selection_prefers_analog`).
- [ ] The driver validates `audio_server`'s forced 48 kHz / 16-bit / 2 ch against the converter's reported `SUPPORTED_PCM_RATES`/`SUPPORTED_STREAM_FORMATS` and fails fast (logged) if unsupported.
- [ ] Under `-device intel-hda`, the driver prints the enumerated widget list and the selected output pin→DAC path for the QEMU codec.

### C.2 — Output stream: BDL + `SDnFMT` + `SDnCTL` SRST/tag/RUN + per-path power-up/amp-unmute/pin/EAPD

**Files:**
- `userspace/drivers/hda/src/stream.rs` (new)
- `kernel-core/src/hda/fmt.rs` (new; `SDnFMT` + BDL packing)

**Symbol:** `OutputStream::configure` programming the stream-descriptor block at `0x80 + n*0x20`: cycle `SDnCTL.SRST` (offset `0x00`, bit0) reset → set `SDnBDPL`/`SDnBDPU`, `SDnCBL`, `SDnLVI`, `SDnFMT` (offset `0x12`: BASE bit14 / MULT−1 / DIV−1 / bits / channels−1) → write the 4-bit stream tag into `SDnCTL[23:20]` → set `SDnCTL.RUN` (bit1) **last**; BDL entries `(addr_iova, len, IOC)` 128-byte aligned; the verb pair `SET_CHANNEL_STREAMID 0x706` + `SET_STREAM_FORMAT 0x2` on the converter; per-path `SET_POWER_STATE 0x705` D0 + `SET_AMP_GAIN_MUTE 0x3` (unmute the input amps of every mixer/selector on the path **and** the output/pin amp, not just the terminal) + `SET_PIN_WIDGET_CONTROL 0x707` (out-enable bit6) + `SET_EAPD_BTLENABLE 0x70C` on the pin
**Why it matters:** the stream tag/format must be set on **both** the stream descriptor *and* the codec converter or the DAC ignores the DMA stream (silent); `SDnCTL.SRST` must be cycled and `SDnCTL.RUN` set or the DMA engine never starts and `SDnLPIB` cannot advance; the BDL must be 128-byte aligned with `SDnCBL == Σ entry lengths` and `SDnLVI == count−1`; a muted intermediate amp or an un-powered widget anywhere on the path leaves the output silent even with DAC+pin correct (a frequent "stream runs but silent" trap); pin out-enable + EAPD is required even on QEMU.

**Acceptance:**
- [ ] Host test: `encode_sdnfmt(48000, 16, 2) == 0x0011` and `encode_sdnfmt(44100, 16, 2)` sets BASE bit14; BDL builder produces 128-byte-aligned entries with IOC on each and `cbl == sum(len)`, `lvi == count-1` (`kernel_core::hda::fmt::tests::sdnfmt_48k_stereo_16`, `bdl_consistency`).
- [ ] The configure sequence cycles `SDnCTL.SRST` (read-back 1 then 0) and sets `SDnCTL.RUN` last (read-back 1); the 4-bit `SDnCTL[23:20]` stream tag equals the tag sent via `SET_CHANNEL_STREAMID`, and the `SDnFMT` value equals the value sent via `SET_STREAM_FORMAT` (driver assertions + serial log).
- [ ] Every amp on the selected path is unmuted and powered (`SET_POWER_STATE` D0) — host-tested for the emitted verb sequence over a synthetic path; not just the terminal output amp.
- [ ] Under `-device intel-hda -device hda-duplex`, the configured stream's `SDnLPIB` advances (or a `BCIS` interrupt fires) when frames are submitted — proving the DMA engine is consuming the copied-in PCM data.

### C.3 — Interrupts (INTCTL/INTSTS/BCIS) + MSI/INTx selection

**File:** `userspace/drivers/hda/src/io.rs` + `kernel-core/src/hda/irq.rs` (new; interrupt-status decode)
**Symbol:** `arm_interrupts` (set `INTCTL` `0x20` GIE bit31 + the output stream's per-stream IE bit); `handle_irq` (read `INTSTS` `0x24`; on GIS, dispatch per-stream `SIS`; clear `SDnSTS` `BCIS` by writing bit2); IRQ subscription via `sys_device_irq_subscribe`
**Why it matters:** without clearing `SDnSTS.BCIS` the interrupt re-asserts forever; Phase 79 found the device-host IRQ allocator forces INTx only for Ethernet-class — audio-class (`0x04`) behavior must be confirmed so the HDA IRQ actually fires.

**Acceptance:**
- [ ] Host test: `decode_intsts(...)` reports GIS + which stream index fired; the `BCIS`-clear value is bit2 (`kernel_core::hda::irq::tests::intsts_decode`, `bcis_clear_value`).
- [ ] Under `-device intel-hda`, the driver receives a stream-completion interrupt (serial logs the IRQ vector + `BCIS` cleared); if the device-host allocator's MSI-X auto-enable breaks delivery for audio-class as it did for Ethernet-class in Phase 79, the fix (force INTx / program MSI cause routing for class `0x04`) is applied in `kernel/src/syscall/device_host.rs` and recorded in this task.

---

## Track D — HDA integration + gate (80b)

### D.1 — HDA driver-side `driver_ipc::audio` server + `audio_server` HDA-first probe

**Files:**
- `userspace/drivers/hda/src/main.rs` (server loop)
- `userspace/audio_server/src/main.rs` / `proxy.rs` (probe order)

**Symbol:** the `hda_driver` server loop handling `AudioRequest` over the `audio.hw` endpoint, copying each `SubmitFrames` grant into its own DMA buffer (A.2) and pacing the stream; `audio_server` resolves `audio.hw` (HDA registers it first), falling back to the AC'97 driver only for the QEMU `0x8086:0x2415` case
**Why it matters:** closes the loop — `audio_server` (policy) drives the HDA driver (mechanism) over the seam that A.4/A.5 proved with AC'97, including the A.6 reconnect path.

**Acceptance:**
- [ ] With both drivers buildable, on a machine exposing only `-device intel-hda` the `hda_driver` wins the `audio.hw` registration and `audio_server` mixes through it; on `-device AC97` the `ac97_driver` serves instead — first-to-match-wins probe order is logged.
- [ ] `audio_server` issues `OpenStream`/`SubmitFrames` to the HDA driver and observes non-zero `frames_consumed` (`Ack`) responses, with `WouldBlock` correctly handled under backpressure.

### D.2 — `hda-smoke` xtask gate

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_hda_smoke` (model on `cmd_audio_smoke`); inject `-device intel-hda -device hda-duplex` (the generic output+input QEMU codec) + `-audiodev wav,id=snd0,path=...` and confirm the codec's DAC output is routed to that audiodev (mirror how `cmd_audio_smoke` pins `AC97,audiodev=snd0`); add to the AGENTS.md opt-in gate table under `M3OS_HDA_REGRESSION`
**Why it matters:** a serial-sentinel + non-silent-WAV gate proves the HDA driver reaches link and produces samples in CI, the same way `audio-smoke` does for AC'97; naming the codec + confirming the wav capture path avoids the gate silently degrading to a serial-only check.

**Acceptance:**
- [ ] `cargo xtask hda-smoke` boots with `-device intel-hda -device hda-duplex`, asserts `HDA_SMOKE:server:READY` + the codec-enumerated path, runs the audio demo, and verifies the captured WAV is non-silent (DAC output confirmed routed to the wav audiodev).
- [ ] `cargo xtask audio-smoke` (AC'97 arm) still passes — no regression.
- [ ] The gate is registered in the AGENTS.md opt-in table with env var `M3OS_HDA_REGRESSION=1`.

---

## Track E — Realtek ALC codec config (80c)

### E.1 — ALC888/892/1220 amp-enable (EAPD verb + GPIO-EAPD + COEF hook)

**Files:**
- `userspace/drivers/hda/src/realtek.rs` (new)
- `kernel-core/src/hda/realtek.rs` (new; host-testable verb sequences)

**Symbol:** `realtek_amp_enable` issuing `SET_EAPD_BTLENABLE 0x70C`; a GPIO-EAPD fallback (`SET_GPIO_DIRECTION 0x717` / `SET_GPIO_MASK 0x716` / `SET_GPIO_DATA 0x715` on the AFG); an optional vendor-COEF hook (`SET_COEF_INDEX 0x500` / `SET_PROC_COEF 0x400`)
**Why it matters:** Realtek speaker/headphone outputs sit behind an external amplifier that defaults OFF — a driver that does only the basic pin-enable is silent on real ALC892/ALC1220 boards even though everything else "works."

**Acceptance:**
- [ ] Host test asserts the exact verb dword sequence emitted by `realtek_amp_enable` for the EAPD path and the GPIO-EAPD fallback (`kernel_core::hda::realtek::tests::eapd_verb_sequence`, `gpio_eapd_sequence`).
- [ ] The COEF hook is present and host-tested but defaults to a no-op (board-specific COEF tables are out of scope — documented in Documentation Notes).

### E.2 — Pin-default real-output selection

**File:** `userspace/drivers/hda/src/realtek.rs` + `codec.rs`
**Symbol:** output-pin selection preferring `default_device == Speaker` (fixed/internal) then `HP-Out`, using the C.1 pin-default decoder; jack-presence (`GET_PIN_SENSE 0xF09`) skips HP pins with nothing plugged in
**Why it matters:** on a real laptop the driver must pick the internal speaker (or headphone when present) — the difference between "stream runs" and "you hear it on the right output."

**Acceptance:**
- [ ] Host test: given a synthetic Realtek pin set (internal speaker + rear line-out + front HP), the selector returns the internal speaker when nothing is plugged into HP, and HP when present (`kernel_core::hda::realtek::tests::output_selection`).

### E.3 — Volume / mute control via amp widgets

**File:** `userspace/drivers/hda/src/realtek.rs` + `codec.rs`
**Symbol:** volume/mute via `SET_AMP_GAIN_MUTE 0x3` on the path's output-amp widgets (payload encodes set-output/left/right + index + mute bit + gain)
**Why it matters:** a non-muted, audible gain on the selected path is required for sound; this is the user-facing volume control the mixer ultimately drives.

**Acceptance:**
- [ ] Host test: the emitted `SET_AMP_GAIN_MUTE` payload encoding for a given (channel, gain, mute) tuple is correct (`kernel_core::hda::realtek::tests::amp_gain_mute_payload`).
- [ ] `SET_AMP_GAIN_MUTE` is issued on the path's output amp(s) with mute clear and a sane default gain.

---

## Track F — Real-hardware bring-up (80c)

### F.1 — Dev-laptop HDA + Realtek validation (hardware-only)

**Files:**
- `scripts/hda-vfio-validate.md` (new; model on `scripts/r8125-vfio-validate.md`)
- `docs/research/hda-realtek-capture.md` (new; empirical register/codec capture)

**Symbol:** the dev laptop's AMD HDA controller `0x1022:0x15e3` + its Realtek codec; the full bring-up path (reset → STATESTS → analog-codec select → enumerate → path select → power/amp/EAPD → stream RUN)
**Why it matters:** like the Phase 79 Realtek tracks, QEMU has only a generic codec — real Realtek amp-enable/pin-default/multi-codec behavior can only be proven on hardware (VFIO passthrough or bare metal).

**Acceptance:**
- [ ] On the dev laptop, the `hda_driver` completes bring-up (claims `0x1022:0x15e3`, selects the analog Realtek codec among any present, enumerates it, selects the internal-speaker path, powers/unmutes the path, enables EAPD) — captured in `docs/research/hda-realtek-capture.md`.
- [ ] `audio-smoke` produces operator-audible, non-silent output through the internal speaker (operator-validated, not a CI sentinel; behind `M3OS_HDA_REGRESSION`).
- [ ] Any kernel/driver bug uncovered on real hardware (cf. Phase 79's ECAM/BAR/IRQ fixes) is committed + recorded in the capture doc.

---

## Track G — Release closeout (80c)

### G.1 — Bump kernel version to `0.80.0`

**Files:**
- `kernel/Cargo.toml` (`version = "0.79.0"` → `"0.80.0"`)
- `AGENTS.md` (`kernel **v0.79.0**` → `**v0.80.0**`; rewrite — not append — the Audio capability bullet to reflect out-of-process drivers + HDA, per the file's "keep it small" maintenance policy, only if it is a new capability class)

**Symbol:** `version` (Cargo manifest) + the AGENTS.md capability-inventory version string
**Why it matters:** the kernel version is the release marker for the phase; the AGENTS.md maintenance policy permits exactly this bump on phase landing.

**Acceptance:**
- [ ] Both files read `0.80.0`; `cargo xtask check` passes.
- [ ] No live kernel-version string remains at `0.79.0` (`grep -rn '0\.79\.0'` returns only historical roadmap/changelog references).
- [ ] The AGENTS.md Audio capability bullet is rewritten (not appended) to reflect out-of-process drivers + HDA.

### G.2 — Author `docs/80-intel-hda-audio.md` learning doc + cross-link

**Files:**
- `docs/80-intel-hda-audio.md` (new)
- cross-link from `docs/roadmap/80-intel-hda-audio.md` (and any existing audio learning doc, if present)

**Symbol:** new learning doc following the design-doc template sections in `docs/appendix/doc-templates.md`
**Why it matters:** AGENTS.md mandates a learning doc per phase (Phase 79 shipped `docs/79-modern-nic.md`).

**Acceptance:**
- [ ] `docs/80-intel-hda-audio.md` exists and conforms to the design-doc template sections (the same criterion the design doc's Acceptance imposes).
- [ ] It covers: mechanism/policy separation and why no kernel facade is needed; the `driver_ipc::audio` + per-submission page-grant transport (and the `sys_shm_*` alternative) + why the driver copies into its own IOMMU-domain DMA buffer; the HDA controller (CORB/RIRB sizing + **RUN-enable**, reset, widget graph, BDL, `SDnFMT`, `SDnCTL` SRST/RUN); the Realtek EAPD/GPIO/COEF amp-enable trap + per-path power/unmute; and the IOMMU IOVA-vs-physical-address difference from the Redox `ihdad` reference.

### G.3 — Roadmap README row + design-doc landing corrections + gate table

**Files:**
- `docs/roadmap/README.md` (Phase 80 row)
- `docs/roadmap/80-intel-hda-audio.md`
- `AGENTS.md` (opt-in gate table — add the `M3OS_HDA_REGRESSION` row)

**Symbol:** README row 80 Status cell; design-doc symbol/offset corrections on landing
**Why it matters:** the roadmap README is the canonical status index; the design doc's register offsets and host-symbol names must match the as-built reality.

**Acceptance:**
- [ ] On landing, README row 80 Status flips `Planned → Complete` (the Tasks cell already links `./tasks/80-intel-hda-audio-tasks.md` as of this planning PR).
- [ ] The design doc's register offsets, verb IDs, and file/symbol references match the in-tree reality (verified, no drift, at landing).
- [ ] AGENTS.md gate table lists `hda-smoke` under `M3OS_HDA_REGRESSION=1`.

---

## Documentation Notes

- **The AC'97 in-process pattern is legacy and is being retired, not extended.** This phase deliberately rejects "add an HDA backend inside `audio_server`" — the correct microkernel decomposition (matching Redox `ihdad`/`audiod`, Linux `snd-hda-intel`/ALSA, and m3OS's own `e1000`/`nvme`/`xhci`) is a separate hardware driver process + a policy/mixer server. AC'97 is extracted (Track A) rather than left as a one-off in-process exception.
- **No kernel `RemoteAudio` facade.** `RemoteNic`/`RemoteBlockDevice` exist in ring 0 only because their consumers (TCP/IP, VFS) are in-kernel. Audio's consumer (`audio_server`) is in userspace, so the driver↔server link is direct userspace IPC — adding a kernel facade would violate the userspace-first rule. The only kernel touches in scope are the possible audio-class IRQ-allocator fix in C.3 (only if real hardware/QEMU shows the Phase 79 MSI-X auto-enable defect for class `0x04`), the A.6 grant-reclamation teardown, and the **deferred** A.3 zero-copy IOMMU work.
- **Bulk PCM crosses the seam by grant, not IPC payload** (AGENTS.md IPC rule). 80a uses a *single-use* per-submission `sys_page_grant_*` (the audio analog of NVMe's `payload_grant` / e1000's per-frame grant) — **not** a persistent shared ring; a page-grant unmaps the sender's pages, so it cannot back a region `audio_server` keeps writing into. A persistent shared mapping is available via the existing `sys_shm_create`/`map`/`unmap`/`destroy` (`0x1018`–`0x101B`, `kernel/src/mm/shm.rs`, used by `display_server`) if a longer-lived buffer is later preferred. `SubmitFrames` carries a grant handle + offset/length, never samples.
- **The driver copies each submission into its own IOMMU-domain DMA buffer; the shipped design is not zero-copy.** A page-granted buffer is mapped into the driver's CPU page table only — Phase 74's `iommu_remap_grant` is a documented no-op for the receiver-holds-a-device-claim case — so it is **not** in the driver's VT-d/AMD-Vi domain. The driver allocates its hardware ring via its own `sys_device_dma_alloc` (real IOVA) and copies in, exactly as the in-process `Ac97Backend` does today (`submit_frames_inner`). True zero-copy is A.3 (deferred): it requires hardening `iommu_remap_grant` + a "does this PID own a device?" predicate, and is **not** required for 1.0.
- **AC'97 is a 32-bit-IOVA PIO device.** Its BDL `phys_addr` fields are u32 and `check_iova_fits_u32` rejects >4 GiB, so the extracted `ac97` driver keeps its own sub-4-GiB `DmaBuffer` and copies submitted frames in — it does **not** DMA from arbitrary granted pages. 80a therefore de-risks the control protocol + wiring + discovery + crash recovery, not zero-copy DMA.
- **HDA is identified by PCI class `0x040300`, not a device ID.** The driver matches class first (vendor-agnostic) plus the AMD `0x1022:0x15e3` ID for the dev laptop; gating on a single vendor ID (the AC'97 mistake) would miss most controllers. On a multi-codec board it selects the analog-output codec over an HDMI/DP-only codec; multi-codec *routing* is deferred.
- **CORB/RIRB and each stream must be explicitly RUN-enabled.** `CORBCTL.CORBRUN` + `RIRBCTL.RIRBDMAEN` (after sizing + pointer reset) start the verb DMA engines; `SDnCTL.SRST` then `SDnCTL.RUN` start the stream DMA engine. Omitting either is a "set everything up but nothing moves" trap whose symptom is an unsatisfiable "VENDOR_ID via CORB/RIRB" / "`SDnLPIB` advances" check.
- **Every amp on the output path — not just the terminal — must be unmuted and powered.** A muted mixer/selector input amp or an un-powered widget (`SET_POWER_STATE` D0) anywhere on the path is a common "stream runs but silent" cause, independent of the Realtek EAPD trap.
- **Generic widget parser vs. Realtek quirks are split** (Track C vs. Track E): the codec-agnostic enumeration/path-select/pin-default-decode is QEMU-testable; the Realtek EAPD/GPIO/COEF amp-enable is the hardware-only add-on. m3OS ships **zero** OEM SSID quirk tables — it trusts BIOS pin config like Redox `ihdad` and Linux `hda_generic.c`.
- **HDA verb-value form.** Verbs `0x2`/`0x3` (`SET_STREAM_FORMAT`/`SET_AMP_GAIN_MUTE`) are the **4-bit-verb form** (bits 19:16 of the command dword), equivalent to Linux's pre-shifted `0x200`/`0x300` in the 12-bit field — not a transcription error. BDL base is **128-byte aligned** (`SDnBDPL` lower 7 bits reserved); some references loosely say "1 KB" but the operative constraint is the low 7 bits = 0.
- **Format/rate is fixed at 48 kHz / 2 ch / 16-bit for 1.0.** `QueryCaps` returns a fixed descriptor and the driver fail-fasts if the codec does not report it in `SUPPORTED_PCM_RATES`/`SUPPORTED_STREAM_FORMATS`; runtime negotiation/resampling is deferred (design doc Deferred Until Later).
- **QEMU emulation reality drives the gates:** AC'97 (`audio-smoke`) and HDA generic-codec (`hda-smoke`) are CI-testable; Realtek-specific config + amp-enable are hardware-only (VFIO/bare-metal), behind `M3OS_HDA_REGRESSION`, exactly like the Phase 79 Realtek NIC tracks.
- **DMA position buffer (`DPLBASE`/`DPUBASE`) is intentionally deferred** — `SDnLPIB` polling is sufficient for the first driver (Redox does the same).
- Line-number references are omitted above where they would drift; the function/symbol names are the durable anchors — locate by symbol (e.g. `build_userspace_bins`, `DRIVERS_ENTRIES`, `KNOWN_CONFIGS`, `cmd_audio_smoke`), not by line.
- Register offsets and verb IDs are stated from the Linux canonical `include/sound/hda_verbs.h`, the Redox `ihdad` `#[repr(C)]` register map, and the Intel HD Audio Spec rev 1.0a (§3.3.7 reset, §4.4 CORB/RIRB); confirm against the spec section before relying on any single offset during implementation.
- Prefer the exact files/symbols above over directory-level descriptions when implementation begins; update each acceptance checkbox as the corresponding behavior lands.
