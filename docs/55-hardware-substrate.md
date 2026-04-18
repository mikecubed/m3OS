# Hardware Substrate

**Aligned Roadmap Phase:** Phase 55
**Status:** Planned
**Source Ref:** phase-55
**Supersedes Legacy Doc:** (none -- new content)

> **Note:** This learning doc is a skeleton. Full content is populated by Track F
> (task F.2 in `docs/roadmap/tasks/55-hardware-substrate-tasks.md`) once the NVMe
> and e1000 driver work lands. Until then, only the "Donor Strategy" section and
> placeholder headings are intentional; the remaining sections exist so the
> design doc has a stable target to cross-reference.

## Overview

Phase 55 turns m3OS's hardware story from "QEMU/VirtIO-centric development target"
into a bounded, named real-hardware support promise. The scope covers a small
native hardware-access layer (BAR mapping, DMA buffers, IRQ delivery, device
binding), PCI modernization (PCIe MCFG, MSI/MSI-X, device claim), and two
reference drivers (NVMe storage, Intel 82540EM e1000 network). Final narrative
content lives with the completed drivers and belongs to F.2.

## What This Doc Covers

_Placeholder -- populated by Track F.2._

## Donor Strategy

Phase 55 adopts the driver-sourcing strategy recorded in
[docs/evaluation/hardware-driver-strategy.md](./evaluation/hardware-driver-strategy.md)
as explicit phase policy. The adoption order is:

1. **Public specs and datasheets first.** Register layouts, command structures,
   descriptor formats, and reset sequencing come from the NVMe spec and the
   Intel 82540EM manuals before any third-party driver code is consulted.
2. **Redox second.** The [Redox drivers repository](https://gitlab.redox-os.org/redox-os/drivers)
   is m3OS's closest external donor: Rust, microkernel-oriented, MIT-licensed,
   and already covers NVMe (`nvmed`) and e1000 (`e1000d`). Per
   [redox-driver-porting.md](./evaluation/redox-driver-porting.md) the reusable
   part is device logic; Redox's scheme/daemon/event glue is not ported.
3. **BSD third.** BSD drivers are consulted as a permissive reference when
   specs or Redox are insufficient, but not used as a primary donor.
4. **Linux as reference only.** Linux is treated as a hardware encyclopedia for
   PCI IDs, quirks, reset sequencing, and timeouts. No Linux driver code is
   imported; m3OS's MIT licensing and native driver model make direct Linux
   reuse the wrong trade-off for this phase.

### Specific Redox drivers consulted

Concrete Redox references adopted during Phase 55 bring-up will be recorded in
this section as NVMe (Track D) and e1000 (Track E) work lands. Each entry
records the Redox driver path, the specific device-logic areas borrowed, and
the m3OS-native replacements for the non-portable Redox integration glue
(`redox_scheme`, `pcid`, `redox_event`, `redox-daemon`, `libredox`).

At the time this skeleton was written, no Redox driver references have been
adopted yet. Track F.2 replaces this paragraph with the actual record.

### Deviations from the donor strategy

Any Phase 55 deviation from the above order (for example, deliberately not
consulting a Redox reference for a specific driver) is documented here with
rationale. None recorded yet.

## Hardware-Access Layer

_Placeholder -- populated by Track F.2 once Track C has landed. Expected to
cover `BarMapping`, `DmaBuffer`, `DeviceIrq`, and the PCI device claim / driver
registration protocol introduced by Tracks B and C._

## Reference Matrix

_Placeholder -- populated by Track F.2. The authoritative reference hardware
matrix lives in [docs/roadmap/55-hardware-substrate.md](./roadmap/55-hardware-substrate.md)
under "Reference Hardware Matrix". F.2 mirrors the supported entries here with
learner-friendly framing and links the design-doc matrix as the source of truth._

## Driver Architecture

_Placeholder -- populated by Track F.2 once Tracks D and E have landed. Expected
to explain the NVMe controller path (admin queue, I/O queue pair, PRP-based
read/write, MSI/MSI-X completion), the e1000 path (TX/RX descriptor rings, IRQ
handling, packet dispatch), and how both drivers plug into the hardware-access
layer instead of inventing bespoke glue._

## Differences From Later Hardware Work

_Placeholder -- populated by Track F.2. Expected to contrast Phase 55's bounded
NVMe + e1000 ring-0 bring-up with later ring-3 driver host work, broader device
coverage (xHCI, Realtek, AHCI, HDA), and IOMMU-aware DMA isolation._

## Key Files

_Placeholder -- populated by Track F.2 once all Phase 55 modules exist._

| File | Purpose |
|---|---|
| _to be filled by F.2_ | _to be filled by F.2_ |

## Related Roadmap Docs

- [Phase 55 roadmap doc](./roadmap/55-hardware-substrate.md)
- [Phase 55 task doc](./roadmap/tasks/55-hardware-substrate-tasks.md)
- [Hardware driver strategy evaluation](./evaluation/hardware-driver-strategy.md)
- [Redox driver porting evaluation](./evaluation/redox-driver-porting.md)

## Deferred or Later-Phase Topics

- Broad laptop/desktop hardware certification
- Wi-Fi, GPU, and USB peripheral matrices
- IOMMU-aware DMA isolation (VT-d / AMD-Vi)
- Ring-3 extraction of the NVMe and e1000 drivers following the Phase 54
  `vfs_server` / `net_server` pattern
- Hardware-acceleration features not needed for the reference targets
