//! Pure-logic NVMe register, command, and completion layouts (Phase 55 D.0).
//!
//! This module contains every NVMe format-level definition that has no
//! hardware dependency:
//!
//! * [`NvmeRegs`] — byte offsets into the controller's BAR0 MMIO region for
//!   the mandatory admin-register set (`CAP`, `VS`, `CC`, `CSTS`, `AQA`,
//!   `ASQ`, `ACQ`) plus the [`doorbell_offset`](NvmeRegs::doorbell_offset)
//!   helper that applies the `CAP.DSTRD` stride.
//! * [`NvmeCommand`] — the 64-byte submission queue entry used for both
//!   admin and I/O commands.
//! * [`NvmeCompletion`] — the 16-byte completion queue entry with status
//!   phase-bit plus status-code accessors ([`completion_phase`],
//!   [`completion_status_code`]).
//! * [`NvmeCap`] — wrapper around the 64-bit Capabilities register with
//!   typed accessors for `MQES`, `CQR`, `DSTRD`, `TO`, and `CSS.NVM`.
//! * Opcode constants for admin (`OP_IDENTIFY`, `OP_CREATE_IO_CQ`,
//!   `OP_CREATE_IO_SQ`) and I/O (`OP_IO_READ`, `OP_IO_WRITE`) commands.
//!
//! Nothing in this module touches MMIO or DMA; the kernel-side driver in
//! `kernel/src/blk/nvme.rs` wraps these definitions with register pokes and
//! `DmaBuffer` allocations.
//!
//! Host-testable via `cargo test -p kernel-core --target
//! x86_64-unknown-linux-gnu nvme::` — see the test module at the bottom.

// ---------------------------------------------------------------------------
// Register offsets
// ---------------------------------------------------------------------------

/// NVMe controller register offsets into BAR0. All values follow the NVMe
/// Base Specification Revision 1.4 §3.1 "Controller Registers".
pub struct NvmeRegs;

impl NvmeRegs {
    /// Controller Capabilities (64-bit, RO). Queue limits, doorbell stride,
    /// command-set support, and reset timeout live here.
    pub const CAP: usize = 0x00;
    /// Version (32-bit, RO). MMMm0000 encoding: major in bits 31:16, minor in
    /// 15:8, tertiary in 7:0.
    pub const VS: usize = 0x08;
    /// Interrupt Mask Set (32-bit, WO).
    pub const INTMS: usize = 0x0C;
    /// Interrupt Mask Clear (32-bit, WO).
    pub const INTMC: usize = 0x10;
    /// Controller Configuration (32-bit, RW). Enable bit, I/O queue entry
    /// sizes, and arbitration live here.
    pub const CC: usize = 0x14;
    /// Controller Status (32-bit, RO). `RDY` bit 0 indicates enable complete.
    pub const CSTS: usize = 0x1C;
    /// Admin Queue Attributes (32-bit, RW). Queue depth encoded as `size -
    /// 1` in both halves (ASQS in 15:0, ACQS in 27:16).
    pub const AQA: usize = 0x24;
    /// Admin Submission Queue Base Address (64-bit, RW). Must be
    /// page-aligned.
    pub const ASQ: usize = 0x28;
    /// Admin Completion Queue Base Address (64-bit, RW). Must be
    /// page-aligned.
    pub const ACQ: usize = 0x30;

    /// Base of the doorbell range. Per-queue doorbells live at `DOORBELL +
    /// (2 * queue_id + is_completion) * (4 << CAP.DSTRD)`.
    pub const DOORBELL_BASE: usize = 0x1000;

    /// Compute the byte offset of a doorbell register for queue `queue_id`.
    ///
    /// `is_completion` selects between submission-tail (false) and
    /// completion-head (true) doorbells. `doorbell_stride_bytes` is the
    /// decoded `CAP.DSTRD` stride in bytes (minimum 4).
    pub const fn doorbell_offset(
        queue_id: u16,
        is_completion: bool,
        doorbell_stride_bytes: usize,
    ) -> usize {
        // 2 doorbells per queue pair: submission tail, completion head.
        let slot = (queue_id as usize) * 2 + if is_completion { 1 } else { 0 };
        Self::DOORBELL_BASE + slot * doorbell_stride_bytes
    }
}

