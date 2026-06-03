//! AHCI HBA register map, DMA-structure layouts, and pure decision helpers
//! (Phase 82 Track A.2 / A.3 / A.5 / A.6, plus the B.4 / C.4 / C.5 predicates).
//!
//! Every register access in the ring-3 driver is a literal offset/bit; pinning
//! them in one host-tested table (cross-checked against Linux `ahci.h`, the
//! AHCI 1.3.1 spec, and QEMU `ahci-internal.h`) means a transcription slip is a
//! failing test, not a silent register write to the wrong offset.
//!
//! The struct layouts are pinned with compile-time `size_of` **and** `offset_of`
//! asserts: the HBA DMA-reads the command list / command table / PRDT at the
//! IOVAs the driver programs, so a wrong field width, a missing reserved gap,
//! or a mis-placed `PRDTL` silently corrupts the command — and a size-only
//! assert does **not** catch a DW0-byte-1 omission because trailing reserved
//! padding absorbs the shift.

#![allow(dead_code)] // the out-of-tree `ahci` driver crate consumes these.

use core::mem::{offset_of, size_of};

// ===========================================================================
// A.2 — Generic Host Control (HBA) register offsets — AHCI 1.3.1 §3.1
// ===========================================================================

/// Host Capabilities (32-bit, RO).
pub const HBA_CAP: usize = 0x00;
/// Global Host Control (32-bit, RW).
pub const HBA_GHC: usize = 0x04;
/// Interrupt Status — per-port bitmap (32-bit, W1C the dispatched port bit).
pub const HBA_IS: usize = 0x08;
/// Ports Implemented — one bit per implemented port (32-bit, RO).
pub const HBA_PI: usize = 0x0C;
/// AHCI Version (32-bit, RO). QEMU `ich9-ahci` reports `0x0001_0000`.
pub const HBA_VS: usize = 0x10;
/// Host Capabilities Extended (32-bit, RO).
pub const HBA_CAP2: usize = 0x24;
/// BIOS/OS Handoff Control and Status (32-bit, RW1C-style handshake).
pub const HBA_BOHC: usize = 0x28;

// --- GHC bits ---------------------------------------------------------------

/// GHC.AE — AHCI Enable. Must be set before any port register has AHCI
/// semantics. On the QEMU model this bit is read-only-1.
pub const GHC_AE: u32 = 1 << 31;
/// GHC.IE — global Interrupt Enable. Armed **last**, after every `PxIE` mask is
/// set and all stale W1C status is cleared.
pub const GHC_IE: u32 = 1 << 1;
/// GHC.HR — HBA Reset. Self-clears to 0 when the reset completes (≤ 1 s).
pub const GHC_HR: u32 = 1 << 0;

// --- CAP bits ---------------------------------------------------------------

/// CAP.S64A — Supports 64-bit Addressing (the `*U` high-dword registers may
/// carry the IOVA's upper 32 bits).
pub const CAP_S64A: u32 = 1 << 31;
/// CAP.SSS — Supports Staggered Spin-up. `0` on QEMU, so staggered spin-up is a
/// bare-metal/VFIO-only path.
pub const CAP_SSS: u32 = 1 << 27;
/// CAP.SCLO — Supports Command List Override (`PxCMD.CLO` to clear a stuck BSY).
pub const CAP_SCLO: u32 = 1 << 24;
/// CAP.NCS shift — Number of Command Slots field starts at bit 8.
pub const CAP_NCS_SHIFT: u32 = 8;
/// CAP.NCS mask — the field is 5 bits; the slot count is `field + 1`.
pub const CAP_NCS_MASK: u32 = 0x1F;

// --- CAP2 / BOHC bits -------------------------------------------------------

/// CAP2.BOH — BIOS/OS Handoff supported. `0` on QEMU `ich9-ahci`.
pub const CAP2_BOH: u32 = 1 << 0;
/// BOHC.BOS — BIOS Owned Semaphore.
pub const BOHC_BOS: u32 = 1 << 0;
/// BOHC.OOS — OS Owned Semaphore (the OS sets this to request ownership).
pub const BOHC_OOS: u32 = 1 << 1;
/// BOHC.BB — BIOS Busy (firmware is still cleaning up; extend the wait).
pub const BOHC_BB: u32 = 1 << 4;

// --- FIS type bytes (AHCI 1.3.1 §10.3.1) ------------------------------------

/// Register FIS — Host to Device. The mandatory type byte every real HBA and
/// QEMU's `ich9-ahci` validate in the command FIS.
pub const FIS_TYPE_REG_H2D: u8 = 0x27;
/// Register FIS — Device to Host (status delivered into the received-FIS area).
pub const FIS_TYPE_REG_D2H: u8 = 0x34;

// ===========================================================================
// Per-port register offsets — AHCI 1.3.1 §3.3
// ===========================================================================

/// Byte offset of port `n`'s register block: `0x100 + n * 0x80`.
#[inline]
pub const fn port_base(n: usize) -> usize {
    0x100 + n * 0x80
}

