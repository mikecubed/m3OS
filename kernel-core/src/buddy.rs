//! Buddy frame allocator for managing physical page frames.
//!
//! Manages page-frame numbers (PFNs) using the classic buddy algorithm with
//! 10 order levels (0..=9). Order 0 = 4 KiB, order 9 = 2 MiB.
//!
//! Design: pure data structure with no unsafe, no hardware access, fully
//! testable on the host via `cargo test -p kernel-core`.

use alloc::vec;
use alloc::vec::Vec;
use core::array;

/// Maximum allocation order. Order 0 = 1 page (4 KiB), order 9 = 512 pages (2 MiB).
pub const MAX_ORDER: usize = 9;

/// Page size in bytes.
pub const PAGE_SIZE: usize = 4096;

/// Hierarchical bitmap used to track free blocks at a single order.
///
/// Level 0 stores one bit per block. Higher levels summarize the lower level:
/// bit `i` at level `n + 1` is set when word `i` at level `n` is non-zero. This
/// keeps removal allocation-free while still letting us find a free block
/// without scanning every leaf word.
#[derive(Default)]
struct SummaryBitmap {
    levels: Vec<Vec<u64>>,
    bits: usize,
}

impl SummaryBitmap {
    fn new(bits: usize) -> Self {
        if bits == 0 {
            return Self {
                levels: vec![Vec::new()],
                bits,
            };
        }

        let mut levels = Vec::new();
        let mut level_bits = bits;
        loop {
            let words = level_bits.div_ceil(64);
            levels.push(vec![0u64; words]);
            if words <= 1 {
                break;
            }
            level_bits = words;
        }

        Self { levels, bits }
    }

    fn contains(&self, bit_index: usize) -> bool {
        if bit_index >= self.bits || self.levels[0].is_empty() {
            return false;
        }

        let word = bit_index / 64;
        let bit = bit_index % 64;
        self.levels[0][word] & (1u64 << bit) != 0
    }

    fn set(&mut self, bit_index: usize) {
        if bit_index >= self.bits || self.levels[0].is_empty() {
            return;
        }

        let mut entry = bit_index;
        for level in 0..self.levels.len() {
            let word_index = entry / 64;
            let bit = entry % 64;
            let word = &mut self.levels[level][word_index];
            let was_zero = *word == 0;
            *word |= 1u64 << bit;
            if !was_zero {
                break;
            }
            entry = word_index;
        }
    }

    fn clear(&mut self, bit_index: usize) {
        if bit_index >= self.bits || self.levels[0].is_empty() {
            return;
        }

        let mut entry = bit_index;
        for level in 0..self.levels.len() {
            let word_index = entry / 64;
            let bit = entry % 64;
            let word = &mut self.levels[level][word_index];
            let before = *word;
            *word &= !(1u64 << bit);
            if before == *word || *word != 0 {
                break;
            }
            entry = word_index;
        }
    }

    fn first_set(&self) -> Option<usize> {
        if self.bits == 0 || self.levels[0].is_empty() {
            return None;
        }

        let top = self.levels.last()?.first().copied()?;
        if top == 0 {
            return None;
        }

        let mut word_index = 0usize;
        for level in (1..self.levels.len()).rev() {
            let word = self.levels[level][word_index];
            debug_assert_ne!(
                word, 0,
                "SummaryBitmap::first_set: summary level {} is out of sync",
                level
            );
            let bit = word.trailing_zeros() as usize;
            word_index = word_index * 64 + bit;
        }

        let leaf = self.levels[0][word_index];
        debug_assert_ne!(
            leaf, 0,
            "SummaryBitmap::first_set: leaf summary is out of sync"
        );
        let bit = leaf.trailing_zeros() as usize;
        let bit_index = word_index * 64 + bit;
        debug_assert!(
            bit_index < self.bits,
            "SummaryBitmap::first_set returned out-of-range bit {} (bits={})",
            bit_index,
            self.bits
        );
        (bit_index < self.bits).then_some(bit_index)
    }
}

