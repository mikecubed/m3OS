//! Property tests for `BarCoverage` — Phase 55c Track C.2.
//!
//! Verifies the BAR identity-coverage invariant holds across arbitrary BAR
//! layouts. Configured with at least 1 024 cases.
//!
//! # Why a separate file
//!
//! Property tests are deliberately separated from the unit tests in
//! `bar_coverage.rs` to keep the acceptance-criteria tests readable and to
//! allow the proptest configuration knob (`cases`) to be set in one place.

use proptest::prelude::*;

use super::bar_coverage::{assert_bar_identity_mapped, Bar, BarCoverage};

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Arbitrary BAR with non-zero length, bounded so property runs stay fast.
/// base: 0..2^32 (32-bit MMIO window), len: 1..=0x1_0000, index: 0..6.
fn bar_strategy() -> impl Strategy<Value = Bar> {
    (0u64..(1u64 << 32), 1usize..=0x1_0000usize, 0u8..6u8).prop_map(|(base, len, index)| Bar {
        index,
        base,
        len,
    })
}

// ---------------------------------------------------------------------------
// Property tests (C.2 acceptance criteria)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 1024, ..Default::default() })]

    /// For any BAR, recording its exact physical range then checking identity
    /// coverage must succeed.
    #[test]
    fn single_bar_exact_coverage_succeeds(bar in bar_strategy()) {
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(bar.base, bar.len);
        let result = assert_bar_identity_mapped(&[bar], &coverage);
        prop_assert!(result.is_ok(), "exact coverage must pass for {:?}", bar);
    }

    /// Recording a strictly larger superset also covers the BAR.
    #[test]
    fn superset_coverage_succeeds(bar in bar_strategy()) {
        let mut coverage = BarCoverage::new();
        // Map from (base - 0x1000) for (len + 0x2000) — guaranteed superset
        // even when base < 0x1000 (saturating_sub clamps to 0).
        let ext_base = bar.base.saturating_sub(0x1000);
        let ext_len = bar.len.saturating_add(0x2000);
        coverage.record_mapped(ext_base, ext_len);
        let result = assert_bar_identity_mapped(&[bar], &coverage);
        prop_assert!(result.is_ok(), "superset coverage must pass for {:?}", bar);
    }

    /// An entirely unmapped BAR (empty coverage) always fails.
    #[test]
    fn empty_coverage_always_fails(bar in bar_strategy()) {
        // bar_strategy guarantees len >= 1.
        let coverage = BarCoverage::new();
        let result = assert_bar_identity_mapped(&[bar], &coverage);
        prop_assert!(result.is_err(), "unmapped BAR must fail for {:?}", bar);
    }

    /// For overlapping BARs, recording the union of their ranges as mapped
    /// makes the assertion pass for both.
    #[test]
    fn overlapping_bars_union_coverage_succeeds(
        base_a in 0u64..(1u64 << 32),
        len_a in 0x1000usize..=0x1_0000usize,
        // offset < len_a ensures bar_b's base lands inside bar_a's window.
        offset in 0x800usize..=0x8000usize,
        len_b in 0x1000usize..=0x1_0000usize,
    ) {
        let base_b = base_a.saturating_add(offset as u64);
        let end_a = base_a.saturating_add(len_a as u64);
        let end_b = base_b.saturating_add(len_b as u64);
        let union_end = core::cmp::max(end_a, end_b);
        let union_len = union_end.saturating_sub(base_a) as usize;

        let mut coverage = BarCoverage::new();
        coverage.record_mapped(base_a, union_len);

        let bars = [
            Bar { index: 0, base: base_a, len: len_a },
            Bar { index: 1, base: base_b, len: len_b },
        ];
        let result = assert_bar_identity_mapped(&bars, &coverage);
        prop_assert!(result.is_ok(), "union coverage must pass for overlapping BARs");
    }

    /// Multiple independent BARs each individually recorded must all pass.
    #[test]
    fn multiple_independent_bars_pass(
        bars in prop::collection::vec(bar_strategy(), 1..=6),
    ) {
        let mut coverage = BarCoverage::new();
        for bar in &bars {
            coverage.record_mapped(bar.base, bar.len);
        }
        // Each BAR has its range recorded; overlapping BARs' ranges are merged
        // and still cover both BARs.
        let result = assert_bar_identity_mapped(&bars, &coverage);
        prop_assert!(result.is_ok(), "all bars individually mapped must pass");
    }

    /// Zero-length BARs are always skipped regardless of coverage state.
    #[test]
    fn zero_length_bars_always_ok(base in 0u64..(1u64 << 48)) {
        let coverage = BarCoverage::new(); // empty — nothing mapped
        let bar = Bar { index: 5, base, len: 0 };
        let result = assert_bar_identity_mapped(&[bar], &coverage);
        prop_assert!(result.is_ok(), "zero-length BAR must be skipped");
    }

    /// Coverage built from two touching ranges behaves as one contiguous span.
    #[test]
    fn touching_ranges_merge_and_cover_both(
        base in 0u64..(1u64 << 32),
        len_a in 1usize..=0x8000usize,
        len_b in 1usize..=0x8000usize,
    ) {
        let base_b = base.saturating_add(len_a as u64);
        let mut coverage = BarCoverage::new();
        coverage.record_mapped(base, len_a);   // [base, base+len_a)
        coverage.record_mapped(base_b, len_b); // [base+len_a, base+len_a+len_b) — touches

        // A BAR spanning the full union must be covered.
        let total_len = len_a.saturating_add(len_b);
        let bar = Bar { index: 0, base, len: total_len };
        let result = assert_bar_identity_mapped(&[bar], &coverage);
        prop_assert!(result.is_ok(), "touching ranges must cover union BAR");
    }
}