/// Port x Command List Base Address (lower 32 bits). Programmed with the
/// `DmaBuffer::iova()` low dword — **never** the host-physical address.
pub const PX_CLB: usize = 0x00;
/// Port x Command List Base Address (upper 32 bits).
pub const PX_CLBU: usize = 0x04;
/// Port x FIS Base Address (lower 32 bits).
pub const PX_FB: usize = 0x08;
/// Port x FIS Base Address (upper 32 bits).
pub const PX_FBU: usize = 0x0C;
/// Port x Interrupt Status (W1C).
pub const PX_IS: usize = 0x10;
/// Port x Interrupt Enable.
pub const PX_IE: usize = 0x14;
/// Port x Command and Status.
pub const PX_CMD: usize = 0x18;
/// Port x Task File Data (BSY/DRQ/ERR live in the low byte).
pub const PX_TFD: usize = 0x20;
/// Port x Signature (device-type signature; valid only after FRE is enabled).
pub const PX_SIG: usize = 0x24;
/// Port x SATA Status (SCR0:SStatus — DET / IPM).
pub const PX_SSTS: usize = 0x28;
/// Port x SATA Control (SCR2:SControl — DET drives COMRESET).
pub const PX_SCTL: usize = 0x2C;
/// Port x SATA Error (SCR1:SError — W1C).
pub const PX_SERR: usize = 0x30;
/// Port x SATA Active (SCR3:SActive — NCQ slot bitmap; scanned for slot alloc).
pub const PX_SACT: usize = 0x34;
/// Port x Command Issue (slot bitmap; the slot bit auto-clears on completion).
pub const PX_CI: usize = 0x38;

// --- PxCMD bits (AHCI 1.3.1 §3.3.7) -----------------------------------------
//
// `CR` and `FR` are **read-only status** the HBA drives; `ST`, `FRE`, `CLO`,
// `SUD`, `POD` are software-writable controls.

/// PxCMD.ST — Start (issue commands from the command list).
pub const CMD_ST: u32 = 1 << 0;
/// PxCMD.SUD — Spin-Up Device (staggered spin-up; meaningful only if CAP.SSS).
pub const CMD_SUD: u32 = 1 << 1;
/// PxCMD.POD — Power On Device.
pub const CMD_POD: u32 = 1 << 2;
/// PxCMD.CLO — Command List Override (clear a stuck BSY/DRQ; needs CAP.SCLO).
pub const CMD_CLO: u32 = 1 << 3;
/// PxCMD.FRE — FIS Receive Enable. Until set, `PxSIG` reads `0xFFFFFFFF`.
pub const CMD_FRE: u32 = 1 << 4;
/// PxCMD.FR — FIS Receive Running (**read-only status**).
pub const CMD_FR: u32 = 1 << 14;
/// PxCMD.CR — Command List Running (**read-only status**).
pub const CMD_CR: u32 = 1 << 15;

// --- PxTFD bits -------------------------------------------------------------

/// Task file Busy.
pub const TFD_BSY: u32 = 0x80;
/// Task file Data Request.
pub const TFD_DRQ: u32 = 0x08;
/// Task file Error.
pub const TFD_ERR: u32 = 0x01;

// --- PxIS / global-IS bits --------------------------------------------------

/// PxIS.DHRS — Device to Host Register FIS Interrupt (a normal completion).
pub const IS_DHRS: u32 = 1 << 0;
/// PxIS.PSS — PIO Setup FIS Interrupt.
pub const IS_PSS: u32 = 1 << 1;
/// PxIS.DSS — DMA Setup FIS Interrupt.
pub const IS_DSS: u32 = 1 << 2;
/// PxIS.SDBS — Set Device Bits FIS Interrupt (NCQ completion).
pub const IS_SDBS: u32 = 1 << 3;
/// PxIS.IFS — Interface Fatal Error Status.
pub const IS_IFS: u32 = 1 << 27;
/// PxIS.HBDS — Host Bus Data Error Status.
pub const IS_HBDS: u32 = 1 << 28;
/// PxIS.HBFS — Host Bus Fatal Error Status.
pub const IS_HBFS: u32 = 1 << 29;
/// PxIS.TFES — Task File Error Status. The HBA halts the engine and leaves the
/// failing slot's `PxCI` bit set; recovery must restart the engine.
pub const PX_IS_TFES: u32 = 1 << 30;

/// The per-port interrupt mask the driver arms in `PxIE`: every normal
/// completion source plus every fatal-error source. Armed before `GHC.IE`.
pub const PORT_INT_MASK: u32 =
    IS_DHRS | IS_PSS | IS_DSS | IS_SDBS | IS_IFS | IS_HBDS | IS_HBFS | PX_IS_TFES;

// --- PxSSTS fields ----------------------------------------------------------

/// SSTS.DET field mask (bits 3:0).
pub const SSTS_DET_MASK: u32 = 0xF;
/// SSTS.DET value meaning "device present and Phy communication established".
pub const SSTS_DET_PRESENT: u32 = 3;
/// SSTS.IPM field shift (bits 11:8).
pub const SSTS_IPM_SHIFT: u32 = 8;
/// SSTS.IPM value meaning "interface active".
pub const SSTS_IPM_ACTIVE: u32 = 1;

/// `true` when `PxSSTS.DET == 3` — a device is present with Phy established.
#[inline]
pub const fn port_present(ssts: u32) -> bool {
    (ssts & SSTS_DET_MASK) == SSTS_DET_PRESENT
}

// ===========================================================================
// A.3 — Command list / command table / PRDT / FIS struct layouts
// ===========================================================================

