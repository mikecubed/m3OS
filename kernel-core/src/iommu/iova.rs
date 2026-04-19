//! IOVA (I/O Virtual Address) space allocator — pure logic, host-testable.
//!
//! Implementation lands in Phase 55a Track A.2.
//!
//! Every IOMMU-mapped DMA allocation pulls an address range from this allocator.
//! It must enforce two invariants across arbitrary allocate / free sequences:
//!
//! 1. **No overlap.** Two live allocations never share any IOVA byte.
//! 2. **Freelist reuse.** A freed range is re-allocable (subject to the
//!    fragmentation bounds documented below) before the bump cursor moves again.
//!
//! Allocation strategy:
//!
//! - Fast path is a monotonic **bump cursor** that tracks the next unallocated
//!   IOVA, aligning up to the caller-requested alignment.
//! - Returned ranges are pushed onto a **freelist**. Subsequent `allocate`
//!   calls first scan the freelist for a first-fit entry that satisfies the
//!   requested `(len, align)` pair before bumping.
//! - Exhaustion (the bump cursor would cross `end`) returns
//!   [`IovaError::Exhausted`]; no allocation, no panic.
//! - **Fragmentation note:** the freelist does not coalesce adjacent freed
//!   ranges. A workload that frees many small sub-ranges of a previously
//!   returned span will see those stay as discrete freelist entries. This is
//!   accepted for Phase 55a: driver callers free whole allocations, not
//!   sub-slices. A future phase may add coalescing if needed.

use alloc::vec::Vec;

/// A half-open IOVA range `[start, start + len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IovaRange {
    /// Inclusive start address.
    pub start: u64,
    /// Length in bytes. Always nonzero for live allocations.
    pub len: usize,
}

impl IovaRange {
    /// Exclusive end address.
    fn end(self) -> u64 {
        self.start + self.len as u64
    }

    /// `true` iff `self` and `other` share at least one IOVA byte.
    fn overlaps(self, other: IovaRange) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}

/// Errors returned by [`IovaAllocator::allocate`] and [`IovaAllocator::free`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IovaError {
    /// No space remains in the IOVA window for the requested allocation.
    Exhausted,
    /// Requested alignment is not a power of two, is smaller than the
    /// allocator's minimum alignment, or cannot be satisfied within the
    /// remaining window.
    AlignmentUnsatisfiable,
    /// Caller asked for a zero-length allocation.
    ZeroLength,
    /// The range passed to `free` is not currently allocated (either never
    /// allocated, already freed, or not a range this allocator handed out).
    DoubleFree,
}

/// Bump + freelist IOVA space allocator.
///
/// Backing state is a Vec-backed freelist and a single cursor. State size is
/// bounded by the number of live-free entries; no other dynamic growth.
pub struct IovaAllocator {
    /// Inclusive window start. Allocations never return addresses below this.
    start: u64,
    /// Exclusive window end. Allocations never return addresses at or above this.
    end: u64,
    /// Minimum per-allocation alignment in bytes. Always power-of-two, >= 4096.
    min_alignment: usize,
    /// Next unused IOVA for the bump path. Aligned to at least `min_alignment`.
    cursor: u64,
    /// Freed ranges available for re-use. First-fit scan on allocate.
    freelist: Vec<IovaRange>,
    /// Currently-live allocations (kept sorted by `start`). Used for
    /// overlap and double-free detection.
    live: Vec<IovaRange>,
}

