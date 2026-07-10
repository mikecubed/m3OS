//! HID Report-Descriptor item parser (Phase 78c Track A.3; live as of Phase 92).
//!
//! # Status: live, host-tested
//!
//! This parser is now **called live at device bind** by the `usb-hid` driver
//! (Phase 92 B.1): the driver fetches the interface's Report Descriptor and
//! runs [`parse_report_descriptor`] over it. Its output then drives the
//! Report-Protocol pointer decode (Phase 92b B.2) via [`decode_pointer_report`],
//! which extracts buttons / axes / wheel state from a raw input report using
//! the parsed [`ReportField`] layout. The logic remains fully host-tested (the
//! tests in this module are the source of truth for parsing + decode behaviour).
//!
//! # What is implemented
//!
//! A parser for *short items* (the common case in USB HID descriptors; long
//! items are skipped). Each item prefix byte encodes:
//!
//! * **bSize** — bits 1:0 — number of following data bytes (0, 1, 2, or 4).
//! * **bType** — bits 3:2 — item type (`Main=0`, `Global=1`, `Local=2`,
//!   reserved=3).
//! * **bTag** — bits 7:4 — which item.
//!
//! The parser maintains a small Global state (Usage Page, Report Size, Report
//! Count, Report ID) and a Local state (Usage, a multi-usage list, and Usage
//! Min/Max range), and emits a [`ReportField`] for each Main Input item.
//! Report IDs, Usage Min/Max ranges, and multi-usage lists (e.g. `Usage X;
//! Usage Y; Input(Variable)` on a mouse/tablet) are all handled. Constant
//! padding and Array inputs advance the bit offset without emitting fields.
//!
//! # Limitations
//!
//! * Only short items are parsed; long items (prefix byte 0xFE) are skipped.
//! * Array items (Variable bit clear) are not decoded into per-key fields —
//!   their bits are reserved (offset advanced) but no fields are emitted.
//! * Logical Min/Max sign-extension is applied at *decode* time
//!   ([`decode_pointer_report`] sign-extends relative axes / wheel by
//!   `bit_size`), not stored per-field at parse time.
//! * Input / Output / Feature items share one running bit offset per Report ID.
//!   In the HID spec these are independent bit-spaces; a descriptor that
//!   *interleaves* an Output/Feature item between Input items under the same
//!   Report ID would push subsequent Input offsets past the Output bits. The
//!   common "all Inputs, then Outputs" ordering (boot keyboards, the QEMU
//!   tablet) is unaffected; per-report-type offset accumulators are a deferred
//!   refinement.
//! * Collection / End Collection nesting is not modelled (these Main items
//!   carry no report bits and are ignored without disturbing offsets).

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
    /// HID Usage (from a Local Usage item, the multi-usage list, or Usage
    /// Min/Max range expansion, or 0 if none).
    pub usage: u16,
    /// Bit offset of this field within its report (body after the Report ID
    /// prefix byte, or from the start of the single report when no IDs are
    /// used). Reset to 0 at each new Report ID.
    pub bit_offset: usize,
    /// Number of bits occupied by this field. One Report Size for each
    /// range-expanded or multi-usage Variable field; Report Size * Report
    /// Count for a single-usage non-Variable field.
    pub bit_size: usize,
    /// Report ID this field belongs to. 0 when the descriptor contains no
    /// Report ID items.
    pub report_id: u8,
    /// `true` when the Main Input item's data byte had the Relative flag
    /// (bit 2 / 0x04) set — the field carries a delta (e.g. a relative mouse
    /// axis). `false` for Absolute fields (e.g. a tablet's absolute position).
    pub is_relative: bool,
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

/// Hard cap on the number of [`ReportField`]s a single descriptor may produce.
///
/// A hostile descriptor can encode a Report Count (or Usage Min/Max range) of
/// `0xFFFFFFFF`, which would otherwise drive the field-expansion loops into an
/// out-of-memory allocation. Both expansion loops stop emitting once
/// `fields.len()` reaches this cap, bounding worst-case memory.
const MAX_REPORT_FIELDS: usize = 65536;