/// AHCI Command Header — one 32-byte slot of the 1 KiB command list (32 slots).
///
/// DW0 is a full 32-bit dword: byte 0 (`CFL`/`A`/`W`/`P`), byte 1
/// (`R`/`B`/`C`/`PMP`), then **`PRDTL` at byte offset 2**. Omitting byte 1
/// lands `prdtl` at offset 1, so the HBA would read the PRDT length from the
/// wrong bytes — a corruption a `size_of == 32` assert does *not* catch
/// (trailing reserved padding absorbs the shift), which is why the offset
/// asserts below pin every field.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HbaCmdHeader {
    /// DW0 byte 0: `CFL` (bits 4:0, FIS length in dwords), `A` (bit 5, ATAPI),
    /// `W` (bit 6, write), `P` (bit 7, prefetchable).
    pub byte0: u8,
    /// DW0 byte 1: `R` (bit 0, reset), `B` (bit 1, BIST), `C` (bit 2, clear
    /// busy on R_OK), `PMP` (bits 7:4, port-multiplier port).
    pub byte1: u8,
    /// DW0 high half: Physical Region Descriptor Table Length (entry count).
    pub prdtl: u16,
    /// DW1: PRD Byte Count — the HBA writes the transferred byte count here.
    pub prdbc: u32,
    /// DW2: Command Table Base Address (lower 32 bits) — the IOVA low dword.
    pub ctba: u32,
    /// DW3: Command Table Base Address (upper 32 bits) — the IOVA high dword.
    pub ctbau: u32,
    /// DW4..DW7: reserved.
    pub _rsv: [u32; 4],
}

const _: () = assert!(size_of::<HbaCmdHeader>() == 32);
const _: () = assert!(offset_of!(HbaCmdHeader, prdtl) == 2);
const _: () = assert!(offset_of!(HbaCmdHeader, prdbc) == 4);
const _: () = assert!(offset_of!(HbaCmdHeader, ctba) == 8);
const _: () = assert!(offset_of!(HbaCmdHeader, ctbau) == 12);

impl HbaCmdHeader {
    /// Command FIS Length in dwords (DW0 byte 0, bits 4:0).
    #[inline]
    pub const fn cfl(&self) -> u8 {
        self.byte0 & 0x1F
    }

    /// Set the Command FIS Length (low 5 bits of byte 0).
    #[inline]
    pub fn set_cfl(&mut self, dwords: u8) {
        self.byte0 = (self.byte0 & !0x1F) | (dwords & 0x1F);
    }

    /// ATAPI bit (DW0 byte 0, bit 5).
    #[inline]
    pub const fn atapi(&self) -> bool {
        self.byte0 & (1 << 5) != 0
    }

    /// Write bit `W` (DW0 byte 0, bit 6) — set for host→device transfers.
    #[inline]
    pub const fn write(&self) -> bool {
        self.byte0 & (1 << 6) != 0
    }

    /// Set or clear the write bit `W` (DW0 byte 0, bit 6).
    #[inline]
    pub fn set_write(&mut self, write: bool) {
        if write {
            self.byte0 |= 1 << 6;
        } else {
            self.byte0 &= !(1 << 6);
        }
    }

    /// Prefetchable bit `P` (DW0 byte 0, bit 7).
    #[inline]
    pub const fn prefetchable(&self) -> bool {
        self.byte0 & (1 << 7) != 0
    }
}

/// One Physical Region Descriptor Table entry — a 16-byte scatter-gather entry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HbaPrdtEntry {
    /// Data Base Address (lower 32 bits) — the data buffer IOVA low dword.
    pub dba: u32,
    /// Data Base Address (upper 32 bits) — the data buffer IOVA high dword.
    pub dbau: u32,
    /// Reserved.
    pub _rsv: u32,
    /// `DBC` — Data Byte Count (bits 21:0, N−1 encoded) plus `I` interrupt
    /// bit (bit 31). Build with [`encode_dbc`].
    pub dbc: u32,
}

const _: () = assert!(size_of::<HbaPrdtEntry>() == 16);

/// PRDT `DBC` interrupt-on-completion bit.
pub const PRDT_DBC_INTERRUPT: u32 = 1 << 31;
/// PRDT `DBC` byte-count mask (bits 21:0).
pub const PRDT_DBC_COUNT_MASK: u32 = 0x003F_FFFF;

/// Encode a PRDT `DBC` field: the **N−1** byte-count convention (`byte_count -
/// 1`, so the low bit is always set because the transfer length is even) plus
/// the optional interrupt-on-completion bit.
///
/// `debug_assert`-guards `byte_count == 0` (an explicitly forbidden case — a
/// zero-length PRDT entry has no defined meaning).
#[inline]
pub fn encode_dbc(byte_count: u32, interrupt: bool) -> u32 {
    debug_assert!(byte_count > 0, "encode_dbc: zero-length PRDT entry");
    let count = (byte_count - 1) & PRDT_DBC_COUNT_MASK;
    if interrupt {
        count | PRDT_DBC_INTERRUPT
    } else {
        count
    }
}

/// Byte offset at which the PRDT region begins inside a command table:
/// `cfis (64) + acmd (16) + reserved (48)`.
pub const CMD_TABLE_PRDT_OFFSET: usize = 0x80;

/// AHCI Command Table — the per-command CFIS + ATAPI command + PRDT region.
///
/// One PRDT entry inline: the single-PRDT data path caps each command at
/// `MAX_SECTORS_PER_REQUEST` (256) sectors = 128 KiB, far below a single PRDT
/// entry's 4 MiB `DBC` ceiling, so one entry suffices.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HbaCmdTable {
    /// Command FIS (the H2D Register FIS is copied into the first 20 bytes).
    pub cfis: [u8; 64],
    /// ATAPI command (unused for SATA; zeroed).
    pub acmd: [u8; 16],
    /// Reserved.
    pub _rsv: [u8; 48],
    /// PRDT region (begins at byte offset `0x80`).
    pub prdt: [HbaPrdtEntry; 1],
}

const _: () = assert!(offset_of!(HbaCmdTable, prdt) == CMD_TABLE_PRDT_OFFSET);
const _: () = assert!(size_of::<HbaCmdTable>() == 0x80 + 16);

