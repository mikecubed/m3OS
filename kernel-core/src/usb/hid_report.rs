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
///
/// When the descriptor contains Report ID items, `report_id` carries the ID
/// that governs this field (0 when no Report ID item has been seen). The
/// `bit_offset` is measured within the report body for that ID (i.e. the
/// 1-byte Report ID prefix byte is NOT counted — offset 0 is the first bit
/// after the ID byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportField {
    /// HID Usage Page (from the most recent Global Usage Page item).
    pub usage_page: u16,
    /// HID Usage (from the most recent Local Usage item or Usage Min/Max
    /// range expansion, or 0 if none).
    pub usage: u16,
    /// Bit offset of this field within its report (body after the Report ID
    /// prefix byte, or from the start of the single report when no IDs are
    /// used). Reset to 0 at each new Report ID.
    pub bit_offset: usize,
    /// Number of bits occupied by this field (Report Size for range-expanded
    /// fields; Report Size * Report Count for single-usage fields).
    pub bit_size: usize,
    /// Report ID this field belongs to. 0 when the descriptor contains no
    /// Report ID items.
    pub report_id: u8,
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
/// Global — Report ID (tag 8). HID spec §6.2.2.7.
const TAG_GLOBAL_REPORT_ID: u8 = 0x8;
/// Global — Report Count (tag 9).
const TAG_GLOBAL_REPORT_COUNT: u8 = 0x9;

