# Phase 82 - AHCI / SATA Storage

**Status:** Planned (optional pre-1.0)
**Source Ref:** phase-82
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 67 (IOMMU Substrate) ✅
**Builds on:** Adds AHCI-mode SATA storage support to the project's NVMe-only storage matrix. Optional for 1.0 — the Phase 74a §1 audit grades AHCI as HIGH (not BLOCKER) because most 2018+ systems are NVMe-only, but older systems and many enterprise deployments still depend on SATA
**Primary Components:** `userspace/drivers/ahci/` (new), `kernel-core/src/storage/` (host-testable AHCI command-table layout), `kernel/initrd/etc/services.d/ahci.conf` (new)

## Milestone Goal

m3OS finds and uses a SATA SSD or HDD attached to an AHCI-mode host controller. The driver is a ring-3 userspace process on top of the Phase 55b host primitives, registers as a `RemoteBlockDevice` (the same facade NVMe uses today), and lets the existing VFS/ext2 stack mount a SATA partition with no kernel-side changes.

## Why This Phase Exists

Phase 74a §3 grades AHCI as HIGH: NVMe-only systems work, but anything with a SATA boot drive does not. The dev laptop has NVMe, so this phase is explicitly *optional* for 1.0 — the Release Gate (Phase 83) can choose to defer it if the rest of the 77–81 sequence slips. Listing it as a numbered phase keeps the option open without forcing it.

## Learning Goals

- Understand how AHCI generalizes legacy IDE/PATA with a memory-mapped command structure
- See how AHCI's per-port command list + command tables differ from NVMe's SQ/CQ model conceptually but solve the same problem
- Learn how SATA's FIS (Frame Information Structure) protocol moves between the host and the drive
- Understand why AHCI is fundamentally lower-throughput than NVMe (one queue per port vs. NVMe's per-core queues)
- See how a second `RemoteBlockDevice` implementation drops in beside NVMe without disturbing the VFS layer

## Feature Scope

### Track A — AHCI host controller

- **A.1** — PCI probe for class code `0x010601` (SATA AHCI). Map ABAR (AHCI Base Address Register).
- **A.2** — Per-port command-list + received-FIS structures via `DmaBuffer<T>`. Port enumeration walks the implemented-ports bitfield.

### Track B — Command issue + completion

- **B.1** — `READ DMA EXT` and `WRITE DMA EXT` via the H2D Register FIS.
- **B.2** — IDENTIFY DEVICE for capacity / model / firmware discovery.
- **B.3** — IRQ on completion via Phase 55b `sys_device_irq_bind`.

### Track C — Block-device facade

- **C.1** — Register the AHCI driver as a `RemoteBlockDevice` per Phase 55b. The kernel-side VFS path is unchanged.
- **C.2** — Boot-time disk probe: the partition table walker (already present for NVMe) discovers ext2 / FAT32 partitions and surfaces them to the VFS the same way.

## Important Components and How They Work

### AHCI Port Command List

Each implemented port owns a 1 KiB command list (32 command headers × 32 bytes), pointing into a per-command command table (CFIS + PRDT scatter-gather entries). The host writes a command header, sets the corresponding bit in `PxCI`, and the controller issues the command. Completion fires an IRQ; the host reads `PxIS` and the received-FIS area to discover which command completed and how.

### FIS protocol

The SATA link carries FIS frames bidirectionally: H2D Register FIS (host → drive command), D2H Register FIS (drive → host status), DMA Setup FIS, PIO Setup FIS, Data FIS, etc. AHCI assembles these in DMA-visible memory and only signals the host on transitions worth surfacing.

### Why no multi-queue at 1.0

AHCI defines exactly one command queue per port (with 32 slots). It cannot match NVMe's per-core scalability — that's intrinsic to the protocol. m3OS at 1.0 ships single-queue NVMe (Phase 55b decision) anyway, so AHCI is symmetric here.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives unchanged.
- Reuses Phase 67's IOMMU `DmaBuffer<T>` for command-list / command-table / PRDT allocation.
- Reuses Phase 55b's NVMe driver pattern as a template — `RemoteBlockDevice` is unchanged.
- Slots into the existing VFS partition walker (Phase 8 territory).

## Implementation Outline

1. Bring up the AHCI driver against QEMU's `-device ahci -drive ...` emulation.
2. Implement IDENTIFY DEVICE; print the disk model / firmware / size at boot.
3. Implement READ DMA EXT (single-block then multi-block).
4. Implement WRITE DMA EXT.
5. Register as `RemoteBlockDevice`; verify ext2 partition mount and read/write.
6. Validate on real hardware if a SATA-equipped test machine is available before Phase 83.
7. Bump kernel to `0.82.0` (if shipped pre-1.0) or defer to post-1.0.

## Acceptance Criteria

- `cargo xtask run --ahci` boots m3OS with the data disk on AHCI instead of VirtIO-blk and the smoke run passes.
- Multi-block read / write throughput exceeds 50 MB/s on QEMU AHCI emulation (sanity check, not a perf target).
- A new `cargo xtask ahci-smoke` gate exercises the IDENTIFY + read + write + IDENTIFY-after-write paths.
- No regression in NVMe — both back-ends coexist; the disk-probe path discovers whichever is attached.
- If shipped pre-1.0: kernel bumped to `0.82.0`. If deferred: doc reflects the deferral and Phase 83's support matrix lists "NVMe only" explicitly.

## Companion Task List

- [Phase 82 Task List](./tasks/82-ahci-sata-tasks.md) — to be authored when implementation planning begins.

## How Real OS Implementations Differ

- Linux's `libahci` + `ahci_platform` framework handles dozens of vendor-specific AHCI variants (Marvell, ASMedia, JMicron, Silicon Image — each with quirks).
- Real OSes implement Native Command Queueing (NCQ) to overlap multiple outstanding commands on the same port; m3OS at 1.0 issues one command at a time per port.
- TRIM / DEALLOCATE / SECURE ERASE / SMART are all part of the SATA spec but deferred.
- Port multipliers (one AHCI port driving multiple drives via a SATA fanout) — deferred.
- Hot-plug detection (PRESENCE_DETECTED) — deferred.

## Deferred Until Later

- Native Command Queueing (NCQ)
- TRIM / DEALLOCATE
- SMART / health monitoring
- SECURE ERASE
- Port multipliers
- Hot-plug
- AHCI in IDE-emulation compatibility mode (firmware setting most modern boards no longer expose)