// ---------------------------------------------------------------------------
// Controller Configuration (CC) bits — spec §3.1.5
// ---------------------------------------------------------------------------

/// CC.EN — enables the controller (bit 0).
pub const CC_EN: u32 = 1 << 0;
/// CC.CSS shift (bits 6:4). NVM Command Set is value 0.
pub const CC_CSS_SHIFT: u32 = 4;
/// CC.MPS shift (bits 10:7). Memory page size = `2^(12 + MPS)`.
pub const CC_MPS_SHIFT: u32 = 7;
/// CC.AMS shift (bits 13:11). Round-robin is value 0.
pub const CC_AMS_SHIFT: u32 = 11;
/// CC.SHN shift (bits 15:14). 0 = no shutdown notification.
pub const CC_SHN_SHIFT: u32 = 14;
/// CC.IOSQES shift (bits 19:16). Submission entry size = `2^N` bytes, N=6 for
/// the 64-byte `NvmeCommand`.
pub const CC_IOSQES_SHIFT: u32 = 16;
/// CC.IOCQES shift (bits 23:20). Completion entry size = `2^N` bytes, N=4 for
/// the 16-byte `NvmeCompletion`.
pub const CC_IOCQES_SHIFT: u32 = 20;

/// CSTS.RDY — controller ready (bit 0).
pub const CSTS_RDY: u32 = 1 << 0;
/// CSTS.CFS — controller fatal status (bit 1).
pub const CSTS_CFS: u32 = 1 << 1;

// ---------------------------------------------------------------------------
// Command-set opcodes
// ---------------------------------------------------------------------------

/// Admin Identify (CNS selects controller vs. namespace vs. list).
pub const OP_IDENTIFY: u8 = 0x06;
/// Admin Create I/O Completion Queue.
pub const OP_CREATE_IO_CQ: u8 = 0x05;
/// Admin Create I/O Submission Queue.
pub const OP_CREATE_IO_SQ: u8 = 0x01;

/// I/O Read (NVM command set).
pub const OP_IO_READ: u8 = 0x02;
/// I/O Write (NVM command set).
pub const OP_IO_WRITE: u8 = 0x01;

/// Identify CNS: namespace structure.
pub const IDENTIFY_CNS_NAMESPACE: u32 = 0x00;
/// Identify CNS: controller structure.
pub const IDENTIFY_CNS_CONTROLLER: u32 = 0x01;

// ---------------------------------------------------------------------------
// NvmeCommand — submission queue entry (64 bytes)
// ---------------------------------------------------------------------------

/// Submission Queue Entry — NVMe base spec §4.2 "Submission Queue Entry -
/// Command Format".
///
/// Every admin and I/O command starts with this 64-byte layout. The kernel
/// driver writes it into a DMA-resident ring page; the device reads it via
/// its own bus master.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NvmeCommand {
    /// CDW0: opcode (bits 7:0), fused (9:8), reserved (13:10), PSDT (15:14),
    /// command identifier (31:16). [`NvmeCommand::new`] composes this field
    /// from `opcode` + `cid`.
    pub cdw0: u32,
    /// Namespace identifier. Zero for admin commands that don't target a
    /// namespace (Identify Controller, Create I/O Queue).
    pub nsid: u32,
    /// CDW2 — reserved in most admin commands.
    pub cdw2: u32,
    /// CDW3 — reserved in most admin commands.
    pub cdw3: u32,
    /// Metadata pointer (rarely used — SGL / metadata buffer).
    pub mptr: u64,
    /// Physical Region Page 1 — first data-buffer physical address.
    pub prp1: u64,
    /// Physical Region Page 2 — second page, or pointer to PRP list if the
    /// transfer spans more than two pages.
    pub prp2: u64,
    /// Command-specific dword 10.
    pub cdw10: u32,
    /// Command-specific dword 11.
    pub cdw11: u32,
    /// Command-specific dword 12.
    pub cdw12: u32,
    /// Command-specific dword 13.
    pub cdw13: u32,
    /// Command-specific dword 14.
    pub cdw14: u32,
    /// Command-specific dword 15.
    pub cdw15: u32,
}

