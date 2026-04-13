use alloc::vec::Vec;
use core::mem;

// ── Constants ───────────────────────────────────────────────────────────

/// Sentinel value marking the end of a slab freelist chain.
const FREELIST_END: usize = 0;

/// [`SpanMeta::partial_idx`] value when the span is **not** in the partial
/// list (i.e. the slab is full).
const NOT_IN_PARTIAL: usize = usize::MAX;

/// Inline bitmap capacity for per-span allocation state.
///
/// 8 words covers a 4 KiB slab page with the minimum supported 8-byte object
/// size (512 objects / 64 bits per word) and avoids recursive heap allocations
/// when the embedding allocator itself uses `SlabCache`.
const INLINE_BITMAP_WORDS: usize = 8;

// ── D.5 — Encoded freelist pointer hardening ────────────────────────────

/// Encode a raw freelist next-pointer for in-object storage.
///
/// The encoding XORs `raw_next` with both a per-cache `freelist_key` and the
/// `slot_addr` where the result will be written.  This makes each stored
/// value position-dependent and key-dependent:
///
/// - Overwriting the stored word with arbitrary bytes is detected on
///   decode (the decoded result won't be a valid in-slab pointer).
/// - Swapping two encoded words between slots decodes to garbage.
/// - The key prevents cross-cache pointer forgery.
#[inline]
fn encode_next(raw_next: usize, freelist_key: usize, slot_addr: usize) -> usize {
    raw_next ^ freelist_key ^ slot_addr
}

/// Decode an encoded freelist pointer back to the raw next-address.
#[inline]
fn decode_next(encoded: usize, freelist_key: usize, slot_addr: usize) -> usize {
    encoded ^ freelist_key ^ slot_addr
}

/// Write an encoded freelist next-pointer into a free object.
///
/// # Safety
///
/// `slot_addr` must be a valid, writable, `usize`-aligned address with at
/// least `size_of::<usize>()` bytes available.
#[inline]
unsafe fn write_encoded_ptr(slot_addr: usize, raw_next: usize, freelist_key: usize) {
    let encoded = encode_next(raw_next, freelist_key, slot_addr);
    unsafe { (slot_addr as *mut usize).write(encoded) };
}

/// Read and decode the freelist next-pointer from a free object.
///
/// # Safety
///
/// `slot_addr` must be a valid, readable, `usize`-aligned address with at
/// least `size_of::<usize>()` bytes, holding a previously-encoded pointer.
#[inline]
unsafe fn read_decoded_ptr(slot_addr: usize, freelist_key: usize) -> usize {
    let encoded = unsafe { (slot_addr as *const usize).read() };
    decode_next(encoded, freelist_key, slot_addr)
}

/// Validate that a decoded freelist pointer is either [`FREELIST_END`] or
/// an aligned address inside the owning slab page.
///
/// # Panics
///
/// Panics with a `D.5 corruption` message if validation fails — this
/// indicates detectable heap corruption.
fn validate_freelist_ptr(decoded: usize, slab_base: usize, page_size: usize, object_size: usize) {
    if decoded == FREELIST_END {
        return;
    }
    assert!(
        decoded >= slab_base && decoded < slab_base + page_size,
        "D.5 corruption: decoded freelist ptr outside slab bounds",
    );
    assert!(
        (decoded - slab_base).is_multiple_of(object_size),
        "D.5 corruption: decoded freelist ptr not aligned to object size",
    );
}

// ── Public types ────────────────────────────────────────────────────────

/// Statistics for a slab cache.
pub struct SlabStats {
    pub total_slabs: usize,
    pub active_objects: usize,
    pub free_slots: usize,
}

struct AllocBitmap {
    words: [u64; INLINE_BITMAP_WORDS],
    word_len: usize,
}

impl AllocBitmap {
    fn new(total_objects: usize) -> Self {
        let word_len = total_objects.div_ceil(64);
        assert!(
            word_len <= INLINE_BITMAP_WORDS,
            "SlabCache: alloc bitmap requires too many words",
        );
        Self {
            words: [0; INLINE_BITMAP_WORDS],
            word_len,
        }
    }