impl Default for HbaCmdTable {
    fn default() -> Self {
        Self {
            cfis: [0; 64],
            acmd: [0; 16],
            _rsv: [0; 48],
            prdt: [HbaPrdtEntry::default(); 1],
        }
    }
}

/// Register FIS — Host to Device (the single command channel to the drive).
///
/// The physical byte order interleaves the LBA bytes around `device`
/// (`lba0..lba2` at offsets 4–6, `device` at 7, `lba3..lba5` at offsets 8–10),
/// exactly as AHCI 1.3.1 §10.3.4 lays it out, so the named-field layout below
/// is hardware-correct on the wire.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FisRegH2D {
    /// Always [`FIS_TYPE_REG_H2D`] (`0x27`).
    pub fis_type: u8,
    /// Port-multiplier port (bits 3:0) and the `C` command bit (bit 7).
    pub pm_c: u8,
    /// ATA command opcode.
    pub command: u8,
    /// Feature register, low byte.
    pub featurel: u8,
    /// LBA bits 7:0.
    pub lba0: u8,
    /// LBA bits 15:8.
    pub lba1: u8,
    /// LBA bits 23:16.
    pub lba2: u8,
    /// Device register (`1 << 6` selects LBA mode).
    pub device: u8,
    /// LBA bits 31:24.
    pub lba3: u8,
    /// LBA bits 39:32.
    pub lba4: u8,
    /// LBA bits 47:40.
    pub lba5: u8,
    /// Feature register, high byte.
    pub featureh: u8,
    /// Sector count, low byte.
    pub countl: u8,
    /// Sector count, high byte.
    pub counth: u8,
    /// Isochronous command completion.
    pub icc: u8,
    /// Control register.
    pub control: u8,
    /// Reserved.
    pub _rsv: [u8; 4],
}

const _: () = assert!(size_of::<FisRegH2D>() == 20);
const _: () = assert!(offset_of!(FisRegH2D, lba0) == 4);
const _: () = assert!(offset_of!(FisRegH2D, device) == 7);
const _: () = assert!(offset_of!(FisRegH2D, lba3) == 8);
const _: () = assert!(offset_of!(FisRegH2D, countl) == 12);

/// The `C` bit in [`FisRegH2D::pm_c`] — set means "this is a command update",
/// not a control update. Every command FIS sets it.
pub const FIS_H2D_C_BIT: u8 = 1 << 7;

/// Register FIS — Device to Host (status delivered into the received-FIS area).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FisRegD2H {
    /// Always [`FIS_TYPE_REG_D2H`] (`0x34`).
    pub fis_type: u8,
    /// Port-multiplier port (bits 3:0) and the `I` interrupt bit (bit 6).
    pub pm_i: u8,
    /// Status register (BSY/DRQ/ERR live here, same bit positions as `PxTFD`).
    pub status: u8,
    /// Error register.
    pub error: u8,
    /// LBA bits 7:0.
    pub lba0: u8,
    /// LBA bits 15:8.
    pub lba1: u8,
    /// LBA bits 23:16.
    pub lba2: u8,
    /// Device register.
    pub device: u8,
    /// LBA bits 31:24.
    pub lba3: u8,
    /// LBA bits 39:32.
    pub lba4: u8,
    /// LBA bits 47:40.
    pub lba5: u8,
    /// Reserved.
    pub _rsv1: u8,
    /// Sector count, low byte.
    pub countl: u8,
    /// Sector count, high byte.
    pub counth: u8,
    /// Reserved.
    pub _rsv2: [u8; 6],
}

const _: () = assert!(size_of::<FisRegD2H>() == 20);

/// Received-FIS area — the 256-byte per-port structure the HBA DMA-writes
/// incoming FIS frames into. `PxFB`/`PxFBU` point at this (by IOVA).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HbaFis {
    /// DMA Setup FIS (offset 0x00).
    pub dsfis: [u8; 28],
    pub _pad0: [u8; 4],
    /// PIO Setup FIS (offset 0x20).
    pub psfis: [u8; 20],
    pub _pad1: [u8; 12],
    /// D2H Register FIS (offset 0x40) — the command-completion status frame.
    pub rfis: [u8; 20],
    pub _pad2: [u8; 4],
    /// Set Device Bits FIS (offset 0x58).
    pub sdbfis: [u8; 8],
    /// Unknown FIS (offset 0x60).
    pub ufis: [u8; 64],
    /// Reserved (offset 0xA0..0x100).
    pub _rsv: [u8; 96],
}

const _: () = assert!(size_of::<HbaFis>() == 256);
const _: () = assert!(offset_of!(HbaFis, psfis) == 0x20);
const _: () = assert!(offset_of!(HbaFis, rfis) == 0x40);
const _: () = assert!(offset_of!(HbaFis, sdbfis) == 0x58);

impl Default for HbaFis {
    fn default() -> Self {
        Self {
            dsfis: [0; 28],
            _pad0: [0; 4],
            psfis: [0; 20],
            _pad1: [0; 12],
            rfis: [0; 20],
            _pad2: [0; 4],
            sdbfis: [0; 8],
            ufis: [0; 64],
            _rsv: [0; 96],
        }
    }
}

// ===========================================================================
// A.5 — free command-slot allocator over `PxSACT | PxCI`
// ===========================================================================

/// Decode the number of command slots from `CAP.NCS`: `((cap >> 8) & 0x1F) + 1`.
#[inline]
pub const fn ncs_from_cap(cap: u32) -> u8 {
    (((cap >> CAP_NCS_SHIFT) & CAP_NCS_MASK) + 1) as u8
}

