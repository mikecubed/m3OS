//! Helpers for mapping userspace memory regions into the kernel page tables.
//!
//! Phase 5 uses a shared address space (kernel + user pages in the same PML4).
//! User pages are mapped with the USER_ACCESSIBLE flag so ring-3 code can access them.
//! Kernel pages remain inaccessible from ring 3 due to their page-table permissions.

// These items are public API for Phase 5 integration; callers are added in a later
// track (main.rs wiring).  Suppress dead-code lints without weakening -D warnings.
#![allow(dead_code)]

use x86_64::{
    VirtAddr,
    structures::paging::{Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB},
};

use super::{frame_allocator, paging::GlobalFrameAlloc};

/// Intermediate (PDPT / PD / PT) page-table entries for **user** mappings must
/// always be `PRESENT | WRITABLE | USER_ACCESSIBLE`, regardless of the leaf
/// PTE's permissions.
///
/// On x86-64 the effective permission of a 4 KiB page is the AND of the
/// WRITABLE/USER bits across every paging level, but m3OS uses the **leaf** PTE
/// as the sole permission arbiter — W^X, the CoW marker (BIT_9), and PKU keys
/// all live on the leaf. The `x86_64` crate's default [`Mapper::map_to`] derives
/// its parent-table flags from the *leaf* flags (`flags & (PRESENT | WRITABLE |
/// USER_ACCESSIBLE)`), so an eager `PROT_READ` file mmap (leaf = `PRESENT|USER`,
/// no WRITABLE) would create **non-writable** intermediate tables. A later
/// writable anonymous mmap that reuses that same 1 GiB/2 MiB region then faults
/// `present+write` *forever*: the intermediate ANDs away WRITABLE while the leaf
/// reads as writable, so the page-fault handler's spurious-write recovery (which
/// only inspects the leaf) loops. (Observed: rustc's mallocng `alloc_group`
/// write spinning 165 M times on one address — Phase 95b.) Always pass these
/// explicit parent flags via [`Mapper::map_to_with_table_flags`].
pub(crate) const USER_PARENT_TABLE_FLAGS: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .union(PageTableFlags::USER_ACCESSIBLE);

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
    unsafe {
        debug_assert!(
            virt_base.is_multiple_of(4096),
            "map_user_pages: virt_base must be 4 KiB-aligned"
        );
        let mut alloc = GlobalFrameAlloc;
        for i in 0..n {
            let vaddr = VirtAddr::new(virt_base + i * 4096);
            let page: Page<Size4KiB> = Page::containing_address(vaddr);
            // Zero-before-exposure: user-visible frame must be zeroed.
            let frame = frame_allocator::allocate_frame_zeroed().ok_or("out of physical frames")?;
            // Safety: frame is freshly allocated and zeroed, vaddr is within user range.
            // Force PRESENT|WRITABLE|USER on the intermediate tables (see
            // `USER_PARENT_TABLE_FLAGS`) so the leaf — not a RO/supervisor parent
            // inherited from the default `map_to` — governs the page permission.
            mapper
                .map_to_with_table_flags(page, frame, flags, USER_PARENT_TABLE_FLAGS, &mut alloc)
                .map_err(|_| "map_to failed")?
                .flush();
        }
        Ok(())
    }
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
    unsafe {
        debug_assert!(
            virt_base.is_multiple_of(4096),
            "map_user_frames: virt_base must be 4 KiB-aligned"
        );
        let mut alloc = GlobalFrameAlloc;
        let mut mapped: u64 = 0;
        for (i, &frame) in frames.iter().enumerate() {
            let vaddr = VirtAddr::new(virt_base + i as u64 * 4096);
            let page: Page<Size4KiB> = Page::containing_address(vaddr);
            // `map_to_with_table_flags` + `USER_PARENT_TABLE_FLAGS`: the eager
            // file-backed mmap path is the *primary* source of RO intermediate
            // tables (a PROT_READ DSO segment), so this is the root-cause fix for
            // the Phase 95b rustc anon-write fault loop.
            match mapper.map_to_with_table_flags(
                page,
                frame,
                flags,
                USER_PARENT_TABLE_FLAGS,
                &mut alloc,
            ) {
                Ok(flush) => {
                    flush.flush();
                    mapped += 1;
                }
                Err(_) => {
                    rollback_user_mapping(mapper, virt_base, mapped);
                    return Err("map_to failed");
                }
            }
        }
        Ok(())
    }
}

