//! USB Mass Storage class — Bulk-Only Transport (BOT) framing + SCSI command
//! subset (USB Mass Storage Class Bulk-Only Transport Revision 1.0, §5).
//!
//! # Status: host-logic only
//!
//! This module is **pure logic with no hardware dependencies**. It provides the
//! CBW/CSW wire-codec and the SCSI CDB builders that a future ring-3
//! `usb-msc` driver will use. No MMIO, no DMA, no kernel dependencies beyond
//! `core`/`alloc`. Host-testable via
//! `cargo test -p kernel-core --target x86_64-unknown-linux-gnu usb::mass_storage`.
//!
//! # BOT Overview (USB MSC BOT Rev 1.0 §2)
//!
//! The host issues a **Command Block Wrapper** (CBW, 31 bytes) on the Bulk-OUT
//! endpoint. The device processes the command and returns a
//! **Command Status Wrapper** (CSW, 13 bytes) on the Bulk-IN endpoint,
//! optionally preceded by a data stage.
//!
//! # SCSI endianness
//!
//! SCSI CDBs are **big-endian** (network byte order). All multi-byte fields in
//! CDBs (LBA, transfer length, allocation length) are stored MSB-first.
//! The BOT CBW/CSW wrapper fields are **little-endian** (USB convention).

extern crate alloc;

use crate::usb::xhci::trb::SetupPacket;

// ---------------------------------------------------------------------------
// CBW — Command Block Wrapper (BOT §5.1)
// ---------------------------------------------------------------------------

/// `dCBWSignature` — the 4-byte magic that opens every CBW (BOT §5.1).
///
/// Wire value (little-endian u32): `0x43425355` → bytes `55 53 42 43`
/// (ASCII "USBC").
pub const CBW_SIGNATURE: u32 = 0x4342_5355;

/// `bmCBWFlags` bit7 = 1: data transfer direction is **Device-to-Host** (IN).
pub const CBW_FLAGS_DATA_IN: u8 = 0x80;

/// `bmCBWFlags` bit7 = 0: data transfer direction is **Host-to-Device** (OUT).
pub const CBW_FLAGS_DATA_OUT: u8 = 0x00;

/// Wire size of a CBW in bytes (BOT §5.1 Table 5.1).
pub const CBW_LEN: usize = 31;

/// USB Mass Storage Bulk-Only Transport Command Block Wrapper (BOT §5.1).
///
/// The `encode` method serialises this into the 31-byte wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cbw {
    /// `dCBWTag` — a host-assigned identifier, echoed in the matching CSW.
    pub tag: u32,
    /// `dCBWDataTransferLength` — expected byte count of the data stage
    /// (zero if there is no data stage).
    pub data_transfer_length: u32,
    /// `bmCBWFlags` — bit7: 1 = device-to-host (IN), 0 = host-to-device (OUT).
    pub flags: u8,
    /// `bCBWLUN` — Logical Unit Number (low 4 bits used, upper 4 must be zero).
    pub lun: u8,
    /// `CBWCB` — the Command Descriptor Block (up to 16 bytes, zero-padded).
    pub cb: [u8; 16],
    /// `bCBWCBLength` — number of valid bytes in `cb` (1–16, low 5 bits used).
    pub cb_len: u8,
}

impl Cbw {
    /// Construct a CBW from a raw Command Descriptor Block slice.
    ///
    /// `cdb` is copied into the low `cdb.len()` bytes of the 16-byte `CBWCB`
    /// field; remaining bytes are zero-padded. Panics if `cdb.len() > 16`.
    pub fn new(tag: u32, data_len: u32, data_in: bool, lun: u8, cdb: &[u8]) -> Self {
        assert!(cdb.len() <= 16, "CDB must not exceed 16 bytes");
        let mut cb = [0u8; 16];
        cb[..cdb.len()].copy_from_slice(cdb);
        Cbw {
            tag,
            data_transfer_length: data_len,
            flags: if data_in {
                CBW_FLAGS_DATA_IN
            } else {
                CBW_FLAGS_DATA_OUT
            },
            lun,
            cb,
            cb_len: cdb.len() as u8,
        }
    }

    /// Encode this CBW into its 31-byte on-wire representation (BOT §5.1
    /// Table 5.1).
    ///
    /// # Wire layout
    ///
    /// | Offset | Field | Notes |
    /// |--------|-------|-------|
    /// | 0–3   | `dCBWSignature`          | LE u32, always `0x43425355` |
    /// | 4–7   | `dCBWTag`                | LE u32 |
    /// | 8–11  | `dCBWDataTransferLength` | LE u32 |
    /// | 12    | `bmCBWFlags`             | bit7 = direction |
    /// | 13    | `bCBWLUN`                | low 4 bits |
    /// | 14    | `bCBWCBLength`           | low 5 bits |
    /// | 15–30 | `CBWCB`                  | 16 bytes, zero-padded |
    pub fn encode(&self) -> [u8; CBW_LEN] {
        let mut buf = [0u8; CBW_LEN];

        // dCBWSignature (bytes 0–3, LE u32)
        buf[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());

        // dCBWTag (bytes 4–7, LE u32)
        buf[4..8].copy_from_slice(&self.tag.to_le_bytes());

        // dCBWDataTransferLength (bytes 8–11, LE u32)
        buf[8..12].copy_from_slice(&self.data_transfer_length.to_le_bytes());

        // bmCBWFlags (byte 12)
        buf[12] = self.flags;

        // bCBWLUN (byte 13, low 4 bits)
        buf[13] = self.lun & 0x0F;

        // bCBWCBLength (byte 14, low 5 bits)
        buf[14] = self.cb_len & 0x1F;

        // CBWCB (bytes 15–30, 16 bytes)
        buf[15..31].copy_from_slice(&self.cb);

        buf
    }
}

// ---------------------------------------------------------------------------
// CSW — Command Status Wrapper (BOT §5.2)
// ---------------------------------------------------------------------------

/// `dCSWSignature` — the 4-byte magic that opens every CSW (BOT §5.2).
///
/// Wire value (little-endian u32): `0x53425355` → bytes `55 53 42 53`
/// (ASCII "USBS").
pub const CSW_SIGNATURE: u32 = 0x5342_5355;

/// Wire size of a CSW in bytes (BOT §5.2 Table 5.2).
pub const CSW_LEN: usize = 13;

/// `bCSWStatus` value indicating the command **passed** (BOT §5.2).
pub const CSW_STATUS_PASSED: u8 = 0;
/// `bCSWStatus` value indicating the command **failed** (BOT §5.2).
pub const CSW_STATUS_FAILED: u8 = 1;
/// `bCSWStatus` value indicating a **phase error** (BOT §5.2).
pub const CSW_STATUS_PHASE_ERROR: u8 = 2;

/// USB Mass Storage Bulk-Only Transport Command Status Wrapper (BOT §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Csw {
    /// `dCSWTag` — echoed from the matching CBW `dCBWTag`.
    pub tag: u32,
    /// `dCSWDataResidue` — difference between the requested and actual transfer
    /// length (zero when the full transfer completed).
    pub data_residue: u32,
    /// `bCSWStatus` — `0` = passed, `1` = failed, `2` = phase error.
    pub status: u8,
}

impl Csw {
    /// Parse a CSW from a raw byte buffer.
    ///
    /// Returns `None` if:
    /// * `buf.len() < 13`, or
    /// * `dCSWSignature` ≠ [`CSW_SIGNATURE`].
    ///
    /// # Wire layout (BOT §5.2 Table 5.2)
    ///
    /// | Offset | Field |
    /// |--------|-------|
    /// | 0–3   | `dCSWSignature` (LE u32, `0x53425355`) |
    /// | 4–7   | `dCSWTag`       (LE u32) |
    /// | 8–11  | `dCSWDataResidue` (LE u32) |
    /// | 12    | `bCSWStatus` |
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < CSW_LEN {
            return None;
        }
        let sig = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if sig != CSW_SIGNATURE {
            return None;
        }
        let tag = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let data_residue = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let status = buf[12];
        Some(Csw {
            tag,
            data_residue,
            status,
        })
    }
}

// ---------------------------------------------------------------------------
// SCSI CDB builders
//
// All multi-byte fields in SCSI CDBs are big-endian (SCSI Architecture Model
// §4). Opcodes are from the SCSI Block Commands standard (SBC-4).
// ---------------------------------------------------------------------------

