# Phase 82 - AHCI / SATA Storage: Task List

**Status:** Planned
**Source Ref:** phase-82
**Depends on:** Phase 55b (Ring-3 Driver Hosting) ✅, Phase 67 (IOMMU Substrate) ✅, Phase 8 (VFS + partition walker) ✅, Phase 74 (IPC Capability Grants) ✅
**Goal:** Land m3OS's first AHCI/SATA storage driver as a ring-3 device-host process so a SATA SSD/HDD on an AHCI-mode host bus adapter (HBA) is usable beside NVMe. The driver reuses the entire Phase 55b/67 device-host substrate (`sys_device_claim`/`sys_device_mmio_map`/`sys_device_dma_alloc`/`sys_device_irq_subscribe`), allocates the per-port command list + received-FIS area + command tables as `DmaBuffer<T>` (programming the **IOVA**, never host-physical, into every HBA register), brings up the HBA and each implemented port through the spec-mandated stop/start engine ordering, issues `IDENTIFY` / `READ DMA EXT` / `WRITE DMA EXT` / `FLUSH CACHE EXT`, and presents upward as a `RemoteBlockDevice` over the existing `kernel-core/src/driver_ipc/block.rs` protocol — so the VFS/ext2 stack mounts a SATA partition with **one kernel change on the data path** (the `blk::remote` cold-path lookup must learn the `"ahci.block"` service name; a possible second device-host IRQ-allocator change is hardware-only — see C.5). Host-testable bit/struct/encoding logic lives in a new `kernel-core/src/storage/` module (mirroring `kernel-core/src/nvme.rs`); QEMU's `ich9-ahci` model covers enumerate → reset → IDENTIFY → write/read/compare → flush → error-recover in CI, while BIOS/OS handoff, staggered spin-up, and hot-plug are bare-metal/VFIO-only.

## Track Layout

| Track | Scope | Dependencies | Status |
|---|---|---|---|
| A | Host-testable AHCI substrate in `kernel-core/src/storage/`: HBA + per-port register/offset/bit defs, Command Header + Command Table + PRDT + H2D/D2H Register FIS + Received-FIS `#[repr(C)]` layouts with size **and offset** asserts, free command-slot allocator over `PxSACT \| PxCI`, ATA opcode + H2D-FIS encoders (with `FIS_TYPE_REG_H2D = 0x27`), signature classifier, with full host unit tests | Phase 55b, Phase 67 | Done ✅ |
| B | HBA + port bring-up in `userspace/drivers/ahci/`: PCI class match + ABAR map, `GHC.AE`/`GHC.HR`, CAP2/BOHC handoff (QEMU-no-op), PI enumeration, port idle (stop engine ordering), DMA-structure program + FRE, COMRESET + presence detect, signature classify + non-`Sata` skip, port start | A | Planned |
| C | Command issue + completion + errors: IDENTIFY, READ/WRITE DMA EXT (single + multi-block via PRDT), FLUSH CACHE EXT durability, slot issue + completion poll, IRQ-on-completion path (`PxIS` then host `IS` clear), `PxIS.TFES`/`PxSERR` error recovery mapped onto the `RemoteBlockDevice` restart semantics | A, B | Planned |
| D | `RemoteBlockDevice` facade: register `"ahci.block"`, serve `BlkRequestHeader`/`BlkReplyHeader`, honor `MAX_SECTORS_PER_REQUEST` chunking, kernel cold-path lookup learns `"ahci.block"`, boot-time MBR partition probe, four-place binary wiring + `ahci_driver.conf` | C, Phase 8 | Planned |
| E | xtask integration + `ahci-smoke` gate: `--device ahci` flag + `DeviceSet`, `-device ich9-ahci` + `-drive if=none` + `ide-hd` emission, `cargo xtask run --device ahci`, `cmd_ahci_smoke` (IDENTIFY + write + read-back + flush + IDENTIFY-after-write) | B, C, D | Planned |
| F | Release closeout: kernel `0.81.0` → `0.82.0` bump, learning doc `docs/82-ahci-sata.md`, README row flip + Tasks-cell link, design-doc reconciliation, `AGENTS.md` gate-table row + check-list crate | A–E landed | Planned |

> **Ordering note.** Track A (the `kernel-core::storage` substrate) is written and host-tested **first** — every register offset, struct size/offset, FIS byte layout (including the `FIS_TYPE_REG_H2D = 0x27` type byte), PRDT DBC encoding, slot-allocation predicate, and signature classifier is proven by `cargo xtask check` with no QEMU, exactly as Phase 80 put HDA verb/`SDnFMT`/BDL math in `kernel-core` and Phase 79 put `nic_ids`/`r8169` there. Track B brings the HBA up against QEMU's `ich9-ahci` device (the AHCI analog of HDA's `-device intel-hda`); C adds the data path; D wires the block facade + partition probe; E is the gate. The BIOS/OS-handoff (B.3), staggered-spin-up, COMRESET timing (B.5), and a real completion interrupt (C.5) are **bare-metal/VFIO-only** because QEMU's model leaves `CAP2.BOH = 0` and `CAP.SSS = 0` — marked explicitly per the Phase 79/80 "skip-with-reason + hardware runbook" precedent.

> **Single-queue rationale.** AHCI defines exactly one command queue per port (32 slots); m3OS at 1.0 issues **one command at a time per port** (no NCQ / `PxSACT` overlap), symmetric with the single-queue NVMe decision (Phase 55b). The slot allocator (A.5) still scans `PxSACT | PxCI` so the design is forward-compatible with NCQ, but the data path waits on a single in-flight slot. NCQ is deferred (design-doc "Deferred Until Later").

> **Kernel-change scope.** The shipped phase touches the kernel on exactly **one data path**: `kernel/src/blk/remote.rs::is_registered()` learns the `"ahci.block"` service name (D.2), the same single named addition Phase 81 made with `default_route_index_by_link`. A *possible* second change — forcing INTx in the device-host IRQ allocator for storage-class `0x01` (the Phase 79 fix shape) — is **bare-metal/VFIO-only** and **out of the 1.0 data path**, because the completion path is polling-primary (C.5); it is recorded if hardware needs it but is not required for the QEMU-tested phase.

---

## Track A — Host-testable AHCI substrate (`kernel-core/src/storage/`)

### A.1 — New `kernel-core` storage module + lib registration

**Files:**
- `kernel-core/src/storage/mod.rs` (new)
- `kernel-core/src/storage/ahci.rs` (new)
- `kernel-core/src/storage/ata.rs` (new)
- `kernel-core/src/lib.rs` (`pub mod storage;`)

**Symbol:** `pub mod storage;` in `lib.rs`; `kernel_core::storage::{ahci, ata}` submodules; module-level doc comment mirroring `kernel-core/src/nvme.rs`'s header (single source of truth for the host-testable AHCI bit math, shared by the `ahci` driver and exercised by host tests)
**Why it matters:** AGENTS.md mandates that pure-logic code be host-testable in `kernel-core` (the kernel is `no_std` and cannot be `cargo test`ed in QEMU); putting the AHCI register/struct/encode logic here makes it provable by `cargo xtask check` exactly like `kernel_core::nvme`, `kernel_core::hda`, and `kernel_core::r8169`.

**Acceptance:**
- [x] `kernel-core/src/lib.rs` declares `pub mod storage;` alphabetically between `slab` and `time` (or wherever the existing ordering places it) and `cargo test -p kernel-core --target x86_64-unknown-linux-gnu` compiles the new module.
- [x] `cargo xtask check` builds with the new module present and runs its host tests (the logic stays in `kernel-core`, which is already in the check list, so no new crate entry is needed — recorded in F.3).

### A.2 — HBA + per-port register map (offsets + bit constants)

