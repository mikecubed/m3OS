# Phase 57 - Audio and Local Session

**Status:** Planned
**Source Ref:** phase-57
**Depends on:** Phase 47 (DOOM) ✅, Phase 55 (Hardware Substrate) ✅, Phase 56 (Display and Input Architecture) ✅
**Builds on:** Extends the first graphical architecture into a minimally complete local-system story by adding audio output and a coherent graphical session flow
**Primary Components:** kernel or userspace audio driver path, future audio device API, display/session services, userspace terminal or launcher, docs/29-pty-subsystem.md

## Milestone Goal

m3OS supports a minimal local interactive session that feels like a system rather than a demo: there is a defined graphical session entry path, a basic terminal or launcher workflow, and audible PCM output on the supported platform.

## Why This Phase Exists

Once the display and input architecture exists, the remaining gap to a believable local system is no longer "can pixels move?" It is whether the system has the rest of the basic human-facing substrate: entering a session, launching something useful, and producing sound.

This phase exists to turn the graphical architecture into a small but coherent local-session experience without pretending that a full desktop ecosystem already exists.

## Learning Goals

- Understand how audio output fits into a minimally useful graphical system.
- Learn how session entry, launcher/terminal behavior, and recovery rules make a UI feel like an operating environment instead of a technology demo.
- See how DMA- or device-driven audio differs from text and graphics subsystems in its latency and buffering requirements.
- Understand which parts of local-session polish are essential and which can wait.

## Feature Scope

### Audio output path

Implement the first supported audio-output contract on the supported target, with a userspace-facing API and a clearly documented driver choice. Single-client or otherwise simplified audio is acceptable if the behavior is explicit.

### Local-session entry and launcher flow

Define how a user reaches the local graphical session, how a minimal launcher or terminal is started, and how the system returns to a recoverable administration path if the session fails.

### Graphical terminal and application baseline

Provide at least one genuinely useful local graphical client, such as a terminal emulator, plus the basic launcher/session glue needed to treat it as part of the system rather than a standalone demo.

### Session shutdown and recovery behavior

The local session needs a clear stop, restart, and fallback path just like the headless service model does.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Audible PCM output on the supported target | Audio is the core new subsystem being added here |
| A defined local-session entry path | The phase must produce a session, not just a device driver |
| At least one useful graphical client workflow | Otherwise the local system remains a pure demo |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Display/session baseline | Phase 56's display and input architecture is already stable and documented | Pull missing session or compositor work into this phase |
| Hardware baseline | Phase 55 identifies and validates the supported audio target or fallback environment | Add the missing hardware-driver or validation work here |
| Recovery baseline | The service/session model can recover from local-session failure | Add missing fallback or restart behavior before closing |
| Scope discipline | The phase defines the minimum useful local-system story and what remains later | Add the missing non-goals and support-boundary documentation |

## Important Components and How They Work

### Audio device contract

The audio path should define how userspace writes PCM data, what buffering model exists, and what simplified assumptions are acceptable for the first supported target.

### Local-session startup flow

The session entry path connects the existing service model to the graphical stack. It should be clear who starts the session, how the first useful app appears, and what happens on failure.

### Terminal or launcher baseline

The first useful local client is the difference between a graphical stack and a local system. This component anchors how users actually interact with the new session.

## How This Builds on Earlier Phases

- Builds on Phase 55's hardware strategy for the first supported audio target.
- Uses the Phase 47 graphics proof as the earlier validation that full-screen graphical workloads already run on the system.
- Extends Phase 56's display/input model into a minimally complete local-session experience.
- Prepares the optional local-system branch that the release gate can either include or defer explicitly.

## Driver hosting and supervision

`audio_server` is a **Phase 55b-style ring-3 supervised driver**, claiming the audio device through `sys_device_claim` and operating it through the existing Phase 55b device-host primitive set (`sys_device_mmio_map`, `sys_device_dma_alloc`, `sys_device_irq_subscribe`). The kernel does not regain a custom audio driver in ring 0 under any circumstance; this subsection records that discipline so a later phase cannot quietly move audio back into the kernel.

The supervision shape is identical to the existing ring-3 NVMe and e1000 drivers:

- **Claim path.** `audio_server` calls `sys_device_claim` on the chosen target's PCI BDF (Intel 82801AA AC'97, vendor `0x8086` device `0x2415` per [`docs/appendix/phase-57-audio-target-choice.md`](../appendix/phase-57-audio-target-choice.md) — the chosen-target memo). The kernel verifies IOMMU BAR identity-coverage per Phase 55c R2 before the claim succeeds; a coverage failure is a hard error, not a warning. The audio device's BARs ride the same `BarCoverage::assert_bar_identity_mapped` check that NVMe and e1000 already exercise — see Phase 55b's claim-path documentation in [`docs/55b-ring-3-driver-host.md`](../55b-ring-3-driver-host.md).
- **MMIO and DMA.** BAR0 (the AC'97 mixer block) and BAR1 (the AC'97 bus-master block) map through `sys_device_mmio_map`. The Buffer Descriptor List (BDL) page and the PCM data ring allocate through `sys_device_dma_alloc`, which routes them through the per-device IOMMU domain established at claim time. **No new kernel DMA helper is introduced**; the existing Phase 55a `DmaBuffer<T>` primitive carries the audio buffers exactly as it carries NVMe submission queues and e1000 descriptor rings today.
- **IRQ multiplexing via Phase 55c bound notifications.** The audio IRQ binds to a `Notification` object via `sys_device_irq_subscribe`, and `audio_server`'s single-threaded io loop multiplexes the IRQ source and the client listening endpoint through the same `RecvResult` machinery the e1000 driver introduced in Phase 55c. The contract is **declared here** and **wired in D.4** (`userspace/audio_server/src/irq.rs`): `subscribe_and_bind` calls `IrqNotification::bind_to_endpoint`; `run_io_loop` blocks only on `endpoint.recv_multi(&irq_notif)`; on `RecvResult::Notification { bits }` the loop calls the backend's IRQ handler; on `RecvResult::Message(req)` it dispatches to the protocol codec. No `irq.wait()` calls in the io loop. Cross-reference: Phase 55c bound-notification design notes at [`docs/appendix/phase-55c-net-send-shape.md`](../appendix/phase-55c-net-send-shape.md).
- **Service manifest and restart policy.** `etc/services.d/audio_server.conf` declares `restart=on-failure max_restart=3` (matching the Phase 56 F.1 supervisor `on-restart` precedent). On crash the supervisor reaps `audio_server`, releases its capabilities (including the `sys_device_claim` handle), and forks a new instance which re-runs `sys_device_claim` and `IrqNotification::bind_to_endpoint` during its `init` step. **No kernel-side claim persistence across restart is introduced.** The kernel's syscall surface is byte-identical to Phase 55b — `audio_server` is a new caller of existing primitives, not a new kernel feature.

