# IOMMU Substrate

**Aligned Roadmap Phase:** Phase 55a
**Status:** Complete
**Source Ref:** phase-55a
**Supersedes Legacy Doc:** (none — new content)

## Overview

Phase 55a closes the DMA-isolation gap that Phase 55 deliberately deferred.
Where Phase 55 delivered the hardware-access layer (`BarMapping`,
`DmaBuffer<T>`, `DeviceIrq`) on top of a flat physical-to-bus identity, Phase
55a routes every `DmaBuffer` through a per-device translation domain installed
in real IOMMU hardware — Intel VT-d or AMD-Vi — so a device that issues a
malformed or hostile bus-master write cannot reach kernel memory the driver
never allocated.

The phase parses the ACPI DMAR (Intel) and IVRS (AMD) tables, builds a
vendor-neutral `IommuUnit` trait implementation for each unit present on the
platform, and wires every `claim_pci_device` through a freshly-created
`DmaDomain`. When no IOMMU is declared — the default `cargo xtask run`
configuration, or any platform whose firmware simply omits DMAR/IVRS — the
subsystem logs a single structured `iommu.fallback.identity` event and
installs an `IdentityDomain` so the Phase 55 drivers keep working, but the
fallback is always *observable* rather than silent.

The authoritative design doc is
[docs/roadmap/55a-iommu-substrate.md](./roadmap/55a-iommu-substrate.md); this
learner-facing doc cross-references it rather than duplicating the normative
content.

## What This Doc Covers

- Why an OS needs **both** an MMU (for CPU-side translation) and an IOMMU
  (for device-side translation), and what each protects against.
- What the **ACPI DMAR and IVRS** tables describe, which vendor owns each,
  and how the OS uses them to discover IOMMU hardware without probing.
- How **VT-d's second-level page table** and **AMD-Vi's host page table**
  differ in shape but serve the same role — per-device IOVA-to-physical
  translation — and why Phase 55a presents them through a single trait.
- The **identity-mapped vs translated domain** design choice, and why Phase
  55a picks translated for every claimed device.
- Why **reserved-region handling** (RMRR for VT-d, unity-map for AMD-Vi) is
  not optional — skipping it hangs the affected device.
- The subsystem's **lock ordering** between domain, unit, and the Phase 53a
  buddy allocator.
- The **per-domain resource caps** (IOVA high-water mark, page-table-page
  count) that keep a misbehaving driver from exhausting the system.

## Core Implementation

### MMU vs IOMMU — two sides of the same translation coin

A modern x86_64 system has two independent translation hardware units:

1. **The CPU MMU.** Translates virtual addresses issued by the CPU into
   physical frames via per-process page tables. Enforces process isolation:
   process A cannot read or write process B's memory because A's page table
   does not map B's frames.
2. **The IOMMU.** Translates bus addresses issued by *devices* into
   physical frames via per-device translation domains. Enforces *device*
   isolation: device A cannot DMA into process B's memory (or the kernel's
   private memory) because device A's domain does not map the frames B
   owns.

