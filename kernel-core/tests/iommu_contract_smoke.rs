//! Smoke test proving the `MockUnit` reference impl satisfies the
//! `IommuUnit` trait end-to-end. Phase 55a Track A.1.
//!
//! Committed failing *before* `kernel_core::iommu::contract` is
//! implemented so the git history shows red → green on the trait
//! surface. Once the trait lands, this file should compile and pass
//! without modification.

mod fixtures;

use fixtures::mock_unit::{MOCK_CAPABILITIES, MockUnit};
use kernel_core::iommu::contract::{
    DomainError, FaultRecord, IommuUnit, Iova, MapFlags, PhysAddr,
};

fn noop_fault_handler(_record: &FaultRecord) {}

#[test]
fn bring_up_create_map_unmap_destroy_round_trip() {
    let mut unit = MockUnit::new(0);

    unit.bring_up().expect("bring_up should succeed");

    let domain = unit.create_domain().expect("first domain");
    let domain_id = domain.id();

    let iova = Iova(0x1000);
    let phys = PhysAddr(0x8000_0000);
    let flags = MapFlags::READ | MapFlags::WRITE;

    unit.map(domain_id, iova, phys, 0x1000, flags)
        .expect("map should succeed");
    assert_eq!(unit.lookup_phys(domain_id, iova), Some(phys));

    unit.flush(domain_id).expect("flush should succeed");
    assert_eq!(unit.flush_count(), 1);

    unit.unmap(domain_id, iova, 0x1000)
        .expect("unmap should succeed");
    assert_eq!(unit.lookup_phys(domain_id, iova), None);

    unit.install_fault_handler(noop_fault_handler)
        .expect("install_fault_handler should succeed");
    assert!(unit.has_fault_handler());

    let caps = unit.capabilities();
    assert_eq!(caps.address_width_bits, MOCK_CAPABILITIES.address_width_bits);

    unit.destroy_domain(domain).expect("destroy should succeed");
}

#[test]
fn double_unmap_returns_not_mapped() {
    let mut unit = MockUnit::new(0);
    unit.bring_up().unwrap();
    let domain = unit.create_domain().unwrap();
    let id = domain.id();

    unit.map(id, Iova(0x2000), PhysAddr(0x4000), 0x1000, MapFlags::READ)
        .unwrap();
    unit.unmap(id, Iova(0x2000), 0x1000).unwrap();
    assert_eq!(
        unit.unmap(id, Iova(0x2000), 0x1000),
        Err(DomainError::NotMapped)
    );

    unit.destroy_domain(domain).unwrap();
}

#[test]
fn map_over_existing_returns_already_mapped() {
    let mut unit = MockUnit::new(0);
    unit.bring_up().unwrap();
    let domain = unit.create_domain().unwrap();
    let id = domain.id();

    unit.map(id, Iova(0x3000), PhysAddr(0x5000), 0x1000, MapFlags::READ)
        .unwrap();
    assert_eq!(
        unit.map(id, Iova(0x3000), PhysAddr(0x6000), 0x1000, MapFlags::WRITE),
        Err(DomainError::AlreadyMapped)
    );

    unit.destroy_domain(domain).unwrap();
}

#[test]
fn create_domain_hands_out_distinct_ids() {
    let mut unit = MockUnit::new(0);
    unit.bring_up().unwrap();
    let a = unit.create_domain().unwrap();
    let b = unit.create_domain().unwrap();
    let c = unit.create_domain().unwrap();
    assert_ne!(a.id(), b.id());
    assert_ne!(b.id(), c.id());
    assert_ne!(a.id(), c.id());
    unit.destroy_domain(a).unwrap();
    unit.destroy_domain(b).unwrap();
    unit.destroy_domain(c).unwrap();
}