/// SCSI opcode: TEST UNIT READY (SBC-4 §5.20).
pub const SCSI_OP_TEST_UNIT_READY: u8 = 0x00;
/// SCSI opcode: REQUEST SENSE (SPC-6 §6.32).
pub const SCSI_OP_REQUEST_SENSE: u8 = 0x03;
/// SCSI opcode: INQUIRY (SPC-6 §6.7).
pub const SCSI_OP_INQUIRY: u8 = 0x12;
/// SCSI opcode: READ CAPACITY (10) (SBC-4 §5.15).
pub const SCSI_OP_READ_CAPACITY10: u8 = 0x25;
/// SCSI opcode: READ (10) (SBC-4 §5.8).
pub const SCSI_OP_READ10: u8 = 0x28;
/// SCSI opcode: WRITE (10) (SBC-4 §5.26).
pub const SCSI_OP_WRITE10: u8 = 0x2A;

/// Build a 6-byte TEST UNIT READY CDB (SBC-4 §5.20).
///
/// All bytes are zero; the opcode byte fully identifies the command.
pub fn cdb_test_unit_ready() -> [u8; 6] {
    [SCSI_OP_TEST_UNIT_READY, 0x00, 0x00, 0x00, 0x00, 0x00]
}

/// Build a 6-byte INQUIRY CDB (SPC-6 §6.7).
///
/// `alloc_len` (byte 4) is the maximum number of bytes the host is prepared
/// to receive. Standard INQUIRY data is 36 bytes.
pub fn cdb_inquiry(alloc_len: u8) -> [u8; 6] {
    [SCSI_OP_INQUIRY, 0x00, 0x00, 0x00, alloc_len, 0x00]
}

/// Build a 10-byte READ CAPACITY (10) CDB (SBC-4 §5.15).
///
/// Returns the last logical block address and the block size of the medium.
/// All parameter bytes are zero (PMI=0, no partial-medium indicator).
pub fn cdb_read_capacity10() -> [u8; 10] {
    [
        SCSI_OP_READ_CAPACITY10,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ]
}

/// Build a 10-byte READ (10) CDB (SBC-4 §5.8).
///
/// * `lba` — starting Logical Block Address (big-endian, bytes 2–5).
/// * `blocks` — transfer length in blocks (big-endian, bytes 7–8).
pub fn cdb_read10(lba: u32, blocks: u16) -> [u8; 10] {
    let lba_be = lba.to_be_bytes();
    let len_be = blocks.to_be_bytes();
    [
        SCSI_OP_READ10, // byte 0: opcode
        0x00,           // byte 1: RDPROTECT | DPO | FUA | RARC | Reserved
        lba_be[0],      // byte 2: LBA MSB
        lba_be[1],      // byte 3
        lba_be[2],      // byte 4
        lba_be[3],      // byte 5: LBA LSB
        0x00,           // byte 6: GROUP NUMBER
        len_be[0],      // byte 7: TRANSFER LENGTH MSB
        len_be[1],      // byte 8: TRANSFER LENGTH LSB
        0x00,           // byte 9: CONTROL
    ]
}

/// Build a 10-byte WRITE (10) CDB (SBC-4 §5.26).
///
/// * `lba` — starting Logical Block Address (big-endian, bytes 2–5).
/// * `blocks` — transfer length in blocks (big-endian, bytes 7–8).
pub fn cdb_write10(lba: u32, blocks: u16) -> [u8; 10] {
    let lba_be = lba.to_be_bytes();
    let len_be = blocks.to_be_bytes();
    [
        SCSI_OP_WRITE10, // byte 0: opcode
        0x00,            // byte 1: WRPROTECT | DPO | FUA | Reserved
        lba_be[0],       // byte 2: LBA MSB
        lba_be[1],       // byte 3
        lba_be[2],       // byte 4
        lba_be[3],       // byte 5: LBA LSB
        0x00,            // byte 6: GROUP NUMBER
        len_be[0],       // byte 7: TRANSFER LENGTH MSB
        len_be[1],       // byte 8: TRANSFER LENGTH LSB
        0x00,            // byte 9: CONTROL
    ]
}

/// Build a 6-byte REQUEST SENSE CDB (SPC-6 §6.32).
///
/// `alloc_len` (byte 4) is the maximum number of sense bytes to return.
/// Standard fixed-format sense data is 18 bytes.
pub fn cdb_request_sense(alloc_len: u8) -> [u8; 6] {
    [SCSI_OP_REQUEST_SENSE, 0x00, 0x00, 0x00, alloc_len, 0x00]
}

// ---------------------------------------------------------------------------
// READ CAPACITY (10) response parser
// ---------------------------------------------------------------------------

/// Parsed READ CAPACITY (10) response (SBC-4 §5.15.2).
///
/// The response is 8 bytes, both fields big-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadCapacity10 {
    /// `RETURNED LOGICAL BLOCK ADDRESS` — the last addressable LBA (0-based).
    ///
    /// Total number of logical blocks = `last_lba + 1`.
    pub last_lba: u32,
    /// `LOGICAL BLOCK LENGTH IN BYTES` — size of one logical block in bytes
    /// (typically 512 for hard drives, 2048 for optical media).
    pub block_size: u32,
}

impl ReadCapacity10 {
    /// Wire size of the READ CAPACITY (10) response in bytes.
    pub const RESPONSE_LEN: usize = 8;

    /// Parse a READ CAPACITY (10) response buffer.
    ///
    /// Returns `None` if `buf.len() < 8`.
    ///
    /// # Wire layout (SBC-4 §5.15.2)
    ///
    /// | Offset | Field |
    /// |--------|-------|
    /// | 0–3 | `RETURNED LOGICAL BLOCK ADDRESS` (BE u32) |
    /// | 4–7 | `LOGICAL BLOCK LENGTH IN BYTES` (BE u32) |
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::RESPONSE_LEN {
            return None;
        }
        let last_lba = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let block_size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        Some(ReadCapacity10 {
            last_lba,
            block_size,
        })
    }
}

// ---------------------------------------------------------------------------
// INQUIRY response parser
// ---------------------------------------------------------------------------

/// Minimum length of a standard INQUIRY response that this parser requires.
///
/// The full standard response is 36 bytes (`ADDITIONAL LENGTH` = 31, plus the
/// 5-byte header). Byte 4 (`ADDITIONAL LENGTH`) specifies how many additional
/// bytes follow byte 4; a minimum-compliant device returns at least 5 bytes
/// before the additional data, giving at least 36 bytes total for the vendor
/// and product identification fields.
pub const INQUIRY_MIN_LEN: usize = 36;

/// Parsed standard INQUIRY response (SPC-6 §6.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InquiryData {
    /// `PERIPHERAL DEVICE TYPE` — low 5 bits of byte 0.
    ///
    /// Common values: 0x00 = Direct-access block device (disk),
    /// 0x05 = CD/DVD, 0x1F = No device present.
    pub peripheral_device_type: u8,
    /// `RMB` — bit 7 of byte 1: 1 = medium is removable.
    pub removable: bool,
    /// `T10 VENDOR IDENTIFICATION` — bytes 8–15 (8 bytes, space-padded).
    pub vendor: [u8; 8],
    /// `PRODUCT IDENTIFICATION` — bytes 16–31 (16 bytes, space-padded).
    pub product: [u8; 16],
}

impl InquiryData {
    /// Parse a standard INQUIRY response buffer.
    ///
    /// Returns `None` if `buf.len() < 36` (the minimum needed to reach the
    /// product identification field at bytes 16–31).
    ///
    /// # Wire layout (SPC-6 §6.7)
    ///
    /// | Offset | Field |
    /// |--------|-------|
    /// | 0      | `PERIPHERAL QUALIFIER` (bits 7:5) + `PERIPHERAL DEVICE TYPE` (bits 4:0) |
    /// | 1      | `RMB` (bit 7) + reserved |
    /// | 2      | `VERSION` |
    /// | 3      | response data format / flags |
    /// | 4      | `ADDITIONAL LENGTH` |
    /// | 5–7    | reserved / flags |
    /// | 8–15   | `T10 VENDOR IDENTIFICATION` (8 bytes) |
    /// | 16–31  | `PRODUCT IDENTIFICATION` (16 bytes) |
    /// | 32–35  | `PRODUCT REVISION LEVEL` (4 bytes, not captured here) |
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < INQUIRY_MIN_LEN {
            return None;
        }
        let peripheral_device_type = buf[0] & 0x1F;
        let removable = (buf[1] & 0x80) != 0;
        let mut vendor = [0u8; 8];
        vendor.copy_from_slice(&buf[8..16]);
        let mut product = [0u8; 16];
        product.copy_from_slice(&buf[16..32]);
        Some(InquiryData {
            peripheral_device_type,
            removable,
            vendor,
            product,
        })
    }
}