**File:** `kernel-core/src/storage/ahci.rs`
**Symbol:** generic-host-control offsets `HBA_CAP = 0x00`, `HBA_GHC = 0x04`, `HBA_IS = 0x08`, `HBA_PI = 0x0C`, `HBA_VS = 0x10`, `HBA_CAP2 = 0x24`, `HBA_BOHC = 0x28`; `GHC_AE = 1 << 31`, `GHC_IE = 1 << 1`, `GHC_HR = 1 << 0`; `CAP_S64A = 1 << 31`, `CAP_SSS = 1 << 27`, `CAP_SCLO = 1 << 24`, `CAP_NCS_SHIFT = 8` / `CAP_NCS_MASK = 0x1F`; `CAP2_BOH = 1 << 0`; `BOHC_BOS = 1 << 0` / `BOHC_OOS = 1 << 1` / `BOHC_BB = 1 << 4`; FIS type bytes `FIS_TYPE_REG_H2D = 0x27` / `FIS_TYPE_REG_D2H = 0x34`; per-port `port_base(n) = 0x100 + n * 0x80`; port offsets `PX_CLB = 0x00`, `PX_CLBU = 0x04`, `PX_FB = 0x08`, `PX_FBU = 0x0C`, `PX_IS = 0x10`, `PX_IE = 0x14`, `PX_CMD = 0x18`, `PX_TFD = 0x20`, `PX_SIG = 0x24`, `PX_SSTS = 0x28`, `PX_SCTL = 0x2C`, `PX_SERR = 0x30`, `PX_SACT = 0x34`, `PX_CI = 0x38`; `PX_CMD` bits `CMD_ST = 1 << 0` / `CMD_SUD = 1 << 1` / `CMD_POD = 1 << 2` / `CMD_CLO = 1 << 3` / `CMD_FRE = 1 << 4` / `CMD_FR = 1 << 14` / `CMD_CR = 1 << 15`; `TFD_BSY = 0x80` / `TFD_DRQ = 0x08` / `TFD_ERR = 0x01`; `PX_IS_TFES = 1 << 30` / `IS_HBFS = 1 << 29` / `IS_HBDS = 1 << 28` / `IS_IFS = 1 << 27`; `SSTS_DET_MASK = 0xF` / `SSTS_DET_PRESENT = 3` / `SSTS_IPM_SHIFT = 8` / `SSTS_IPM_ACTIVE = 1`
**Why it matters:** every register access in Track B is a literal offset/bit; pinning them in one host-tested table (cross-checked against Linux `ahci.h`, the AHCI 1.3.1 spec, and QEMU `ahci-internal.h`) means a transcription slip is a failing test, not a silent register write to the wrong offset — `PxCMD.CR`/`FR` are **read-only status** bits and `PxIS`/`PxSERR` are **write-1-to-clear**, which the bit names encode so call sites cannot mistreat them; the `FIS_TYPE_REG_H2D = 0x27` type byte is the constant every real HBA validates in the command FIS, so it lives in the table beside the rest.

**Acceptance:**
- [x] Host test asserts the generic-host-control offsets equal `{CAP:0x00, GHC:0x04, IS:0x08, PI:0x0C, VS:0x10, CAP2:0x24, BOHC:0x28}` and `port_base(0)==0x100`, `port_base(1)==0x180`, `port_base(5)==0x380` (`kernel_core::storage::ahci::tests::register_offsets`).
- [x] Host test asserts the `PxCMD` bit values `ST=0x1, SUD=0x2, POD=0x4, CLO=0x8, FRE=0x10, FR=0x4000, CR=0x8000` and that `GHC_AE==1<<31`, `GHC_HR==1<<0`, `CAP2_BOH==1<<0` (`tests::cmd_bits`, `tests::ghc_bits`).
- [x] Host test asserts the FIS type bytes `FIS_TYPE_REG_H2D == 0x27` and `FIS_TYPE_REG_D2H == 0x34` (`tests::fis_type_bytes`).
- [x] Host test asserts `PX_IS_TFES == 1 << 30` and `SSTS_DET_PRESENT == 3` and that a `PxSSTS` value `0x113` decodes to DET=3 / IPM=1 (the QEMU device-present value) via a `port_present(ssts) -> bool` helper (`tests::ssts_present`).

### A.3 — Command-list / command-table / PRDT / FIS struct layouts (`#[repr(C)]` + size & offset asserts)

**File:** `kernel-core/src/storage/ahci.rs`
**Symbol:** `#[repr(C)] HbaCmdHeader { byte0: u8 (cfl:5/a/w/p bitfield accessors), byte1: u8 (R/B/C/PMP — DW0 byte 1), prdtl: u16, prdbc: u32, ctba: u32, ctbau: u32, _rsv: [u32; 4] }` so PRDTL is the high 16 bits of DW0 (byte offset 2), with `const _: () = assert!(size_of::<HbaCmdHeader>() == 32)` **and** offset asserts; `#[repr(C)] HbaPrdtEntry { dba: u32, dbau: u32, _rsv: u32, dbc: u32 }` with `assert!(size_of::<HbaPrdtEntry>() == 16)` and a `encode_dbc(byte_count, interrupt) -> u32` helper applying the **N−1** encoding (`(byte_count - 1) | (interrupt << 31)`, low bit of count always 1) with `debug_assert!(byte_count > 0)`; `#[repr(C)] HbaCmdTable` with `cfis: [u8; 64]`, `acmd: [u8; 16]`, `_rsv: [u8; 48]`, and the PRDT region beginning at offset `0x80`; `#[repr(C)] FisRegH2D` (`fis_type`, `pm_c` byte with the C-bit `1 << 7`, `command`, `featurel`, `lba0..lba5`, `device`, `featureh`, `countl`, `counth`, `icc`, `control`) with `assert!(size_of::<FisRegH2D>() == 20)`; `#[repr(C)] FisRegD2H`; `#[repr(C)] HbaFis` (the 256-byte received-FIS area, `dsfis`/`psfis`/`rfis` at `0x40`/`sdbfis`)
**Why it matters:** the HBA DMA-reads these structures at the IOVAs programmed in B.4, so a wrong field width, a missing reserved gap, or a mis-placed PRDTL silently corrupts the command — DW0 of the command header is a full 32-bit dword (byte0 flags, byte1 flags, then **PRDTL at byte offset 2**); omitting byte 1 lands `prdtl` at offset 1 and the HBA reads the PRDT length from the wrong bytes, a corruption a passing `size_of == 32` assert (absorbed by trailing reserved padding) would *not* catch — so the layout is pinned with **offset** asserts, not just size; the AHCI command header is exactly 32 bytes (32 × 32 B = the 1 KiB command list), the PRDT entry exactly 16 bytes with the DBC N−1 encoding, and the H2D Register FIS exactly 20 bytes (`CFL = 5` dwords); compile-time asserts make a layout mistake a build failure, mirroring `kernel_core::mt792x::dma::Mt76Desc`'s 16-byte assert.

**Acceptance:**
- [x] Compile-time `const _: () = assert!(...)` guarantees `size_of::<HbaCmdHeader>() == 32`, `size_of::<HbaPrdtEntry>() == 16`, `size_of::<FisRegH2D>() == 20`, and `size_of::<HbaFis>() == 256` (build fails otherwise).
- [x] Compile-time **offset** asserts guarantee `offset_of!(HbaCmdHeader, prdtl) == 2`, `offset_of!(HbaCmdHeader, prdbc) == 4`, `offset_of!(HbaCmdHeader, ctba) == 8`, `offset_of!(HbaCmdHeader, ctbau) == 12` (catches the DW0-byte-1 omission a size-only assert would miss).
- [x] Host test asserts `encode_dbc(8 * 512, false) == (8 * 512 - 1)` (no interrupt bit) and `encode_dbc(512, true) == (511 | (1 << 31))`, that the encoded count is always odd (low bit set), and that `encode_dbc(0, _)` is rejected/`debug_assert`-guarded (documented zero-length case) (`kernel_core::storage::ahci::tests::prdt_dbc_n_minus_1`, `tests::prdt_dbc_rejects_zero`).
- [x] Host test asserts the command-table PRDT region starts at byte offset `0x80` (`cfis` 64 + `acmd` 16 + reserved 48) and that `HbaCmdHeader`'s `cfl` accessor reads/writes the low 5 bits of byte 0 while the `w` (write) accessor is bit 6 (`tests::cmd_table_layout`, `tests::cmd_header_bitfields`).

### A.4 — ATA opcode + H2D-FIS command encoders

**File:** `kernel-core/src/storage/ata.rs`
**Symbol:** opcode consts `ATA_CMD_READ_DMA_EXT = 0x25`, `ATA_CMD_WRITE_DMA_EXT = 0x35`, `ATA_CMD_IDENTIFY = 0xEC`, `ATA_CMD_IDENTIFY_PACKET = 0xA1`, `ATA_CMD_FLUSH_CACHE_EXT = 0xEA`; `encode_rw_fis(write: bool, lba: u64, sectors: u16) -> FisRegH2D` hard-wiring `fis_type = FIS_TYPE_REG_H2D` (`0x27`), splitting the 48-bit LBA into `lba0..lba5`, setting `device = 1 << 6` (LBA48 mode), `command = WRITE/READ_DMA_EXT`, `countl`/`counth`, and the C-bit (`pm_c = 1 << 7`), with `debug_assert!(sectors != 0)` (an LBA48 count of 0 means 65536 — explicitly documented/forbidden); `encode_identify_fis() -> FisRegH2D` (`fis_type = 0x27`); `encode_flush_fis() -> FisRegH2D` (`fis_type = 0x27`, non-data: `command = 0xEA`, C-bit set, no LBA/count); `parse_identify(buf: &[u16; 256]) -> AtaIdentify { lba48_sectors, logical_sector_bytes, supports_lba48, has_flush_ext }` reading the LBA48 sector count from words 100–103, the logical-sector size from **word 106** (if bit 14==1 && bit 15==0 && bit 12==1, use words 117–118 ×2 for bytes; else default 512), and the command-set words for FLUSH CACHE EXT support
**Why it matters:** the H2D Register FIS is the single command channel to the drive — the `fis_type = 0x27` byte is validated by QEMU's `ich9-ahci` and every real HBA (a zero/wrong type makes the HBA reject the command), and an LBA byte split error or a missing C-bit yields a misaddressed transfer or a control update the drive ignores; encoding it once in a host-tested function (with `fis_type = 0x27`, `device = 1 << 6` LBA48, and the C-bit hard-wired) guarantees every command in Track C is well-formed, and `parse_identify` is where capacity/LBA48/flush-capability and the logical sector size come from to size the block device.

