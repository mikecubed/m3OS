//! Lock-free MPSC intrusive free list for cross-CPU slab frees (Track E.1).
//!
//! When CPU B frees an object that was allocated on CPU A's slab, it cannot
//! touch CPU A's magazine lock.  Instead it CAS-pushes the freed pointer onto
//! CPU A's per-size-class [`CrossCpuFreeList`].  CPU A batch-collects the
//! entire queue on its next allocation via [`CrossCpuFreeList::take_all`].
//!
//! # Invariants
//!
//! * **Push** is lock-free (CAS loop) and allocation-free — safe from any
//!   context including ISR-disabled magazine paths.
//! * **Take-all** is a single atomic swap — O(1) regardless of queue depth.
//! * Each node stores an intrusive next-pointer at its first `*mut u8` bytes.
//!   This is valid because freed slab objects have at least
//!   `size_of::<usize>()` bytes (enforced by the slab cache constructor).
//! * The list is MPSC: multiple producers (any CPU), single consumer (owning
//!   CPU).
//!
//! # Metadata lifetime
//!
//! The owning CPU's cross-CPU free lists persist for the core's lifetime
//! (allocated via `PerCoreData`).  Nodes in the list are freed slab objects
//! whose backing slab page remains live until explicit reclaim (Track F).
//! Between `push` and the owning CPU's `take_all`, the only access to the
//! node memory is through the intrusive next-pointer at offset 0.

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

/// An MPSC intrusive free list for cross-CPU slab object return.
///
/// Each freed object's first `size_of::<*mut u8>()` bytes store the
/// intrusive next-pointer while the object is on this list.
pub struct CrossCpuFreeList {
    head: AtomicPtr<u8>,
}

// Safety: The list is designed for concurrent access from multiple CPUs.
// AtomicPtr provides the necessary synchronization.
unsafe impl Send for CrossCpuFreeList {}
unsafe impl Sync for CrossCpuFreeList {}

impl Default for CrossCpuFreeList {
    fn default() -> Self {
        Self::new()
    }
}

impl CrossCpuFreeList {
    /// Create a new empty free list.
    pub const fn new() -> Self {
        Self {
            head: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Push a freed object onto the list (lock-free CAS, allocation-free).
    ///
    /// Uses `Release` ordering on CAS success so the owning CPU's `Acquire`
    /// load in [`take_all`] sees the next-pointer write.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a freed slab object with at least
    /// `size_of::<*mut u8>()` writable bytes at its start.  The caller
    /// must not read or write the object (except through this list) until
    /// the owning CPU collects it.
    pub unsafe fn push(&self, ptr: *mut u8) {
        debug_assert!(!ptr.is_null());
        loop {
            let old_head = self.head.load(Ordering::Relaxed);
            // Store current head as intrusive next-pointer in the freed object.
            unsafe { ptr.cast::<*mut u8>().write(old_head) };
            match self.head.compare_exchange_weak(
                old_head,
                ptr,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(_) => continue,
            }
        }
    }

    /// Take the entire queue atomically, returning the head of an intrusive
    /// singly-linked list (or null if the queue was empty).
    ///
    /// Only the owning CPU should call this.  Uses `Acquire` ordering to
    /// synchronize with all producers' `Release` stores.
    ///
    /// Walk the returned chain with [`ChainIter`]; null terminates the list.
    pub fn take_all(&self) -> *mut u8 {
        self.head.swap(ptr::null_mut(), Ordering::Acquire)
    }

    /// Non-authoritative emptiness check (may race with concurrent pushes).
    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Relaxed).is_null()
    }
}

/// Iterator over a chain returned by [`CrossCpuFreeList::take_all`].
///
/// Reads and follows the intrusive next-pointers, yielding each node.
///
/// # Safety contract
///
/// The chain must have been produced by [`CrossCpuFreeList::take_all`] and
/// each node must still be valid memory with a readable `*mut u8` at offset 0.
pub struct ChainIter {
    current: *mut u8,
}

impl ChainIter {
    /// Create an iterator from a chain head (may be null for an empty chain).
    ///
    /// # Safety
    ///
    /// All nodes in the chain must be valid, freed slab objects with
    /// readable next-pointers at offset 0.
    pub unsafe fn new(head: *mut u8) -> Self {
        Self { current: head }
    }
}

impl Iterator for ChainIter {
    type Item = *mut u8;