/// Parse a raw HID Report Descriptor and return one [`ReportField`] per
/// logical field in declaration order.
///
/// # Capabilities
///
/// * **Usage Min/Max ranges** — when `Usage Minimum` and `Usage Maximum`
///   local items are present, the Variable Input item expands into one
///   `ReportField` per usage in the range `[usage_min..=usage_max]`, clamped
///   to `report_count`. Each field occupies `report_size` bits at consecutive
///   offsets. The range path takes priority over the multi-usage list.
///
/// * **Multi-usage lists** — when several `Usage` local items precede one
///   Variable Input with `Report Count = C`, the item expands into `C`
///   fields, each `report_size` bits. Field `i` takes the `i`-th declared
///   usage (the last usage is repeated if the count exceeds the usage list;
///   usage 0 if no usages were declared). A single `Usage` is the one-element
///   case of this.
///
/// * **Constant / Array inputs** — a Constant Input (data bit0 set) or an
///   Array Input (data bit1 clear) reserves `report_size * report_count` bits
///   (advancing the offset) but emits no fields, so padding does not produce
///   spurious usage-0 fields.
///
/// * **Relative flag** — each emitted field records `is_relative` from the
///   Main Input data byte (bit2 / 0x04), so a decoder can distinguish a
///   relative mouse delta from a tablet's absolute position.
///
/// * **Report IDs** — when `Report ID` global items are present, each field
///   carries the current `report_id`. The `bit_offset` within each report
///   ID's scope is reset to 0 when a new Report ID item is encountered (the
///   1-byte ID prefix byte is not counted in the offset — offset 0 is the
///   first bit of the report body for that ID).
///
/// # Bounds
///
/// At most [`MAX_REPORT_FIELDS`] fields are emitted; a hostile descriptor with
/// a `0xFFFFFFFF` Report Count or usage range cannot drive an unbounded
/// allocation.
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
    // Every Local Usage item seen for the pending Main item, in declaration
    // order. Real pointers emit several (e.g. Usage X; Usage Y) ahead of one
    // Variable Input with Report Count = number of usages. The single-usage
    // case is just a one-element list.
    let mut usages: Vec<u16> = Vec::new();
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
                // Usage — accumulated into the multi-usage list (a single Usage
                // is just a one-element list). Only captured when no Min/Max
                // range is active.
                TAG_LOCAL_USAGE if !has_usage_range => {
                    usages.push(item.data_u32() as u16);
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
                        // Decode the Main Input data-byte flags (HID §6.2.2.5):
                        //   bit0 (0x01) = Constant (1) / Data (0)
                        //   bit1 (0x02) = Variable (1) / Array (0)
                        //   bit2 (0x04) = Relative (1) / Absolute (0)
                        let data = item.data_u32();
                        let is_constant = data & 0x01 != 0;
                        let is_variable = data & 0x02 != 0;
                        let is_relative = data & 0x04 != 0;

                        if is_constant {
                            // Constant padding — reserve the bits, emit nothing.
                            advance_offset(
                                report_id,
                                total_bits,
                                &mut id_offsets_id,
                                &mut id_offsets_bits,
                            );
                        } else if has_usage_range && rs > 0 {
                            // Usage Min/Max range: one field per usage in the
                            // range, clamped to report_count slots.
                            let range_len = (usage_max as usize)
                                .saturating_sub(usage_min as usize)
                                .saturating_add(1);
                            let slots = rc.min(range_len);
                            let mut emitted = 0usize;
                            for i in 0..slots {
                                if fields.len() >= MAX_REPORT_FIELDS {
                                    break;
                                }
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
                                    is_relative,
                                });
                                advance_offset(
                                    report_id,
                                    rs,
                                    &mut id_offsets_id,
                                    &mut id_offsets_bits,
                                );
                                emitted += 1;
                            }
                            // Advance the running offset over every slot not
                            // individually emitted — both the spec-legal padding
                            // when report count exceeds the usage range, and any
                            // slots dropped by the `MAX_REPORT_FIELDS` cap — so
                            // the item spans its full `rs * rc` bits and a
                            // following Main item's offsets stay aligned.
                            if emitted < rc {
                                advance_offset(
                                    report_id,
                                    rs.saturating_mul(rc - emitted),
                                    &mut id_offsets_id,
                                    &mut id_offsets_bits,
                                );
                            }
                        } else if is_variable && rs > 0 {
                            // Variable Data: emit one field per report-count
                            // slot, each `report_size` bits. Field `i` gets the
                            // i-th declared usage (last usage repeated when the
                            // count exceeds the usage list; usage 0 when empty).
                            let mut emitted = 0usize;
                            for i in 0..rc {
                                if fields.len() >= MAX_REPORT_FIELDS {
                                    break;
                                }
                                let u = if usages.is_empty() {
                                    0
                                } else {
                                    usages[i.min(usages.len() - 1)]
                                };
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
                                    is_relative,
                                });
                                advance_offset(
                                    report_id,
                                    rs,
                                    &mut id_offsets_id,
                                    &mut id_offsets_bits,
                                );
                                emitted += 1;
                            }
                            // Advance the running offset over any slots dropped by
                            // the `MAX_REPORT_FIELDS` cap so the item spans its
                            // full `rs * rc` bits and a following Main item's
                            // offsets stay aligned to the report layout.
                            if emitted < rc {
                                advance_offset(
                                    report_id,
                                    rs.saturating_mul(rc - emitted),
                                    &mut id_offsets_id,
                                    &mut id_offsets_bits,
                                );
                            }
                        } else {
                            // Array Data (Variable bit clear): reserve the bits
                            // but emit no per-key fields (not modelled).
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
                usages.clear();
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
// Report-Protocol pointer decode (Phase 92b B.2)
// ---------------------------------------------------------------------------

/// Decoded pointer (mouse / tablet) state from one Report-Protocol input
/// report, produced by [`decode_pointer_report`].
///
/// Relative axes (`rel_x`/`rel_y`/`wheel`) are accumulated (sign-extended)
/// deltas; absolute axes (`abs_x`/`abs_y`) carry the last reported position
/// (a tablet always reports a position, so `Some` presence counts as input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodedPointer {
    /// Button bitmap: a Button-page (0x09) usage `n` sets bit `n - 1`.
    pub buttons: u32,
    /// Accumulated relative Generic-Desktop X (usage 0x30, `is_relative`).
    pub rel_x: i32,
    /// Accumulated relative Generic-Desktop Y (usage 0x31, `is_relative`).
    pub rel_y: i32,
    /// Absolute Generic-Desktop X (usage 0x30, absolute), if reported.
    pub abs_x: Option<u32>,
    /// Absolute Generic-Desktop Y (usage 0x31, absolute), if reported.
    pub abs_y: Option<u32>,
    /// Accumulated Generic-Desktop Wheel (usage 0x38), signed.
    pub wheel: i32,
    /// `true` if any button bit is set or any axis/wheel is non-zero (an
    /// absolute axis being present counts as input).
    pub any_input: bool,
}

/// Generic Desktop usage page.
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// Button usage page.
const USAGE_PAGE_BUTTON: u16 = 0x09;
/// Generic Desktop usage — X axis.
const USAGE_X: u16 = 0x30;
/// Generic Desktop usage — Y axis.
const USAGE_Y: u16 = 0x31;
/// Generic Desktop usage — Wheel.
const USAGE_WHEEL: u16 = 0x38;

/// Extract `bit_size` bits (LSB-first, the HID packing order) starting at
/// `bit_offset` within `report`, returning them as a `u64` with the first
/// extracted bit as the result's LSB.
///
/// Bounds-safe: returns 0 if the field runs past the end of `report` (a
/// truncated/short report never panics). `bit_size` is capped at 64.
fn extract_bits(report: &[u8], bit_offset: usize, bit_size: usize) -> u64 {
    let bit_size = bit_size.min(64);
    if bit_size == 0 {
        return 0;
    }
    // Reject out-of-range fields (the last bit must be addressable).
    let end_bit = match bit_offset.checked_add(bit_size) {
        Some(e) => e,
        None => return 0,
    };
    if end_bit > report.len().saturating_mul(8) {
        return 0;
    }
    let mut value: u64 = 0;
    for k in 0..bit_size {
        let abs = bit_offset + k;
        let byte = report[abs / 8];
        let bit = (byte >> (abs % 8)) & 0x01;
        value |= (bit as u64) << k;
    }
    value
}

