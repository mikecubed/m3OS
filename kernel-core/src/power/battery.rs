//! Phase 103 A.2 — pure-logic battery/AC decode over evaluated ACPI
//! objects (ACPI 6.5 §10.2). Host-tested; no kernel dependencies.
//!
//! The inputs are [`AmlValue`]s as returned by evaluating the Control
//! Method Battery (`PNP0C0A`) and AC adapter (`ACPI0003`) methods
//! through the Phase 101 interpreter (in-process or over acpid's
//! `ACPI_EVAL` IPC verb):
//!
//! - `_BST` → [`BatteryStatus`]: `[state, present_rate, remaining_capacity,
//!   present_voltage]` (4 integers).
//! - `_BIF` → [`BatteryInfo`]: `[power_unit, design_capacity,
//!   last_full_capacity, technology, design_voltage, …]`.
//! - `_BIX` → the `_BIF` layout shifted by one leading `revision` field.
//! - `_PSR` → AC online (integer 1) / offline (0).
//!
//! The percentage is COMPUTED (`remaining / last_full`), with the two
//! classic gotchas covered: the `0xFFFF_FFFF` "unknown" sentinel any
//! field may carry, and the power-unit field (mW vs mA) which does NOT
//! affect the percentage (both operands share the unit) but is kept for
//! rate display.

use super::super::acpi::aml::object::AmlValue;

/// ACPI's "unknown" sentinel for battery fields (§10.2.2.6).
pub const ACPI_UNKNOWN: u32 = 0xFFFF_FFFF;

/// `_BST` state bit: discharging.
pub const BST_STATE_DISCHARGING: u32 = 1 << 0;
/// `_BST` state bit: charging.
pub const BST_STATE_CHARGING: u32 = 1 << 1;
/// `_BST` state bit: critical energy level.
pub const BST_STATE_CRITICAL: u32 = 1 << 2;

/// Decoded `_BST` (dynamic battery status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub state: u32,
    /// Discharge/charge rate in the `_BIF` power unit (mW or mA);
    /// [`ACPI_UNKNOWN`] when firmware cannot report it.
    pub present_rate: u32,
    pub remaining_capacity: u32,
    pub present_voltage: u32,
}

/// Decoded `_BIF`/`_BIX` (static battery info; the `_BIX` revision field
/// is skipped so both share this shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryInfo {
    /// 0 = mW/mWh, 1 = mA/mAh (§10.2.2.1).
    pub power_unit: u32,
    pub design_capacity: u32,
    pub last_full_capacity: u32,
    pub design_voltage: u32,
}

fn package_int(elems: &[AmlValue], index: usize) -> Option<u32> {
    match elems.get(index)? {
        AmlValue::Integer(v) => u32::try_from(*v & 0xFFFF_FFFF).ok(),
        _ => None,
    }
}

/// Decode a `_BST` package. `None` on any shape violation.
pub fn decode_bst(value: &AmlValue) -> Option<BatteryStatus> {
    let AmlValue::Package(elems) = value else {
        return None;
    };
    if elems.len() < 4 {
        return None;
    }
    Some(BatteryStatus {
        state: package_int(elems, 0)?,
        present_rate: package_int(elems, 1)?,
        remaining_capacity: package_int(elems, 2)?,
        present_voltage: package_int(elems, 3)?,
    })
}

/// Decode a `_BIF` package (fields 0/1/2/4 of the 13-element layout).
pub fn decode_bif(value: &AmlValue) -> Option<BatteryInfo> {
    decode_info_at(value, 0)
}

/// Decode a `_BIX` package — the `_BIF` layout preceded by a revision
/// integer (§10.2.2.2), so every field shifts by one.
pub fn decode_bix(value: &AmlValue) -> Option<BatteryInfo> {
    decode_info_at(value, 1)
}

fn decode_info_at(value: &AmlValue, base: usize) -> Option<BatteryInfo> {
    let AmlValue::Package(elems) = value else {
        return None;
    };
    if elems.len() < base + 5 {
        return None;
    }
    Some(BatteryInfo {
        power_unit: package_int(elems, base)?,
        design_capacity: package_int(elems, base + 1)?,
        last_full_capacity: package_int(elems, base + 2)?,
        design_voltage: package_int(elems, base + 4)?,
    })
}

/// Decode `_PSR`: AC adapter online (`Some(true)`) / offline
/// (`Some(false)`); `None` for anything non-integer or out of range.
pub fn decode_psr(value: &AmlValue) -> Option<bool> {
    match value {
        AmlValue::Integer(0) => Some(false),
        AmlValue::Integer(1) => Some(true),
        _ => None,
    }
}

