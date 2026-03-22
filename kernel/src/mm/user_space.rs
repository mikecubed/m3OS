//! Helpers for mapping userspace memory regions into the kernel page tables.
//!
//! Phase 5 uses a shared address space (kernel + user pages in the same PML4).
//! User pages are mapped with the USER_ACCESSIBLE flag so ring-3 code can access them.
//! Kernel pages remain inaccessible from ring 3 due to their page-table permissions.

// These items are public API for Phase 5 integration; callers are added in a later
// track (main.rs wiring).  Suppress dead-code lints without weakening -D warnings.
#![allow(dead_code)]

use x86_64::{
    structures::paging::{Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB},
    VirtAddr,
};

use super::{frame_allocator, paging::GlobalFrameAlloc};

/// Virtual base address where userspace code is loaded.
pub const USER_CODE_BASE: u64 = 0x0000_0000_0040_0000; // 4 MiB

/// Number of pages to reserve for userspace code.
pub const USER_CODE_PAGES: u64 = 4; // 16 KiB max

/// Virtual address of userspace stack top.
pub const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000; // 2 GiB

/// Number of pages for userspace stack.
pub const USER_STACK_PAGES: u64 = 4; // 16 KiB

/// Map `n` pages of physical memory at `virt_base` with user-accessible flags.
///
/// Allocates fresh physical frames for each page.
///
/// # Safety
/// `mapper` must be the currently-active page table and `virt_base` must not
/// already be mapped.  `virt_base` must be 4 KiB-aligned; misaligned bases
/// cause `Page::containing_address` to round down and map the wrong page.
///
/// # Error handling
/// If `map_to` fails after a frame has been allocated, that frame is leaked
/// (the frame allocator does not support deallocation in Phase 5).  A mapping
/// failure at boot is unrecoverable regardless.
pub unsafe fn map_user_pages(
    mapper: &mut OffsetPageTable,
    virt_base: u64,
    n: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    debug_assert!(
        virt_base.is_multiple_of(4096),
        "map_user_pages: virt_base must be 4 KiB-aligned"
    );
    let mut alloc = GlobalFrameAlloc;
    for i in 0..n {
        let vaddr = VirtAddr::new(virt_base + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        let frame = frame_allocator::allocate_frame().ok_or("out of physical frames")?;
        // Safety: frame is freshly allocated, vaddr is within user range.
        mapper
            .map_to(page, frame, flags, &mut alloc)
            .map_err(|_| "map_to failed")?
            .flush();
    }
    Ok(())
}

/// Map a contiguous run of physical frames (e.g. for embedded code bytes) at `virt_base`.
///
/// Unlike `map_user_pages`, this maps the **given** physical frames rather than
/// allocating new ones.  Used to map the embedded hello binary at its load address.
///
/// # Safety
/// `virt_base` must be 4 KiB-aligned; misaligned bases cause
/// `Page::containing_address` to round down and map the wrong page.
pub unsafe fn map_user_frames(
    mapper: &mut OffsetPageTable,
    virt_base: u64,
    frames: &[PhysFrame<Size4KiB>],
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    debug_assert!(
        virt_base.is_multiple_of(4096),
        "map_user_frames: virt_base must be 4 KiB-aligned"
    );
    let mut alloc = GlobalFrameAlloc;
    for (i, &frame) in frames.iter().enumerate() {
        let vaddr = VirtAddr::new(virt_base + i as u64 * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        mapper
            .map_to(page, frame, flags, &mut alloc)
            .map_err(|_| "map_to failed")?
            .flush();
    }
    Ok(())
}

/// Copy `src` bytes into the user code region at `virt_base`.
///
/// The region must already be mapped (e.g. via `map_user_pages`).
/// The bounds check is sized to the **code** region (`USER_CODE_PAGES * 4096`);
/// do not use this helper to write to the stack or any other user region.
pub fn copy_to_user(virt_base: u64, src: &[u8]) -> Result<(), &'static str> {
    let max_bytes = (USER_CODE_PAGES * 4096) as usize;
    if src.len() > max_bytes {
        return Err("copy_to_user: src exceeds mapped user code region");
    }
    // Safety: caller guarantees virt_base is mapped and we've checked the length.
    let dst = unsafe { core::slice::from_raw_parts_mut(virt_base as *mut u8, src.len()) };
    dst.copy_from_slice(src);
    Ok(())
}

/// Set up code and stack regions for a userspace process.
///
/// Maps USER_CODE_PAGES pages at USER_CODE_BASE (read-write + executable, user-accessible)
/// and USER_STACK_PAGES pages below USER_STACK_TOP (read-write, no-execute, user-accessible).
///
/// Code pages are mapped writable so the kernel can copy the binary before `iretq`
/// (CR0.WP blocks ring-0 writes to read-only pages).  W^X enforcement is deferred to Phase 6+.
///
/// Returns `Ok(())` on success, or an error string if any mapping operation fails.
pub unsafe fn setup_user_memory(mapper: &mut OffsetPageTable) -> Result<(), &'static str> {
    // Code: user-accessible, present, writable (no NO_EXECUTE flag → executable).
    // WRITABLE is required so the kernel can copy the binary into these pages
    // before iretq (CR0.WP prevents ring-0 writes to read-only pages).
    // W^X (write-xor-execute) enforcement is deferred to Phase 6+ when a proper
    // ELF loader will let us separate the copy step from the execute step.
    let code_flags =
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    // Stack: user-accessible, present, writable, no-execute
    let stack_flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;

    map_user_pages(mapper, USER_CODE_BASE, USER_CODE_PAGES, code_flags)?;
    map_user_pages(
        mapper,
        USER_STACK_TOP - USER_STACK_PAGES * 4096,
        USER_STACK_PAGES,
        stack_flags,
    )?;
    Ok(())
}