### What is **not** changed in the kernel

The kernel does **not** learn audio. It only learns "device claim covers the audio BAR(s)." Specifically:

- **No `sys_audio_*` syscalls.** Per [`docs/appendix/phase-57-audio-abi.md`](../appendix/phase-57-audio-abi.md) (the Phase 57 ABI memo), audio is a pure-userspace IPC contract on `audio_server`'s endpoint. The kernel gains no new audio syscall arms in `sys_dispatch`.
- **No kernel-side audio facade.** No `RemoteAudio` analogous to Phase 55b's `RemoteBlockDevice` / `RemoteNic`. There are no legacy kernel callers for audio; the facade pattern does not apply.
- **No new IRQ-handler logic in ring 0.** The kernel's MSI ISR continues to do the minimum (read status, set a notification bit, EOI). All AC'97-specific status-register interpretation lives in `userspace/audio_server/src/device.rs`.
- **No audio-specific DMA helper.** `sys_device_dma_alloc` is unchanged. The IOMMU domain machinery is unchanged. The Phase 55a `DmaBuffer<T>` primitive is unchanged. Audio buffers ride the existing path.
- **No new entry in `kernel-core::driver_ipc`.** The audio wire format lives in `kernel-core::audio::protocol` (Track B.3), consumed by `audio_server` and `audio_client` directly without a kernel-side IPC dispatch helper.

The single concrete change in `kernel/` for Phase 57 audio is one line of widening: `kernel/src/device_host/mod.rs`'s claim path recognizes `0x8086:0x2415` (Intel AC'97) as a valid claim target alongside the existing NVMe and e1000 IDs (Track C.1). That is the entire kernel-side audio surface.

### Cross-references

- Chosen-target memo: [`docs/appendix/phase-57-audio-target-choice.md`](../appendix/phase-57-audio-target-choice.md) — Intel 82801AA AC'97, BDL DMA model, IRQ shape.
- Audio ABI memo: [`docs/appendix/phase-57-audio-abi.md`](../appendix/phase-57-audio-abi.md) — pure-userspace IPC, no kernel facade.
- Phase 55b ring-3 driver host pattern: [`docs/55b-ring-3-driver-host.md`](../55b-ring-3-driver-host.md) — `sys_device_claim` and the five device-host primitives.
- Phase 55a IOMMU substrate: [`docs/55a-iommu-substrate.md`](../55a-iommu-substrate.md) — VT-d / AMD-Vi domains and `DmaBuffer<T>` routing.
- Phase 55c bound-notification + `RecvResult` shape: [`docs/appendix/phase-55c-net-send-shape.md`](../appendix/phase-55c-net-send-shape.md) — io-loop multiplexing pattern that D.4 reuses verbatim for audio.

## Implementation Outline

1. Choose the first supported audio target and userspace-facing API.
2. Implement the minimum audio-output path needed for the local-system story.
3. Define and wire the graphical session entry flow.
4. Ship at least one useful graphical client workflow, such as a terminal plus launcher.
5. Validate shutdown, recovery, and fallback behavior for the local session.
6. Update docs to distinguish the supported local-system path from later desktop ambitions.

## Learning Documentation Requirement

- Create `docs/57-audio-and-local-session.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the audio contract, session flow, launcher/terminal behavior, and how this phase differs from a full desktop environment.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/29-pty-subsystem.md`, `docs/README.md`, and `docs/roadmap/README.md`.
- Update `docs/evaluation/usability-roadmap.md`, `docs/evaluation/gui-strategy.md`, and `docs/evaluation/roadmap/R09-display-and-input-architecture.md`.
- Update hardware/audio support docs and any session-startup or local-login documentation.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.57.0`.

## Acceptance Criteria

- The supported target can produce audible PCM output through the documented audio contract.
- There is a documented and working path into a local graphical session.
- A user can launch and use at least one genuinely useful graphical client, such as a terminal.
- Session shutdown, crash recovery, and fallback to administration are documented and tested.
- The docs clearly distinguish this minimal local session from a broader future desktop ecosystem.

## Companion Task List

- Phase 57 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature desktop operating systems ship with richer sound servers, multimedia stacks, login managers, and application ecosystems.
- m3OS should begin with a deliberately small local-session story that proves the concept and stays operable.
- The right comparison is not "does this match Linux desktop polish?" but "does this create a coherent local-system milestone?"

## Deferred Until Later

- Rich desktop audio routing and mixing
- Media playback, recording, and advanced codecs
- Multiple graphical sessions or richer display-manager features
- Full desktop shell, notifications, settings panels, and broader app ecosystems
