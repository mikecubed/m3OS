//! Phase 56 Track B.3 — frame-tick metadata and coalescing logic.
//!
//! The kernel emits a periodic notification at [`FrameTickConfig::hz`]. This
//! module owns the *interpretation* of that signal: the requested rate, the
//! per-tick interval in microseconds, and the coalescing rule for bursts of
//! ticks delivered while a waiter was descheduled.

/// Configuration of the frame-tick source. The kernel publishes the runtime
/// values here through a metadata page; userspace also reads this page so it
/// can adapt animation budgets later.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameTickConfig {
    /// Requested frame rate. Default 60.
    pub hz: u32,
}

impl FrameTickConfig {
    /// Default frame rate published by the kernel-side timer (60 Hz).
    pub const DEFAULT_HZ: u32 = 60;
    /// Lowest accepted frame rate. One tick per second is the slowest
    /// meaningful animation cadence; zero is rejected.
    pub const MIN_HZ: u32 = 1;
    /// Highest accepted frame rate. One kHz keeps `period_micros` above
    /// the LAPIC's millisecond-resolution timer scheduling floor.
    pub const MAX_HZ: u32 = 1000;

    /// Returns the canonical 60 Hz configuration used by the kernel-side
    /// timer at boot.
    pub const fn default_60hz() -> Self {
        Self {
            hz: Self::DEFAULT_HZ,
        }
    }

    /// Construct a new configuration, returning `None` if `hz` falls outside
    /// `[MIN_HZ, MAX_HZ]`.
    pub const fn new(hz: u32) -> Option<Self> {
        if hz < Self::MIN_HZ || hz > Self::MAX_HZ {
            None
        } else {
            Some(Self { hz })
        }
    }

    /// Returns the per-tick interval in microseconds. Truncates toward zero
    /// when the division is non-exact (e.g. 60 Hz → 16_666 µs). The metadata
    /// page exposes both this value and `hz` so userspace need not redo the
    /// math.
    pub const fn period_micros(self) -> u32 {
        // `hz` is constrained by `new`/`default_60hz` to be in [1, 1000],
        // so the divisor is always non-zero on a well-formed value. We still
        // guard against a zero divisor defensively without panicking, since
        // the field is `pub` and could be constructed by struct-literal.
        match 1_000_000u32.checked_div(self.hz) {
            Some(v) => v,
            None => 0,
        }
    }

    /// Returns the closest LAPIC-tick period (in milliseconds) that
    /// approximates the requested frame rate. Used by the kernel-side
    /// timer setup; clamped to `[1, 1000]`.
    ///
    /// The math uses ceiling division `(1000 + hz - 1) / hz` so the
    /// scheduled tick is *at most* the requested period — a 60 Hz
    /// configuration rounds to 17 ms (≈58.8 Hz effective) rather than
    /// 16 ms (≈62.5 Hz), keeping us at or below the requested rate.
    pub const fn lapic_period_ms(self) -> u32 {
        if self.hz == 0 {
            return 1000;
        }
        let raw = 1000u32.div_ceil(self.hz);
        if raw < 1 {
            1
        } else if raw > 1000 {
            1000
        } else {
            raw
        }
    }
}

/// Coalesce missed ticks. The kernel-side ISR calls `accumulate(1)` per
/// fired tick; the userspace waiter calls `drain()` after waking. If the
/// waiter slept through several ticks they are coalesced into a single
/// "you missed N ticks; the latest tick index is M" reading — never a
/// queue that grows unboundedly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameTickCounter {
    pending: u32,
    total: u64,
}

impl FrameTickCounter {
    /// Construct a fresh counter with both pending and total at zero.
    pub const fn new() -> Self {
        Self {
            pending: 0,
            total: 0,
        }
    }

    /// Atomic-equivalent in pure-logic-land: increments both pending and
    /// total. `pending` saturates at [`u32::MAX`] so a runaway interrupt
    /// storm cannot wrap the missed-tick reading; `total` saturates at
    /// [`u64::MAX`] so the monotone-index promise survives even pathological
    /// uptime.
    pub fn accumulate(&mut self, ticks: u32) {
        if ticks == 0 {
            return;
        }
        self.pending = self.pending.saturating_add(ticks);
        self.total = self.total.saturating_add(ticks as u64);
    }

    /// Read and clear the pending counter. Returns `(missed_ticks,
    /// total_index)` where `missed_ticks` is the value of `pending` before
    /// the drain and `total_index` is the post-drain (and pre-drain — they
    /// are equal) value of `total`.
    pub fn drain(&mut self) -> (u32, u64) {
        let missed = self.pending;
        self.pending = 0;
        (missed, self.total)
    }

    /// Number of ticks accumulated since the last [`drain`](Self::drain).
    pub const fn pending(self) -> u32 {
        self.pending
    }

