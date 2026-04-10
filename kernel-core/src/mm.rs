//! Memory management data structures, host-testable.
//!
//! `VmaTree` replaces the previous `Vec<MemoryMapping>` linear scan with
//! a `BTreeMap<u64, MemoryMapping>` for O(log n) VMA lookup by address.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Describes a contiguous virtual memory area (VMA).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryMapping {
    /// Starting virtual address (page-aligned).
    pub start: u64,
    /// Length in bytes (page-aligned).
    pub len: u64,
    /// Protection bits (`PROT_READ | PROT_WRITE | PROT_EXEC`).
    pub prot: u64,
    /// Mapping flags (`MAP_PRIVATE | MAP_ANONYMOUS`, etc.).
    pub flags: u64,
}

/// VMA tree for O(log n) address lookup, backed by `BTreeMap`.
///
/// Keyed by the starting virtual address of each mapping.
pub struct VmaTree {
    tree: BTreeMap<u64, MemoryMapping>,
}

impl VmaTree {
    /// Create an empty VMA tree.
    pub fn new() -> Self {
        VmaTree {
            tree: BTreeMap::new(),
        }
    }

    /// Find the VMA containing `addr`. O(log n).
    pub fn find_containing(&self, addr: u64) -> Option<&MemoryMapping> {
        // Find the last entry with start <= addr, then check if addr < start + len.
        self.tree
            .range(..=addr)
            .next_back()
            .map(|(_, vma)| vma)
            .filter(|vma| addr < vma.start.saturating_add(vma.len))
    }

    /// Find mutable VMA containing `addr`. O(log n).
    pub fn find_containing_mut(&mut self, addr: u64) -> Option<&mut MemoryMapping> {
        self.tree
            .range_mut(..=addr)
            .next_back()
            .map(|(_, vma)| vma)
            .filter(|vma| addr < vma.start.saturating_add(vma.len))
    }

    /// Insert a new VMA. If a VMA already exists at `mapping.start`, it is
    /// replaced.
    pub fn insert(&mut self, mapping: MemoryMapping) {
        self.tree.insert(mapping.start, mapping);
    }

    /// Remove the VMA starting at exactly `start`. Returns it if found.
    pub fn remove(&mut self, start: u64) -> Option<MemoryMapping> {
        self.tree.remove(&start)
    }

