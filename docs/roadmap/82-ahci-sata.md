# Phase 82 - AHCI / SATA Storage

**Status:** Complete ✅ (landed pre-1.0; kernel `0.82.0`)
**Source Ref:** phase-82
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 67 (IOMMU Substrate) ✅
**Builds on:** Adds AHCI-mode SATA storage support to the project's NVMe-only storage matrix. Optional for 1.0 — the Phase 74a §1 audit grades AHCI as HIGH (not BLOCKER) because most 2018+ systems are NVMe-only, but older systems and many enterprise deployments still depend on SATA
**Primary Components:** `userspace/drivers/ahci/` (new), `kernel-core/src/storage/` (host-testable AHCI command-table layout), `kernel/initrd/etc/services.d/ahci_driver.conf` (new)

> **As-built reconciliation (landing).** The shipped phase matches this design with two clarifications. (1) **Kernel changes**: the data-path change is the `blk::remote::is_registered()` cold-path lookup learning `"ahci.block"` (D.2, as planned). Making a SATA disk serve the **root** required one further small, no-regression kernel change the original design under-specified — `kernel/src/blk/mbr.rs::read_mbr()` now reads the sector-0 partition probe through the `blk::read_sectors` facade (which routes to a registered `ahci.block`/`nvme.block` driver when virtio-blk is not the root) instead of reading virtio-blk directly. (2) **Bootstrap**: the kernel mounts the root before any ring-3 driver exists, so `init` spawns `/drivers/ahci` from the ramdisk and retries the ext2 mount when the initial virtio-blk mount fails (gated on mount-failure, so the normal virtio boot is untouched). The IRQ syscall is `sys_device_irq_subscribe`; the xtask flag is `cargo xtask run --device ahci` and the QEMU device is `-device ich9-ahci` (`ahci` is the QEMU alias).

## Milestone Goal

m3OS finds and uses a SATA SSD or HDD attached to an AHCI-mode host controller. The driver is a ring-3 userspace process on top of the Phase 55b host primitives, registers as a `RemoteBlockDevice` (the same facade NVMe uses today), and lets the existing VFS/ext2 stack mount a SATA partition with a small scoped kernel change — the `blk::remote` cold-path service lookup learns the `"ahci.block"` name (and the `blk::mbr` probe reads through the block facade so the mount-time partition probe can reach a SATA root); everything else in the VFS path is unchanged.

## Why This Phase Exists

Phase 74a §3 grades AHCI as HIGH: NVMe-only systems work, but anything with a SATA boot drive does not. The dev laptop has NVMe, so this phase is explicitly *optional* for 1.0 — the Release Gate (Phase 83) can choose to defer it if the rest of the 77–81 sequence slips. Listing it as a numbered phase keeps the option open without forcing it.

## Learning Goals