// ---------------------------------------------------------------------------
// GET_MAX_LUN class-specific control request (BOT §3.2)
// ---------------------------------------------------------------------------

/// `bmRequestType` for the GET_MAX_LUN class-specific request (BOT §3.2):
/// Device-to-Host (bit 7 = 1), Class type (bits 6:5 = 01),
/// Interface recipient (bits 4:0 = 00001) → 0xA1.
pub const BM_REQUEST_TYPE_CLASS_INTERFACE_D2H: u8 = 0xA1;

/// `bRequest` for GET_MAX_LUN (BOT §3.2).
pub const B_REQUEST_GET_MAX_LUN: u8 = 0xFE;

/// Build the GET_MAX_LUN [`SetupPacket`] for the given interface number
/// (BOT §3.2).
///
/// The device returns a single byte — the highest valid LUN number (0–15).
/// A device with a single LUN returns 0.
///
/// # Wire encoding (8 bytes)
///
/// | Offset | Value | Meaning |
/// |--------|-------|---------|
/// | 0 | `0xA1` | bmRequestType: class, interface, D2H |
/// | 1 | `0xFE` | bRequest: GET_MAX_LUN |
/// | 2–3 | `0x0000` | wValue = 0 |
/// | 4–5 | `interface` | wIndex = interface number (LE u16) |
/// | 6–7 | `0x0001` | wLength = 1 |
pub const fn get_max_lun(interface: u8) -> SetupPacket {
    SetupPacket {
        bm_request_type: BM_REQUEST_TYPE_CLASS_INTERFACE_D2H,
        b_request: B_REQUEST_GET_MAX_LUN,
        w_value: 0,
        w_index: interface as u16,
        w_length: 1,
    }
}

// ---------------------------------------------------------------------------
// UAS — USB Attached SCSI Protocol (UAS r01)
// ---------------------------------------------------------------------------
//
// # UAS Overview (USB Attached SCSI Protocol r01)
//
// UAS replaces BOT's serial CBW/CSW with typed **Information Units (IUs)**
// carried over **four dedicated pipes**:
//
// | Pipe          | Direction | Purpose                          |
// |---------------|-----------|----------------------------------|
// | Command       | OUT       | Host sends Command IUs            |
// | Status        | IN        | Device sends Sense / Response IUs |
// | Data-In       | IN        | Device sends read data            |
// | Data-Out      | OUT       | Host sends write data             |
//
// Each pipe uses xHCI **streams** (one stream per in-flight tag).
//
// ## Tag ↔ Stream ID
//
// UAS uses the 16-bit Tag value directly as the xHCI Stream ID for the
// Command, Status, Data-In, and Data-Out pipes. Tag 0 is reserved; tags
// begin at 1. The daemon opens a stream with `stream_id = tag` on all four
// pipes before submitting a Command IU and closes them after receiving the
// matching Sense/Response IU.
//
// ## Codec scope
//
// This module is the **wire codec only** (pure host logic, no hardware
// dependencies). The live pipe and stream plumbing belongs in the ring-3
// `usb-storage` daemon.

// ---------------------------------------------------------------------------
// IU ID constants (UAS r01 §7)
// ---------------------------------------------------------------------------

/// IU ID for a **Command IU** — carries a SCSI CDB from host to device.
pub const UAS_IU_COMMAND: u8 = 0x01;

/// IU ID for a **Sense IU** — device reports SCSI status and sense data.
pub const UAS_IU_SENSE: u8 = 0x03;

/// IU ID for a **Response IU** — device reports task-management status.
pub const UAS_IU_RESPONSE: u8 = 0x04;

/// IU ID for a **Task Management IU** — host issues an abort/reset request.
pub const UAS_IU_TASK_MGMT: u8 = 0x05;

/// IU ID for a **Read Ready IU** — device signals it is ready to receive data
/// on the Data-In stream for the given tag.
pub const UAS_IU_READ_READY: u8 = 0x06;

/// IU ID for a **Write Ready IU** — device signals it is ready to accept data
/// on the Data-Out stream for the given tag.
pub const UAS_IU_WRITE_READY: u8 = 0x07;

// ---------------------------------------------------------------------------
// Response IU response-code constants (UAS r01 §7.5)
// ---------------------------------------------------------------------------

/// Task Management Function completed successfully.
pub const UAS_RESPONSE_TASK_MGMT_FUNCTION_COMPLETE: u8 = 0x00;
/// The received IU was invalid.
pub const UAS_RESPONSE_INVALID_IU: u8 = 0x02;
/// The requested Task Management Function is not supported.
pub const UAS_RESPONSE_TMF_NOT_SUPPORTED: u8 = 0x04;
/// The Task Management Function failed.
pub const UAS_RESPONSE_TMF_FAILED: u8 = 0x05;
/// The Task Management Function succeeded.
pub const UAS_RESPONSE_TMF_SUCCEEDED: u8 = 0x08;
/// A command with an overlapping tag was received.
pub const UAS_RESPONSE_OVERLAPPED_TAG: u8 = 0x09;

// ---------------------------------------------------------------------------
// Task Management Function constants (UAS r01 §7.6)
// ---------------------------------------------------------------------------

/// Abort the task identified by the Task Tag field.
pub const UAS_TMF_ABORT_TASK: u8 = 0x01;
/// Perform a Logical Unit Reset.
pub const UAS_TMF_LOGICAL_UNIT_RESET: u8 = 0x08;

// ---------------------------------------------------------------------------
// Wire sizes
// ---------------------------------------------------------------------------

/// Wire size of a UAS Command IU in bytes (UAS r01 §7.2).
///
/// Fixed at 32 bytes for a 16-byte CDB (no Additional CDB bytes).
pub const UAS_COMMAND_IU_LEN: usize = 32;

/// Wire size of the minimum UAS Sense IU header (IU ID through status byte,
/// UAS r01 §7.3).
///
/// Full Sense IU = 16 header bytes + up to 252 bytes of sense data.
pub const UAS_SENSE_IU_MIN_LEN: usize = 16;

/// Wire size of a UAS Response IU in bytes (UAS r01 §7.5).
pub const UAS_RESPONSE_IU_LEN: usize = 8;

/// Wire size of a UAS Read/Write Ready IU in bytes (UAS r01 §7.7/7.8).
pub const UAS_READY_IU_LEN: usize = 4;

/// Wire size of a UAS Task Management IU in bytes (UAS r01 §7.6).
pub const UAS_TASK_MGMT_IU_LEN: usize = 16;

// ---------------------------------------------------------------------------
// Command IU (UAS r01 §7.2)
// ---------------------------------------------------------------------------

/// UAS **Command IU** — carries a SCSI CDB from the host to the device.
///
/// Wire layout (32 bytes, 16-byte CDB, no additional CDB bytes):
///
/// | Offset | Field | Notes |
/// |--------|-------|-------|
/// | 0      | IU ID | `0x01` |
/// | 1      | Reserved | |
/// | 2–3    | Tag   | BE u16; also the xHCI Stream ID |
/// | 4      | Command Priority \| Task Attribute | bits 6:4 priority, bits 2:0 attr |
/// | 5      | Reserved | |
/// | 6      | Reserved | |
/// | 7      | Additional CDB Length | 0 for a standard 16-byte CDB |
/// | 8–15   | Logical Unit Number   | 8 bytes (SCSI ADDRESS METHOD) |
/// | 16–31  | CDB (Command Descriptor Block) | 16 bytes, zero-padded |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandIu {
    /// 16-bit command tag; also the xHCI Stream ID for all four UAS pipes.
    pub tag: u16,
    /// `COMMAND PRIORITY` (bits 6:4) and `TASK ATTRIBUTE` (bits 2:0) packed
    /// into a single byte (UAS r01 §7.2).
    pub command_priority_task_attr: u8,
    /// 8-byte Logical Unit Number (SCSI LUN address format).
    pub lun: [u8; 8],
    /// 16-byte Command Descriptor Block (zero-padded for shorter CDBs).
    pub cdb: [u8; 16],
}