impl IovaAllocator {
    /// Create a new allocator for `[start, end)` with at least `min_alignment`
    /// bytes of alignment on every returned range.
    ///
    /// Preconditions (enforced via `debug_assert!`):
    /// - `start < end`
    /// - `min_alignment` is a power of two
    /// - `min_alignment >= 4096`
    /// - `start` is aligned to `min_alignment`
    ///
    /// Release builds silently clamp violations rather than panic (non-test
    /// code in `kernel-core` never panics). The `cursor` is rounded up to
    /// `min_alignment` inside `[start, end)`; a caller that passes a garbage
    /// range gets an allocator that immediately reports `Exhausted`.
    pub fn new(start: u64, end: u64, min_alignment: usize) -> Self {
        debug_assert!(
            start < end,
            "IovaAllocator::new: start {start:#x} must be < end {end:#x}"
        );
        debug_assert!(
            min_alignment.is_power_of_two(),
            "IovaAllocator::new: min_alignment {min_alignment} must be power of two",
        );
        debug_assert!(
            min_alignment >= 4096,
            "IovaAllocator::new: min_alignment {min_alignment} must be >= 4096",
        );
        debug_assert!(
            start.is_multiple_of(min_alignment as u64),
            "IovaAllocator::new: start {start:#x} must be aligned to min_alignment {min_alignment}",
        );

        // Release-build hardening: if the caller violated preconditions, clamp to a
        // degenerate but well-behaved state rather than producing garbage allocations.
        let min_alignment = if min_alignment.is_power_of_two() && min_alignment >= 4096 {
            min_alignment
        } else {
            4096
        };
        let start = align_up_u64(start, min_alignment as u64);
        let end = if end > start { end } else { start };

        Self {
            start,
            end,
            min_alignment,
            cursor: start,
            freelist: Vec::new(),
            live: Vec::new(),
        }
    }

    /// Allocate `len` bytes aligned to at least `alignment`.
    ///
    /// `alignment` must be a power of two and at least `min_alignment`. `len`
    /// is rounded up to `min_alignment` to keep every live range fully
    /// aligned.
    pub fn allocate(&mut self, len: usize, alignment: usize) -> Result<IovaRange, IovaError> {
        if len == 0 {
            return Err(IovaError::ZeroLength);
        }
        if !alignment.is_power_of_two() || alignment < self.min_alignment {
            return Err(IovaError::AlignmentUnsatisfiable);
        }

        // Round len up so subsequent bump stays aligned.
        let aligned_len = align_up_usize(len, self.min_alignment);
        if aligned_len == 0 {
            return Err(IovaError::AlignmentUnsatisfiable);
        }

        if let Some(range) = self.pop_from_freelist(aligned_len, alignment) {
            self.insert_live(range);
            return Ok(range);
        }

        self.bump(aligned_len, alignment)
    }

    /// Return a previously-allocated range to the freelist.
    ///
    /// The range must be byte-identical (same `start` and `len`) to a range
    /// currently held as live by this allocator; otherwise the call returns
    /// [`IovaError::DoubleFree`].
    pub fn free(&mut self, range: IovaRange) -> Result<(), IovaError> {
        // Match live set by (start, len) exactly — partial frees are not supported.
        let Some(pos) = self.live.iter().position(|r| *r == range) else {
            return Err(IovaError::DoubleFree);
        };
        let range = self.live.remove(pos);
        self.freelist.push(range);
        Ok(())
    }

    /// Pre-reserve a range as unavailable (used e.g. for identity-mapped
    /// RMRR / unity-map regions). The reservation is tracked in the live
    /// set so subsequent allocations do not overlap it.
    ///
    /// `range` must lie fully inside `[start, end)` and not overlap any
    /// existing live range.
    pub fn reserve(&mut self, range: IovaRange) -> Result<(), IovaError> {
        if range.len == 0 {
            return Err(IovaError::ZeroLength);
        }
        if range.start < self.start || range.end() > self.end {
            return Err(IovaError::AlignmentUnsatisfiable);
        }
        if self.live.iter().any(|r| r.overlaps(range)) {
            return Err(IovaError::AlignmentUnsatisfiable);
        }
        self.insert_live(range);
        Ok(())
    }

    /// Minimum alignment observed by every returned range.
    pub fn min_alignment(&self) -> usize {
        self.min_alignment
    }

    /// Inclusive window start.
    pub fn window_start(&self) -> u64 {
        self.start
    }