    #[inline]
    fn contains(&self, slot_index: usize) -> bool {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        debug_assert!(word < self.word_len);
        self.words[word] & (1u64 << bit) != 0
    }

    #[inline]
    fn set(&mut self, slot_index: usize, allocated: bool) {
        let word = slot_index / 64;
        let bit = slot_index % 64;
        let mask = 1u64 << bit;
        debug_assert!(word < self.word_len);
        if allocated {
            self.words[word] |= mask;
        } else {
            self.words[word] &= !mask;
        }
    }
}

// ── Out-of-line span metadata ───────────────────────────────────────────

/// Metadata for one slab page, stored **outside** the page itself.
///
/// Keeping metadata out-of-line means the entire page is available for
/// client objects — critical for the 4 096-byte size class on 4 096-byte
/// pages.
struct SpanMeta {
    /// Base address of this slab's backing page.
    base: usize,
    /// Raw address of the first free object, or [`FREELIST_END`] if the
    /// slab is completely full.
    freelist_head: usize,
    /// Number of currently allocated (in-use) objects.
    inuse_count: usize,
    /// Total object capacity of this slab.
    total_objects: usize,
    /// Allocation-state bitmap: bit set iff the slot is currently allocated.
    alloc_bitmap: AllocBitmap,
    /// Owning CPU ID (reserved for future per-CPU slab affinity).
    #[allow(dead_code)]
    owning_cpu: usize,
    /// Size class (== `object_size`) this span belongs to.
    #[allow(dead_code)]
    size_class: usize,
    /// Position in the parent cache's [`SlabCache::partial_list`], or
    /// [`NOT_IN_PARTIAL`] when this slab is full and not in the list.
    partial_idx: usize,
}

impl SpanMeta {
    fn is_full(&self) -> bool {
        self.inuse_count == self.total_objects
    }

    fn is_empty(&self) -> bool {
        self.inuse_count == 0
    }

    fn in_partial_list(&self) -> bool {
        self.partial_idx != NOT_IN_PARTIAL
    }
}

// ── SlabCache ───────────────────────────────────────────────────────────

/// A slab cache with embedded-freelist allocation and D.5 pointer
/// hardening.
///
/// Free objects contain an XOR-encoded next-pointer at their start.  The
/// encoding mixes the raw pointer with a per-cache key and the
/// object's own address — corruption or cross-slot forgery is detected
/// on every allocation.
///
/// Slab metadata is stored out-of-line in [`SpanMeta`] structures so the
/// full backing page is available for client objects (including
/// `object_size == page_size`).
pub struct SlabCache {
    /// Size of each object in bytes (≥ `size_of::<usize>()`).
    object_size: usize,
    /// Size of each backing slab page (typically 4096).
    page_size: usize,
    /// Objects that fit in a single slab page.
    objects_per_slab: usize,
    /// Per-cache XOR key for D.5 freelist pointer hardening.
    freelist_key: usize,
    /// Out-of-line span metadata, kept sorted by base address so owning-span
    /// lookup stays O(log n) without extra side metadata.
    spans: Vec<SpanMeta>,
    /// Span indices that have ≥ 1 free slot (the *partial list*).
    /// Full slabs leave this list and re-enter when an object is freed.
    partial_list: Vec<usize>,
}

impl SlabCache {
    /// Create a new slab cache for objects of `object_size` bytes.
    ///
    /// `page_size` is the size of each backing page (typically 4096).
    ///
    /// A deterministic hardening key is derived from the size
    /// parameters.  For production use, prefer
    /// [`with_freelist_key`](Self::with_freelist_key) with a random key.
    pub fn new(object_size: usize, page_size: usize) -> Self {
        let freelist_key = object_size
            .wrapping_mul(0x517c_c1b7_2722_0a95)
            .wrapping_add(page_size.wrapping_mul(0x6c62_272e_07bb_0142));
        Self::with_freelist_key(object_size, page_size, freelist_key)
    }