/// Buddy frame allocator.
///
/// Each order level maintains:
/// - A leaf bitmap where bit *i* is set when block *i* at that order is free.
/// - Compact summary bitmaps that make "find any free block" fast without a
///   max-PFN-sized position vector.
///
/// Free-list removal is therefore O(1): clearing a buddy block touches only the
/// leaf bit and, at most, one word per summary level. Overall allocation and
/// free may traverse/split/merge across orders up to MAX_ORDER, giving O(log n)
/// behavior bounded by MAX_ORDER + 1 levels.
///
/// **Heap/bootstrap note:** All bitmap storage is allocated once in `new()` and
/// never grows afterwards, so `free()` and buddy coalescing do not allocate.
pub struct BuddyAllocator {
    /// Per-order hierarchical bitmaps. Leaf bit at `block_index(pfn, order)` is
    /// set iff that block is free.
    free_maps: [SummaryBitmap; MAX_ORDER + 1],
    /// Per-order free block counts.
    free_counts: [usize; MAX_ORDER + 1],
    /// Total number of page frames managed.
    total_pages: usize,
}

impl BuddyAllocator {
    /// Create a new buddy allocator sized for `total_pages` page frames.
    ///
    /// All pages start as *allocated* (not free). Call [`add_region`] or [`free`]
    /// to populate the free pool.
    pub fn new(total_pages: usize) -> Self {
        let free_maps =
            array::from_fn(|order| SummaryBitmap::new(blocks_at_order(total_pages, order)));
        Self {
            free_maps,
            free_counts: [0; MAX_ORDER + 1],
            total_pages,
        }
    }

    /// Mark a contiguous region of pages as free, coalescing into the largest
    /// possible buddy blocks.
    ///
    /// `start_pfn` and `page_count` describe a range of page frames that should
    /// be added to the free pool. Pages outside `0..total_pages` are silently
    /// skipped.
    pub fn add_region(&mut self, start_pfn: usize, page_count: usize) {
        let end_pfn = start_pfn.saturating_add(page_count).min(self.total_pages);
        let start_pfn = start_pfn.min(self.total_pages);
        for pfn in start_pfn..end_pfn {
            self.free(pfn, 0);
        }
    }

    /// Allocate a block of `1 << order` contiguous pages.
    ///
    /// Returns the start PFN of the allocated block, or `None` if no block of
    /// sufficient size is available.
    pub fn allocate(&mut self, order: usize) -> Option<usize> {
        if order > MAX_ORDER {
            return None;
        }

        let mut source_order = None;
        for o in order..=MAX_ORDER {
            if self.free_counts[o] > 0 {
                source_order = Some(o);
                break;
            }
        }
        let source_order = source_order?;

        let pfn = self.pop_free(source_order)?;
        for o in (order..source_order).rev() {
            let buddy = pfn ^ (1 << o);
            self.push_free(o, buddy);
        }

        Some(pfn)
    }

    /// Return a block of `1 << order` contiguous pages starting at `pfn`.
    ///
    /// The allocator will attempt to merge with the buddy recursively up to
    /// `MAX_ORDER`.
    pub fn free(&mut self, pfn: usize, order: usize) {
        if order > MAX_ORDER {
            return;
        }

        debug_assert!(
            pfn.is_multiple_of(1 << order),
            "BuddyAllocator::free: pfn {} not aligned to order {} (expected alignment {})",
            pfn,
            order,
            1usize << order,
        );

        if self.is_free(order, pfn) {
            debug_assert!(
                false,
                "BuddyAllocator::free: double-free of pfn {} at order {}",
                pfn, order,
            );
            return;
        }

        let block_pages = 1usize << order;
        if pfn.saturating_add(block_pages) > self.total_pages {
            if pfn < self.total_pages {
                self.push_free(order, pfn);
            }
            return;
        }

        if order < MAX_ORDER {
            let buddy = pfn ^ (1 << order);
            if buddy.saturating_add(block_pages) <= self.total_pages && self.is_free(order, buddy) {
                self.remove_free(order, buddy);
                let parent = pfn & !(1 << order);
                self.free(parent, order + 1);
                return;
            }
        }

        self.push_free(order, pfn);
    }

    /// Total number of free pages across all orders.
    pub fn free_count(&self) -> usize {
        let mut total = 0;
        for order in 0..=MAX_ORDER {
            total += self.free_counts[order] * (1 << order);
        }
        total
    }

