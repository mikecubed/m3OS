//! Phase 56 Track E.4 — frame composition stats ring.
//!
//! `display_server` records one [`FrameStatSample`] per `compose_frame`
//! call and pushes it into a [`FrameStatsRing`]. The control-socket
//! `frame-stats` verb returns the current window snapshot as a
//! `ControlEvent::FrameStatsReply`.
//!
//! ## Why a separate module (not `frame_tick.rs`)
//!
//! `frame_tick.rs` owns the pure-logic types around the kernel-driven
//! timer source: `FrameTickConfig` (period, hz) and `FrameTickCounter`
//! (missed-tick coalescing). The frame-stats ring is a downstream
//! observability concern about how long composition *takes*, not when
//! the next tick fires. Keeping them separate avoids a god-module and
//! lets either be extended without coupling.
//!
//! ## Resource bound
//!
//! The ring has a fixed [`CAPACITY`] (= 64) entries. Push is O(1) and
//! never grows the ring unboundedly. When full, the oldest sample is
//! overwritten — the spec calls for a "rolling window," not a complete
//! history.

use crate::display::protocol::FrameStatSample;
use alloc::vec::Vec;

/// Number of samples the ring retains at any time.
///
/// 64 was chosen so the in-memory footprint is small (64 × 12 B = 768 B
/// per ring) while still giving an `m3ctl frame-stats` query enough
/// history to compute meaningful percentiles over roughly one second of
/// 60 Hz composition.
pub const CAPACITY: usize = 64;

/// Fixed-capacity ring of frame composition timing samples.
///
/// Push is O(1). Iteration via [`iter_newest_first`](FrameStatsRing::iter_newest_first)
/// returns samples in *most-recent-first* order, which matches the
/// expected display order for `m3ctl frame-stats` (the most recent
/// frames are the most informative).
///
/// The ring is `pub` so `display_server` can construct + own one. All
/// fields are private so callers cannot synthesize an inconsistent
/// state.
#[derive(Clone, Debug)]
pub struct FrameStatsRing {
    /// Backing storage. `slots[head]` is the next index to write.
    /// Slots are filled lazily; `len` tracks how many are valid.
    slots: [FrameStatSample; CAPACITY],
    /// Cursor pointing at the slot to be written on the next `push`.
    head: usize,
    /// Number of valid samples currently in the ring (`0..=CAPACITY`).
    len: usize,
}

impl Default for FrameStatsRing {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameStatsRing {
    /// Construct an empty ring.
    pub const fn new() -> Self {
        const ZERO: FrameStatSample = FrameStatSample {
            frame_index: 0,
            compose_micros: 0,
        };
        Self {
            slots: [ZERO; CAPACITY],
            head: 0,
            len: 0,
        }
    }

    /// Number of samples currently retained (`0..=CAPACITY`).
    pub const fn len(&self) -> usize {
        self.len
    }

    /// True iff no samples have been pushed since [`new`](Self::new) was
    /// called or the ring was last cleared.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True iff [`len`](Self::len) has reached [`CAPACITY`].
    pub const fn is_full(&self) -> bool {
        self.len == CAPACITY
    }

    /// Push a new sample. O(1). Overwrites the oldest sample once the
    /// ring is full.
    pub fn push(&mut self, sample: FrameStatSample) {
        // `head` is the next *write* position, so we always write here
        // first and then advance. When the ring is full the slot we
        // overwrite *is* the oldest sample — that matches the desired
        // rolling-window semantics.
        self.slots[self.head] = sample;
        self.head = (self.head + 1) % CAPACITY;
        if self.len < CAPACITY {
            self.len = self.len.saturating_add(1);
        }
    }

    /// Iterate the retained samples in newest-first order.
    ///
    /// Returns an [`ExactSizeIterator`] borrowing into `self`; the
    /// snapshot survives further `push`es only via the iterator's
    /// lifetime, which the borrow-checker enforces.
    pub fn iter_newest_first(&self) -> NewestFirstIter<'_> {
        NewestFirstIter {
            ring: self,
            yielded: 0,
        }
    }

    /// Materialize the retained samples into an owned `Vec` in
    /// newest-first order. Bounded by [`CAPACITY`] so the allocation is
    /// O(1) in the worst case. Used by the control-socket `frame-stats`
    /// reply path to build the wire payload.
    pub fn snapshot_newest_first(&self) -> Vec<FrameStatSample> {
        let mut out = Vec::with_capacity(self.len);
        for s in self.iter_newest_first() {
            out.push(s);
        }
        out
    }

    /// Drop every retained sample.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// Iterator returned by [`FrameStatsRing::iter_newest_first`].
pub struct NewestFirstIter<'a> {
    ring: &'a FrameStatsRing,
    /// Number of samples yielded so far.
    yielded: usize,
}

