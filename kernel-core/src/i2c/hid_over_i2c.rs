//! Phase 102 Track B — **HID-over-I2C v1.0** transport codec, pure logic.
//!
//! The wire framing that rides on top of the Track A DesignWare master
//! ([`super::designware`]) — OpenBSD `ihidev(4)` (`sys/dev/i2c/ihidev.c`) as the
//! structural reference. Two pieces, both host-tested against byte vectors so
//! the framing is pinned independently of hardware:
//!
//! 1. the fixed **HID Descriptor** (`I2cHidDescriptor`) read from the device's
//!    descriptor register — it hands back the report-descriptor / input /
//!    output / command / data register addresses + max lengths + IDs;
//! 2. the **command frames** (RESET, SET_POWER, GET_REPORT) written to the
//!    command register, and the 2-byte-length-prefixed **input-report** parse
//!    read from the input register.
//!
//! The bytes these builders return are exactly the `write` buffer the daemon
//! hands to [`super::designware::plan_transfer`] (they already include the
//! 2-byte command-register address prefix).

use alloc::vec::Vec;

// ─── HID Descriptor (fixed 30-byte little-endian layout) ─────────────────────

/// The fixed HID-over-I2C **HID Descriptor**, read from the device's descriptor
/// register (whose address comes from ACPI for the specific device). All fields
/// are little-endian. Register fields (`*_register`) are I2C register addresses
/// used for subsequent reads/writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct I2cHidDescriptor {
    /// `wHIDDescLength` — total descriptor length (30 for v1.0).
    pub hid_desc_length: u16,
    /// `bcdVersion` — spec version (0x0100 for v1.0).
    pub bcd_version: u16,
    /// `wReportDescLength` — length of the HID report descriptor.
    pub report_desc_length: u16,
    /// `wReportDescRegister` — register the report descriptor is read from.
    pub report_desc_register: u16,
    /// `wInputRegister` — register input reports are read from.
    pub input_register: u16,
    /// `wMaxInputLength` — max input-report length (incl. the 2 length bytes).
    pub max_input_length: u16,
    /// `wOutputRegister` — register output reports are written to.
    pub output_register: u16,
    /// `wMaxOutputLength` — max output-report length.
    pub max_output_length: u16,
    /// `wCommandRegister` — register command frames are written to.
    pub command_register: u16,
    /// `wDataRegister` — register GET/SET_REPORT data transfers use.
    pub data_register: u16,
    /// `wVendorID`.
    pub vendor_id: u16,
    /// `wProductID`.
    pub product_id: u16,
    /// `wVersionID`.
    pub version_id: u16,
}

/// The fixed on-wire descriptor length for HID-over-I2C v1.0.
pub const I2C_HID_DESC_LENGTH: usize = 30;

#[inline]
fn u16le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

impl I2cHidDescriptor {
    /// Parse a HID Descriptor from the (little-endian) bytes read out of the
    /// descriptor register. Returns `None` if the buffer is shorter than the
    /// fixed 30-byte layout. A caller should additionally sanity-check
    /// [`Self::hid_desc_length`] == 30 and [`Self::bcd_version`] == 0x0100.
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < I2C_HID_DESC_LENGTH {
            return None;
        }
        Some(Self {
            hid_desc_length: u16le(buf, 0),
            bcd_version: u16le(buf, 2),
            report_desc_length: u16le(buf, 4),
            report_desc_register: u16le(buf, 6),
            input_register: u16le(buf, 8),
            max_input_length: u16le(buf, 10),
            output_register: u16le(buf, 12),
            max_output_length: u16le(buf, 14),
            command_register: u16le(buf, 16),
            data_register: u16le(buf, 18),
            vendor_id: u16le(buf, 20),
            product_id: u16le(buf, 22),
            version_id: u16le(buf, 24),
            // bytes 26..30 reserved.
        })
    }

    /// Whether the descriptor looks structurally valid (length + version) —
    /// a cheap bring-up sanity gate before trusting the register addresses.
    #[inline]
    pub fn looks_valid(&self) -> bool {
        self.hid_desc_length as usize == I2C_HID_DESC_LENGTH && self.bcd_version == 0x0100
    }
}

// ─── Command frames (written to the command register) ────────────────────────

/// HID-over-I2C command opcodes (byte1 low nibble).
pub const OPCODE_RESET: u8 = 0x01;
pub const OPCODE_GET_REPORT: u8 = 0x02;
pub const OPCODE_SET_REPORT: u8 = 0x03;
pub const OPCODE_SET_POWER: u8 = 0x08;

/// Report types (command byte0 bits 4..=5).
pub const REPORT_TYPE_INPUT: u8 = 0x01;
pub const REPORT_TYPE_OUTPUT: u8 = 0x02;
pub const REPORT_TYPE_FEATURE: u8 = 0x03;