    /// Per-order free block counts.
    pub fn free_count_by_order(&self) -> [usize; MAX_ORDER + 1] {
        self.free_counts
    }

    // ---- internal helpers ----

    /// Block index for a PFN at a given order.
    fn block_index(pfn: usize, order: usize) -> usize {
        pfn >> order
    }

    /// Test whether the block at `(pfn, order)` is marked free in the bitmap.
    fn is_free(&self, order: usize, pfn: usize) -> bool {
        self.free_maps[order].contains(Self::block_index(pfn, order))
    }

    /// Set the free bit for `(pfn, order)`.
    fn set_free(&mut self, order: usize, pfn: usize) {
        self.free_maps[order].set(Self::block_index(pfn, order));
    }

    /// Clear the free bit for `(pfn, order)`.
    fn clear_free(&mut self, order: usize, pfn: usize) {
        self.free_maps[order].clear(Self::block_index(pfn, order));
    }

    /// Mark a block free at `order`.
    fn push_free(&mut self, order: usize, pfn: usize) {
        self.set_free(order, pfn);
        self.free_counts[order] += 1;
    }

    /// Remove and return any free block from `order`.
    fn pop_free(&mut self, order: usize) -> Option<usize> {
        let block_index = self.free_maps[order].first_set()?;
        self.free_maps[order].clear(block_index);
        self.free_counts[order] -= 1;
        Some(block_index << order)
    }

    /// Remove a specific PFN from the free pool at `order` in O(1) by clearing
    /// its leaf bit plus the affected summary words.
    fn remove_free(&mut self, order: usize, pfn: usize) {
        if !self.is_free(order, pfn) {
            debug_assert!(
                false,
                "BuddyAllocator::remove_free: pfn {} not found in free map for order {}",
                pfn, order,
            );
            return;
        }
        self.clear_free(order, pfn);
        self.free_counts[order] -= 1;
    }
}

