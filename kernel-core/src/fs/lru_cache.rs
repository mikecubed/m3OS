//! Bounded LRU block cache — pure logic, host-testable.
//!
//! A read-through cache of filesystem blocks keyed by block number. Used by the
//! ring-3 `vfs_server` (and available to the in-kernel ext2 engine) as a
//! **metadata** cache: the data-block run reader bypasses it, so what lands here
//! is inode-table / directory / allocation-bitmap / indirect-pointer blocks —
//! the blocks re-read across a workload.
//!
//! # Why LRU (Phase 95c Track C)
//!
//! The original cache was **fill-and-hold**: once full it stopped admitting new
//! blocks (kept the first `cap` distinct blocks ever seen). That is fine while a
//! workload's metadata working set fits under the cap, but a large operation
//! (e.g. installing a multi-hundred-MB package, then cold-loading a 162 MB DSO)
//! overflows it — and then every later metadata read, *including the hot
//! single/double-indirect pointer blocks a sequential scan re-touches*, misses
//! to the device. Worse, in the realistic install-then-load flow the install
//! fills the cache, so fill-and-hold refuses to admit the cold-load's indirect
//! blocks at all and they re-read once per cluster fault. LRU evicts the stale
//! (least-recently-used) blocks and keeps the genuinely-hot ones resident.
//!
//! # Coherence
//!
//! The cache holds **clean** disk content only. The owner invalidates on write
//! (`remove`/`clear`), so a hit never serves stale data. Eviction only ever
//! drops a clean block — a later read re-fetches it from disk — so it can never
//! introduce incoherence regardless of policy.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// A bounded least-recently-used cache mapping block number → block bytes.
///
/// Recency is tracked with a monotonic tick: `map` stores each block's bytes and
/// its last-access tick; `by_tick` is the inverse index (tick → block) kept in
/// sorted order, so the least-recently-used block is `by_tick`'s first entry and
/// eviction is O(log n). `get` and `insert` bump the tick (most-recently-used);
/// `remove`/`clear` drop entries. A `cap` of 0 disables caching (every `insert`
/// is a no-op), which callers can use to bypass the cache without branching.
pub struct LruBlockCache {
    map: BTreeMap<u32, (Vec<u8>, u64)>,
    by_tick: BTreeMap<u64, u32>,
    next_tick: u64,
    cap: usize,
}

impl LruBlockCache {
    /// Create a cache holding at most `cap` blocks.
    pub fn new(cap: usize) -> Self {
        Self {
            map: BTreeMap::new(),
            by_tick: BTreeMap::new(),
            next_tick: 0,
            cap,
        }
    }

    /// Number of blocks currently held.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when the cache holds no blocks.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// True if `block` is cached (does NOT update recency).
    pub fn contains_key(&self, block: u32) -> bool {
        self.map.contains_key(&block)
    }

    /// Look up `block`, marking it most-recently-used on a hit. Returns the
    /// cached bytes (the caller clones what it needs) or `None` on a miss.
    pub fn get(&mut self, block: u32) -> Option<&[u8]> {
        let old_tick = match self.map.get(&block) {
            Some((_, t)) => *t,
            None => return None,
        };
        self.next_tick += 1;
        let new_tick = self.next_tick;
        self.by_tick.remove(&old_tick);
        self.by_tick.insert(new_tick, block);
        let entry = self.map.get_mut(&block).expect("present");
        entry.1 = new_tick;
        Some(&entry.0)
    }

    /// Insert or refresh `block`, marking it most-recently-used. When the cache
    /// is full and `block` is new, the least-recently-used block is evicted
    /// first. A `cap` of 0 makes this a no-op.
    pub fn insert(&mut self, block: u32, data: Vec<u8>) {
        if self.cap == 0 {
            return;
        }
        if let Some((_, old_tick)) = self.map.get(&block) {
            // Refreshing an existing block — retire its old recency slot.
            let old = *old_tick;
            self.by_tick.remove(&old);
        } else if self.map.len() >= self.cap {
            // Evict the least-recently-used (smallest tick) before admitting.
            if let Some((&min_tick, &victim)) = self.by_tick.iter().next() {
                self.by_tick.remove(&min_tick);
                self.map.remove(&victim);
            }
        }
        self.next_tick += 1;
        let tick = self.next_tick;
        self.by_tick.insert(tick, block);
        self.map.insert(block, (data, tick));
    }