impl CommandIu {
    /// Construct a Command IU from a SCSI CDB slice.
    ///
    /// `cdb_bytes` is copied into the low `cdb_bytes.len()` bytes of the
    /// 16-byte CDB field; remaining bytes are zero-padded.  Panics if
    /// `cdb_bytes.len() > 16`.
    ///
    /// `task_attr` is placed in bits 2:0; command priority is set to 0.
    pub fn new(tag: u16, lun: [u8; 8], cdb_bytes: &[u8], task_attr: u8) -> Self {
        assert!(cdb_bytes.len() <= 16, "CDB must not exceed 16 bytes");
        let mut cdb = [0u8; 16];
        cdb[..cdb_bytes.len()].copy_from_slice(cdb_bytes);
        CommandIu {
            tag,
            command_priority_task_attr: task_attr & 0x07,
            lun,
            cdb,
        }
    }

    /// Encode this Command IU into its 32-byte on-wire representation
    /// (UAS r01 §7.2).
    pub fn encode(&self) -> [u8; UAS_COMMAND_IU_LEN] {
        let mut buf = [0u8; UAS_COMMAND_IU_LEN];

        // byte 0: IU ID
        buf[0] = UAS_IU_COMMAND;
        // byte 1: Reserved
        buf[1] = 0x00;
        // bytes 2–3: Tag (BE u16)
        let tag_be = self.tag.to_be_bytes();
        buf[2] = tag_be[0];
        buf[3] = tag_be[1];
        // byte 4: Command Priority | Task Attribute
        buf[4] = self.command_priority_task_attr;
        // byte 5: Reserved
        buf[5] = 0x00;
        // byte 6: Reserved
        buf[6] = 0x00;
        // byte 7: Additional CDB Length (0 for standard 16-byte CDB)
        buf[7] = 0x00;
        // bytes 8–15: Logical Unit Number (8 bytes)
        buf[8..16].copy_from_slice(&self.lun);
        // bytes 16–31: CDB (16 bytes)
        buf[16..32].copy_from_slice(&self.cdb);

        buf
    }
}

// ---------------------------------------------------------------------------
// Sense IU (UAS r01 §7.3)
// ---------------------------------------------------------------------------

/// Minimum wire size of sense data that `SenseIu::parse` will capture.
///
/// Up to [`UAS_SENSE_MAX_DATA`] bytes of sense data are stored inline.
pub const UAS_SENSE_MAX_DATA: usize = 252;

/// UAS **Sense IU** — device reports SCSI command status and sense data.
///
/// Wire layout (16 header bytes + up to 252 bytes of sense data):
///
/// | Offset | Field | Notes |
/// |--------|-------|-------|
/// | 0      | IU ID | `0x03` |
/// | 1      | Reserved | |
/// | 2–3    | Tag   | BE u16 |
/// | 4–5    | Status Qualifier | BE u16 |
/// | 6      | Status  | SCSI status byte (0x00 = GOOD) |
/// | 7–13   | Reserved (7 bytes) | |
/// | 14–15  | Sense Data Length | BE u16 |
/// | 16…    | Sense Data | `sense_data_length` bytes |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenseIu {
    /// 16-bit command tag matching the originating Command IU.
    pub tag: u16,
    /// SCSI Status Qualifier (big-endian u16 at bytes 4–5).
    pub status_qualifier: u16,
    /// SCSI Status byte (byte 6): `0x00` = GOOD, `0x02` = CHECK CONDITION, etc.
    pub status: u8,
    /// Number of valid bytes in `sense_data`.
    pub sense_data_length: u16,
    /// Raw sense data bytes (up to [`UAS_SENSE_MAX_DATA`] bytes, zero-padded).
    pub sense_data: [u8; UAS_SENSE_MAX_DATA],
}

impl SenseIu {
    /// Parse a Sense IU from a raw byte buffer.
    ///
    /// Returns `None` if:
    /// * `buf.len() < 16` (minimum header), or
    /// * `buf[0]` ≠ [`UAS_IU_SENSE`].
    ///
    /// Sense data is capped at [`UAS_SENSE_MAX_DATA`] bytes even if the wire
    /// value of `Sense Data Length` exceeds that.
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < UAS_SENSE_IU_MIN_LEN {
            return None;
        }
        if buf[0] != UAS_IU_SENSE {
            return None;
        }
        let tag = u16::from_be_bytes([buf[2], buf[3]]);
        let status_qualifier = u16::from_be_bytes([buf[4], buf[5]]);
        let status = buf[6];
        let sense_data_length = u16::from_be_bytes([buf[14], buf[15]]);

        let mut sense_data = [0u8; UAS_SENSE_MAX_DATA];
        let available = buf.len().saturating_sub(UAS_SENSE_IU_MIN_LEN);
        let copy_len = (sense_data_length as usize)
            .min(available)
            .min(UAS_SENSE_MAX_DATA);
        sense_data[..copy_len].copy_from_slice(&buf[16..16 + copy_len]);

        Some(SenseIu {
            tag,
            status_qualifier,
            status,
            sense_data_length,
            sense_data,
        })
    }
}

// ---------------------------------------------------------------------------
// Response IU (UAS r01 §7.5)
// ---------------------------------------------------------------------------

/// UAS **Response IU** — device reports task management function status.
///
/// Wire layout (8 bytes):
///
/// | Offset | Field | Notes |
/// |--------|-------|-------|
/// | 0      | IU ID | `0x04` |
/// | 1      | Reserved | |
/// | 2–3    | Tag   | BE u16 |
/// | 4–6    | Additional Response Info | 3 bytes (implementation-specific) |
/// | 7      | Response Code | `UAS_RESPONSE_*` constant |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseIu {
    /// 16-bit task management tag matching the originating Task Management IU.
    pub tag: u16,
    /// Three bytes of implementation-specific additional response info.
    pub additional_response_info: [u8; 3],
    /// Response code — see `UAS_RESPONSE_*` constants.
    pub response_code: u8,
}

impl ResponseIu {
    /// Parse a Response IU from a raw byte buffer.
    ///
    /// Returns `None` if:
    /// * `buf.len() < 8`, or
    /// * `buf[0]` ≠ [`UAS_IU_RESPONSE`].
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < UAS_RESPONSE_IU_LEN {
            return None;
        }
        if buf[0] != UAS_IU_RESPONSE {
            return None;
        }
        let tag = u16::from_be_bytes([buf[2], buf[3]]);
        let additional_response_info = [buf[4], buf[5], buf[6]];
        let response_code = buf[7];
        Some(ResponseIu {
            tag,
            additional_response_info,
            response_code,
        })
    }
}

// ---------------------------------------------------------------------------
// Read Ready / Write Ready IUs (UAS r01 §7.7 / §7.8)
// ---------------------------------------------------------------------------

/// Parse a **Read Ready IU** or **Write Ready IU** and return its tag.
///
/// Both IUs share the same 4-byte layout:
///
/// | Offset | Field | Notes |
/// |--------|-------|-------|
/// | 0      | IU ID | `0x06` (Read Ready) or `0x07` (Write Ready) |
/// | 1      | Reserved | |
/// | 2–3    | Tag   | BE u16 |
///
/// Returns `None` if `buf.len() < 4` or the IU ID is not `expected_iu_id`.
fn parse_ready_iu(buf: &[u8], expected_iu_id: u8) -> Option<u16> {
    if buf.len() < UAS_READY_IU_LEN {
        return None;
    }
    if buf[0] != expected_iu_id {
        return None;
    }
    Some(u16::from_be_bytes([buf[2], buf[3]]))
}

/// Parse a **Read Ready IU** (`bIUID = 0x06`) and return its tag.
///
/// The device sends this IU on the Status pipe to signal that it is ready to
/// send data on the Data-In stream identified by the returned tag.
///
/// Returns `None` if `buf.len() < 4` or the IU ID byte is not `0x06`.
pub fn parse_read_ready_iu(buf: &[u8]) -> Option<u16> {
    parse_ready_iu(buf, UAS_IU_READ_READY)
}