/// Compute the battery percentage (0–100) from `_BST` + `_BIF`/`_BIX`.
///
/// `None` when either operand is the [`ACPI_UNKNOWN`] sentinel or the
/// last-full capacity is zero (fresh/bogus firmware). A remaining
/// capacity above last-full (seen on aged batteries mid-calibration)
/// clamps to 100 rather than reporting >100%.
pub fn percent(status: &BatteryStatus, info: &BatteryInfo) -> Option<u8> {
    if status.remaining_capacity == ACPI_UNKNOWN
        || info.last_full_capacity == ACPI_UNKNOWN
        || info.last_full_capacity == 0
    {
        return None;
    }
    let pct = (u64::from(status.remaining_capacity) * 100) / u64::from(info.last_full_capacity);
    Some(pct.min(100) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    /// A Dell-shaped `_BIF`: mWh unit, 57 Wh design / 50 Wh last-full.
    fn dell_bif() -> AmlValue {
        AmlValue::Package(vec![
            AmlValue::Integer(0),      // power unit: mW/mWh
            AmlValue::Integer(56_999), // design capacity
            AmlValue::Integer(50_110), // last full charge capacity
            AmlValue::Integer(1),      // technology: rechargeable
            AmlValue::Integer(11_400), // design voltage (mV)
            AmlValue::Integer(5_699),  // warning capacity
            AmlValue::Integer(1_710),  // low capacity
            AmlValue::Integer(357),    // granularity 1
            AmlValue::Integer(357),    // granularity 2
            AmlValue::String(String::from("DELL M59JH14")),
            AmlValue::String(String::from("01234")),
            AmlValue::String(String::from("LION")),
            AmlValue::String(String::from("SMP")),
        ])
    }

    fn bst(state: u32, rate: u32, remaining: u32) -> AmlValue {
        AmlValue::Package(vec![
            AmlValue::Integer(u64::from(state)),
            AmlValue::Integer(u64::from(rate)),
            AmlValue::Integer(u64::from(remaining)),
            AmlValue::Integer(11_213),
        ])
    }

    #[test]
    fn decodes_dell_shaped_bif_and_bst_to_percent() {
        let info = decode_bif(&dell_bif()).expect("bif decodes");
        assert_eq!(info.power_unit, 0);
        assert_eq!(info.last_full_capacity, 50_110);
        assert_eq!(info.design_voltage, 11_400);

        let status = decode_bst(&bst(BST_STATE_DISCHARGING, 8_760, 25_055)).expect("bst");
        assert_eq!(status.state & BST_STATE_DISCHARGING, BST_STATE_DISCHARGING);
        // 25055 / 50110 = exactly 50%.
        assert_eq!(percent(&status, &info), Some(50));
    }

    #[test]
    fn bix_layout_shifts_by_the_revision_field() {
        // Wrap the _BIF fields behind a leading revision integer.
        let AmlValue::Package(mut elems) = dell_bif() else {
            unreachable!()
        };
        elems.insert(0, AmlValue::Integer(1)); // revision
        let info = decode_bix(&AmlValue::Package(elems)).expect("bix decodes");
        assert_eq!(info.last_full_capacity, 50_110);
        assert_eq!(info.design_voltage, 11_400);
    }

    #[test]
    fn unknown_sentinels_yield_no_percent() {
        let info = decode_bif(&dell_bif()).unwrap();
        let status = decode_bst(&bst(BST_STATE_CHARGING, ACPI_UNKNOWN, ACPI_UNKNOWN)).unwrap();
        assert_eq!(percent(&status, &info), None);

        // Unknown last-full also refuses to fabricate a percentage.
        let bogus = BatteryInfo {
            last_full_capacity: ACPI_UNKNOWN,
            ..info
        };
        let ok = decode_bst(&bst(BST_STATE_CHARGING, 100, 10_000)).unwrap();
        assert_eq!(percent(&ok, &bogus), None);
    }

    #[test]
    fn zero_last_full_capacity_yields_no_percent() {
        let info = BatteryInfo {
            power_unit: 0,
            design_capacity: 0,
            last_full_capacity: 0,
            design_voltage: 0,
        };
        let status = decode_bst(&bst(0, 0, 100)).unwrap();
        assert_eq!(percent(&status, &info), None);
    }

    #[test]
    fn over_full_battery_clamps_to_100() {
        let info = decode_bif(&dell_bif()).unwrap();
        let status = decode_bst(&bst(BST_STATE_CHARGING, 0, 51_000)).unwrap();
        assert_eq!(percent(&status, &info), Some(100));
    }

    #[test]
    fn psr_decodes_online_offline_and_rejects_junk() {
        assert_eq!(decode_psr(&AmlValue::Integer(1)), Some(true));
        assert_eq!(decode_psr(&AmlValue::Integer(0)), Some(false));
        assert_eq!(decode_psr(&AmlValue::Integer(2)), None);
        assert_eq!(decode_psr(&AmlValue::Package(vec![])), None);
    }

    #[test]
    fn malformed_packages_decode_to_none() {
        assert_eq!(decode_bst(&AmlValue::Integer(1)), None);
        assert_eq!(
            decode_bst(&AmlValue::Package(vec![AmlValue::Integer(1)])),
            None
        );
        assert_eq!(
            decode_bif(&AmlValue::Package(vec![AmlValue::String(String::from(
                "x"
            ))])),
            None
        );
    }
}
