//! Phase 103 Track B acceptance (host side): a hand-assembled DSDT with
//! a backlight-capable display device (`_BCL`/`_BCM`/`_BQC`) enumerates
//! via `Namespace::backlight_devices()`, `_BCM` applies a level through
//! `evaluate_with_args` (Arg0 + `Store` — the first arg-taking method
//! the query surface exercises), `_BQC` reads the persisted level back,
//! and the pure `kernel_core::power::backlight` decode + percent
//! mapping round-trip.
//!
//! QEMU q35 firmware declares **no** backlight device, so the in-VM
//! gate can only prove the "none" posture; this fixture is the only
//! automated coverage of the populated path until a real-hardware DSDT
//! capture lands (the ThermalZone-fixture situation).

mod fixtures;

use fixtures::aml_builder as amb;
use kernel_core::acpi::aml::MockRegionSpace;
use kernel_core::acpi::aml::object::AmlValue;
use kernel_core::acpi::namespace::Namespace;
use kernel_core::power::backlight::{decode_bcl, decode_bqc};

/// DSDT: Scope(\_SB) {
///   Device(GFX0) {
///     Device(LCD0) {
///       Name(_BCL, Package{ 80, 50, 0, 20, 40, 50, 60, 80, 100 })
///       Name(BRTV, 50)
///       Method(_BCM, 1) { Store(Arg0, BRTV) }
///       Method(_BQC, 0) { Return(BRTV) }
///     }
///   }
///   Device(EC0) { Name(_HID, EisaId("PNP0C09")) }   // no _BCL
/// }
fn build_dsdt() -> Vec<u8> {
    let bcl_elems: Vec<Vec<u8>> = [80u32, 50, 0, 20, 40, 50, 60, 80, 100]
        .iter()
        .map(|&v| amb::dword(v))
        .collect();
    let mut lcd0 = amb::name("_BCL", &amb::package(&bcl_elems));
    lcd0.extend_from_slice(&amb::name("BRTV", &amb::dword(50)));
    // Store(Arg0, BRTV): 0x70 Arg0(0x68) NameSeg.
    let mut bcm_body = vec![0x70, 0x68];
    bcm_body.extend_from_slice(&amb::seg("BRTV"));
    lcd0.extend_from_slice(&amb::method("_BCM", 1, &bcm_body));
    // Return(BRTV): 0xA4 NameSeg.
    let mut bqc_body = vec![0xA4];
    bqc_body.extend_from_slice(&amb::seg("BRTV"));
    lcd0.extend_from_slice(&amb::method("_BQC", 0, &bqc_body));

    let gfx0 = amb::device("GFX0", &amb::device("LCD0", &lcd0));
    let ec0 = amb::device("EC0", &amb::name("_HID", &amb::dword(0x090C_D041)));
    let mut body = gfx0;
    body.extend_from_slice(&ec0);

    let mut sb_path = vec![0x5C]; // '\'
    sb_path.extend_from_slice(&amb::seg("_SB"));
    amb::table(b"DSDT", &amb::scope(&sb_path, &body))
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
fn backlight_devices_enumerates_only_bcl_carriers() {
    let (ns, _mock) = load();
    let paths: Vec<String> = ns
        .backlight_devices()
        .iter()
        .map(|&id| ns.full_path(id))
        .collect();
    assert_eq!(paths, ["\\_SB.GFX0.LCD0"], "EC0 and GFX0 itself excluded");
}

#[test]
fn bcm_applies_and_bqc_reads_back() {
    let (mut ns, mut mock) = load();

    let bcl = ns
        .evaluate(&mut mock, "\\_SB.GFX0.LCD0._BCL")
        .ok()
        .and_then(|v| decode_bcl(&v))
        .expect("_BCL decodes");
    assert_eq!(bcl.ac_default, 80);
    assert_eq!(bcl.battery_default, 50);
    assert_eq!(bcl.levels, [0, 20, 40, 50, 60, 80, 100]);

    // Initial _BQC reflects the fixture's boot level.
    let bqc = ns
        .evaluate(&mut mock, "\\_SB.GFX0.LCD0._BQC")
        .ok()
        .and_then(|v| decode_bqc(&v))
        .expect("_BQC decodes");
    assert_eq!(bqc, 50);

    // The powerd sequence: pct → nearest level → _BCM(level) → _BQC.
    let target = bcl.nearest_level(100);
    assert_eq!(target, 100);
    ns.evaluate_with_args(
        &mut mock,
        "\\_SB.GFX0.LCD0._BCM",
        vec![AmlValue::Integer(target as u64)],
    )
    .expect("_BCM evaluates with Arg0");
    let bqc = ns
        .evaluate(&mut mock, "\\_SB.GFX0.LCD0._BQC")
        .ok()
        .and_then(|v| decode_bqc(&v))
        .expect("_BQC decodes after set");
    assert_eq!(bqc, 100, "Store(Arg0, BRTV) persisted the new level");
    assert_eq!(bcl.level_to_percent(bqc), 100);
}