impl<'a> Iterator for NewestFirstIter<'a> {
    type Item = FrameStatSample;

    fn next(&mut self) -> Option<Self::Item> {
        if self.yielded >= self.ring.len {
            return None;
        }
        // `head` points one past the most-recent write. So index
        // `(head + CAPACITY - 1 - yielded) % CAPACITY` walks backwards
        // from the newest sample.
        let idx = (self.ring.head + CAPACITY - 1 - self.yielded) % CAPACITY;
        let sample = self.ring.slots[idx];
        self.yielded = self.yielded.saturating_add(1);
        Some(sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.ring.len - self.yielded;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for NewestFirstIter<'a> {}

// ---------------------------------------------------------------------------
// Tests — committed before the implementation that makes them pass.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn s(idx: u64, us: u32) -> FrameStatSample {
        FrameStatSample {
            frame_index: idx,
            compose_micros: us,
        }
    }

    #[test]
    fn new_is_empty() {
        let r = FrameStatsRing::new();
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
        assert!(!r.is_full());
        assert_eq!(r.iter_newest_first().count(), 0);
    }

    #[test]
    fn push_one_then_iter() {
        let mut r = FrameStatsRing::new();
        r.push(s(1, 100));
        assert_eq!(r.len(), 1);
        let v = r.snapshot_newest_first();
        assert_eq!(v, vec![s(1, 100)]);
    }

    #[test]
    fn push_three_iter_newest_first() {
        let mut r = FrameStatsRing::new();
        r.push(s(1, 10));
        r.push(s(2, 20));
        r.push(s(3, 30));
        let v = r.snapshot_newest_first();
        assert_eq!(v, vec![s(3, 30), s(2, 20), s(1, 10)]);
    }

    #[test]
    fn fills_to_capacity_then_reports_full() {
        let mut r = FrameStatsRing::new();
        for i in 0..CAPACITY as u64 {
            r.push(s(i, i as u32 * 2));
        }
        assert_eq!(r.len(), CAPACITY);
        assert!(r.is_full());
    }

    #[test]
    fn overflow_overwrites_oldest_only() {
        let mut r = FrameStatsRing::new();
        for i in 0..(CAPACITY as u64 + 5) {
            r.push(s(i, i as u32));
        }
        assert_eq!(r.len(), CAPACITY);
        let v = r.snapshot_newest_first();
        // Newest sample is the last we pushed; oldest retained is
        // `total - CAPACITY = (CAPACITY+5) - CAPACITY = 5`.
        assert_eq!(v.first().copied(), Some(s(CAPACITY as u64 + 4, (CAPACITY + 4) as u32)));
        assert_eq!(v.last().copied(), Some(s(5, 5)));
        assert_eq!(v.len(), CAPACITY);
    }

    #[test]
    fn clear_resets_to_empty() {
        let mut r = FrameStatsRing::new();
        r.push(s(1, 100));
        r.push(s(2, 200));
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.iter_newest_first().count(), 0);
    }

    #[test]
    fn iter_size_hint_exact() {
        let mut r = FrameStatsRing::new();
        for i in 0..10u64 {
            r.push(s(i, 0));
        }
        let it = r.iter_newest_first();
        assert_eq!(it.size_hint(), (10, Some(10)));
        // Use `count` (returns size) — confirms ExactSizeIterator
        // semantics.
        assert_eq!(it.count(), 10);
    }

    proptest! {
        #[test]
        fn proptest_len_matches_pushes_capped_at_capacity(
            pushes in proptest::collection::vec((any::<u64>(), any::<u32>()), 0..200),
        ) {
            let mut r = FrameStatsRing::new();
            for (idx, us) in &pushes {
                r.push(s(*idx, *us));
            }
            let expected = pushes.len().min(CAPACITY);
            prop_assert_eq!(r.len(), expected);
            prop_assert_eq!(r.iter_newest_first().count(), expected);
        }

        #[test]
        fn proptest_newest_first_matches_last_n_reverse(
            pushes in proptest::collection::vec((any::<u64>(), any::<u32>()), 0..200),
        ) {
            let mut r = FrameStatsRing::new();
            for (idx, us) in &pushes {
                r.push(s(*idx, *us));
            }
            // Compute the expected "last CAPACITY entries reversed" using
            // a trivial reference impl — a `Vec` push-into / take-tail.
            let mut reference: Vec<FrameStatSample> = pushes
                .iter()
                .map(|(i, u)| s(*i, *u))
                .collect();
            if reference.len() > CAPACITY {
                let drop = reference.len() - CAPACITY;
                reference.drain(0..drop);
            }
            reference.reverse();
            let actual = r.snapshot_newest_first();
            prop_assert_eq!(actual, reference);
        }
    }
}