**Acceptance:**
- [x] Host test asserts every encoder sets the FIS type byte: `encode_rw_fis(..).fis_type == 0x27`, `encode_identify_fis().fis_type == 0x27`, `encode_flush_fis().fis_type == 0x27` (`kernel_core::storage::ata::tests::fis_type_is_h2d`).
- [x] Host test asserts `encode_rw_fis(false, 0x01_0203_0405, 8)` produces `command == 0x25`, `device == 0x40`, `lba0..lba5 == [0x05,0x04,0x03,0x02,0x01,0x00]`, `countl == 8`, `counth == 0`, and the C-bit set; the `write=true` variant sets `command == 0x35`; a `sectors == 0` call is rejected/`debug_assert`-guarded (or its `0==65536` semantics documented) (`tests::rw_fis_lba48_split`, `tests::rw_fis_rejects_zero_count`).
- [x] Host test asserts `encode_identify_fis().command == 0xEC` and `encode_flush_fis().command == 0xEA` with the C-bit set and zero PRDT-bearing fields (non-data) (`tests::identify_fis`, `tests::flush_fis_is_non_data`).
- [x] Host test: `parse_identify` over a synthetic 256-word IDENTIFY block returns the LBA48 sector count assembled from words 100–103, `has_flush_ext = true` when the command-set bit is set, and `logical_sector_bytes == 512` for a block whose **word 106** indicates standard 512-byte sectors (QEMU `ide-hd`); computed capacity = `lba48_sectors * logical_sector_bytes` (`tests::parse_identify_capacity`, `tests::parse_identify_default_512`).

### A.5 — Free command-slot allocator over `PxSACT | PxCI` + NCS bound

