//! Phase 57b — Track A.2: pure-logic preempt counter model.
//!
//! This module pins the **contract** of the per-task `preempt_count` that
//! Phase 57b's kernel-side `preempt_disable` / `preempt_enable` will mutate
//! through a per-CPU pointer.  It lives in `kernel-core` so it is host-
//! testable (`cargo test -p kernel-core --target x86_64-unknown-linux-gnu`)
//! and so the property fuzz in `tests/preempt_property.rs` can exercise it
//! without touching the kernel.
//!
//! ### Why a model?
//!
//! Track A's deliverable is the *contract*, not the kernel wiring.  By
//! landing a pure-logic counter first (and a property fuzz that pins its
//! contract on the host) we get the SOLID one-purpose type, the DRY single
//! definition, and the TDD red/green discipline before any kernel-side
//! atomics or per-CPU pointer plumbing exists.
//!
//! ### Invariants the contract pins
//!
//! 1. A freshly constructed [`Counter`] has `count() == 0`.
//! 2. [`Counter::disable`] increments by exactly 1.
//! 3. [`Counter::enable`] decrements by exactly 1.
//! 4. `count()` is monotonic between paired calls — no half-increments,
//!    no skipped steps, no atomic reordering visible at the model layer
//!    (the kernel-side counter is `AtomicI32`; the model uses `i32`
//!    because tests are single-threaded and the model is exercised
//!    sequentially per-task).
//! 5. [`Counter::assert_balanced`] is the user-mode-return invariant: it
//!    panics if `count() != 0`.  Phase 57b Track D.3 wires this assertion
//!    into the syscall-return and IRQ-return-to-ring-3 paths.
//!
//! ### What the model does NOT cover
//!
//! - Atomic ordering between cores — kernel uses `AtomicI32::fetch_add`
//!   with `Acquire` / `Release`; the model is single-threaded.
//! - The boot dummy (`SCHED_PREEMPT_COUNT_DUMMY`) — Track C.1.
//!
//! Source ref: phase-57b-track-A.2, phase-57d-track-A.1

/// Pure-logic preempt counter.
///
/// Wraps an `i32` and exposes `disable` / `enable` / `count` /
/// `assert_balanced`.  The single-purpose, SOLID-clean view of the
/// kernel's per-task `preempt_count` field.  Track D.1 lands a parallel
/// `AtomicI32` field on `Task`; the contract this type pins is what the
/// kernel-side counter must behave like.
#[derive(Debug, Default, Clone)]
pub struct Counter {
    /// The current preempt-disable depth.  Negative values are a bug —
    /// see the doc-comment on [`Counter::enable`].
    value: i32,
}

impl Counter {
    /// Construct a fresh counter at depth 0.
    ///
    /// Invariant: `count() == 0` immediately after construction, so a
    /// new task starts in the preemptible state.  Track C.1's per-CPU
    /// `current_preempt_count_ptr` initialises to a per-core dummy
    /// `Counter` whose value is also 0 at boot.
    pub const fn new() -> Self {
        Self { value: 0 }
    }

    /// Increment the counter — enter a non-preemptible region.
    ///
    /// Invariant: every `disable` must be paired with exactly one
    /// matching `enable`.  Phase 57b Track F wires this into
    /// `IrqSafeMutex::lock`; Phase 57b Track G wires it into the per-
    /// callsite migrations enumerated in
    /// `docs/handoffs/57b-spinlock-callsite-audit.md`.
    ///
    /// Ordering: in the kernel, the matching atomic is
    /// `fetch_add(1, Acquire)` — `Acquire` because the lock acquisition
    /// that pairs with this counter raise must happen-before the
    /// critical section.  At the model layer there is no concurrency, so
    /// the increment is a plain `+= 1`.
    pub fn disable(&mut self) {
        self.value += 1;
    }

    /// Decrement the counter — leave a non-preemptible region.
    ///
    /// Invariant: callers must guarantee a matching prior `disable`.
    /// The model does not panic on underflow (the kernel-side
    /// `AtomicI32::fetch_sub` would not panic either) — instead, the
    /// tests in `kernel-core/tests/preempt_property.rs` only emit an
    /// `enable` after at least one outstanding `disable`, and the
    /// property assertion catches any negative `count()`.  Phase 57d's
    /// deferred-reschedule (see `docs/roadmap/57d-voluntary-preemption.md`)
    /// is the runtime check that catches an unmatched enable in the
    /// kernel.
    ///
    /// Ordering: in the kernel, the matching atomic is
    /// `fetch_sub(1, Release)` — `Release` because the critical section
    /// must happen-before the lock release that pairs with this counter
    /// drop.
    pub fn enable(&mut self) {
        self.value -= 1;
    }

    /// Read the current depth.
    ///
    /// Invariant: at every user-mode return boundary (Track D.3), the
    /// value must be 0.  Returns `i32` (signed) so a debugger can spot
    /// underflow as a negative number rather than silently wrapping.
    ///
    /// Ordering: kernel-side reads use `AtomicI32::load(Relaxed)` for
    /// the user-mode-return debug-assertion path; readers that branch
    /// on the value (Phase 57d's preemption-eligibility check) use
    /// `load(Acquire)` to pair with the matching `Release` in `enable`.
    pub fn count(&self) -> i32 {
        self.value
    }