    /// Exclusive window end.
    pub fn window_end(&self) -> u64 {
        self.end
    }

    /// Current number of freelist entries (diagnostic).
    pub fn freelist_len(&self) -> usize {
        self.freelist.len()
    }

    /// Current number of live allocations (diagnostic).
    pub fn live_len(&self) -> usize {
        self.live.len()
    }

    // ---- internal helpers ----

    /// Scan the freelist for a first-fit range satisfying `(len, align)`.
    /// If a matched entry is larger than needed, it is split and the remainder
    /// is returned to the freelist.
    fn pop_from_freelist(&mut self, len: usize, alignment: usize) -> Option<IovaRange> {
        for i in 0..self.freelist.len() {
            let entry = self.freelist[i];
            let aligned_start = align_up_u64(entry.start, alignment as u64);
            if aligned_start < entry.start {
                // Overflow.
                continue;
            }
            let aligned_end = aligned_start.checked_add(len as u64)?;
            if aligned_end > entry.end() {
                continue;
            }

            self.freelist.swap_remove(i);

            // Split: anything below `aligned_start` returns to freelist as a
            // head remainder; anything above `aligned_end` returns as a tail
            // remainder. Each remainder is itself `min_alignment`-aligned on
            // both ends because `entry` and the returned range are.
            if aligned_start > entry.start {
                let head_len = (aligned_start - entry.start) as usize;
                self.freelist.push(IovaRange {
                    start: entry.start,
                    len: head_len,
                });
            }
            if aligned_end < entry.end() {
                let tail_len = (entry.end() - aligned_end) as usize;
                self.freelist.push(IovaRange {
                    start: aligned_end,
                    len: tail_len,
                });
            }

            return Some(IovaRange {
                start: aligned_start,
                len,
            });
        }
        None
    }

    /// Bump-allocate a new range past `cursor`, skipping any pre-reserved
    /// live ranges that sit on or past the cursor. Returns `Exhausted` if
    /// the bump path would cross `end`.
    fn bump(&mut self, len: usize, alignment: usize) -> Result<IovaRange, IovaError> {
        // Sort tracking: `live` is kept sorted by start, so scanning forward
        // past every overlap converges in O(live.len()).
        let mut cursor = self.cursor;
        loop {
            let aligned_start = align_up_u64(cursor, alignment as u64);
            if aligned_start < cursor {
                return Err(IovaError::Exhausted);
            }
            let Some(aligned_end) = aligned_start.checked_add(len as u64) else {
                return Err(IovaError::Exhausted);
            };
            if aligned_end > self.end {
                return Err(IovaError::Exhausted);
            }

            let candidate = IovaRange {
                start: aligned_start,
                len,
            };

            // If any live (reserved) range overlaps the candidate, jump the
            // cursor past it and retry. Because `live` is sorted by start and
            // reservations cannot shrink in place, this terminates.
            let overlap = self.live.iter().find(|r| r.overlaps(candidate)).copied();
            if let Some(overlap) = overlap {
                cursor = overlap.end();
                continue;
            }

            self.cursor = aligned_end;
            self.insert_live(candidate);
            return Ok(candidate);
        }
    }

    /// Insert into the sorted live set. Debug-asserts no overlap.
    fn insert_live(&mut self, range: IovaRange) {
        debug_assert!(
            !self.live.iter().any(|r| r.overlaps(range)),
            "IovaAllocator::insert_live: new range overlaps existing live range",
        );
        let pos = self
            .live
            .binary_search_by(|r| r.start.cmp(&range.start))
            .unwrap_or_else(|i| i);
        self.live.insert(pos, range);
    }
}

/// Round `value` up to the next multiple of `align`. `align` must be nonzero.
fn align_up_u64(value: u64, align: u64) -> u64 {
    debug_assert!(align > 0);
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .unwrap_or(value)
}

/// Round `value` up to the next multiple of `align`. `align` must be nonzero.
fn align_up_usize(value: usize, align: usize) -> usize {
    debug_assert!(align > 0);
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .unwrap_or(value)
}