/// Sign-extend a `bit_size`-wide unsigned `value` to a signed `i32`. If the
/// top bit (`bit_size - 1`) is set, the value is negative.
fn sign_extend(value: u64, bit_size: usize) -> i32 {
    if bit_size == 0 || bit_size >= 64 {
        return value as i32;
    }
    let sign_bit = 1u64 << (bit_size - 1);
    if value & sign_bit != 0 {
        // value - 2^bit_size, done as a signed subtraction.
        (value as i64 - (1i64 << bit_size)) as i32
    } else {
        value as i32
    }
}

/// Decode a Report-Protocol input report into [`DecodedPointer`] state using
/// `fields` (the output of [`parse_report_descriptor`]).
///
/// # Report ID handling
///
/// If ANY field has `report_id != 0`, `report[0]` is treated as the report ID;
/// only fields whose `report_id == report[0]` are decoded, and each field's
/// body begins at absolute bit `8 + field.bit_offset` (after the 1-byte ID
/// prefix). If every field has `report_id == 0`, the body begins at bit 0
/// (absolute bit = `field.bit_offset`).
///
/// # Decoding
///
/// * Generic Desktop (page 0x01): X (0x30) / Y (0x31) accumulate into
///   `rel_x`/`rel_y` (sign-extended) when `is_relative`, else store into
///   `abs_x`/`abs_y`. Wheel (0x38) sign-extends into `wheel`.
/// * Button (page 0x09): a 1-bit field with usage `n` (1..=31) sets bit
///   `n - 1` of `buttons` when the extracted bit is 1.
///
/// Out-of-bounds fields (a too-short report) decode to 0 without panicking.
pub fn decode_pointer_report(fields: &[ReportField], report: &[u8]) -> DecodedPointer {
    let mut out = DecodedPointer::default();

    // Determine whether the descriptor uses Report IDs.
    let uses_report_id = fields.iter().any(|f| f.report_id != 0);
    let (active_id, body_base_bit) = if uses_report_id {
        let id = report.first().copied().unwrap_or(0);
        (id, 8usize)
    } else {
        (0u8, 0usize)
    };

    for f in fields {
        if uses_report_id && f.report_id != active_id {
            continue;
        }
        let abs_bit = body_base_bit + f.bit_offset;
        let raw = extract_bits(report, abs_bit, f.bit_size);

        match (f.usage_page, f.usage) {
            (USAGE_PAGE_GENERIC_DESKTOP, USAGE_X) => {
                if f.is_relative {
                    out.rel_x = out.rel_x.saturating_add(sign_extend(raw, f.bit_size));
                } else {
                    out.abs_x = Some(raw as u32);
                }
            }
            (USAGE_PAGE_GENERIC_DESKTOP, USAGE_Y) => {
                if f.is_relative {
                    out.rel_y = out.rel_y.saturating_add(sign_extend(raw, f.bit_size));
                } else {
                    out.abs_y = Some(raw as u32);
                }
            }
            (USAGE_PAGE_GENERIC_DESKTOP, USAGE_WHEEL) => {
                out.wheel = out.wheel.saturating_add(sign_extend(raw, f.bit_size));
            }
            // A Button-page (0x09) 1-bit variable field with usage `n` (1..=32)
            // sets bit `n - 1` when asserted (32 buttons fit the u32 bitmap).
            (USAGE_PAGE_BUTTON, n)
                if f.bit_size == 1 && (1..=32).contains(&n) && raw & 0x01 != 0 =>
            {
                out.buttons |= 1u32 << (n - 1);
            }
            _ => { /* other usage page / usage — ignored */ }
        }
    }

    out.any_input = out.buttons != 0
        || out.rel_x != 0
        || out.rel_y != 0
        || out.wheel != 0
        || out.abs_x.is_some()
        || out.abs_y.is_some();

    out
}

// ---------------------------------------------------------------------------
// Report-Protocol consumer-control decode (Phase 92b B.3)
// ---------------------------------------------------------------------------

/// HID Usage Page 0x0C — Consumer (media / volume / brightness controls).
const USAGE_PAGE_CONSUMER: u16 = 0x0C;