    /// Panic if the counter is non-zero.
    ///
    /// This is the user-mode-return invariant: a task that returns to
    /// ring 3 with `preempt_count != 0` has a forgotten
    /// `preempt_enable` somewhere in its kernel path, and Phase 57d
    /// would deadlock the moment preemption fires inside a held lock.
    /// Catching the bug at the boundary is the cheapest detection
    /// available.  Track D.3 wires `debug_assert!` calls into the
    /// syscall-return and IRQ-return-to-ring-3 paths against the same
    /// invariant.
    ///
    /// The panic message includes the literal substring `preempt_count`
    /// so the property test in `kernel-core/tests/preempt_property.rs`
    /// can `#[should_panic(expected = "preempt_count")]` against it.
    pub fn assert_balanced(&self) {
        assert!(
            self.value == 0,
            "preempt_count != 0 at user-mode return boundary: {}",
            self.value
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 57d Track A.1 — Voluntary preemption model
// ---------------------------------------------------------------------------

/// Events that can drive the voluntary-preemption model in property tests.
///
/// Used by `kernel-core/tests/preempt_property.rs` to generate random event
/// sequences and verify the four core invariants.  Not wired into the kernel —
/// the kernel uses concrete function calls (`preempt_disable` /
/// `preempt_enable` / the zero-crossing hook at `preempt_enable`'s tail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Enter a non-preemptible region (`preempt_disable`).
    Disable,
    /// Leave a non-preemptible region (`preempt_enable`).
    Enable,
    /// A user-mode-return boundary: attempt voluntary preemption.
    Preempt { from_user: bool },
    /// `preempt_enable` dropped the count to zero with a pending reschedule.
    PreemptEnableZeroCrossing,
}

/// Pure-logic snapshot of per-task preempt state for property testing.
///
/// Mirrors the kernel fields touched by the voluntary-preemption path so
/// property tests can verify invariants without a live kernel.  All fields
/// are `pub` so proptest strategies can construct arbitrary states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreemptState {
    /// Current preempt-disable nesting depth.  Must be ≥ 0; negative values
    /// indicate a bug (unmatched `preempt_enable`).
    pub preempt_count: i32,
    /// Set by the scheduler when a higher-priority task becomes runnable.
    /// Cleared when the task is actually preempted or rescheduled.
    pub reschedule: bool,
    /// Snapshot of the CPU mode at the point of the preemption check.
    pub from_user: bool,
    /// Set by the zero-crossing hook in `preempt_enable` when `reschedule`
    /// is true at the moment `preempt_count` drops to zero.  The scheduler
    /// loop polls this flag and yields at the next safe point.
    pub preempt_resched_pending: bool,
}

/// Simulate a user-mode-return preemption check.
///
/// Returns `(new_state, did_preempt)`.
///
/// Preemption fires — `did_preempt == true` — iff **all** of:
/// * `from_user` is `true` (we are returning to ring 3), and
/// * `state.preempt_count == 0` (no preempt-disable lock is held), and
/// * `state.reschedule || state.preempt_resched_pending` (the scheduler
///   signalled that a context switch is needed).
///
/// When `did_preempt` is `true` both `reschedule` and `preempt_resched_pending`
/// are cleared in the returned state (the kernel performs the switch).
/// Otherwise the state is returned unchanged.
pub fn apply_preempt(state: PreemptState, from_user: bool) -> (PreemptState, bool) {
    let eligible = from_user
        && state.preempt_count == 0
        && (state.reschedule || state.preempt_resched_pending);

    if eligible {
        let new_state = PreemptState {
            reschedule: false,
            preempt_resched_pending: false,
            ..state
        };
        (new_state, true)
    } else {
        (state, false)
    }
}

/// Simulate the zero-crossing hook inside `preempt_enable`.
///
/// Decrements `preempt_count` by 1.  If the post-decrement count is zero and
/// `reschedule` is `true`, sets `preempt_resched_pending` so the scheduler
/// loop picks it up at the next safe yield point.
///
/// The returned state has `preempt_count` decremented; all other fields are
/// unchanged except `preempt_resched_pending` which may be set.
pub fn apply_preempt_enable_zero_crossing(state: PreemptState) -> PreemptState {
    let new_count = state.preempt_count - 1;
    let pending = if new_count == 0 && state.reschedule {
        true
    } else {
        state.preempt_resched_pending
    };
    PreemptState {
        preempt_count: new_count,
        preempt_resched_pending: pending,
        ..state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_counter_is_zero() {
        let c = Counter::new();
        assert_eq!(c.count(), 0);
    }

    #[test]
    fn disable_increments_and_enable_decrements() {
        let mut c = Counter::new();
        c.disable();
        assert_eq!(c.count(), 1);
        c.disable();
        assert_eq!(c.count(), 2);
        c.enable();
        assert_eq!(c.count(), 1);
        c.enable();
        assert_eq!(c.count(), 0);
    }

    #[test]
    fn assert_balanced_passes_at_zero() {
        let c = Counter::new();
        c.assert_balanced();
    }

    #[test]
    #[should_panic(expected = "preempt_count")]
    fn assert_balanced_panics_when_outstanding() {
        let mut c = Counter::new();
        c.disable();
        c.assert_balanced();
    }

    #[test]
    fn nesting_to_max_depth_round_trips_to_zero() {
        let mut c = Counter::new();
        for _ in 0..32 {
            c.disable();
        }
        assert_eq!(c.count(), 32);
        for _ in 0..32 {
            c.enable();
        }
        assert_eq!(c.count(), 0);
        c.assert_balanced();
    }
}
