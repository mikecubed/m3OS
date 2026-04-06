# Release Phase R08 — Hardware Substrate

**Status:** Proposed  
**Depends on:** [R07 — Deep Serverization](./R07-deep-serverization.md)  
**Official roadmap phases covered:** [Phase 15](../../roadmap/15-hardware-discovery.md),
[Phase 16](../../roadmap/16-network.md),
[Phase 24](../../roadmap/24-persistent-storage.md),
[Phase 47](../../roadmap/47-doom.md),
[Phase 48](../../roadmap/48-mouse-input.md),
[Phase 49](../../roadmap/49-audio.md)  
**Primary evaluation docs:** [Hardware Driver Strategy](../hardware-driver-strategy.md),
[Redox Driver Porting](../redox-driver-porting.md),
[GUI Strategy](../gui-strategy.md)

## Why This Phase Exists

m3OS is no longer limited by whether it can boot or schedule tasks. It is now
limited by whether it can support a **small, deliberate set of real hardware**
without abandoning its architectural direction. The current driver story is still
QEMU- and VirtIO-heavy, which is excellent for development but too narrow for a
serious 1.0 claim.

This phase exists to turn hardware support into a disciplined program instead of
a vague ambition: choose the donor strategy, build the missing platform
primitives, and support a narrow reference hardware matrix first.

```mermaid
flowchart TD
    SPEC["Public specs"] --> HAL["m3OS hardware-access layer"]
    REDOX["Redox logic"] --> HAL
    BSD["BSD reference"] --> HAL
    LINUX["Linux quirks/reference"] --> HAL
    HAL --> DRV1["NVMe"]
    HAL --> DRV2["Intel e1000/e1000e"]
    HAL --> DRV3["PS/2 mouse"]
    DRV1 --> HW["Reference hardware boot"]
    DRV2 --> HW
```

## Current vs. required vs. later

| Area | Current state | Required in this phase | Later extension |
|---|---|---|---|
| Bus/platform | Legacy PCI discovery and QEMU-friendly assumptions dominate | PCIe-era helpers, cleaner BAR mapping, stronger interrupt and DMA discipline | IOMMU awareness and wider platform maturity |
| Storage | VirtIO-first | NVMe on a reference machine or equivalent serious real-hardware path | AHCI and broader storage matrix |
| Networking | VirtIO-first | Intel e1000/e1000e-class support on reference hardware | Realtek and other NIC families |
| Input | Keyboard story exists, mouse is planned | Minimal pointing-device support that fits the GUI plan | USB HID and richer input devices |
| Audio/GPU | Not a release blocker yet | Clear scope line around what is and is not needed for 1.0 | Audio, richer display, later GPU work |

## Detailed workstreams

| Track | What changes | Why now |
|---|---|---|
| Hardware-access layer | Add small native abstractions for BAR mapping, DMA buffers, IRQ delivery, and device binding | Native abstractions are cheaper than foreign-ABI shims |
| Donor strategy | Use specs first, Redox second, BSD third, Linux as behavior reference only | This keeps licensing and architecture sane |
| Reference hardware matrix | Choose a small number of known-good systems and document them | 1.0 needs supportable promises, not vague "real hardware" claims |
| First serious drivers | Prioritize NVMe and Intel e1000/e1000e, with PS/2 mouse as an input bridge | These give the highest leverage for headless and later GUI work |
| Validation loop | Make real-hardware bring-up reproducible and observable, not a one-off lab success | Real support requires repeatability |

## How This Differs from Linux, Redox, and production systems

- **Linux** has the broadest hardware ecosystem in existence, but its driver
  model is tightly bound to Linux internals and licensing assumptions.
- **Redox** is the closest donor because it is Rust-based, userspace-driver
  oriented, and MIT-licensed, but its drivers are still not drop-in because they
  assume Redox-specific integration layers.
- **Production OSes** succeed on hardware by picking clear abstractions and
  maintaining them over time. m3OS needs the small version of that discipline,
  not an instant compatibility layer.

## What This Phase Teaches

This phase teaches how to separate **device logic** from **OS integration**.
That is the key to borrowing from Redox or BSD without warping m3OS into someone
else's kernel personality.

It also teaches a useful release lesson: "works on real hardware" only means
something when the project can name the hardware and explain the support level.

## What This Phase Unlocks

After this phase, m3OS can stop talking about real hardware as a future dream
and start talking about it as a narrow, testable part of the release story. That
benefits both the headless/admin path and the later local desktop path.

## Acceptance Criteria

- A small native hardware-access layer exists for BAR mapping, DMA, IRQs, and
  device binding
- The reference donor strategy is documented and followed consistently
- NVMe and Intel e1000/e1000e-class support exist on a documented reference
  machine or equivalent narrow target
- Real-hardware boot and validation steps are documented and repeatable
- No Linux compatibility layer or Redox scheme-emulation layer is introduced as
  the primary hardware strategy

## Key Cross-Links

- [Hardware Driver Strategy](../hardware-driver-strategy.md)
- [Redox Driver Porting](../redox-driver-porting.md)
- [Phase 15 — Hardware Discovery](../../roadmap/15-hardware-discovery.md)
- [Phase 48 — Mouse Input](../../roadmap/48-mouse-input.md)

## Open Questions

- Does 1.0 require MSI/MSI-X immediately, or is a narrower interrupt story
  acceptable on the first reference matrix?
- Is PS/2 mouse enough for the first local-system milestone, or must USB HID be
  in scope before 1.0?
