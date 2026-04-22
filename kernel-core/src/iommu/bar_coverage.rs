//! BAR identity-coverage invariant — Phase 55c Track C.
//!
//! Checks that every PCI Base Address Register (BAR) belonging to a claimed
//! device is identity-mapped in the device's IOMMU domain. This is pure
//! logic — no MMIO, no VT-d / AMD-Vi wiring. Kernel-side wiring that calls
//! this at `sys_device_claim` time lives in Track D.
//!
//! # Model
//!
//! [`BarCoverage`] is a set of physical address ranges that have been
//! identity-mapped. [`BarCoverage::record_mapped`] adds a range;
//! [`assert_bar_identity_mapped`] verifies that every non-zero-length BAR is
//! fully contained within the coverage.
//!
//! Because the backing set merges overlapping and touching mapped ranges, a
//! BAR is "covered" iff there exists a single merged region that starts at or
//! before the BAR's base and ends at or after the BAR's end. This makes the
//! check O(log N) in the number of mapped regions.
//!
//! # Error richness
//!
//! [`BarCoverageError`] carries the BAR index and physical range so that
//! Track D's `iommu.missing_bar_coverage` log event can include structured
//! BDF + BAR metadata without a secondary lookup.
//!
//! # Pure-logic scope
//!
//! This module is host-testable via `cargo test -p kernel-core` like every
//! other module under `kernel_core::iommu::`. It depends only on
//! [`super::regions::ReservedRegionSet`] for its interval-merge algebra.

use super::regions::ReservedRegionSet;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single PCI Base Address Register descriptor.
///
/// BAR indices 0–5 are standard MMIO/IO-port BARs; the ROM BAR uses index 6
/// by convention in some firmwares. Callers may pass any value — the
/// invariant check does not depend on a specific range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    /// BAR index (0–5 for standard BARs; 6 for ROM by convention).
    pub index: u8,
    /// Physical base address of the MMIO window.
    pub base: u64,
    /// Length of the MMIO window in bytes. Zero means the BAR is vestigial;
    /// [`assert_bar_identity_mapped`] skips zero-length BARs silently.
    pub len: usize,
}

/// Error produced by [`assert_bar_identity_mapped`] when a BAR's physical
/// range is not fully covered by the device's identity map.
///
/// Carries enough structured data for Track D's `iommu.missing_bar_coverage`
/// log event to record BDF + BAR metadata without a secondary lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarCoverageError {
    /// Index of the first BAR that is not fully identity-mapped (0–5 typical).
    pub bar_index: u8,
    /// Physical base address of the uncovered BAR.
    pub phys_base: u64,
    /// Length in bytes of the uncovered BAR.
    pub len: usize,
}

/// Set of physical address ranges that have been identity-mapped in a
/// device's IOMMU domain.
///
/// Backed by a [`ReservedRegionSet`] so overlapping and touching mapped ranges
/// merge automatically. Coverage checks are O(log N) in the number of
/// distinct merged regions.
#[derive(Clone, Debug, Default)]
pub struct BarCoverage {
    mapped: ReservedRegionSet,
}

impl BarCoverage {
    /// Create an empty coverage set — no ranges identity-mapped yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the physical range `[phys, phys + len)` has been
    /// identity-mapped.
    ///
    /// Zero-length records are silently ignored. Overlapping and touching
    /// records are merged into a single contiguous span.
    pub fn record_mapped(&mut self, _phys: u64, _len: usize) {
        unimplemented!("C.1 implementation pending — tests committed first (TDD red)")
    }

    /// Return `true` if the range `[base, base + len)` is fully covered.
    ///
    /// A zero-length range is always considered covered (vestigial BARs
    /// require no identity mapping). A non-zero range is covered iff a single
    /// merged region exists whose `start <= base` and whose `end >= base + len`.
    pub fn covers(&self, _base: u64, _len: usize) -> bool {
        unimplemented!("C.1 implementation pending — tests committed first (TDD red)")
    }
}

// ---------------------------------------------------------------------------
// Public function
// ---------------------------------------------------------------------------

/// Assert that every non-zero-length BAR in `bars` is fully identity-mapped
/// within `coverage`.
///
/// Returns `Ok(())` when every BAR passes the check. Returns the first
/// [`BarCoverageError`] encountered on failure, carrying the BAR index and
/// physical range for structured `iommu.missing_bar_coverage` logging in
/// Track D.
///
/// Zero-length BARs are skipped — they are vestigial and contribute nothing
/// to the MMIO window that the IOMMU must protect.
pub fn assert_bar_identity_mapped(
    _bars: &[Bar],
    _coverage: &BarCoverage,
) -> Result<(), BarCoverageError> {
    unimplemented!("C.1 implementation pending — tests committed first (TDD red)")
}

