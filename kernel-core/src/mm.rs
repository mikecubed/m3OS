//! Memory management data structures, host-testable.
//!
//! `VmaTree` replaces the previous `Vec<MemoryMapping>` linear scan with
//! a `BTreeMap<u64, MemoryMapping>` for O(log n) VMA lookup by address.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Kernel-internal `mmap` flag (bit 32, above every POSIX 32-bit `MAP_*` flag):
/// request a **lazy, demand-paged file-backed** `MAP_PRIVATE` mapping (Phase
/// 95b). Only the `ld-musl` loader sets it, on each `PT_LOAD` it maps, and it
/// keeps the backing fd open for the mapping's lifetime so the page-fault
/// handler can read a faulting page straight from the file. A plain (flag-absent)
/// `MAP_PRIVATE` file mmap stays **eager** — preserving POSIX mmap-then-close for
/// callers like `lld` that map an input file and immediately close the fd. The
/// flag is stored in the VMA's `flags`, so it propagates through every
/// split/trim, and the demand-fault path keys off it to read-from-file vs
/// zero-fill.
pub const MAP_LAZY_FILE: u64 = 1 << 32;

/// For a file-backed `MAP_SHARED` **writable** mapping: the backing fd and the
/// file offset of the mapping's first byte. `munmap`/`msync` use this to write
/// the mapped pages back to the file (m3OS file-backed mmap is eager-loaded into
/// anonymous frames, so without this the dirty pages would never reach the
/// file). For a Phase 95b lazy `MAP_LAZY_FILE` mapping it instead records the
/// read source for demand paging (`fd` + the file offset of the first mapped
/// byte). `None` for anonymous, private-eager, read-only, or device mappings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileBacking {
    /// The descriptor the mapping was created from (still open at unmap for the
    /// write-back to land — the common `mmap`/write/`munmap`/`close` order).
    pub fd: u32,
    /// File offset of the first mapped byte.
    pub offset: u64,
}

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
    /// File write-back info for a file-backed `MAP_SHARED` writable mapping;
    /// `None` otherwise. See [`FileBacking`].
    pub file_backing: Option<FileBacking>,
    /// Protection key (0..=15) tagging this region (Phase 90a Track B.3). The
    /// default key 0 means "no PKU restriction" — every legacy mapping carries
    /// it, preserving pre-PKU behaviour bit-for-bit. `pkey_mprotect` sets a
    /// non-zero key here so that a **future demand-fault** in the range composes
    /// a PTE tagged with the key (see `demand_map_vma_page` →
    /// `compose_user_pte_flags`). Splits (mprotect range split, partial unmap)
    /// carry the key through to every resulting piece, exactly as `prot` does.
    pub pkey: u8,
}

