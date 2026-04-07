# Phase 55 - Hardware Substrate

**Status:** Planned
**Source Ref:** phase-55
**Depends on:** Phase 15 (Hardware Discovery) ✅, Phase 16 (Network) ✅, Phase 24 (Persistent Storage) ✅, Phase 54 (Deep Serverization) ✅
**Builds on:** Extends the QEMU/VirtIO-first system into a narrow, testable real-hardware story without abandoning the userspace-service direction established by the earlier convergence phases
**Primary Components:** kernel/src/pci, kernel/src/blk, kernel/src/net, docs/evaluation/hardware-driver-strategy.md, docs/evaluation/redox-driver-porting.md, xtask/src/main.rs

## Milestone Goal

m3OS supports a small, deliberate reference-hardware matrix with a documented driver strategy, a reusable hardware-access layer, and at least one serious real-hardware storage and networking path. The project stops talking about hardware support as a vague future aspiration and starts treating it as a bounded support promise.

## Why This Phase Exists

After the convergence phases, the next missing piece is not whether the OS can boot, schedule, or supervise processes. It is whether the project can support a real machine without quietly turning back into a kernel-centric compatibility layer. The current VirtIO-heavy sweet spot is great for development, but too narrow for a serious release story.

This phase exists to turn "real hardware" into a disciplined program with a donor strategy, reference targets, validation loops, and clearly named drivers.

## Learning Goals

- Understand the difference between reusable device logic and OS-specific integration glue.
- Learn how to choose driver donor strategies without creating licensing or architecture traps.
- See why real-hardware support must start with a reference matrix, not vague compatibility claims.
- Understand how hardware work and microkernel boundaries interact instead of competing.

## Feature Scope

### Hardware-access layer

Create the small abstractions needed for BAR mapping, DMA buffers, IRQ delivery, and device binding so later drivers share one kernel-facing contract instead of each inventing its own.

### Donor and reference strategy

Make the driver-sourcing policy explicit: public specs first, Redox as the closest Rust donor, BSD as a permissive behavioral reference, Linux as a behavior/quirk reference instead of a compatibility target.

### First reference drivers

Prioritize the first non-VirtIO wins that materially improve the support story, such as NVMe storage and Intel e1000/e1000e networking, with input support chosen only where it strengthens the hardware or later local-system story.

### Reference hardware matrix and validation loop

Choose a small set of named machines or configurations and document how the phase is validated on them. The goal is a supportable promise, not aspirational breadth.

## Critical and Non-Deferrable Items

| Item | Why it cannot be deferred in this phase |
|---|---|
| Documented donor strategy | Driver reuse decisions affect licensing, maintenance, and architecture |
| Small native hardware-access layer | Without it, each new driver drags in bespoke kernel glue |
| Named reference hardware matrix | "Works on real hardware" means nothing without named targets |
| At least one serious storage and networking path | Otherwise the hardware story remains mostly virtual |

## Evaluation Gate

| Check | Required state before closing the phase | If missing, add it to this phase |
|---|---|---|
| Service-boundary readiness | Phase 54 has already narrowed the kernel enough that new drivers do not immediately widen the TCB again | Pull missing boundary or ownership cleanup into this phase |
| Donor-source readiness | Specs, Redox references, and any BSD/Linux behavioral references are identified for the chosen drivers | Add the missing source-analysis work before implementation starts |
| Validation environment | The reference machines, QEMU configs, or lab setup are documented and reproducible | Add the missing bring-up tooling or notes to this phase |
| Release posture | The project has an agreed narrow hardware promise for the milestone | Add the missing support-matrix work instead of leaving hardware claims vague |

## Important Components and How They Work

### Hardware-access layer and device binding

The hardware-access layer should provide a reusable contract for mapping registers, allocating DMA-safe buffers, and connecting interrupts to later service or driver logic. It is the seam that keeps device support from turning into driver-by-driver kernel personality.

### Reference-driver integration

The first reference drivers should be chosen for leverage, not novelty. A serious storage path and a serious NIC path do more to validate the hardware program than a grab bag of miscellaneous device support.

### Validation and support matrix

A hardware phase is only finished if the project can reproduce the bring-up and explain the supported targets. That documentation is part of the feature, not an afterthought.

## How This Builds on Earlier Phases

- Builds on the original hardware-discovery, storage, and networking phases by moving from virtual-first success to a bounded real-hardware story.
- Depends on Phase 54 so hardware support is layered on top of a narrower architecture instead of widening the kernel again by convenience.
- Creates the substrate later reused by graphics, input, and audio work.

## Implementation Outline

1. Choose the initial reference hardware matrix and driver targets.
2. Define the donor strategy and document it in the roadmap and hardware docs.
3. Implement the hardware-access layer for BAR mapping, DMA, IRQs, and device binding.
4. Bring up the first serious storage and networking drivers on the reference targets.
5. Add reproducible validation steps for real-hardware bring-up.
6. Update support-matrix and subsystem docs to reflect the new hardware posture.

## Learning Documentation Requirement

- Create `docs/55-hardware-substrate.md` using the aligned learning-doc template in `docs/appendix/doc-templates.md`.
- Explain the hardware-access layer, donor strategy, reference matrix, and how the chosen drivers fit into the system architecture.
- Link the learning doc from `docs/README.md` when this phase lands.

## Related Documentation and Version Updates

- Update `docs/15-hardware-discovery.md`, `docs/16-network.md`, `docs/24-persistent-storage.md`, and `docs/README.md`.
- Update `docs/evaluation/hardware-driver-strategy.md`, `docs/evaluation/redox-driver-porting.md`, and `docs/evaluation/roadmap/R08-hardware-substrate.md`.
- Update `README.md`, `docs/roadmap/README.md`, and any setup or validation docs that describe supported hardware.
- When the phase lands, bump `kernel/Cargo.toml` and any release/version references to `0.55.0`.

## Acceptance Criteria

- A documented donor strategy exists and is followed for the first real-hardware drivers.
- A small native hardware-access layer exists for BAR mapping, DMA, IRQs, and device binding.
- The project documents a narrow reference hardware matrix and how to validate it.
- At least one serious storage path and one serious networking path work on the reference targets.
- Real-hardware bring-up is reproducible enough to be part of the release narrative.

## Companion Task List

- Phase 55 task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Mature operating systems support much broader device matrices and more complex driver frameworks than m3OS should attempt here.
- Linux driver internals and licensing assumptions make direct reuse a poor primary strategy.
- m3OS should favor a small number of understandable, well-documented drivers on named hardware over shallow breadth.

## Deferred Until Later

- Broad laptop/desktop certification
- Wide Wi-Fi, GPU, and USB peripheral matrices
- IOMMU-heavy isolation work
- Hardware-acceleration features not needed for the reference targets
