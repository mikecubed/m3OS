//! HID Report-Descriptor item parser — skeleton (Phase 78c Track A.3).
//!
//! # Status: skeleton, host-tested only
//!
//! This module is **not wired to any live device**. The HID Boot Protocol
//! (see [`super::hid`]) covers every HID 1.0 keyboard and mouse without a
//! Report Descriptor, so Report Protocol parsing is intentionally deferred
//! from live use. See the phase-78c design doc section "Deferred Until Later".
//!
//! # What is implemented
//!
//! A minimal parser for *short items* (the common case in USB HID descriptors;
//! long items are skipped). Each item prefix byte encodes:
//!
//! * **bSize** — bits 1:0 — number of following data bytes (0, 1, 2, or 4).
//! * **bType** — bits 3:2 — item type (`Main=0`, `Global=1`, `Local=2`,
//!   reserved=3).
//! * **bTag** — bits 7:4 — which item.
//!
//! The parser maintains a small Global state (Usage Page, Report Size, Report
//! Count) and a Local state (Usage), and emits a [`ReportField`] for each
//! Main Input item encountered.
//!
//! # Limitations (acceptable for a skeleton)
//!
//! * Only short items are parsed; long items (prefix byte 0xFE) are skipped.
//! * Report ID is ignored (single-report-per-interface assumption, adequate
//!   for boot-class devices even when parsed in Report Protocol mode).
//! * Only the first Usage per field is captured (arrays and range usages are
//!   not modelled).
//! * Logical Min/Max, Physical Min/Max, and other global items are parsed but
//!   not stored.

extern crate alloc;

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One logical field in a HID report, derived from a Main `Input` item.
///
/// Fields are returned in declaration order by [`parse_report_descriptor`].
/// The consumer can use `bit_offset` and `bit_size` to extract field values
/// from a raw report byte slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportField {
    /// HID Usage Page (from the most recent Global Usage Page item).
    pub usage_page: u16,
    /// HID Usage (from the most recent Local Usage item, or 0 if none).
    pub usage: u16,
    /// Bit offset of this field from the start of the report.
    pub bit_offset: usize,
    /// Number of bits occupied by this field (Report Size * Report Count).
    pub bit_size: usize,
}

// ---------------------------------------------------------------------------
// Internal item parser helpers
// ---------------------------------------------------------------------------

/// Item type tag (bits 3:2 of the item prefix byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemType {
    Main,
    Global,
    Local,
    Reserved,
}

/// Parsed short item.
#[derive(Debug, Clone, Copy)]
struct ShortItem {
    item_type: ItemType,
    /// Bits 7:4 of the prefix byte.
    tag: u8,
    /// Up to 4 data bytes, stored in `data[..size]`.
    data: [u8; 4],
    /// Number of valid bytes in `data` (0, 1, 2, or 4).
    size: usize,
}

impl ShortItem {
    /// Interpret the data bytes as a little-endian `u32`.
    fn data_u32(self) -> u32 {
        match self.size {
            0 => 0,
            1 => self.data[0] as u32,
            2 => u16::from_le_bytes([self.data[0], self.data[1]]) as u32,
            4 => u32::from_le_bytes(self.data),
            _ => 0,
        }
    }
}

/// Walk `raw` and yield each short item in turn.
///
/// Returns an iterator-like function that advances a `&mut usize` cursor.
fn next_item(raw: &[u8], pos: &mut usize) -> Option<ShortItem> {
    loop {
        if *pos >= raw.len() {
            return None;
        }
        let prefix = raw[*pos];
        *pos += 1;

        // Long item: prefix == 0xFE. Skip: next byte is data_size, then that
        // many bytes of data, then a tag byte.
        if prefix == 0xFE {
            if *pos >= raw.len() {
                return None;
            }
            let long_size = raw[*pos] as usize;
            // Skip: data_size byte + tag byte + data bytes.
            *pos += 1 + 1 + long_size;
            continue;
        }

        let raw_size = prefix & 0x03;
        let size: usize = match raw_size {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4, // bSize=3 means 4 data bytes in the HID spec.
            _ => unreachable!(),
        };
        let item_type = match (prefix >> 2) & 0x03 {
            0 => ItemType::Main,
            1 => ItemType::Global,
            2 => ItemType::Local,
            _ => ItemType::Reserved,
        };
        let tag = prefix >> 4;

        // Read data bytes.
        if *pos + size > raw.len() {
            return None; // Truncated descriptor — bail.
        }
        let mut data = [0u8; 4];
        data[..size].copy_from_slice(&raw[*pos..*pos + size]);
        *pos += size;

        return Some(ShortItem {
            item_type,
            tag,
            data,
            size,
        });
    }
}

// ---------------------------------------------------------------------------
// HID item tag constants
// ---------------------------------------------------------------------------