impl MemoryMapping {
    /// The [`FileBacking`] for a sub-mapping that starts at `new_start` (which
    /// must lie within this mapping): the file offset advances by the distance
    /// from this mapping's start, so a split/trimmed piece writes back to the
    /// correct file region. `None` if this mapping is not file-backed.
    pub fn file_backing_at(&self, new_start: u64) -> Option<FileBacking> {
        self.file_backing.map(|fb| FileBacking {
            fd: fb.fd,
            offset: fb.offset + (new_start.saturating_sub(self.start)),
        })
    }
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
                    file_backing: vma.file_backing_at(vma_start),
                    pkey: vma.pkey,
                });
                // Right piece: [end, vma_end)
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(end),
                    pkey: vma.pkey,
                });
                removed.push(MemoryMapping {
                    start,
                    len,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(start),
                    pkey: vma.pkey,
                });
            } else if vma_start < start {
                // VMA overlaps on the left -- trim right side.
                to_remove.push(vma_start);
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(vma_start),
                    pkey: vma.pkey,
                });
                removed.push(MemoryMapping {
                    start,
                    len: vma_end - start,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(start),
                    pkey: vma.pkey,
                });
            } else {
                // VMA overlaps on the right -- trim left side.
                to_remove.push(vma_start);
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(end),
                    pkey: vma.pkey,
                });
                removed.push(MemoryMapping {
                    start: vma_start,
                    len: end - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(vma_start),
                    pkey: vma.pkey,
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
                    file_backing: vma.file_backing_at(vma_start),
                    pkey: vma.pkey,
                });
                to_insert.push(MemoryMapping {
                    start,
                    len: end - start,
                    prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(start),
                    pkey: vma.pkey,
                });
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(end),
                    pkey: vma.pkey,
                });
            } else if vma_start < start {
                // Overlap at tail of VMA -- split into head (old) + tail (new).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(vma_start),
                    pkey: vma.pkey,
                });
                to_insert.push(MemoryMapping {
                    start,
                    len: vma_end - start,
                    prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(start),
                    pkey: vma.pkey,
                });
            } else {
                // Overlap at head of VMA -- split into head (new) + tail (old).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: end - vma_start,
                    prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(vma_start),
                    pkey: vma.pkey,
                });
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    prot: vma.prot,
                    flags: vma.flags,
                    file_backing: vma.file_backing_at(end),
                    pkey: vma.pkey,
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

    /// Set the protection key for all VMAs overlapping `[start, start+len)`
    /// (Phase 90a Track B.3, `pkey_mprotect`).
    ///
    /// Mirrors [`Self::update_range_prot`]: partially overlapping VMAs are split
    /// at the boundaries so that only the overlapping portion gets the new
    /// `pkey`; the non-overlapping pieces keep their original key. The kernel
    /// reads this key on a demand-fault in the range so a faulted-in page is
    /// tagged with the right protection key.
    pub fn update_range_pkey(&mut self, start: u64, len: u64, pkey: u8) {
        let end = start.saturating_add(len);
        let mut to_remove = Vec::new();
        let mut to_insert = Vec::new();

        for (&vma_start, vma) in self.tree.range(..end) {
            let vma_end = vma_start.saturating_add(vma.len);
            if vma_end <= start {
                continue; // No overlap
            }
            if vma_start >= start && vma_end <= end {
                // Fully contained -- update pkey in place (collected for later).
                to_remove.push((vma_start, true, vma.clone()));
            } else if vma_start < start && vma_end > end {
                // Middle split: head (old key) + middle (new key) + tail (old key).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    pkey: vma.pkey,
                    ..vma.clone()
                });
                to_insert.push(MemoryMapping {
                    start,
                    len: end - start,
                    pkey,
                    file_backing: vma.file_backing_at(start),
                    ..vma.clone()
                });
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    pkey: vma.pkey,
                    file_backing: vma.file_backing_at(end),
                    ..vma.clone()
                });
            } else if vma_start < start {
                // Overlap at tail of VMA -- split into head (old key) + tail (new key).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: start - vma_start,
                    pkey: vma.pkey,
                    ..vma.clone()
                });
                to_insert.push(MemoryMapping {
                    start,
                    len: vma_end - start,
                    pkey,
                    file_backing: vma.file_backing_at(start),
                    ..vma.clone()
                });
            } else {
                // Overlap at head of VMA -- split into head (new key) + tail (old key).
                to_remove.push((vma_start, false, vma.clone()));
                to_insert.push(MemoryMapping {
                    start: vma_start,
                    len: end - vma_start,
                    pkey,
                    ..vma.clone()
                });
                to_insert.push(MemoryMapping {
                    start: end,
                    len: vma_end - end,
                    pkey: vma.pkey,
                    file_backing: vma.file_backing_at(end),
                    ..vma.clone()
                });
            }
        }

        for (key, just_update_pkey, original) in to_remove {
            self.tree.remove(&key);
            if just_update_pkey {
                self.tree.insert(key, MemoryMapping { pkey, ..original });
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
            file_backing: None,
            pkey: 0,
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

    // -- update_range_pkey (Phase 90a B.3) --------------------------------

    #[test]
    fn update_range_pkey_fully_contained() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x2000, 0x2000)); // [0x2000, 0x4000), key 0
        t.update_range_pkey(0x2000, 0x2000, 5);
        assert_eq!(t.len(), 1);
        let m = t.find_containing(0x2000).unwrap();
        assert_eq!(m.pkey, 5);
        // prot and other fields are untouched by a key-only update.
        assert_eq!(m.prot, 3);
    }

    #[test]
    fn update_range_pkey_middle_split_preserves_neighbours() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x2000, 0x4000)); // [0x2000, 0x6000)
        t.update_range_pkey(0x3000, 0x1000, 7); // tag only [0x3000, 0x4000)
        assert_eq!(t.len(), 3);
        assert_eq!(t.find_containing(0x2000).unwrap().pkey, 0); // head untouched
        assert_eq!(t.find_containing(0x3000).unwrap().pkey, 7); // middle tagged
        assert_eq!(t.find_containing(0x4000).unwrap().pkey, 0); // tail untouched
    }

    #[test]
    fn update_range_pkey_tail_split() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x2000, 0x3000)); // [0x2000, 0x5000)
        t.update_range_pkey(0x4000, 0x1000, 9); // tag [0x4000, 0x5000)
        assert_eq!(t.len(), 2);
        assert_eq!(t.find_containing(0x2000).unwrap().pkey, 0);
        assert_eq!(t.find_containing(0x4000).unwrap().pkey, 9);
    }

    #[test]
    fn update_range_pkey_head_split() {
        let mut t = VmaTree::new();
        t.insert(mapping(0x2000, 0x3000)); // [0x2000, 0x5000)
        t.update_range_pkey(0x2000, 0x1000, 4); // tag [0x2000, 0x3000)
        assert_eq!(t.len(), 2);
        assert_eq!(t.find_containing(0x2000).unwrap().pkey, 4);
        assert_eq!(t.find_containing(0x3000).unwrap().pkey, 0);
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
            file_backing: None,
            pkey: 0,
        });
        t.insert(mapping(0x2000, 0x1000));
        assert!(t.any(|m| m.flags & 0x100 != 0));
        assert!(!t.any(|m| m.flags & 0x200 != 0));
    }

    // -- Phase 86d Track A: Go arena reserve→commit (MAP_FIXED) ------------
    //
    // These model the VMA-tree side of `sys_linux_mmap`'s MAP_FIXED path:
    // a PROT_NONE reservation is committed PROT_RW *at the same address* by
    // `remove_range` (overwrite/split) followed by `insert`. The neighbor
    // VMAs flanking the arena MUST be left byte-for-byte intact — that is the
    // GC-arena hazard the phase calls out.

    const PROT_NONE: u64 = 0x0;
    const PROT_RW: u64 = 0x3; // PROT_READ | PROT_WRITE
    const MAP_ANON_PRIV: u64 = 0x22; // MAP_PRIVATE | MAP_ANONYMOUS
    // Go reserves arenas near ~0xc000000000; well under USER_SPACE_END (128 TiB).
    const ARENA: u64 = 0x0000_00c0_0000_0000;
    const ARENA_LEN: u64 = 0x0400_0000; // 64 MiB, one Go heapArena
    const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

    fn reserve_then_commit(commit_start: u64, commit_len: u64) -> VmaTree {
        let mut t = VmaTree::new();
        // Neighbors immediately below and above the arena.
        let below = MemoryMapping {
            start: 0x1000,
            len: 0x1000,
            prot: PROT_RW,
            flags: MAP_ANON_PRIV,
            file_backing: None,
            pkey: 0,
        };
        let above = MemoryMapping {
            start: ARENA + ARENA_LEN,
            len: 0x1000,
            prot: PROT_RW,
            flags: MAP_ANON_PRIV,
            file_backing: None,
            pkey: 0,
        };
        t.insert(below);
        t.insert(above);
        // sysReserveOS: PROT_NONE reservation, no committed frames.
        t.insert(MemoryMapping {
            start: ARENA,
            len: ARENA_LEN,
            prot: PROT_NONE,
            flags: MAP_ANON_PRIV,
            file_backing: None,
            pkey: 0,
        });
        // sysMapOS: commit PROT_RW MAP_FIXED — overwrite/split then insert.
        t.remove_range(commit_start, commit_len);
        t.insert(MemoryMapping {
            start: commit_start,
            len: commit_len,
            prot: PROT_RW,
            flags: MAP_ANON_PRIV,
            file_backing: None,
            pkey: 0,
        });
        t
    }

    #[test]
    fn map_fixed_full_commit_replaces_reservation_in_place() {
        let t = reserve_then_commit(ARENA, ARENA_LEN);
        // The committed mapping lands at the EXACT arena address, PROT_RW.
        let committed = t.find_containing(ARENA).unwrap();
        assert_eq!(committed.start, ARENA);
        assert_eq!(committed.len, ARENA_LEN);
        assert_eq!(committed.prot, PROT_RW);
        // No PROT_NONE reservation byte survives the commit.
        assert!(!t.any(|m| m.prot == PROT_NONE));
        // Arena fits inside canonical user space.
        assert!(ARENA + ARENA_LEN <= USER_SPACE_END);
        // Neighbors are byte-for-byte intact.
        let below = t.find_containing(0x1000).unwrap();
        assert_eq!(
            (below.start, below.len, below.prot),
            (0x1000, 0x1000, PROT_RW)
        );
        let above = t.find_containing(ARENA + ARENA_LEN).unwrap();
        assert_eq!(
            (above.start, above.len, above.prot),
            (ARENA + ARENA_LEN, 0x1000, PROT_RW)
        );
        // below + committed-arena + above.
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn map_fixed_subrange_commit_splits_reservation() {
        // Go also commits a sub-window of a larger reservation; the reservation
        // must split into head(PROT_NONE) + committed-mid(PROT_RW) + tail(PROT_NONE).
        let mid_start = ARENA + 0x0010_0000; // 1 MiB into the arena
        let mid_len = 0x0010_0000; // commit a 1 MiB window
        let t = reserve_then_commit(mid_start, mid_len);
        // Committed window is exactly placed and writable.
        let mid = t.find_containing(mid_start).unwrap();
        assert_eq!(
            (mid.start, mid.len, mid.prot),
            (mid_start, mid_len, PROT_RW)
        );
        // Head of the reservation stays PROT_NONE at the arena base.
        let head = t.find_containing(ARENA).unwrap();
        assert_eq!((head.start, head.prot), (ARENA, PROT_NONE));
        assert_eq!(head.len, mid_start - ARENA);
        // Tail of the reservation stays PROT_NONE up to the arena end.
        let tail = t.find_containing(mid_start + mid_len).unwrap();
        assert_eq!((tail.start, tail.prot), (mid_start + mid_len, PROT_NONE));
        assert_eq!(tail.len, (ARENA + ARENA_LEN) - (mid_start + mid_len));
        // Neighbors untouched.
        assert_eq!(t.find_containing(0x1000).unwrap().prot, PROT_RW);
        assert_eq!(t.find_containing(ARENA + ARENA_LEN).unwrap().prot, PROT_RW);
        // below + head + mid + tail + above.
        assert_eq!(t.len(), 5);
    }
}
