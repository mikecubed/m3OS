//! IOVA (I/O Virtual Address) space allocator — pure logic, host-testable.
//!
//! Implementation lands in Phase 55a Track A.2.
//!
//! This file currently contains only failing tests. The implementation lands
//! in the follow-up commit; keeping the tests first makes the git history
//! demonstrate test-driven development.

// ---------------------------------------------------------------------------
// Tests (commit order: this failing-tests commit precedes the implementation
// commit that makes them pass).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use proptest::prelude::*;

    const PAGE: usize = 4096;
    const PAGE_U64: u64 = 4096;
    const WINDOW_START: u64 = 0x1_0000_0000; // 4 GiB
    const WINDOW_END: u64 = 0x2_0000_0000; // 8 GiB

    fn fresh_allocator() -> IovaAllocator {
        IovaAllocator::new(WINDOW_START, WINDOW_END, PAGE)
    }

    // ---------- Unit tests ----------

    #[test]
    fn happy_path_allocate_returns_page_aligned() {
        let mut alloc = fresh_allocator();
        let r = alloc.allocate(PAGE, PAGE).expect("first alloc ok");
        assert_eq!(r.len, PAGE);
        assert!(r.start >= WINDOW_START);
        assert!(r.start + r.len as u64 <= WINDOW_END);
        assert!(r.start.is_multiple_of(PAGE_U64));
    }

    #[test]
    fn sequential_allocations_do_not_overlap() {
        let mut alloc = fresh_allocator();
        let a = alloc.allocate(PAGE, PAGE).unwrap();
        let b = alloc.allocate(PAGE, PAGE).unwrap();
        let c = alloc.allocate(2 * PAGE, PAGE).unwrap();
        assert!(a.start + a.len as u64 <= b.start || b.start + b.len as u64 <= a.start);
        assert!(a.start + a.len as u64 <= c.start || c.start + c.len as u64 <= a.start);
        assert!(b.start + b.len as u64 <= c.start || c.start + c.len as u64 <= b.start);
    }

    #[test]
    fn free_then_allocate_reuses_freed_range() {
        let mut alloc = fresh_allocator();
        let a = alloc.allocate(PAGE, PAGE).unwrap();
        let b = alloc.allocate(PAGE, PAGE).unwrap();
        alloc.free(a).unwrap();
        let c = alloc.allocate(PAGE, PAGE).unwrap();
        assert_eq!(c, a, "allocator should reuse the freed range");
        assert_ne!(b, c);
    }

    #[test]
    fn alignment_larger_than_min_is_honored() {
        let mut alloc = fresh_allocator();
        // Burn a page so the bump cursor is not 2 MiB-aligned.
        let _ = alloc.allocate(PAGE, PAGE).unwrap();
        let big_align = 2 * 1024 * 1024; // 2 MiB
        let r = alloc.allocate(PAGE, big_align).unwrap();
        assert!(r.start.is_multiple_of(big_align as u64));
    }

    #[test]
    fn alignment_not_power_of_two_errors() {
        let mut alloc = fresh_allocator();
        let err = alloc.allocate(PAGE, 6144).unwrap_err();
        assert_eq!(err, IovaError::AlignmentUnsatisfiable);
    }

    #[test]
    fn alignment_smaller_than_min_errors() {
        let mut alloc = fresh_allocator();
        let err = alloc.allocate(PAGE, 1024).unwrap_err();
        assert_eq!(err, IovaError::AlignmentUnsatisfiable);
    }

    #[test]
    fn zero_length_allocation_errors() {
        let mut alloc = fresh_allocator();
        let err = alloc.allocate(0, PAGE).unwrap_err();
        assert_eq!(err, IovaError::ZeroLength);
    }

    #[test]
    fn exhaustion_returns_exhausted_without_panic() {
        // Small window: 4 pages.
        let mut alloc = IovaAllocator::new(WINDOW_START, WINDOW_START + 4 * PAGE_U64, PAGE);
        let _ = alloc.allocate(PAGE, PAGE).unwrap();
        let _ = alloc.allocate(PAGE, PAGE).unwrap();
        let _ = alloc.allocate(PAGE, PAGE).unwrap();
        let _ = alloc.allocate(PAGE, PAGE).unwrap();
        let err = alloc.allocate(PAGE, PAGE).unwrap_err();
        assert_eq!(err, IovaError::Exhausted);
        // A subsequent request also returns Exhausted, without mutating state.
        let err2 = alloc.allocate(PAGE, PAGE).unwrap_err();
        assert_eq!(err2, IovaError::Exhausted);
    }

    #[test]
    fn double_free_errors() {
        let mut alloc = fresh_allocator();
        let a = alloc.allocate(PAGE, PAGE).unwrap();
        alloc.free(a).unwrap();
        let err = alloc.free(a).unwrap_err();
        assert_eq!(err, IovaError::DoubleFree);
    }

    #[test]
    fn free_of_unknown_range_errors() {
        let mut alloc = fresh_allocator();
        let fake = IovaRange {
            start: WINDOW_START,
            len: PAGE,
        };
        let err = alloc.free(fake).unwrap_err();
        assert_eq!(err, IovaError::DoubleFree);
    }

    #[test]
    fn mixed_size_allocation_and_freelist_split() {
        let mut alloc = fresh_allocator();
        // Allocate a 16-page chunk, free it, then ask for smaller chunks:
        // the freelist entry should be split, not discarded.
        let big = alloc.allocate(16 * PAGE, PAGE).unwrap();
        alloc.free(big).unwrap();
        let small_a = alloc.allocate(PAGE, PAGE).unwrap();
        let small_b = alloc.allocate(PAGE, PAGE).unwrap();
        assert!(
            small_a.start + small_a.len as u64 <= small_b.start
                || small_b.start + small_b.len as u64 <= small_a.start
        );
        // Both small allocations come from the freed big range.
        let big_end = big.start + big.len as u64;
        assert!(small_a.start >= big.start && small_a.start + small_a.len as u64 <= big_end);
        assert!(small_b.start >= big.start && small_b.start + small_b.len as u64 <= big_end);
    }

    #[test]
    fn reserve_blocks_overlapping_allocation() {
        let mut alloc = fresh_allocator();
        let reserved = IovaRange {
            start: WINDOW_START,
            len: 4 * PAGE,
        };
        alloc.reserve(reserved).unwrap();
        let r = alloc.allocate(PAGE, PAGE).unwrap();
        let reserved_end = reserved.start + reserved.len as u64;
        let r_end = r.start + r.len as u64;
        assert!(r_end <= reserved.start || r.start >= reserved_end);
    }

    #[test]
    fn exhaustion_with_alignment_gap() {
        // Tight window, large alignment request cannot fit.
        let mut alloc = IovaAllocator::new(WINDOW_START, WINDOW_START + 2 * PAGE_U64, PAGE);
        let err = alloc.allocate(PAGE, 2 * 1024 * 1024).unwrap_err();
        assert_eq!(err, IovaError::Exhausted);
    }

    // ---------- Property tests ----------

    #[derive(Debug, Clone)]
    enum Op {
        Alloc { len_pages: u32, align_shift: u8 },
        Free { index: u16 },
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (1u32..=8u32, 0u8..=3u8)
                .prop_map(|(len_pages, align_shift)| Op::Alloc { len_pages, align_shift }),
            (0u16..=64u16).prop_map(|index| Op::Free { index }),
        ]
    }

    proptest! {
        #[test]
        fn no_overlap_across_arbitrary_sequences(ops in prop::collection::vec(op_strategy(), 1..200)) {
            let mut alloc = IovaAllocator::new(WINDOW_START, WINDOW_START + 256 * PAGE_U64, PAGE);
            let mut live: Vec<IovaRange> = Vec::new();

            for op in ops {
                match op {
                    Op::Alloc { len_pages, align_shift } => {
                        let len = (len_pages as usize) * PAGE;
                        let align = PAGE << align_shift as usize;
                        match alloc.allocate(len, align) {
                            Ok(r) => {
                                prop_assert_eq!(r.len, len);
                                prop_assert!(r.start.is_multiple_of(align as u64));
                                prop_assert!(r.start >= alloc.window_start());
                                prop_assert!(r.start + r.len as u64 <= alloc.window_end());
                                for existing in &live {
                                    let r_end = r.start + r.len as u64;
                                    let e_end = existing.start + existing.len as u64;
                                    prop_assert!(r_end <= existing.start || existing.start + existing.len as u64 <= r.start || r.start >= e_end);
                                }
                                live.push(r);
                            }
                            Err(IovaError::Exhausted) | Err(IovaError::AlignmentUnsatisfiable) => {
                                // Expected occasional failures; state must be unchanged.
                            }
                            Err(other) => prop_assert!(false, "unexpected alloc error: {:?}", other),
                        }
                    }
                    Op::Free { index } => {
                        if live.is_empty() {
                            continue;
                        }
                        let i = (index as usize) % live.len();
                        let r = live.remove(i);
                        prop_assert!(alloc.free(r).is_ok());
                    }
                }
            }
        }

        #[test]
        fn alignment_guarantee_holds(
            align_shift in 0u8..=6u8,
            len_pages in 1u32..=4u32,
        ) {
            let mut alloc = IovaAllocator::new(WINDOW_START, WINDOW_START + 512 * PAGE_U64, PAGE);
            let align = PAGE << align_shift as usize;
            let len = (len_pages as usize) * PAGE;
            match alloc.allocate(len, align) {
                Ok(r) => {
                    prop_assert!(r.start.is_multiple_of(align as u64));
                    prop_assert_eq!(r.len, len);
                }
                Err(IovaError::Exhausted) | Err(IovaError::AlignmentUnsatisfiable) => {}
                Err(other) => prop_assert!(false, "unexpected error: {:?}", other),
            }
        }

        #[test]
        fn freed_range_is_reallocable(
            allocs in prop::collection::vec(1u32..=4u32, 1..20),
        ) {
            let mut alloc = IovaAllocator::new(WINDOW_START, WINDOW_START + 512 * PAGE_U64, PAGE);
            let mut ranges: Vec<IovaRange> = Vec::new();
            for pages in &allocs {
                let len = (*pages as usize) * PAGE;
                if let Ok(r) = alloc.allocate(len, PAGE) {
                    ranges.push(r);
                }
            }
            for r in &ranges {
                prop_assert!(alloc.free(*r).is_ok());
            }
            for r in &ranges {
                let realloc = alloc.allocate(r.len, PAGE);
                prop_assert!(realloc.is_ok(), "reallocation of {:?} failed", r);
                let got = realloc.unwrap();
                prop_assert!(alloc.free(got).is_ok());
            }
        }

        #[test]
        fn no_panic_on_arbitrary_free(
            fake_start in 0u64..=0xFFFF_FFFF_FFFFu64,
            fake_len_pages in 1u32..=16u32,
        ) {
            let mut alloc = fresh_allocator();
            let fake = IovaRange {
                start: fake_start,
                len: (fake_len_pages as usize) * PAGE,
            };
            let r = alloc.free(fake);
            prop_assert_eq!(r, Err(IovaError::DoubleFree));
        }
    }
}