    fn next(&mut self) -> Option<*mut u8> {
        if self.current.is_null() {
            return None;
        }
        let node = self.current;
        self.current = unsafe { node.cast::<*mut u8>().read() };
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Allocate a 64-byte aligned block as a fake slab object.
    fn fake_obj() -> *mut u8 {
        let layout = std::alloc::Layout::from_size_align(64, 8).unwrap();
        let ptr = unsafe { std::alloc::alloc(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        ptr
    }

    unsafe fn free_obj(ptr: *mut u8) {
        let layout = std::alloc::Layout::from_size_align(64, 8).unwrap();
        unsafe { std::alloc::dealloc(ptr, layout) };
    }

    #[test]
    fn empty_list() {
        let list = CrossCpuFreeList::new();
        assert!(list.is_empty());
        assert!(list.take_all().is_null());
    }

    #[test]
    fn push_and_take_single() {
        let list = CrossCpuFreeList::new();
        let obj = fake_obj();
        unsafe { list.push(obj) };
        assert!(!list.is_empty());

        let chain = list.take_all();
        assert_eq!(chain, obj);
        // Only element — next must be null.
        let next = unsafe { chain.cast::<*mut u8>().read() };
        assert!(next.is_null());
        // List drained.
        assert!(list.is_empty());

        unsafe { free_obj(obj) };
    }

    #[test]
    fn push_multiple_and_collect_all() {
        let list = CrossCpuFreeList::new();
        let mut objs: Vec<*mut u8> = (0..10).map(|_| fake_obj()).collect();

        for &obj in &objs {
            unsafe { list.push(obj) };
        }

        let chain = list.take_all();
        assert!(list.is_empty());

        let collected: Vec<*mut u8> = unsafe { ChainIter::new(chain) }.collect();
        assert_eq!(collected.len(), 10);
        // Every pushed object must appear exactly once.
        objs.sort();
        let mut actual: Vec<*mut u8> = collected.clone();
        actual.sort();
        assert_eq!(objs, actual);

        for obj in objs {
            unsafe { free_obj(obj) };
        }
    }

    #[test]
    fn concurrent_push_single_collect() {
        let list = Arc::new(CrossCpuFreeList::new());
        let n_threads = 8;
        let n_per_thread = 100;

        let handles: Vec<_> = (0..n_threads)
            .map(|_| {
                let list = Arc::clone(&list);
                thread::spawn(move || {
                    let mut my_objs = Vec::new();
                    for _ in 0..n_per_thread {
                        let obj = fake_obj();
                        my_objs.push(obj as usize);
                        unsafe { list.push(obj) };
                    }
                    my_objs
                })
            })
            .collect();

        let mut all_pushed: Vec<usize> = Vec::new();
        for h in handles {
            all_pushed.extend(h.join().unwrap());
        }

        let chain = list.take_all();
        let collected: Vec<usize> = unsafe { ChainIter::new(chain) }
            .map(|p| p as usize)
            .collect();

        assert_eq!(collected.len(), n_threads * n_per_thread);

        let mut expected = all_pushed.clone();
        expected.sort();
        let mut actual = collected;
        actual.sort();
        assert_eq!(expected, actual);

        for addr in all_pushed {
            unsafe { free_obj(addr as *mut u8) };
        }
    }

    #[test]
    fn interleaved_push_and_take() {
        let list = CrossCpuFreeList::new();

        // Batch 1: push 5, take all.
        let batch1: Vec<*mut u8> = (0..5).map(|_| fake_obj()).collect();
        for &obj in &batch1 {
            unsafe { list.push(obj) };
        }
        let chain1: Vec<*mut u8> = unsafe { ChainIter::new(list.take_all()) }.collect();
        assert_eq!(chain1.len(), 5);

        // Batch 2: push 3, take all.
        let batch2: Vec<*mut u8> = (0..3).map(|_| fake_obj()).collect();
        for &obj in &batch2 {
            unsafe { list.push(obj) };
        }
        let chain2: Vec<*mut u8> = unsafe { ChainIter::new(list.take_all()) }.collect();
        assert_eq!(chain2.len(), 3);

        // No overlap.
        for p in &chain2 {
            assert!(!chain1.contains(p));
        }

        for obj in batch1.into_iter().chain(batch2) {
            unsafe { free_obj(obj) };
        }
    }

    #[test]
    fn chain_iter_empty() {
        let iter = unsafe { ChainIter::new(ptr::null_mut()) };
        assert_eq!(iter.count(), 0);
    }
}
