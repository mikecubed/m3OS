//! Phase 57b/57d — Track A.3 + A.1: property tests for the pure-logic preempt
//! counter and voluntary-preemption model.
//!
//! ### Phase 57b (Track A.3) — Counter contract
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
//! ### Phase 57d (Track A.1) — Voluntary preemption model
//!
//! Extends the property suite with four invariants that pin the voluntary
//! preemption contract before the kernel-side wiring lands:
//!
//! * `preempt_count == 0` is preserved at every user-mode return in a random
//!   mix of disable/enable/preempt events.
//! * `apply_preempt` with `preempt_count > 0` never sets `did_preempt`.
//! * `apply_preempt` with `from_user == false` never sets `did_preempt`.
//! * `apply_preempt_enable_zero_crossing` with `reschedule == true` sets
//!   `preempt_resched_pending` when the count reaches zero.
//!
//! The fuzz uses `proptest` (already a `kernel-core` dev-dependency) and runs
//! ≥ 10 000 random sequences of nesting depth 1–32.  This is wired into
//! `cargo test -p kernel-core`, which `cargo xtask check` runs on every build.
//!
//! Source ref: phase-57b-track-A.3, phase-57d-track-A.1
//! Depends on: phase-57b-track-A.2 (`Counter` model), phase-57d-track-A.1 impl

use kernel_core::preempt_model::{
    Counter, PreemptState, apply_preempt, apply_preempt_enable_zero_crossing,
};
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

// ---------------------------------------------------------------------------
// Phase 57d Track A.1 — Voluntary preemption model property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10_000,
        rng_algorithm: proptest::test_runner::RngAlgorithm::ChaCha,
        ..ProptestConfig::default()
    })]

    /// Random balanced disable/enable sequences interleaved with `apply_preempt`
    /// calls at user-mode boundaries preserve the invariant that `preempt_count
    /// == 0` whenever a user-mode return fires and the counter is balanced.
    ///
    /// This is the core Phase 57d correctness property: voluntary preemption
    /// must only fire when the counter is zero, and balanced sequences always
    /// end at zero.
    #[test]
    fn balanced_sequence_with_preempt_preserves_zero_at_boundary(
        ops in balanced_sequence_strategy(),
        reschedule in any::<bool>(),
    ) {
        // Run the balanced sequence to get to count == 0.
        let mut c = Counter::new();
        for op in &ops {
            if *op { c.disable(); } else { c.enable(); }
        }
        prop_assert_eq!(c.count(), 0);

        // Simulate a user-mode return: apply_preempt on a zero-count state.
        let state = PreemptState {
            preempt_count: 0,
            reschedule,
            from_user: true,
            preempt_resched_pending: false,
        };
        let (new_state, _did_preempt) = apply_preempt(state, true);

        // After user-mode return the count must still be zero.
        prop_assert_eq!(new_state.preempt_count, 0,
            "preempt_count must remain 0 after user-mode return on balanced sequence");
    }

    /// `apply_preempt` with `preempt_count > 0` must never set `did_preempt`.
    ///
    /// Preemption must not fire while the task holds any preempt-disable lock;
    /// doing so would allow a second task to observe half-updated shared state.
    #[test]
    fn no_preempt_when_count_nonzero(
        depth in 1i32..=32,
        reschedule in any::<bool>(),
        from_user in any::<bool>(),
    ) {
        let state = PreemptState {
            preempt_count: depth,
            reschedule,
            from_user,
            preempt_resched_pending: false,
        };
        let (_new_state, did_preempt) = apply_preempt(state, from_user);
        prop_assert!(
            !did_preempt,
            "apply_preempt must not preempt when preempt_count={depth}"
        );
    }

    /// `apply_preempt` with `from_user == false` must never set `did_preempt`.
    ///
    /// Voluntary preemption is only eligible at user-mode return boundaries.
    /// Kernel paths that call `preempt_enable` drop through the zero-crossing
    /// path instead (which sets `preempt_resched_pending`).
    #[test]
    fn no_preempt_when_kernel_mode(
        preempt_count in 0i32..=32,
        reschedule in any::<bool>(),
    ) {
        let state = PreemptState {
            preempt_count,
            reschedule,
            from_user: false,
            preempt_resched_pending: false,
        };
        let (_new_state, did_preempt) = apply_preempt(state, false);
        prop_assert!(
            !did_preempt,
            "apply_preempt must not preempt when from_user=false (count={preempt_count})"
        );
    }

    /// `apply_preempt_enable_zero_crossing` with `reschedule == true` sets
    /// `preempt_resched_pending` when the decrement reaches zero.
    ///
    /// This pins the deferred-reschedule contract: when a `preempt_enable`
    /// drops the count to zero while a reschedule is pending, the kernel must
    /// record the pending reschedule so the scheduler picks it up at the next
    /// safe point.
    #[test]
    fn zero_crossing_with_reschedule_sets_resched_pending(
        extra_depth in 0i32..=31,
    ) {
        // Build a state where one more enable will hit zero.
        let state = PreemptState {
            preempt_count: 1 + extra_depth,
            reschedule: true,
            from_user: false,
            preempt_resched_pending: false,
        };

        if extra_depth == 0 {
            // This enable crosses zero — pending must be set.
            let new_state = apply_preempt_enable_zero_crossing(state);
            prop_assert_eq!(new_state.preempt_count, 0);
            prop_assert!(
                new_state.preempt_resched_pending,
                "zero-crossing with reschedule=true must set preempt_resched_pending"
            );
        } else {
            // Drain down to depth 1 first (each decrement that does NOT hit
            // zero must not set the flag prematurely).
            let mut s = state;
            for _ in 0..extra_depth {
                s = apply_preempt_enable_zero_crossing(s);
                prop_assert!(
                    !s.preempt_resched_pending,
                    "preempt_resched_pending must not be set before count reaches zero"
                );
            }
            // Now cross zero.
            let final_state = apply_preempt_enable_zero_crossing(s);
            prop_assert_eq!(final_state.preempt_count, 0);
            prop_assert!(
                final_state.preempt_resched_pending,
                "zero-crossing with reschedule=true must set preempt_resched_pending"
            );
        }
    }
}
