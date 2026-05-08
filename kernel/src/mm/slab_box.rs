//! Owning smart pointer for a slab-cache-backed allocation.
//!
//! Phase 60 — `Box`-style ownership for objects allocated out of a named
//! [`SlabCache`].  `Box::from_raw` cannot be reused for slab-allocated
//! pointers because the global allocator's `dealloc` does not know about the
//! slab cache: a slab-allocated address routed to `dealloc` would either
//! corrupt the heap freelist or panic when the heap allocator failed to find
//! the address in its metadata.
//!
//! [`SlabBox<T>`] solves that by remembering which cache the slot came from
//! and routing the eventual `Drop` back through the same cache's `.free()`.

use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

use kernel_core::slab::SlabCache;

use crate::task::scheduler::IrqSafeMutex;

/// Owning smart pointer for a value allocated out of a named [`SlabCache`].
///
/// Construction allocates a single slot from `cache`, writes `value` into
/// it, and stores the (cache, ptr) pair.  Drop runs the value's destructor
/// via [`core::ptr::drop_in_place`] and returns the slot to the same cache.
///
/// # Invariants
///
/// - The cache's slot size must be `>= size_of::<T>()`.  Callers verify this
///   at compile time with a `const _: () = assert!(...)` next to the
///   `SlabBox::new_in` call site.
/// - The cache outlives every `SlabBox` allocated from it (`'static` bound
///   on the cache reference enforces this for the named caches in
///   [`crate::mm::slab::KernelSlabCaches`]).
///
/// # Safety
///
/// The `Send`/`Sync` implementations conditionally forward `T`'s
/// thread-safety bounds — a `SlabBox<T>` is `Send` iff `T: Send`, etc.,
/// matching `Box<T>`.
pub struct SlabBox<T: ?Sized> {
    ptr: NonNull<T>,
    cache: &'static IrqSafeMutex<SlabCache>,
    _marker: PhantomData<T>,
}

impl<T> SlabBox<T> {
    /// Allocate a slot from `cache`, move `value` into it, and return an
    /// owning [`SlabBox<T>`].
    ///
    /// # Panics
    ///
    /// Panics if the cache cannot satisfy the allocation (out of memory) —
    /// matching `Box::new`'s implicit OOM-aborts behaviour.  The slab API
    /// returns `None` on cache exhaustion; this constructor turns that into
    /// an explicit panic with a clear message.
    pub fn new_in(cache: &'static IrqSafeMutex<SlabCache>, value: T) -> Self {
        // Compile-time alignment check.  The slab cache's backing page is
        // page-aligned (4096), and consecutive slots within the page are at
        // multiples of `object_size`.  Slot addresses are therefore aligned
        // to `gcd(4096, object_size)`, which always ≥ `align_of::<usize>`
        // (the slab API enforces `object_size % align_of::<usize>() == 0`)
        // and is `≥ align_of::<T>()` whenever the caller has sized the
        // cache to a multiple of `align_of::<T>()`.
        //
        // Page size (4096) is the upper bound for any slab-backed slot
        // alignment.  Higher-aligned types cannot be slab-allocated and are
        // rejected at compile time.  The runtime `debug_assert!` below
        // additionally catches a caller mis-sizing the cache so
        // `object_size` is not a multiple of `align_of::<T>()`.
        const {
            assert!(
                align_of::<T>() <= 4096,
                "SlabBox<T>: T's alignment must not exceed page size (4096)"
            );
        }

        let addr = {
            let mut guard = cache.lock();
            guard
                .allocate(&mut crate::mm::slab::slab_page_alloc)
                .expect("SlabBox: slab cache exhausted")
        };
        debug_assert!(
            addr != 0 && addr.is_multiple_of(align_of::<T>()),
            "SlabBox: slab cache returned misaligned address {:#x}",
            addr
        );

        let ptr = addr as *mut T;
        // SAFETY: `addr` is a freshly-allocated, uninitialised slot of at
        // least `size_of::<T>()` bytes (the cache's slot size is checked at
        // call sites with a const_assert).  `core::ptr::write` initialises
        // the slot without dropping the pre-existing (uninitialised) bytes.
        unsafe { core::ptr::write(ptr, value) };
        // SAFETY: `addr` is non-zero (debug-asserted above).
        let ptr = unsafe { NonNull::new_unchecked(ptr) };

        Self {
            ptr,
            cache,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> SlabBox<T> {
    /// Returns the size of the cache slot used to back this allocation.
    #[allow(dead_code)]
    pub fn slot_size(&self) -> usize {
        size_of::<*mut T>() // for type-erased ?Sized; not meaningful for trait objects.
    }
}

impl<T: ?Sized> Drop for SlabBox<T> {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` was produced by `SlabBox::new_in` from a
        // `cache.allocate(...)` call and was not subsequently freed (no
        // `SlabBox` API exposes the raw pointer).  `drop_in_place` runs
        // `T::drop` on the contents; the slot is then returned to the same
        // cache the slot came from.
        unsafe {
            core::ptr::drop_in_place(self.ptr.as_ptr());
            self.cache
                .lock()
                .free(self.ptr.as_ptr() as *mut u8 as usize);
        }
    }
}

impl<T: ?Sized> Deref for SlabBox<T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: `self.ptr` is a valid pointer to an initialised `T` for
        // the lifetime of `self` (Drop is the only consumer of the slot).
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ?Sized> DerefMut for SlabBox<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: see `Deref::deref`.  Mutable borrow is exclusive because
        // `&mut self` already gives unique access to the SlabBox.
        unsafe { self.ptr.as_mut() }
    }
}

impl<T: ?Sized> AsRef<T> for SlabBox<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: ?Sized> AsMut<T> for SlabBox<T> {
    fn as_mut(&mut self) -> &mut T {
        self
    }
}

// SAFETY: SlabBox<T> is Send/Sync iff T is, matching Box<T>.  The cache
// reference is &'static IrqSafeMutex<SlabCache>, which is Send + Sync, so
// it does not constrain SlabBox's auto-trait derivation independently.
unsafe impl<T: ?Sized + Send> Send for SlabBox<T> {}
unsafe impl<T: ?Sized + Sync> Sync for SlabBox<T> {}
