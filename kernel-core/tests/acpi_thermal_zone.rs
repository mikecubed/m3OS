//! Phase 103 Track C acceptance (host side): a hand-assembled DSDT with
//! a `ThermalZone` enumerates via `Namespace::thermal_zones()`, its
//! `_TMP`/`_CRT`/`_PSV` evaluate through the interpreter, and the pure
//! thermal decode + trip classification produce the expected posture.
//!
//! QEMU q35 firmware declares **no** thermal zones, so the in-VM gate
//! can only prove the "zero zones" posture; this fixture is the only
//! automated coverage of the populated path until a real-hardware DSDT
//! capture lands (same situation as the Dell touchpad `_CRS` fixture).

mod fixtures;

use fixtures::aml_builder as amb;
use kernel_core::acpi::aml::MockRegionSpace;
use kernel_core::acpi::namespace::Namespace;
use kernel_core::power::thermal::{ThermalState, TripPoints, classify, decode_temp_dk};

/// DSDT: Scope(\_TZ) {
///   ThermalZone(TZ00) {
///     Method(_TMP) -> 2982   // 25.0 °C
///     Name(_CRT, 3732)       // 100.0 °C
///     Name(_PSV, 3582)       // 85.0 °C
///   }
///   ThermalZone(TZ01) { Method(_TMP) -> 3650 }   // 91.8 °C, no trips
///   Device(EC0) { Name(_HID, EisaId("PNP0C09")) }  // not a zone
/// }
fn build_dsdt() -> Vec<u8> {
    let mut tz00 = amb::method("_TMP", 0, &return_int(2982));
    tz00.extend_from_slice(&amb::name("_CRT", &amb::dword(3732)));
    tz00.extend_from_slice(&amb::name("_PSV", &amb::dword(3582)));
    let tz01 = amb::method("_TMP", 0, &return_int(3650));
    let ec0 = amb::device("EC0", &amb::name("_HID", &amb::dword(0x090C_D041)));

    let mut body = amb::thermal_zone("TZ00", &tz00);
    body.extend_from_slice(&amb::thermal_zone("TZ01", &tz01));
    body.extend_from_slice(&ec0);

    let mut tz_path = vec![0x5C]; // '\'
    tz_path.extend_from_slice(&amb::seg("_TZ"));
    amb::table(b"DSDT", &amb::scope(&tz_path, &body))
}

/// `Return(<dword literal>)`.
fn return_int(v: u32) -> Vec<u8> {
    let mut b = vec![0xA4];
    b.extend_from_slice(&amb::dword(v));
    b
}

fn load() -> (Namespace, MockRegionSpace) {
    let mut ns = Namespace::new();
    let mut mock = MockRegionSpace::new();
    let summary = ns
        .load_table(&build_dsdt(), &mut mock)
        .expect("fixture loads");
    assert!(summary.skipped.is_empty(), "skipped: {:?}", summary.skipped);
    (ns, mock)
}

#[test]
fn thermal_zones_enumerates_only_zones() {
    let (ns, _mock) = load();
    let zones = ns.thermal_zones();
    let paths: Vec<String> = zones.iter().map(|&id| ns.full_path(id)).collect();
    assert_eq!(
        paths,
        ["\\_TZ.TZ00", "\\_TZ.TZ01"],
        "zones in arena order, EC0 excluded"
    );
    // devices() still includes the zones (device-scope behavior) — the
    // new accessor is a strict subset filter, not a reclassification.
    let devs = ns.devices();
    assert!(zones.iter().all(|z| devs.contains(z)));
}

#[test]
fn zone_methods_evaluate_and_classify() {
    let (mut ns, mut mock) = load();

    let tmp = ns
        .evaluate(&mut mock, "\\_TZ.TZ00._TMP")
        .expect("_TMP evaluates");
    let tmp_dk = decode_temp_dk(&tmp).expect("plausible temperature");
    assert_eq!(tmp_dk, 2982);

    let crt = ns
        .evaluate(&mut mock, "\\_TZ.TZ00._CRT")
        .expect("_CRT evaluates");
    let psv = ns
        .evaluate(&mut mock, "\\_TZ.TZ00._PSV")
        .expect("_PSV evaluates");
    let trips = TripPoints {
        critical_dk: decode_temp_dk(&crt),
        passive_dk: decode_temp_dk(&psv),
    };
    assert_eq!(trips.critical_dk, Some(3732));
    assert_eq!(trips.passive_dk, Some(3582));
    assert_eq!(classify(tmp_dk, &trips), ThermalState::Normal);
    // The same zone at 90 °C sits in the passive band.
    assert_eq!(classify(3632, &trips), ThermalState::Passive);
}

#[test]
fn zone_without_trips_reads_but_never_escalates() {
    let (mut ns, mut mock) = load();
    let tmp = ns
        .evaluate(&mut mock, "\\_TZ.TZ01._TMP")
        .expect("_TMP evaluates");
    let tmp_dk = decode_temp_dk(&tmp).expect("plausible temperature");
    assert_eq!(tmp_dk, 3650);
    // No _CRT/_PSV declared: evaluation fails per-method, trips stay None.
    assert!(ns.evaluate(&mut mock, "\\_TZ.TZ01._CRT").is_err());
    assert_eq!(
        classify(tmp_dk, &TripPoints::default()),
        ThermalState::Normal
    );
}
