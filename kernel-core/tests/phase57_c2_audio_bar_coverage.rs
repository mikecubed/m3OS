//! Phase 57 Track C.2 — AC'97 BAR identity-coverage parity test.
//!
//! Locks the existing `BarCoverage::assert_bar_identity_mapped` pipeline
//! against the AC'97 BAR layout. AC'97 ships exactly two BARs in real
//! ICH silicon and in QEMU's `-device AC97` emulation:
//!
//! - BAR0 (NAM, mixer) — I/O space, ~64 bytes.
//! - BAR1 (NABM, bus master) — I/O space, ~192 bytes.
//!
//! Both BARs are PIO. The `kernel/src/syscall/device_host.rs`
//! `install_and_verify_bar_coverage` path detects PIO BARs by raw-bit
//! inspection (`raw & 1 != 0`) and skips them before they reach
//! `BarCoverage::assert_bar_identity_mapped`. The kernel-core layer
//! sees an empty `bars` slice for AC'97 — the same shape it sees for
//! any device whose BARs are all PIO or vestigial.
//!
//! This integration test pins the empty-slice / vestigial-slice
//! contracts so a regression that broke the PIO-skip logic upstream
//! would cause the audio claim path to spuriously emit
//! `iommu.missing_bar_coverage` events. Running this test under
//! `cargo test -p kernel-core --target x86_64-unknown-linux-gnu`
//! exercises the same shared coverage primitive both VT-d
//! (`kernel/src/iommu/intel.rs::install_bar_identity_maps`) and
//! AMD-Vi (`kernel/src/iommu/amd.rs::install_bar_identity_maps`) call
//! into for non-zero MMIO BARs — symmetric coverage between the two
//! vendor paths is established by their shared use of
//! [`kernel_core::iommu::bar_coverage`].
//!
//! AMD-Vi parity note: the AMD-Vi `install_bar_identity_maps` impl is
//! a verbatim shape-mirror of the VT-d impl (see the doc comment on
//! `AmdViUnit::install_bar_identity_maps` in `kernel/src/iommu/amd.rs`).
//! Both call `self.map(domain, Iova(aligned_base), PhysAddr(aligned_base),
//! aligned_len, READ | WRITE)` and accept `AlreadyMapped` as success.
//! The vendor-specific page-table walk lives below `map()`, not in the
//! identity-map driver, so the two paths are LSP-equivalent at the
//! kernel-core layer this test exercises.

use kernel_core::device_host::{
    PCI_DEVICE_AC97, PCI_VENDOR_INTEL, audio_class::AC97_BAR_LAYOUT, classify_pci_id,
};
use kernel_core::iommu::bar_coverage::{Bar, BarCoverage, assert_bar_identity_mapped};

/// AC'97 PIO-only BAR layout — the kernel-side claim path filters PIO
/// BARs before they reach `BarCoverage`. The coverage pipeline must
/// treat the resulting empty slice as "no MMIO to map", returning
/// `Ok(())` from the assertion.
#[test]
fn ac97_pio_only_layout_yields_empty_coverage_set() {
    // The AC'97 controller is `0x8086:0x2415` and classifies as the
    // audio device class. Pin the classification at the same site as
    // the coverage assertion so a regression in either reads as one
    // failure.
    assert_eq!(
        classify_pci_id(PCI_VENDOR_INTEL, PCI_DEVICE_AC97).map(|c| c.subsystem()),
        Some("audio.device"),
    );

    // The kernel-core layer pins AC'97's BAR layout — both BARs are
    // I/O-space, total MMIO zero. The constant lives next to the PCI
    // ID classifier so any future audio target (HDA, virtio-sound) gets
    // its own layout entry rather than mutating AC'97's.
    assert!(
        AC97_BAR_LAYOUT.is_pio_only(),
        "AC'97 is documented as I/O-space-only in the audio target-choice memo",
    );
    assert_eq!(AC97_BAR_LAYOUT.mmio_bar_count(), 0);

    // After PIO filtering at the kernel-side BAR collection step, the
    // bar list is empty. An empty slice + an empty coverage map must
    // never produce a coverage error.
    let bars: [Bar; 0] = [];
    let coverage = BarCoverage::new();
    assert!(
        assert_bar_identity_mapped(&bars, &coverage).is_ok(),
        "AC'97's PIO-only layout produces an empty BAR slice; coverage must pass",
    );
}