    /// Create a new slab cache with an explicit D.5 hardening key.
    pub fn with_freelist_key(object_size: usize, page_size: usize, freelist_key: usize) -> Self {
        assert!(object_size > 0, "object_size must be > 0");
        assert!(page_size > 0, "page_size must be > 0");
        assert!(
            object_size >= mem::size_of::<usize>(),
            "object_size ({}) must be >= {} for embedded freelist pointers",
            object_size,
            mem::size_of::<usize>(),
        );
        assert!(
            object_size.is_multiple_of(mem::align_of::<usize>()),
            "object_size ({}) must be a multiple of {} for pointer alignment",
            object_size,
            mem::align_of::<usize>(),
        );
        let objects_per_slab = page_size / object_size;
        assert!(
            objects_per_slab > 0,
            "object_size ({}) must be <= page_size ({})",
            object_size,
            page_size,
        );

        SlabCache {
            object_size,
            page_size,
            objects_per_slab,
            freelist_key,
            spans: Vec::new(),
            partial_list: Vec::new(),
        }
    }

    /// Allocate a single object, returning its address.
    ///
    /// `page_alloc` is called to obtain a new page when all existing slabs are
    /// full. It should return the base address of a fresh page, or `None` if
    /// out of memory.
    ///
    /// # Panics
    ///
    /// Panics if a D.5 freelist validation check fails (heap corruption).
    pub fn allocate(&mut self, page_alloc: &mut dyn FnMut() -> Option<usize>) -> Option<usize> {
        if let Some(&span_idx) = self.partial_list.last() {
            return self.allocate_from_span(span_idx);
        }

        // All slabs are full (or none exist): request a new page.
        let base = page_alloc()?;
        let span_idx = self.create_span(base);
        self.allocate_from_span(span_idx)
    }

    /// Free an object at the given address.
    ///
    /// Returns `true` if the containing slab became completely empty (the page
    /// could be returned to the system).
    ///
    /// # Panics
    ///
    /// Panics if `addr` does not belong to any slab in this cache.
    pub fn free(&mut self, addr: usize) -> bool {
        let span_idx = self
            .find_span_index(addr)
            .expect("SlabCache::free: address not found in any slab");
        let slot_index = self.slot_index_for_addr(span_idx, addr);
        assert!(
            self.slot_is_allocated(span_idx, slot_index),
            "SlabCache::free: double-free detected",
        );

        let was_full = self.spans[span_idx].is_full();

        // Prepend the freed object to this slab's freelist — O(1).
        let old_head = self.spans[span_idx].freelist_head;
        let freelist_key = self.freelist_key;
        // Safety: `addr` is within a valid slab page and object_size ≥
        // size_of::<usize>() is enforced by the constructor.
        unsafe {
            write_encoded_ptr(addr, old_head, freelist_key);
        }
        self.spans[span_idx].freelist_head = addr;
        self.set_slot_allocated(span_idx, slot_index, false);
        self.spans[span_idx].inuse_count -= 1;

        // A formerly-full slab re-enters the partial list.
        if was_full {
            self.add_to_partial_list(span_idx);
        }

        self.spans[span_idx].is_empty()
    }

    /// Return statistics about this cache.
    pub fn stats(&self) -> SlabStats {
        let total_slabs = self.spans.len();
        let mut active = 0usize;
        let mut free = 0usize;
        for span in &self.spans {
            active += span.inuse_count;
            free += span.total_objects - span.inuse_count;
        }
        SlabStats {
            total_slabs,
            active_objects: active,
            free_slots: free,
        }
    }

    /// Reclaim every completely-empty slab page.
    ///
    /// Empty slabs are removed from the span table in-place without allocating
    /// scratch vectors, then passed to `reclaim_page` so the embedding allocator
    /// can return the backing page to its frame pool.
    pub fn reclaim_empty(&mut self, mut reclaim_page: impl FnMut(usize)) -> usize {
        let mut reclaimed = 0usize;
        let mut span_idx = 0usize;
        while span_idx < self.spans.len() {
            if !self.spans[span_idx].is_empty() {
                span_idx += 1;
                continue;
            }

            if self.spans[span_idx].in_partial_list() {
                self.remove_from_partial_list(span_idx);
            }

            let base = self.spans.remove(span_idx).base;
            self.adjust_partial_list_after_remove(span_idx);
            reclaim_page(base);
            reclaimed += 1;
        }
        reclaimed
    }