/// Parse a **Write Ready IU** (`bIUID = 0x07`) and return its tag.
///
/// The device sends this IU on the Status pipe to signal that it is ready to
/// accept data on the Data-Out stream identified by the returned tag.
///
/// Returns `None` if `buf.len() < 4` or the IU ID byte is not `0x07`.
pub fn parse_write_ready_iu(buf: &[u8]) -> Option<u16> {
    parse_ready_iu(buf, UAS_IU_WRITE_READY)
}

// ---------------------------------------------------------------------------
// Task Management IU (UAS r01 §7.6)
// ---------------------------------------------------------------------------

/// UAS **Task Management IU** — host issues an abort or reset request.
///
/// Wire layout (16 bytes):
///
/// | Offset | Field | Notes |
/// |--------|-------|-------|
/// | 0      | IU ID | `0x05` |
/// | 1      | Reserved | |
/// | 2–3    | Tag   | BE u16 — tag for *this* TM IU |
/// | 4      | TM Function | `UAS_TMF_*` constant |
/// | 5      | Reserved | |
/// | 6–7    | Task Tag | BE u16 — tag of the command to abort/reset |
/// | 8–15   | Logical Unit Number | 8 bytes |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskMgmtIu {
    /// Tag for this Task Management IU itself (also the xHCI Stream ID).
    pub tag: u16,
    /// Task Management Function — see `UAS_TMF_*` constants.
    pub tm_function: u8,
    /// Tag of the command being targeted (e.g., the tag to abort).
    pub task_tag: u16,
    /// 8-byte Logical Unit Number.
    pub lun: [u8; 8],
}

