//! Minimal slab allocator for task storage.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

/// A simple slab allocator that reuses freed slots.
pub struct Slab<T> {
    entries: Vec<Option<T>>,
    free_list: Vec<usize>,
    len: usize,
}

impl<T> Slab<T> {
    /// Create a new empty slab.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            free_list: Vec::new(),
            len: 0,
        }
    }

    /// Insert a value, returning its key.
    pub fn insert(&mut self, val: T) -> usize {
        self.len += 1;
        if let Some(key) = self.free_list.pop() {
            self.entries[key] = Some(val);
            key
        } else {
            let key = self.entries.len();
            self.entries.push(Some(val));
            key
        }
    }

    /// Remove the value at `key`, returning it.
    ///
    /// # Panics
    /// Panics if `key` is out of bounds or the slot is empty.
    pub fn remove(&mut self, key: usize) -> T {
        let val = self.entries[key]
            .take()
            .expect("slab: remove called on empty slot");
        self.free_list.push(key);
        self.len -= 1;
        val
    }

    /// Get a reference to the value at `key`.
    pub fn get(&self, key: usize) -> Option<&T> {
        self.entries.get(key).and_then(|slot| slot.as_ref())
    }

    /// Get a mutable reference to the value at `key`.
    pub fn get_mut(&mut self, key: usize) -> Option<&mut T> {
        self.entries.get_mut(key).and_then(|slot| slot.as_mut())
    }

    /// Returns `true` if the slab contains a value at `key`.
    pub fn contains(&self, key: usize) -> bool {
        self.entries.get(key).is_some_and(|slot| slot.is_some())
    }

    /// Returns the number of occupied slots.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the slab is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let mut slab = Slab::new();
        let k0 = slab.insert("hello");
        let k1 = slab.insert("world");
        assert_eq!(slab.get(k0), Some(&"hello"));
        assert_eq!(slab.get(k1), Some(&"world"));
    }

    #[test]
    fn test_remove() {
        let mut slab = Slab::new();
        let k = slab.insert(42);
        assert_eq!(slab.remove(k), 42);
        assert_eq!(slab.get(k), None);
        assert!(!slab.contains(k));
    }

    #[test]
    fn test_reuse_freed_slots() {
        let mut slab = Slab::new();
        let k0 = slab.insert("a");
        let _k1 = slab.insert("b");
        slab.remove(k0);
        let k2 = slab.insert("c");
        // Freed slot k0 should be reused
        assert_eq!(k2, k0);
        assert_eq!(slab.get(k2), Some(&"c"));
    }

    #[test]
    fn test_get_out_of_bounds_returns_none() {
        let slab: Slab<i32> = Slab::new();
        assert_eq!(slab.get(0), None);
        assert_eq!(slab.get(999), None);
    }

    #[test]
    fn test_get_empty_slot_returns_none() {
        let mut slab = Slab::new();
        let k = slab.insert(10);
        slab.remove(k);
        assert_eq!(slab.get(k), None);
    }

    #[test]
    fn test_len_tracks_correctly() {
        let mut slab = Slab::new();
        assert_eq!(slab.len(), 0);
        assert!(slab.is_empty());

        let k0 = slab.insert(1);
        assert_eq!(slab.len(), 1);

        let _k1 = slab.insert(2);
        assert_eq!(slab.len(), 2);

        slab.remove(k0);
        assert_eq!(slab.len(), 1);
        assert!(!slab.is_empty());
    }

    #[test]
    fn test_contains() {
        let mut slab = Slab::new();
        assert!(!slab.contains(0));
        let k = slab.insert("x");
        assert!(slab.contains(k));
        slab.remove(k);
        assert!(!slab.contains(k));
    }

    #[test]
    fn test_get_mut() {
        let mut slab = Slab::new();
        let k = slab.insert(10);
        if let Some(val) = slab.get_mut(k) {
            *val = 20;
        }
        assert_eq!(slab.get(k), Some(&20));
    }

    #[test]
    #[should_panic(expected = "remove called on empty slot")]
    fn test_remove_empty_panics() {
        let mut slab = Slab::new();
        let k = slab.insert(1);
        slab.remove(k);
        slab.remove(k); // should panic
    }
}