    // ── Internal helpers ────────────────────────────────────────────────

    /// Create a span for a freshly-allocated page, initialise its embedded
    /// freelist, and add it to the partial list.
    fn create_span(&mut self, base: usize) -> usize {
        let span_idx = self.spans.partition_point(|span| span.base < base);
        let total = self.objects_per_slab;
        let freelist_key = self.freelist_key;

        // Thread the freelist from last slot back to first so that
        // allocations walk forward through memory (cache-friendly).
        let mut next_addr = FREELIST_END;
        for i in (0..total).rev() {
            let slot_addr = base + i * self.object_size;
            // Safety: slot_addr is within the freshly-allocated page.
            unsafe { write_encoded_ptr(slot_addr, next_addr, freelist_key) };
            next_addr = slot_addr;
        }

        self.spans.insert(
            span_idx,
            SpanMeta {
                base,
                freelist_head: next_addr, // == base (first slot)
                inuse_count: 0,
                total_objects: total,
                alloc_bitmap: AllocBitmap::new(total),
                owning_cpu: 0,
                size_class: self.object_size,
                partial_idx: NOT_IN_PARTIAL,
            },
        );
        self.adjust_partial_list_after_insert(span_idx);
        self.add_to_partial_list(span_idx);
        span_idx
    }

    /// Pop one object from a partial span's freelist — O(1).
    fn allocate_from_span(&mut self, span_idx: usize) -> Option<usize> {
        let head = self.spans[span_idx].freelist_head;
        if head == FREELIST_END {
            return None;
        }

        validate_freelist_ptr(
            head,
            self.spans[span_idx].base,
            self.page_size,
            self.object_size,
        );
        self.assert_slot_is_free(span_idx, head, "freelist_head");

        // Read and decode the next pointer from the head object.
        let freelist_key = self.freelist_key;
        // Safety: head is a freelist address within a valid slab page.
        let next = unsafe { read_decoded_ptr(head, freelist_key) };

        // D.5 validation: the decoded pointer must be in-bounds or sentinel.
        validate_freelist_ptr(
            next,
            self.spans[span_idx].base,
            self.page_size,
            self.object_size,
        );
        if next != FREELIST_END {
            self.assert_slot_is_free(span_idx, next, "freelist_next");
        }

        self.spans[span_idx].freelist_head = next;
        let slot_index = self.slot_index_for_addr(span_idx, head);
        self.set_slot_allocated(span_idx, slot_index, true);
        self.spans[span_idx].inuse_count += 1;

        // If the slab just became full, remove it from the partial list.
        if self.spans[span_idx].is_full() {
            self.remove_from_partial_list(span_idx);
        }

        Some(head)
    }

    /// Find which span contains `addr` — O(log n) via binary search on the
    /// base-address-sorted `spans` table.
    fn find_span_index(&self, addr: usize) -> Option<usize> {
        // `partition_point` returns the first index where span.base > addr,
        // so the candidate (largest base <= addr) is at pos - 1.
        let pos = self.spans.partition_point(|span| span.base <= addr);
        if pos == 0 {
            return None;
        }
        let span_idx = pos - 1;
        let base = self.spans[span_idx].base;
        if addr < base + self.page_size {
            assert!(
                (addr - base).is_multiple_of(self.object_size),
                "SlabCache: addr not aligned to object_size",
            );
            Some(span_idx)
        } else {
            None
        }
    }

    fn slot_index_for_addr(&self, span_idx: usize, addr: usize) -> usize {
        let span = &self.spans[span_idx];
        let offset = addr - span.base;
        assert!(
            offset.is_multiple_of(self.object_size),
            "SlabCache: addr not aligned to object_size",
        );
        let slot_index = offset / self.object_size;
        assert!(
            slot_index < span.total_objects,
            "SlabCache: addr lies outside object slots",
        );
        slot_index
    }

    fn slot_is_allocated(&self, span_idx: usize, slot_index: usize) -> bool {
        self.spans[span_idx].alloc_bitmap.contains(slot_index)
    }

    fn set_slot_allocated(&mut self, span_idx: usize, slot_index: usize, allocated: bool) {
        self.spans[span_idx].alloc_bitmap.set(slot_index, allocated);
    }