/// Number of blocks at a given order for a total page count.
fn blocks_at_order(total_pages: usize, order: usize) -> usize {
    (total_pages + (1 << order) - 1) >> order
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_single_page() {
        let mut buddy = BuddyAllocator::new(16);
        buddy.add_region(0, 16);
        let pfn = buddy.allocate(0);
        assert!(pfn.is_some());
        assert!(pfn.unwrap() < 16);
    }

    #[test]
    fn free_single_page_reusable() {
        let mut buddy = BuddyAllocator::new(4);
        buddy.add_region(0, 4);
        let pfn = buddy.allocate(0).unwrap();
        buddy.free(pfn, 0);
        let pfn2 = buddy.allocate(0);
        assert!(pfn2.is_some());
    }

    #[test]
    fn allocate_all_pages_exhaustion() {
        let mut buddy = BuddyAllocator::new(8);
        buddy.add_region(0, 8);
        for _ in 0..8 {
            assert!(buddy.allocate(0).is_some());
        }
        assert!(buddy.allocate(0).is_none());
        assert_eq!(buddy.free_count(), 0);
    }

    #[test]
    fn free_all_pages_available() {
        let mut buddy = BuddyAllocator::new(8);
        buddy.add_region(0, 8);
        let mut pages = Vec::new();
        for _ in 0..8 {
            pages.push(buddy.allocate(0).unwrap());
        }
        for pfn in pages {
            buddy.free(pfn, 0);
        }
        assert_eq!(buddy.free_count(), 8);
    }

    #[test]
    fn buddy_merging() {
        let mut buddy = BuddyAllocator::new(2);
        buddy.add_region(0, 2);
        let counts = buddy.free_count_by_order();
        assert_eq!(
            counts[1], 1,
            "expected 1 order-1 block after adding 2 pages"
        );
        assert_eq!(counts[0], 0);

        let p0 = buddy.allocate(0).unwrap();
        let p1 = buddy.allocate(0).unwrap();
        assert!(buddy.allocate(0).is_none());

        buddy.free(p0, 0);
        buddy.free(p1, 0);
        let counts = buddy.free_count_by_order();
        assert_eq!(counts[1], 1);
        assert_eq!(counts[0], 0);
    }

    #[test]
    fn splitting_higher_order() {
        let mut buddy = BuddyAllocator::new(4);
        buddy.add_region(0, 4);
        let counts = buddy.free_count_by_order();
        assert_eq!(counts[2], 1);
        assert_eq!(counts[1], 0);
        assert_eq!(counts[0], 0);

        let pfn = buddy.allocate(0).unwrap();
        assert!(pfn < 4);
        assert_eq!(buddy.free_count(), 3);
    }

    #[test]
    fn alternating_alloc_free() {
        let mut buddy = BuddyAllocator::new(16);
        buddy.add_region(0, 16);
        let initial = buddy.free_count();
        assert_eq!(initial, 16);

        let mut allocated = Vec::new();
        for round in 0..4 {
            for _ in 0..4 {
                if let Some(pfn) = buddy.allocate(0) {
                    allocated.push(pfn);
                }
            }
            let start = round * 4;
            for i in start..start + 2 {
                if i < allocated.len() {
                    buddy.free(allocated[i], 0);
                }
            }
        }
        assert_eq!(buddy.free_count(), 8);
    }

    #[test]
    fn single_page_region() {
        let mut buddy = BuddyAllocator::new(1);
        buddy.add_region(0, 1);
        assert_eq!(buddy.free_count(), 1);
        let pfn = buddy.allocate(0).unwrap();
        assert_eq!(pfn, 0);
        assert_eq!(buddy.free_count(), 0);
        assert!(buddy.allocate(0).is_none());
        buddy.free(pfn, 0);
        assert_eq!(buddy.free_count(), 1);
    }

    #[test]
    fn non_power_of_two_region() {
        let mut buddy = BuddyAllocator::new(5);
        buddy.add_region(0, 5);
        assert_eq!(buddy.free_count(), 5);
        let counts = buddy.free_count_by_order();
        assert_eq!(counts[2], 1);
        assert_eq!(counts[0], 1);
    }

    #[test]
    fn multi_order_allocation() {
        let mut buddy = BuddyAllocator::new(16);
        buddy.add_region(0, 16);

        let pfn = buddy.allocate(2).unwrap();
        assert_eq!(pfn % 4, 0);
        assert_eq!(buddy.free_count(), 12);

        buddy.free(pfn, 2);
        assert_eq!(buddy.free_count(), 16);
    }

    #[test]
    fn add_region_with_offset() {
        let mut buddy = BuddyAllocator::new(512);
        buddy.add_region(256, 128);
        assert_eq!(buddy.free_count(), 128);

        let pfn = buddy.allocate(0).unwrap();
        assert!(pfn >= 256 && pfn < 384);
    }

    #[test]
    fn multiple_regions() {
        let mut buddy = BuddyAllocator::new(1024);
        buddy.add_region(0, 64);
        buddy.add_region(512, 64);
        assert_eq!(buddy.free_count(), 128);
    }

    #[test]
    fn order_too_large_returns_none() {
        let mut buddy = BuddyAllocator::new(16);
        buddy.add_region(0, 16);
        assert!(buddy.allocate(MAX_ORDER + 1).is_none());
    }

    // ----- D.2 tests: O(1) removal representation -----

    /// Freeing buddies in reverse allocation order forces `remove_free` on every
    /// merge. After all frees the region must fully coalesce.
    #[test]
    fn coalesce_chain_via_reverse_free() {
        let mut buddy = BuddyAllocator::new(8);
        buddy.add_region(0, 8);
        assert_eq!(buddy.free_count_by_order()[3], 1);

        let mut pages: Vec<usize> = (0..8).map(|_| buddy.allocate(0).unwrap()).collect();
        assert_eq!(buddy.free_count(), 0);

        pages.reverse();
        for pfn in &pages {
            buddy.free(*pfn, 0);
        }
        assert_eq!(buddy.free_count(), 8);
        assert_eq!(buddy.free_count_by_order()[3], 1);
    }

    /// Interleaved alloc/free across multiple orders must keep the free-state
    /// metadata consistent while `remove_free` clears merged buddies directly.
    #[test]
    fn interleaved_multi_order_remove_consistency() {
        let mut buddy = BuddyAllocator::new(32);
        buddy.add_region(0, 32);
        assert_eq!(buddy.free_count(), 32);

        let a0 = buddy.allocate(0).unwrap();
        let a1 = buddy.allocate(1).unwrap();
        let a2 = buddy.allocate(2).unwrap();
        let a3 = buddy.allocate(3).unwrap();
        assert_eq!(buddy.free_count(), 32 - 1 - 2 - 4 - 8);

        buddy.free(a2, 2);
        buddy.free(a0, 0);
        buddy.free(a3, 3);
        buddy.free(a1, 1);

        assert_eq!(buddy.free_count(), 32);
        assert_eq!(buddy.free_count_by_order()[5], 1);
    }

    /// Stress: 256 pages, allocate all order-0, free in butterfly (even then
    /// odd) order to maximise coalesce remove_free calls. Free count must stay
    /// consistent throughout.
    #[test]
    fn stress_butterfly_free_pattern() {
        let n = 256;
        let mut buddy = BuddyAllocator::new(n);
        buddy.add_region(0, n);

        let mut pages: Vec<usize> = (0..n).map(|_| buddy.allocate(0).unwrap()).collect();
        assert_eq!(buddy.free_count(), 0);

        pages.sort();

        let mut freed = 0usize;
        for pfn in pages.iter().filter(|p| **p % 2 == 0) {
            buddy.free(*pfn, 0);
            freed += 1;
            assert_eq!(buddy.free_count(), freed);
        }
        for pfn in pages.iter().filter(|p| **p % 2 == 1) {
            buddy.free(*pfn, 0);
            freed += 1;
            assert_eq!(buddy.free_count(), freed);
        }
        assert_eq!(buddy.free_count(), n);
    }

    /// Targeted: allocate two buddy pairs, free only one of each pair, verify
    /// no spurious merges, then free the remaining halves and verify merge.
    #[test]
    fn partial_buddy_pair_no_spurious_merge() {
        let mut buddy = BuddyAllocator::new(4);
        buddy.add_region(0, 4);

        let p0 = buddy.allocate(0).unwrap();
        let p1 = buddy.allocate(0).unwrap();
        let p2 = buddy.allocate(0).unwrap();
        let p3 = buddy.allocate(0).unwrap();
        assert_eq!(buddy.free_count(), 0);

        let mut pfns = [p0, p1, p2, p3];
        pfns.sort();

        buddy.free(pfns[0], 0);
        assert_eq!(buddy.free_count(), 1);
        assert_eq!(buddy.free_count_by_order()[0], 1);

        buddy.free(pfns[2], 0);
        assert_eq!(buddy.free_count(), 2);
        assert_eq!(buddy.free_count_by_order()[0], 2);

        buddy.free(pfns[1], 0);
        assert_eq!(buddy.free_count(), 3);
        assert_eq!(buddy.free_count_by_order()[1], 1);
        assert_eq!(buddy.free_count_by_order()[0], 1);

        buddy.free(pfns[3], 0);
        assert_eq!(buddy.free_count(), 4);
        assert_eq!(buddy.free_count_by_order()[2], 1);
    }

    /// Repeated split/merge churn must keep the allocator stable without any
    /// auxiliary free-list metadata growing across cycles.
    #[test]
    fn repeated_split_merge_cycles_round_trip() {
        let mut buddy = BuddyAllocator::new(2);
        buddy.add_region(0, 2);

        for _ in 0..1024 {
            let a = buddy.allocate(0).unwrap();
            let b = buddy.allocate(0).unwrap();
            assert!(buddy.allocate(0).is_none());

            buddy.free(a, 0);
            buddy.free(b, 0);

            assert_eq!(buddy.free_count(), 2);
            assert_eq!(buddy.free_count_by_order()[1], 1);
        }
    }

    #[test]
    fn sparse_high_pfn_region_round_trips() {
        let mut buddy = BuddyAllocator::new(1 << 24);
        let high_pfn = (1 << 24) - 1;

        buddy.free(high_pfn, 0);
        assert!(buddy.is_free(0, high_pfn));
        assert_eq!(buddy.allocate(0), Some(high_pfn));
    }
}