/// Power states for [`set_power_command`] (SET_POWER byte0 bits 0..=1).
pub const POWER_ON: u8 = 0x00;
pub const POWER_SLEEP: u8 = 0x01;

/// The command byte0 = (reportType << 4) | reportID nibble. When `report_id`
/// is ≥ 0x0F the nibble is 0x0F and the real ID rides a trailing byte — encoded
/// by the callers below.
#[inline]
fn cmd_byte0(report_type: u8, report_id: u8) -> u8 {
    ((report_type & 0x03) << 4) | (report_id.min(0x0F) & 0x0F)
}

/// RESET command: write `[cmdReg_lo, cmdReg_hi, 0x00, OPCODE_RESET]` to the
/// command register. The device signals reset-complete with a zero-length input
/// report (see [`parse_input_report`] → [`InputReport::Empty`]).
pub fn reset_command(command_register: u16) -> Vec<u8> {
    let [lo, hi] = command_register.to_le_bytes();
    let mut v = Vec::with_capacity(4);
    v.extend_from_slice(&[lo, hi, 0x00, OPCODE_RESET]);
    v
}

/// SET_POWER command: `[cmdReg_lo, cmdReg_hi, powerState, OPCODE_SET_POWER]`.
/// Use [`POWER_ON`] to wake the device for input, [`POWER_SLEEP`] to idle it.
pub fn set_power_command(command_register: u16, power_state: u8) -> Vec<u8> {
    let [lo, hi] = command_register.to_le_bytes();
    let mut v = Vec::with_capacity(4);
    v.extend_from_slice(&[lo, hi, power_state & 0x03, OPCODE_SET_POWER]);
    v
}

/// GET_REPORT command: request `report_id` of `report_type` be placed at the
/// data register (read back separately). Layout:
/// `[cmdReg_lo, cmdReg_hi, byte0, OPCODE_GET_REPORT, (idExt?), dataReg_lo, dataReg_hi]`,
/// where `byte0` carries the report type + a report-ID nibble, and — when
/// `report_id >= 0x0F` — an extra `idExt` byte carries the full ID (v1.0 §7.2.2).
pub fn get_report_command(
    command_register: u16,
    data_register: u16,
    report_type: u8,
    report_id: u8,
) -> Vec<u8> {
    let [clo, chi] = command_register.to_le_bytes();
    let [dlo, dhi] = data_register.to_le_bytes();
    let mut v = Vec::with_capacity(7);
    v.extend_from_slice(&[clo, chi, cmd_byte0(report_type, report_id), OPCODE_GET_REPORT]);
    if report_id >= 0x0F {
        v.push(report_id); // extended report-ID byte
    }
    v.extend_from_slice(&[dlo, dhi]);
    v
}

// ─── Input-report parse (read from the input register) ───────────────────────

/// The result of parsing an input-register read. HID-over-I2C prefixes every
/// input report with a 2-byte little-endian **total length** (including the 2
/// length bytes themselves).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputReport<'a> {
    /// A zero (or 2-byte-only) length prefix — the reset-complete sentinel, or
    /// a spurious interrupt with no data. No report body.
    Empty,
    /// The report body (which may itself begin with a 1-byte report ID). The
    /// slice is clamped to what was actually read, so a truncated read yields
    /// the available prefix rather than reading past the buffer.
    Report(&'a [u8]),
}