    /// Drop `block` from the cache (write-invalidation). No-op if absent.
    pub fn remove(&mut self, block: u32) {
        if let Some((_, tick)) = self.map.remove(&block) {
            self.by_tick.remove(&tick);
        }
    }

    /// Drop every cached block (full invalidation).
    pub fn clear(&mut self) {
        self.map.clear();
        self.by_tick.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn blk(b: u8) -> Vec<u8> {
        vec![b; 4]
    }

    #[test]
    fn insert_then_get_hits() {
        let mut c = LruBlockCache::new(4);
        c.insert(1, blk(0xAA));
        assert_eq!(c.get(1), Some(&[0xAA; 4][..]));
        assert_eq!(c.get(2), None);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut c = LruBlockCache::new(2);
        c.insert(1, blk(1));
        c.insert(2, blk(2));
        // Touch 1 so 2 becomes the LRU.
        assert!(c.get(1).is_some());
        c.insert(3, blk(3)); // evicts 2
        assert!(c.contains_key(1));
        assert!(!c.contains_key(2));
        assert!(c.contains_key(3));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn eviction_without_touch_drops_oldest_insert() {
        let mut c = LruBlockCache::new(2);
        c.insert(1, blk(1));
        c.insert(2, blk(2));
        c.insert(3, blk(3)); // no touches → 1 is LRU → evicted
        assert!(!c.contains_key(1));
        assert!(c.contains_key(2));
        assert!(c.contains_key(3));
    }

    #[test]
    fn refresh_existing_does_not_evict() {
        let mut c = LruBlockCache::new(2);
        c.insert(1, blk(1));
        c.insert(2, blk(2));
        // Re-inserting an existing key updates value + recency, no eviction.
        c.insert(1, blk(0xFF));
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(1), Some(&[0xFF; 4][..]));
        assert!(c.contains_key(2));
        // Now 2 is LRU; inserting 3 evicts 2, keeps the refreshed 1.
        c.insert(3, blk(3));
        assert!(c.contains_key(1));
        assert!(!c.contains_key(2));
    }

    #[test]
    fn remove_and_clear() {
        let mut c = LruBlockCache::new(4);
        c.insert(1, blk(1));
        c.insert(2, blk(2));
        c.remove(1);
        assert!(!c.contains_key(1));
        assert!(c.contains_key(2));
        // Removing frees a slot AND its recency entry — no stale tick lingers.
        c.insert(3, blk(3));
        c.insert(4, blk(4));
        assert_eq!(c.len(), 3); // 2,3,4
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.get(2), None);
    }

    #[test]
    fn cap_zero_disables_caching() {
        let mut c = LruBlockCache::new(0);
        c.insert(1, blk(1));
        assert_eq!(c.len(), 0);
        assert_eq!(c.get(1), None);
    }

    #[test]
    fn stale_tick_after_remove_does_not_misevict() {
        // Regression: a removed block's tick must not linger in `by_tick`, else a
        // later eviction could pick a phantom victim and corrupt sizing.
        let mut c = LruBlockCache::new(2);
        c.insert(1, blk(1));
        c.insert(2, blk(2));
        c.remove(2);
        c.insert(3, blk(3)); // cache has {1,3}, no eviction needed (len was 1)
        assert!(c.contains_key(1));
        assert!(c.contains_key(3));
        assert_eq!(c.len(), 2);
        // Inserting a 3rd now evicts the true LRU (1), not a phantom.
        c.insert(4, blk(4));
        assert!(!c.contains_key(1));
        assert!(c.contains_key(3));
        assert!(c.contains_key(4));
    }
}