Without the IOMMU, any PCIe device with bus-master capability can write to
any physical address the platform exposes. This is the compromise every
ring-0 driver silently accepts under "no IOMMU" — the driver is trusted not
to program malformed descriptors, and firmware is trusted not to lie about
memory layout. As soon as drivers move to ring 3 (Phase 55b's goal), that
compromise becomes a regression: a userspace driver with raw DMA authority
can reach arbitrary kernel memory through the device. IOMMU translation
closes that gap, and Phase 55a ships the substrate the ring-3 driver host
needs before Phase 55b makes the move.

The two units are wired independently: the CPU does not know the IOMMU is
there, the IOMMU does not know which process is current. Each is a
translation stage that its own software configures. They do, however, share
the same physical-frame backing store — `DmaBuffer::allocate` pulls
contiguous frames from the Phase 53a buddy allocator, zero-initializes
them, and installs the IOVA→phys mapping in the device's domain. The CPU
side of the mapping is the usual kernel-virtual alias set up during heap
initialization.

### ACPI DMAR and IVRS — how the OS finds the IOMMU hardware

The firmware tells the OS which IOMMU units exist and which PCI devices
each one covers via ACPI:

- **DMAR (DMA Remapping)** — Intel VT-d. Contains zero or more DRHD
  entries (DMA Remapping Hardware Definition, one per IOMMU unit), plus
  reserved-memory region records (RMRR), ATSR (ATS root ports), and RHSA
  (NUMA affinity). Each DRHD carries the unit's register base address and
  a list of device scope entries naming the BDFs the unit owns.
- **IVRS (I/O Virtualization Reporting Structure)** — AMD-Vi. Contains
  IVHD (I/O Virtualization Hardware Definition) blocks — types 10h, 11h,
  and 40h — plus IVMD unity-map region records. Each IVHD names a unit
  and a set of device entries (ranges, aliases, and special-device tags)
  describing the BDFs the unit owns.

The pure-logic decoders in
[`kernel-core/src/iommu/tables.rs`](../kernel-core/src/iommu/tables.rs)
consume raw byte slices and produce typed
`DmaRemappingUnit` / `IvhdBlock` records with named error enums
(`DmarParseError`, `IvrsParseError`). They live in `kernel-core` so every
decoder variant can be host-tested against synthesized blobs —
including a `proptest`-based corruption suite that feeds arbitrary bytes
and proves the decoders never panic, never loop, and never allocate
proportional to input length.

The kernel-side glue in [`kernel/src/acpi/mod.rs`](../kernel/src/acpi/mod.rs)
is thin: it locates the DMAR or IVRS signature through the existing XSDT
walk, validates the checksum, and hands the table body to the pure-logic
decoder. If both tables are present (a malformed multi-vendor platform),
DMAR wins and IVRS is logged-and-ignored rather than panicked on. If
neither is present, the unit list is empty and the identity fallback
engages.

A second pure-logic helper,
[`kernel-core::iommu::device_map`](../kernel-core/src/iommu/device_map.rs),
compiles the decoded scopes into a `DeviceToUnitMap` — an O(log N)
lookup from `(segment, bus, device, function)` to the owning unit
index, used by `claim_pci_device` to pick the right unit when requesting
a new domain.

### Translated domains over identity-mapped domains

Every IOMMU implementation supports two basic domain shapes:

- **Identity domain.** Every IOVA equals its physical frame. Cheapest to
  set up, lowest protection — the IOMMU is effectively a pass-through.
- **Translated domain.** Each device sees its own IOVA space; the page
  table walks translate IOVAs into arbitrary physical frames. Highest
  protection — a malformed descriptor points to an IOVA that either is
  not mapped (a fault) or points to a frame the driver legitimately
  allocated (a correct DMA).

Phase 55a picks **translated domains for every claimed device**. The
reasoning is the one that justifies the phase's existence: ring-3 drivers
(Phase 55b) cannot be trusted not to issue malformed descriptors, and a
translated domain is the only shape where a malformed descriptor is a
fault rather than kernel-memory corruption.

The identity fallback is retained as `IdentityDomain` for the
no-IOMMU-declared boot path (plain `cargo xtask run`), but the name is
explicit: boot emits exactly one `iommu.fallback.identity` log event
naming the reason (`no_dmar_or_ivrs`, `vtd_init_failed`,
`amdvi_init_failed`), and the `iommu::active()` accessor surfaces the
state to meminfo / diagnostic output so an operator cannot forget the
IOMMU is off.

### VT-d and AMD-Vi page-table shapes — different spelling, same sentence

Both vendors implement translated domains via multi-level page tables,
but the table shapes are distinct:

- **VT-d** uses a **second-level page table** modeled on x86_64
  supervisor page tables: PML4 / PDPT / PD / PT, with optional 2 MiB and
  1 GiB large-page entries. A per-device context-table entry points to
  the domain's PML4 root; a per-bus root-table entry points to the bus's
  context table. The full walk is: `RTADDR → root[bus] →
  context[dev:fn] → PML4 → PDPT → PD → PT`.
- **AMD-Vi** uses a **host page-table** also organized as a multi-level
  hierarchy with 4 KiB / 2 MiB / 1 GiB leaves. The lookup shape is
  different: a per-BDF device-table entry (indexed by the 16-bit BDF
  directly) points to the domain's page-table root. The full walk is:
  `DeviceTable[bdf] → PT root → ...`.

The bit layouts for each vendor's entries live in pure logic alongside the
decoders:
[`kernel-core/src/iommu/vtd_page_table.rs`](../kernel-core/src/iommu/vtd_page_table.rs),
[`kernel-core/src/iommu/vtd_regs.rs`](../kernel-core/src/iommu/vtd_regs.rs),
[`kernel-core/src/iommu/amdvi_page_table.rs`](../kernel-core/src/iommu/amdvi_page_table.rs),
and
[`kernel-core/src/iommu/amdvi_regs.rs`](../kernel-core/src/iommu/amdvi_regs.rs).
Each is encode/decode round-trip tested with `proptest` so the kernel
never declares a second copy of the same bit layout.

The vendor-agnostic surface is the `IommuUnit` trait in
[`kernel-core/src/iommu/contract.rs`](../kernel-core/src/iommu/contract.rs).
VT-d's `VtdUnit` (in [`kernel/src/iommu/intel.rs`](../kernel/src/iommu/intel.rs))
and AMD-Vi's `AmdViUnit` (in [`kernel/src/iommu/amd.rs`](../kernel/src/iommu/amd.rs))
each implement the same trait methods — `bring_up`, `create_domain`,
`destroy_domain`, `map`, `unmap`, `flush`, `install_fault_handler`,
`capabilities` — and each passes the shared contract suite in
`kernel-core/tests/iommu_contract.rs`. Drivers consume the trait; they
never see vendor-specific page-table shape.

### Reserved regions — RMRR and unity-map

Firmware often holds a few physical regions that a device *must* still
reach after the IOMMU is enabled. The most common examples: the GPU
framebuffer the firmware draws to during POST, ACPI reclaim memory, and
the EFI runtime services region. If the driver's domain does not identity-
map these ranges, the device hangs on the first DMA cycle after translation
turns on — firmware is still driving that region and has no idea the OS
switched the IOMMU into translated mode.

DMAR records these as **RMRR** (Reserved Memory Region Reporting) entries,
naming the start/end and the device scope they apply to. IVRS records them
as **IVMD** unity-map regions with the same intent. Both vendor paths feed
into the shared
[`ReservedRegionSet`](../kernel-core/src/iommu/regions.rs) algebra:
`union`, `merge_overlapping`, `contains`. At domain creation time,
`DmaDomain::pre_map_reserved` walks the set once, installs an identity
mapping for each region in the new domain's page tables, and records the
IOVA as pre-reserved in the domain's `IovaAllocator` so driver allocations
never collide with it.

This is the shared helper both vendors call. Getting it right once is the
DRY hook that keeps VT-d and AMD-Vi from growing parallel bugs in firmware-
region handling.

### Lock ordering

The IOMMU subsystem's lock ordering is authoritative and mirrored in
[`kernel/src/iommu/mod.rs`](../kernel/src/iommu/mod.rs) and
[`kernel-core/src/iommu/contract.rs`](../kernel-core/src/iommu/contract.rs):

```text
domain lock  →  unit lock  →  buddy-allocator lock
```

The rules:

- A caller that already holds a domain lock may acquire the owning unit's
  lock, which in turn may acquire the Phase 53a buddy allocator's lock.
  No reverse nesting is permitted.
- **Driver-side locks never nest IOMMU-unit locks.** A driver that holds
  its own device lock must release it before invoking any `IommuUnit`
  method that takes the unit lock. If the driver needs to coordinate the
  two, the domain lock is the outer one.
- **IOMMU-unit locks never nest buddy-allocator locks held by callers.** A
  caller that already holds the buddy-allocator lock must release it
  before invoking `IommuUnit::map` or any other method that itself may
  take the allocator.
- **Fault handlers run in IRQ context** and must not take any lock that a
  non-IRQ path could hold for longer than bounded work. Drain the fault
  ring, log via the lock-free / spin-based structured logger in
  [`kernel/src/iommu/fault.rs`](../kernel/src/iommu/fault.rs), then
  return.

In `cfg!(debug_assertions)` builds these rules are enforced by lock-
ordering assertions embedded in the subsystem; the release path relies on
the ordering being statically obvious from call sites.

### Per-domain resource caps

Each domain carries two named resource bounds:

- **IOVA high-water mark.** An allocation that would cross this limit
  returns `IovaError::Exhausted`. The default IOVA window is the full
  48-bit address space minus the reserved-region set; a later phase may
  lower the mark for devices that do not need the full range.
- **Page-table pages cap.** Each domain is allowed a finite number of
  page-table page allocations from the buddy allocator. Exceeding the cap
  returns `DomainError::PageTablePagesCapExceeded` without corrupting the
  domain. The default is sized so every reference matrix device —
  NVMe, e1000, VirtIO-blk, VirtIO-net — fits with headroom; drivers that
  allocate unusually many 4 KiB mappings may need the cap bumped.

Both caps are expressed as constants at the top of each vendor module so a
future phase can revise them without hunting through call sites. Fault-
record and event-log ring sizes are similarly capped — overflow increments
a documented counter and drops the oldest record, and the overflow is
logged once per fault-storm window.

### Why these caps matter

The caps are a safety net, not an optimization. A driver (particularly a
future ring-3 driver in Phase 55b) that allocates IOVA or requests
mappings in a tight loop without bound would otherwise drag the entire
buddy allocator into page-table-page growth and deprive legitimate
callers of memory. Named caps per domain let the kernel fail the offending
driver with a typed error and keep running.

## Key Files

| File | Purpose |
|---|---|
| [`kernel/src/iommu/mod.rs`](../kernel/src/iommu/mod.rs) | Boot-time wiring: calls the pure-logic builders, caches unit descriptors and reserved-region set, exposes the public accessors (`iommu_units_from_acpi`, `device_to_unit`, `reserved_regions`) |
| [`kernel/src/iommu/intel.rs`](../kernel/src/iommu/intel.rs) | VT-d `IommuUnit` implementation: register MMIO, root/context/page tables, translation enable, queued invalidation, fault IRQ |
| [`kernel/src/iommu/amd.rs`](../kernel/src/iommu/amd.rs) | AMD-Vi `IommuUnit` implementation: device table, command/event buffers, page-table walker, translation enable, event-log IRQ |
| [`kernel/src/iommu/fault.rs`](../kernel/src/iommu/fault.rs) | Shared structured fault-event logger used by both vendor IRQ handlers |
| [`kernel-core/src/iommu/mod.rs`](../kernel-core/src/iommu/mod.rs) | Pure-logic foundation module root; declares the submodules below |
| [`kernel-core/src/iommu/tables.rs`](../kernel-core/src/iommu/tables.rs) | DMAR (Intel) and IVRS (AMD) ACPI table decoders; `DmarParseError` / `IvrsParseError` |
| [`kernel-core/src/iommu/contract.rs`](../kernel-core/src/iommu/contract.rs) | `IommuUnit` trait, `DmaDomain`, `DomainId`, `Iova`, `MapFlags`, `IommuError`, `DomainError`, `IommuCapabilities` |
| [`kernel-core/src/iommu/iova.rs`](../kernel-core/src/iommu/iova.rs) | `IovaAllocator` — bump + freelist IOVA space allocator with typed `IovaError` |
| [`kernel-core/src/iommu/regions.rs`](../kernel-core/src/iommu/regions.rs) | `ReservedRegion`, `ReservedRegionSet`, `union`, `merge_overlapping`, `contains` |
| [`kernel-core/src/iommu/device_map.rs`](../kernel-core/src/iommu/device_map.rs) | `DeviceToUnitMap` — O(log N) BDF → unit-index lookup |
| [`kernel-core/src/iommu/acpi_integration.rs`](../kernel-core/src/iommu/acpi_integration.rs) | `iommu_units_from_dmar` / `iommu_units_from_ivrs` / `reserved_regions_from_tables` ACPI-to-descriptor builders |
| [`kernel-core/src/iommu/vtd_page_table.rs`](../kernel-core/src/iommu/vtd_page_table.rs), [`vtd_regs.rs`](../kernel-core/src/iommu/vtd_regs.rs) | VT-d page-table-entry and register bit layouts (host-testable, round-trip tested) |
| [`kernel-core/src/iommu/amdvi_page_table.rs`](../kernel-core/src/iommu/amdvi_page_table.rs), [`amdvi_regs.rs`](../kernel-core/src/iommu/amdvi_regs.rs) | AMD-Vi page-table-entry and register bit layouts (host-testable, round-trip tested) |
| [`kernel/src/mm/dma.rs`](../kernel/src/mm/dma.rs) | `DmaBuffer::allocate(device, size)` — IOMMU-aware DMA allocator; `bus_address()` returns IOVA when IOMMU active, physical frame address under identity fallback |
| [`kernel/src/pci/mod.rs`](../kernel/src/pci/mod.rs) | `claim_pci_device` with per-device domain lifecycle; `PciDeviceHandle` drop tears the domain down |

## How This Phase Differs From Later Memory Work

- **Phase 55b (Ring-3 Driver Host)** will add a `sys_device_dma_alloc`
  syscall that wraps `DmaBuffer::allocate` behind a device capability.
  The device-keyed contract, per-device domain lifetime, and identity-
  fallback path delivered here are the primitives 55b's capability layer
  will wrap. Any change to the `DmaBuffer` signature or the
  `PciDeviceHandle` lifetime after this phase forces re-work in 55b, so
  the Track A / E contracts are treated as stable once Phase 55a closes.
- **Phase 56 (Display and Input Architecture)** assumes per-device
  isolation for its multi-client display service — one graphical
  client's device-visible buffers cannot reach another client's memory
  via bus-master DMA. Phase 55a provides that isolation by construction
  through translated domains. The Phase 55 Reference Hardware Matrix
  gains a `-device intel-iommu` row here, which Phase 56's validation
  configurations can reference without adding IOMMU setup to its own
  track plan.
- **Phase 53a (Kernel Memory Modernization)** supplied the buddy
  allocator that backs both the physical DMA frames and the page-table
  pages used by the IOMMU. Phase 55a consumes `alloc_contiguous_frames`
  unchanged — no allocator work was required.

## Related Roadmap Docs

- [Phase 55a roadmap doc](./roadmap/55a-iommu-substrate.md)
- [Phase 55a task doc](./roadmap/tasks/55a-iommu-substrate-tasks.md)
- [Phase 55 — Hardware Substrate](./55-hardware-substrate.md) (the
  substrate Phase 55a extends)
- [Phase 15 — Hardware Discovery](./15-hardware-discovery.md) (the ACPI
  parser Phase 55a extends with DMAR/IVRS)
- [Phase 55b — Ring-3 Driver Host](./roadmap/55b-ring-3-driver-host.md)
  (the downstream phase this substrate unblocks)

## Deferred or Later-Phase Topics

- **ARM SMMU.** m3OS is x86_64-only; a later phase that adds ARM
  support would implement a third `IommuUnit` impl and extend the
  contract suite.
- **SR-IOV virtual functions.** Per-VF domain handling is deferred until
  a driver that exercises VFs exists on the reference matrix.
- **VFIO / device passthrough to guest VMs.** Out of scope until m3OS
  hosts VMs.
- **IOMMU groups beyond per-device domains.** Phase 55a uses a
  one-domain-per-BDF shape; sharing a domain across a group is a later
  phase's concern.
- **Interrupt remapping.** Intentionally disabled in Phase 55a; re-
  introducing it will need an IDT-integration design.
- **VT-d scalable mode.** Second-level translation only; scalable-mode
  (first-level + second-level) is deferred.
- **Dynamic IOVA-space compaction and large-page promotion
  optimizations.** The bump + freelist `IovaAllocator` is sufficient
  for the reference matrix; a future phase may coalesce freed ranges
  if workloads demand it.
