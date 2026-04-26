//! Phase 56 Track E.4 — frame composition stats ring (failing-test stub).
//!
//! Tests for the [`FrameStatsRing`] shape are committed first so the
//! failing-then-green discipline is auditable in the git history. The
//! `FrameStatsRing` type, its `push` / `iter_newest_first` /
//! `snapshot_newest_first` methods, and the `CAPACITY` constant are
//! intentionally undeclared here — the build fails until the
//! implementation lands in the next commit.

// ---------------------------------------------------------------------------
// Tests — committed before the implementation that makes them pass.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::protocol::FrameStatSample;
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
        assert_eq!(
            v.first().copied(),
            Some(s(CAPACITY as u64 + 4, (CAPACITY + 4) as u32))
        );
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
    }
}