/// Map a contiguous run of `page_count` physical frames starting at
/// `base_phys` (which must be 4 KiB-aligned) into the user address
/// space at `virt_base`.
///
/// Equivalent to building a `[PhysFrame; page_count]` and calling
/// [`map_user_frames`], but without the heap allocation. For large
/// SHM mappings (a 4K framebuffer surface is 8 064 pages =
/// ~63 KiB of `PhysFrame` scratch) the per-call `Vec` is the leading
/// source of kernel-heap churn on every `sys_shm_map`, fragmenting
/// the buddy under 60 Hz client-attach traffic.
///
/// # Safety
/// `virt_base` and `base_phys` must both be 4 KiB-aligned; the
/// frames `[base_phys, base_phys + page_count * 4096)` must be
/// valid physical memory the caller is allowed to expose to the
/// target address space; `mapper` must be the address space that
/// will receive the mapping. Misaligned bases cause
/// `Page::containing_address` / `PhysFrame::from_start_address` to
/// round down and map the wrong page.
pub unsafe fn map_user_frames_contiguous(
    mapper: &mut OffsetPageTable,
    virt_base: u64,
    base_phys: u64,
    page_count: u64,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    unsafe {
        debug_assert!(
            virt_base.is_multiple_of(4096),
            "map_user_frames_contiguous: virt_base must be 4 KiB-aligned"
        );
        debug_assert!(
            base_phys.is_multiple_of(4096),
            "map_user_frames_contiguous: base_phys must be 4 KiB-aligned"
        );
        let mut alloc = GlobalFrameAlloc;
        for i in 0..page_count {
            let phys = x86_64::PhysAddr::new(base_phys + i * 4096);
            let frame = match PhysFrame::<Size4KiB>::from_start_address(phys) {
                Ok(f) => f,
                Err(_) => {
                    rollback_user_mapping(mapper, virt_base, i);
                    return Err("invalid base_phys alignment");
                }
            };
            let vaddr = VirtAddr::new(virt_base + i * 4096);
            let page: Page<Size4KiB> = Page::containing_address(vaddr);
            // See `USER_PARENT_TABLE_FLAGS`: intermediates stay writable+user.
            match mapper.map_to_with_table_flags(
                page,
                frame,
                flags,
                USER_PARENT_TABLE_FLAGS,
                &mut alloc,
            ) {
                Ok(flush) => flush.flush(),
                Err(_) => {
                    rollback_user_mapping(mapper, virt_base, i);
                    return Err("map_to failed");
                }
            }
        }
        Ok(())
    }
}

/// Unmap the first `mapped` pages of a partially-built mapping
/// starting at `virt_base`. Used by both
/// [`map_user_frames`] and [`map_user_frames_contiguous`] on the
/// failure path so a partial mapping doesn't leak page-table entries
/// when `map_to` fails part-way through. The rollback uses
/// integer arithmetic instead of a `Vec` of pushed pages — the
/// `map_to` loop above is strictly contiguous, so we know exactly
/// which `mapped` virtual addresses got installed.
///
/// # Safety
/// `mapper` must be the address space that received the partial
/// mapping; `virt_base` must be the same base that was passed to
/// the failing map call; `mapped` must not exceed the number of
/// pages actually installed.
unsafe fn rollback_user_mapping(mapper: &mut OffsetPageTable, virt_base: u64, mapped: u64) {
    for i in 0..mapped {
        let vaddr = VirtAddr::new(virt_base + i * 4096);
        let page: Page<Size4KiB> = Page::containing_address(vaddr);
        if let Ok((_frame, flush)) = mapper.unmap(page) {
            flush.flush();
        }
    }
}

// Phase 77 Track B: the legacy `copy_to_user` helper that lived here did a
// raw `from_raw_parts_mut(virt_base as *mut u8, ..)` write through the USER
// virtual address — the one direct user-page dereference left in ring 0 after
// every live path migrated to `mm::user_mem` (physmap-routed) and the modern
// ELF loader.  It had zero callers and, once `CR4.SMAP` is enabled, would
// `#PF` if ever resurrected.  Removed as part of the SMAP audit (the kernel
// is now SMAP-clean: all user-memory access reaches the bytes through the
// physical-memory direct map, never the user virtual address).

// Phase 75: the legacy `setup_user_memory` helper used to map user code
// pages with `PRESENT | WRITABLE | USER_ACCESSIBLE` (a W+X mapping) before
// the modern ELF loader (`mm::elf::load_elf_into`) took over every binary
// load path. The helper carried two `// W^X enforcement is deferred to
// Phase 6+` markers and had zero live callers by Phase 11. Removing it
// closes the audit-§E1 W+X dead-code hazard documented in
// `docs/roadmap/75-wx-enforcement.md`.