impl TaskMgmtIu {
    /// Encode this Task Management IU into its 16-byte on-wire representation
    /// (UAS r01 §7.6).
    pub fn encode(&self) -> [u8; UAS_TASK_MGMT_IU_LEN] {
        let mut buf = [0u8; UAS_TASK_MGMT_IU_LEN];

        // byte 0: IU ID
        buf[0] = UAS_IU_TASK_MGMT;
        // byte 1: Reserved
        buf[1] = 0x00;
        // bytes 2–3: Tag (BE u16)
        let tag_be = self.tag.to_be_bytes();
        buf[2] = tag_be[0];
        buf[3] = tag_be[1];
        // byte 4: TM Function
        buf[4] = self.tm_function;
        // byte 5: Reserved
        buf[5] = 0x00;
        // bytes 6–7: Task Tag (BE u16)
        let task_tag_be = self.task_tag.to_be_bytes();
        buf[6] = task_tag_be[0];
        buf[7] = task_tag_be[1];
        // bytes 8–15: Logical Unit Number (8 bytes)
        buf[8..16].copy_from_slice(&self.lun);

        buf
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CBW encoding tests
    // -----------------------------------------------------------------------

    /// A basic CBW encodes to exactly 31 bytes with the correct "USBC" signature.
    #[test]
    fn cbw_encode_length_is_31() {
        let cbw = Cbw::new(1, 512, true, 0, &cdb_read10(0, 1));
        assert_eq!(cbw.encode().len(), CBW_LEN);
    }

    /// The signature bytes are `55 53 42 43` (ASCII "USBC", LE u32 0x43425355).
    #[test]
    fn cbw_encode_signature_usbc() {
        let cbw = Cbw::new(1, 512, true, 0, &cdb_read10(0, 1));
        let wire = cbw.encode();
        assert_eq!(
            &wire[0..4],
            &[0x55, 0x53, 0x42, 0x43],
            "signature must be 55 53 42 43 (USBC)"
        );
    }

    /// Tag and data_transfer_length are encoded little-endian at the expected
    /// offsets.
    #[test]
    fn cbw_encode_tag_and_length_le() {
        let cbw = Cbw::new(0x0102_0304, 0x0A0B_0C0D, false, 0, &cdb_test_unit_ready());
        let wire = cbw.encode();

        // tag at bytes 4–7 (LE)
        assert_eq!(&wire[4..8], &[0x04, 0x03, 0x02, 0x01]);
        // data_transfer_length at bytes 8–11 (LE)
        assert_eq!(&wire[8..12], &[0x0D, 0x0C, 0x0B, 0x0A]);
    }

    /// flags byte (offset 12): data-IN sets bit7, data-OUT clears it.
    #[test]
    fn cbw_encode_flags_data_in() {
        let cbw_in = Cbw::new(1, 512, true, 0, &cdb_read10(0, 1));
        let cbw_out = Cbw::new(2, 512, false, 0, &cdb_write10(0, 1));
        assert_eq!(cbw_in.encode()[12], 0x80, "data-IN must have bit7 set");
        assert_eq!(cbw_out.encode()[12], 0x00, "data-OUT must have bit7 clear");
    }

    /// LUN (offset 13) uses only the low 4 bits.
    #[test]
    fn cbw_encode_lun_low_nibble() {
        let cbw = Cbw::new(1, 0, false, 0x0F, &cdb_test_unit_ready());
        let wire = cbw.encode();
        assert_eq!(wire[13], 0x0F);

        // Upper bits are masked away.
        let cbw2 = Cbw::new(1, 0, false, 0xFF, &cdb_test_unit_ready());
        assert_eq!(cbw2.encode()[13], 0x0F);
    }

    /// The CDB is copied into bytes 15–30 and zero-padded.
    #[test]
    fn cbw_encode_cdb_copied_and_zero_padded() {
        let cdb = cdb_inquiry(36); // 6-byte CDB
        let cbw = Cbw::new(7, 36, true, 0, &cdb);
        let wire = cbw.encode();

        // bCBWCBLength (byte 14) = 6
        assert_eq!(wire[14], 6);

        // CDB bytes at 15..21 match
        assert_eq!(&wire[15..21], &cdb);

        // Bytes 21..31 must be zero (padding)
        assert_eq!(&wire[21..31], &[0u8; 10]);
    }

    /// CBW using `Cbw::new` helper round-trips a READ10 CDB correctly.
    #[test]
    fn cbw_new_read10_round_trip() {
        let cdb = cdb_read10(0x1234, 8);
        let cbw = Cbw::new(42, 8 * 512, true, 0, &cdb);
        let wire = cbw.encode();

        // Signature
        assert_eq!(&wire[0..4], &[0x55, 0x53, 0x42, 0x43]);
        // Tag = 42 (LE)
        assert_eq!(u32::from_le_bytes([wire[4], wire[5], wire[6], wire[7]]), 42);
        // Data length = 8 * 512 = 4096 (LE)
        assert_eq!(
            u32::from_le_bytes([wire[8], wire[9], wire[10], wire[11]]),
            8 * 512
        );
        // Flags = 0x80 (IN)
        assert_eq!(wire[12], 0x80);
        // CDB at bytes 15..25
        assert_eq!(&wire[15..25], &cdb);
    }

    // -----------------------------------------------------------------------
    // CSW parsing tests
    // -----------------------------------------------------------------------

    /// A valid 13-byte "USBS" CSW parses correctly.
    #[test]
    fn csw_parse_valid() {
        // Build a wire CSW: sig=USBS, tag=0x0000_0001, residue=0, status=0
        let mut buf = [0u8; 13];
        buf[0..4].copy_from_slice(&CSW_SIGNATURE.to_le_bytes()); // 55 53 42 53
        buf[4..8].copy_from_slice(&1u32.to_le_bytes());
        buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        buf[12] = CSW_STATUS_PASSED;

        let csw = Csw::parse(&buf).expect("valid CSW must parse");
        assert_eq!(csw.tag, 1);
        assert_eq!(csw.data_residue, 0);
        assert_eq!(csw.status, CSW_STATUS_PASSED);
    }

    /// CSW wire bytes `55 53 42 53` match the "USBS" signature.
    #[test]
    fn csw_signature_bytes_usbs() {
        let sig_le = CSW_SIGNATURE.to_le_bytes();
        assert_eq!(
            sig_le,
            [0x55, 0x53, 0x42, 0x53],
            "CSW signature must be 55 53 42 53 (USBS)"
        );
    }

    /// CSW parsing extracts tag, residue, and status correctly.
    #[test]
    fn csw_parse_extracts_fields() {
        let mut buf = [0u8; 13];
        buf[0..4].copy_from_slice(&CSW_SIGNATURE.to_le_bytes());
        buf[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        buf[8..12].copy_from_slice(&0x0000_0100u32.to_le_bytes());
        buf[12] = CSW_STATUS_FAILED;

        let csw = Csw::parse(&buf).unwrap();
        assert_eq!(csw.tag, 0xDEAD_BEEF);
        assert_eq!(csw.data_residue, 0x100);
        assert_eq!(csw.status, CSW_STATUS_FAILED);
    }

    /// A CSW with the wrong signature returns None.
    #[test]
    fn csw_parse_wrong_signature_returns_none() {
        let mut buf = [0u8; 13];
        buf[0..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // wrong sig
        assert!(Csw::parse(&buf).is_none());
    }

    /// A buffer shorter than 13 bytes returns None.
    #[test]
    fn csw_parse_short_buffer_returns_none() {
        let buf = [0u8; 12]; // one byte short
        assert!(Csw::parse(&buf).is_none());
    }

    /// An empty buffer returns None without panicking.
    #[test]
    fn csw_parse_empty_buffer_returns_none() {
        assert!(Csw::parse(&[]).is_none());
    }

    // -----------------------------------------------------------------------
    // SCSI CDB builder tests
    // -----------------------------------------------------------------------

    /// TEST UNIT READY is 6 bytes, all zero.
    #[test]
    fn cdb_test_unit_ready_all_zero() {
        let cdb = cdb_test_unit_ready();
        assert_eq!(cdb.len(), 6);
        assert_eq!(cdb, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    /// INQUIRY CDB: opcode 0x12, byte4 = alloc_len.
    #[test]
    fn cdb_inquiry_opcode_and_alloc_len() {
        let cdb = cdb_inquiry(36);
        assert_eq!(cdb.len(), 6);
        assert_eq!(cdb[0], SCSI_OP_INQUIRY);
        assert_eq!(cdb[4], 36);
        // bytes 1–3 and 5 are zero
        assert_eq!(cdb[1], 0);
        assert_eq!(cdb[2], 0);
        assert_eq!(cdb[3], 0);
        assert_eq!(cdb[5], 0);
    }

    /// READ CAPACITY (10) is 10 bytes, opcode 0x25, rest zero.
    #[test]
    fn cdb_read_capacity10_opcode_and_zeros() {
        let cdb = cdb_read_capacity10();
        assert_eq!(cdb.len(), 10);
        assert_eq!(cdb[0], SCSI_OP_READ_CAPACITY10);
        assert_eq!(&cdb[1..10], &[0u8; 9]);
    }

    /// READ (10): opcode 0x28, LBA big-endian at bytes 2–5, blocks big-endian
    /// at bytes 7–8.
    #[test]
    fn cdb_read10_lba_and_blocks_big_endian() {
        let cdb = cdb_read10(0x0000_1234, 8);
        assert_eq!(cdb.len(), 10);
        assert_eq!(cdb[0], SCSI_OP_READ10);
        // LBA 0x00001234 in BE → bytes 2..6 = 00 00 12 34
        assert_eq!(&cdb[2..6], &[0x00, 0x00, 0x12, 0x34]);
        // blocks 8 in BE → bytes 7..9 = 00 08
        assert_eq!(&cdb[7..9], &[0x00, 0x08]);
    }

    /// READ (10) with a large LBA encodes all 4 bytes correctly.
    #[test]
    fn cdb_read10_large_lba() {
        let cdb = cdb_read10(0xABCD_EF01, 0x0200);
        assert_eq!(&cdb[2..6], &[0xAB, 0xCD, 0xEF, 0x01]);
        assert_eq!(&cdb[7..9], &[0x02, 0x00]);
    }

    /// WRITE (10): opcode 0x2A, same layout as READ (10).
    #[test]
    fn cdb_write10_opcode_and_fields() {
        let cdb = cdb_write10(0x0000_1234, 8);
        assert_eq!(cdb.len(), 10);
        assert_eq!(cdb[0], SCSI_OP_WRITE10);
        assert_eq!(&cdb[2..6], &[0x00, 0x00, 0x12, 0x34]);
        assert_eq!(&cdb[7..9], &[0x00, 0x08]);
    }

    /// REQUEST SENSE: opcode 0x03, byte4 = alloc_len.
    #[test]
    fn cdb_request_sense_opcode_and_alloc_len() {
        let cdb = cdb_request_sense(18);
        assert_eq!(cdb.len(), 6);
        assert_eq!(cdb[0], SCSI_OP_REQUEST_SENSE);
        assert_eq!(cdb[4], 18);
    }

    // -----------------------------------------------------------------------
    // ReadCapacity10 parser tests
    // -----------------------------------------------------------------------

    /// A valid 8-byte READ CAPACITY (10) response parses correctly.
    #[test]
    fn read_capacity10_parse_valid() {
        // last_lba = 0x001D_7FFF (1,933,311 → 1 GB disk at 512 B/block)
        // block_size = 512 (0x0000_0200)
        let buf: [u8; 8] = [0x00, 0x1D, 0x7F, 0xFF, 0x00, 0x00, 0x02, 0x00];
        let rc = ReadCapacity10::parse(&buf).expect("must parse 8-byte response");
        assert_eq!(rc.last_lba, 0x001D_7FFF);
        assert_eq!(rc.block_size, 512);
    }

    /// Short buffer returns None.
    #[test]
    fn read_capacity10_parse_short_returns_none() {
        let buf = [0u8; 7];
        assert!(ReadCapacity10::parse(&buf).is_none());
    }

    /// Both fields are decoded big-endian.
    #[test]
    fn read_capacity10_parse_big_endian() {
        let buf: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let rc = ReadCapacity10::parse(&buf).unwrap();
        assert_eq!(rc.last_lba, 0x0102_0304);
        assert_eq!(rc.block_size, 0x0506_0708);
    }

    // -----------------------------------------------------------------------
    // InquiryData parser tests
    // -----------------------------------------------------------------------

    fn make_inquiry_buf() -> [u8; 36] {
        let mut buf = [0u8; 36];
        // byte 0: PERIPHERAL DEVICE TYPE = 0x00 (Direct-access block device)
        buf[0] = 0x00;
        // byte 1: RMB = 1 (removable)
        buf[1] = 0x80;
        // bytes 8–15: vendor "VENDOR  " (space-padded)
        buf[8..16].copy_from_slice(b"VENDOR  ");
        // bytes 16–31: product "PRODUCT         " (space-padded)
        buf[16..32].copy_from_slice(b"PRODUCT         ");
        buf
    }

    #[test]
    fn inquiry_parse_device_type_and_removable() {
        let buf = make_inquiry_buf();
        let inq = InquiryData::parse(&buf).expect("must parse 36-byte response");
        assert_eq!(inq.peripheral_device_type, 0x00);
        assert!(inq.removable);
    }

    #[test]
    fn inquiry_parse_vendor_and_product() {
        let buf = make_inquiry_buf();
        let inq = InquiryData::parse(&buf).unwrap();
        assert_eq!(&inq.vendor, b"VENDOR  ");
        assert_eq!(&inq.product, b"PRODUCT         ");
    }

    #[test]
    fn inquiry_parse_non_removable() {
        let mut buf = make_inquiry_buf();
        buf[1] = 0x00; // RMB = 0
        let inq = InquiryData::parse(&buf).unwrap();
        assert!(!inq.removable);
    }

    /// byte 0 low-5-bits extraction: mask out peripheral qualifier (bits 7:5).
    #[test]
    fn inquiry_parse_device_type_masked() {
        let mut buf = make_inquiry_buf();
        buf[0] = 0xE5; // peripheral qualifier = 0b111, device type = 0x05 (CD/DVD)
        let inq = InquiryData::parse(&buf).unwrap();
        assert_eq!(inq.peripheral_device_type, 0x05);
    }

    /// Short buffer returns None.
    #[test]
    fn inquiry_parse_short_returns_none() {
        let buf = [0u8; 35]; // one byte short
        assert!(InquiryData::parse(&buf).is_none());
    }

    /// Empty buffer returns None without panicking.
    #[test]
    fn inquiry_parse_empty_returns_none() {
        assert!(InquiryData::parse(&[]).is_none());
    }

    // -----------------------------------------------------------------------
    // GET_MAX_LUN control-request encoding test
    // -----------------------------------------------------------------------

    /// `get_max_lun(0)` encodes to the 8 SETUP bytes `A1 FE 00 00 00 00 01 00`.
    #[test]
    fn get_max_lun_encoding() {
        let pkt = get_max_lun(0);
        // bmRequestType = 0xA1
        assert_eq!(pkt.bm_request_type, 0xA1);
        // bRequest = 0xFE (GET_MAX_LUN)
        assert_eq!(pkt.b_request, 0xFE);
        // wValue = 0
        assert_eq!(pkt.w_value, 0);
        // wIndex = 0 (interface 0)
        assert_eq!(pkt.w_index, 0);
        // wLength = 1
        assert_eq!(pkt.w_length, 1);

        // Verify the 8-byte wire encoding matches A1 FE 00 00 00 00 01 00
        let wire = pkt.as_u64().to_le_bytes();
        assert_eq!(wire, [0xA1, 0xFE, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00]);
    }

    /// `get_max_lun` with a non-zero interface routes to the correct wIndex.
    #[test]
    fn get_max_lun_nonzero_interface() {
        let pkt = get_max_lun(2);
        assert_eq!(pkt.w_index, 2);
    }

    // -----------------------------------------------------------------------
    // UAS Command IU encoding tests
    // -----------------------------------------------------------------------

    /// Command IU encodes to exactly 32 bytes with the correct IU ID.
    #[test]
    fn uas_command_iu_encode_length_and_iuid() {
        let cdb = cdb_read10(0x0000_1234, 1);
        let iu = CommandIu::new(1, [0u8; 8], &cdb, 0);
        let wire = iu.encode();
        assert_eq!(wire.len(), UAS_COMMAND_IU_LEN);
        assert_eq!(wire[0], UAS_IU_COMMAND, "byte 0 must be IU ID 0x01");
    }

    /// Tag is encoded big-endian at bytes 2–3.
    #[test]
    fn uas_command_iu_encode_tag_big_endian() {
        let cdb = cdb_read10(0, 1);
        let iu = CommandIu::new(0x1234, [0u8; 8], &cdb, 0);
        let wire = iu.encode();
        assert_eq!(wire[2], 0x12, "tag MSB at byte 2");
        assert_eq!(wire[3], 0x34, "tag LSB at byte 3");
    }

    /// Reserved bytes 1, 5, 6 are zero; Additional CDB Length (byte 7) is 0.
    #[test]
    fn uas_command_iu_encode_reserved_and_additional_cdb_len() {
        let cdb = cdb_read10(0, 1);
        let iu = CommandIu::new(1, [0u8; 8], &cdb, 0);
        let wire = iu.encode();
        assert_eq!(wire[1], 0x00, "byte 1 reserved");
        assert_eq!(wire[5], 0x00, "byte 5 reserved");
        assert_eq!(wire[6], 0x00, "byte 6 reserved");
        assert_eq!(wire[7], 0x00, "byte 7 Additional CDB Length = 0");
    }

    /// LUN bytes appear at offsets 8–15.
    #[test]
    fn uas_command_iu_encode_lun_placement() {
        let cdb = cdb_read10(0, 1);
        let lun: [u8; 8] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let iu = CommandIu::new(1, lun, &cdb, 0);
        let wire = iu.encode();
        assert_eq!(&wire[8..16], &lun, "LUN must be at bytes 8–15");
    }

    /// CDB is placed at bytes 16–31 (zero-padded for shorter CDBs).
    #[test]
    fn uas_command_iu_encode_cdb_at_offset_16() {
        let cdb = cdb_read10(0xABCD_EF01, 0x0008);
        let iu = CommandIu::new(5, [0u8; 8], &cdb, 0);
        let wire = iu.encode();
        assert_eq!(&wire[16..26], &cdb, "10-byte CDB must be at bytes 16–25");
        assert_eq!(&wire[26..32], &[0u8; 6], "bytes 26–31 must be zero-padded");
    }

    /// Full byte-exact check for a READ(10) Command IU.
    #[test]
    fn uas_command_iu_encode_read10_exact() {
        // READ(10) LBA=0x00000000, blocks=1
        let cdb = cdb_read10(0x0000_0000, 1);
        // tag=0x0001, LUN=all zeros, task_attr=0 (simple)
        let iu = CommandIu::new(0x0001, [0u8; 8], &cdb, 0x00);
        let wire = iu.encode();

        // byte 0: IU ID = 0x01
        assert_eq!(wire[0], 0x01);
        // byte 1: reserved = 0x00
        assert_eq!(wire[1], 0x00);
        // bytes 2–3: tag = 0x0001 (BE)
        assert_eq!(&wire[2..4], &[0x00, 0x01]);
        // byte 4: priority|attr = 0x00
        assert_eq!(wire[4], 0x00);
        // bytes 5–7: reserved + additional CDB len = 0x00
        assert_eq!(&wire[5..8], &[0x00, 0x00, 0x00]);
        // bytes 8–15: LUN = all zeros
        assert_eq!(&wire[8..16], &[0u8; 8]);
        // bytes 16–25: READ(10) CDB (opcode 0x28, LBA 0, blocks 1)
        assert_eq!(wire[16], 0x28); // READ(10) opcode
        assert_eq!(&wire[17..22], &[0x00, 0x00, 0x00, 0x00, 0x00]); // flags+LBA
        assert_eq!(wire[22], 0x00); // group
        assert_eq!(&wire[23..25], &[0x00, 0x01]); // transfer length = 1 block
        assert_eq!(wire[25], 0x00); // control
        // bytes 26–31: zero padding
        assert_eq!(&wire[26..32], &[0u8; 6]);
    }

    // -----------------------------------------------------------------------
    // UAS Sense IU parsing tests
    // -----------------------------------------------------------------------

    fn make_sense_iu_buf(tag: u16, status: u8, sense_data: &[u8]) -> alloc::vec::Vec<u8> {
        let sense_len = sense_data.len() as u16;
        let mut buf = alloc::vec![0u8; 16 + sense_data.len()];
        buf[0] = UAS_IU_SENSE;
        buf[1] = 0x00; // reserved
        buf[2] = (tag >> 8) as u8;
        buf[3] = (tag & 0xFF) as u8;
        // status qualifier = 0 (bytes 4–5)
        buf[4] = 0x00;
        buf[5] = 0x00;
        buf[6] = status;
        // bytes 7–13: reserved
        buf[14] = (sense_len >> 8) as u8;
        buf[15] = (sense_len & 0xFF) as u8;
        buf[16..].copy_from_slice(sense_data);
        buf
    }

    /// A valid Sense IU with known sense data parses correctly.
    #[test]
    fn uas_sense_iu_parse_valid() {
        let sense = [
            0x70, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x3A, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        let buf = make_sense_iu_buf(0x0042, 0x02, &sense);
        let iu = SenseIu::parse(&buf).expect("valid Sense IU must parse");
        assert_eq!(iu.tag, 0x0042);
        assert_eq!(iu.status, 0x02); // CHECK CONDITION
        assert_eq!(iu.sense_data_length, sense.len() as u16);
        assert_eq!(&iu.sense_data[..sense.len()], &sense);
    }

    /// Tag round-trips big-endian correctly.
    #[test]
    fn uas_sense_iu_parse_tag_big_endian() {
        let buf = make_sense_iu_buf(0xBEEF, 0x00, &[]);
        let iu = SenseIu::parse(&buf).unwrap();
        assert_eq!(iu.tag, 0xBEEF);
    }

    /// GOOD status (0x00) parses without error.
    #[test]
    fn uas_sense_iu_parse_good_status() {
        let buf = make_sense_iu_buf(1, 0x00, &[]);
        let iu = SenseIu::parse(&buf).unwrap();
        assert_eq!(iu.status, 0x00);
        assert_eq!(iu.sense_data_length, 0);
    }

    /// A buffer shorter than 16 bytes returns None.
    #[test]
    fn uas_sense_iu_parse_short_returns_none() {
        let buf = [UAS_IU_SENSE; 15]; // one byte short
        assert!(SenseIu::parse(&buf).is_none());
    }

    /// An empty buffer returns None without panicking.
    #[test]
    fn uas_sense_iu_parse_empty_returns_none() {
        assert!(SenseIu::parse(&[]).is_none());
    }

    /// Wrong IU ID returns None.
    #[test]
    fn uas_sense_iu_parse_wrong_iuid_returns_none() {
        let mut buf = make_sense_iu_buf(1, 0x00, &[]);
        buf[0] = 0xFF; // wrong IU ID
        assert!(SenseIu::parse(&buf).is_none());
    }

    // -----------------------------------------------------------------------
    // UAS Response IU parsing tests
    // -----------------------------------------------------------------------

    fn make_response_iu_buf(tag: u16, response_code: u8) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0] = UAS_IU_RESPONSE;
        buf[1] = 0x00;
        buf[2] = (tag >> 8) as u8;
        buf[3] = (tag & 0xFF) as u8;
        // additional response info bytes 4–6 = 0
        buf[7] = response_code;
        buf
    }

    /// A valid Response IU parses tag and response code correctly.
    #[test]
    fn uas_response_iu_parse_valid() {
        let buf = make_response_iu_buf(0x0007, UAS_RESPONSE_TASK_MGMT_FUNCTION_COMPLETE);
        let iu = ResponseIu::parse(&buf).expect("valid Response IU must parse");
        assert_eq!(iu.tag, 0x0007);
        assert_eq!(iu.response_code, UAS_RESPONSE_TASK_MGMT_FUNCTION_COMPLETE);
    }

    /// Tag is decoded big-endian.
    #[test]
    fn uas_response_iu_parse_tag_big_endian() {
        let buf = make_response_iu_buf(0xABCD, UAS_RESPONSE_TMF_SUCCEEDED);
        let iu = ResponseIu::parse(&buf).unwrap();
        assert_eq!(iu.tag, 0xABCD);
        assert_eq!(iu.response_code, UAS_RESPONSE_TMF_SUCCEEDED);
    }

    /// All `UAS_RESPONSE_*` constants decode without aliasing.
    #[test]
    fn uas_response_iu_response_code_constants() {
        let codes = [
            UAS_RESPONSE_TASK_MGMT_FUNCTION_COMPLETE,
            UAS_RESPONSE_INVALID_IU,
            UAS_RESPONSE_TMF_NOT_SUPPORTED,
            UAS_RESPONSE_TMF_FAILED,
            UAS_RESPONSE_TMF_SUCCEEDED,
            UAS_RESPONSE_OVERLAPPED_TAG,
        ];
        // Verify all constants are distinct.
        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(
                    codes[i], codes[j],
                    "response code constants must be distinct"
                );
            }
        }
    }

    /// Truncated buffer (< 8 bytes) returns None.
    #[test]
    fn uas_response_iu_parse_short_returns_none() {
        let buf = [UAS_IU_RESPONSE; 7]; // one byte short
        assert!(ResponseIu::parse(&buf).is_none());
    }

    /// Wrong IU ID returns None.
    #[test]
    fn uas_response_iu_parse_wrong_iuid_returns_none() {
        let mut buf = make_response_iu_buf(1, 0x00);
        buf[0] = 0xFF;
        assert!(ResponseIu::parse(&buf).is_none());
    }

    // -----------------------------------------------------------------------
    // UAS Read Ready / Write Ready IU tests
    // -----------------------------------------------------------------------

    fn make_ready_iu(iu_id: u8, tag: u16) -> [u8; 4] {
        [iu_id, 0x00, (tag >> 8) as u8, (tag & 0xFF) as u8]
    }

    /// Read Ready IU extracts the tag correctly.
    #[test]
    fn uas_read_ready_iu_parse_tag() {
        let buf = make_ready_iu(UAS_IU_READ_READY, 0x0003);
        let tag = parse_read_ready_iu(&buf).expect("Read Ready IU must parse");
        assert_eq!(tag, 0x0003);
    }

    /// Write Ready IU extracts the tag correctly.
    #[test]
    fn uas_write_ready_iu_parse_tag() {
        let buf = make_ready_iu(UAS_IU_WRITE_READY, 0xF00D);
        let tag = parse_write_ready_iu(&buf).expect("Write Ready IU must parse");
        assert_eq!(tag, 0xF00D);
    }

    /// Read Ready IU with wrong IU ID returns None.
    #[test]
    fn uas_read_ready_iu_wrong_iuid_returns_none() {
        let buf = make_ready_iu(UAS_IU_WRITE_READY, 1); // wrong: write, not read
        assert!(parse_read_ready_iu(&buf).is_none());
    }

    /// Write Ready IU with wrong IU ID returns None.
    #[test]
    fn uas_write_ready_iu_wrong_iuid_returns_none() {
        let buf = make_ready_iu(UAS_IU_READ_READY, 1); // wrong: read, not write
        assert!(parse_write_ready_iu(&buf).is_none());
    }

    /// Short buffer (< 4 bytes) returns None for both ready IU helpers.
    #[test]
    fn uas_ready_iu_short_buffer_returns_none() {
        let buf = [UAS_IU_READ_READY; 3];
        assert!(parse_read_ready_iu(&buf).is_none());
        let buf2 = [UAS_IU_WRITE_READY; 3];
        assert!(parse_write_ready_iu(&buf2).is_none());
    }

    /// Tag is decoded big-endian in both ready IU helpers.
    #[test]
    fn uas_ready_iu_tag_big_endian() {
        let buf = make_ready_iu(UAS_IU_READ_READY, 0x1234);
        let tag = parse_read_ready_iu(&buf).unwrap();
        assert_eq!(tag, 0x1234);
    }

    // -----------------------------------------------------------------------
    // UAS Task Management IU encoding tests
    // -----------------------------------------------------------------------

    /// Task Management IU encodes to exactly 16 bytes with the correct IU ID.
    #[test]
    fn uas_task_mgmt_iu_encode_length_and_iuid() {
        let iu = TaskMgmtIu {
            tag: 1,
            tm_function: UAS_TMF_ABORT_TASK,
            task_tag: 99,
            lun: [0u8; 8],
        };
        let wire = iu.encode();
        assert_eq!(wire.len(), UAS_TASK_MGMT_IU_LEN);
        assert_eq!(wire[0], UAS_IU_TASK_MGMT, "byte 0 must be IU ID 0x05");
    }

    /// Byte-exact encoding for an ABORT_TASK Task Management IU.
    #[test]
    fn uas_task_mgmt_iu_encode_abort_task_exact() {
        // TM tag=0x0002, ABORT_TASK targeting task_tag=0x0001, LUN=0
        let iu = TaskMgmtIu {
            tag: 0x0002,
            tm_function: UAS_TMF_ABORT_TASK,
            task_tag: 0x0001,
            lun: [0u8; 8],
        };
        let wire = iu.encode();

        // byte 0: IU ID = 0x05
        assert_eq!(wire[0], 0x05);
        // byte 1: reserved = 0x00
        assert_eq!(wire[1], 0x00);
        // bytes 2–3: tag = 0x0002 (BE)
        assert_eq!(&wire[2..4], &[0x00, 0x02]);
        // byte 4: TM Function = ABORT_TASK = 0x01
        assert_eq!(wire[4], UAS_TMF_ABORT_TASK);
        // byte 5: reserved = 0x00
        assert_eq!(wire[5], 0x00);
        // bytes 6–7: task tag = 0x0001 (BE)
        assert_eq!(&wire[6..8], &[0x00, 0x01]);
        // bytes 8–15: LUN = all zeros
        assert_eq!(&wire[8..16], &[0u8; 8]);
    }

    /// LOGICAL_UNIT_RESET encodes the TM Function byte correctly.
    #[test]
    fn uas_task_mgmt_iu_encode_logical_unit_reset() {
        let lun: [u8; 8] = [0xC1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let iu = TaskMgmtIu {
            tag: 0x0010,
            tm_function: UAS_TMF_LOGICAL_UNIT_RESET,
            task_tag: 0x0000,
            lun,
        };
        let wire = iu.encode();
        assert_eq!(wire[4], UAS_TMF_LOGICAL_UNIT_RESET);
        assert_eq!(&wire[8..16], &lun);
    }

    /// Tags and task_tags encode big-endian correctly.
    #[test]
    fn uas_task_mgmt_iu_encode_tags_big_endian() {
        let iu = TaskMgmtIu {
            tag: 0xABCD,
            tm_function: UAS_TMF_ABORT_TASK,
            task_tag: 0xEF01,
            lun: [0u8; 8],
        };
        let wire = iu.encode();
        assert_eq!(&wire[2..4], &[0xAB, 0xCD], "tag must be big-endian");
        assert_eq!(&wire[6..8], &[0xEF, 0x01], "task_tag must be big-endian");
    }
}