// ---------------------------------------------------------------------------
// Unit tests (C.1 acceptance criteria — committed red before implementation)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// C.1 — single_bar_identity_maps
    /// A single BAR whose physical range is exactly recorded in the coverage
    /// must pass the assertion.
    #[test]
    fn single_bar_identity_maps() {
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(0x1_0000, 0x4000);
        let bars = [Bar { index: 0, base: 0x1_0000, len: 0x4000 }];
        assert!(assert_bar_identity_mapped(&bars, &coverage).is_ok());
    }

    /// C.1 — multi_bar_identity_maps
    /// Multiple BARs, each individually identity-mapped, all pass.
    #[test]
    fn multi_bar_identity_maps() {
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(0x8000_0000, 0x10000);
        coverage.record_mapped(0x9000_0000, 0x4000);
        let bars = [
            Bar { index: 0, base: 0x8000_0000, len: 0x10000 },
            Bar { index: 2, base: 0x9000_0000, len: 0x4000 },
        ];
        assert!(assert_bar_identity_mapped(&bars, &coverage).is_ok());
    }

    /// C.1 — missing_bar_fails_assertion_with_typed_error
    /// When a BAR's range is only partially covered, the returned error must
    /// carry the BAR index and the full BAR range (not the covered portion).
    #[test]
    fn missing_bar_fails_assertion_with_typed_error() {
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(0x1_0000, 0x2000); // maps only the first half
        let bars = [Bar { index: 3, base: 0x1_0000, len: 0x4000 }];
        let err = assert_bar_identity_mapped(&bars, &coverage)
            .expect_err("partial coverage must fail");
        assert_eq!(err.bar_index, 3);
        assert_eq!(err.phys_base, 0x1_0000);
        assert_eq!(err.len, 0x4000);
    }

    /// C.1 — zero_length_bar_is_noop
    /// A zero-length (vestigial) BAR must never cause a coverage failure even
    /// when the coverage set is empty.
    #[test]
    fn zero_length_bar_is_noop() {
        let coverage = BarCoverage::new();
        let bars = [Bar { index: 1, base: 0xDEAD_BEEF, len: 0 }];
        assert!(assert_bar_identity_mapped(&bars, &coverage).is_ok());
    }

    /// C.1 — bar_overlap_detected
    /// Two BARs that share a physical page (overlapping MMIO windows).
    /// The identity map covers the union; the assertion must pass for both.
    #[test]
    fn bar_overlap_detected() {
        // BAR 0: [0x4000, 0x8000)
        // BAR 1: [0x6000, 0xA000) — overlaps BAR 0's upper half
        // Identity map covers the union [0x4000, 0xA000).
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(0x4000, 0x6000);
        let bars = [
            Bar { index: 0, base: 0x4000, len: 0x4000 },
            Bar { index: 1, base: 0x6000, len: 0x4000 },
        ];
        assert!(assert_bar_identity_mapped(&bars, &coverage).is_ok());
    }

    // Two additional unit tests beyond the five required by C.1 -----------

    /// Entirely unmapped BAR — empty coverage set — must always fail.
    #[test]
    fn entirely_unmapped_bar_fails() {
        let coverage = BarCoverage::new();
        let bars = [Bar { index: 2, base: 0xC000_0000, len: 0x1000 }];
        let err = assert_bar_identity_mapped(&bars, &coverage)
            .expect_err("unmapped BAR must fail");
        assert_eq!(err.bar_index, 2);
    }

    /// A BAR partially mapped starting mid-range (base not covered) fails.
    #[test]
    fn bar_partially_mapped_at_end_fails() {
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(0x2000, 0x1000); // maps [0x2000, 0x3000)
        // BAR needs [0x1000, 0x3000) — 0x1000..0x2000 is uncovered.
        let bars = [Bar { index: 0, base: 0x1000, len: 0x2000 }];
        let err = assert_bar_identity_mapped(&bars, &coverage)
            .expect_err("partial coverage at start must fail");
        assert_eq!(err.bar_index, 0);
    }

    /// `record_mapped(_, 0)` is a no-op; subsequent coverage check fails.
    #[test]
    fn record_mapped_zero_length_is_noop() {
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(0x1000, 0); // no-op
        let bars = [Bar { index: 0, base: 0x1000, len: 0x1000 }];
        assert!(assert_bar_identity_mapped(&bars, &coverage).is_err());
    }
}