// Local tags (bType = 2).
/// Local — Usage (tag 0).
const TAG_LOCAL_USAGE: u8 = 0x0;
/// Local — Usage Minimum (tag 1). HID spec §6.2.2.8.
const TAG_LOCAL_USAGE_MIN: u8 = 0x1;
/// Local — Usage Maximum (tag 2). HID spec §6.2.2.8.
const TAG_LOCAL_USAGE_MAX: u8 = 0x2;

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
/// logical field in declaration order.
///
/// # Capabilities
///
/// * **Usage Min/Max ranges** — when `Usage Minimum` and `Usage Maximum`
///   local items are present, the Input item expands into one `ReportField`
///   per usage in the range `[usage_min..=usage_max]`, clamped to
///   `report_count`. Each field occupies `report_size` bits at consecutive
///   offsets. The single-`Usage` path is preserved when no min/max is set.
///
/// * **Report IDs** — when `Report ID` global items are present, each field
///   carries the current `report_id`. The `bit_offset` within each report
///   ID's scope is reset to 0 when a new Report ID item is encountered (the
///   1-byte ID prefix byte is not counted in the offset — offset 0 is the
///   first bit of the report body for that ID).
///
/// # Note — skeleton scope
///
/// This is a skeleton intended for host testing only (see module-level doc).
/// It handles the most common subset needed to verify the parsing logic with
/// hand-crafted test descriptors. Production use would require handling:
/// multi-usage arrays, logical min/max sign extension, long items, and
/// collection items.
pub fn parse_report_descriptor(raw: &[u8]) -> Vec<ReportField> {
    let mut fields = Vec::new();
    let mut pos = 0usize;

    // Global state.
    let mut usage_page: u16 = 0;
    let mut report_size: u32 = 0;
    let mut report_count: u32 = 0;
    let mut report_id: u8 = 0;
    // (Logical min/max are parsed but not stored — not needed for field layout.)

    // Local state (cleared after each Main item).
    let mut usage: u16 = 0;
    let mut usage_min: u16 = 0;
    let mut usage_max: u16 = 0;
    // Track whether a Usage Min/Max pair has been set for this field.
    let mut has_usage_range: bool = false;

    // Per-report-ID bit offset tracking.  We maintain one offset per ID.
    // For the common no-ID case (report_id == 0) this is a single counter.
    // We use a simple parallel pair of vecs rather than a HashMap to keep
    // the dependency on `alloc` minimal (no BTreeMap/HashMap needed).
    let mut id_offsets_id: Vec<u8> = Vec::new();
    let mut id_offsets_bits: Vec<usize> = Vec::new();

    /// Look up or insert the running bit offset for `id`.
    fn bit_offset_for(id: u8, ids: &mut Vec<u8>, bits: &mut Vec<usize>) -> usize {
        if let Some(pos) = ids.iter().position(|&x| x == id) {
            bits[pos]
        } else {
            ids.push(id);
            bits.push(0);
            0
        }
    }

    /// Advance the running bit offset for `id` by `delta` bits.
    fn advance_offset(id: u8, delta: usize, ids: &mut Vec<u8>, bits: &mut Vec<usize>) {
        if let Some(pos) = ids.iter().position(|&x| x == id) {
            bits[pos] = bits[pos].saturating_add(delta);
        } else {
            ids.push(id);
            bits.push(delta);
        }
    }

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
                TAG_GLOBAL_REPORT_ID => {
                    report_id = item.data_u32() as u8;
                    // Ensure a slot exists for this ID (offset starts at 0).
                    bit_offset_for(report_id, &mut id_offsets_id, &mut id_offsets_bits);
                }
                TAG_GLOBAL_REPORT_COUNT => {
                    report_count = item.data_u32();
                }
                _ => { /* other global items ignored in skeleton */ }
            },
            ItemType::Local => match item.tag {
                // Single Usage — only captured when no Min/Max range is active.
                TAG_LOCAL_USAGE if !has_usage_range => {
                    usage = item.data_u32() as u16;
                }
                TAG_LOCAL_USAGE => { /* range already set — ignore extra Usage */ }
                TAG_LOCAL_USAGE_MIN => {
                    usage_min = item.data_u32() as u16;
                    has_usage_range = true;
                }
                TAG_LOCAL_USAGE_MAX => {
                    usage_max = item.data_u32() as u16;
                    has_usage_range = true;
                }
                _ => { /* other local items ignored */ }
            },
            ItemType::Main => {
                let rs = report_size as usize;
                let rc = report_count as usize;
                let total_bits = rs.saturating_mul(rc);

                match item.tag {
                    TAG_MAIN_INPUT if total_bits > 0 => {
                        if has_usage_range && rs > 0 {
                            // Expand the usage range into one field per usage,
                            // clamped to report_count slots.
                            let range_len = (usage_max as usize)
                                .saturating_sub(usage_min as usize)
                                .saturating_add(1);
                            let slots = rc.min(range_len);
                            for i in 0..slots {
                                let u = usage_min.saturating_add(i as u16);
                                let off = bit_offset_for(
                                    report_id,
                                    &mut id_offsets_id,
                                    &mut id_offsets_bits,
                                );
                                fields.push(ReportField {
                                    usage_page,
                                    usage: u,
                                    bit_offset: off,
                                    bit_size: rs,
                                    report_id,
                                });
                                advance_offset(
                                    report_id,
                                    rs,
                                    &mut id_offsets_id,
                                    &mut id_offsets_bits,
                                );
                            }
                            // Consume any remaining padding slots (report count
                            // exceeds the range length — uncommon but spec-legal).
                            let padding = rc.saturating_sub(range_len);
                            if padding > 0 {
                                advance_offset(
                                    report_id,
                                    rs.saturating_mul(padding),
                                    &mut id_offsets_id,
                                    &mut id_offsets_bits,
                                );
                            }
                        } else {
                            // Single-usage path (original behaviour).
                            let off =
                                bit_offset_for(report_id, &mut id_offsets_id, &mut id_offsets_bits);
                            fields.push(ReportField {
                                usage_page,
                                usage,
                                bit_offset: off,
                                bit_size: total_bits,
                                report_id,
                            });
                            advance_offset(
                                report_id,
                                total_bits,
                                &mut id_offsets_id,
                                &mut id_offsets_bits,
                            );
                        }
                    }
                    TAG_MAIN_INPUT => { /* total_bits == 0 — nothing to emit */ }
                    TAG_MAIN_OUTPUT | TAG_MAIN_FEATURE => {
                        // Advance bit offset but do not emit a field.
                        advance_offset(
                            report_id,
                            total_bits,
                            &mut id_offsets_id,
                            &mut id_offsets_bits,
                        );
                    }
                    _ => { /* Collection / End Collection etc. — ignore */ }
                }
                // Clear local state after any Main item (HID spec §6.2.2.8).
                usage = 0;
                usage_min = 0;
                usage_max = 0;
                has_usage_range = false;
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
        assert_eq!(f.report_id, 0); // no Report ID item in this descriptor
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
        assert_eq!(fields[0].report_id, 0);

        assert_eq!(fields[1].usage_page, 0x0009);
        assert_eq!(fields[1].usage, 0x0002); // Button 2
        assert_eq!(fields[1].bit_offset, 1);
        assert_eq!(fields[1].bit_size, 1);
        assert_eq!(fields[1].report_id, 0);

        assert_eq!(fields[2].usage_page, 0x0009);
        assert_eq!(fields[2].usage, 0x0003); // Button 3
        assert_eq!(fields[2].bit_offset, 2);
        assert_eq!(fields[2].bit_size, 1);
        assert_eq!(fields[2].report_id, 0);
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

    // -----------------------------------------------------------------------
    // B.2 new tests: Usage Min/Max ranges
    // -----------------------------------------------------------------------

    /// Usage Min=1 / Max=5, Report Size=1, Report Count=5 → five 1-bit button
    /// fields with usages 1..=5 at offsets 0..4.
    ///
    /// Item encoding (short items):
    ///   Usage Page 9   : 05 09  (Button page)
    ///   Usage Min 1    : 19 01  (Local, tag 1, size 1, data=0x01)
    ///   Usage Max 5    : 29 05  (Local, tag 2, size 1, data=0x05)
    ///   Logical Min 0  : 15 00
    ///   Logical Max 1  : 25 01
    ///   Report Size 1  : 75 01
    ///   Report Count 5 : 95 05
    ///   Input          : 81 02
    #[test]
    fn usage_min_max_range_expands_to_one_field_per_usage() {
        let raw: &[u8] = &[
            0x05, 0x09, // Usage Page = Button (0x09)
            0x19, 0x01, // Usage Minimum = 1
            0x29, 0x05, // Usage Maximum = 5
            0x15, 0x00, // Logical Minimum = 0
            0x25, 0x01, // Logical Maximum = 1
            0x75, 0x01, // Report Size = 1
            0x95, 0x05, // Report Count = 5
            0x81, 0x02, // Input (Data, Variable, Absolute)
        ];
        let fields = parse_report_descriptor(raw);
        assert_eq!(
            fields.len(),
            5,
            "expected 5 fields (one per usage in range)"
        );
        for (i, f) in fields.iter().enumerate() {
            assert_eq!(f.usage_page, 0x0009, "field {i}: usage_page");
            assert_eq!(f.usage, (i as u16) + 1, "field {i}: usage");
            assert_eq!(f.bit_offset, i, "field {i}: bit_offset");
            assert_eq!(f.bit_size, 1, "field {i}: bit_size");
            assert_eq!(f.report_id, 0, "field {i}: report_id");
        }
    }

    // -----------------------------------------------------------------------
    // B.2 new tests: Report IDs
    // -----------------------------------------------------------------------

    /// A descriptor with two Report IDs tags each field with its ID and resets
    /// bit_offset per ID.
    ///
    /// Descriptor structure:
    ///   Usage Page 1   : 05 01
    ///   Report ID 1    : 85 01  (Global, tag 8, size 1, data=0x01)
    ///   Usage 0x30     : 09 30
    ///   Report Size 8  : 75 08
    ///   Report Count 1 : 95 01
    ///   Input          : 81 02  → report_id=1, bit_offset=0, bit_size=8
    ///   Report ID 2    : 85 02
    ///   Usage 0x31     : 09 31
    ///   Report Size 16 : 75 10
    ///   Report Count 1 : 95 01
    ///   Input          : 81 02  → report_id=2, bit_offset=0, bit_size=16
    #[test]
    fn two_report_ids_tag_fields_and_reset_offset() {
        let raw: &[u8] = &[
            0x05, 0x01, // Usage Page = Generic Desktop (0x01)
            // Report ID 1 field
            0x85, 0x01, // Report ID = 1
            0x09, 0x30, // Usage = X (0x30)
            0x15, 0x00, // Logical Minimum = 0
            0x25, 0xFF, // Logical Maximum = 255
            0x75, 0x08, // Report Size = 8
            0x95, 0x01, // Report Count = 1
            0x81, 0x02, // Input → report_id=1, bit_offset=0, bit_size=8
            // Report ID 2 field
            0x85, 0x02, // Report ID = 2
            0x09, 0x31, // Usage = Y (0x31)
            0x75, 0x10, // Report Size = 16
            0x95, 0x01, // Report Count = 1
            0x81, 0x02, // Input → report_id=2, bit_offset=0, bit_size=16
        ];
        let fields = parse_report_descriptor(raw);
        assert_eq!(fields.len(), 2, "expected 2 fields (one per report ID)");

        // Field 0: Report ID 1, 8-bit X axis.
        assert_eq!(fields[0].report_id, 1, "field 0 report_id");
        assert_eq!(fields[0].usage, 0x0030, "field 0 usage");
        assert_eq!(
            fields[0].bit_offset, 0,
            "field 0 bit_offset resets to 0 for ID 1"
        );
        assert_eq!(fields[0].bit_size, 8, "field 0 bit_size");

        // Field 1: Report ID 2, 16-bit Y axis — offset resets to 0 for this ID.
        assert_eq!(fields[1].report_id, 2, "field 1 report_id");
        assert_eq!(fields[1].usage, 0x0031, "field 1 usage");
        assert_eq!(
            fields[1].bit_offset, 0,
            "field 1 bit_offset resets to 0 for ID 2"
        );
        assert_eq!(fields[1].bit_size, 16, "field 1 bit_size");
    }

    /// Multiple fields under the same Report ID accumulate their offsets
    /// independently from fields under other Report IDs.
    #[test]
    fn same_report_id_accumulates_offsets() {
        // Two 8-bit fields both under Report ID 3.
        let raw: &[u8] = &[
            0x05, 0x01, // Usage Page = Generic Desktop
            0x85, 0x03, // Report ID = 3
            0x09, 0x30, // Usage = X
            0x75, 0x08, // Report Size = 8
            0x95, 0x01, // Report Count = 1
            0x81, 0x02, // Input → report_id=3, bit_offset=0, bit_size=8
            0x09, 0x31, // Usage = Y
            0x75, 0x08, // Report Size = 8
            0x95, 0x01, // Report Count = 1
            0x81, 0x02, // Input → report_id=3, bit_offset=8, bit_size=8
        ];
        let fields = parse_report_descriptor(raw);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].report_id, 3);
        assert_eq!(fields[0].bit_offset, 0);
        assert_eq!(fields[0].bit_size, 8);
        assert_eq!(fields[1].report_id, 3);
        assert_eq!(fields[1].bit_offset, 8);
        assert_eq!(fields[1].bit_size, 8);
    }
}