    fn assert_slot_is_free(&self, span_idx: usize, addr: usize, label: &str) {
        let slot_index = self.slot_index_for_addr(span_idx, addr);
        assert!(
            !self.slot_is_allocated(span_idx, slot_index),
            "D.5 corruption: {label} points to allocated slot",
        );
    }

    /// Add `span_idx` to the partial list.
    fn add_to_partial_list(&mut self, span_idx: usize) {
        debug_assert!(!self.spans[span_idx].in_partial_list());
        let pos = self.partial_list.len();
        self.partial_list.push(span_idx);
        self.spans[span_idx].partial_idx = pos;
    }

    /// Remove `span_idx` from the partial list — O(1) via swap-remove.
    fn remove_from_partial_list(&mut self, span_idx: usize) {
        let pos = self.spans[span_idx].partial_idx;
        debug_assert!(pos < self.partial_list.len());

        let last = self.partial_list.len() - 1;
        if pos != last {
            let swapped = self.partial_list[last];
            self.partial_list[pos] = swapped;
            self.spans[swapped].partial_idx = pos;
        }
        self.partial_list.pop();
        self.spans[span_idx].partial_idx = NOT_IN_PARTIAL;
    }

    fn adjust_partial_list_after_insert(&mut self, inserted_span_idx: usize) {
        for span_idx in &mut self.partial_list {
            if *span_idx >= inserted_span_idx {
                *span_idx += 1;
            }
        }
    }

    fn adjust_partial_list_after_remove(&mut self, removed_span_idx: usize) {
        for span_idx in &mut self.partial_list {
            if *span_idx > removed_span_idx {
                *span_idx -= 1;
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test page pool ──────────────────────────────────────────────────
    //
    // The embedded freelist writes encoded pointers into slab pages, so
    // test pages must be real, writable memory.  `PagePool` heap-allocates
    // `Vec<u8>` buffers and keeps them alive for the test's lifetime.

    struct PagePool {
        pages: Vec<Vec<u8>>,
        page_size: usize,
    }

    impl PagePool {
        fn new(page_size: usize) -> Self {
            Self {
                pages: Vec::new(),
                page_size,
            }
        }

        fn allocator(&mut self) -> impl FnMut() -> Option<usize> + '_ {
            move || {
                let page = vec![0u8; self.page_size];
                let addr = page.as_ptr() as usize;
                self.pages.push(page);
                Some(addr)
            }
        }
    }

    // ── Basic allocation ────────────────────────────────────────────────

    #[test]
    fn allocate_single_object() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(64, 4096);
        let addr = cache.allocate(&mut pool.allocator());
        assert!(addr.is_some());
        let stats = cache.stats();
        assert_eq!(stats.total_slabs, 1);
        assert_eq!(stats.active_objects, 1);
    }

    #[test]
    fn allocate_until_slab_full_creates_new_slab() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(64, 4096);
        let slots_per_slab = 4096 / 64;

        for _ in 0..slots_per_slab {
            assert!(cache.allocate(&mut pool.allocator()).is_some());
        }
        assert_eq!(cache.stats().total_slabs, 1);
        assert_eq!(cache.stats().free_slots, 0);

        // Next allocation triggers a second slab.
        assert!(cache.allocate(&mut pool.allocator()).is_some());
        assert_eq!(cache.stats().total_slabs, 2);
    }

    #[test]
    fn free_all_objects_slab_becomes_empty() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(128, 4096);
        let slots = 4096 / 128;

        let mut addrs = Vec::new();
        for _ in 0..slots {
            addrs.push(cache.allocate(&mut pool.allocator()).unwrap());
        }

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
    fn mixed_alloc_free_reuses_slot() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(256, 4096);

        let a = cache.allocate(&mut pool.allocator()).unwrap();
        let b = cache.allocate(&mut pool.allocator()).unwrap();
        let c = cache.allocate(&mut pool.allocator()).unwrap();

        // Freeing b prepends it to the freelist head, so the next
        // allocation returns b's address.
        cache.free(b);
        let d = cache.allocate(&mut pool.allocator()).unwrap();
        assert_eq!(d, b, "freed slot should be reused");