// Compile-time guard: NvmeCommand must be exactly 64 bytes (NVMe spec
// §4.2). A mismatch would violate hardware expectations and shift every
// SQ entry by the wrong offset.
const _: () = assert!(core::mem::size_of::<NvmeCommand>() == 64);

impl NvmeCommand {
    /// Construct an empty command with the opcode and command identifier
    /// pre-filled into CDW0.
    ///
    /// Command IDs are driver-chosen and returned verbatim in the matching
    /// completion entry. Callers typically set further fields (NSID, PRP1,
    /// CDW10..15) on the returned struct before writing it to the SQ.
    pub const fn new(opcode: u8, cid: u16) -> Self {
        let cdw0 = (opcode as u32) | ((cid as u32) << 16);
        Self {
            cdw0,
            nsid: 0,
            cdw2: 0,
            cdw3: 0,
            mptr: 0,
            prp1: 0,
            prp2: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
        }
    }

    /// Opcode field (CDW0 bits 7:0).
    pub const fn opcode(&self) -> u8 {
        (self.cdw0 & 0xFF) as u8
    }

    /// Command identifier (CDW0 bits 31:16).
    pub const fn cid(&self) -> u16 {
        ((self.cdw0 >> 16) & 0xFFFF) as u16
    }
}

impl Default for NvmeCommand {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

// ---------------------------------------------------------------------------
// NvmeCompletion — completion queue entry (16 bytes)
// ---------------------------------------------------------------------------

/// Completion Queue Entry — NVMe base spec §4.6.
///
/// The device writes these to the host-resident CQ and raises the assigned
/// interrupt. The host walks the CQ by watching the phase bit in
/// `status_phase` flip with every lap around the ring.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NvmeCompletion {
    /// DW0: command-specific result. For Identify this is unused; for
    /// Create I/O CQ/SQ it is unused as well.
    pub result: u32,
    /// DW1: reserved.
    pub reserved: u32,
    /// DW2 low 16 bits: SQ head pointer.
    pub sq_head: u16,
    /// DW2 high 16 bits: SQ identifier.
    pub sq_id: u16,
    /// DW3 low 16 bits: command identifier echoed from the SQ entry.
    pub cid: u16,
    /// DW3 high 16 bits: phase tag (bit 0) + status (bits 15:1). Use
    /// [`completion_phase`] / [`completion_status_code`] to decode.
    pub status_phase: u16,
}

// Compile-time guard: NvmeCompletion must be exactly 16 bytes (NVMe spec
// §4.6).
const _: () = assert!(core::mem::size_of::<NvmeCompletion>() == 16);

/// Extract the phase tag from a CQ entry — flips each lap of the ring so the
/// host can tell "new entry" from "stale entry left from last pass".
pub const fn completion_phase(entry: &NvmeCompletion) -> bool {
    entry.status_phase & 0x1 != 0
}

/// Extract the 15-bit status field (bits 15:1) from a CQ entry. Zero means
/// success; non-zero values decompose into Status Code Type + Status Code
/// per NVMe spec §4.6.1.
pub const fn completion_status_code(entry: &NvmeCompletion) -> u16 {
    entry.status_phase >> 1
}

// ---------------------------------------------------------------------------
// NvmeCap — Capabilities register accessors (64-bit)
// ---------------------------------------------------------------------------