/// Decode the set of **Consumer-control** usages (Usage Page 0x0C) currently
/// asserted in a Report-Protocol input report, using `fields` (the output of
/// [`parse_report_descriptor`]). Returns the usage IDs of every 1-bit Consumer
/// field whose bit is set — e.g. Volume Increment (0x00E9), Mute (0x00E2) — so
/// the caller can map each via [`super::hid::hid_consumer_usage_to_keycode`]
/// and route it (Phase 92b B.3, media/volume keys → `audio_server`).
///
/// Report-ID handling matches [`decode_pointer_report`]: a Report-ID'd
/// descriptor selects fields by `report[0]` and offsets the body past the ID
/// byte. Bitmap (variable, 1-bit-per-usage) consumer reports are decoded here;
/// array-style consumer reports (a usage *code* in a wide field) are not
/// modelled (the parser does not emit per-key fields for HID Array items — see
/// the module limitations). Out-of-bounds fields decode to 0 without panicking.
pub fn decode_consumer_usages(fields: &[ReportField], report: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    let uses_report_id = fields.iter().any(|f| f.report_id != 0);
    let (active_id, body_base_bit) = if uses_report_id {
        (report.first().copied().unwrap_or(0), 8usize)
    } else {
        (0u8, 0usize)
    };
    for f in fields {
        if f.usage_page != USAGE_PAGE_CONSUMER || f.usage == 0 {
            continue;
        }
        if uses_report_id && f.report_id != active_id {
            continue;
        }
        let abs_bit = body_base_bit + f.bit_offset;
        // A variable Consumer control is a 1-bit "asserted" flag.
        if f.bit_size >= 1 && extract_bits(report, abs_bit, f.bit_size) & 0x01 != 0 {
            out.push(f.usage);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Precision-Touchpad / multitouch decode (Phase 102 Track C)
// ---------------------------------------------------------------------------

/// HID Usage Page 0x0D — Digitizers (touchpad / touchscreen / stylus).
const USAGE_PAGE_DIGITIZER: u16 = 0x0D;
/// Digitizer Usage 0x42 — Tip Switch (finger touching the surface).
const USAGE_DIG_TIP_SWITCH: u16 = 0x42;
/// Digitizer Usage 0x51 — Contact Identifier (stable per-finger id).
const USAGE_DIG_CONTACT_ID: u16 = 0x51;
/// Digitizer Usage 0x54 — Contact Count (valid contacts this frame).
const USAGE_DIG_CONTACT_COUNT: u16 = 0x54;

/// One decoded touchpad contact (finger). `x`/`y` are the **absolute** logical
/// coordinates straight from the report — the `i2c-hid` daemon scales and
/// differences them into the relative `PointerEvent` deltas `mouse_server`
/// expects (the mapping is device-geometry-dependent, so it stays in the
/// daemon, not this pure decode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TouchContact {
    /// Tip Switch — `true` while this finger is touching.
    pub tip: bool,
    /// Contact Identifier — stable across frames for the same finger.
    pub contact_id: u8,
    /// Absolute X in the touchpad's logical coordinate space.
    pub x: u16,
    /// Absolute Y in the touchpad's logical coordinate space.
    pub y: u16,
}

/// A decoded Windows-Precision-Touchpad input-report frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TouchpadFrame {
    /// Every contact *slot* present in the report, in descriptor order. A lifted
    /// finger is still a slot but with `tip == false`.
    pub contacts: Vec<TouchContact>,
    /// The report's Contact Count field — how many slots are valid this frame
    /// (`0` if the descriptor carries no Contact Count usage; fall back to
    /// counting `tip` contacts).
    pub contact_count: u8,
    /// The clickpad / physical button (Button page usage 1) state.
    pub button: bool,
}

impl TouchpadFrame {
    /// The contacts actually touching (`tip == true`) — the fingers the daemon
    /// turns into pointer motion / a two-finger scroll.
    pub fn active_contacts(&self) -> impl Iterator<Item = &TouchContact> {
        self.contacts.iter().filter(|c| c.tip)
    }
}

/// Accumulates one contact's fields until a repeated usage (the next finger's
/// collection) or the end of the report flushes it.
#[derive(Default)]
struct ContactAcc {
    cur: TouchContact,
    has_tip: bool,
    has_id: bool,
    has_x: bool,
    has_y: bool,
    dirty: bool,
}

impl ContactAcc {
    /// Push the accumulated contact (if any field was set) and reset.
    fn flush_into(&mut self, out: &mut Vec<TouchContact>) {
        if self.dirty {
            out.push(self.cur);
            *self = Self::default();
        }
    }
}

/// Decode a **Windows-Precision-Touchpad / digitizer** input report using
/// `fields` (the [`parse_report_descriptor`] output — the descriptor language is
/// identical over I2C and USB, so this is shared with the USB HID path).
/// OpenBSD `imt(4)` (`sys/dev/i2c/imt.c`) is the reference.
///
/// Per-contact usages — Digitizer Tip Switch (0x42) / Contact Identifier (0x51)
/// and Generic-Desktop X (0x30) / Y (0x31) — are grouped into [`TouchContact`]s
/// in descriptor order: a **repeated** per-contact usage begins the next finger
/// (Precision Touchpad lays out one collection per finger). Digitizer Contact
/// Count (0x54) fills [`TouchpadFrame::contact_count`]; a Button-page (0x09)
/// usage-1 bit fills [`TouchpadFrame::button`].
///
/// Report-ID handling matches [`decode_pointer_report`]: an ID'd descriptor
/// selects fields by `report[0]` and offsets the body past the ID byte. A
/// too-short report decodes to zeroed fields without panicking (the shared
/// `extract_bits` is bounds-safe).
pub fn decode_touchpad_report(fields: &[ReportField], report: &[u8]) -> TouchpadFrame {
    let mut frame = TouchpadFrame::default();

    let uses_report_id = fields.iter().any(|f| f.report_id != 0);
    let (active_id, body_base_bit) = if uses_report_id {
        (report.first().copied().unwrap_or(0), 8usize)
    } else {
        (0u8, 0usize)
    };

    let mut acc = ContactAcc::default();

    for f in fields {
        if uses_report_id && f.report_id != active_id {
            continue;
        }
        let abs_bit = body_base_bit + f.bit_offset;
        let raw = extract_bits(report, abs_bit, f.bit_size);

        match (f.usage_page, f.usage) {
            (USAGE_PAGE_DIGITIZER, USAGE_DIG_TIP_SWITCH) => {
                if acc.has_tip {
                    acc.flush_into(&mut frame.contacts);
                }
                acc.cur.tip = raw & 0x01 != 0;
                acc.has_tip = true;
                acc.dirty = true;
            }
            (USAGE_PAGE_DIGITIZER, USAGE_DIG_CONTACT_ID) => {
                if acc.has_id {
                    acc.flush_into(&mut frame.contacts);
                }
                acc.cur.contact_id = raw as u8;
                acc.has_id = true;
                acc.dirty = true;
            }
            (USAGE_PAGE_GENERIC_DESKTOP, USAGE_X) => {
                if acc.has_x {
                    acc.flush_into(&mut frame.contacts);
                }
                acc.cur.x = raw as u16;
                acc.has_x = true;
                acc.dirty = true;
            }
            (USAGE_PAGE_GENERIC_DESKTOP, USAGE_Y) => {
                if acc.has_y {
                    acc.flush_into(&mut frame.contacts);
                }
                acc.cur.y = raw as u16;
                acc.has_y = true;
                acc.dirty = true;
            }
            (USAGE_PAGE_DIGITIZER, USAGE_DIG_CONTACT_COUNT) => {
                frame.contact_count = raw as u8;
            }
            // A clickpad / physical button: Button page, usage 1..=8, 1 bit set.
            (USAGE_PAGE_BUTTON, n) if f.bit_size >= 1 && (1..=8).contains(&n) => {
                if raw & 0x01 != 0 {
                    frame.button = true;
                }
            }
            _ => { /* pressure, width, scan-time, azimuth, … — ignored */ }
        }
    }
    acc.flush_into(&mut frame.contacts);

    frame
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

    // -----------------------------------------------------------------------
    // B.1 readiness: hostile field-count cap
    // -----------------------------------------------------------------------

    /// A descriptor with a 4-byte Report Count of 0xFFFFFFFF plus a full
    /// Usage Min/Max range and Report Size 1 must not OOM: the field-emission
    /// loop is capped at MAX_REPORT_FIELDS.
    #[test]
    fn hostile_report_count_is_bounded() {
        let raw: &[u8] = &[
            0x05, 0x09, // Usage Page = Button
            0x19, 0x01, // Usage Minimum = 1
            0x2A, 0xFF, 0xFF, // Usage Maximum = 0xFFFF (Local tag 2, bSize=2)
            0x75, 0x01, // Report Size = 1
            0x97, 0xFF, 0xFF, 0xFF,
            0xFF, // Report Count = 0xFFFFFFFF (Global tag 9, bSize=3 → 4 bytes)
            0x81, 0x02, // Input (Data, Variable, Absolute)
        ];
        let fields = parse_report_descriptor(raw); // must not panic / OOM
        assert!(
            fields.len() <= MAX_REPORT_FIELDS,
            "field count {} exceeds cap {}",
            fields.len(),
            MAX_REPORT_FIELDS
        );
    }

    /// A Usage Min/Max range whose Report Count exceeds the range length leaves
    /// padding slots; the running bit offset must still advance over the *full*
    /// `report_size * report_count` span so a following Main item is aligned.
    /// Guards the offset-advance fix for the variable/usage-range branches.
    #[test]
    fn usage_range_padding_advances_full_span() {
        let raw: &[u8] = &[
            0x05, 0x09, // Usage Page = Button
            0x19, 0x01, // Usage Minimum = 1
            0x29, 0x03, // Usage Maximum = 3   (range length = 3)
            0x75, 0x01, // Report Size = 1
            0x95, 0x08, // Report Count = 8    (5 padding slots beyond the range)
            0x81, 0x02, // Input (Data, Variable, Absolute) → 3 buttons @ 0,1,2
            0x05, 0x01, // Usage Page = Generic Desktop
            0x09, 0x30, // Usage = X
            0x75, 0x08, // Report Size = 8
            0x95, 0x01, // Report Count = 1
            0x81, 0x02, // Input → X must sit at bit 8 (after the full 8-bit block)
        ];
        let fields = parse_report_descriptor(raw);
        assert_eq!(fields.len(), 4, "3 buttons + 1 axis");
        assert_eq!(fields[0].bit_offset, 0);
        assert_eq!(fields[1].bit_offset, 1);
        assert_eq!(fields[2].bit_offset, 2);
        // The X axis must start at bit 8: the button item spans report_size(1) *
        // report_count(8) = 8 bits, not just the 3 emitted usage slots.
        assert_eq!(fields[3].usage, 0x0030, "Generic Desktop X");
        assert_eq!(
            fields[3].bit_offset, 8,
            "axis must follow the full 8-bit span"
        );
        assert_eq!(fields[3].bit_size, 8);
    }

    // -----------------------------------------------------------------------
    // B.2 Report-Protocol pointer decode
    // -----------------------------------------------------------------------

    /// A usb-tablet-like descriptor: 3 buttons + 5-bit const pad, then an
    /// absolute 16-bit X / 16-bit Y, then a relative 8-bit Wheel.
    ///
    /// Bytes (HID short items):
    ///   05 01            Usage Page = Generic Desktop
    ///   09 02            Usage = Mouse
    ///   A1 01            Collection (Application)
    ///   09 01              Usage = Pointer
    ///   A1 00              Collection (Physical)
    ///   05 09                Usage Page = Button
    ///   19 01                Usage Minimum = 1
    ///   29 03                Usage Maximum = 3
    ///   15 00                Logical Minimum = 0
    ///   25 01                Logical Maximum = 1
    ///   75 01                Report Size = 1
    ///   95 03                Report Count = 3
    ///   81 02                Input (Data, Var, Abs)  → 3 button fields
    ///   75 05                Report Size = 5
    ///   95 01                Report Count = 1
    ///   81 03                Input (Const, Var, Abs) → 5-bit padding (no field)
    ///   05 01                Usage Page = Generic Desktop
    ///   09 30                Usage = X
    ///   09 31                Usage = Y
    ///   15 00                Logical Minimum = 0
    ///   26 FF 7F             Logical Maximum = 0x7FFF (bSize=2)
    ///   75 10                Report Size = 16
    ///   95 02                Report Count = 2
    ///   81 02                Input (Data, Var, Abs)  → X (off 8) + Y (off 24)
    ///   09 38                Usage = Wheel
    ///   15 81                Logical Minimum = -127
    ///   25 7F                Logical Maximum = 127
    ///   75 08                Report Size = 8
    ///   95 01                Report Count = 1
    ///   81 06                Input (Data, Var, Rel)  → Wheel (off 40, rel)
    ///   C0               End Collection
    ///   C0               End Collection
    const TABLET_DESC: &[u8] = &[
        0x05, 0x01, // Usage Page = Generic Desktop
        0x09, 0x02, // Usage = Mouse
        0xA1, 0x01, // Collection (Application)
        0x09, 0x01, //   Usage = Pointer
        0xA1, 0x00, //   Collection (Physical)
        0x05, 0x09, //     Usage Page = Button
        0x19, 0x01, //     Usage Minimum = 1
        0x29, 0x03, //     Usage Maximum = 3
        0x15, 0x00, //     Logical Minimum = 0
        0x25, 0x01, //     Logical Maximum = 1
        0x75, 0x01, //     Report Size = 1
        0x95, 0x03, //     Report Count = 3
        0x81, 0x02, //     Input (Data, Var, Abs)
        0x75, 0x05, //     Report Size = 5
        0x95, 0x01, //     Report Count = 1
        0x81, 0x03, //     Input (Const, Var, Abs) — padding
        0x05, 0x01, //     Usage Page = Generic Desktop
        0x09, 0x30, //     Usage = X
        0x09, 0x31, //     Usage = Y
        0x15, 0x00, //     Logical Minimum = 0
        0x26, 0xFF, 0x7F, // Logical Maximum = 0x7FFF (bSize=2)
        0x75, 0x10, //     Report Size = 16
        0x95, 0x02, //     Report Count = 2
        0x81, 0x02, //     Input (Data, Var, Abs)
        0x09, 0x38, //     Usage = Wheel
        0x15, 0x81, //     Logical Minimum = -127
        0x25, 0x7F, //     Logical Maximum = 127
        0x75, 0x08, //     Report Size = 8
        0x95, 0x01, //     Report Count = 1
        0x81, 0x06, //     Input (Data, Var, Rel)
        0xC0, //   End Collection
        0xC0, // End Collection
    ];

    /// The tablet descriptor parses into the expected field layout: 3 button
    /// fields, then absolute X (off 8) / Y (off 24), then relative Wheel
    /// (off 40). The 5-bit const padding must NOT produce a field.
    #[test]
    fn tablet_descriptor_parses_expected_fields() {
        let fields = parse_report_descriptor(TABLET_DESC);
        assert_eq!(
            fields.len(),
            6,
            "expected 6 fields (3 buttons + X + Y + Wheel), got {fields:?}"
        );

        // Three buttons: Button page, usages 1/2/3, size 1, offsets 0/1/2.
        for i in 0..3 {
            assert_eq!(fields[i].usage_page, 0x0009, "field {i} usage_page");
            assert_eq!(fields[i].usage, (i as u16) + 1, "field {i} usage");
            assert_eq!(fields[i].bit_offset, i, "field {i} bit_offset");
            assert_eq!(fields[i].bit_size, 1, "field {i} bit_size");
            assert!(!fields[i].is_relative, "field {i} is_relative");
        }

        // X: Generic Desktop, usage 0x30, offset 8, size 16, absolute.
        assert_eq!(fields[3].usage_page, 0x0001);
        assert_eq!(fields[3].usage, 0x0030);
        assert_eq!(fields[3].bit_offset, 8);
        assert_eq!(fields[3].bit_size, 16);
        assert!(!fields[3].is_relative);

        // Y: Generic Desktop, usage 0x31, offset 24, size 16, absolute.
        assert_eq!(fields[4].usage_page, 0x0001);
        assert_eq!(fields[4].usage, 0x0031);
        assert_eq!(fields[4].bit_offset, 24);
        assert_eq!(fields[4].bit_size, 16);
        assert!(!fields[4].is_relative);

        // Wheel: Generic Desktop, usage 0x38, offset 40, size 8, relative.
        assert_eq!(fields[5].usage_page, 0x0001);
        assert_eq!(fields[5].usage, 0x0038);
        assert_eq!(fields[5].bit_offset, 40);
        assert_eq!(fields[5].bit_size, 8);
        assert!(fields[5].is_relative);
    }

    /// Decode a tablet report: buttons left+middle, abs X/Y positions, and a
    /// negative wheel delta.
    #[test]
    fn decode_tablet_report() {
        let fields = parse_report_descriptor(TABLET_DESC);
        // byte0 = 0x05 → buttons bit0 (left) and bit2 (middle)
        // X = 0x4000 (little-endian 0x00, 0x40)
        // Y = 0x2000 (little-endian 0x00, 0x20)
        // wheel = 0xFB = -5
        let report: &[u8] = &[0x05, 0x00, 0x40, 0x00, 0x20, 0xFB];
        let p = decode_pointer_report(&fields, report);
        assert_eq!(p.buttons, 0b101, "left + middle");
        assert_eq!(p.abs_x, Some(0x4000));
        assert_eq!(p.abs_y, Some(0x2000));
        assert_eq!(p.wheel, -5);
        assert!(p.any_input);
    }

    /// A simple relative mouse: 3 buttons + 5-bit const pad + relative i8 X + i8 Y.
    const REL_MOUSE_DESC: &[u8] = &[
        0x05, 0x01, // Usage Page = Generic Desktop
        0x09, 0x02, // Usage = Mouse
        0xA1, 0x01, // Collection (Application)
        0x05, 0x09, //   Usage Page = Button
        0x19, 0x01, //   Usage Minimum = 1
        0x29, 0x03, //   Usage Maximum = 3
        0x15, 0x00, //   Logical Minimum = 0
        0x25, 0x01, //   Logical Maximum = 1
        0x75, 0x01, //   Report Size = 1
        0x95, 0x03, //   Report Count = 3
        0x81, 0x02, //   Input (Data, Var, Abs) — 3 buttons
        0x75, 0x05, //   Report Size = 5
        0x95, 0x01, //   Report Count = 1
        0x81, 0x03, //   Input (Const) — 5-bit pad
        0x05, 0x01, //   Usage Page = Generic Desktop
        0x09, 0x30, //   Usage = X
        0x09, 0x31, //   Usage = Y
        0x15, 0x81, //   Logical Minimum = -127
        0x25, 0x7F, //   Logical Maximum = 127
        0x75, 0x08, //   Report Size = 8
        0x95, 0x02, //   Report Count = 2
        0x81, 0x06, //   Input (Data, Var, Rel) — X, Y relative
        0xC0, // End Collection
    ];

    /// Decode a relative mouse report with X=+5, Y=-3 → rel deltas, no abs.
    #[test]
    fn decode_relative_mouse_report() {
        let fields = parse_report_descriptor(REL_MOUSE_DESC);
        // X and Y are relative i8: +5 = 0x05, -3 = 0xFD.
        // byte0 = 0x00 (no buttons), byte1 = X = 0x05, byte2 = Y = 0xFD.
        let report: &[u8] = &[0x00, 0x05, 0xFD];
        let p = decode_pointer_report(&fields, report);
        assert_eq!(p.rel_x, 5);
        assert_eq!(p.rel_y, -3);
        assert_eq!(p.abs_x, None);
        assert_eq!(p.abs_y, None);
        assert_eq!(p.buttons, 0);
        assert!(p.any_input);
    }

    /// A two-Report-ID descriptor: ID 1 carries an absolute 8-bit X, ID 2 an
    /// absolute 8-bit Y. Decoding selects fields by report[0] and offsets the
    /// body by the 1-byte ID prefix.
    const TWO_ID_DESC: &[u8] = &[
        0x05, 0x01, // Usage Page = Generic Desktop
        // Report ID 1 → X
        0x85, 0x01, // Report ID = 1
        0x09, 0x30, // Usage = X
        0x15, 0x00, // Logical Minimum = 0
        0x25, 0xFF, // Logical Maximum = 255
        0x75, 0x08, // Report Size = 8
        0x95, 0x01, // Report Count = 1
        0x81, 0x02, // Input (Data, Var, Abs)
        // Report ID 2 → Y
        0x85, 0x02, // Report ID = 2
        0x09, 0x31, // Usage = Y
        0x75, 0x08, // Report Size = 8
        0x95, 0x01, // Report Count = 1
        0x81, 0x02, // Input (Data, Var, Abs)
    ];

    /// Decoding by report ID selects only the matching field and offsets past
    /// the 1-byte ID prefix.
    #[test]
    fn decode_report_id_selects_matching_field() {
        let fields = parse_report_descriptor(TWO_ID_DESC);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].report_id, 1);
        assert_eq!(fields[1].report_id, 2);

        // ID 1: X = 7, Y not decoded.
        let p1 = decode_pointer_report(&fields, &[0x01, 0x07]);
        assert_eq!(p1.abs_x, Some(7));
        assert_eq!(p1.abs_y, None);
        assert!(p1.any_input);

        // ID 2: Y = 9, X not decoded.
        let p2 = decode_pointer_report(&fields, &[0x02, 0x09]);
        assert_eq!(p2.abs_y, Some(9));
        assert_eq!(p2.abs_x, None);
        assert!(p2.any_input);
    }

    /// Decoding a too-short report against the tablet descriptor must not
    /// panic; out-of-bounds fields decode to 0 (so the absolute X/Y axes,
    /// which always produce a value, read back as `Some(0)`).
    #[test]
    fn decode_short_report_does_not_panic() {
        let fields = parse_report_descriptor(TABLET_DESC);
        // Only 1 byte — every multi-byte axis runs past the end.
        let p = decode_pointer_report(&fields, &[0x05]); // must not panic
        // Buttons live in byte 0, so they still decode.
        assert_eq!(p.buttons, 0b101);
        // The absolute axes always report a position; an OOB extract yields 0.
        assert_eq!(p.abs_x, Some(0));
        assert_eq!(p.abs_y, Some(0));
        // The relative wheel runs past the end → 0 delta.
        assert_eq!(p.wheel, 0);
    }

    /// `extract_bits` packs LSB-first and is bounds-safe.
    #[test]
    fn extract_bits_lsb_first_and_bounds_safe() {
        // 0b1010_0101 = 0xA5. Bits LSB-first: 1,0,1,0,0,1,0,1.
        let report: &[u8] = &[0xA5];
        assert_eq!(extract_bits(report, 0, 1), 1);
        assert_eq!(extract_bits(report, 1, 1), 0);
        assert_eq!(extract_bits(report, 0, 4), 0b0101);
        assert_eq!(extract_bits(report, 4, 4), 0b1010);
        assert_eq!(extract_bits(report, 0, 8), 0xA5);
        // Cross-byte little-endian: [0x34, 0x12] as a 16-bit field = 0x1234.
        let report2: &[u8] = &[0x34, 0x12];
        assert_eq!(extract_bits(report2, 0, 16), 0x1234);
        // Out of bounds → 0, no panic.
        assert_eq!(extract_bits(report, 4, 8), 0);
        assert_eq!(extract_bits(&[], 0, 8), 0);
    }

    /// `sign_extend` turns a top-bit-set value negative.
    #[test]
    fn sign_extend_handles_negatives() {
        assert_eq!(sign_extend(0x05, 8), 5);
        assert_eq!(sign_extend(0xFB, 8), -5);
        assert_eq!(sign_extend(0xFF, 8), -1);
        assert_eq!(sign_extend(0x7F, 8), 127);
        assert_eq!(sign_extend(0x80, 8), -128);
        assert_eq!(sign_extend(0x01, 1), -1); // 1-bit signed: top bit set
    }

    /// A 32-button device: Button usage 32 maps to bit 31 of the u32 bitmap
    /// (regression guard for the button-range cap — must be 1..=32, not 1..=31).
    #[test]
    fn decode_thirty_two_buttons() {
        let raw: &[u8] = &[
            0x05, 0x09, // Usage Page = Button
            0x19, 0x01, // Usage Minimum = 1
            0x29, 0x20, // Usage Maximum = 32
            0x15, 0x00, // Logical Minimum = 0
            0x25, 0x01, // Logical Maximum = 1
            0x75, 0x01, // Report Size = 1
            0x95, 0x20, // Report Count = 32
            0x81, 0x02, // Input (Data, Variable, Absolute)
        ];
        let fields = parse_report_descriptor(raw);
        assert_eq!(fields.len(), 32);
        // Only button 32 pressed → report bit 31 set (byte 3 == 0x80).
        let p = decode_pointer_report(&fields, &[0x00, 0x00, 0x00, 0x80]);
        assert_eq!(p.buttons, 1u32 << 31, "button 32 must set bit 31");
        // Button 1 pressed → bit 0.
        let p1 = decode_pointer_report(&fields, &[0x01, 0x00, 0x00, 0x00]);
        assert_eq!(p1.buttons, 0b1);
    }

    // -----------------------------------------------------------------------
    // B.3 consumer-control decode
    // -----------------------------------------------------------------------

    /// A bitmap Consumer-control descriptor: three 1-bit fields — Volume
    /// Increment (0xE9), Volume Decrement (0xEA), Mute (0xE2).
    const CONSUMER_BITMAP_DESC: &[u8] = &[
        0x05, 0x0C, // Usage Page (Consumer)
        0x09, 0x01, // Usage (Consumer Control)
        0xA1, 0x01, // Collection (Application)
        0x15, 0x00, // Logical Minimum (0)
        0x25, 0x01, // Logical Maximum (1)
        0x75, 0x01, // Report Size (1)
        0x95, 0x03, // Report Count (3)
        0x09, 0xE9, // Usage (Volume Increment)
        0x09, 0xEA, // Usage (Volume Decrement)
        0x09, 0xE2, // Usage (Mute)
        0x81, 0x02, // Input (Data, Variable, Absolute)
        0xC0, // End Collection
    ];

    /// The bitmap consumer descriptor parses to three Consumer-page fields.
    #[test]
    fn consumer_bitmap_descriptor_parses() {
        let fields = parse_report_descriptor(CONSUMER_BITMAP_DESC);
        assert_eq!(fields.len(), 3);
        assert!(
            fields
                .iter()
                .all(|f| f.usage_page == 0x0C && f.bit_size == 1)
        );
        assert_eq!(fields[0].usage, 0x00E9);
        assert_eq!(fields[1].usage, 0x00EA);
        assert_eq!(fields[2].usage, 0x00E2);
        assert_eq!(fields[0].bit_offset, 0);
        assert_eq!(fields[1].bit_offset, 1);
        assert_eq!(fields[2].bit_offset, 2);
    }

    /// Bit 0 set → Volume Increment is the only asserted usage.
    #[test]
    fn decode_consumer_single_bit() {
        let fields = parse_report_descriptor(CONSUMER_BITMAP_DESC);
        let active = decode_consumer_usages(&fields, &[0b0000_0001]);
        assert_eq!(active, alloc::vec![0x00E9]);
        // Mute (bit 2).
        let muted = decode_consumer_usages(&fields, &[0b0000_0100]);
        assert_eq!(muted, alloc::vec![0x00E2]);
    }

    /// Two bits set → two asserted usages, in declaration order.
    #[test]
    fn decode_consumer_multiple_bits() {
        let fields = parse_report_descriptor(CONSUMER_BITMAP_DESC);
        let active = decode_consumer_usages(&fields, &[0b0000_0011]);
        assert_eq!(active, alloc::vec![0x00E9, 0x00EA]);
        // No bits → empty, no panic on a short report.
        assert!(decode_consumer_usages(&fields, &[0x00]).is_empty());
        assert!(decode_consumer_usages(&fields, &[]).is_empty());
    }

    // --- Precision-Touchpad multitouch decode (Phase 102 Track C) ---

    fn tp_field(usage_page: u16, usage: u16, bit_offset: usize, bit_size: usize) -> ReportField {
        ReportField {
            usage_page,
            usage,
            bit_offset,
            bit_size,
            report_id: 1,
            is_relative: false,
        }
    }

    /// Two per-finger collections {Tip, ContactID, X, Y} + Contact Count +
    /// clickpad Button, all under report ID 1 (the shape a Precision Touchpad
    /// report descriptor parses to).
    fn two_contact_fields() -> Vec<ReportField> {
        alloc::vec![
            tp_field(0x0D, 0x42, 0, 1),   // contact0 Tip Switch
            tp_field(0x0D, 0x51, 8, 8),   // contact0 Contact ID
            tp_field(0x01, 0x30, 16, 16), // contact0 X (Generic Desktop)
            tp_field(0x01, 0x31, 32, 16), // contact0 Y
            tp_field(0x0D, 0x42, 48, 1),  // contact1 Tip Switch
            tp_field(0x0D, 0x51, 56, 8),  // contact1 Contact ID
            tp_field(0x01, 0x30, 64, 16), // contact1 X
            tp_field(0x01, 0x31, 80, 16), // contact1 Y
            tp_field(0x0D, 0x54, 96, 8),  // Contact Count
            tp_field(0x09, 0x01, 104, 1), // clickpad Button
        ]
    }

    #[test]
    fn touchpad_two_fingers_down_with_button() {
        let fields = two_contact_fields();
        // [reportID=1, tip0=1, id0=0, X0=0x0140, Y0=0x00C8, tip1=1, id1=1,
        //  X1=0x0280, Y1=0x0190, count=2, button=1]
        let report = [
            1u8, 0x01, 0x00, 0x40, 0x01, 0xC8, 0x00, 0x01, 0x01, 0x80, 0x02, 0x90, 0x01, 0x02,
            0x01,
        ];
        let frame = decode_touchpad_report(&fields, &report);
        assert_eq!(frame.contacts.len(), 2);
        assert_eq!(
            frame.contacts[0],
            TouchContact { tip: true, contact_id: 0, x: 320, y: 200 }
        );
        assert_eq!(
            frame.contacts[1],
            TouchContact { tip: true, contact_id: 1, x: 640, y: 400 }
        );
        assert_eq!(frame.contact_count, 2);
        assert!(frame.button);
        assert_eq!(frame.active_contacts().count(), 2);
    }

    #[test]
    fn touchpad_one_finger_lifted_is_a_slot_but_not_active() {
        let fields = two_contact_fields();
        // contact1 Tip byte (index 7) = 0 → lifted; button byte (14) = 0.
        let report = [
            1u8, 0x01, 0x00, 0x40, 0x01, 0xC8, 0x00, 0x00, 0x01, 0x80, 0x02, 0x90, 0x01, 0x02,
            0x00,
        ];
        let frame = decode_touchpad_report(&fields, &report);
        assert_eq!(frame.contacts.len(), 2, "both slots still present");
        assert!(frame.contacts[0].tip);
        assert!(!frame.contacts[1].tip, "second finger lifted");
        assert!(!frame.button);
        assert_eq!(
            frame.active_contacts().count(),
            1,
            "only the touching finger is active"
        );
    }

    #[test]
    fn touchpad_short_report_decodes_without_panicking() {
        let fields = two_contact_fields();
        // Only the report ID + the first body byte are present; every field past
        // the end reads as 0 (bounds-safe extract_bits).
        let frame = decode_touchpad_report(&fields, &[1u8, 0x01]);
        assert_eq!(frame.contacts.len(), 2);
        assert!(frame.contacts[0].tip, "byte present, tip bit set");
        assert!(!frame.contacts[1].tip, "past the report end → 0");
        assert_eq!(frame.contact_count, 0);
        assert!(!frame.button);
    }
}