**File:** `kernel-core/src/storage/ahci.rs`
**Symbol:** `ncs_from_cap(cap: u32) -> u8` extracting `((cap >> 8) & 0x1F) + 1` (number of command slots); `find_free_slot(sact: u32, ci: u32, ncs: u8) -> Option<u8>` returning the lowest slot index `< ncs` whose bit is clear in `sact | ci`; `cmd_complete(ci: u32, slot: u8, is: u32) -> bool` (slot's `PxCI` bit clear **and** no `PxIS` error bit set)
**Why it matters:** AHCI completion is "the slot's `PxCI` bit auto-clears" and a command may be issued only on a free slot (`PxSACT | PxCI` bit clear); pinning the slot scan and the NCS bound in host-tested pure functions makes the issue/reap loop in C.1 correct by construction and forward-compatible with NCQ (the same scan over `PxSACT` is what NCQ needs), exactly as the Redox `slot()` and OSDev `find_cmdslot` references do.

**Acceptance:**
- [x] Host test: `ncs_from_cap` returns `32` for the QEMU CAP value (`NCS` field == 31) and the correct count for a synthetic CAP with `NCS = 0` → `1` (`kernel_core::storage::ahci::tests::ncs_from_cap`).
- [x] Host test: `find_free_slot(0, 0, 32)` returns `Some(0)`; with slots 0–2 busy in `ci` it returns `Some(3)`; with all `ncs` slots busy it returns `None`; a slot `>= ncs` is never returned (`tests::find_free_slot`).
- [x] Host test: `cmd_complete(ci, slot, is)` is `true` only when the slot bit is clear in `ci` and `is & PX_IS_TFES == 0`; an error bit set returns `false` even with `PxCI` clear (`tests::cmd_complete_requires_no_error`).

### A.6 — Device-signature classifier

**File:** `kernel-core/src/storage/ahci.rs`
**Symbol:** `enum PortDeviceType { Sata, Satapi, PortMultiplier, Semb, None, Unknown(u32) }`; consts `SIG_ATA = 0x0000_0101`, `SIG_ATAPI = 0xEB14_0101`, `SIG_PM = 0x9669_0101`, `SIG_SEMB = 0xC33C_0101`; `classify_signature(sig: u32) -> PortDeviceType`; `classify_port(ssts: u32, sig: u32) -> PortDeviceType` returning `None` unless `SSTS.DET == 3`; `is_driveable(dt: PortDeviceType) -> bool` (`true` only for `Sata`)
**Why it matters:** the driver must drive only `SIG_ATA` ports and skip port multipliers / SEMB / ATAPI (out of 1.0 scope); classifying from `PxSIG` is the dispatch point, and `PxSIG` is only valid after FRE is enabled (the QEMU model returns `0xFFFFFFFF` until then), so the classifier folds in the presence check and the `is_driveable` gate keeps an enclosure/PM device on a real backplane from wedging bring-up.

**Acceptance:**
- [x] Host test: `classify_signature(0x0000_0101) == Sata`, `0xEB14_0101 == Satapi`, `0x9669_0101 == PortMultiplier`, `0xC33C_0101 == Semb`, `0xFFFF_FFFF == Unknown(..)` (`kernel_core::storage::ahci::tests::classify_signature`).
- [x] Host test: `classify_port(0x113, 0x0000_0101) == Sata` (DET=3 present) but `classify_port(0x000, 0x0000_0101) == None` (no device) (`tests::classify_port_requires_present`).
- [x] Host test: `is_driveable(Sata) == true` and `is_driveable` is `false` for `Satapi`/`PortMultiplier`/`Semb`/`None`/`Unknown(..)` (`tests::only_sata_is_driveable`).

---

## Track B — HBA + port bring-up (`userspace/drivers/ahci/`)

### B.1 — PCI claim + AHCI class match + ABAR (BAR5) MMIO map + BME

**Files:**
- `userspace/drivers/ahci/src/main.rs` (new; model on `userspace/drivers/nvme/src/main.rs::program_main`)
- `userspace/drivers/ahci/src/init.rs` (new)
- `userspace/lib/driver_runtime/src/pci_enum.rs` (reuse `enumerate_pci_class` for class `0x01`/subclass `0x06`/prog-IF `0x01`)

**Symbol:** `program_main`; `ahci_pci_match(class, subclass, prog_if) -> bool` accepting `(0x01, 0x06, 0x01)` — intentionally a **3-arg, class-only** predicate (it deliberately drops the vendor/device params present in `hda_pci_match` because AHCI matches purely on class `0x010601`; no signature copy is implied); `claim_and_map` mapping **BAR5** (`AHCI_ABAR_BAR_INDEX = 5`) via `sys_device_mmio_map` and enabling Bus Master + Memory Space; emits an `AHCI_SMOKE:server:READY` sentinel before the event loop
**Why it matters:** AHCI is identified primarily by PCI class `0x010601`, not a device ID (gating on a vendor ID would miss most controllers — the AC'97 mistake Phase 80 corrected for HDA); the ABAR lives in **BAR5** (CPU MMIO via `sys_device_mmio_map`, *never* an IOVA), and Bus Master must be enabled before the HBA may DMA the command list / FIS / PRDT.

**Acceptance:**
- [ ] Host test asserts `ahci_pci_match(0x01, 0x06, 0x01) == true` and rejects the NVMe class `(0x01, 0x08, 0x02)` and the SATA-IDE-mode prog-IF `(0x01, 0x06, 0x00)` (`kernel_core::storage::ahci::tests::pci_match` if the predicate is hosted in `kernel-core`, else `ahci_driver::tests::pci_match`).
- [ ] Under `cargo xtask run --device ahci` (or `cargo xtask ahci-smoke`), the driver claims the `ich9-ahci` device, maps BAR5, and emits `AHCI_SMOKE:server:READY` before its event loop.
- [ ] The driver crate builds with the `os-binary`/lib split (like `r8169`/`hda`) so the match + slot/encode logic is host-testable in the lib target.

### B.2 — `GHC.AE` AHCI-enable + `GHC.HR` HBA reset + CAP/PI/VS read

**File:** `userspace/drivers/ahci/src/init.rs`
**Symbol:** `enable_ahci` (set `GHC_AE`, read-back-confirm, retry up to 5× with a short delay, like Linux `ahci_enable_ahci`); `reset_hba` (set `GHC_HR`, poll until it reads back 0 with a **1 s** bounded timeout, then re-assert `GHC_AE`, then re-read `CAP`/`PI`); `read_caps -> HbaCaps { ncs, s64a, sss, sclo, pi, version }` reading `CAP`/`PI`/`VS`
**Why it matters:** `GHC.AE` must be set before any port-register access has AHCI semantics, and the global `GHC.HR` reset clears `AE` and reloads `CAP`/`PI` (so both must be re-read **after** the reset self-clears and `AE` is re-asserted, matching Linux `ahci_reset_controller` → `ahci_save_initial_config` ordering) — the spec bounds the reset self-clear at 1 s, after which the controller is dead and must be abandoned; `CAP.NCS` gives the slot count (A.5) and `CAP.S64A` decides whether the `*U` high-dword registers may carry the IOVA's upper 32 bits.

**Acceptance:**
- [ ] `enable_ahci` confirms `GHC_AE` reads back set; on QEMU `AE` is already forced and the call is idempotent (driver assertion + serial sentinel `AHCI: GHC_AE confirmed`).
- [ ] `reset_hba` polls `GHC_HR` to 0 within the 1 s budget, then re-asserts `GHC_AE` and **only then** reads `CAP`/`PI`/`VS`, logging the exact sentinel `AHCI: VS=0x00010000 PI=0x<hex>` (PI/CAP read after reset+AE, not before); on timeout the driver logs and aborts (no hang).
- [ ] The driver logs the exact sentinel `AHCI: CAP.NCS=32 S64A=1` (NCS == 32 on QEMU's `ich9-ahci`) and `find_free_slot` is bounded by the read NCS, not a hardcoded 32.

### B.3 — BIOS/OS handoff (CAP2.BOH / BOHC) — bare-metal path with QEMU no-op gate

**File:** `userspace/drivers/ahci/src/init.rs`
**Symbol:** `bios_os_handoff` gated on `CAP2_BOH`: set `BOHC_OOS`, poll `BOHC_BOS` → 0 (allow ~25 ms), and if `BOHC_BB` becomes set extend the wait up to ~2 s; a host-tested `handoff_needed(cap2: u32) -> bool` predicate
**Why it matters:** on firmware that still owns the HBA (legacy BIOS, or VFIO/bare-metal where firmware did not hand off), the OS must take ownership via the BOHC handshake before driving the controller; QEMU's `ich9-ahci` leaves `CAP2.BOH = 0`, so the handoff must be **gated on the bit** and skipped entirely there — attempting the handshake on QEMU would read zeros forever.

**Acceptance:**
- [ ] Host test: `handoff_needed(cap2)` is `true` only when `cap2 & CAP2_BOH != 0`; `false` for the QEMU `cap2 == 0` case (`kernel_core::storage::ahci::tests::handoff_needed`).
- [ ] Under QEMU the driver logs the exact sentinel `bios/os handoff: skipped (CAP2.BOH=0)` and does not poll BOHC.
- [ ] *(Bare-metal/VFIO-only — QEMU has no BIOS-owned HBA.)* On real firmware that reports `CAP2.BOH=1`, the handoff completes: `BOHC.BOS` clears (and `BB` clears if set) within the bounded budget before any port is driven.

### B.4 — Port presence enumeration + idle (engine stop ordering) + DMA-structure program

**Files:**
- `userspace/drivers/ahci/src/port.rs` (new)
- `kernel-core/src/storage/ahci.rs` (the `stop_engine`/`start_engine` predicate helpers)

**Symbol:** `for each PI bit` → `Port::probe` (read `PxSSTS`, require `DET == 3`); `stop_engine` (clear `CMD_ST`, poll `CMD_CR` → 0 with a 500 ms budget, then clear `CMD_FRE`, poll `CMD_FR` → 0); `program_dma_structures` allocating the command list (32 × `HbaCmdHeader`, 1 KiB-aligned), the received-FIS area (256 B, 256 B-aligned), and 32 command tables as `DmaBuffer<T>`, then writing `PxCLB/PxCLBU` (`dma.iova()` lo/hi) + `PxFB/PxFBU` and each header's `ctba/ctbau`; host-tested `engine_stopped(cmd: u32) -> bool` (`CR` and `FR` both clear)
**Why it matters:** the cardinal AHCI ordering rule is **clear `ST` and confirm `CR == 0` before clearing `FRE`, and confirm `CR == 0` before re-setting `ST`** — `CR`/`FR` are read-only status the HBA drives, and reprogramming `PxCLB`/`PxFB` while the engine runs is undefined and corrupts the command-list pointer; the addresses written are `DmaBuffer::iova()` (the device-visible IOVA under VT-d/AMD-Vi), never the user VA — the single most likely first-driver bug, identical to the HDA CORB/RIRB and mt792x descriptor IOVA traps.

**Acceptance:**
- [ ] Host test: `engine_stopped(cmd)` is `true` only when both `CMD_CR` and `CMD_FR` are clear; the stop predicate models clear-`ST`→wait-`CR`, then clear-`FRE`→wait-`FR` ordering (`kernel_core::storage::ahci::tests::engine_stop_ordering`).
- [ ] Under QEMU the driver enumerates the `PI` bitmap, finds the `ide-hd` port with `PxSSTS == 0x113`, skips empty ports (`DET != 3`), and logs the exact sentinel `AHCI: ports_found=<N>`.
- [ ] The address written to `PxCLB`/`PxFB` is asserted equal to the corresponding `DmaBuffer::iova()` (not `user_ptr()`) — driver assertion + serial log (mirrors the HDA `CORBLBASE == dma.iova()` check).
- [ ] The command list is 1 KiB-aligned (32 × 32 B) and the received-FIS area is 256 B-aligned (allocation `align` arguments asserted).

### B.5 — FRE enable + COMRESET + presence/ready wait + signature classify + port start

**File:** `userspace/drivers/ahci/src/port.rs`
**Symbol:** `enable_fis_rx` (set `CMD_FRE`, poll `CMD_FR` → 1 — this is what makes `PxSIG` valid on QEMU); `comreset` (write `PxSCTL.DET = 1`, wait ≥ 1 ms, write `DET = 0`, poll `PxSSTS.DET == 3`) — **bare-metal-meaningful, QEMU-tolerant**; `wait_ready` (poll `PxTFD` until `BSY|DRQ` clear, bounded ~1 s; on stuck-BSY use `PxCMD.CLO` if `CAP.SCLO`); `classify` (read `PxSIG` → A.6) followed by an `is_driveable` gate; `start_engine` (confirm `CR == 0`, set `CMD_FRE` then `CMD_ST`); clear `PxSERR`/`PxIS` before start
**Why it matters:** `PxSIG` reads `0xFFFFFFFF` until FRE is enabled and the initial D2H FIS is delivered (a QEMU ordering trap — classify *after* FRE, not before); `PxSCTL.DET` COMRESET re-establishes the PHY on real hardware (QEMU's link is always up so it is tolerant); `PxTFD.BSY/DRQ` must both be clear before `PxCMD.ST` is set or a real drive hangs; and `PxSERR`/`PxIS` are write-1-to-clear and must be cleared before the engine starts or a stale bit immediately re-interrupts; only a `Sata` signature is driven, so a port multiplier / SEMB / ATAPI device is logged and skipped (A.6 `is_driveable`).

**Acceptance:**
- [ ] Under QEMU, after `enable_fis_rx`, `PxSIG` reads `0x0000_0101` for the `ide-hd` disk (was `0xFFFFFFFF` before FRE) and `classify` returns `Sata`; serial logs the classified device type.
- [ ] A non-`Sata` signature (PM `0x9669_0101` / SEMB `0xC33C_0101` / ATAPI when unsupported) is logged with the exact sentinel `AHCI: port <n> skipped non-SATA sig=0x<hex>` and the port is **not** driven (so an enclosure/PM device on a real backplane cannot wedge bring-up).
- [ ] The driver clears `PxSERR` (write read-back value) and `PxIS` before setting `CMD_ST`, then confirms `CMD_CR` reads back 1 after start (driver assertion + serial log).
- [ ] `wait_ready` confirms `PxTFD.BSY` and `PxTFD.DRQ` are both clear before the first command is issued (the QEMU model starts `tfdata = 0x7F` then clears via the D2H FIS).
- [ ] *(Bare-metal/VFIO-only — QEMU's link is always up.)* `comreset` brings `PxSSTS.DET` to 3 on a real SATA link after asserting/de-asserting `PxSCTL.DET`.

---

## Track C — Command issue + completion + errors

### C.1 — Slot issue + completion poll (single in-flight command)

**Files:**
- `userspace/drivers/ahci/src/cmd.rs` (new)
- `kernel-core/src/storage/ahci.rs` (reuse A.5 `find_free_slot`/`cmd_complete`)

**Symbol:** `issue_command(slot, cfl, write, prdtl)` filling the command header (`cfl = 5`, set the `w` bit for writes), zeroing the command table, copying the H2D FIS into `cfis`, waiting `PxTFD & (BSY|DRQ) == 0`, then setting the slot bit in `PxCI`; `await_completion(slot, timeout_ms)` polling `cmd_complete(PxCI, slot, PxIS)` with a bounded budget; returns `Err` → Track C.4 recovery on `PxIS.TFES` or timeout
**Why it matters:** completion is "the slot's `PxCI` bit auto-clears with no `PxIS` error bit" — at 1.0 m3OS keeps one command in flight per port (single-queue), so the issue→poll loop is the entire data-path engine; a missing pre-issue `BSY/DRQ` wait or a poll that ignores `PxIS.TFES` would either hang on a busy port or report a failed command as success.

**Acceptance:**
- [ ] Host test (over the A.5 predicates): a slot is issued only when free (`find_free_slot` returned it) and `await_completion` returns success only when `cmd_complete` is true; an `is` with `PX_IS_TFES` set makes it return an error (`ahci_driver::cmd::tests::issue_then_complete`, reusing `kernel_core` predicates).
- [ ] Under QEMU, an `IDENTIFY` issued via `PxCI` completes (`PxCI` bit clears) within the timeout with no `PxIS` error.

### C.2 — IDENTIFY DEVICE (capacity / LBA48 / flush capability)

**File:** `userspace/drivers/ahci/src/cmd.rs`
**Symbol:** `identify() -> AtaIdentify` issuing `encode_identify_fis()` (A.4) with one PRDT entry pointing at a 512-byte `DmaBuffer`, awaiting completion, and parsing the 256-word block via `parse_identify`; the driver caches `lba48_sectors`, `logical_sector_bytes`, and `has_flush_ext` for the block-device geometry
**Why it matters:** IDENTIFY is the recommended first command (validates the whole issue/PRDT/completion path before any read/write) and is where the device capacity, LBA48 support, logical sector size, and FLUSH-CACHE-EXT capability come from — the `RemoteBlockDevice` reports `lba48_sectors` as its logical block count and gates C.3's flush on `has_flush_ext`.

**Acceptance:**
- [ ] Under QEMU `-device ide-hd`, `identify` returns a non-zero capacity and a model/serial string sourced from the `-drive`, logged with the exact sentinel `AHCI: identify sectors=<N> sector_bytes=512 flush=1` (like NVMe's model print).
- [ ] The PRDT entry for IDENTIFY points at the 512-byte `DmaBuffer::iova()` and `DBC == encode_dbc(512, _)` (driver assertion).

### C.3 — READ DMA EXT + WRITE DMA EXT (single + multi-block via PRDT) + FLUSH CACHE EXT

**File:** `userspace/drivers/ahci/src/cmd.rs`
**Symbol:** `read_sectors(lba, count, buf)` / `write_sectors(lba, count, buf)` building `encode_rw_fis(write, lba, count)` (A.4) + one PRDT entry (`DBA/DBAU = DmaBuffer::iova()`, `DBC = encode_dbc(count * sector_bytes, _)`); a bounce `DmaBuffer` the driver copies in/out around the transfer (each command capped at `< 256` sectors, like Redox's 256-sector bounce); `flush()` issuing `encode_flush_fis()` (`0xEA`, non-data, `PRDTL = 0`) and awaiting completion before reporting a write durable
**Why it matters:** READ/WRITE DMA EXT (`0x25`/`0x35`) are the 48-bit-LBA variants (so `device = 1 << 6`), and the PRDT carries the data IOVA + the N−1-encoded byte count; a `WRITE DMA EXT` completing only means the data reached the drive's volatile cache — **FLUSH CACHE EXT is required for durability** (Redox `ahcid` omits it, a data-loss bug this driver must not repeat), so a sync/barrier issues `0xEA` and only reports durable once it completes without error.

**Acceptance:**
- [ ] Under QEMU, a single-block write to a known LBA followed by a read-back of that LBA byte-compares equal (the core "the driver moves data" assertion).
- [ ] A multi-block (e.g. 8-sector) write/read round-trip via a single PRDT entry byte-compares equal; the PRDT `DBC` equals `encode_dbc(8 * 512, _)`.
- [ ] `flush()` issues command `0xEA` with `PRDTL == 0` (non-data) and completes without `PxIS.TFES`; the write path reports durable only after the flush returns (serial sentinel `AHCI: flush durable lba=<N>`).
- [ ] The data buffer programmed into the PRDT is the bounce `DmaBuffer::iova()`, asserted not equal to `user_ptr()`.

### C.4 — `PxIS.TFES` / `PxSERR` error recovery → engine restart

**Files:**
- `userspace/drivers/ahci/src/port.rs`
- `kernel-core/src/storage/ahci.rs` (the recovery-decision helpers)

**Symbol:** `recover_port` on `PxIS.TFES`/`IS_HBFS` or a command timeout: read `PxTFD`/`PxSERR` to capture the error, `stop_engine` (clear `ST`, poll `CR` → 0), write `PxSERR` and `PxIS` back (W1C clear), and — for a fatal/interface error — `comreset`, then `start_engine` and re-issue or fail the slot; host-tested `is_fatal(is: u32) -> bool` (any of `TFES|HBFS|HBDS|IFS`)
**Why it matters:** on `TFES` the HBA halts the engine and leaves the failing slot's `PxCI` bit set, so recovery **must** stop the engine, clear both write-1-to-clear latches (`PxSERR` then `PxIS`) before re-enabling, and restart — the same invariant as bring-up (never re-arm `ST` while `CR == 1`); mapping a failed/timed-out command onto the `RemoteBlockDevice` restart path (D.2) lets the VFS retry transparently.

**Acceptance:**
- [ ] Host test: `is_fatal(is)` is `true` for any of `PX_IS_TFES`/`IS_HBFS`/`IS_HBDS`/`IS_IFS` and `false` otherwise (`kernel_core::storage::ahci::tests::is_fatal`).
- [ ] Under QEMU, issuing a command for an out-of-range LBA sets `PxTFD.ERR` and `PxIS.TFES`, leaves the slot's `PxCI` bit set, and `recover_port` clears `PxSERR`/`PxIS`, restarts the engine (`CR` reads back 1), and the next valid command succeeds (proves recovery, not a wedged port).
- [ ] A command timeout (no completion within the budget) routes through `recover_port` and surfaces a `BlockDriverError::IoError`/`DriverRestarting`-class result to the facade (D.2), never a hang.

### C.5 — IRQ-on-completion path (`PxIE`/`GHC.IE`/`PxIS`/host-`IS` clear) — polling-primary

**File:** `userspace/drivers/ahci/src/io.rs` (new)
**Symbol:** `arm_interrupts` (set the port's `PxIE` completion + error bits, then `GHC.IE` **last**) + `handle_irq` (read host `IS`, dispatch to the port, read+clear `PxIS` W1C, **then** write 1 to the dispatched port's bit in the HBA-global `IS` register); IRQ subscription via `sys_device_irq_subscribe`; the data path **polls `PxCI`** for completion (QEMU-robust) with the IRQ as a wakeup
**Why it matters:** AHCI completion is reliably observable by polling `PxCI` (QEMU auto-clears the bit on non-NCQ completion), and the smoke gate should prefer polling to avoid coupling to IRQ routing; per AHCI 1.3.1 the IRQ-clear order is **clear `PxIS` first, then W1C the matching port bit in the global `IS`** — otherwise the global interrupt-pending bit latches and (on a level-triggered/INTx path) the line never deasserts and the bare-metal IRQ path wedges; `GHC.IE` must be enabled **last** (after every `PxIE` mask is set and all stale W1C status cleared) or the controller delivers a spurious interrupt immediately. Because the completion path is polling-primary, the IRQ path is hardware-only; Phase 79 found the device-host IRQ allocator forces INTx for Ethernet-class, so any storage-class (`0x01`) INTx fix is recorded as a **bare-metal/VFIO-only** change, out of the 1.0 data path (preserving the "one kernel change" invariant for the shipped phase).

**Acceptance:**
- [ ] Host test: the interrupt-status decoder reports which port fired from the host `IS` bitmap, the `PxIS`-clear value clears the dispatched bits, and the global-`IS` clear value clears the dispatched port's bit (`is_clear(port)`) (`kernel_core::storage::ahci::tests::is_decode`, `pxis_clear`, `host_is_clear`).
- [ ] The data path completes IDENTIFY/read/write by polling `PxCI` under QEMU regardless of IRQ delivery (polling is the authoritative completion path; the smoke gate does not depend on the IRQ).
- [ ] *(Bare-metal/VFIO-only — QEMU storage IRQ routing differs from real HBAs, and this is out of the 1.0 data path.)* With `arm_interrupts` set (`GHC.IE` last) the driver receives a real completion interrupt; `handle_irq` clears `PxIS` then W1C-clears the global `IS` bit (the line deasserts). If the device-host allocator must force INTx for storage-class `0x01` (the Phase 79 fix shape), that change is recorded as a hardware-only addition (it is **not** required for the polling-primary shipped phase).

---

## Track D — `RemoteBlockDevice` facade + partition probe + wiring

### D.1 — Serve the block protocol as `"ahci.block"`

**Files:**
- `userspace/drivers/ahci/src/main.rs` (server loop; model on `userspace/drivers/nvme/src/main.rs::program_main`)
- `kernel-core/src/driver_ipc/block.rs` (reused unchanged)

**Symbol:** `SERVICE_NAME = "ahci.block"`; `ipc_register_service(ep, "ahci.block")`; the server loop decoding `BlkRequestHeader` via `decode_blk_request`, dispatching `BLK_READ`/`BLK_WRITE` to C.3, and replying with `encode_blk_reply` (`BlkReplyHeader { cmd_id, status, bytes }` + read-payload grant); honors `MAX_SECTORS_PER_REQUEST` (256) by chunking
**Why it matters:** the `kernel-core/src/driver_ipc/block.rs` protocol is the exact contract the in-kernel `RemoteBlockDevice` already speaks to NVMe (`"nvme.block"`); registering under `"ahci.block"` and serving the same `BlkRequestHeader`/`BlkReplyHeader` envelope means the driver drops in beside NVMe with no protocol change — and `MAX_SECTORS_PER_REQUEST` chunking respects the 256-sector cap that both the facade and the AHCI single-PRDT path require.

**Acceptance:**
- [ ] The driver registers `ipc_register_service(ep, "ahci.block")` and its server loop round-trips a `BLK_READ`/`BLK_WRITE` through `decode_blk_request` → C.3 → `encode_blk_reply` (host test over the envelope, model on `nvme` `program_main` tests).
- [ ] A request with `sector_count > MAX_SECTORS_PER_REQUEST` is rejected with `BlockDriverError::InvalidRequest` (or chunked), never issued as one oversized command (host-asserted).
- [ ] No bulk data appears inline in any `BlkReply` — read data rides a grant handle, exactly as the NVMe driver does (grep-verifiable: no `Vec<u8>`/`&[u8]` sample field in the reply path).

### D.2 — Kernel cold-path lookup learns `"ahci.block"` (the one data-path kernel change)

**File:** `kernel/src/blk/remote.rs`
**Symbol:** `is_registered()` — extend the cold-path service-registry lookup to try `"ahci.block"` in addition to `"nvme.block"` (same `lookup_endpoint_with_owner` + `/drivers/`-owner trust gate + `VIRTIO_BLK_READY` cold-path gate); the `register`/`mark_driver_ready` restart semantics (`BlockDispatchState`) are reused unchanged
**Why it matters:** the kernel `blk::remote::is_registered()` cold path currently hardcodes a single `"nvme.block"` lookup — an AHCI driver registering `"ahci.block"` would never be discovered without this; it is a **genuine (small) kernel change** on the data path, not free reuse (analogous to Phase 81's `default_route_index_by_link` being a real, scoped kernel addition). The owner-trust gate (`/drivers/` exec-path) and the `VIRTIO_BLK_READY` deferral are preserved so a SATA driver cannot hijack the virtio root disk or be spoofed by an untrusted process. The only *other* possible kernel change in this phase is the C.5 device-host IRQ-allocator INTx path, which is bare-metal/VFIO-only and out of the polling-primary data path.

**Acceptance:**
- [ ] `is_registered()` discovers a ring-3 driver published under **either** `"nvme.block"` or `"ahci.block"` (whichever a trusted `/drivers/` process registered first), preserving the `VIRTIO_BLK_READY` cold-path gate and the untrusted-owner rejection (kernel host test of the dispatch-state path + serial log `auto-registered ring-3 ... driver`).
- [ ] With only the AHCI driver present and `VIRTIO_BLK_READY == false`, the facade routes block I/O through `"ahci.block"`; with virtio-blk active it still defers (no regression to the virtio root path).
- [ ] The `BlockDispatchState` restart/timeout semantics (`DriverRestarting` → bounded wait → retry-once) apply identically to the AHCI driver (host-asserted; the C.4 recovery surfaces `DriverRestarting`/`IoError`).

### D.3 — Boot-time MBR partition probe → VFS mount (no VFS change)

**Files:**
- `userspace/drivers/ahci/src/main.rs` (read sector 0, walk partitions)
- `kernel-core/src/fs/mbr.rs` (reused: `parse_mbr`, `find_ext2_partition`, `find_fat32_partition`)

**Symbol:** at bring-up the driver/facade reads LBA 0 and runs `parse_mbr(&sector0)` → `find_ext2_partition(&entries)` / `find_fat32_partition(&entries)` to surface the partition `(start_lba, sector_count)` the VFS mounts — the same walker NVMe/virtio use; no new kernel VFS code
**Why it matters:** the design doc's value is "a second `RemoteBlockDevice` drops in beside NVMe without disturbing the VFS layer" — the partition walker (`kernel-core/src/fs/mbr.rs`) is already shared, so a SATA disk with an ext2/FAT32 partition mounts through the existing path with **zero VFS changes**, the whole point of the facade reuse.

**Acceptance:**
- [ ] `find_ext2_partition` / `find_fat32_partition` (already host-tested in `kernel-core/src/fs/mbr.rs`) are the partition walkers used; the AHCI path adds no new partition-table code.
- [ ] Under `cargo xtask run --device ahci` (data disk on AHCI), the boot log shows the ext2 partition discovered on the SATA device and the VFS mounting it (the same `find_ext2_partition(...) == Some((start, count))` shape virtio/NVMe produce).
- [ ] No file under `kernel/src/fs/` is modified for the SATA mount (grep-verifiable: the only data-path kernel diff is D.2's `blk/remote.rs` lookup).

### D.4 — Four-place binary wiring (`ahci_driver`) + `ahci_driver.conf`

**Files:**
- `Cargo.toml` (root `members` — add `userspace/drivers/ahci`)
- `xtask/src/main.rs` (`build_userspace` `bins` array + `--features os-binary` map + `populate_ext2_files` service conf)
- `kernel/src/fs/ramdisk.rs` (`generated_initrd_asset!`/`static AHCI_DRIVER_ELF` + the `/drivers/ahci` `DRIVERS_ENTRIES`/`BIN_ENTRIES` tuple)
- `userspace/init/src/main.rs` (`KNOWN_CONFIGS`)
- `kernel/initrd/etc/services.d/ahci_driver.conf` (via `populate_ext2_files`)

**Symbol:** the four AGENTS.md wiring places for a new userspace binary applied to `ahci_driver`; the service conf `name=ahci_driver\ncommand=/drivers/ahci\ntype=daemon\nrestart=on-failure\nmax_restart=5\n` (service `name` is `ahci_driver`; `/drivers/ahci` is the `command=` ramdisk path), matching the Phase 81 `mt792x_driver.conf` precedent where the service name matches the daemon
**Why it matters:** AGENTS.md "Adding a New Userspace Binary" requires **four distinct** wiring places — miss the `bins` array and the driver is never built into the image; miss the ramdisk entry and `execve` returns `ENOENT`; miss the `.conf`/`KNOWN_CONFIGS` and `init` never spawns it. `nvme`/`hda` each appear in all four; the `/drivers/` exec-path prefix is also what D.2's owner-trust gate authorizes. The filename `ahci_driver.conf` is used **byte-identically** in `populate_ext2_files` and `KNOWN_CONFIGS` so the two-place wiring cannot drift.

**Acceptance:**
- [ ] `userspace/drivers/ahci` is added to root `Cargo.toml` `members`.
- [ ] `ahci_driver` is added to the `bins` array in `build_userspace` with `needs_alloc = true` (it uses `alloc`/`kernel-core`) and the `--features os-binary` map.
- [ ] `static AHCI_DRIVER_ELF = generated_initrd_asset!("ahci_driver")` + a `/drivers/ahci` ramdisk tuple are added to `kernel/src/fs/ramdisk.rs`.
- [ ] `ahci_driver.conf` is present (byte-identically) in `populate_ext2_files` **and** `KNOWN_CONFIGS`; after `cargo xtask clean` + boot, `init` logs `init: driver.registered name=ahci_driver` (the daemon spawns).

---

## Track E — xtask integration + `ahci-smoke` gate

### E.1 — `--device ahci` flag + `DeviceSet` + QEMU args

**File:** `xtask/src/main.rs`
**Symbol:** add `ahci: bool` to `DeviceSet`; add `"ahci" => devices.ahci = true` to `apply_device_flag` (the established `--device <name>` convention — there is **no** top-level `--ahci` alias, exactly as there is none for `nvme`/`e1000`); add `ahci` to the `apply_device_flag` "unknown `--device` value (supported: …)" error message **and** the usage string's `--device nvme|e1000|audio|xhci` list; `qemu_args_with_devices_resolved` emits, when `devices.ahci`, `-device ich9-ahci,id=ahci` + `-drive file=<disk>,if=none,id=ahcidisk0,format=raw` + `-device ide-hd,drive=ahcidisk0,bus=ahci.0`, attaching the **data disk** to AHCI instead of virtio-blk (the `--fresh`/`disk.img` path is reused)
**Why it matters:** QEMU's only functional AHCI model is `ich9-ahci` (the canonical name; `-device ahci` is an alias), and a disk is a three-object chain (`-drive if=none` backend + `ich9-ahci` controller + `ide-hd` glued to `bus=ahci.0`) — `if=none` is mandatory so QEMU does not auto-wire the drive to a legacy IDE controller; routing the data disk to AHCI is what lets `cargo xtask run --device ahci` boot m3OS off SATA. The flag rides the existing `--device <name>` parser (`extract_device_flags` → `apply_device_flag`); a standalone `--ahci` would fall through to `remaining` and silently never attach the device, the bug the convention prevents.

**Acceptance:**
- [ ] `apply_device_flag("ahci", ..)` sets `devices.ahci = true`, the `apply_device_flag` error message lists `ahci` among supported values, and the usage string lists `ahci` among `--device` values (no standalone `--ahci` is introduced).
- [ ] `qemu_args_with_devices_resolved` with `devices.ahci` emits exactly `-device ich9-ahci,id=ahci`, `-drive ...if=none,id=ahcidisk0...`, and `-device ide-hd,drive=ahcidisk0,bus=ahci.0` (host-asserted over the produced arg vector, model on the NVMe arg test).
- [ ] `cargo xtask run --device ahci` boots with the data disk on AHCI (virtio-blk replaced for that disk) and the serial smoke run reaches the shell.

### E.2 — `cmd_ahci_smoke` gate (IDENTIFY + write + read-back + flush + IDENTIFY-after-write)

**File:** `xtask/src/main.rs`
**Symbol:** `cmd_ahci_smoke` + `ahci_smoke_steps` + `ahci_smoke_qemu_args` (model on `cmd_hda_smoke`/`cmd_multi_nic_smoke`); injects `-device ich9-ahci` + `-drive if=none` + `-device ide-hd`, asserts `AHCI_SMOKE:server:READY`, then drives IDENTIFY → write a known pattern at an LBA → read it back and byte-compare → FLUSH CACHE EXT → IDENTIFY again, asserting the **binding** per-step serial sentinel set; register the subcommand in the dispatch `match` + help
**Why it matters:** a serial-sentinel gate proves the AHCI driver enumerates, moves data both directions, and flushes for durability in CI — the same way `hda-smoke` proves HDA and `multi-nic-smoke` proves the NICs; the write→read-back byte-compare is the load-bearing "the driver actually transfers data" assertion (a `READY` sentinel alone only proves the process started).

**Acceptance:**
- [ ] `cargo xtask ahci-smoke` boots with `-device ich9-ahci` + `ide-hd`, asserts `AHCI_SMOKE:server:READY`, and asserts the full **binding** five-step sentinel set `AHCI_SMOKE:identify:PASS`, `AHCI_SMOKE:write:PASS`, `AHCI_SMOKE:readback:PASS`, `AHCI_SMOKE:flush:PASS`, `AHCI_SMOKE:identify2:PASS` (the IDENTIFY-after-write step is named, not advisory).
- [ ] The read-back byte-compare of the written LBA pattern is asserted equal (the gate fails if the data path is silently broken).
- [ ] The subcommand is registered in the xtask dispatch `match` and the usage/help string; the gate is added to the AGENTS.md opt-in gate table under `M3OS_AHCI_REGRESSION` (F.3).
- [ ] *(Bare-metal/VFIO-only.)* The BOHC handoff (B.3), COMRESET (B.5), and a real completion interrupt (C.5) are validated on hardware — QEMU's `ich9-ahci` leaves `CAP2.BOH=0` / `CAP.SSS=0` and has no real SATA timing, so the gate prints a skip-with-reason for those (mirroring `wifi-smoke`/`multi-nic-smoke` skips).

---

## Track F — Release closeout

### F.1 — Bump kernel version to `0.82.0`

**Files:**
- `kernel/Cargo.toml` (`version = "0.81.0"` → `"0.82.0"`)
- `AGENTS.md` (`kernel **v0.81.0**` → `**v0.82.0**`; add a Storage/SATA capability bullet **only if** AHCI is a new capability class beside NVMe — per the file's "keep it small" maintenance policy, prefer rewriting the existing storage bullet over adding prose)

**Symbol:** `version` (Cargo manifest) + the AGENTS.md capability-inventory version string
**Why it matters:** the kernel version is the release marker for the phase; the AGENTS.md maintenance policy permits exactly this bump on phase landing (the same form as Phase 79's `0.79.0` and Phase 80's `0.80.0` bumps).

**Acceptance:**
- [ ] `kernel/Cargo.toml` reads `version = "0.82.0"` and `AGENTS.md` reads `kernel **v0.82.0**`; `cargo xtask check` passes.
- [ ] A **scoped** check confirms the kernel release marker no longer reads `0.81.0`: `grep -rn '0\.81\.0' kernel/ kernel-core/ xtask/ --include=*.toml --include=*.rs` returns no live kernel-version or check-list hit. The expected non-kernel matches of a broader grep are the independently-versioned driver-crate manifests (`userspace/drivers/hda/Cargo.toml`, `userspace/drivers/ac97/Cargo.toml` from Phase 80, and the new `userspace/drivers/ahci/Cargo.toml`), which Phase 82 deliberately does not touch; landed phase docs under `docs/roadmap/` legitimately retain prior versions.
- [ ] The AGENTS.md storage capability bullet reflects AHCI/SATA beside NVMe (rewritten, not appended), only if AHCI is a new capability class.

### F.2 — Author `docs/82-ahci-sata.md` learning doc + cross-link

**Files:**
- `docs/82-ahci-sata.md` (new; top-level `docs/`, conforming to the design-doc template sections in `docs/appendix/doc-templates.md`)
- cross-link from `docs/roadmap/82-ahci-sata.md` (and the storage learning doc, if present)

**Symbol:** new learning doc following the design-doc template sections
**Why it matters:** AGENTS.md mandates a learning doc per phase (Phase 79 shipped `docs/79-modern-nic.md`, Phase 80 shipped `docs/80-intel-hda-audio.md`).

**Acceptance:**
- [ ] `docs/82-ahci-sata.md` exists and conforms to the design-doc template sections (the same criterion the design doc's Acceptance imposes on the learning doc).
- [ ] It covers: how AHCI generalizes legacy IDE with a memory-mapped per-port command list + command tables; the FIS protocol (H2D/D2H Register FIS, the `FIS_TYPE_REG_H2D = 0x27` type byte, the C-bit, the received-FIS area); the command-list / command-table / PRDT layout (with the DW0-byte-1/PRDTL-at-offset-2 trap) and the DBC **N−1** encoding; the engine stop/start ordering invariant (`ST`→`CR`, `FRE`→`FR`) and why it is mandatory; the **IOVA-not-physical-address** difference from the Redox `ahcid` reference for every register-programmed address; FLUSH CACHE EXT durability (and why Redox's omission is a data-loss bug); `PxIS.TFES`/`PxSERR` error recovery → engine restart mapped onto the `RemoteBlockDevice` restart path; the AHCI IRQ-clear order (`PxIS` then global `IS`) and why polling is primary on QEMU; the single-queue-per-port (no NCQ) decision and its symmetry with single-queue NVMe; and the QEMU-`ich9-ahci`-vs-bare-metal reality (BOHC/SSS/hot-plug are hardware-only).

### F.3 — Roadmap README row + design-doc reconciliation + gate table + check list

**Files:**
- `docs/roadmap/README.md` (Phase 82 row)
- `docs/roadmap/82-ahci-sata.md`
- `AGENTS.md` (opt-in gate table — add the `M3OS_AHCI_REGRESSION` row; the `cargo xtask check` crate list — add a new host-test crate **only if** Track A introduced a separate crate rather than living in `kernel-core`, which is already in the list)

**Symbol:** README row 82 Status/Tasks cells; design-doc symbol/offset corrections on landing; AGENTS.md gate-table + check-list edits
**Why it matters:** the roadmap README is the canonical status index, and the design doc's register offsets / host-symbol names / file paths / syscall names must match the as-built reality on landing (Phase 79/80 both did this reconciliation pass).

**Acceptance:**
- [ ] On landing, README row 82's Tasks cell flips from "Deferred until implementation planning" to `[Tasks](./tasks/82-ahci-sata-tasks.md)` and the Status reflects the outcome (Complete, or "Deferred post-1.0" if Phase 83 defers it).
- [ ] The design doc's register offsets, opcodes, and file/symbol references (`kernel-core/src/storage/`, `userspace/drivers/ahci/`, `ahci.block`, `ich9-ahci`) match the in-tree reality at landing (no drift).
- [ ] The design doc's drifted claims are reconciled in the same pass: its `B.3` IRQ syscall reference (`sys_device_irq_bind`) is corrected to the real `sys_device_irq_subscribe` / `SYS_DEVICE_IRQ_SUBSCRIBE`; its "no kernel-side changes" claim (Milestone Goal / Feature Scope) is corrected to "one scoped kernel change on the data path (the `blk::remote` cold-path lookup)"; and any `cargo xtask run --ahci` invocation / bare `-device ahci` reference is corrected to `cargo xtask run --device ahci` / `-device ich9-ahci` (noting `ahci` is the QEMU alias for `ich9-ahci`).
- [ ] AGENTS.md gate table lists `ahci-smoke` under `M3OS_AHCI_REGRESSION=1` in the canonical `| Gate | Env var |` row shape, and the `cargo xtask check` crate list names any new host-test crate introduced by Track A (or notes the logic lives in `kernel-core`, already in the list).

---

## Documentation Notes

- **Single queue per port is intrinsic to AHCI, and matches the m3OS storage stance.** AHCI defines exactly one command queue per port (32 slots); m3OS at 1.0 issues one command at a time per port (no NCQ / `PxSACT` overlap), symmetric with the single-queue NVMe decision (Phase 55b). The slot allocator (A.5) deliberately scans `PxSACT | PxCI` rather than `PxCI` alone so the design is forward-compatible with NCQ, but the data-path engine (C.1) waits on a single in-flight slot. NCQ, TRIM/DEALLOCATE, SMART, SECURE ERASE, port multipliers, and hot-plug are all deferred (design-doc "Deferred Until Later"); FLUSH CACHE EXT is **not** deferred — it is in scope (C.3) for write durability.
- **IOVA, never host-physical, for every register-programmed address.** This is the single most important correctness rule and the #1 first-driver bug: `PxCLB`/`PxCLBU`, `PxFB`/`PxFBU`, each command header's `ctba`/`ctbau`, and every PRDT `DBA`/`DBAU` are device DMA addresses the HBA dereferences — under VT-d/AMD-Vi they **must** be the `DmaBuffer::iova()`, not `user_ptr()`. The Redox `ahcid` reference writes `Dma::physical()` because it runs under a flat physical model; m3OS substitutes the IOMMU IOVA exactly as the HDA CORB/RIRB (`CORBLBASE == dma.iova()`) and mt792x descriptor tracks do. B.4/C.2/C.3 each assert the programmed value equals the IOVA and not the VA.
- **The command-header DW0 layout is a silent-corruption trap.** DW0 of `HbaCmdHeader` is a full 32-bit dword: byte 0 (CFL/A/W/P), byte 1 (R/B/C/PMP), then **PRDTL is the high 16 bits at byte offset 2**. Omitting byte 1 lands `prdtl` at offset 1, so the HBA reads the PRDT length from the wrong bytes — and a `size_of::<HbaCmdHeader>() == 32` assert does **not** catch it (trailing reserved padding absorbs the shift). A.3 therefore pins the layout with `offset_of!` asserts (`prdtl == 2`, `prdbc == 4`, `ctba == 8`, `ctbau == 12`), not size alone — the same discipline the IOVA trap demands.
- **The H2D Register FIS type byte is mandatory.** `FIS_TYPE_REG_H2D = 0x27` (and `FIS_TYPE_REG_D2H = 0x34`) live in the A.2 constant table, and every A.4 encoder hard-wires `fis_type = 0x27`. QEMU's `ich9-ahci` and every real HBA validate the FIS type in the command FIS; a zero/wrong type byte makes the HBA reject or ignore the command, so the encoders assert it (A.4).
- **FLUSH CACHE EXT is required for durability — Redox omits it, which is a data-loss bug.** A `WRITE DMA EXT` (`0x35`) completing only guarantees the data reached the drive's volatile write cache, not the platters. The Redox `ahcid` source has no `FLUSH CACHE` and so can lose a "successful" write on power loss; m3OS issues `FLUSH CACHE EXT` (`0xEA`, non-data, `PRDTL=0`, C-bit) on sync/barrier and reports a write durable only after the flush completes without error (C.3). On QEMU this maps to a host `blk_aio_flush()`; true media persistence then depends on the host `-drive cache=` mode, so a strict durability run uses `cache=writethrough`/`directsync`.
- **Engine stop/start ordering is a hard invariant, identical on bring-up and recovery.** Clear `PxCMD.ST` and confirm `PxCMD.CR == 0` **before** clearing `PxCMD.FRE`; confirm `CR == 0` **before** re-setting `ST`; `CR`/`FR` are read-only status the HBA drives. `PxIS` and `PxSERR` are write-1-to-clear and must both be cleared before the engine restarts or interrupts re-enable, or the controller immediately re-interrupts. C.4's `recover_port` follows the same ordering as B.4/B.5, and a failed/timed-out command is mapped onto the `RemoteBlockDevice` restart semantics (`BlockDispatchState` `DriverRestarting` → bounded wait → retry-once) so the VFS retries transparently — the same recovery shape `nvme` already uses.
- **AHCI interrupt-clear order: `PxIS` first, then the global `IS`.** Per AHCI 1.3.1, `handle_irq` (C.5) clears the port's `PxIS` (W1C) **then** writes 1 to the dispatched port's bit in the HBA-global `IS` (offset 0x08). Reversing this latches the global interrupt-pending bit and, on a level-triggered/INTx path, the line never deasserts and the bare-metal IRQ path wedges. `GHC.IE` is enabled **last**, after every `PxIE` mask is set and all stale W1C status is cleared, or the controller delivers a spurious interrupt immediately. On QEMU the completion path is **polling `PxCI`** (the bit auto-clears on non-NCQ completion), so the gate never depends on IRQ delivery.
- **The one genuine data-path kernel change is the `blk::remote` cold-path lookup.** `kernel/src/blk/remote.rs::is_registered()` currently hardcodes a single `"nvme.block"` service lookup; D.2 extends it to also try `"ahci.block"`, preserving the `/drivers/`-owner trust gate and the `VIRTIO_BLK_READY` deferral. This is a real, scoped kernel addition (not free reuse), analogous to Phase 81's `default_route_index_by_link`. The **only** other possible kernel touch in this phase is the device-host IRQ-allocator INTx path for storage-class `0x01` (C.5), which is **bare-metal/VFIO-only** and out of the polling-primary 1.0 data path — so the shipped phase holds the "one kernel change" invariant. Everything else — the `kernel-core/src/driver_ipc/block.rs` protocol, the `BlockDispatchState` restart machinery, the `kernel-core/src/fs/mbr.rs` partition walker, and the VFS mount path — is reused unchanged, which is the whole point of the `RemoteBlockDevice` facade.
- **Only `Sata`-signature ports are driven.** A.6 classifies `PxSIG` into `Sata`/`Satapi`/`PortMultiplier`/`Semb`/`None`/`Unknown`, and `is_driveable` returns `true` only for `Sata`; B.5 logs and skips any non-`Sata` port (`SIG_SEMB`/`SIG_PM`/ATAPI-when-unsupported) so an enclosure-management bridge or port multiplier on a real backplane cannot wedge bring-up. SEMB/PM/ATAPI are otherwise out of 1.0 scope.
- **QEMU `ich9-ahci` reality drives the gate split.** The CI tier (enumerate → reset → IDENTIFY → write/read-compare → flush → induce+recover one TFES) runs against `-device ich9-ahci` + `ide-hd` (PCI class `0x010601`, VID:DID `8086:2922`, CAP NCS=31 / S64A / Gen1, VS `0x00010000`). The hardware/VFIO tier — BIOS/OS handoff (`CAP2.BOH=0` on QEMU), staggered spin-up (`CAP.SSS=0`), COMRESET timing, hot-plug, and real completion-interrupt routing — is skip-with-reason in CI and validated on bare metal, mirroring how `wifi-smoke` is skip-with-reason (no QEMU mt76 model) and the Phase 79 Realtek/igc tracks are hardware-only. A QEMU ordering trap to honor: `PxSIG` reads `0xFFFFFFFF` until `PxCMD.FRE` is enabled and the initial D2H FIS is delivered, so device classification (A.6) must follow FRE (B.5), never precede it.
- **The flag is `--device ahci`, not `--ahci`.** xtask attaches devices via the `--device <name>` convention (`extract_device_flags` → `apply_device_flag`); there is no top-level `--nvme`/`--e1000`/`--ahci` alias, and a standalone `--ahci` would fall through to `remaining` and silently never attach the controller. E.1 adds `"ahci"` to `apply_device_flag`, its error message, and the usage string — never a standalone flag — and every invocation in this plan reads `cargo xtask run --device ahci` (the `cargo xtask ahci-smoke` subcommand is a separate dispatch entry, which is correct).
- **Cross-OS reference.** The bring-up path mirrors Redox OS `ahcid` (a Rust ring-3 microkernel AHCI driver whose `Dma<T>.physical()` is exactly where m3OS substitutes the `DmaBuffer` IOVA) for the per-port command-list/FIS/PRDT structure, slot allocation (`slot()` over `PxSACT|PxCI`), and engine start/stop ordering; Linux `libahci` (`ahci_enable_ahci`, `ahci_reset_controller`, `ahci_save_initial_config`, `ahci_stop_engine`/`ahci_start_engine`, `ahci_dev_classify`) for the precise timeouts, the post-reset `CAP`/`PI` re-read ordering, the `CAP2.BOH` handoff gate, and the `DEF_PORT_IRQ`/`TF_ERR` recovery masks. m3OS departs from both by (a) running fully in ring 3 on the Phase 55b device-host substrate, (b) programming IOMMU IOVAs rather than physical addresses, and (c) issuing `FLUSH CACHE EXT` for durability that Redox skips.
- Line-number references are omitted above where they would drift; the function/symbol names are the durable anchors — locate by symbol (e.g. `program_main`, `qemu_args_with_devices_resolved`, `apply_device_flag`, `DRIVERS_ENTRIES`, `KNOWN_CONFIGS`, `is_registered`), not by line.
- Register offsets, bit values, and ATA opcodes are stated from the Linux canonical `drivers/ata/ahci.h` + `include/linux/ata.h`, the QEMU `hw/ide/ahci-internal.h` model, the Redox `ahcid` `#[repr(C)]` register map, and the AHCI 1.3.1 specification (§10.1.2 software init, §10.4 reset/timeouts, §10.6 BIOS/OS handoff, §5.5 command-list/FIS layout); confirm against the spec section before relying on any single offset during implementation.
- Prefer the exact files/symbols above over directory-level descriptions when implementation begins; update each acceptance checkbox as the corresponding behavior lands.