/// Controller Capabilities register. Parsed from the 64-bit read at `CAP`
/// (offset 0x00). Field layout per NVMe spec §3.1.1:
///
/// * MQES (15:0) — max queue entries supported (returned value is `size - 1`).
/// * CQR (16) — contiguous queues required.
/// * AMS (18:17) — arbitration mechanism support.
/// * TO (31:24) — worst-case RDY transition time in 500 ms units.
/// * DSTRD (35:32) — doorbell stride (`4 << DSTRD` bytes).
/// * NSSRS (36) — NVM subsystem reset supported.
/// * CSS (44:37) — command sets supported (bit 0 = NVM).
/// * BPS (45) — boot partitions supported.
/// * MPSMIN (51:48) — min memory page size (`2^(12 + MPSMIN)`).
/// * MPSMAX (55:52) — max memory page size.
///
/// The fields the driver actually depends on are exposed as methods; future
/// additions can follow the same pattern.
#[derive(Debug, Clone, Copy)]
pub struct NvmeCap(pub u64);

impl NvmeCap {
    /// Max queue entries supported (decoded — already added 1 to the raw
    /// field, so `mqes() == 256` means the controller supports a 256-entry
    /// queue).
    pub const fn mqes(&self) -> u16 {
        let raw = (self.0 & 0xFFFF) as u16;
        // Spec: the register holds `size - 1`. Treat raw == 0 as 1 entry
        // (spec prohibits 0-entry queues but be conservative).
        raw.wrapping_add(1)
    }

    /// Contiguous Queues Required: true means host memory for queues must be
    /// contiguous. Always true on real hardware; QEMU tolerates
    /// non-contiguous but we always satisfy this anyway via buddy
    /// allocation.
    pub const fn cqr(&self) -> bool {
        (self.0 >> 16) & 1 != 0
    }

    /// Doorbell stride in bytes. Encoded as `4 << DSTRD`.
    pub const fn doorbell_stride(&self) -> usize {
        let dstrd = ((self.0 >> 32) & 0xF) as usize;
        4usize << dstrd
    }

    /// Timeout value: worst case time from CC.EN transition to CSTS.RDY
    /// matching, expressed in 500 ms units. Zero means "use implementation
    /// default", which we treat as 1 unit.
    pub const fn timeout_500ms_units(&self) -> u8 {
        ((self.0 >> 24) & 0xFF) as u8
    }

    /// NVM Command Set supported (CSS bit 0).
    pub const fn css_nvme(&self) -> bool {
        (self.0 >> 37) & 1 != 0
    }

    /// Memory Page Size Minimum in bytes: `2^(12 + MPSMIN)`.
    pub const fn min_memory_page_bytes(&self) -> u32 {
        let mpsmin = ((self.0 >> 48) & 0xF) as u32;
        1u32 << (12 + mpsmin)
    }
}

