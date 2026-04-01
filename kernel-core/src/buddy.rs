//! Buddy frame allocator for managing physical page frames.
//!
//! Manages page-frame numbers (PFNs) using the classic buddy algorithm with
//! 10 order levels (0..=9).  Order 0 = 4 KiB, order 9 = 2 MiB.
//!
//! Design: pure data structure with no unsafe, no hardware access, fully
//! testable on the host via `cargo test -p kernel-core`.

use alloc::vec;
use alloc::vec::Vec;

/// Maximum allocation order.  Order 0 = 1 page (4 KiB), order 9 = 512 pages (2 MiB).
pub const MAX_ORDER: usize = 9;

/// Page size in bytes.
pub const PAGE_SIZE: usize = 4096;

/// Buddy frame allocator.
///
/// Each order level maintains:
/// - A bitmap where bit *i* is set when block *i* at that order is free.
/// - A free list (Vec used as a stack) of free block start PFNs.
///
/// The free list gives O(1) allocate/free; the bitmap enables O(1) buddy-merge
/// checks without scanning the free list.
pub struct BuddyAllocator {
    /// Per-order free lists: each entry is a PFN of a free block at that order.
    free_lists: [Vec<usize>; MAX_ORDER + 1],
    /// Per-order bitmaps: bit at `block_index(pfn, order)` is set iff that block
    /// is free.  Stored as Vec<u64> for efficient bit manipulation.
    bitmaps: [Vec<u64>; MAX_ORDER + 1],
    /// Per-order free block counts.
    free_counts: [usize; MAX_ORDER + 1],
    /// Total number of page frames managed.
    total_pages: usize,
}

impl BuddyAllocator {
    /// Create a new buddy allocator sized for `total_pages` page frames.
    ///
    /// All pages start as *allocated* (not free).  Call [`add_region`] or [`free`]
    /// to populate the free pool.
    pub fn new(total_pages: usize) -> Self {
        // For each order, we need ceil(total_pages / (1 << order)) bits,
        // stored in ceil(bits / 64) u64 words.
        let mut bitmaps: [Vec<u64>; MAX_ORDER + 1] = Default::default();
        for (order, bitmap) in bitmaps.iter_mut().enumerate() {
            let blocks = blocks_at_order(total_pages, order);
            let words = blocks.div_ceil(64);
            *bitmap = vec![0u64; words];
        }

        Self {
            free_lists: Default::default(),
            bitmaps,
            free_counts: [0; MAX_ORDER + 1],
            total_pages,
        }
    }

    /// Mark a contiguous region of pages as free, coalescing into the largest
    /// possible buddy blocks.
    ///
    /// `start_pfn` and `page_count` describe a range of page frames that should
    /// be added to the free pool.  Pages outside `0..total_pages` are silently
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

        // Find the smallest order >= requested that has free blocks.
        let mut found_order = None;
        for o in order..=MAX_ORDER {
            if self.free_counts[o] > 0 {
                found_order = Some(o);
                break;
            }
        }
        let source_order = found_order?;

        // Pop a block from the source order's free list.
        let pfn = self.pop_free(source_order)?;