    /// Monotone tick index. Equals the total number of ticks observed since
    /// construction, saturating at [`u64::MAX`].
    pub const fn total(self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn default_is_60hz() {
        let cfg = FrameTickConfig::default_60hz();
        assert_eq!(cfg.hz, 60);
        assert_eq!(cfg.period_micros(), 16_666);
        assert_eq!(cfg.period_micros(), 1_000_000 / 60);
    }

    #[test]
    fn period_micros_for_30hz() {
        let cfg = FrameTickConfig::new(30).expect("30 Hz is in range");
        assert_eq!(cfg.period_micros(), 33_333);
    }

    #[test]
    fn period_micros_for_120hz() {
        let cfg = FrameTickConfig::new(120).expect("120 Hz is in range");
        assert_eq!(cfg.period_micros(), 8_333);
    }

    #[test]
    fn lapic_period_ms_default_is_17() {
        let cfg = FrameTickConfig::default_60hz();
        // Ceiling division: (1000 + 60 - 1) / 60 = 1059 / 60 = 17.
        assert_eq!(cfg.lapic_period_ms(), 17);
        // Confirm the rounding contract is exactly ceiling-division — i.e.
        // `(1000 + hz - 1) / hz`, equivalent to `1000.div_ceil(hz)`.
        assert_eq!(cfg.lapic_period_ms(), 1000u32.div_ceil(cfg.hz));
    }

    #[test]
    fn lapic_period_ms_for_1hz_is_1000() {
        let cfg = FrameTickConfig::new(1).expect("1 Hz is in range");
        assert_eq!(cfg.lapic_period_ms(), 1000);
    }

    #[test]
    fn lapic_period_ms_for_1000hz_is_1() {
        let cfg = FrameTickConfig::new(1000).expect("1000 Hz is the cap");
        assert_eq!(cfg.lapic_period_ms(), 1);
    }

    #[test]
    fn new_rejects_zero_hz() {
        assert!(FrameTickConfig::new(0).is_none());
    }

    #[test]
    fn new_rejects_above_max() {
        assert!(FrameTickConfig::new(2000).is_none());
        assert!(FrameTickConfig::new(FrameTickConfig::MAX_HZ + 1).is_none());
    }

    #[test]
    fn new_accepts_within_range() {
        let cfg = FrameTickConfig::new(120).expect("120 Hz is in range");
        assert_eq!(cfg.hz, 120);
    }

    #[test]
    fn accumulate_advances_total_and_pending() {
        let mut c = FrameTickCounter::new();
        assert_eq!(c.pending(), 0);
        assert_eq!(c.total(), 0);
        c.accumulate(1);
        assert_eq!(c.pending(), 1);
        assert_eq!(c.total(), 1);
    }

    #[test]
    fn drain_returns_count_and_clears_pending() {
        let mut c = FrameTickCounter::new();
        c.accumulate(5);
        let (missed, total) = c.drain();
        assert_eq!(missed, 5);
        assert_eq!(total, 5);
        assert_eq!(c.pending(), 0);
        assert_eq!(c.total(), 5);
    }

    #[test]
    fn multiple_drains_after_no_ticks_return_zero() {
        let mut c = FrameTickCounter::new();
        c.accumulate(3);
        let (_, prev_total) = c.drain();
        assert_eq!(prev_total, 3);
        let (missed, total) = c.drain();
        assert_eq!(missed, 0);
        assert_eq!(total, prev_total);
        let (missed2, total2) = c.drain();
        assert_eq!(missed2, 0);
        assert_eq!(total2, prev_total);
    }

    #[test]
    fn accumulate_zero_is_a_noop() {
        let mut c = FrameTickCounter::new();
        c.accumulate(7);
        let before = c;
        c.accumulate(0);
        assert_eq!(c, before);
        assert_eq!(c.pending(), 7);
        assert_eq!(c.total(), 7);
    }

    #[test]
    fn accumulate_saturates_pending_at_u32_max() {
        let mut c = FrameTickCounter::new();
        c.accumulate(u32::MAX);
        assert_eq!(c.pending(), u32::MAX);
        assert_eq!(c.total(), u32::MAX as u64);
        c.accumulate(1);
        assert_eq!(c.pending(), u32::MAX);
        // Total continues to climb past u32::MAX since it is u64-saturating.
        assert_eq!(c.total(), u32::MAX as u64 + 1);
    }

    proptest! {
        #[test]
        fn proptest_drain_then_pending_zero(
            steps in proptest::collection::vec(any::<u32>(), 0..32),
        ) {
            let mut c = FrameTickCounter::new();
            for n in &steps {
                c.accumulate(*n);
            }
            let _ = c.drain();
            prop_assert_eq!(c.pending(), 0);
        }

        #[test]
        fn proptest_total_monotone_nondecreasing(
            steps in proptest::collection::vec(any::<u32>(), 0..32),
        ) {
            let mut c = FrameTickCounter::new();
            let mut prev_total = c.total();
            for n in &steps {
                c.accumulate(*n);
                let now = c.total();
                prop_assert!(now >= prev_total);
                prev_total = now;
            }
        }
    }
}
