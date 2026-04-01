use alloc::vec;
use alloc::vec::Vec;

/// Statistics for a slab cache.
pub struct SlabStats {
    pub total_slabs: usize,
    pub active_objects: usize,
    pub free_slots: usize,
}

/// A single slab: one page divided into fixed-size slots.
struct Slab {
    /// Base address of this slab's page.
    base: usize,
    /// Bitmap of free slots (1 = free, 0 = allocated).
    free_bitmap: Vec<u64>,
    /// Number of free slots.
    free_count: usize,
    /// Total number of slots in this slab.
    total_slots: usize,
}

impl Slab {
    /// Create a new slab at the given base address.
    fn new(base: usize, total_slots: usize) -> Self {
        let bitmap_words = total_slots.div_ceil(64);
        let mut free_bitmap = vec![!0u64; bitmap_words];

        // Clear unused bits in the last word.
        let remainder = total_slots % 64;
        if remainder != 0 {
            free_bitmap[bitmap_words - 1] = (1u64 << remainder) - 1;
        }

        Slab {
            base,
            free_bitmap,
            free_count: total_slots,
            total_slots,
        }
    }

    /// Allocate a slot, returning its index. Returns None if full.
    fn allocate(&mut self) -> Option<usize> {
        if self.free_count == 0 {
            return None;
        }
        for (word_idx, word) in self.free_bitmap.iter_mut().enumerate() {
            if *word != 0 {
                let bit = word.trailing_zeros() as usize;
                *word &= !(1u64 << bit);
                self.free_count -= 1;
                return Some(word_idx * 64 + bit);
            }
        }
        None
    }

    /// Free a slot by index. Returns true if the slab is now completely empty.
    fn free(&mut self, slot_index: usize) -> bool {
        let word_idx = slot_index / 64;
        let bit = slot_index % 64;
        self.free_bitmap[word_idx] |= 1u64 << bit;
        self.free_count += 1;
        self.free_count == self.total_slots
    }

    /// Returns true if this slab has free slots.
    fn has_free(&self) -> bool {
        self.free_count > 0
    }
}

/// A slab cache manages objects of a single fixed size.
pub struct SlabCache {
    /// Size of each object in bytes.
    object_size: usize,
    /// Page size used for slab allocation.
    page_size: usize,
    /// Number of slots per slab.
    slots_per_slab: usize,
    /// All slabs managed by this cache.
    slabs: Vec<Slab>,
}

impl SlabCache {
    /// Create a new slab cache for objects of `object_size` bytes.
    ///
    /// `page_size` is the size of each backing page (typically 4096).
    pub fn new(object_size: usize, page_size: usize) -> Self {
        assert!(object_size > 0, "object_size must be > 0");
        assert!(page_size > 0, "page_size must be > 0");
        let slots_per_slab = page_size / object_size;
        assert!(slots_per_slab > 0, "object_size must be <= page_size");

        SlabCache {
            object_size,
            page_size,
            slots_per_slab,
            slabs: Vec::new(),
        }
    }

    /// Allocate a single object, returning its address.
    ///
    /// `page_alloc` is called to obtain a new page when all existing slabs are
    /// full. It should return the base address of a fresh page, or `None` if
    /// out of memory.
    pub fn allocate(&mut self, page_alloc: &mut dyn FnMut() -> Option<usize>) -> Option<usize> {
        // Search partial slabs first (have free slots but are not empty).
        for slab in &mut self.slabs {
            if slab.has_free() {
                let slot = slab.allocate()?;
                return Some(slab.base + slot * self.object_size);
            }
        }

        // All slabs are full: allocate a new page.
        let base = page_alloc()?;
        let mut slab = Slab::new(base, self.slots_per_slab);
        let slot = slab.allocate()?;
        let addr = slab.base + slot * self.object_size;
        self.slabs.push(slab);
        Some(addr)
    }

    /// Free an object at the given address.
    ///
    /// Returns `true` if the containing slab became completely empty (the page
    /// could be returned to the system).
    pub fn free(&mut self, addr: usize) -> bool {
        for slab in &mut self.slabs {
            if addr >= slab.base && addr < slab.base + self.page_size {
                let slot_index = (addr - slab.base) / self.object_size;
                return slab.free(slot_index);
            }
        }
        // Address not found in any slab.
        false
    }