    /// Remove all VMAs overlapping `[start, start+len)`.
    ///
    /// Partially overlapping VMAs are split at the boundaries so that only
    /// the `[start, start+len)` portion is removed. Returns the removed
    /// (or excised) portions.
    pub fn remove_range(&mut self, start: u64, len: u64) -> Vec<MemoryMapping> {
        let end = start.saturating_add(len);
        let mut removed = Vec::new();
        let mut to_remove = Vec::new();
        let mut to_insert = Vec::new();

        // Find all VMAs that could overlap [start, end).
        // A VMA at key `k` overlaps if k < end AND k + vma.len > start.
        for (&vma_start, vma) in self.tree.range(..end) {
            let vma_end = vma_start.saturating_add(vma.len);
            if vma_end <= start {
                continue; // VMA entirely before range
            }
            // VMA overlaps the range.
            if vma_start >= start && vma_end <= end {
                // Fully contained -- remove entirely.
                to_remove.push(vma_start);
                removed.push(vma.clone());
            } else if vma_start < start && vma_end > end {
                // VMA spans the entire range -- split into two pieces.
                to_remove.push(vma_start);
                // Left piece: [vma_start, start)
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                });
                // Right piece: [end, vma_end)
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                });
                removed.push(MemoryMapping {
                    start,
                    len,
                    prot: vma.prot,
                    flags: vma.flags,
                });
            } else if vma_start < start {
                // VMA overlaps on the left -- trim right side.
                to_remove.push(vma_start);
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                });
                removed.push(MemoryMapping {
                    start,
                    len: vma_end - start,
                    prot: vma.prot,
                    flags: vma.flags,
                });
            } else {
                // VMA overlaps on the right -- trim left side.
                to_remove.push(vma_start);
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                });
                removed.push(MemoryMapping {
                    start: vma_start,
                    len: end - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                });
            }
        }

        for key in to_remove {
            self.tree.remove(&key);
        }
        for vma in to_insert {
            self.tree.insert(vma.start, vma);
        }
        removed
    }

    /// Update protection bits for all VMAs overlapping `[start, start+len)`.
    ///
    /// Partially overlapping VMAs are split at the boundaries so that only
    /// the overlapping portion gets the new `prot` value.
    pub fn update_range_prot(&mut self, start: u64, len: u64, prot: u64) {
        let end = start.saturating_add(len);
        let mut to_remove = Vec::new();
        let mut to_insert = Vec::new();

        for (&vma_start, vma) in self.tree.range(..end) {
            let vma_end = vma_start.saturating_add(vma.len);
            if vma_end <= start {
                continue; // No overlap
            }
            if vma_start >= start && vma_end <= end {
                // Fully contained -- just update prot in place (collected for later).
                to_remove.push((vma_start, true, vma.clone()));
            } else if vma_start < start && vma_end > end {
                // Middle split: head (old prot) + middle (new prot) + tail (old prot).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                });
                to_insert.push(MemoryMapping {
                    start,
                    len: end - start,
                    prot,
                    flags: vma.flags,
                });
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                });
            } else if vma_start < start {
                // Overlap at tail of VMA -- split into head (old) + tail (new).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                });
                to_insert.push(MemoryMapping {
                    start,
                    len: vma_end - start,
                    prot,
                    flags: vma.flags,
                });
            } else {
                // Overlap at head of VMA -- split into head (new) + tail (old).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: end - vma_start,
                    prot,
                    flags: vma.flags,
                });
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                });
            }
        }

        for (key, just_update_prot, original) in to_remove {
            self.tree.remove(&key);
            if just_update_prot {
                // Re-insert with new prot.
                self.tree.insert(key, MemoryMapping { prot, ..original });
            }
        }
        for vma in to_insert {
            self.tree.insert(vma.start, vma);
        }
    }

    /// Check whether any VMA satisfies the predicate.
    pub fn any<F: Fn(&MemoryMapping) -> bool>(&self, f: F) -> bool {
        self.tree.values().any(f)
    }

    /// Iterate over all VMAs in address order.
    pub fn iter(&self) -> impl Iterator<Item = &MemoryMapping> {
        self.tree.values()
    }

    /// Clear all VMAs.
    pub fn clear(&mut self) {
        self.tree.clear();
    }

    /// Number of VMAs.
    pub fn len(&self) -> usize {
        self.tree.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }
}

impl Default for VmaTree {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for VmaTree {
    fn clone(&self) -> Self {
        VmaTree {
            tree: self.tree.clone(),
        }
    }
}

// -----------------------------------------------------------------------
// Unit tests (host-testable via `cargo test -p kernel-core`)
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(start: u64, len: u64) -> MemoryMapping {
        MemoryMapping {
            start,
            len,
            prot: 3,
            flags: 0x22,
        }
    }

    // -- find_containing --------------------------------------------------

