# Phase 82 — AHCI / SATA Storage (Learning Doc)

**Status:** Complete
**Source Ref:** phase-82
**Depends on:** Phase 55b (Ring-3 Driver Hosting), Phase 67 (IOMMU Substrate), Phase 8 (VFS + MBR partition walker), Phase 74 (IPC Capability Grants)
**Builds on:** the NVMe-only ring-3 storage matrix — adds an AHCI-mode SATA block driver as a second `RemoteBlockDevice` behind the **same** block IPC protocol NVMe already speaks, so a SATA disk drops in beside NVMe with one scoped kernel change.
**Primary Components:** `kernel-core/src/storage/{ahci,ata}.rs` (host-tested register/struct/FIS/PRDT/slot/classifier logic), `userspace/drivers/ahci/{lib,init,port,cmd,io,main}.rs` (the ring-3 driver — `lib.rs` is the host-testable `os-binary`/lib split), `kernel/src/blk/remote.rs` (cold-path `ahci.block` lookup) + `kernel/src/blk/mbr.rs` (facade-aware MBR probe), `userspace/init/src/main.rs` (SATA-root bootstrap), the `ahci-smoke` gate.

## Milestone Goal

m3OS finds and uses a SATA SSD/HDD attached to an AHCI-mode host bus adapter (HBA). The driver is a ring-3 userspace process on the Phase 55b device-host substrate, registers as a `RemoteBlockDevice` (the same facade NVMe uses), and lets the existing VFS/ext2 stack mount a SATA partition — `cargo xtask run --device ahci` boots m3OS with `/` mounted off SATA through `ahci.block`, and the `ahci-smoke` gate proves IDENTIFY → write → read-back-compare → flush → IDENTIFY-after-write end-to-end against QEMU's `ich9-ahci`.

## Why This Phase Exists

NVMe-only systems work, but anything with a SATA boot drive did not. AHCI is the storage silicon on essentially every x86 board from ~2005 to the NVMe transition, and many enterprise deployments still ship SATA. Listing AHCI as a numbered phase keeps a 1.0 with real SATA support on the table without forcing it. The phase is also a clean second instance of the Phase 55b `RemoteBlockDevice` contract: it proves the block facade really is device-agnostic — a second back-end joins it with **one** scoped kernel change (the `blk::remote` cold-path service lookup learning `"ahci.block"`), exactly as Phase 81's NIC work added `default_route_index_by_link`.

## Learning Goals