    /// Return statistics about this cache.
    pub fn stats(&self) -> SlabStats {
        let total_slabs = self.slabs.len();
        let mut free_slots = 0usize;
        let mut total_slots = 0usize;
        for slab in &self.slabs {
            free_slots += slab.free_count;
            total_slots += slab.total_slots;
        }
        SlabStats {
            total_slabs,
            active_objects: total_slots - free_slots,
            free_slots,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: creates a page allocator that hands out pages starting at a
    /// given base address, incrementing by page_size each call.
    fn make_page_alloc(page_size: usize) -> impl FnMut() -> Option<usize> {
        let mut next = page_size; // start at page_size to avoid address 0
        move || {
            let addr = next;
            next += page_size;
            Some(addr)
        }
    }

    #[test]
    fn allocate_single_object() {
        let mut cache = SlabCache::new(64, 4096);
        let mut alloc = make_page_alloc(4096);
        let addr = cache.allocate(&mut alloc);
        assert!(addr.is_some());
        let stats = cache.stats();
        assert_eq!(stats.total_slabs, 1);
        assert_eq!(stats.active_objects, 1);
    }

    #[test]
    fn allocate_until_slab_full_creates_new_slab() {
        let mut cache = SlabCache::new(64, 4096);
        let mut alloc = make_page_alloc(4096);
        let slots_per_slab = 4096 / 64;

        // Fill the first slab completely.
        for _ in 0..slots_per_slab {
            assert!(cache.allocate(&mut alloc).is_some());
        }
        assert_eq!(cache.stats().total_slabs, 1);
        assert_eq!(cache.stats().free_slots, 0);

        // Next allocation should create a second slab.
        assert!(cache.allocate(&mut alloc).is_some());
        assert_eq!(cache.stats().total_slabs, 2);
    }

    #[test]
    fn free_all_objects_slab_becomes_empty() {
        let mut cache = SlabCache::new(128, 4096);
        let mut alloc = make_page_alloc(4096);
        let slots = 4096 / 128;

        let mut addrs = Vec::new();
        for _ in 0..slots {
            addrs.push(cache.allocate(&mut alloc).unwrap());
        }

        // Free all but the last: slab should not be empty yet.
        for addr in &addrs[..addrs.len() - 1] {
            assert!(!cache.free(*addr));
        }

        // Freeing the last object should mark the slab empty.
        assert!(cache.free(addrs[addrs.len() - 1]));

        let stats = cache.stats();
        assert_eq!(stats.active_objects, 0);
        assert_eq!(stats.free_slots, slots);
    }

    #[test]
    fn mixed_alloc_free_patterns() {
        let mut cache = SlabCache::new(256, 4096);
        let mut alloc = make_page_alloc(4096);

        let a = cache.allocate(&mut alloc).unwrap();
        let b = cache.allocate(&mut alloc).unwrap();
        let c = cache.allocate(&mut alloc).unwrap();

        // Free middle object, then re-allocate: should reuse the slot.
        cache.free(b);
        let d = cache.allocate(&mut alloc).unwrap();
        // d should be the same address as b (reused slot).
        assert_eq!(d, b);

        let stats = cache.stats();
        assert_eq!(stats.active_objects, 3);

        // Free all.
        cache.free(a);
        cache.free(c);
        cache.free(d);
        assert_eq!(cache.stats().active_objects, 0);
    }

    #[test]
    fn different_object_sizes() {
        for size in &[64, 128, 256, 512] {
            let mut cache = SlabCache::new(*size, 4096);
            let mut alloc = make_page_alloc(4096);
            let expected_slots = 4096 / size;

            for _ in 0..expected_slots {
                assert!(cache.allocate(&mut alloc).is_some());
            }
            let stats = cache.stats();
            assert_eq!(stats.total_slabs, 1);
            assert_eq!(stats.active_objects, expected_slots);
            assert_eq!(stats.free_slots, 0);
        }
    }

    #[test]
    fn free_returns_true_when_slab_empty() {
        let mut cache = SlabCache::new(512, 4096);
        let mut alloc = make_page_alloc(4096);
        let slots = 4096 / 512; // 8 slots

        let mut addrs = Vec::new();
        for _ in 0..slots {
            addrs.push(cache.allocate(&mut alloc).unwrap());
        }

        // Free all but the last: none should return true.
        for addr in &addrs[..slots - 1] {
            assert!(!cache.free(*addr), "slab should not be empty yet");
        }

        // Last free makes the slab empty.
        assert!(
            cache.free(addrs[slots - 1]),
            "slab should be empty after freeing all objects"
        );
    }

    #[test]
    fn page_alloc_failure_returns_none() {
        let mut cache = SlabCache::new(64, 4096);
        let mut fail_alloc = || None;
        assert!(cache.allocate(&mut fail_alloc).is_none());
    }
}
