//! Phase 101 Track A/B/C integration surface: load a *real* firmware
//! definition block — the DSDT QEMU generates for the q35 machine — and
//! drive the namespace/query/`_CRS` pipeline end to end.
//!
//! This is the host-side twin of the planned QEMU `acpi-smoke` gate:
//! the same bytes the in-VM namespace build will parse. The Dell
//! Tiger Lake capture (see `fixtures/acpi/README.md`) extends these
//! tests to real laptop silicon once the next hardware session lands it.

use kernel_core::acpi::aml::MockRegionSpace;
use kernel_core::acpi::namespace::{Namespace, NodeObject, eisa_id_decode};
use kernel_core::acpi::resource::{ResourceItem, decode_crs};

const QEMU_Q35_DSDT: &[u8] = include_bytes!("fixtures/acpi/qemu-q35-dsdt.aml");

fn load_qemu_dsdt() -> (Namespace, MockRegionSpace) {
    let mut ns = Namespace::new();
    let mut mock = MockRegionSpace::new();
    let summary = ns
        .load_table(QEMU_Q35_DSDT, &mut mock)
        .expect("QEMU q35 DSDT must load");
    assert!(
        summary.skipped.is_empty(),
        "QEMU q35 DSDT loaded with skipped packages: {:?}",
        summary.skipped
    );
    (ns, mock)
}

#[test]
fn qemu_dsdt_builds_namespace() {
    let (ns, _mock) = load_qemu_dsdt();
    // The tree is substantial (PCI hierarchy, ISA devices, GPE block).
    assert!(ns.len() > 100, "only {} nodes built", ns.len());
    // Core scopes the q35 DSDT populates.
    for path in ["\\_SB", "\\_SB.PCI0", "\\_GPE"] {
        assert!(ns.resolve_str(path).is_some(), "{path} missing");
    }
    // The PCI root device carries the PCIe root-complex _HID.
    let pci0 = ns.resolve_str("\\_SB.PCI0").unwrap();
    assert!(matches!(ns.node(pci0).object, NodeObject::Device));
}

#[test]
fn qemu_dsdt_finds_devices_by_hid() {
    let (mut ns, mut mock) = load_qemu_dsdt();
    // The PCIe root complex: EisaId("PNP0A08") with a PNP0A03 _CID.
    let roots = ns.find_by_hid(&mut mock, "PNP0A08");
    assert_eq!(roots.len(), 1, "PCIe root complex not found");
    assert_eq!(ns.full_path(roots[0]), "\\_SB.PCI0");
    let via_cid = ns.find_by_hid(&mut mock, "PNP0A03");
    assert_eq!(via_cid, roots, "_CID fallback should find the same node");
}

#[test]
fn qemu_dsdt_com1_crs_decodes() {
    let (mut ns, mut mock) = load_qemu_dsdt();
    // COM1: EisaId("PNP0501"), IO 0x3F8/8 + IRQ 4.
    let coms = ns.find_by_hid(&mut mock, "PNP0501");
    assert!(!coms.is_empty(), "no PNP0501 UART found");
    let crs = ns
        .crs_bytes(&mut mock, coms[0])
        .expect("COM1 _CRS must evaluate to a buffer");
    let res = decode_crs(&crs).expect("COM1 _CRS must decode");
    let io = res.items.iter().find_map(|i| match i {
        ResourceItem::Io { min, len, .. } => Some((*min, *len)),
        _ => None,
    });
    assert_eq!(
        io,
        Some((0x3F8, 8)),
        "COM1 IO window wrong: {:?}",
        res.items
    );
    assert_eq!(res.interrupts(), vec![4], "COM1 IRQ wrong: {:?}", res.items);
}

#[test]
fn qemu_dsdt_sta_evaluates() {
    let (mut ns, mut mock) = load_qemu_dsdt();
    // COM1's _STA on q35 reads the LPC config space (via a field) or is
    // constant depending on QEMU version; either way it must *evaluate*
    // without error through the mock region backend.
    let coms = ns.find_by_hid(&mut mock, "PNP0501");
    for dev in coms {
        let sta = ns.sta(&mut mock, dev);
        assert!(sta <= 0x1F, "implausible _STA {sta:#x}");
    }
}

#[test]
fn qemu_dsdt_eisa_ids_decode() {
    // Spot-check the EISA decode against IDs the q35 DSDT uses.
    assert_eq!(eisa_id_decode(0x080AD041), "PNP0A08");
    assert_eq!(eisa_id_decode(0x0105D041), "PNP0501");
    assert_eq!(eisa_id_decode(0x0303D041), "PNP0303");
}

#[test]
fn qemu_dsdt_truncation_never_panics() {
    // Slice the real DSDT at a spread of prefix lengths (with the header
    // length field patched to match) — every cut must fail or succeed
    // cleanly, never panic. This is the no-panic-on-malformed-AML
    // acceptance criterion run against real firmware bytes.
    let mut cut = 0usize;
    while cut < QEMU_Q35_DSDT.len() {
        let mut short = QEMU_Q35_DSDT[..cut].to_vec();
        if cut >= 8 {
            short[4..8].copy_from_slice(&(cut as u32).to_le_bytes());
        }
        let mut ns = Namespace::new();
        let mut mock = MockRegionSpace::new();
        let _ = ns.load_table(&short, &mut mock);
        cut += 13;
    }
}

#[test]
fn qemu_dsdt_corruption_never_panics() {
    // Flip a byte at a spread of offsets in the body and reload: the
    // loader may skip packages or fail, but must not panic.
    let mut offset = 36usize;
    while offset < QEMU_Q35_DSDT.len() {
        let mut bent = QEMU_Q35_DSDT.to_vec();
        bent[offset] ^= 0xFF;
        let mut ns = Namespace::new();
        let mut mock = MockRegionSpace::new();
        let _ = ns.load_table(&bent, &mut mock);
        offset += 131;
    }
}