- How **AHCI generalizes legacy IDE/PATA**: instead of programmed-I/O task-file registers, each port owns a memory-mapped **command list** of 32 command headers, each pointing at a **command table** (the command FIS + a scatter-gather PRDT). The host writes a slot and sets one bit in `PxCI`; the controller bus-masters the whole command.
- How SATA's **FIS (Frame Information Structure)** protocol moves between host and drive — the H2D Register FIS (type byte `0x27`) is the single command channel; the D2H Register FIS (`0x34`) lands in the per-port received-FIS area as status.
- Why a ring-3 AHCI driver under an IOMMU programs **IOVA** (from its own `DmaBuffer<T>`) into every base register and PRDT — the single biggest departure from the Redox `ahcid` reference, which writes host-physical addresses.
- Why **FLUSH CACHE EXT** is mandatory for durability, and why omitting it (as Redox `ahcid` does) is a silent data-loss bug.
- The **engine stop/start ordering invariant** (`ST`→`CR`, `FRE`→`FR`) and why reprogramming the command-list pointer while the engine runs corrupts it.
- Why AHCI is intrinsically lower-throughput than NVMe (one command queue per port vs. NVMe's per-core queues) and how m3OS's single-in-flight data path mirrors the single-queue NVMe decision.
- The bootstrap reality of a **ring-3 root block driver**: the kernel mounts the root before any ring-3 driver exists, so a SATA root needs init to spawn the driver and retry the mount.

## Feature Scope

### AHCI generalizes legacy IDE: the per-port command list

Legacy IDE/PATA drove a drive through a handful of programmed-I/O task-file registers (LBA, count, command, status) and moved data a word at a time or via a single bus-master PRD. AHCI replaces that with a **memory-resident structure the controller DMA-reads**:

```
Port x  ──PxCLB──►  Command List (1 KiB = 32 × HbaCmdHeader[32 B])
                       slot N ──ctba──►  Command Table
                                          ├─ cfis[64]   (the H2D Register FIS, 20 B used)
                                          ├─ acmd[16]   (ATAPI command — unused for SATA)
                                          ├─ _rsv[48]
                                          └─ prdt[]      (scatter-gather, begins at offset 0x80)
Port x  ──PxFB───►  Received-FIS area (256 B: DSFIS / PSFIS / RFIS@0x40 / SDBFIS / ...)
```

The host fills a command header + table, sets the slot's bit in `PxCI`, and the controller issues the command, DMAs the data through the PRDT, writes a D2H Register FIS into the received-FIS area, and **auto-clears the `PxCI` bit** on completion. Completion is observed by polling `PxCI` or via an interrupt. This is the conceptual cousin of NVMe's SQ/CQ rings — a different shape, same idea: command descriptors in host memory, a doorbell, DMA, a completion the host reaps.

These layouts are pinned, host-tested, in `kernel-core/src/storage/ahci.rs` (the `HbaCmdHeader`/`HbaCmdTable`/`HbaPrdtEntry`/`FisRegH2D`/`HbaFis` `#[repr(C)]` structs with compile-time `size_of` **and** `offset_of` asserts) and `ata.rs` (the FIS encoders + IDENTIFY parser), exactly as `kernel_core::nvme` hosts the NVMe formats and `kernel_core::hda` hosts the HDA verb/format math.

### The FIS protocol

The SATA link carries FIS frames bidirectionally. The two this driver cares about:

- **H2D Register FIS** (`fis_type = 0x27`) — the single command channel host→drive. It carries the ATA opcode, the 48-bit LBA (split across `lba0..lba5` interleaved around the `device` byte at offsets 4–10), the sector count, and the **C-bit** (`pm_c & 0x80`) that marks this as a command (not a control) update. QEMU's `ich9-ahci` and every real HBA validate the `0x27` type byte — a wrong/zero type makes the HBA reject the command — so `kernel_core::storage::ata` hard-wires it in every encoder (`encode_rw_fis` / `encode_identify_fis` / `encode_flush_fis`).
- **D2H Register FIS** (`fis_type = 0x34`) — drive→host status, DMA-written into the received-FIS area at offset `0x40` (`HbaFis::rfis`). It carries the ATA status/error bytes (same `BSY`/`DRQ`/`ERR` bit positions as `PxTFD`).

`READ DMA EXT` (`0x25`) and `WRITE DMA EXT` (`0x35`) are the 48-bit-LBA variants, so the FIS sets `device = 1 << 6` (LBA mode). An LBA byte-split error or a missing C-bit yields a misaddressed transfer or a control update the drive silently ignores — which is why the split is done once, in a host-tested function (`tests::rw_fis_lba48_split` pins `encode_rw_fis(false, 0x01_0203_0405, 8)` → `lba0..lba5 == [0x05,0x04,0x03,0x02,0x01,0x00]`).

### The command-list / command-table / PRDT layout — the silent-corruption trap

DW0 of `HbaCmdHeader` is a full 32-bit dword: **byte 0** (CFL/A/W/P), **byte 1** (R/B/C/PMP), then **PRDTL at byte offset 2**. The trap: if you model the header as `byte0` + `prdtl: u16` and omit byte 1, `prdtl` lands at offset 1, and the HBA reads the PRDT length from the wrong bytes — a corruption that a `size_of::<HbaCmdHeader>() == 32` assert does **not** catch, because the trailing reserved padding absorbs the one-byte shift. So `kernel_core::storage::ahci` pins the layout with `offset_of!` asserts (`prdtl == 2`, `prdbc == 4`, `ctba == 8`, `ctbau == 12`), not size alone. The same discipline pins `FisRegH2D` (`lba0 == 4`, `device == 7`, `lba3 == 8`) and the command-table PRDT offset (`0x80`).

Each PRDT entry carries the data buffer's IOVA (`DBA`/`DBAU`) and a **`DBC` (Data Byte Count) field with the N−1 encoding**: the low 22 bits hold `byte_count - 1` (so the low bit is always set, since transfer lengths are even), and bit 31 is interrupt-on-completion. `encode_dbc(byte_count, interrupt)` implements it and `debug_assert`-guards a zero-length entry (`tests::prdt_dbc_n_minus_1`).

### IOVA, never host-physical — the #1 first-driver bug

`PxCLB`/`PxCLBU`, `PxFB`/`PxFBU`, each command header's `ctba`/`ctbau`, and every PRDT `DBA`/`DBAU` are **device DMA addresses the HBA dereferences**. Under VT-d / AMD-Vi they must be the `DmaBuffer::iova()` — the device-visible IOVA the kernel installed in the driver's IOMMU domain — **never** the driver's user virtual address. The Redox `ahcid` reference writes `Dma::physical()` because it runs under a flat physical model; m3OS substitutes the IOMMU IOVA exactly as the Phase 80 HDA CORB/RIRB (`CORBLBASE == dma.iova()`) and the Phase 81 mt792x descriptor tracks do. `port.rs::program_dma_structures` and `cmd.rs::prepare_command` each assert the programmed value equals the IOVA and not the VA (`debug_assert_ne!(clb, user_ptr())`).

### Engine stop/start ordering — a hard invariant

`PxCMD.CR` (Command List Running) and `PxCMD.FR` (FIS Receive Running) are **read-only status bits the HBA drives**; `ST` and `FRE` are the software controls. The cardinal rule, honored identically on bring-up and recovery:

1. Clear `PxCMD.ST`, then **wait for `CR == 0`**.
2. Clear `PxCMD.FRE`, then **wait for `FR == 0`**.
3. Reprogram `PxCLB`/`PxFB` only while the engine is stopped.
4. Confirm `CR == 0` before re-setting `ST`.

Reprogramming the command-list pointer while `CR == 1` is undefined and corrupts it. The `engine_stopped(cmd)` predicate (`CR` and `FR` both clear) is host-tested (`tests::engine_stop_ordering`); `port.rs::stop_engine` / `start_engine` enforce the ordering with bounded polls.

`PxSERR` and `PxIS` are **write-1-to-clear** and must both be cleared before the engine restarts, or a stale latched bit immediately re-interrupts. The struct/bit names encode the W1C semantics so call sites cannot mistreat them.

### FLUSH CACHE EXT durability — Redox omits it, which is a data-loss bug

A `WRITE DMA EXT` completion only guarantees the data reached the drive's **volatile write cache**, not the platters. The Redox `ahcid` source has no `FLUSH CACHE` and so can lose a "successful" write on power loss. m3OS issues `FLUSH CACHE EXT` (`0xEA`, non-data, `PRDTL = 0`, C-bit) after every write and reports the write durable only once the flush completes without `PxIS.TFES` (`cmd.rs::flush`, called from `handle_write`). On QEMU this maps to a host `blk_aio_flush()`; true media persistence then depends on the host `-drive cache=` mode, so a strict durability run uses `cache=writethrough`/`directsync`.

### Error recovery → engine restart, mapped onto the RemoteBlockDevice restart path

On a task-file error the HBA latches `PxIS.TFES`, halts the engine, and leaves the failing slot's `PxCI` bit set. `is_fatal(is)` (`TFES|HBFS|HBDS|IFS`) is host-tested; `port.rs::recover_port` captures `PxTFD`/`PxSERR`, stops the engine, clears both W1C latches (`PxSERR` then `PxIS`), COMRESETs on an interface error, and restarts — the same ordering invariant as bring-up. A failed or timed-out command surfaces a `BlockDriverError::IoError` to the facade, which already has the `DriverRestarting` → bounded-wait → retry-once machinery (`BlockDispatchState`) that NVMe uses, so the VFS retries transparently.

### AHCI interrupt-clear order: PxIS first, then global IS — and why polling is primary

Per AHCI 1.3.1, `handle_irq` clears the port's `PxIS` (W1C) **then** writes 1 to the dispatched port's bit in the HBA-global `IS` register (offset `0x08`). Reversing this latches the global interrupt-pending bit and, on a level-triggered/INTx path, the line never deasserts and the bare-metal IRQ path wedges. `GHC.IE` is enabled **last**, after every `PxIE` mask is set and all stale W1C status is cleared, or the controller delivers a spurious interrupt immediately. These clear values are host-tested (`tests::pxis_clear`, `host_is_clear`, `is_decode`).

On QEMU the completion path is **polling `PxCI`** (the bit auto-clears on non-NCQ completion), so the data path never depends on IRQ delivery and the `ahci-smoke` gate does not couple to IRQ routing. The IRQ arm/handle plumbing (`io.rs`) is therefore the bare-metal/VFIO path; Phase 79 found the device-host IRQ allocator forces INTx for Ethernet-class, so any storage-class INTx fix is recorded as hardware-only and is out of the polling-primary 1.0 data path.

### Single queue per port (no NCQ) — symmetry with single-queue NVMe

AHCI defines exactly one command queue per port (32 slots). It cannot match NVMe's per-core scalability — that's intrinsic to the protocol. m3OS at 1.0 ships single-queue NVMe anyway (Phase 55b decision), so AHCI is symmetric: the slot allocator (`find_free_slot`) scans `PxSACT | PxCI` so the design is forward-compatible with NCQ, but the data-path engine (`cmd.rs::run_command`) waits on a single in-flight slot. With one command in flight, `find_free_slot` always returns slot 0, so the driver allocates one command table + one bounce buffer per port.

### QEMU `ich9-ahci` vs. bare-metal reality

The CI tier runs against `-device ich9-ahci` + `ide-hd` (PCI class `0x010601`, VID:DID `8086:2922`, `CAP.NCS = 31` → 32 slots, `S64A`, `VS = 0x00010000`, `PI = 0x3f`). The hardware/VFIO tier — BIOS/OS handoff (`CAP2.BOH = 0` on QEMU), staggered spin-up (`CAP.SSS = 0`), COMRESET timing, hot-plug, and real completion-interrupt routing — is **skip-with-reason** in CI and validated on bare metal, mirroring how `wifi-smoke` is skip-with-reason and the Phase 79 Realtek/igc tracks are hardware-only. A QEMU ordering trap to honor: `PxSIG` reads `0xFFFFFFFF` until `PxCMD.FRE` is enabled and the initial D2H FIS is delivered, so device classification must follow FRE, never precede it — `port.rs` classifies after `enable_fis_rx`.

## Important Components and How They Work

- **`kernel-core/src/storage/{ahci,ata}.rs` — the host-tested substrate.** Every register offset, struct (command header / command table / PRDT / received-FIS), FIS byte layout (`FIS_TYPE_REG_H2D = 0x27`), PRDT `DBC` N−1 encoding, slot allocator (`find_free_slot` over `PxSACT | PxCI`), signature classifier (`classify_port`/`is_driveable`), and the ATA opcode/LBA48/IDENTIFY-parse helpers live here, pinned by compile-time size/offset asserts and 34 host tests. None of it pokes hardware, so `cargo xtask check` proves it with no QEMU.
- **`userspace/drivers/ahci/lib.rs` — the `os-binary`/lib split.** Holds the driver-side decision logic that *can* be host-tested without a syscall surface: `request_is_oversized`, `pick_slot`, and `poll_outcome` (the issue/reap classifier that checks `is_fatal` *before* `cmd_complete`, so a host-bus error never reads as success). The production register-poking modules below are `#[cfg(not(test))]`-gated so the lib target stays host-testable.
- **`init.rs` / `port.rs` / `cmd.rs` — the production hardware path.** `init.rs` enables AHCI, resets the HBA, re-reads `CAP`/`PI`/`VS`, and runs the `CAP2.BOH` handoff gate. `port.rs` brings a port up through the stop → program-DMA(IOVA) → FRE → COMRESET → classify → clear-W1C → start ordering and owns `recover_port`. `cmd.rs` is the single-in-flight engine: build header + command table (CFIS + one PRDT at the bounce-buffer IOVA), issue on `PxCI`, poll `poll_outcome`, recover on a fatal `PxIS`.
- **`io.rs` — the (bare-metal-only) interrupt path.** `arm_interrupts`/`handle_irq` (PxIS-then-global-IS clear order) are written and unit-tested but unwired from the main loop because the data path is polling-primary under QEMU; the IRQ path is reserved for VFIO/bare-metal (Track C.5).
- **`main.rs` — the entry point and block server.** Discovers the controller by PCI class `0x010601`, brings up the first driveable SATA port, IDENTIFYs, then either runs the destructive boot self-test (a *blank* scratch disk — the `ahci-smoke` path) or read-only-probes the MBR (a disk with a valid MBR — the data-disk path); a LBA-0 read *error* fails closed. It registers `ahci.block` and serves `BLK_READ`/`BLK_WRITE` (write followed by `FLUSH CACHE EXT`) over the `driver_ipc::block` protocol.
- **`kernel/src/blk/remote.rs` + `kernel/src/blk/mbr.rs` — the two scoped kernel changes.** `is_registered()` learns the `"ahci.block"` name (preserving the `/drivers/`-owner trust gate and the `VIRTIO_BLK_READY` deferral so a SATA driver can never hijack the virtio root); `read_mbr()` reads sector 0 through the `blk::read_sectors` facade so the mount-time partition probe can reach a SATA root.
- **`userspace/init/src/main.rs` — the SATA-root bootstrap.** Spawns `/drivers/ahci` from the ramdisk and retries the ext2 mount only when the initial virtio-blk mount fails, so the normal virtio boot is untouched.

## How This Builds on Earlier Phases

- Reuses the Phase 55b ring-3 device-host primitives unchanged: `sys_device_claim` (which auto-enables Memory Space + Bus Master), `sys_device_mmio_map` (BAR5/ABAR), `sys_device_dma_alloc` (`DmaBuffer<T>`), `sys_device_irq_subscribe`.
- Reuses Phase 67's IOMMU `DmaBuffer<T>` for the command list / received-FIS / command table / data bounce, programming the device-visible IOVA into every HBA register.
- Reuses the Phase 55b `kernel-core/src/driver_ipc/block.rs` protocol, the `BlockDispatchState` restart machinery, and the `driver_runtime::ipc::block::BlockServer` server loop — unchanged.
- Reuses the Phase 8 `kernel-core/src/fs/mbr.rs` partition walker (`parse_mbr` / `find_ext2_partition` / `find_fat32_partition`) — no new partition-table code.

## Implementation Outline

1. Land the host-tested `kernel-core/src/storage/{ahci,ata}.rs` substrate (register/struct/FIS/PRDT/slot/classifier + 34 host tests) — provable by `cargo xtask check` with no QEMU.
2. Bring up the HBA against `-device ich9-ahci`: `GHC.AE` → `GHC.HR` reset → re-read `CAP`/`PI`/`VS` → `CAP2.BOH` handoff gate.
3. Bring up the first implemented SATA port: stop engine → program DMA structures (IOVA) → FRE → COMRESET → wait-ready → classify (skip non-SATA) → clear W1C → start engine.
4. IDENTIFY; print capacity / LBA48 / sector size / flush capability.
5. READ/WRITE DMA EXT (single + multi-block via one PRDT), then FLUSH CACHE EXT.
6. Serve `ahci.block`; teach the `blk::remote` cold-path lookup the name; mount an ext2 partition off SATA.
7. Bump the kernel to `0.82.0`.

## Acceptance Criteria

- `cargo xtask run --device ahci` boots with the data disk on AHCI; the boot log shows `CAP.NCS=32 S64A=1`, `PI=0x0000003f`, `ports_found=1`, `port 0 classified SATA`, `identify sectors=2097152 sector_bytes=512 flush=1`, `ext2 partition found`, `auto-registered ring-3 'ahci0' driver ... (ahci.block ...)`, and `/ mounted (ext2 via ring-3 ahci.block)` — then reaches the login shell.
- `cargo xtask ahci-smoke` boots `-device ich9-ahci` + a blank scratch `ide-hd` and asserts the binding sentinel set `AHCI_SMOKE:identify:PASS` / `write:PASS` / `readback:PASS` / `flush:PASS` / `identify2:PASS` / `recover:PASS` / `server:READY`; the read-back byte-compare is the load-bearing assertion, and `recover:PASS` exercises C.4 error recovery (an out-of-range LBA latches `PxIS.TFES`, `recover_port` restarts the engine, and a valid read then succeeds).
- No NVMe regression — both back-ends coexist; the standard `smoke-test` (virtio root) still passes 22 steps.
- `cargo xtask check` passes (clippy `-D warnings`, rustfmt, host tests).

## Companion Task List

- [Phase 82 Task List](./roadmap/tasks/82-ahci-sata-tasks.md)
- [Phase 82 Design Doc](./roadmap/82-ahci-sata.md)

## How Real OS Implementations Differ

- Linux's `libahci` + `ahci_platform` framework handles dozens of vendor-specific AHCI variants (Marvell, ASMedia, JMicron, Silicon Image) each with quirks; m3OS drives the generic class path only.
- Real OSes implement Native Command Queueing (NCQ) to overlap multiple outstanding commands per port; m3OS issues one at a time.
- TRIM / DEALLOCATE / SECURE ERASE / SMART are part of the SATA spec but deferred.
- Port multipliers (one AHCI port fanning out to multiple drives) and hot-plug (PRESENCE_DETECTED) are deferred.
- A production OS mounts the root via an initramfs that loads the storage driver before pivoting; m3OS's `init` spawns `/drivers/ahci` from the ramdisk and retries the ext2 mount (a minimal stand-in for that initramfs handoff).

## Deferred Until Later

- Native Command Queueing (NCQ)
- TRIM / DEALLOCATE
- SMART / health monitoring
- SECURE ERASE
- Port multipliers
- Hot-plug
- A real completion-interrupt data path (polling is primary; the IRQ arm/handle plumbing is bare-metal/VFIO-only)
- AHCI in IDE-emulation compatibility mode (prog-IF `0x00`)
