# Phase 55 - Hardware Substrate

**Status:** Complete
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

Choose a small set of named machines or configurations and document how the phase is validated on them. The goal is a supportable promise, not aspirational breadth. The concrete list of named targets and the exact QEMU invocations used for validation are recorded in "Reference Hardware Matrix" and "Reference QEMU configurations" later in this doc.

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

## Reference Hardware Matrix

The following table is the bounded, named set of targets Phase 55 commits to supporting. "Works on real hardware" for Phase 55 means "works on an entry in this table". Entries outside the table are explicitly out of scope.

### QEMU-emulated reference targets

These targets are validated in the project's CI / xtask harness and are the primary surface for Phase 55 bring-up and regression.

| Device class | Target | PCI vendor:device | QEMU flag | Physical test hardware | Validation status |
|---|---|---|---|---|---|
| Block storage (VirtIO) | VirtIO-blk (existing baseline) | `0x1af4:0x1001` | `-drive file=disk.img,if=virtio` (default) | none at this time | QEMU emulation validated (baseline, pre-Phase 55) |
| Block storage (NVMe) | QEMU NVMe controller | `0x1b36:0x0010` | `-drive file=nvme.img,if=none,id=nvme0 -device nvme,serial=deadbeef,drive=nvme0` | none at this time | QEMU emulation planned; physical target deferred |
| Network (VirtIO) | VirtIO-net (existing baseline) | `0x1af4:0x1000` | `-netdev user,id=net0 -device virtio-net,netdev=net0` (default) | none at this time | QEMU emulation validated (baseline, pre-Phase 55) |
| Network (Intel e1000) | Intel 82540EM classic e1000 | `0x8086:0x100E` | `-device e1000,netdev=net0 -netdev user,id=net0` | none at this time | QEMU emulation planned; physical target deferred |

Notes on the QEMU entries:

- The VirtIO rows are the existing baseline and remain the default when `cargo xtask run` is invoked with no device overrides. Phase 55 does not break or replace them.
- The NVMe and e1000 rows are the new targets introduced by Phase 55. They are exposed by the xtask `--device` flags documented in "Reference QEMU configurations" below and in task F.1.
- The e1000e family (82574, 82576, etc.) is different silicon and is **not** in scope. See Documentation Notes in `docs/roadmap/tasks/55-hardware-substrate-tasks.md` for the "Intel NIC scope" note.

### Physical-hardware reference targets

Phase 55 does not commit a named physical machine to its support promise. As specific hardware gets validated (either in a lab rig or on a contributor machine) those entries belong in a dedicated row with device class, PCI IDs, board/model, and the validation artifact that records the run.

| Device class | Target | PCI vendor:device | Physical test hardware | Validation status |
|---|---|---|---|---|
| Block storage (NVMe) | Any NVMe-class device matching the spec behavior Phase 55 relies on | vendor-specific; recorded on first validated run | none at this time | physical target deferred |
| Network (Intel e1000) | Intel 82540EM or pin-compatible classic e1000 silicon | `0x8086:0x100E` | none at this time | physical target deferred |

**IOMMU caveat for all physical-hardware entries:** VT-d / AMD-Vi enabled systems may block driver DMA until IOMMU mappings exist; IOMMU support is deferred per Phase 55 design doc. Validators running on physical hardware should either disable the IOMMU in firmware for Phase 55 bring-up or record the failure mode so a later phase can address it.

### Reference QEMU configurations

The exact QEMU invocations that Phase 55 development and CI target. These are recorded before driver development starts so implementation and documentation cannot drift apart. Task F.1 exposes these as xtask subcommands (`cargo xtask run --device nvme` and `cargo xtask run --device e1000`); until F.1 lands, validators can pass the flags below directly.

**NVMe reference configuration:**

```
-drive file=nvme.img,if=none,id=nvme0 -device nvme,serial=deadbeef,drive=nvme0
```

Notes: `nvme.img` is the backing file for the NVMe namespace. The `serial=deadbeef` value is arbitrary but required by QEMU. This is an **addition** to the existing VirtIO-blk boot disk, not a replacement for it.

**e1000 reference configuration:**

```
-device e1000,netdev=net0 -netdev user,id=net0
```

Notes: This **replaces** the default VirtIO-net device for that run. The `user` netdev uses QEMU's SLIRP user-mode networking, consistent with the existing VirtIO-net default.

**Default behavior remains VirtIO.** Both VirtIO configurations stay the default for `cargo xtask run` and `cargo xtask run-gui`. NVMe and e1000 are opt-in and must not break the VirtIO path.

**xtask cross-reference.** The xtask integration that exposes these configurations as repeatable flags is owned by task F.1 in `docs/roadmap/tasks/55-hardware-substrate-tasks.md`. The flag names F.1 commits to are `--device nvme` and `--device e1000`, and they must remain consistent with the raw QEMU strings above.

## Evaluation Gate Verification

All four evaluation-gate rows have been verified at Phase 55 close (Track F).
The authoritative narrative lives in the "Evaluation Gate Verification"
section of [docs/55-hardware-substrate.md](../55-hardware-substrate.md)
(learning doc); the results are summarised here for cross-reference:

| Gate | Verification status at close |
|---|---|
| Service-boundary readiness | Verified. Phase 54 serverization narrowed the kernel enough that NVMe and e1000 do not widen the TCB beyond the hardware-access-layer contract. Ring-0 placement is a deliberate trade-off with a documented ring-3 extraction path (see "Ring-0 placement is deliberate and bounded" in the task-doc Documentation Notes). |
| Donor-source readiness | Verified. NVMe spec 1.4 and Intel 82540EM manual §13.4/§13.5 were the primary sources; Redox `nvmed` and `e1000d` remained the closest Rust external donors but no Redox code was imported. Details in the learning doc's "Specific Redox drivers consulted" subsection. |
| Validation environment | Verified. Task F.1 exposes the Reference QEMU Configurations as `cargo xtask run --device nvme` and `cargo xtask run --device e1000`, plus their combination. The xtask flags reproduce the exact QEMU fragments recorded above. Physical-hardware validation is deferred per the IOMMU caveat on the matrix. |
| Release posture | Verified. The Reference Hardware Matrix above is the narrow hardware promise for the milestone; entries outside the matrix are out of scope. The matrix is cross-referenced from both this design doc and the learning doc. |

With all four gates verified, Phase 55 closure activities (kernel version
bump to `0.55.0`, roadmap row flip to Complete, cross-subsystem doc updates)
proceed in Track F.