// Global tags (bType = 1).
/// Global — Usage Page (tag 0).
const TAG_GLOBAL_USAGE_PAGE: u8 = 0x0;
/// Global — Logical Minimum (tag 1).
const TAG_GLOBAL_LOGICAL_MIN: u8 = 0x1;
/// Global — Logical Maximum (tag 2).
const TAG_GLOBAL_LOGICAL_MAX: u8 = 0x2;
/// Global — Report Size (tag 7).
const TAG_GLOBAL_REPORT_SIZE: u8 = 0x7;
/// Global — Report Count (tag 9).
const TAG_GLOBAL_REPORT_COUNT: u8 = 0x9;

// Local tags (bType = 2).
/// Local — Usage (tag 0).
const TAG_LOCAL_USAGE: u8 = 0x0;

// Main tags (bType = 0).
/// Main — Input (tag 8).
const TAG_MAIN_INPUT: u8 = 0x8;
/// Main — Output (tag 9).
const TAG_MAIN_OUTPUT: u8 = 0x9;
/// Main — Feature (tag 11).
const TAG_MAIN_FEATURE: u8 = 0xB;

// ---------------------------------------------------------------------------
// parse_report_descriptor
// ---------------------------------------------------------------------------

/// Parse a raw HID Report Descriptor and return one [`ReportField`] per
/// Main `Input` item in declaration order.
///
/// # Note — skeleton scope
///
/// This is a skeleton intended for host testing only (see module-level doc).
/// It handles the most common subset needed to verify the parsing logic with
/// hand-crafted test descriptors. Production use would require handling:
/// report IDs, multi-usage arrays, logical min/max sign extension, long
/// items, Usage Min/Max ranges, and collection items.
pub fn parse_report_descriptor(raw: &[u8]) -> Vec<ReportField> {
    let mut fields = Vec::new();
    let mut pos = 0usize;

    // Global state.
    let mut usage_page: u16 = 0;
    let mut report_size: u32 = 0;
    let mut report_count: u32 = 0;
    // (Logical min/max are parsed but not stored — not needed for field layout.)
    let _logical_min: u32 = 0;
    let _logical_max: u32 = 0;

    // Local state (cleared after each Main item).
    let mut usage: u16 = 0;

    // Running bit offset into the report byte stream.
    let mut bit_offset: usize = 0;

    while let Some(item) = next_item(raw, &mut pos) {
        match item.item_type {
            ItemType::Global => match item.tag {
                TAG_GLOBAL_USAGE_PAGE => {
                    usage_page = item.data_u32() as u16;
                }
                TAG_GLOBAL_LOGICAL_MIN => { /* parsed but not stored */ }
                TAG_GLOBAL_LOGICAL_MAX => { /* parsed but not stored */ }
                TAG_GLOBAL_REPORT_SIZE => {
                    report_size = item.data_u32();
                }
                TAG_GLOBAL_REPORT_COUNT => {
                    report_count = item.data_u32();
                }
                _ => { /* other global items ignored in skeleton */ }
            },
            ItemType::Local => match item.tag {
                TAG_LOCAL_USAGE => {
                    // Only the first Usage per field is captured.
                    usage = item.data_u32() as u16;
                }
                _ => { /* other local items ignored */ }
            },
            ItemType::Main => {
                let total_bits = (report_size as usize).saturating_mul(report_count as usize);
                match item.tag {
                    TAG_MAIN_INPUT => {
                        if total_bits > 0 {
                            fields.push(ReportField {
                                usage_page,
                                usage,
                                bit_offset,
                                bit_size: total_bits,
                            });
                        }
                        bit_offset = bit_offset.saturating_add(total_bits);
                    }
                    TAG_MAIN_OUTPUT | TAG_MAIN_FEATURE => {
                        // Advance bit offset but do not emit a field.
                        bit_offset = bit_offset.saturating_add(total_bits);
                    }
                    _ => { /* Collection / End Collection etc. — ignore */ }
                }
                // Clear local state after any Main item (HID spec §6.2.2.8).
                usage = 0;
            }
            ItemType::Reserved => { /* skip */ }
        }
    }

    fields
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-crafted minimal descriptor: one 8-bit field (Usage Page 0x01,
    /// Usage 0x30 = X axis), Report Size=8, Report Count=1, then Input.
    ///
    /// Item encoding (short items):
    ///   Usage Page 1  : 05 01  (Global, tag 0, size 1, data=0x01)
    ///   Usage 0x30    : 09 30  (Local,  tag 0, size 1, data=0x30)
    ///   Logical Min 0 : 15 00  (Global, tag 1, size 1, data=0x00)
    ///   Logical Max 255: 25 FF (Global, tag 2, size 1, data=0xFF)
    ///   Report Size 8 : 75 08  (Global, tag 7, size 1, data=0x08)
    ///   Report Count 1: 95 01  (Global, tag 9, size 1, data=0x01)
    ///   Input (Data,Var,Abs): 81 02 (Main, tag 8, size 1, data=0x02)
    const ONE_FIELD_8BIT: &[u8] = &[
        0x05, 0x01, // Usage Page = Generic Desktop (0x01)
        0x09, 0x30, // Usage = X (0x30)
        0x15, 0x00, // Logical Minimum = 0
        0x25, 0xFF, // Logical Maximum = 255
        0x75, 0x08, // Report Size = 8
        0x95, 0x01, // Report Count = 1
        0x81, 0x02, // Input (Data, Variable, Absolute)
    ];

    /// Hand-crafted descriptor: three 1-bit button fields (Usage Page 0x09,
    /// Usages 1/2/3), each Report Size=1, Report Count=1, each with Input.
    ///
    /// Note: we encode three separate Usage+Input pairs to test that each
    /// emits its own ReportField with the correct bit offset.
    ///
    /// Item encoding:
    ///   Usage Page 9  : 05 09
    ///   Usage 1       : 09 01
    ///   Logical Min 0 : 15 00
    ///   Logical Max 1 : 25 01
    ///   Report Size 1 : 75 01
    ///   Report Count 1: 95 01
    ///   Input         : 81 02   → bit_offset=0, bit_size=1, usage=1
    ///   Usage 2       : 09 02
    ///   Input         : 81 02   → bit_offset=1, bit_size=1, usage=2
    ///   Usage 3       : 09 03
    ///   Input         : 81 02   → bit_offset=2, bit_size=1, usage=3
    const THREE_BUTTON_FIELDS: &[u8] = &[
        0x05, 0x09, // Usage Page = Button (0x09)
        0x09, 0x01, // Usage = Button 1
        0x15, 0x00, // Logical Minimum = 0
        0x25, 0x01, // Logical Maximum = 1
        0x75, 0x01, // Report Size = 1
        0x95, 0x01, // Report Count = 1
        0x81, 0x02, // Input → field 0: offset=0, size=1, usage=0x01
        0x09, 0x02, // Usage = Button 2
        0x81, 0x02, // Input → field 1: offset=1, size=1, usage=0x02
        0x09, 0x03, // Usage = Button 3
        0x81, 0x02, // Input → field 2: offset=2, size=1, usage=0x03
    ];

    /// One 8-bit field parses to the expected ReportField values.
    #[test]
    fn one_field_8bit_correct() {
        let fields = parse_report_descriptor(ONE_FIELD_8BIT);
        assert_eq!(fields.len(), 1, "expected exactly 1 field");
        let f = &fields[0];
        assert_eq!(f.usage_page, 0x0001); // Generic Desktop
        assert_eq!(f.usage, 0x0030); // X axis
        assert_eq!(f.bit_offset, 0);
        assert_eq!(f.bit_size, 8); // Report Size=8, Report Count=1
    }

    /// Three 1-bit button fields parse to sequential bit offsets.
    #[test]
    fn three_button_fields_correct_offsets() {
        let fields = parse_report_descriptor(THREE_BUTTON_FIELDS);
        assert_eq!(fields.len(), 3, "expected 3 fields");

        assert_eq!(fields[0].usage_page, 0x0009); // Button page
        assert_eq!(fields[0].usage, 0x0001); // Button 1
        assert_eq!(fields[0].bit_offset, 0);
        assert_eq!(fields[0].bit_size, 1);

        assert_eq!(fields[1].usage_page, 0x0009);
        assert_eq!(fields[1].usage, 0x0002); // Button 2
        assert_eq!(fields[1].bit_offset, 1);
        assert_eq!(fields[1].bit_size, 1);

        assert_eq!(fields[2].usage_page, 0x0009);
        assert_eq!(fields[2].usage, 0x0003); // Button 3
        assert_eq!(fields[2].bit_offset, 2);
        assert_eq!(fields[2].bit_size, 1);
    }

    /// An empty descriptor produces no fields and does not panic.
    #[test]
    fn empty_descriptor_returns_empty() {
        let fields = parse_report_descriptor(&[]);
        assert!(fields.is_empty());
    }

    /// A truncated descriptor (starts with a valid prefix but runs out of data)
    /// does not panic and returns whatever complete fields were parsed before
    /// the truncation.
    #[test]
    fn truncated_descriptor_does_not_panic() {
        // Truncate the 8-bit field descriptor mid-way.
        let truncated = &ONE_FIELD_8BIT[..6];
        let _fields = parse_report_descriptor(truncated); // must not panic
    }

    /// A descriptor with only Global/Local items and no Main Input items
    /// returns no fields.
    #[test]
    fn no_main_items_yields_no_fields() {
        // Usage Page + Usage + Report Size + Report Count — no Input.
        let raw: &[u8] = &[
            0x05, 0x01, // Usage Page
            0x09, 0x30, // Usage
            0x75, 0x08, // Report Size
            0x95, 0x01, // Report Count
        ];
        let fields = parse_report_descriptor(raw);
        assert!(fields.is_empty());
    }
}
