pub mod debug;
pub mod elf;
pub mod frame_allocator;
pub mod heap;
pub mod memory_map;
pub mod paging;
pub mod user_mem;
pub mod user_space;

use bootloader_api::BootInfo;
use spin::Once;
use x86_64::{
    structures::paging::{OffsetPageTable, PageTable, PhysFrame, Size4KiB},
    VirtAddr,
};

static PHYS_OFFSET: Once<u64> = Once::new();

/// Returns the physical memory offset established during `mm::init`.
///
/// Panics if called before `mm::init`.
#[allow(dead_code)]
pub fn phys_offset() -> u64 {
    *PHYS_OFFSET.get().expect("mm not initialized")
}

pub fn init(boot_info: &'static mut BootInfo) {
    // End mutable access; coerce &'static mut → &'static so the borrow checker
    // tracks that we no longer hold exclusive access to BootInfo.
    let boot_info: &'static BootInfo = boot_info;

    // The bootloader guarantees this slice is valid for the kernel's lifetime.
    let static_regions: &'static [bootloader_api::info::MemoryRegion] = &boot_info.memory_regions;

    let phys_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("[mm] bootloader did not provide physical memory offset");

    // Store physical memory offset globally so other modules can rebuild the mapper.
    PHYS_OFFSET.call_once(|| phys_offset);

    memory_map::init(static_regions);
    frame_allocator::init(static_regions);

    // Log reserved regions below 1 MiB to confirm allocator skips them (P2-T008)
    debug::log_reserved_below_1mib();

    let mut mapper = unsafe { paging::init(x86_64::VirtAddr::new(phys_offset)) };
    heap::init_heap(&mut mapper, &mut paging::GlobalFrameAlloc);

    log::info!("[mm] Memory subsystem initialized");
}

// ---------------------------------------------------------------------------
// Per-process page table helpers (P11-T002 / P11-T013)
// ---------------------------------------------------------------------------

/// Create a fresh user-space page table that inherits all kernel mappings.
///
/// Allocates a new PML4 frame, zeroes it, then copies the upper-half entries
/// (indices 256–511) from the currently-active PML4 so the new process can
/// reach kernel code and data without a separate mapping step.
///
/// Returns the physical frame of the new PML4, or `None` if frame allocation
/// fails.
#[allow(dead_code)]
pub fn new_process_page_table() -> Option<PhysFrame<Size4KiB>> {
    use x86_64::registers::control::Cr3;

    let frame = frame_allocator::allocate_frame()?;
    let phys_off = VirtAddr::new(phys_offset());

    // Zero the new PML4 frame.
    let new_pml4_virt = phys_off + frame.start_address().as_u64();
    // SAFETY: frame is freshly allocated, no other reference exists.
    unsafe {
        core::ptr::write_bytes(new_pml4_virt.as_mut_ptr::<u8>(), 0, 4096);
    }

    // Copy kernel (upper-half) entries from the current PML4.
    let (cur_frame, _) = Cr3::read();
    let cur_pml4_virt = phys_off + cur_frame.start_address().as_u64();
    // SAFETY: cur_pml4 is the live PML4; new_pml4 is ours alone.
    unsafe {
        let cur_pml4: *const PageTable = cur_pml4_virt.as_ptr();
        let new_pml4: *mut PageTable = new_pml4_virt.as_mut_ptr();
        for i in 256usize..512 {
            (&mut (*new_pml4))[i] = (&(*cur_pml4))[i].clone();
        }
    }

    Some(frame)
}

/// Free all user-space page table frames for the given PML4 physical address.
///
/// Walks PML4 indices 0–255 (user half), frees every mapped user-accessible
/// physical frame, and frees the page-table structure frames themselves.
///
/// Frame reclamation is a stub in the bump allocator (no-op); the real benefit
/// today is correctness — the function documents the ownership transfer and
/// will become fully effective once Phase 13 adds a free list.
///
/// # Safety
///
/// `cr3_phys` must be the physical address of a valid, now-unreachable PML4
/// that is no longer loaded in CR3. No other code may access the page table
/// after this call.
#[allow(dead_code)]
pub fn free_process_page_table(cr3_phys: u64) {
    use x86_64::structures::paging::{PageTable, PageTableFlags};
    let phys_off = VirtAddr::new(phys_offset());
    // SAFETY: cr3_phys is a valid PML4 frame being freed. The caller guarantees
    // it is no longer active (not in CR3) and has exclusive ownership.
    unsafe {
        let pml4: &PageTable = &*(phys_off + cr3_phys).as_ptr::<PageTable>();
        for p4 in 0usize..256 {
            let p4e = &pml4[p4];
            if !p4e.flags().contains(PageTableFlags::PRESENT) {
                continue;
            }
            let pdpt: &PageTable = &*(phys_off + p4e.addr().as_u64()).as_ptr::<PageTable>();
            for p3 in 0usize..512 {
                let p3e = &pdpt[p3];
                if !p3e.flags().contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if p3e.flags().contains(PageTableFlags::HUGE_PAGE) {
                    continue;
                }
                let pd: &PageTable = &*(phys_off + p3e.addr().as_u64()).as_ptr::<PageTable>();
                for p2 in 0usize..512 {
                    let p2e = &pd[p2];
                    if !p2e.flags().contains(PageTableFlags::PRESENT) {
                        continue;
                    }
                    if p2e.flags().contains(PageTableFlags::HUGE_PAGE) {
                        continue;
                    }
                    let pt: &PageTable = &*(phys_off + p2e.addr().as_u64()).as_ptr::<PageTable>();
                    for p1 in 0usize..512 {
                        let pte = &pt[p1];
                        if !pte.flags().contains(PageTableFlags::PRESENT) {
                            continue;
                        }
                        if !pte.flags().contains(PageTableFlags::USER_ACCESSIBLE) {
                            continue;
                        }
                        frame_allocator::free_frame(pte.addr().as_u64());
                    }
                    frame_allocator::free_frame(p2e.addr().as_u64()); // free PT frame
                }
                frame_allocator::free_frame(p3e.addr().as_u64()); // free PD frame
            }
            frame_allocator::free_frame(p4e.addr().as_u64()); // free PDPT frame
        }
        frame_allocator::free_frame(cr3_phys); // free PML4 frame
    }
}

/// Build an `OffsetPageTable` mapper over an arbitrary PML4 frame.
///
/// Does **not** switch CR3, so the current address space remains active.
/// All page-table walks go through the physical-memory offset, allowing the
/// kernel to manipulate any process's page table without changing CR3.
///
/// # Safety
///
/// - `cr3_frame` must point to a valid, 4 KiB-aligned PML4.
/// - No other `OffsetPageTable` over the same frame may be alive at the same
///   time (aliasing `&mut PageTable` is UB).
/// - The physical memory offset must be valid (i.e. `mm::init` must have run).
#[allow(dead_code)]
pub unsafe fn mapper_for_frame(cr3_frame: PhysFrame<Size4KiB>) -> OffsetPageTable<'static> {
    let phys_off = VirtAddr::new(phys_offset());
    let pml4_virt = phys_off + cr3_frame.start_address().as_u64();
    let pml4: &'static mut PageTable = &mut *pml4_virt.as_mut_ptr();
    OffsetPageTable::new(pml4, phys_off)
}
