//! Reserved-region set algebra — pure logic, host-testable.
//!
//! Scaffolding stubs for Phase 55a Track A.3. The real implementation is
//! introduced in a follow-up commit; this commit lands only the failing
//! test suite and the minimum type surface the tests need to compile.

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionFlags(pub u32);

impl RegionFlags {
    pub const NONE: Self = Self(0);
    pub const WRITABLE: Self = Self(1 << 0);
    pub const EXECUTABLE: Self = Self(1 << 1);
    pub const CACHEABLE: Self = Self(1 << 2);
    pub const FIRMWARE_OWNED: Self = Self(1 << 3);

    pub const fn union(self, _other: Self) -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservedRegion {
    pub start: u64,
    pub len: usize,
    pub flags: RegionFlags,
}

impl ReservedRegion {
    pub fn end(&self) -> u64 {
        0
    }

    pub fn contains_addr(&self, _addr: u64) -> bool {
        false
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReservedRegionSet {
    regions: Vec<ReservedRegion>,
}

impl ReservedRegionSet {
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    pub fn insert(&mut self, _region: ReservedRegion) {
        // Stub — real implementation in follow-up commit.
    }

    pub fn union(&mut self, _other: &ReservedRegionSet) {
        // Stub — real implementation in follow-up commit.
    }

    pub(crate) fn merge_overlapping(&mut self) {
        // Stub — real implementation in follow-up commit.
    }

    pub fn contains(&self, _addr: u64) -> Option<&ReservedRegion> {
        None
    }

    pub fn iter(&self) -> impl Iterator<Item = &ReservedRegion> {
        self.regions.iter()
    }

    pub fn len(&self) -> usize {
        self.regions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn region(start: u64, len: usize, flags: u32) -> ReservedRegion {
        ReservedRegion {
            start,
            len,
            flags: RegionFlags(flags),
        }
    }

    #[test]
    fn empty_set_contains_nothing() {
        let set = ReservedRegionSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        assert!(set.contains(0).is_none());
        assert!(set.contains(0x1000).is_none());
        assert!(set.contains(u64::MAX).is_none());
    }

    #[test]
    fn single_insert_contains_within_range() {
        let mut set = ReservedRegionSet::new();
        set.insert(region(0x1000, 0x1000, 0b0001));

        assert!(set.contains(0x0FFF).is_none());
        let hit = set.contains(0x1000).expect("start inside");
        assert_eq!(hit.start, 0x1000);
        assert_eq!(hit.len, 0x1000);
        assert_eq!(hit.flags, RegionFlags(0b0001));

        assert!(set.contains(0x17FF).is_some());
        assert!(set.contains(0x1FFF).is_some());
        assert!(set.contains(0x2000).is_none());
        assert!(set.contains(0x2001).is_none());
    }

    #[test]
    fn zero_length_insert_is_noop() {
        let mut set = ReservedRegionSet::new();
        set.insert(region(0x1000, 0, 0b0001));
        assert!(set.is_empty());
    }

    #[test]
    fn overlapping_inserts_merge_and_or_flags() {
        let mut set = ReservedRegionSet::new();
        set.insert(region(0x1000, 0x2000, 0b0001));
        set.insert(region(0x2000, 0x2000, 0b0010));

        assert_eq!(set.len(), 1);
        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0x1000);
        assert_eq!(r.len, 0x3000);
        assert_eq!(r.flags, RegionFlags(0b0011));
    }

    #[test]
    fn touching_inserts_merge() {
        // [0..100] + [100..200] → [0..200]
        let mut set = ReservedRegionSet::new();
        set.insert(region(0, 100, 0b0001));
        set.insert(region(100, 100, 0b0100));

        assert_eq!(set.len(), 1);
        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0);
        assert_eq!(r.len, 200);
        assert_eq!(r.flags, RegionFlags(0b0101));
    }

    #[test]
    fn non_touching_inserts_stay_separate() {
        let mut set = ReservedRegionSet::new();
        set.insert(region(0, 100, 0));
        set.insert(region(101, 100, 0));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn inserts_get_sorted_by_start() {
        let mut set = ReservedRegionSet::new();
        set.insert(region(0x5000, 0x1000, 0));
        set.insert(region(0x1000, 0x1000, 0));
        set.insert(region(0x3000, 0x1000, 0));

        let starts: Vec<u64> = set.iter().map(|r| r.start).collect();
        assert_eq!(starts, vec![0x1000, 0x3000, 0x5000]);
    }

    #[test]
    fn insert_swallowed_region_keeps_union_flags() {
        // Outer region absorbs an interior region; flags OR together.
        let mut set = ReservedRegionSet::new();
        set.insert(region(0x0, 0x1000, 0b0001));
        set.insert(region(0x100, 0x200, 0b1000));

        assert_eq!(set.len(), 1);
        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0);
        assert_eq!(r.len, 0x1000);
        assert_eq!(r.flags, RegionFlags(0b1001));
    }

    #[test]
    fn insert_triggers_chained_merge() {
        // Start with two isolated islands then drop a bridge between them.
        let mut set = ReservedRegionSet::new();
        set.insert(region(0, 100, 0b0001));
        set.insert(region(300, 100, 0b0010));

        assert_eq!(set.len(), 2);
        set.insert(region(50, 300, 0b0100)); // spans 50..350, overlaps both
        assert_eq!(set.len(), 1);

        let r = set.iter().next().unwrap();
        assert_eq!(r.start, 0);
        assert_eq!(r.len, 400);
        assert_eq!(r.flags, RegionFlags(0b0111));
    }

    #[test]
    fn union_merges_overlapping_ranges() {
        let mut a = ReservedRegionSet::new();
        a.insert(region(0x1000, 0x1000, 0b0001));
        a.insert(region(0x5000, 0x1000, 0b0010));

        let mut b = ReservedRegionSet::new();
        b.insert(region(0x1800, 0x1000, 0b0100)); // overlaps a's first range
        b.insert(region(0x9000, 0x1000, 0b1000));

        a.union(&b);

        // After union we expect: [0x1000..0x2800, flags=0b0101],
        //                        [0x5000..0x6000, flags=0b0010],
        //                        [0x9000..0xA000, flags=0b1000].
        assert_eq!(a.len(), 3);
        let rs: Vec<&ReservedRegion> = a.iter().collect();

        assert_eq!(rs[0].start, 0x1000);
        assert_eq!(rs[0].len, 0x1800);
        assert_eq!(rs[0].flags, RegionFlags(0b0101));

        assert_eq!(rs[1].start, 0x5000);
        assert_eq!(rs[1].len, 0x1000);
        assert_eq!(rs[1].flags, RegionFlags(0b0010));

        assert_eq!(rs[2].start, 0x9000);
        assert_eq!(rs[2].len, 0x1000);
        assert_eq!(rs[2].flags, RegionFlags(0b1000));
    }

    #[test]
    fn union_with_empty_is_noop() {
        let mut a = ReservedRegionSet::new();
        a.insert(region(0x1000, 0x1000, 0b0001));
        let empty = ReservedRegionSet::new();
        a.union(&empty);
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn union_into_empty_copies() {
        let mut a = ReservedRegionSet::new();
        let mut b = ReservedRegionSet::new();
        b.insert(region(0x1000, 0x1000, 0b0001));
        b.insert(region(0x3000, 0x1000, 0b0010));

        a.union(&b);
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn contains_binary_search_across_100_regions() {
        let mut set = ReservedRegionSet::new();
        // Build 100 disjoint regions spaced 0x1000 apart, each 0x100 long.
        for i in 0..100u64 {
            set.insert(region(i * 0x1000, 0x100, 0));
        }
        assert_eq!(set.len(), 100);

        // Probe inside every region.
        for i in 0..100u64 {
            let addr_start = i * 0x1000;
            let addr_mid = addr_start + 0x80;
            let addr_last = addr_start + 0xFF;
            let addr_gap = addr_start + 0x100;
            let addr_next = addr_start + 0x1000 - 1;

            assert!(set.contains(addr_start).is_some(), "start of region {}", i);
            assert!(set.contains(addr_mid).is_some(), "mid of region {}", i);
            assert!(set.contains(addr_last).is_some(), "last of region {}", i);
            assert!(
                set.contains(addr_gap).is_none(),
                "gap after region {} must miss",
                i
            );
            if i < 99 {
                assert!(
                    set.contains(addr_next).is_none(),
                    "just before region {} must miss",
                    i + 1
                );
            }
        }

        // Above the highest region must miss.
        assert!(set.contains(100 * 0x1000).is_none());
    }

    #[test]
    fn iter_returns_sorted_sequence() {
        let mut set = ReservedRegionSet::new();
        set.insert(region(0x3000, 0x100, 0));
        set.insert(region(0x1000, 0x100, 0));
        set.insert(region(0x2000, 0x100, 0));

        let starts: Vec<u64> = set.iter().map(|r| r.start).collect();
        for pair in starts.windows(2) {
            assert!(pair[0] < pair[1]);
        }
    }

    // ----- Property tests -----

    use proptest::prelude::*;

    fn region_strategy() -> impl Strategy<Value = ReservedRegion> {
        // Keep ranges inside a bounded sandbox so property cases stay tractable.
        // Starts: 0..2^20, lengths: 1..=4096, flags: 0..=15.
        (0u64..(1 << 20), 1usize..=4096, 0u32..=15).prop_map(|(start, len, f)| ReservedRegion {
            start,
            len,
            flags: RegionFlags(f),
        })
    }

    proptest! {
        /// Inserting the same region twice equals inserting once.
        #[test]
        fn idempotent_double_insert(r in region_strategy()) {
            let mut a = ReservedRegionSet::new();
            a.insert(r);

            let mut b = ReservedRegionSet::new();
            b.insert(r);
            b.insert(r);

            let av: Vec<ReservedRegion> = a.iter().copied().collect();
            let bv: Vec<ReservedRegion> = b.iter().copied().collect();
            prop_assert_eq!(av, bv);
        }

        /// Inserting in any permutation yields the same final set.
        #[test]
        fn order_independent(rs in prop::collection::vec(region_strategy(), 0..=16)) {
            let mut a = ReservedRegionSet::new();
            for r in &rs {
                a.insert(*r);
            }

            // Insert in reversed order.
            let mut b = ReservedRegionSet::new();
            for r in rs.iter().rev() {
                b.insert(*r);
            }

            // Insert via union of singletons (another permutation surface).
            let mut c = ReservedRegionSet::new();
            for r in &rs {
                let mut s = ReservedRegionSet::new();
                s.insert(*r);
                c.union(&s);
            }

            let av: Vec<ReservedRegion> = a.iter().copied().collect();
            let bv: Vec<ReservedRegion> = b.iter().copied().collect();
            let cv: Vec<ReservedRegion> = c.iter().copied().collect();
            prop_assert_eq!(&av, &bv);
            prop_assert_eq!(&av, &cv);
        }

        /// `contains(addr)` returns `Some` iff `addr` falls within some
        /// inserted region (after all unions).
        #[test]
        fn contains_iff_covered(
            rs in prop::collection::vec(region_strategy(), 0..=12),
            probes in prop::collection::vec(0u64..(1 << 21), 0..=32),
        ) {
            let mut set = ReservedRegionSet::new();
            for r in &rs {
                set.insert(*r);
            }

            for &addr in &probes {
                // Ground truth: check every original inserted region.
                let covered = rs.iter().any(|r| r.start <= addr && addr < r.start.saturating_add(r.len as u64));
                let hit = set.contains(addr);
                prop_assert_eq!(hit.is_some(), covered, "addr={:#x} covered={} hit={:?}", addr, covered, hit);
            }
        }

        /// Set invariants hold after any sequence of insertions: regions are
        /// sorted by start and no two neighbors overlap or touch.
        #[test]
        fn invariants_sorted_and_disjoint(
            rs in prop::collection::vec(region_strategy(), 0..=24),
        ) {
            let mut set = ReservedRegionSet::new();
            for r in &rs {
                set.insert(*r);
            }

            let regions: Vec<ReservedRegion> = set.iter().copied().collect();
            for pair in regions.windows(2) {
                prop_assert!(pair[0].start < pair[1].start);
                // Non-touching: end of previous must be strictly less than next start.
                prop_assert!(pair[0].end() < pair[1].start);
            }
        }
    }
}