/// Parse an input-register read `raw = [len_lo, len_hi, body...]`. Returns
/// `None` only when fewer than 2 bytes were read (the length prefix itself is
/// incomplete). A length of 0 — or 2 (header only) — is [`InputReport::Empty`]
/// (reset-complete / no data). Otherwise the body is `raw[2..len]`, clamped to
/// `raw.len()` so an under-read never panics.
pub fn parse_input_report(raw: &[u8]) -> Option<InputReport<'_>> {
    if raw.len() < 2 {
        return None;
    }
    let total = u16le(raw, 0) as usize;
    if total <= 2 {
        return Some(InputReport::Empty);
    }
    let end = total.min(raw.len());
    Some(InputReport::Report(&raw[2..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic Precision-Touchpad HID Descriptor (Elan-shaped): descriptor
    /// register content the daemon reads back on bring-up.
    fn sample_desc_bytes() -> [u8; I2C_HID_DESC_LENGTH] {
        let mut b = [0u8; I2C_HID_DESC_LENGTH];
        b[0..2].copy_from_slice(&30u16.to_le_bytes()); // wHIDDescLength
        b[2..4].copy_from_slice(&0x0100u16.to_le_bytes()); // bcdVersion
        b[4..6].copy_from_slice(&0x01D9u16.to_le_bytes()); // wReportDescLength
        b[6..8].copy_from_slice(&0x0002u16.to_le_bytes()); // wReportDescRegister
        b[8..10].copy_from_slice(&0x0003u16.to_le_bytes()); // wInputRegister
        b[10..12].copy_from_slice(&0x0040u16.to_le_bytes()); // wMaxInputLength
        b[12..14].copy_from_slice(&0x0004u16.to_le_bytes()); // wOutputRegister
        b[14..16].copy_from_slice(&0x0040u16.to_le_bytes()); // wMaxOutputLength
        b[16..18].copy_from_slice(&0x0005u16.to_le_bytes()); // wCommandRegister
        b[18..20].copy_from_slice(&0x0006u16.to_le_bytes()); // wDataRegister
        b[20..22].copy_from_slice(&0x04F3u16.to_le_bytes()); // wVendorID (Elan)
        b[22..24].copy_from_slice(&0x311Cu16.to_le_bytes()); // wProductID
        b[24..26].copy_from_slice(&0x0001u16.to_le_bytes()); // wVersionID
        b
    }

    #[test]
    fn descriptor_round_trips_registers_and_ids() {
        let d = I2cHidDescriptor::parse(&sample_desc_bytes()).unwrap();
        assert!(d.looks_valid());
        assert_eq!(d.hid_desc_length, 30);
        assert_eq!(d.bcd_version, 0x0100);
        assert_eq!(d.report_desc_length, 0x01D9);
        assert_eq!(d.report_desc_register, 0x0002);
        assert_eq!(d.input_register, 0x0003);
        assert_eq!(d.max_input_length, 0x0040);
        assert_eq!(d.command_register, 0x0005);
        assert_eq!(d.data_register, 0x0006);
        assert_eq!(d.vendor_id, 0x04F3); // the Dell touchpad (Elan)
        assert_eq!(d.product_id, 0x311C);
    }

    #[test]
    fn descriptor_rejects_short_buffer_and_bad_header() {
        assert_eq!(I2cHidDescriptor::parse(&[0u8; 29]), None);
        let mut b = sample_desc_bytes();
        b[0] = 0; // wHIDDescLength = 0 → structurally invalid
        assert!(!I2cHidDescriptor::parse(&b).unwrap().looks_valid());
    }

    #[test]
    fn reset_and_set_power_frames_match_wire_layout() {
        // Command register 0x0005 → LE [0x05, 0x00].
        assert_eq!(reset_command(0x0005), alloc::vec![0x05, 0x00, 0x00, 0x01]);
        assert_eq!(
            set_power_command(0x0005, POWER_ON),
            alloc::vec![0x05, 0x00, 0x00, 0x08]
        );
        assert_eq!(
            set_power_command(0x0005, POWER_SLEEP),
            alloc::vec![0x05, 0x00, 0x01, 0x08]
        );
    }

    #[test]
    fn get_report_frame_small_and_extended_ids() {
        // Feature report id 3: byte0 = (3<<4)|3 = 0x33, opcode 0x02, data reg
        // 0x0006 → [0x06,0x00].
        assert_eq!(
            get_report_command(0x0005, 0x0006, REPORT_TYPE_FEATURE, 3),
            alloc::vec![0x05, 0x00, 0x33, 0x02, 0x06, 0x00]
        );
        // Report id 0x20 (>=15): nibble 0x0F, then a trailing id byte before the
        // data register. byte0 = (1<<4)|0x0F = 0x1F.
        assert_eq!(
            get_report_command(0x0005, 0x0006, REPORT_TYPE_INPUT, 0x20),
            alloc::vec![0x05, 0x00, 0x1F, 0x02, 0x20, 0x06, 0x00]
        );
    }

    #[test]
    fn input_report_length_prefix_parse() {
        // A 6-byte report: total length 6 (incl. the 2 header bytes) → body is
        // the 4 payload bytes.
        let raw = [0x06, 0x00, 0x01, 0xAA, 0xBB, 0xCC];
        assert_eq!(
            parse_input_report(&raw),
            Some(InputReport::Report(&[0x01, 0xAA, 0xBB, 0xCC]))
        );
        // Reset-complete sentinel: length 0.
        assert_eq!(parse_input_report(&[0x00, 0x00]), Some(InputReport::Empty));
        // Header-only length 2: no body → Empty.
        assert_eq!(parse_input_report(&[0x02, 0x00]), Some(InputReport::Empty));
        // Fewer than 2 bytes: the length prefix itself is incomplete → None.
        assert_eq!(parse_input_report(&[0x06]), None);
        assert_eq!(parse_input_report(&[]), None);
    }

    #[test]
    fn input_report_truncated_read_clamps_without_panicking() {
        // Length prefix claims 10 bytes but only 5 were read: return the
        // available body (bytes 2..5) rather than reading past the buffer.
        let raw = [0x0A, 0x00, 0x11, 0x22, 0x33];
        assert_eq!(
            parse_input_report(&raw),
            Some(InputReport::Report(&[0x11, 0x22, 0x33]))
        );
    }
}
