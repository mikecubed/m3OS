//! Phase 57b — Track A.3: property tests for the pure-logic preempt counter
//! model.
//!
//! These tests pin the contract of [`kernel_core::preempt_model::Counter`]
//! before any kernel-side wiring lands.  The counter must:
//!
//! * Return to 0 after every balanced sequence of `disable` / `enable` calls.
//! * Report `count() > 0` while any unmatched `disable` is pending.
//! * Never observe a negative count (asserted by reading `count()` on every
//!   step — `enable` cannot run before its paired `disable`, so a balanced
//!   walk never goes below zero).
//!
//! The fuzz uses `proptest` (already a `kernel-core` dev-dependency) and runs
//! ≥ 10 000 random sequences of nesting depth 1–32.  This is wired into
//! `cargo test -p kernel-core`, which `cargo xtask check` runs on every build.
//!
//! Source ref: phase-57b-track-A.3
//! Depends on: phase-57b-track-A.2 (`Counter` model)

use kernel_core::preempt_model::Counter;
use proptest::prelude::*;

/// Generate a random *balanced* sequence of `disable` / `enable` operations
/// of nesting depth `1..=32`.
///
/// The strategy emits a flat `Vec<bool>` where `true` means `disable` and
/// `false` means `enable`.  Construction guarantees the sequence is balanced
/// (every prefix has at least as many `disable`s as `enable`s, and the total
/// is balanced) by walking depth up/down within the `1..=32` band.
fn balanced_sequence_strategy() -> impl Strategy<Value = Vec<bool>> {
    // The op count must be even (every disable is paired).  Choose a depth
    // budget in 1..=32 and a length budget that fits within it.
    (1usize..=32, 1usize..=64).prop_flat_map(|(max_depth, half_len)| {
        // half_len pairs => 2 * half_len operations.
        let ops = 2 * half_len;
        prop::collection::vec(any::<bool>(), ops).prop_map(move |bits| {
            // Walk through `bits`: when `true`, emit a disable iff current
            // depth < max_depth, else emit an enable.  When `false`, emit
            // an enable iff current depth > 0, else emit a disable.  At
            // the end, drain any residual depth with enables.
            let mut out = Vec::with_capacity(ops + max_depth);
            let mut depth: usize = 0;
            for b in bits {
                if b {
                    if depth < max_depth {
                        out.push(true);
                        depth += 1;
                    } else {
                        out.push(false);
                        depth -= 1;
                    }
                } else if depth > 0 {
                    out.push(false);
                    depth -= 1;
                } else {
                    out.push(true);
                    depth += 1;
                }
            }
            // Drain residual depth so the sequence is balanced.
            while depth > 0 {
                out.push(false);
                depth -= 1;
            }
            out
        })
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Track A.3 acceptance: ≥ 10 000 random sequences.
        cases: 10_000,
        // Deterministic for CI reproducibility — same seed every run.
        rng_algorithm: proptest::test_runner::RngAlgorithm::ChaCha,
        ..ProptestConfig::default()
    })]

    /// A balanced sequence ends at `count() == 0`, never goes negative
    /// mid-sequence, and reports `count() > 0` whenever any `disable` is
    /// outstanding.
    #[test]
    fn balanced_sequence_ends_at_zero_and_never_negative(
        ops in balanced_sequence_strategy(),
    ) {
        let mut c = Counter::new();
        let mut outstanding: i32 = 0;
        for op in &ops {
            if *op {
                c.disable();
                outstanding += 1;
            } else {
                // We only emit an enable when `outstanding > 0`, so the
                // counter never goes negative.
                prop_assert!(outstanding > 0,
                    "test bug: enable emitted with no outstanding disable");
                c.enable();
                outstanding -= 1;
            }
            // After every step, the counter must equal `outstanding`.
            prop_assert_eq!(c.count(), outstanding,
                "Counter::count() drifted from outstanding-disable count");
            // And must never be negative.
            prop_assert!(c.count() >= 0,
                "Counter::count() went negative");
            // While any disable is pending, count must be > 0.
            if outstanding > 0 {
                prop_assert!(c.count() > 0,
                    "Counter::count() == 0 with {} disables outstanding",
                    outstanding);
            }
        }
        // End-of-sequence: exactly zero and balanced.
        prop_assert_eq!(c.count(), 0,
            "Counter::count() != 0 at end of balanced sequence");
        // assert_balanced must not panic.
        c.assert_balanced();
    }

    /// Nested `disable` calls increment monotonically up to the chosen
    /// depth; the matching `enable` calls return the counter to 0.
    #[test]
    fn nested_disables_increment_monotonically(
        depth in 1usize..=32,
    ) {
        let mut c = Counter::new();
        for i in 0..depth {
            prop_assert_eq!(c.count(), i as i32);
            c.disable();
            prop_assert_eq!(c.count(), (i + 1) as i32);
        }
        for i in 0..depth {
            prop_assert_eq!(c.count(), (depth - i) as i32);
            c.enable();
            prop_assert_eq!(c.count(), (depth - i - 1) as i32);
        }
        prop_assert_eq!(c.count(), 0);
        c.assert_balanced();
    }
}

/// Direct deterministic regression: a freshly-constructed counter starts at
/// zero and `assert_balanced` does not panic.
#[test]
fn fresh_counter_is_balanced() {
    let c = Counter::new();
    assert_eq!(c.count(), 0);
    c.assert_balanced();
}

/// Direct deterministic regression: `assert_balanced` panics when the
/// counter is non-zero.  Kept as a `should_panic` rather than a property
/// test so the panic message is part of the regression contract.
#[test]
#[should_panic(expected = "preempt_count")]
fn assert_balanced_panics_on_nonzero() {
    let mut c = Counter::new();
    c.disable();
    c.assert_balanced();
}
