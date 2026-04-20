# Phase 55a - IOMMU Substrate

**Status:** Planned
**Source Ref:** phase-55a
**Depends on:** Phase 55 (Hardware Substrate) ✅
**Builds on:** Closes the DMA-isolation gap intentionally deferred from Phase 55, routing Phase 55's `DmaBuffer<T>` allocation through per-device VT-d / AMD-Vi mappings so the hardware-access layer can protect the kernel from device-initiated memory corruption.
**Primary Components:** kernel/src/acpi, kernel/src/iommu (new), kernel/src/mm/dma, kernel/src/pci

## Milestone Goal

m3OS parses the ACPI DMAR / IVRS tables, constructs per-device DMA translation domains, and routes every `DmaBuffer<T>` allocation through an IOMMU-mapped IOVA. A malformed or hostile device descriptor no longer has write access to arbitrary kernel memory. The Phase 55 reference matrix extends to cover a validated `-device intel-iommu` QEMU configuration.

## Why This Phase Exists

Phase 55 established the hardware-access layer (`BarMapping`, `DmaBuffer<T>`, `DeviceIrq`) and shipped two real-hardware drivers (NVMe, Intel 82540EM e1000) on top of it. The Reference Hardware Matrix in the Phase 55 design doc explicitly flags an IOMMU caveat: physical-hardware validation is blocked on systems with VT-d or AMD-Vi enabled, because the flat physical-to-bus identity mapping used by `DmaBuffer<T>` requires IOMMU cooperation it currently does not have.

