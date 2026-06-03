//! Pure-logic AHCI / SATA storage substrate (Phase 82 Track A).
//!
//! This module contains every AHCI/SATA format-level definition that has no
//! hardware dependency, mirroring how [`crate::nvme`] hosts the NVMe register /
//! command / completion layouts and how [`crate::hda`] hosts the HDA verb /
//! `SDnFMT` / BDL math. The kernel is `no_std` and cannot be `cargo test`ed in
//! QEMU, so pinning the register offsets, struct layouts, FIS byte encodings,
//! the PRDT byte-count encoding, the command-slot allocator predicate, and the
//! device-signature classifier here makes every one of them provable by
//! `cargo xtask check` with no QEMU — exactly like `kernel_core::nvme`.
//!
//! * [`ahci`] — Host Bus Adapter (HBA) generic-host-control register offsets
//!   and bit constants, per-port register offsets and bits, the `#[repr(C)]`
//!   command list / command table / PRDT / received-FIS structures with
//!   compile-time size **and offset** asserts, the free-command-slot allocator
//!   over `PxSACT | PxCI`, the device-signature classifier, and the
//!   engine-stop / interrupt-clear / error-recovery decision helpers.
//! * [`ata`] — ATA opcode constants and the H2D Register FIS encoders
//!   (`IDENTIFY`, `READ DMA EXT`, `WRITE DMA EXT`, `FLUSH CACHE EXT`) plus the
//!   `IDENTIFY DEVICE` response parser ([`ata::parse_identify`]).
//!
//! Nothing in this module touches MMIO or DMA. The sole production consumer is
//! `userspace/drivers/ahci/` (the ring-3 driver host process); the host test
//! suites at the bottom of each submodule are the authoritative proof of the
//! bit math. Do **not** delete this module — it is a compile-time dependency of
//! the userspace crate.
//!
//! Register offsets, bit values, and ATA opcodes are stated from the Linux
//! canonical `drivers/ata/ahci.h` + `include/linux/ata.h`, the QEMU
//! `hw/ide/ahci-internal.h` model, the Redox `ahcid` `#[repr(C)]` register map,
//! and the AHCI 1.3.1 specification (§5.5 command-list/FIS layout, §10.1.2
//! software init, §10.4 reset/timeouts, §10.6 BIOS/OS handoff).

pub mod ahci;
pub mod ata;