/// Return the lowest free command slot index `< ncs` — one whose bit is clear in
/// both `PxSACT` and `PxCI` — or `None` if every slot is busy.
///
/// Scanning `sact | ci` (rather than `ci` alone) keeps the allocator
/// forward-compatible with NCQ, which marks active slots in `PxSACT`.
#[inline]
pub fn find_free_slot(sact: u32, ci: u32, ncs: u8) -> Option<u8> {
    let busy = sact | ci;
    let mut slot = 0u8;
    while slot < ncs {
        if busy & (1u32 << slot) == 0 {
            return Some(slot);
        }
        slot += 1;
    }
    None
}

/// `true` when slot `slot`'s command has completed cleanly: its `PxCI` bit is
/// clear (the HBA auto-clears it on completion) **and** no `PxIS` error bit is
/// set. An error latched in `is` makes this `false` even with `PxCI` clear, so a
/// failed command is never reported as success.
#[inline]
pub const fn cmd_complete(ci: u32, slot: u8, is: u32) -> bool {
    let slot_clear = ci & (1u32 << slot) == 0;
    let no_error = is & PX_IS_TFES == 0;
    slot_clear && no_error
}

// ===========================================================================
// A.6 — device-signature classifier
// ===========================================================================

/// What kind of device a port's `PxSIG` signature names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDeviceType {
    /// SATA disk (`SIG_ATA`) — the only type m3OS 1.0 drives.
    Sata,
    /// SATAPI / ATAPI device (`SIG_ATAPI`) — out of 1.0 scope.
    Satapi,
    /// Port multiplier (`SIG_PM`) — out of 1.0 scope.
    PortMultiplier,
    /// Enclosure-management bridge (`SIG_SEMB`) — out of 1.0 scope.
    Semb,
    /// No device present (`PxSSTS.DET != 3`).
    None,
    /// An unrecognized signature value (carried for logging).
    Unknown(u32),
}

/// SATA disk signature.
pub const SIG_ATA: u32 = 0x0000_0101;
/// SATAPI / ATAPI signature.
pub const SIG_ATAPI: u32 = 0xEB14_0101;
/// Port-multiplier signature.
pub const SIG_PM: u32 = 0x9669_0101;
/// Enclosure-management-bridge signature.
pub const SIG_SEMB: u32 = 0xC33C_0101;

/// Classify a raw `PxSIG` value into a [`PortDeviceType`] (presence-agnostic).
#[inline]
pub const fn classify_signature(sig: u32) -> PortDeviceType {
    match sig {
        SIG_ATA => PortDeviceType::Sata,
        SIG_ATAPI => PortDeviceType::Satapi,
        SIG_PM => PortDeviceType::PortMultiplier,
        SIG_SEMB => PortDeviceType::Semb,
        other => PortDeviceType::Unknown(other),
    }
}

/// Classify a port from `(PxSSTS, PxSIG)`. Returns [`PortDeviceType::None`]
/// unless a device is present (`SSTS.DET == 3`); `PxSIG` is only valid after
/// FRE is enabled, so the presence gate keeps a stale `0xFFFFFFFF` signature
/// from being mis-classified.
#[inline]
pub const fn classify_port(ssts: u32, sig: u32) -> PortDeviceType {
    if !port_present(ssts) {
        return PortDeviceType::None;
    }
    classify_signature(sig)
}

/// `true` only for [`PortDeviceType::Sata`] — the gate that keeps an
/// enclosure-management bridge / port multiplier / ATAPI device on a real
/// backplane from wedging bring-up.
#[inline]
pub const fn is_driveable(dt: PortDeviceType) -> bool {
    matches!(dt, PortDeviceType::Sata)
}

// ===========================================================================
// B.4 — engine stop/start ordering predicate
// ===========================================================================

/// `true` only when both `PxCMD.CR` and `PxCMD.FR` are clear — i.e. neither the
/// command-list engine nor the FIS-receive engine is running.
///
/// The cardinal AHCI ordering rule is: clear `ST` and confirm `CR == 0` **before**
/// clearing `FRE`, then confirm `CR == 0` **before** re-setting `ST`. Both
/// `CR`/`FR` are read-only status the HBA drives; reprogramming `PxCLB`/`PxFB`
/// while the engine runs corrupts the command-list pointer.
#[inline]
pub const fn engine_stopped(cmd: u32) -> bool {
    cmd & (CMD_CR | CMD_FR) == 0
}

// ===========================================================================
// B.3 — BIOS/OS handoff gate
// ===========================================================================

/// `true` only when the controller advertises BIOS/OS Handoff (`CAP2.BOH`).
/// QEMU's `ich9-ahci` leaves `CAP2.BOH = 0`, so the handoff handshake is gated
/// off there and is a bare-metal/VFIO-only path.
#[inline]
pub const fn handoff_needed(cap2: u32) -> bool {
    cap2 & CAP2_BOH != 0
}

// ===========================================================================
// C.4 — error-recovery decision helper
// ===========================================================================

/// `true` when any fatal `PxIS` error bit is set (`TFES`/`HBFS`/`HBDS`/`IFS`).
/// On a fatal error the HBA halts the engine and leaves the failing slot's
/// `PxCI` bit set; recovery stops the engine, clears `PxSERR`/`PxIS`, and
/// restarts.
#[inline]
pub const fn is_fatal(is: u32) -> bool {
    is & (PX_IS_TFES | IS_HBFS | IS_HBDS | IS_IFS) != 0
}

// ===========================================================================
// C.5 — interrupt-status decode + W1C-clear helpers (polling-primary)
// ===========================================================================

/// Decode the HBA-global `IS` register: it is already a per-port bitmap (bit `n`
/// = port `n` fired), so decoding is the identity. Provided as a named helper so
/// the IRQ path documents intent rather than poking the raw register.
#[inline]
pub const fn is_decode(is: u32) -> u32 {
    is
}