Ring-0 drivers programming bus-mastering devices without IOMMU protection is a known bounded compromise (documented in the Phase 55 task doc's "Ring-0 placement is deliberate and bounded" note). Ring-3 drivers (Phase 55b) without IOMMU protection is not a bounded compromise — it is a regression, because a userspace driver with raw DMA authority can corrupt arbitrary kernel memory via bus-master writes without the kernel having the ability to audit the descriptor chain. IOMMU isolation must land before the ring-3 driver host.

## Learning Goals

- Understand the difference between CPU MMU translation (process memory isolation) and IOMMU translation (device DMA isolation), and why both are required for a properly enforced microkernel.
- See how ACPI DMAR (Intel) and IVRS (AMD) describe DMA remapping hardware to the OS, and how the OS uses those descriptions to discover IOMMU units and the devices behind them.
- Learn the VT-d / AMD-Vi page-table format and how the OS uses it to map IOVA (I/O virtual address) ranges to physical frames on a per-device basis.
- Understand the design choice between identity-mapped domains (every IOVA equals its physical frame; simplest, lowest protection) and translated domains (per-device IOVA space; highest protection) and why Phase 55a picks the translated-domain shape.

## Feature Scope

### ACPI DMAR and IVRS parsing

Extend the Phase 15 / Phase 55 ACPI parser in `kernel/src/acpi/mod.rs` to locate DMAR (Intel VT-d) or IVRS (AMD-Vi) tables and decode their top-level entries: DMA remapping hardware unit addresses, scope structures describing which PCI segments and devices each unit covers, and reserved-memory region descriptors. The output is a list of IOMMU units and a device-to-unit mapping consumed by the new `kernel/src/iommu/` module.

### IOMMU unit initialization

Introduce `kernel/src/iommu/` containing the per-unit bring-up code: map each unit's register MMIO through the Phase 55 `BarMapping` abstraction, enable translation, install the root / device-table structure. Support VT-d and AMD-Vi initialization behind a common `IommuUnit` trait so the rest of the kernel is vendor-agnostic.

### Per-device DMA domain and IOVA allocator

Each PCIe device claimed through `claim_pci_device` (Phase 55's `PciDeviceHandle`) gets its own DMA domain at claim time. The domain owns a page-table hierarchy in IOVA space and an allocator that hands out aligned IOVA ranges. Domain lifetime tracks the handle's lifetime — releasing the device releases the domain.

### `DmaBuffer<T>` routed through IOMMU mappings

Rewrite `kernel/src/mm/dma.rs::DmaBuffer<T>` so `allocate` takes a `&PciDeviceHandle` (or equivalent device reference) and installs an IOVA mapping in that device's domain. `DmaBuffer::bus_address()` returns the IOVA, not the physical frame address. On `Drop`, the IOVA mapping is invalidated and the IOMMU TLB is flushed for that unit.

### Identity-map fallback for bring-up

If ACPI reports no IOMMU (legacy firmware, QEMU without `-device intel-iommu`), `DmaBuffer<T>` falls back to the Phase 55 flat-physical path so the default `cargo xtask run` configurations still work. The fallback path is gated behind a boot-time log line so it is always observable when IOMMU protection is absent.

## Important Components and How They Work

### `kernel/src/iommu/mod.rs`

Top-level module exposing `init_from_acpi(rsdp)`, `domain_for_device(handle)`, and the `IommuUnit` trait. Vendor-specific submodules (`intel.rs` for VT-d, `amd.rs` for AMD-Vi) implement the trait behind a common interface. `init_from_acpi` runs once during kernel init after PCIe ECAM is online, before any driver probe runs.

### `DmaBuffer::allocate(device, size)`

Replaces Phase 55's `DmaBuffer::allocate(size)`. Allocates physically-contiguous frames through the Phase 53a buddy allocator, installs an IOVA mapping in the device's domain, returns a `DmaBuffer<T>` whose `bus_address()` is the IOVA. Drivers that need a physical frame address explicitly (rare — mostly legacy descriptors) call `DmaBuffer::physical_address()`.

### Reserved-region handling

DMAR / IVRS tables describe reserved memory regions (typically firmware GPU framebuffer, ACPI tables, EFI runtime) that must remain identity-mapped in every domain or the affected device stops working. The IOMMU bring-up pre-installs those reserved regions in each new domain before the domain accepts driver allocations.

## How This Builds on Earlier Phases

- Extends Phase 15's ACPI table parsing with DMAR / IVRS decoding.
- Extends Phase 55's `DmaBuffer<T>` and `PciDeviceHandle` contracts to carry per-device IOMMU state without changing their driver-facing signatures beyond the new `device` argument to `DmaBuffer::allocate`.
- Reuses Phase 53a's `alloc_contiguous_frames` for the physical backing of IOVA-mapped allocations.
- Closes the IOMMU caveat documented in the Phase 55 Reference Hardware Matrix so the matrix can add `-device intel-iommu` as a validated configuration.

## Implementation Outline

1. Extend `kernel/src/acpi/mod.rs` to parse DMAR and IVRS tables and expose the resulting IOMMU unit list and device-to-unit mapping.
2. Create `kernel/src/iommu/mod.rs` with the `IommuUnit` trait, domain type, and IOVA allocator; implement `intel.rs` (VT-d) first, `amd.rs` (AMD-Vi) second.
3. Wire `PciDeviceHandle` creation in `kernel/src/pci/mod.rs` to request a domain from the matching IOMMU unit.
4. Rewrite `DmaBuffer<T>` to accept a device reference and route through the device's domain; migrate all Phase 55 callers (NVMe, e1000, VirtIO-blk, VirtIO-net).
5. Add `-device intel-iommu` as an xtask validation configuration (`cargo xtask run --iommu` or equivalent flag); extend the Phase 55 reference matrix to name the validated IOMMU configuration.
6. Update `docs/15-hardware-discovery.md`, `docs/55-hardware-substrate.md`, and the Phase 55 Reference Hardware Matrix to remove the IOMMU caveat for validated configurations.

## Acceptance Criteria

- Booting under QEMU with `-device intel-iommu` logs DMAR parsing output, IOMMU unit bring-up, and domain creation per claimed device.
- NVMe and e1000 drivers operate unchanged (data-path smoke passes) with IOMMU translation enabled.
- A deliberately-malformed NVMe PRP entry pointing outside the driver's DMA allocation triggers an IOMMU fault rather than corrupting kernel memory.
- `DmaBuffer::bus_address()` returns an IOVA (not a physical frame address) when IOMMU is active; callers that need the physical address use the explicit `physical_address()` accessor.
- Default `cargo xtask run` (no IOMMU) still boots cleanly with the identity-map fallback path; the fallback is logged at boot.
- Kernel version is bumped to `v0.55.1` across `kernel/Cargo.toml`, `AGENTS.md`, `README.md`, and both roadmap READMEs.

## Companion Task List

- Phase 55a task list — defer until implementation planning begins.

## How Real OS Implementations Differ

- Linux's `drivers/iommu/` subsystem supports many more IOMMU hardware variants (ARM SMMU, Apple DART, etc.) and integrates with the DMA API (`dma_alloc_coherent`, streaming DMA, scatter-gather), whereas Phase 55a implements only VT-d and AMD-Vi behind a narrow `DmaBuffer` contract.
- Mature kernels expose per-process IOMMU groups for VFIO device passthrough to guests; m3OS does not need that until it hosts VMs.
- Production systems carry significant quirk tables for vendor-specific DMAR bugs; Phase 55a ships the straight-path implementation and records quirks only as they are observed on the reference matrix.

## Known Open Bug — must close before Phase 58

- **VT-d MMIO translation drops driver `CTRL.RST` writes under `--iommu`.** Surfaced by Phase 55b's tighter `cargo xtask device-smoke --device {nvme,e1000} --iommu` assertions. The per-device domain setup does not install identity-mapped MMIO windows for each claimed device's BAR regions, so ring-3 drivers' MMIO resets are silently lost under active VT-d translation. Full diagnosis, reproduction, and acceptance criteria in [`docs/appendix/phase-55b-residuals.md`](../appendix/phase-55b-residuals.md) (item R2). **This must close before the Phase 58 1.0 gate ships its "IOMMU-isolated ring-3 drivers" claim.**

## Deferred Until Later

- VFIO / device passthrough for guest VMs.
- SR-IOV virtual function support.
- IOMMU group enforcement policies beyond per-device domains.
- ARM SMMU support (m3OS is x86_64-only).
- Dynamic IOVA space compaction and large-page promotion optimizations.