// ---------------------------------------------------------------------------
// Tests — acceptance D.0 bullet 7 (at least 3 host tests).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // (1) Command construction: builds a 64-byte command with expected
    //     opcode/CID/NSID/PRP layout.
    #[test]
    fn command_construction_has_correct_layout() {
        assert_eq!(core::mem::size_of::<NvmeCommand>(), 64);
        let mut cmd = NvmeCommand::new(OP_IDENTIFY, 0x1234);
        cmd.nsid = 0xAABBCCDD;
        cmd.prp1 = 0x1_0000_0000;
        cmd.prp2 = 0x2_0000_0000;
        cmd.cdw10 = IDENTIFY_CNS_CONTROLLER;

        assert_eq!(cmd.opcode(), OP_IDENTIFY);
        assert_eq!(cmd.cid(), 0x1234);
        assert_eq!(cmd.nsid, 0xAABBCCDD);
        assert_eq!(cmd.prp1, 0x1_0000_0000);
        assert_eq!(cmd.prp2, 0x2_0000_0000);
        assert_eq!(cmd.cdw10, IDENTIFY_CNS_CONTROLLER);

        // opcode in bits 7:0, CID in 31:16.
        assert_eq!(cmd.cdw0 & 0xFF, OP_IDENTIFY as u32);
        assert_eq!((cmd.cdw0 >> 16) & 0xFFFF, 0x1234);
    }

    // (2) Capability parsing: synthetic CAP value → expected mqes, cqr,
    //     doorbell_stride, timeout_500ms_units, css_nvme.
    #[test]
    fn cap_field_accessors_decode_correctly() {
        // Build a synthetic CAP register:
        //   MQES  = 0x00FF   (256-entry queues)
        //   CQR   = 1        (contiguous)
        //   TO    = 0x20     (16 s total = 32 * 500 ms)
        //   DSTRD = 0        (4-byte stride)
        //   CSS   = bit 37 set (NVM command set)
        //   MPSMIN= 0        (4 KiB pages)
        let mut cap = 0u64;
        cap |= 0x00FF; // MQES
        cap |= 1 << 16; // CQR
        cap |= 0x20u64 << 24; // TO
        cap |= 0u64 << 32; // DSTRD = 0
        cap |= 1u64 << 37; // CSS NVM
        cap |= 0u64 << 48; // MPSMIN = 0

        let c = NvmeCap(cap);
        assert_eq!(c.mqes(), 256);
        assert!(c.cqr());
        assert_eq!(c.doorbell_stride(), 4);
        assert_eq!(c.timeout_500ms_units(), 0x20);
        assert!(c.css_nvme());
        assert_eq!(c.min_memory_page_bytes(), 4096);

        // Vary DSTRD: DSTRD=2 → stride = 16 bytes.
        let c2 = NvmeCap(cap | (2u64 << 32));
        assert_eq!(c2.doorbell_stride(), 16);

        // Vary MPSMIN: MPSMIN=1 → 8 KiB pages.
        let c3 = NvmeCap(cap | (1u64 << 48));
        assert_eq!(c3.min_memory_page_bytes(), 8192);

        // Doorbell offset sanity: queue 1 submission tail with 8-byte stride
        // is DOORBELL_BASE + 2 * 8 = 0x1000 + 16.
        assert_eq!(NvmeRegs::doorbell_offset(1, false, 8), 0x1000 + 16);
        assert_eq!(NvmeRegs::doorbell_offset(1, true, 8), 0x1000 + 24);
        // Queue 0 with 4-byte stride: completion head at 0x1004.
        assert_eq!(NvmeRegs::doorbell_offset(0, false, 4), 0x1000);
        assert_eq!(NvmeRegs::doorbell_offset(0, true, 4), 0x1004);
    }

    // (3) Completion status extraction: crafted completion entry → expected
    //     status code + phase.
    #[test]
    fn completion_status_and_phase_decode_correctly() {
        assert_eq!(core::mem::size_of::<NvmeCompletion>(), 16);

        // Phase bit (bit 0) set, status code zero (success).
        let good = NvmeCompletion {
            result: 0,
            reserved: 0,
            sq_head: 5,
            sq_id: 0,
            cid: 0x42,
            status_phase: 0x0001,
        };
        assert!(completion_phase(&good));
        assert_eq!(completion_status_code(&good), 0);
        assert_eq!(good.cid, 0x42);
        assert_eq!(good.sq_head, 5);

        // Phase bit clear (stale), status code 0x2A = "Invalid Field in
        // Command" (SCT=0, SC=0x02) shifted left by 1.
        let bad = NvmeCompletion {
            result: 0,
            reserved: 0,
            sq_head: 0,
            sq_id: 0,
            cid: 0x99,
            status_phase: (0x2A << 1),
        };
        assert!(!completion_phase(&bad));
        assert_eq!(completion_status_code(&bad), 0x2A);

        // Phase bit + non-zero status combined.
        let both = NvmeCompletion {
            result: 0,
            reserved: 0,
            sq_head: 0,
            sq_id: 0,
            cid: 0x01,
            status_phase: (0x81 << 1) | 0x1,
        };
        assert!(completion_phase(&both));
        assert_eq!(completion_status_code(&both), 0x81);
    }
}