// ---------------------------------------------------------------------------
// Tests
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
        assert!(!a.overlaps(b));
        assert!(!a.overlaps(c));
        assert!(!b.overlaps(c));
    }

    #[test]
    fn free_then_allocate_reuses_freed_range() {
        let mut alloc = fresh_allocator();
        let a = alloc.allocate(PAGE, PAGE).unwrap();
        let b = alloc.allocate(PAGE, PAGE).unwrap();
        // Bump past b, then free a. Next same-size alloc should take a.
        alloc.free(a).unwrap();
        let c = alloc.allocate(PAGE, PAGE).unwrap();
        assert_eq!(c, a, "allocator should reuse the freed range");
        // b is still live.
        assert_ne!(b, c);
    }

    #[test]
    fn alignment_larger_than_min_is_honored() {
        let mut alloc = fresh_allocator();
        // Burn a page so the bump cursor is at WINDOW_START + PAGE, which is
        // NOT 2 MiB-aligned. Next 2 MiB-aligned request must skip ahead.
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
        assert!(!small_a.overlaps(small_b));
        // Both small allocations come from the freed big range.
        assert!(small_a.start >= big.start && small_a.start + small_a.len as u64 <= big.end());
        assert!(small_b.start >= big.start && small_b.start + small_b.len as u64 <= big.end());
    }

    #[test]
    fn reserve_blocks_overlapping_allocation() {
        let mut alloc = fresh_allocator();
        // Reserve a region at the very start of the window.
        let reserved = IovaRange {
            start: WINDOW_START,
            len: 4 * PAGE,
        };
        alloc.reserve(reserved).unwrap();
        // First allocation must not overlap the reservation.
        let r = alloc.allocate(PAGE, PAGE).unwrap();
        assert!(!r.overlaps(reserved));
    }

    #[test]
    fn exhaustion_with_alignment_gap() {
        // Tight window whose start is NOT 2-MiB-aligned (offset by one page)
        // so a 2-MiB-aligned request cannot fit.
        let misaligned_start = WINDOW_START + PAGE_U64;
        let mut alloc = IovaAllocator::new(misaligned_start, misaligned_start + 2 * PAGE_U64, PAGE);
        let err = alloc.allocate(PAGE, 2 * 1024 * 1024).unwrap_err();
        assert_eq!(err, IovaError::Exhausted);
    }

    // ---------- Property tests ----------

    /// Strategy: generate a sequence of opcodes. Each opcode is either
    /// "allocate(len_pages, align_shift)" or "free(k)" where k selects the k-th
    /// currently-live allocation.
    #[derive(Debug, Clone)]
    enum Op {
        Alloc { len_pages: u32, align_shift: u8 },
        Free { index: u16 },
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (1u32..=8u32, 0u8..=3u8).prop_map(|(len_pages, align_shift)| Op::Alloc {
                len_pages,
                align_shift
            }),
            (0u16..=64u16).prop_map(|index| Op::Free { index }),
        ]
    }

    proptest! {
        #[test]
        fn no_overlap_across_arbitrary_sequences(ops in prop::collection::vec(op_strategy(), 1..200)) {
            // 256-page window = 1 MiB. Enough for the op lengths (<= 8 pages).
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
                                    prop_assert!(!r.overlaps(*existing));
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
            // Free every allocation.
            for r in &ranges {
                prop_assert!(alloc.free(*r).is_ok());
            }
            // Every freed range must be re-allocable (possibly at a different start)
            // for at least one same-size request.
            for r in &ranges {
                let realloc = alloc.allocate(r.len, PAGE);
                prop_assert!(realloc.is_ok(), "reallocation of {:?} failed", r);
                // Put it back so the next iteration has space.
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
            // Arbitrary free of a not-allocated range must error, never panic.
            let r = alloc.free(fake);
            prop_assert_eq!(r, Err(IovaError::DoubleFree));
        }
    }
}