    #[test]
    fn find_containing_hit() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x3000));
        assert!(t.find_containing(0x1000).is_some());
        assert!(t.find_containing(0x2000).is_some());
        assert!(t.find_containing(0x3FFF).is_some());
    }

    #[test]
    fn find_containing_miss() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x3000));
        assert!(t.find_containing(0x0FFF).is_none());
        assert!(t.find_containing(0x4000).is_none());
        assert!(t.find_containing(0x5000).is_none());
    }

    #[test]
    fn find_containing_boundary() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        // Exactly at start -- inside.
        assert!(t.find_containing(0x1000).is_some());
        // One byte before end -- inside.
        assert!(t.find_containing(0x1FFF).is_some());
        // Exactly at end -- outside (half-open interval).
        assert!(t.find_containing(0x2000).is_none());
    }

    #[test]
    fn find_containing_empty_tree() {
        let t = VmaTree::new();
        assert!(t.find_containing(0x1000).is_none());
    }

    #[test]
    fn find_containing_multiple_vmas() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        t.insert(mapping(0x3000, 0x2000));
        t.insert(mapping(0x6000, 0x1000));
        // In the gap between first and second.
        assert!(t.find_containing(0x2500).is_none());
        // In the second VMA.
        assert_eq!(t.find_containing(0x4000).unwrap().start, 0x3000);
        // In the third VMA.
        assert_eq!(t.find_containing(0x6500).unwrap().start, 0x6000);
    }

    // -- find_containing_mut ----------------------------------------------

    #[test]
    fn find_containing_mut_modifies() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        if let Some(vma) = t.find_containing_mut(0x1500) {
            vma.prot = 7;
        }
        assert_eq!(t.find_containing(0x1500).unwrap().prot, 7);
    }

    // -- insert + remove --------------------------------------------------

    #[test]
    fn insert_and_remove() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        assert_eq!(t.len(), 1);
        let removed = t.remove(0x1000);
        assert!(removed.is_some());
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn remove_nonexistent() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        assert!(t.remove(0x2000).is_none());
        assert_eq!(t.len(), 1);
    }

    // -- remove_range: full overlap ---------------------------------------

    #[test]
    fn remove_range_full_overlap() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x2000, 0x1000));
        let removed = t.remove_range(0x2000, 0x1000);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].start, 0x2000);
        assert!(t.is_empty());
    }

    #[test]
    fn remove_range_full_overlap_superset() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x2000, 0x1000));
        let removed = t.remove_range(0x1000, 0x3000);
        assert_eq!(removed.len(), 1);
        assert!(t.is_empty());
    }

    // -- remove_range: partial left overlap -------------------------------

    #[test]
    fn remove_range_partial_left() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x3000)); // [0x1000, 0x4000)
        let removed = t.remove_range(0x1000, 0x1000); // remove [0x1000, 0x2000)
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].start, 0x1000);
        assert_eq!(removed[0].len, 0x1000);
        // Remaining: [0x2000, 0x4000)
        assert_eq!(t.len(), 1);
        let remaining = t.find_containing(0x2000).unwrap();
        assert_eq!(remaining.start, 0x2000);
        assert_eq!(remaining.len, 0x2000);
    }

    // -- remove_range: partial right overlap ------------------------------

    #[test]
    fn remove_range_partial_right() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x3000)); // [0x1000, 0x4000)
        let removed = t.remove_range(0x3000, 0x2000); // remove [0x3000, 0x5000)
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].start, 0x3000);
        assert_eq!(removed[0].len, 0x1000);
        // Remaining: [0x1000, 0x3000)
        assert_eq!(t.len(), 1);
        let remaining = t.find_containing(0x1000).unwrap();
        assert_eq!(remaining.start, 0x1000);
        assert_eq!(remaining.len, 0x2000);
    }

    // -- remove_range: spanning split (hole punch) ------------------------

    #[test]
    fn remove_range_hole_punch() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x4000)); // [0x1000, 0x5000)
        let removed = t.remove_range(0x2000, 0x1000); // remove [0x2000, 0x3000)
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].start, 0x2000);
        assert_eq!(removed[0].len, 0x1000);
        // Remaining: [0x1000, 0x2000) and [0x3000, 0x5000)
        assert_eq!(t.len(), 2);
        let left = t.find_containing(0x1000).unwrap();
        assert_eq!(left.start, 0x1000);
        assert_eq!(left.len, 0x1000);
        let right = t.find_containing(0x3000).unwrap();
        assert_eq!(right.start, 0x3000);
        assert_eq!(right.len, 0x2000);
        // Gap region should be empty.
        assert!(t.find_containing(0x2500).is_none());
    }

    // -- remove_range: multiple VMAs --------------------------------------

    #[test]
    fn remove_range_multiple_vmas() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        t.insert(mapping(0x2000, 0x1000));
        t.insert(mapping(0x3000, 0x1000));
        let removed = t.remove_range(0x1000, 0x3000);
        assert_eq!(removed.len(), 3);
        assert!(t.is_empty());
    }

    // -- remove_range: no overlap -----------------------------------------

    #[test]
    fn remove_range_no_overlap() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        let removed = t.remove_range(0x5000, 0x1000);
        assert!(removed.is_empty());
        assert_eq!(t.len(), 1);
    }

    // -- update_range_prot ------------------------------------------------

    #[test]
    fn update_range_prot_fully_contained() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x3000));
        t.update_range_prot(0x1000, 0x3000, 7);
        assert_eq!(t.find_containing(0x1000).unwrap().prot, 7);
    }

    #[test]
    fn update_range_prot_middle_split() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x4000)); // [0x1000, 0x5000) prot=3
        t.update_range_prot(0x2000, 0x1000, 7); // change [0x2000, 0x3000) to prot=7
        assert_eq!(t.len(), 3);
        // Head: [0x1000, 0x2000) prot=3
        let head = t.find_containing(0x1000).unwrap();
        assert_eq!(head.start, 0x1000);
        assert_eq!(head.len, 0x1000);
        assert_eq!(head.prot, 3);
        // Middle: [0x2000, 0x3000) prot=7
        let mid = t.find_containing(0x2000).unwrap();
        assert_eq!(mid.start, 0x2000);
        assert_eq!(mid.len, 0x1000);
        assert_eq!(mid.prot, 7);
        // Tail: [0x3000, 0x5000) prot=3
        let tail = t.find_containing(0x3000).unwrap();
        assert_eq!(tail.start, 0x3000);
        assert_eq!(tail.len, 0x2000);
        assert_eq!(tail.prot, 3);
    }

    #[test]
    fn update_range_prot_tail_split() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x3000)); // [0x1000, 0x4000)
        t.update_range_prot(0x2000, 0x2000, 7); // change [0x2000, 0x4000)
        assert_eq!(t.len(), 2);
        assert_eq!(t.find_containing(0x1000).unwrap().prot, 3);
        assert_eq!(t.find_containing(0x2000).unwrap().prot, 7);
    }

    #[test]
    fn update_range_prot_head_split() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x2000, 0x3000)); // [0x2000, 0x5000)
        t.update_range_prot(0x2000, 0x1000, 7); // change [0x2000, 0x3000)
        assert_eq!(t.len(), 2);
        assert_eq!(t.find_containing(0x2000).unwrap().prot, 7);
        assert_eq!(t.find_containing(0x3000).unwrap().prot, 3);
    }

    // -- clear / len / is_empty -------------------------------------------

    #[test]
    fn clear_and_len() {
        let mut t = VmaTree::new();
        assert!(t.is_empty());
        t.insert(mapping(0x1000, 0x1000));
        t.insert(mapping(0x2000, 0x1000));
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
        t.clear();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }

    // -- clone ------------------------------------------------------------

    #[test]
    fn clone_is_independent() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x1000, 0x1000));
        let mut t2 = t.clone();
        t2.insert(mapping(0x2000, 0x1000));
        assert_eq!(t.len(), 1);
        assert_eq!(t2.len(), 2);
    }

    // -- iter -------------------------------------------------------------

    #[test]
    fn iter_in_address_order() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x3000, 0x1000));
        t.insert(mapping(0x1000, 0x1000));
        t.insert(mapping(0x2000, 0x1000));
        let starts: Vec<u64> = t.iter().map(|m| m.start).collect();
        assert_eq!(starts, vec![0x1000, 0x2000, 0x3000]);
    }

    // -- any --------------------------------------------------------------

    #[test]
    fn any_predicate() {
        let mut t = VmaTree::new();
        t.insert(MemoryMapping {
            start: 0x1000,
            len: 0x1000,
            prot: 3,
            flags: 0x100,
        });
        t.insert(mapping(0x2000, 0x1000));
        assert!(t.any(|m| m.flags & 0x100 != 0));
        assert!(!t.any(|m| m.flags & 0x200 != 0));
    }
}