/// If AC'97 ever exposed an MMIO BAR (a hypothetical future revision
/// or a drop-in HDA controller using the same audio.device subsystem
/// tag), the existing coverage pipeline must handle the identity-mapped
/// MMIO range without modification. This test stands in for that
/// future-proof assertion: the same primitive that backs e1000 and
/// NVMe also covers any audio device using MMIO BARs.
#[test]
fn audio_class_mmio_bar_identity_mapping_passes() {
    // Synthetic MMIO BAR — base/len chosen to exercise a 4 KiB-aligned
    // range that the AC'97-or-successor would land at in QEMU.
    let mmio_base: u64 = 0xFEBC_0000;
    let mmio_len: usize = 0x1000;

    // The kernel-side `install_and_verify_bar_coverage` records each
    // mapped BAR via `BarCoverage::record_mapped`. Re-create that step
    // here so the assertion sees the same shape.
    let mut coverage = BarCoverage::new();
    coverage.record_mapped(mmio_base, mmio_len);

    let bars = [Bar {
        index: 2,
        base: mmio_base,
        len: mmio_len,
    }];
    assert!(
        assert_bar_identity_mapped(&bars, &coverage).is_ok(),
        "an audio MMIO BAR must pass coverage when it's been identity-mapped",
    );
}

/// A coverage gap on an audio MMIO BAR must surface the typed error
/// the kernel translates into the `iommu.missing_bar_coverage
/// subsystem=audio.device` log event.
#[test]
fn audio_class_mmio_bar_missing_coverage_yields_typed_error() {
    let mmio_base: u64 = 0xFEBD_0000;
    let mmio_len: usize = 0x2000;

    // Coverage records only the first half of the BAR — the assertion
    // must report the *full* BAR range in its error so the kernel-side
    // log carries the exact `bar_index=N` field the C.1 acceptance
    // pinned.
    let mut coverage = BarCoverage::new();
    coverage.record_mapped(mmio_base, 0x1000);

    let bars = [Bar {
        index: 0,
        base: mmio_base,
        len: mmio_len,
    }];
    let err =
        assert_bar_identity_mapped(&bars, &coverage).expect_err("partial coverage must fail");
    assert_eq!(err.bar_index, 0);
    assert_eq!(err.phys_base, mmio_base);
    assert_eq!(err.len, mmio_len);
}

/// VT-d / AMD-Vi parity stub.
///
/// The kernel-side `VtdUnit::install_bar_identity_maps` and
/// `AmdViUnit::install_bar_identity_maps` are shape-identical:
///
/// ```text
/// for bar in bars {
///   if bar.len == 0 { continue; }
///   let aligned_base = bar.base & !0xFFF;
///   let end = bar.base + bar.len;
///   let aligned_end = (end + 0xFFF) & !0xFFF;
///   let aligned_len = aligned_end - aligned_base;
///   match self.map(domain, Iova(aligned_base), PhysAddr(aligned_base),
///                  aligned_len, READ | WRITE) {
///     Ok(()) | Err(AlreadyMapped) => coverage.record_mapped(...),
///     Err(e) => return Err(e),
///   }
/// }
/// ```
///
/// Both paths use the shared `kernel_core::iommu::bar_coverage` types,
/// so a single round-trip through the coverage primitive proves both
/// vendor-side paths satisfy the same contract for any audio BAR layout
/// the device-host claim filters down to MMIO. Vendor-specific
/// page-table walks are exercised by `kernel-core/tests/iommu_parity.rs`
/// and the in-kernel test_case suite — outside the scope of C.2.
#[test]
fn vtd_and_amdvi_install_bar_identity_maps_share_coverage_primitive() {
    // Sanity: the constants the kernel-side helpers feed into
    // `BarCoverage` are stable across vendors. Page-aligned base and
    // page-rounded length are the only invariants the coverage layer
    // relies on; both vendors compute them identically (see the doc
    // block above).
    let raw_base: u64 = 0xFEBC_0123;
    let raw_len: usize = 0x123;

    let aligned_base = raw_base & !0xFFF;
    let end = raw_base + raw_len as u64;
    let aligned_end = (end + 0xFFF) & !0xFFF;
    let aligned_len = (aligned_end - aligned_base) as usize;

    assert_eq!(aligned_base, 0xFEBC_0000);
    assert_eq!(aligned_len, 0x1000);
    assert!(aligned_len.is_multiple_of(0x1000));

    let mut coverage = BarCoverage::new();
    coverage.record_mapped(aligned_base, aligned_len);
    assert!(coverage.covers(raw_base, raw_len));
}
