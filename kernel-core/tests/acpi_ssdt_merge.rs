//! Phase 101 Track B.2/B.3/B.4 + C.5 acceptance: a DSDT plus an SSDT
//! that re-opens a DSDT-defined scope merge into one namespace, string
//! and `EisaId` `_HID`s both match, `_STA` filters absent devices, and
//! `device_resources` answers the touchpad question end to end.
//!
//! The synthetic SSDT mirrors the shape the Dell's tables give the
//! `DLL0945` touchpad (string `_HID`, `_STA` method, `_CRS` with I2C
//! SerialBus + GpioInt) so these tests keep covering the exact query
//! path Phase 102 will use until the real capture lands (see
//! `fixtures/acpi/README.md`).

mod fixtures;

use fixtures::aml_builder as amb;
use kernel_core::acpi::aml::MockRegionSpace;
use kernel_core::acpi::namespace::Namespace;
use kernel_core::acpi::resource::{Polarity, ResourceItem, Trigger};

/// DSDT: Scope(\_SB) { Device(PCI0) { Name(_HID, EisaId("PNP0A08")) } }
fn build_dsdt() -> Vec<u8> {
    let pci0 = amb::device("PCI0", &amb::name("_HID", &amb::dword(0x080A_D041)));
    let mut sb_path = vec![0x5C]; // '\'
    sb_path.extend_from_slice(&amb::seg("_SB"));
    amb::table(b"DSDT", &amb::scope(&sb_path, &pci0))
}

/// SSDT: Scope(\_SB.PCI0) {
///   Device(TPD0) { _HID "DLL0945"; Method(_STA)->0x0F; _CRS touchpad }
///   Device(TPD1) { _HID "DLL0946"; Method(_STA)->Zero }
/// }
fn build_ssdt() -> Vec<u8> {
    let sta_present = amb::method("_STA", 0, &[0xA4, 0x0A, 0x0F]); // Return(0x0F)
    let sta_absent = amb::method("_STA", 0, &[0xA4, 0x00]); // Return(Zero)
    let mut tpd0_body = amb::name("_HID", &amb::string("DLL0945"));
    tpd0_body.extend_from_slice(&sta_present);
    tpd0_body.extend_from_slice(&amb::name(
        "_CRS",
        &amb::buffer(&amb::touchpad_crs(0x2C, 0x0112)),
    ));
    let mut tpd1_body = amb::name("_HID", &amb::string("DLL0946"));
    tpd1_body.extend_from_slice(&sta_absent);
    let mut devices = amb::device("TPD0", &tpd0_body);
    devices.extend_from_slice(&amb::device("TPD1", &tpd1_body));
    // \_SB.PCI0 as a root-anchored dual NameString.
    let mut path = vec![0x5C, 0x2E];
    path.extend_from_slice(&amb::seg("_SB"));
    path.extend_from_slice(&amb::seg("PCI0"));
    amb::table(b"SSDT", &amb::scope(&path, &devices))
}

fn load_both() -> (Namespace, MockRegionSpace) {
    let mut ns = Namespace::new();
    let mut mock = MockRegionSpace::new();
    for t in [build_dsdt(), build_ssdt()] {
        let summary = ns.load_table(&t, &mut mock).expect("fixture loads");
        assert!(summary.skipped.is_empty(), "skipped: {:?}", summary.skipped);
    }
    (ns, mock)
}

#[test]
fn ssdt_extends_dsdt_scope() {
    let (ns, _mock) = load_both();
    // The SSDT's Scope(\_SB.PCI0) re-opened the DSDT's device — the
    // touchpad must hang under the ORIGINAL node, no duplicate PCI0.
    let pci0 = ns.resolve_str("\\_SB.PCI0").expect("PCI0 from DSDT");
    let tpd0 = ns.resolve_str("\\_SB.PCI0.TPD0").expect("TPD0 from SSDT");
    assert_eq!(ns.node(tpd0).parent, Some(pci0));
    let sb = ns.resolve_str("\\_SB").unwrap();
    let pci_children = ns
        .node(sb)
        .children
        .iter()
        .filter(|&&c| ns.node(c).seg == *b"PCI0")
        .count();
    assert_eq!(pci_children, 1, "duplicate PCI0 after merge");
}

#[test]
fn string_hid_and_eisa_hid_both_match() {
    let (mut ns, mut mock) = load_both();
    // String _HID from the SSDT.
    let touchpads = ns.find_by_hid(&mut mock, "DLL0945");
    assert_eq!(touchpads.len(), 1);
    assert_eq!(ns.full_path(touchpads[0]), "\\_SB.PCI0.TPD0");
    // EisaId integer _HID from the DSDT.
    let pci = ns.find_by_hid(&mut mock, "PNP0A08");
    assert_eq!(pci.len(), 1);
    assert_eq!(ns.full_path(pci[0]), "\\_SB.PCI0");
}

#[test]
fn sta_zero_devices_are_filtered() {
    let (mut ns, mut mock) = load_both();
    // TPD1 exists in the tree but its _STA returns 0 → not enumerated.
    assert!(ns.resolve_str("\\_SB.PCI0.TPD1").is_some());
    assert!(ns.find_by_hid(&mut mock, "DLL0946").is_empty());
    let tpd1 = ns.resolve_str("\\_SB.PCI0.TPD1").unwrap();
    assert_eq!(ns.sta(&mut mock, tpd1), 0);
}

#[test]
fn device_resources_answers_the_touchpad_question() {
    let (mut ns, mut mock) = load_both();
    let tpd0 = ns.find_by_hid(&mut mock, "DLL0945")[0];
    let res = ns
        .device_resources(&mut mock, tpd0)
        .expect("_CRS evaluates and decodes");
    let Some(ResourceItem::I2cSerialBus {
        address,
        speed_hz,
        source,
        ..
    }) = res.i2c()
    else {
        panic!("no I2C connection: {:?}", res.items);
    };
    assert_eq!(*address, 0x2C);
    assert_eq!(*speed_hz, 400_000);
    assert_eq!(source, "\\_SB.PCI0.I2C1");
    let Some(ResourceItem::GpioInt {
        pins,
        trigger,
        polarity,
        source,
        ..
    }) = res.gpio_int()
    else {
        panic!("no GpioInt: {:?}", res.items);
    };
    assert_eq!(pins.as_slice(), &[0x0112]);
    assert_eq!(*trigger, Trigger::Level);
    assert_eq!(*polarity, Polarity::ActiveLow);
    assert_eq!(source, "\\_SB.GPI0");
}