        assert_eq!(cache.stats().active_objects, 3);

        // Free everything.
        cache.free(a);
        cache.free(c);
        cache.free(d);
        assert_eq!(cache.stats().active_objects, 0);
    }

    #[test]
    fn full_page_object_size_allocatable() {
        // 4096-byte objects on 4096-byte pages → 1 object per slab.
        // This only works because metadata is out-of-line.
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(4096, 4096);

        let addr = cache.allocate(&mut pool.allocator()).unwrap();
        assert_eq!(cache.stats().total_slabs, 1);
        assert_eq!(cache.stats().active_objects, 1);
        assert_eq!(cache.stats().free_slots, 0);

        assert!(cache.free(addr), "single-object slab should be empty");
        assert_eq!(cache.stats().active_objects, 0);
        assert_eq!(cache.stats().free_slots, 1);
    }

    // ── Partial list management ─────────────────────────────────────────

    #[test]
    fn partial_list_full_slab_leaves_and_reenters() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(512, 4096);
        let slots = 4096 / 512; // 8

        // Fill every slot → slab becomes full → leaves partial list.
        let mut addrs = Vec::new();
        for _ in 0..slots {
            addrs.push(cache.allocate(&mut pool.allocator()).unwrap());
        }
        assert_eq!(cache.stats().free_slots, 0);
        assert!(
            cache.partial_list.is_empty(),
            "full slab must not be in partial list"
        );

        // Free one object → slab re-enters partial list.
        cache.free(addrs.pop().unwrap());
        assert_eq!(
            cache.partial_list.len(),
            1,
            "slab should re-enter partial list"
        );

        // Allocating again should reuse the same slab (no new page).
        let _ = cache.allocate(&mut pool.allocator()).unwrap();
        assert_eq!(
            cache.stats().total_slabs,
            1,
            "should reuse partial slab, not create a new one"
        );
    }

    #[test]
    fn different_object_sizes() {
        for &size in &[8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096] {
            let mut pool = PagePool::new(4096);
            let mut cache = SlabCache::new(size, 4096);
            let expected_slots = 4096 / size;

            for _ in 0..expected_slots {
                assert!(cache.allocate(&mut pool.allocator()).is_some());
            }
            let stats = cache.stats();
            assert_eq!(stats.total_slabs, 1);
            assert_eq!(stats.active_objects, expected_slots);
            assert_eq!(stats.free_slots, 0);
        }
    }

    // ── D.5 hardening ───────────────────────────────────────────────────

    #[test]
    fn d5_encode_decode_roundtrip() {
        let freelist_key = 0xDEAD_BEEF_CAFE_BABE_u64 as usize;
        let slot_addr = 0x1000_usize;

        for &raw_next in &[0_usize, 0x1040, 0x1080, 0x2000, usize::MAX] {
            let encoded = encode_next(raw_next, freelist_key, slot_addr);
            let decoded = decode_next(encoded, freelist_key, slot_addr);
            assert_eq!(
                decoded, raw_next,
                "round-trip failed for raw_next={:#x}",
                raw_next
            );
        }
    }

    #[test]
    fn d5_encoding_is_position_dependent() {
        let freelist_key = 0x1234_5678_9ABC_DEF0_u64 as usize;
        let raw_next = 0x3000_usize;

        let enc_a = encode_next(raw_next, freelist_key, 0x1000);
        let enc_b = encode_next(raw_next, freelist_key, 0x2000);
        assert_ne!(
            enc_a, enc_b,
            "same raw_next at different slots must encode differently"
        );
    }

    #[test]
    fn d5_encoding_is_key_dependent() {
        let slot_addr = 0x1000_usize;
        let raw_next = 0x2000_usize;

        let enc_a = encode_next(raw_next, 0xAAAA, slot_addr);
        let enc_b = encode_next(raw_next, 0xBBBB, slot_addr);
        assert_ne!(
            enc_a, enc_b,
            "same raw_next with different keys must encode differently"
        );
    }

    #[test]
    #[should_panic(expected = "D.5 corruption")]
    fn d5_corruption_detected_on_allocate() {
        let mut pool = PagePool::new(4096);
        let freelist_key = 0xAAAA_BBBB_CCCC_DDDD_u64 as usize;
        let mut cache = SlabCache::with_freelist_key(64, 4096, freelist_key);

        // First allocate: creates slab, pops slot 0 from the freelist.
        let first = cache.allocate(&mut pool.allocator()).unwrap();

        // Slot 1 is now the freelist head.  Corrupt its encoded pointer.
        let slot1 = first + 64;
        unsafe { (slot1 as *mut usize).write(0xBAD_BAD_BAD) };

        // Next allocate pops slot 1, reads the corrupted pointer → panic.
        let _ = cache.allocate(&mut pool.allocator());
    }

    #[test]
    #[should_panic(expected = "double-free")]
    fn double_free_panics() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(64, 4096);
        let addr = cache.allocate(&mut pool.allocator()).unwrap();

        cache.free(addr);
        cache.free(addr);
    }

    #[test]
    #[should_panic(expected = "not aligned")]
    fn misaligned_free_panics() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(64, 4096);
        let addr = cache.allocate(&mut pool.allocator()).unwrap();

        cache.free(addr + 1);
    }

    #[test]
    #[should_panic(expected = "outside object slots")]
    fn free_from_wasted_page_tail_panics() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(48, 4096);
        let addr = cache.allocate(&mut pool.allocator()).unwrap();
        let span_idx = cache.find_span_index(addr).unwrap();
        let invalid_addr = cache.spans[span_idx].base + cache.spans[span_idx].total_objects * 48;

        cache.free(invalid_addr);
    }

    #[test]
    #[should_panic(expected = "D.5 corruption")]
    fn d5_corruption_detected_on_invalid_head() {
        let mut pool = PagePool::new(4096);
        let freelist_key = 0xFEED_FACE_1234_5678_u64 as usize;
        let mut cache = SlabCache::with_freelist_key(64, 4096, freelist_key);
        let addr = cache.allocate(&mut pool.allocator()).unwrap();

        cache.free(addr);
        let span_idx = cache.find_span_index(addr).unwrap();
        cache.spans[span_idx].freelist_head = addr + 1;

        let _ = cache.allocate(&mut pool.allocator());
    }

    #[test]
    #[should_panic(expected = "allocated slot")]
    fn d5_corruption_detected_when_head_points_to_allocated_slot() {
        let mut pool = PagePool::new(4096);
        let freelist_key = 0x1357_9BDF_2468_ACED_u64 as usize;
        let mut cache = SlabCache::with_freelist_key(64, 4096, freelist_key);
        let first = cache.allocate(&mut pool.allocator()).unwrap();
        let second = cache.allocate(&mut pool.allocator()).unwrap();

        cache.free(first);
        let span_idx = cache.find_span_index(first).unwrap();
        cache.spans[span_idx].freelist_head = second;

        let _ = cache.allocate(&mut pool.allocator());
    }

    #[test]
    fn page_alloc_failure_returns_none() {
        let mut cache = SlabCache::new(64, 4096);
        let mut fail_alloc = || None;
        assert!(cache.allocate(&mut fail_alloc).is_none());
    }

    #[test]
    fn reclaim_empty_returns_pages_and_updates_stats() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(256, 4096);

        let a = cache.allocate(&mut pool.allocator()).unwrap();
        let b = cache.allocate(&mut pool.allocator()).unwrap();
        cache.free(a);
        cache.free(b);

        let mut reclaimed = Vec::new();
        let count = cache.reclaim_empty(|base| reclaimed.push(base));
        assert_eq!(count, 1);
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(cache.stats().total_slabs, 0);
        assert!(cache.partial_list.is_empty());
    }

    #[test]
    fn reclaim_empty_keeps_partial_indices_consistent() {
        let mut pool = PagePool::new(4096);
        let mut cache = SlabCache::new(512, 4096);
        let slots = 4096 / 512;

        let mut first_slab = Vec::new();
        for _ in 0..slots {
            first_slab.push(cache.allocate(&mut pool.allocator()).unwrap());
        }
        let survivor = cache.allocate(&mut pool.allocator()).unwrap();
        for addr in first_slab {
            cache.free(addr);
        }

        let reclaimed = cache.reclaim_empty(|_| {});
        assert_eq!(reclaimed, 1);
        assert_eq!(cache.stats().total_slabs, 1);
        assert_eq!(cache.partial_list.len(), 1);

        cache.free(survivor);
        assert_eq!(cache.reclaim_empty(|_| {}), 1);
        assert_eq!(cache.stats().total_slabs, 0);
        assert!(cache.partial_list.is_empty());
    }

    // ── Sorted span lookup tests ────────────────────────────────────────

    #[test]
    fn sorted_spans_find_spans_regardless_of_insertion_order() {
        // Use object_size == page_size so each allocation creates a new span.
        let page_size = 4096;
        let mut bufs: Vec<Vec<u8>> = (0..4).map(|_| vec![0u8; page_size]).collect();
        // Sort buffers by address so we can hand them out in reverse.
        bufs.sort_by_key(|b| b.as_ptr() as usize);

        let mut cache = SlabCache::new(page_size, page_size);

        // Feed pages in descending base-address order.
        let mut idx = bufs.len();
        let mut alloc = || {
            if idx == 0 {
                return None;
            }
            idx -= 1;
            Some(bufs[idx].as_ptr() as usize)
        };

        let mut addrs = Vec::new();
        for _ in 0..4 {
            addrs.push(cache.allocate(&mut alloc).unwrap());
        }

        // Verify every allocated address resolves to the correct span.
        for &a in &addrs {
            let span_idx = cache
                .find_span_index(a)
                .expect("address must be found in lookup");
            assert_eq!(cache.spans[span_idx].base, a);
        }

        // Verify the span table itself is sorted by base address.
        for w in cache.spans.windows(2) {
            assert!(w[0].base < w[1].base, "spans must stay sorted by base");
        }
    }

    #[test]
    fn sorted_spans_consistent_after_reclaim_removes() {
        let page_size = 4096;
        let mut pool = PagePool::new(page_size);
        let mut cache = SlabCache::new(page_size, page_size); // 1 object per slab

        // Create 5 single-object spans.
        let mut addrs: Vec<usize> = (0..5)
            .map(|_| cache.allocate(&mut pool.allocator()).unwrap())
            .collect();
        assert_eq!(cache.spans.len(), 5);

        // Free spans 0 and 2 (non-contiguous) so reclaim must remove holes while
        // keeping the remaining spans sorted.
        cache.free(addrs[0]);
        cache.free(addrs[2]);
        let reclaimed = cache.reclaim_empty(|_| {});
        assert_eq!(reclaimed, 2);
        assert_eq!(cache.spans.len(), 3);

        // The surviving addresses (indices 1, 3, 4) must still resolve.
        addrs.remove(2);
        addrs.remove(0);
        for &a in &addrs {
            let span_idx = cache
                .find_span_index(a)
                .expect("surviving address must resolve after reclaim");
            assert_eq!(cache.spans[span_idx].base, a);
        }
        for w in cache.spans.windows(2) {
            assert!(
                w[0].base < w[1].base,
                "spans must stay sorted after reclaim"
            );
        }

        // Allocate and free through the survivors to check full round-trip.
        for &a in &addrs {
            cache.free(a);
        }
        let reclaimed = cache.reclaim_empty(|_| {});
        assert_eq!(reclaimed, 3);
        assert_eq!(cache.spans.len(), 0);
    }

    #[test]
    fn reclaim_empty_supports_min_object_size_bitmap() {
        let object_size = mem::size_of::<usize>();
        let page_size = 4096;
        let mut pool = PagePool::new(page_size);
        let mut cache = SlabCache::new(object_size, page_size);
        let mut addrs = Vec::new();

        for _ in 0..(page_size / object_size) {
            addrs.push(
                cache
                    .allocate(&mut pool.allocator())
                    .expect("minimal-object slab allocation"),
            );
        }

        for addr in addrs {
            cache.free(addr);
        }

        assert_eq!(cache.reclaim_empty(|_| {}), 1);
        assert_eq!(cache.stats().total_slabs, 0);
    }
}
