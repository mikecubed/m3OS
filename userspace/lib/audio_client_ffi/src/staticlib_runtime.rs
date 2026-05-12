//! Minimal runtime for the musl-static staticlib build of
//! `audio_mixer`. Provides a `#[panic_handler]` that aborts and a
//! `#[global_allocator]` that calls into musl's libc malloc/free.
//!
//! Only compiled for the `target_env = "musl"` staticlib build —
//! host tests and rlib consumers stay on the regular Rust std /
//! testing alloc / panic infrastructure.

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(p: *mut c_void);
    fn calloc(nmemb: usize, size: usize) -> *mut c_void;
    fn realloc(p: *mut c_void, size: usize) -> *mut c_void;
    fn abort() -> !;
}

struct LibcAllocator;

unsafe impl GlobalAlloc for LibcAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // libc malloc returns 16-byte-aligned memory on x86_64 which
        // satisfies most allocations. For larger alignment, fall back
        // to over-allocate-and-align (Tier 1 — DOOM never asks for
        // > 16-byte alignment via this path).
        if layout.align() <= 16 {
            unsafe { malloc(layout.size()) as *mut u8 }
        } else {
            // Over-allocate by `align` + word and store the original
            // pointer just before the aligned address so `dealloc` can
            // recover it.
            let extra = layout.align() + core::mem::size_of::<usize>();
            let raw = unsafe { malloc(layout.size() + extra) as *mut u8 };
            if raw.is_null() {
                return core::ptr::null_mut();
            }
            let unaligned = unsafe { raw.add(core::mem::size_of::<usize>()) } as usize;
            let aligned = (unaligned + layout.align() - 1) & !(layout.align() - 1);
            let aligned_ptr = aligned as *mut u8;
            unsafe {
                core::ptr::write(
                    aligned_ptr.sub(core::mem::size_of::<usize>()) as *mut usize,
                    raw as usize,
                );
            }
            aligned_ptr
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.align() <= 16 {
            unsafe { free(ptr as *mut c_void) }
        } else {
            let raw =
                unsafe { core::ptr::read(ptr.sub(core::mem::size_of::<usize>()) as *const usize) };
            unsafe { free(raw as *mut c_void) }
        }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= 16 {
            unsafe { calloc(1, layout.size()) as *mut u8 }
        } else {
            let p = unsafe { self.alloc(layout) };
            if !p.is_null() {
                unsafe {
                    core::ptr::write_bytes(p, 0, layout.size());
                }
            }
            p
        }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.align() <= 16 {
            unsafe { realloc(ptr as *mut c_void, new_size) as *mut u8 }
        } else {
            // Conservative path: alloc new + copy + free old.
            let new_layout = Layout::from_size_align(new_size, layout.align()).unwrap_or(layout);
            let np = unsafe { self.alloc(new_layout) };
            if !np.is_null() {
                let n = core::cmp::min(layout.size(), new_size);
                unsafe {
                    core::ptr::copy_nonoverlapping(ptr, np, n);
                    self.dealloc(ptr, layout);
                }
            }
            np
        }
    }
}

#[global_allocator]
static GLOBAL: LibcAllocator = LibcAllocator;

#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo<'_>) -> ! {
    unsafe { abort() }
}

#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}