/// `true` when the HBA-global `IS` bitmap shows port `port` fired.
#[inline]
pub const fn host_is_port_fired(is: u32, port: u8) -> bool {
    is & (1u32 << port) != 0
}

/// The value to W1C-write into the HBA-global `IS` register to clear the
/// dispatched port's pending bit. Per AHCI 1.3.1 this is written **after** the
/// matching `PxIS` is cleared, or the global interrupt-pending bit latches and a
/// level-triggered/INTx line never deasserts.
#[inline]
pub const fn host_is_clear(port: u8) -> u32 {
    1u32 << port
}

/// The W1C value to write back into `PxIS` to clear every status bit that was
/// read set — the standard "read `PxIS`, write it back" idempotent clear.
#[inline]
pub const fn pxis_clear(pxis: u32) -> u32 {
    pxis
}

// ===========================================================================
// B.1 — PCI class match (AHCI is identified by class `0x010601`)
// ===========================================================================

/// PCI base class for a mass-storage controller.
pub const AHCI_PCI_CLASS: u8 = 0x01;
/// PCI subclass for a SATA controller.
pub const AHCI_PCI_SUBCLASS: u8 = 0x06;
/// PCI programming interface for AHCI 1.0 (SATA in AHCI mode). `0x00` would be
/// vendor-specific/IDE-emulation mode, which this driver does not handle.
pub const AHCI_PCI_PROG_IF: u8 = 0x01;
/// The BAR index of the AHCI Base Address Register (ABAR) — always BAR5.
pub const AHCI_ABAR_BAR_INDEX: u8 = 5;

