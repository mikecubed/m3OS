//! Phase 103 C — pure-logic thermal decode + trip classification
//! (ACPI 6.5 §11). Host-tested; no kernel dependencies.
//!
//! ACPI thermal objects report temperatures in **decikelvin** (tenths
//! of a Kelvin): `_TMP` = current temperature, `_CRT` = critical trip,
//! `_PSV` = passive-cooling trip. The conversion and the trip
//! comparison are the falsifiable core; the evaluation itself rides
//! acpid's `ACPI_EVAL` like the battery methods.

use super::super::acpi::aml::object::AmlValue;

/// 0 °C in decikelvin (273.15 K, truncated to tenths — the ACPI
/// convention).
pub const ZERO_CELSIUS_DECIKELVIN: i64 = 2732;

/// Convert an ACPI decikelvin reading to **deci-celsius** (tenths of a
/// degree, keeping the sensor's full precision): 2982 dK → 250 (25.0 °C).
pub fn deci_celsius_from_decikelvin(raw: u64) -> i64 {
    raw as i64 - ZERO_CELSIUS_DECIKELVIN
}

/// Whole degrees Celsius, truncated toward zero.
pub fn celsius_from_decikelvin(raw: u64) -> i64 {
    deci_celsius_from_decikelvin(raw) / 10
}

/// Decode a `_TMP`/`_CRT`/`_PSV` result (a bare integer, decikelvin).
/// Rejects non-integers and readings outside a physically plausible
/// window (0 dK and > 5000 dK ≈ 227 °C are firmware junk).
pub fn decode_temp_dk(value: &AmlValue) -> Option<u64> {
    match value {
        AmlValue::Integer(v) if *v > 0 && *v <= 5000 => Some(*v),
        _ => None,
    }
}

/// A zone's trip points (absent methods leave `None` — many zones
/// declare only `_CRT`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TripPoints {
    pub critical_dk: Option<u64>,
    pub passive_dk: Option<u64>,
}

/// Classified thermal posture for one zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThermalState {
    /// Below every declared trip.
    Normal,
    /// At/above `_PSV` — the governor should cap performance.
    Passive,
    /// At/above `_CRT` — initiate critical shutdown.
    Critical,
}

/// Classify a `_TMP` reading against the zone's trips. `_CRT` dominates;
/// an inverted firmware table (`_PSV` > `_CRT`) still classifies
/// critical correctly because `_CRT` is checked first.
pub fn classify(tmp_dk: u64, trips: &TripPoints) -> ThermalState {
    if let Some(crt) = trips.critical_dk
        && tmp_dk >= crt
    {
        return ThermalState::Critical;
    }
    if let Some(psv) = trips.passive_dk
        && tmp_dk >= psv
    {
        return ThermalState::Passive;
    }
    ThermalState::Normal
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn decikelvin_conversion_matches_known_points() {
        // 25.0 °C = 298.2 K = 2982 dK.
        assert_eq!(deci_celsius_from_decikelvin(2982), 250);
        assert_eq!(celsius_from_decikelvin(2982), 25);
        // 0.0 °C.
        assert_eq!(deci_celsius_from_decikelvin(2732), 0);
        // A hot laptop: 95.0 °C = 3682 dK.
        assert_eq!(celsius_from_decikelvin(3682), 95);
        // Below freezing keeps its sign: -10.0 °C = 2632 dK.
        assert_eq!(deci_celsius_from_decikelvin(2632), -100);
    }

    #[test]
    fn temp_decode_rejects_junk() {
        assert_eq!(decode_temp_dk(&AmlValue::Integer(2982)), Some(2982));
        assert_eq!(decode_temp_dk(&AmlValue::Integer(0)), None);
        assert_eq!(decode_temp_dk(&AmlValue::Integer(60_000)), None);
        assert_eq!(decode_temp_dk(&AmlValue::Package(vec![])), None);
    }

    #[test]
    fn classification_honors_trip_order() {
        // Dell-shaped zone: passive 85 °C (3582), critical 100 °C (3732).
        let trips = TripPoints {
            critical_dk: Some(3732),
            passive_dk: Some(3582),
        };
        assert_eq!(classify(2982, &trips), ThermalState::Normal);
        assert_eq!(classify(3581, &trips), ThermalState::Normal);
        assert_eq!(classify(3582, &trips), ThermalState::Passive); // at-trip
        assert_eq!(classify(3731, &trips), ThermalState::Passive);
        assert_eq!(classify(3732, &trips), ThermalState::Critical); // at-trip
        assert_eq!(classify(4000, &trips), ThermalState::Critical);
    }

    #[test]
    fn missing_trips_never_escalate() {
        assert_eq!(classify(5000, &TripPoints::default()), ThermalState::Normal);
        let crt_only = TripPoints {
            critical_dk: Some(3732),
            passive_dk: None,
        };
        assert_eq!(classify(3600, &crt_only), ThermalState::Normal);
        assert_eq!(classify(3732, &crt_only), ThermalState::Critical);
    }

    #[test]
    fn inverted_firmware_trips_still_classify_critical() {
        let inverted = TripPoints {
            critical_dk: Some(3500),
            passive_dk: Some(3600), // firmware bug: passive above critical
        };
        assert_eq!(classify(3550, &inverted), ThermalState::Critical);
    }
}