- Understand how AHCI generalizes legacy IDE/PATA with a memory-mapped command structure
- See how AHCI's per-port command list + command tables differ from NVMe's SQ/CQ model conceptually but solve the same problem
- Learn how SATA's FIS (Frame Information Structure) protocol moves between the host and the drive
- Understand why AHCI is fundamentally lower-throughput than NVMe (one queue per port vs. NVMe's per-core queues)
- See how a second `RemoteBlockDevice` implementation drops in beside NVMe without disturbing the VFS layer

## Feature Scope

### Track A — AHCI host controller bring-up

- **A.1** — PCI probe for class code `0x010601` (SATA AHCI). Map ABAR (AHCI Base Address Register, BAR5) as CPU MMIO and enable Bus Master + Memory Space.
- **A.2** — HBA enable + reset: set `GHC.AE` (AHCI-enable) before any port-register access has AHCI semantics, perform a `GHC.HR` HBA reset bounded at 1 s, and re-read `CAP`/`PI`/`VS` after the reset self-clears and `AE` is re-asserted (the reset reloads them).
- **A.3** — BIOS/OS handoff (`CAP2.BOH` / `BOHC`): on firmware that still owns the HBA, take ownership via the BOHC handshake. Gated on `CAP2.BOH` — QEMU's `ich9-ahci` leaves `CAP2.BOH = 0`, so this is a no-op there and a bare-metal/VFIO-only path.
- **A.4** — Per-port command-list + received-FIS structures via `DmaBuffer<T>`, programming the device-visible **IOVA** (never host-physical) into `PxCLB`/`PxFB`. Port enumeration walks the implemented-ports bitfield (`PI`); each present port is detected via `PxSSTS.DET == 3`.
- **A.5** — Command-engine start/stop ordering: clear `PxCMD.ST` and confirm `PxCMD.CR == 0` before clearing `PxCMD.FRE`, and confirm `CR == 0` before re-setting `ST`. `CR`/`FR` are read-only status the HBA drives; reprogramming the command-list pointer while the engine runs corrupts it.
- **A.6** — Port PHY bring-up: COMRESET via `PxSCTL.DET = 1` → wait → `DET = 0` → poll `PxSSTS.DET == 3`; enable FIS receive (`PxCMD.FRE`) so `PxSIG` becomes valid; read `PxSIG` and check the device-type signature (`0x00000101` = SATA), driving only SATA ports.
- **A.7** — Free command-slot allocation over `PxSACT | PxCI` bounded by `CAP.NCS`, with the single-in-flight (no-NCQ) data-path engine issuing one command per port at 1.0.

### Track B — Command issue + completion + durability

- **B.1** — `IDENTIFY DEVICE` for capacity / model / firmware / LBA48 / FLUSH-capability discovery (the recommended first command, validating the whole issue/PRDT/completion path).
- **B.2** — `READ DMA EXT` (`0x25`) and `WRITE DMA EXT` (`0x35`) via the H2D Register FIS (type byte `0x27`, LBA48 `device = 1 << 6`), with the PRDT scatter-gather entry carrying the data IOVA and the N−1-encoded byte count.
- **B.3** — `FLUSH CACHE EXT` (`0xEA`, non-data) for write durability: a `WRITE DMA EXT` completion only means the data reached the drive's volatile cache, so a sync/barrier issues FLUSH CACHE EXT and reports a write durable only after it completes without error.
- **B.4** — IRQ on completion via the Phase 55b `sys_device_irq_subscribe`, with `PxIE` armed and `GHC.IE` enabled last, and the IRQ-clear order `PxIS` (W1C) then the global `IS` bit. The data path is **polling-primary** (`PxCI` auto-clears on completion), so the IRQ is a wakeup and the gate does not depend on IRQ delivery.

### Track C — Block-device facade + error recovery

- **C.1** — Register the AHCI driver as a `RemoteBlockDevice` per Phase 55b, serving the same `BlkRequestHeader`/`BlkReplyHeader` protocol NVMe uses and honoring the `MAX_SECTORS_PER_REQUEST` (256) chunk cap. The kernel-side VFS path is unchanged; the one kernel change is the `blk::remote` cold-path lookup learning the `"ahci.block"` service name.
- **C.2** — Boot-time disk probe: the partition table walker (already present for NVMe) discovers ext2 / FAT32 partitions and surfaces them to the VFS the same way.
- **C.3** — `PxIS.TFES` / `PxSERR` error recovery → engine restart: on a task-file error or command timeout, stop the engine, clear both write-1-to-clear latches (`PxSERR` then `PxIS`) before re-enabling, restart, and map a failed/timed-out command onto the `RemoteBlockDevice` restart semantics (`DriverRestarting` → bounded wait → retry-once) so the VFS retries transparently.

## Important Components and How They Work

### AHCI Port Command List

Each implemented port owns a 1 KiB command list (32 command headers × 32 bytes), pointing into a per-command command table (CFIS + PRDT scatter-gather entries). The host writes a command header, sets the corresponding bit in `PxCI`, and the controller issues the command. Completion is observable by polling `PxCI` (the slot bit auto-clears) or via an IRQ; the host reads `PxIS` and the received-FIS area to discover which command completed and how.

### FIS protocol

The SATA link carries FIS frames bidirectionally: H2D Register FIS (host → drive command, type byte `0x27`), D2H Register FIS (drive → host status, type byte `0x34`), DMA Setup FIS, PIO Setup FIS, Data FIS, etc. AHCI assembles these in DMA-visible memory and only signals the host on transitions worth surfacing. The H2D Register FIS is the single command channel — an LBA byte split error, a missing C-bit, or a wrong type byte yields a misaddressed or rejected command.

### Why no multi-queue at 1.0

AHCI defines exactly one command queue per port (with 32 slots). It cannot match NVMe's per-core scalability — that's intrinsic to the protocol. m3OS at 1.0 ships single-queue NVMe (Phase 55b decision) anyway, so AHCI is symmetric here; the slot allocator scans `PxSACT | PxCI` so the design is forward-compatible with NCQ, but the data path waits on a single in-flight slot.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 driver-host primitives unchanged.
- Reuses Phase 67's IOMMU `DmaBuffer<T>` for command-list / command-table / PRDT allocation, programming the device-visible IOVA into every HBA register.
- Reuses Phase 55b's NVMe driver pattern as a template — `RemoteBlockDevice` is unchanged; the one kernel addition is the `blk::remote::is_registered()` cold-path lookup learning `"ahci.block"`.
- Slots into the existing VFS partition walker (Phase 8 territory).

## Implementation Outline

1. Bring up the AHCI driver against QEMU's `-device ich9-ahci` + `-drive ... if=none` + `-device ide-hd` emulation.
2. Implement IDENTIFY DEVICE; print the disk model / firmware / size at boot.
3. Implement READ DMA EXT (single-block then multi-block).
4. Implement WRITE DMA EXT, then FLUSH CACHE EXT for write durability.
5. Register as `RemoteBlockDevice`; verify ext2 partition mount and read/write.
6. Validate on real hardware if a SATA-equipped test machine is available before Phase 83.
7. Bump kernel to `0.82.0` (if shipped pre-1.0) or defer to post-1.0.

## Acceptance Criteria

- `cargo xtask run --device ahci` boots m3OS with the data disk on AHCI instead of VirtIO-blk and the smoke run passes.
- Multi-block read / write throughput exceeds 50 MB/s on QEMU AHCI emulation (sanity check, not a perf target).
- A new `cargo xtask ahci-smoke` gate exercises the IDENTIFY + read + write + read-back-compare + IDENTIFY-after-write + induced-TFES error-recovery paths.
- A new `cargo xtask ahci-root-smoke` gate proves the headline end-to-end in CI: it routes the real ext2 data disk to `ich9-ahci` (the `--device ahci` topology) and asserts the full chain — virtio root absent → driver MBR/ext2 probe on the SATA disk → kernel owner-gate accepts `/drivers/ahci` and binds `ahci.block` → `init: / mounted (ext2 via ring-3 ahci.block)` → login prompt (so the root FS genuinely serves directory/file/ELF reads, not just a `mount()` that returned 0). This replaces the prior manual-only validation of the root-over-SATA path.
- The write path is durable: a `WRITE DMA EXT` is followed by `FLUSH CACHE EXT` and reported durable only after the flush completes without `PxIS.TFES`; the `ahci-smoke` gate asserts the FLUSH CACHE EXT step (`0xEA`, `PRDTL = 0`) completes successfully after a write.
- No regression in NVMe — both back-ends coexist; the disk-probe path discovers whichever is attached.
- If shipped pre-1.0: kernel bumped to `0.82.0`. If deferred: doc reflects the deferral and Phase 83's support matrix lists "NVMe only" explicitly.

## Companion Task List

- [Phase 82 Task List](./tasks/82-ahci-sata-tasks.md)
- [Phase 82 Learning Doc](../82-ahci-sata.md)

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