        // Split down from source_order to the requested order.
        for o in (order..source_order).rev() {
            // The upper buddy at order `o` becomes free.
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
        // Alignment check: PFN must be aligned to block size.
        debug_assert!(
            pfn.is_multiple_of(1 << order),
            "BuddyAllocator::free: pfn {} not aligned to order {} (expected alignment {})",
            pfn,
            order,
            1usize << order,
        );
        // Double-free guard: if already marked free at this order, bail out.
        if self.is_free(order, pfn) {
            debug_assert!(
                false,
                "BuddyAllocator::free: double-free of pfn {} at order {}",
                pfn, order,
            );
            return;
        }
        // Bounds check: the block must fit within managed pages.
        let block_pages = 1usize << order;
        if pfn.saturating_add(block_pages) > self.total_pages {
            // Cannot merge further; just insert at current order if the PFN is valid.
            if pfn < self.total_pages {
                self.push_free(order, pfn);
            }
            return;
        }

        if order < MAX_ORDER {
            let buddy = pfn ^ (1 << order);
            // Check buddy is within bounds and free at this order.
            if buddy.saturating_add(block_pages) <= self.total_pages && self.is_free(order, buddy) {
                // Remove buddy from free list and merge.
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
        let idx = Self::block_index(pfn, order);
        let word = idx / 64;
        let bit = idx % 64;
        if word >= self.bitmaps[order].len() {
            return false;
        }
        self.bitmaps[order][word] & (1u64 << bit) != 0
    }

    /// Set the free bit for `(pfn, order)`.
    fn set_free(&mut self, order: usize, pfn: usize) {
        let idx = Self::block_index(pfn, order);
        let word = idx / 64;
        let bit = idx % 64;
        if word < self.bitmaps[order].len() {
            self.bitmaps[order][word] |= 1u64 << bit;
        }
    }

    /// Clear the free bit for `(pfn, order)`.
    fn clear_free(&mut self, order: usize, pfn: usize) {
        let idx = Self::block_index(pfn, order);
        let word = idx / 64;
        let bit = idx % 64;
        if word < self.bitmaps[order].len() {
            self.bitmaps[order][word] &= !(1u64 << bit);
        }
    }

    /// Push a PFN onto the free list at `order` and set its bitmap bit.
    fn push_free(&mut self, order: usize, pfn: usize) {
        self.set_free(order, pfn);
        self.free_lists[order].push(pfn);
        self.free_counts[order] += 1;
    }

    /// Pop a PFN from the free list at `order` and clear its bitmap bit.
    fn pop_free(&mut self, order: usize) -> Option<usize> {
        let pfn = self.free_lists[order].pop()?;
        self.clear_free(order, pfn);
        self.free_counts[order] -= 1;
        Some(pfn)
    }

    /// Remove a specific PFN from the free list at `order` and clear its bitmap bit.
    ///
    /// This is O(n) in the free list length, but buddy merges are infrequent
    /// relative to the list size.
    fn remove_free(&mut self, order: usize, pfn: usize) {
        if let Some(pos) = self.free_lists[order].iter().position(|&p| p == pfn) {
            self.clear_free(order, pfn);
            self.free_lists[order].swap_remove(pos);
            self.free_counts[order] -= 1;
        } else {
            debug_assert!(
                false,
                "BuddyAllocator::remove_free: pfn {} not found in free list for order {}",
                pfn, order,
            );
        }
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
        // Allocate all 8 pages.
        for _ in 0..8 {
            assert!(buddy.allocate(0).is_some());
        }
        // Should be exhausted.
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
        // Allocate 2 adjacent order-0 blocks from a 2-page region.
        // Freeing both should merge into one order-1 block.
        let mut buddy = BuddyAllocator::new(2);
        buddy.add_region(0, 2);
        // After add_region, 2 pages should merge into 1 order-1 block.
        let counts = buddy.free_count_by_order();
        assert_eq!(
            counts[1], 1,
            "expected 1 order-1 block after adding 2 pages"
        );
        assert_eq!(counts[0], 0);

        // Allocate order-0: should split the order-1 block.
        let p0 = buddy.allocate(0).unwrap();
        let p1 = buddy.allocate(0).unwrap();
        assert!(buddy.allocate(0).is_none());

        // Free both: should merge back to order-1.
        buddy.free(p0, 0);
        buddy.free(p1, 0);
        let counts = buddy.free_count_by_order();
        assert_eq!(counts[1], 1);
        assert_eq!(counts[0], 0);
    }

    #[test]
    fn splitting_higher_order() {
        // 4-page region: should coalesce to one order-2 block.
        let mut buddy = BuddyAllocator::new(4);
        buddy.add_region(0, 4);
        let counts = buddy.free_count_by_order();
        assert_eq!(counts[2], 1);
        assert_eq!(counts[1], 0);
        assert_eq!(counts[0], 0);

        // Allocate order-0: splits order-2 -> order-1 + order-0,
        // then order-0 is returned.
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

        // Alternating pattern: alloc 4, free 2, alloc 4, free 2, ...
        let mut allocated = Vec::new();
        for round in 0..4 {
            for _ in 0..4 {
                if let Some(pfn) = buddy.allocate(0) {
                    allocated.push(pfn);
                }
            }
            // Free the first 2 from this round.
            let start = round * 4;
            for i in start..start + 2 {
                if i < allocated.len() {
                    buddy.free(allocated[i], 0);
                }
            }
        }
        // We allocated 16, freed 8, so 8 should be free.
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
        // 5 pages: should get partial blocks without panicking.
        let mut buddy = BuddyAllocator::new(5);
        buddy.add_region(0, 5);
        assert_eq!(buddy.free_count(), 5);
        // Should be: 1 order-2 block (4 pages) + 1 order-0 block (1 page).
        let counts = buddy.free_count_by_order();
        assert_eq!(counts[2], 1);
        assert_eq!(counts[0], 1);
    }

    #[test]
    fn multi_order_allocation() {
        // 16 pages: order-4 block.
        let mut buddy = BuddyAllocator::new(16);
        buddy.add_region(0, 16);

        // Allocate order-2 (4 pages).
        let pfn = buddy.allocate(2).unwrap();
        assert_eq!(pfn % 4, 0); // Must be aligned to 4-page boundary.
        assert_eq!(buddy.free_count(), 12);

        // Free it back.
        buddy.free(pfn, 2);
        assert_eq!(buddy.free_count(), 16);
    }

    #[test]
    fn add_region_with_offset() {
        // Add a region starting at PFN 256 (simulating physical memory above 1 MiB).
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
}
