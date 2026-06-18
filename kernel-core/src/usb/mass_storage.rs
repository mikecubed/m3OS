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
}
