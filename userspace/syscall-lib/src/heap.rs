//! Simple userspace heap allocator using brk syscall.
//!
//! Uses a linked-list free list with first-fit allocation.
//! Enable with the `alloc` feature flag.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};

/// Minimum block size (header + at least 8 bytes of data).
const MIN_BLOCK: usize = core::mem::size_of::<BlockHeader>() + 8;

/// Free block header stored at the start of each free block.
#[repr(C)]
struct BlockHeader {
    /// Size of the usable region (excludes header).
    size: usize,
    /// Pointer to the next free block, or null.
    next: *mut BlockHeader,
}

/// A simple brk-based allocator with a free list.
pub struct BrkAllocator {
    /// Start of the free list (first free block).
    free_list: core::cell::UnsafeCell<*mut BlockHeader>,
    /// Current program break (top of heap).
    brk_current: AtomicU64,
    /// Whether brk has been initialized.
    initialized: core::cell::UnsafeCell<bool>,
}

// Safety: Single-threaded userspace processes.
unsafe impl Sync for BrkAllocator {}

impl Default for BrkAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl BrkAllocator {
    pub const fn new() -> Self {
        BrkAllocator {
            free_list: core::cell::UnsafeCell::new(core::ptr::null_mut()),
            brk_current: AtomicU64::new(0),
            initialized: core::cell::UnsafeCell::new(false),
        }
    }

    unsafe fn init(&self) {
        unsafe {
            let initialized = &mut *self.initialized.get();
            if *initialized {
                return;
            }
            // Query current brk
            let cur = crate::brk(0);
            self.brk_current.store(cur, Ordering::Relaxed);
            *initialized = true;
        }
    }

    /// Grow the heap by at least `size` bytes (aligned to page size).
    /// Returns a pointer to the start of the new region.
    unsafe fn grow(&self, size: usize) -> *mut u8 {
        let cur = self.brk_current.load(Ordering::Relaxed);
        // Align up to page boundary
        let needed = (size + 0xFFF) & !0xFFF;
        let new_brk = cur + needed as u64;
        let result = crate::brk(new_brk);
        if result < new_brk {
            return core::ptr::null_mut(); // OOM
        }
        self.brk_current.store(result, Ordering::Relaxed);
        cur as *mut u8
    }
}

unsafe impl GlobalAlloc for BrkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            self.init();

            let size = layout.size().max(8);
            let align = layout.align().max(core::mem::align_of::<BlockHeader>());
            let total_needed = size + core::mem::size_of::<BlockHeader>();

            // Search free list for a fitting block (first-fit).
            let free_list = &mut *self.free_list.get();
            let mut prev: *mut *mut BlockHeader = free_list;
            let mut cur = *prev;

            while !cur.is_null() {
                let block = &mut *cur;
                let data_ptr = (cur as *mut u8).add(core::mem::size_of::<BlockHeader>());
                let aligned = align_up(data_ptr as usize, align) as *mut u8;
                let offset = aligned as usize - data_ptr as usize;

                if block.size >= size + offset {
                    // Remove from free list
                    *prev = block.next;

                    // Write the actual block size just before the aligned pointer
                    // so dealloc can find it.
                    let header_ptr = (aligned as *mut BlockHeader).sub(1);
                    (*header_ptr).size = block.size - offset;
                    (*header_ptr).next = core::ptr::null_mut();

                    return aligned;
                }

                prev = &mut (*cur).next;
                cur = (*cur).next;
            }

            // No free block found — grow the heap.
            let alloc_size = total_needed.max(MIN_BLOCK);
            let alloc_size_aligned = (alloc_size + align - 1) & !(align - 1);
            // Add extra for alignment
            let raw = self.grow(alloc_size_aligned + align);
            if raw.is_null() {
                return core::ptr::null_mut();
            }

            // Place header at aligned position
            let data_start = raw.add(core::mem::size_of::<BlockHeader>());
            let aligned = align_up(data_start as usize, align) as *mut u8;
            let header_ptr = (aligned as *mut BlockHeader).sub(1);
            (*header_ptr).size = alloc_size_aligned + align - (aligned as usize - raw as usize);
            (*header_ptr).next = core::ptr::null_mut();

            aligned
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe {
            if ptr.is_null() {
                return;
            }

            // Read back the header just before the pointer.
            let header = (ptr as *mut BlockHeader).sub(1);

            // Add to front of free list.
            let free_list = &mut *self.free_list.get();
            (*header).next = *free_list;
            *free_list = header;
        }
    }
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