/// `true` for an AHCI-mode SATA controller: class `0x01` / subclass `0x06` /
/// prog-IF `0x01`. Deliberately a 3-arg, class-only predicate — AHCI matches
/// purely on class `0x010601`, with no vendor/device-ID dependency (gating on a
/// vendor ID would miss most controllers). The IDE-emulation prog-IF `0x00` is
/// rejected because this driver only drives AHCI mode.
#[inline]
pub const fn ahci_pci_match(class: u8, subclass: u8, prog_if: u8) -> bool {
    class == AHCI_PCI_CLASS && subclass == AHCI_PCI_SUBCLASS && prog_if == AHCI_PCI_PROG_IF
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- A.2 register offsets / bits ---------------------------------------

    #[test]
    fn register_offsets() {
        assert_eq!(HBA_CAP, 0x00);
        assert_eq!(HBA_GHC, 0x04);
        assert_eq!(HBA_IS, 0x08);
        assert_eq!(HBA_PI, 0x0C);
        assert_eq!(HBA_VS, 0x10);
        assert_eq!(HBA_CAP2, 0x24);
        assert_eq!(HBA_BOHC, 0x28);
        assert_eq!(port_base(0), 0x100);
        assert_eq!(port_base(1), 0x180);
        assert_eq!(port_base(5), 0x380);
        // Per-port offsets.
        assert_eq!(PX_CLB, 0x00);
        assert_eq!(PX_CLBU, 0x04);
        assert_eq!(PX_FB, 0x08);
        assert_eq!(PX_FBU, 0x0C);
        assert_eq!(PX_IS, 0x10);
        assert_eq!(PX_IE, 0x14);
        assert_eq!(PX_CMD, 0x18);
        assert_eq!(PX_TFD, 0x20);
        assert_eq!(PX_SIG, 0x24);
        assert_eq!(PX_SSTS, 0x28);
        assert_eq!(PX_SCTL, 0x2C);
        assert_eq!(PX_SERR, 0x30);
        assert_eq!(PX_SACT, 0x34);
        assert_eq!(PX_CI, 0x38);
    }

    #[test]
    fn cmd_bits() {
        assert_eq!(CMD_ST, 0x1);
        assert_eq!(CMD_SUD, 0x2);
        assert_eq!(CMD_POD, 0x4);
        assert_eq!(CMD_CLO, 0x8);
        assert_eq!(CMD_FRE, 0x10);
        assert_eq!(CMD_FR, 0x4000);
        assert_eq!(CMD_CR, 0x8000);
        // CR/FR are the read-only status bits the engine_stopped predicate
        // watches.
        assert!(engine_stopped(0));
        assert!(!engine_stopped(CMD_CR));
        assert!(!engine_stopped(CMD_FR));
        assert!(!engine_stopped(CMD_CR | CMD_FR));
        // Software-writable bits do not affect the stop predicate.
        assert!(engine_stopped(CMD_ST | CMD_FRE));
    }

    #[test]
    fn ghc_bits() {
        assert_eq!(GHC_AE, 1 << 31);
        assert_eq!(GHC_HR, 1 << 0);
        assert_eq!(GHC_IE, 1 << 1);
        assert_eq!(CAP2_BOH, 1 << 0);
        assert_eq!(CAP_S64A, 1 << 31);
        assert_eq!(CAP_SSS, 1 << 27);
        assert_eq!(CAP_SCLO, 1 << 24);
    }

    #[test]
    fn fis_type_bytes() {
        assert_eq!(FIS_TYPE_REG_H2D, 0x27);
        assert_eq!(FIS_TYPE_REG_D2H, 0x34);
    }

    #[test]
    fn ssts_present() {
        assert_eq!(PX_IS_TFES, 1 << 30);
        assert_eq!(SSTS_DET_PRESENT, 3);
        // The QEMU device-present value: DET=3, SPD=1, IPM=1.
        let ssts = 0x113;
        assert_eq!(ssts & SSTS_DET_MASK, 3);
        assert_eq!((ssts >> SSTS_IPM_SHIFT) & 0xF, SSTS_IPM_ACTIVE);
        assert!(port_present(ssts));
        // No device: DET=0.
        assert!(!port_present(0x000));
        // DET=1 (presence detected but no Phy) is not "present" for our purpose.
        assert!(!port_present(0x001));
    }

    // --- A.3 struct layouts -------------------------------------------------

    #[test]
    fn struct_sizes_are_pinned() {
        assert_eq!(size_of::<HbaCmdHeader>(), 32);
        assert_eq!(size_of::<HbaPrdtEntry>(), 16);
        assert_eq!(size_of::<FisRegH2D>(), 20);
        assert_eq!(size_of::<HbaFis>(), 256);
        // 32 command headers × 32 B = the 1 KiB command list.
        assert_eq!(size_of::<HbaCmdHeader>() * 32, 1024);
    }

    #[test]
    fn cmd_header_offsets() {
        assert_eq!(offset_of!(HbaCmdHeader, prdtl), 2);
        assert_eq!(offset_of!(HbaCmdHeader, prdbc), 4);
        assert_eq!(offset_of!(HbaCmdHeader, ctba), 8);
        assert_eq!(offset_of!(HbaCmdHeader, ctbau), 12);
    }

    #[test]
    fn prdt_dbc_n_minus_1() {
        // 8 sectors × 512 B, no interrupt → byte_count - 1.
        assert_eq!(encode_dbc(8 * 512, false), 8 * 512 - 1);
        // 512 B with the interrupt bit set.
        assert_eq!(encode_dbc(512, true), 511 | (1 << 31));
        // The encoded count is always odd (low bit set) because transfer
        // lengths are even.
        assert_eq!(encode_dbc(512, false) & 1, 1);
        assert_eq!(encode_dbc(128 * 1024, false) & 1, 1);
    }

    #[test]
    #[should_panic(expected = "zero-length PRDT entry")]
    fn prdt_dbc_rejects_zero() {
        // Zero-length is a debug-asserted programming error.
        let _ = encode_dbc(0, false);
    }

    #[test]
    fn cmd_table_layout() {
        assert_eq!(offset_of!(HbaCmdTable, prdt), 0x80);
        assert_eq!(CMD_TABLE_PRDT_OFFSET, 0x80);
        // cfis 64 + acmd 16 + reserved 48 = 128 = 0x80.
        assert_eq!(64 + 16 + 48, 0x80);
    }

    #[test]
    fn cmd_header_bitfields() {
        let mut h = HbaCmdHeader::default();
        h.set_cfl(5);
        assert_eq!(h.cfl(), 5);
        // cfl is the low 5 bits of byte 0.
        assert_eq!(h.byte0 & 0x1F, 5);
        assert!(!h.write());
        h.set_write(true);
        assert!(h.write());
        // The write bit is bit 6 of byte 0.
        assert_eq!(h.byte0 & (1 << 6), 1 << 6);
        // Setting write did not disturb cfl.
        assert_eq!(h.cfl(), 5);
        h.set_write(false);
        assert!(!h.write());
        assert_eq!(h.cfl(), 5);
    }

    #[test]
    fn fis_h2d_field_offsets() {
        // The named-field layout must be hardware-correct: LBA bytes interleave
        // around the device register.
        assert_eq!(offset_of!(FisRegH2D, fis_type), 0);
        assert_eq!(offset_of!(FisRegH2D, command), 2);
        assert_eq!(offset_of!(FisRegH2D, lba0), 4);
        assert_eq!(offset_of!(FisRegH2D, device), 7);
        assert_eq!(offset_of!(FisRegH2D, lba3), 8);
        assert_eq!(offset_of!(FisRegH2D, countl), 12);
    }

    // --- A.5 slot allocator -------------------------------------------------

    #[test]
    fn ncs_from_cap() {
        // QEMU `ich9-ahci`: NCS field == 31 → 32 slots.
        let qemu_cap = (31u32 << CAP_NCS_SHIFT) | CAP_S64A;
        assert_eq!(super::ncs_from_cap(qemu_cap), 32);
        // NCS field == 0 → 1 slot.
        assert_eq!(super::ncs_from_cap(0), 1);
        // NCS field == 7 → 8 slots.
        assert_eq!(super::ncs_from_cap(7 << CAP_NCS_SHIFT), 8);
    }

    #[test]
    fn find_free_slot() {
        // All free → slot 0.
        assert_eq!(super::find_free_slot(0, 0, 32), Some(0));
        // Slots 0-2 busy in CI → slot 3.
        assert_eq!(super::find_free_slot(0, 0b0000_0111, 32), Some(3));
        // Busy via SACT as well as CI (NCQ forward-compat).
        assert_eq!(super::find_free_slot(0b0000_0001, 0b0000_0110, 32), Some(3));
        // All ncs slots busy → None.
        assert_eq!(super::find_free_slot(0xFFFF_FFFF, 0, 32), None);
        // A free bit beyond ncs is never returned: ncs=4, slots 0-3 busy.
        assert_eq!(super::find_free_slot(0, 0b0000_1111, 4), None);
        // ncs=4 with slot 2 free.
        assert_eq!(super::find_free_slot(0, 0b0000_1011, 4), Some(2));
    }

    #[test]
    fn cmd_complete_requires_no_error() {
        // Slot 0 clear in CI, no error → complete.
        assert!(cmd_complete(0, 0, 0));
        // Slot 0 still set in CI → not complete.
        assert!(!cmd_complete(0b1, 0, 0));
        // Slot 3 clear but a TFES error latched → not complete.
        assert!(!cmd_complete(0, 3, PX_IS_TFES));
        // Slot 3 clear, a non-fatal completion bit set → still complete.
        assert!(cmd_complete(0, 3, IS_DHRS));
    }

    // --- A.6 signature classifier ------------------------------------------

    #[test]
    fn classify_signature() {
        assert_eq!(super::classify_signature(SIG_ATA), PortDeviceType::Sata);
        assert_eq!(super::classify_signature(SIG_ATAPI), PortDeviceType::Satapi);
        assert_eq!(
            super::classify_signature(SIG_PM),
            PortDeviceType::PortMultiplier
        );
        assert_eq!(super::classify_signature(SIG_SEMB), PortDeviceType::Semb);
        assert_eq!(
            super::classify_signature(0xFFFF_FFFF),
            PortDeviceType::Unknown(0xFFFF_FFFF)
        );
    }

    #[test]
    fn classify_port_requires_present() {
        // DET=3 present + ATA sig → Sata.
        assert_eq!(classify_port(0x113, SIG_ATA), PortDeviceType::Sata);
        // No device → None even with an ATA signature lingering.
        assert_eq!(classify_port(0x000, SIG_ATA), PortDeviceType::None);
        // Present + a stale 0xFFFFFFFF signature → Unknown (caught by driver).
        assert_eq!(
            classify_port(0x113, 0xFFFF_FFFF),
            PortDeviceType::Unknown(0xFFFF_FFFF)
        );
    }

    #[test]
    fn only_sata_is_driveable() {
        assert!(is_driveable(PortDeviceType::Sata));
        assert!(!is_driveable(PortDeviceType::Satapi));
        assert!(!is_driveable(PortDeviceType::PortMultiplier));
        assert!(!is_driveable(PortDeviceType::Semb));
        assert!(!is_driveable(PortDeviceType::None));
        assert!(!is_driveable(PortDeviceType::Unknown(0x1234)));
    }

    // --- B.4 engine stop ordering ------------------------------------------

    #[test]
    fn engine_stop_ordering() {
        // Both CR and FR clear → stopped.
        assert!(engine_stopped(0));
        // CR set (command engine running) → not stopped: must wait CR→0 before
        // clearing FRE.
        assert!(!engine_stopped(CMD_CR));
        // FR set (FIS-receive engine running) → not stopped.
        assert!(!engine_stopped(CMD_FR));
        assert!(!engine_stopped(CMD_CR | CMD_FR));
    }

    // --- B.3 handoff gate ---------------------------------------------------

    #[test]
    fn handoff_needed() {
        // CAP2.BOH set → handoff required.
        assert!(super::handoff_needed(CAP2_BOH));
        // QEMU cap2 == 0 → no handoff.
        assert!(!super::handoff_needed(0));
        // Other CAP2 bits set but not BOH → no handoff.
        assert!(!super::handoff_needed(0xFFFF_FFFE));
    }

    // --- C.4 is_fatal -------------------------------------------------------

    #[test]
    fn is_fatal() {
        assert!(super::is_fatal(PX_IS_TFES));
        assert!(super::is_fatal(IS_HBFS));
        assert!(super::is_fatal(IS_HBDS));
        assert!(super::is_fatal(IS_IFS));
        assert!(super::is_fatal(PX_IS_TFES | IS_DHRS));
        // Normal completion bits are not fatal.
        assert!(!super::is_fatal(0));
        assert!(!super::is_fatal(IS_DHRS));
        assert!(!super::is_fatal(IS_PSS | IS_DSS | IS_SDBS));
    }

    // --- C.5 interrupt decode + W1C clears ---------------------------------

    #[test]
    fn is_decode() {
        // The global IS is a per-port bitmap.
        let is = 0b0000_0100; // port 2 fired
        assert_eq!(super::is_decode(is), is);
        assert!(host_is_port_fired(is, 2));
        assert!(!host_is_port_fired(is, 0));
        assert!(!host_is_port_fired(is, 1));
        // Multiple ports.
        let multi = 0b0000_1001; // ports 0 and 3
        assert!(host_is_port_fired(multi, 0));
        assert!(host_is_port_fired(multi, 3));
        assert!(!host_is_port_fired(multi, 1));
    }

    #[test]
    fn pxis_clear() {
        // PxIS is W1C: the clear value echoes the read value.
        assert_eq!(super::pxis_clear(IS_DHRS), IS_DHRS);
        assert_eq!(
            super::pxis_clear(IS_DHRS | PX_IS_TFES),
            IS_DHRS | PX_IS_TFES
        );
        assert_eq!(super::pxis_clear(0), 0);
    }

    #[test]
    fn host_is_clear() {
        // The global-IS clear value sets only the dispatched port's bit.
        assert_eq!(super::host_is_clear(0), 0b1);
        assert_eq!(super::host_is_clear(2), 0b100);
        assert_eq!(super::host_is_clear(5), 1 << 5);
    }

    // --- B.1 PCI match ------------------------------------------------------

    #[test]
    fn pci_match() {
        // AHCI-mode SATA.
        assert!(ahci_pci_match(0x01, 0x06, 0x01));
        // NVMe is class 0x01 / subclass 0x08 / prog-IF 0x02 — rejected.
        assert!(!ahci_pci_match(0x01, 0x08, 0x02));
        // SATA in IDE-emulation mode (prog-IF 0x00) — rejected (out of scope).
        assert!(!ahci_pci_match(0x01, 0x06, 0x00));
        // Ethernet — rejected.
        assert!(!ahci_pci_match(0x02, 0x00, 0x00));
        // The ABAR is BAR5.
        assert_eq!(AHCI_ABAR_BAR_INDEX, 5);
    }
}
