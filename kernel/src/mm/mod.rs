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

/// Physical address of the kernel's original PML4 (set once during mm::init).
/// Used by `new_process_page_table` and `restore_kernel_cr3` so they always
/// reference the bootloader-created page table rather than whatever CR3 happens
/// to be active when called (which could be a process's page table after fork).
static KERNEL_PML4_PHYS: Once<u64> = Once::new();

/// Returns the physical memory offset established during `mm::init`.
///
/// Panics if called before `mm::init`.
#[allow(dead_code)]
pub fn phys_offset() -> u64 {
    *PHYS_OFFSET.get().expect("mm not initialized")
}

/// Switch CR3 back to the kernel's original page table.
///
/// Called from process-exit paths (syscall handlers, fault trampolines) that
/// run while the current task's CR3 is still pointing at the dying process's
/// page table.  Restoring the kernel CR3 before yielding ensures that the
/// next scheduler-picked task starts with a consistent address space.
///
/// # Safety
///
/// Must only be called with interrupts disabled or inside a syscall handler
/// where re-entrancy is not a concern.  Only callable from ring 0 (Cr3::write
/// is a privileged operation).
pub fn restore_kernel_cr3() {
    use x86_64::{
        registers::control::{Cr3, Cr3Flags},
        structures::paging::PhysFrame,
        PhysAddr,
    };
    let phys = *KERNEL_PML4_PHYS.get().expect("mm not initialized");
    // SAFETY: phys is the bootloader's PML4 frame — always valid.
    unsafe {
        let frame =
            PhysFrame::from_start_address(PhysAddr::new(phys)).expect("kernel PML4 unaligned");
        Cr3::write(frame, Cr3Flags::empty());
    }
}

pub fn init(boot_info: &'static mut BootInfo) {
    // Capture the kernel's PML4 frame before any CR3 switches occur.
    {
        use x86_64::registers::control::Cr3;
        let (kpml4, _) = Cr3::read();
        KERNEL_PML4_PHYS.call_once(|| kpml4.start_address().as_u64());
    }

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
    frame_allocator::init(static_regions, phys_offset);

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
/// Allocates a new PML4 frame, zeroes it, then:
/// - Copies upper-half entries (256–511) from the current PML4 (kernel heap,
///   physical-memory offset mapping, etc.).
/// - Deep-copies PML4[0]'s PDPT and every PD table within it so the process
///   can reach kernel code at low virtual addresses (e.g. the trampoline at
///   0x1d9d0) after CR3 switch, while ELF-loader writes land in the process's
///   private PD instead of contaminating the shared kernel page structures.
///
/// Returns the physical frame of the new PML4, or `None` if frame allocation
/// fails.
#[allow(dead_code)]
pub fn new_process_page_table() -> Option<PhysFrame<Size4KiB>> {
    use x86_64::structures::paging::PageTableFlags;

    let phys_off = VirtAddr::new(phys_offset());

    // Allocate and zero the new PML4.
    let pml4_frame = frame_allocator::allocate_frame()?;
    let new_pml4_virt = phys_off + pml4_frame.start_address().as_u64();
    // SAFETY: frame is freshly allocated; no other reference exists.
    unsafe {
        core::ptr::write_bytes(new_pml4_virt.as_mut_ptr::<u8>(), 0, 4096);
    }

    // Always derive from the kernel's original PML4, not the current CR3.
    // If called from a syscall handler running with a process's CR3, Cr3::read()
    // would return the dying process's PML4 and the new process would inherit
    // its user-space mappings — causing map_to to fail with "already mapped".
    let kernel_pml4_phys = *KERNEL_PML4_PHYS.get().expect("mm not initialized");
    let cur_pml4_virt = phys_off + kernel_pml4_phys;

    // SAFETY: cur_pml4 is the kernel's PML4 (set during mm::init); new_pml4 is ours alone. All virtual
    // addresses are derived from the physical-memory offset established by mm::init.
    unsafe {
        let cur_pml4 = &*(cur_pml4_virt.as_ptr::<PageTable>());
        let new_pml4 = &mut *(new_pml4_virt.as_mut_ptr::<PageTable>());

        // Upper half (256–511): kernel heap, stacks, physmem offset mapping, etc.
        // Lower half (1–255): kernel binary + physical-memory mapping.
        // The kernel is linked at low addresses and the bootloader maps it via a
        // virtual-address offset (e.g. 0x10000000000 → PML4[2]).  Without copying
        // these entries the CPU triple-faults immediately after CR3 switch because
        // the kernel's next instruction is unreachable in the new address space.
        // ELF-loader user mappings always land in PML4[0] (USER_VADDR_MIN = 0x400000),
        // so shallow-copying PML4[1..256] never causes page-table contamination.
        for i in 1usize..512 {
            new_pml4[i] = cur_pml4[i].clone();
        }

        // PML4[0]: deep-copy the PDPT and each PD so the ELF loader can add user
        // entries (at USER_VADDR_MIN = 0x400000) to a process-private PD rather
        // than the shared kernel page structures.  If the kernel's PML4[0] is not
        // present (common case: kernel binary is in PML4[2]), this block is skipped
        // and the ELF loader creates a fresh PDPT/PD chain for the user mapping.
        let p4e = &cur_pml4[0];
        if p4e.flags().contains(PageTableFlags::PRESENT) {
            let pdpt_frame = frame_allocator::allocate_frame()?;
            let new_pdpt_virt = phys_off + pdpt_frame.start_address().as_u64();
            core::ptr::write_bytes(new_pdpt_virt.as_mut_ptr::<u8>(), 0, 4096);

            let cur_pdpt = &*(phys_off + p4e.addr().as_u64()).as_ptr::<PageTable>();
            let new_pdpt = &mut *new_pdpt_virt.as_mut_ptr::<PageTable>();

            for j in 0usize..512 {
                let p3e = &cur_pdpt[j];
                if !p3e.flags().contains(PageTableFlags::PRESENT) {
                    continue;
                }
                if p3e.flags().contains(PageTableFlags::HUGE_PAGE) {
                    // 1 GiB huge page: no sub-table to contaminate; copy as-is.
                    new_pdpt[j] = p3e.clone();
                    continue;
                }
                // Non-huge PDPT entry: deep-copy its PD so the ELF loader can
                // add user-space entries without touching the kernel's PD.
                let pd_frame = frame_allocator::allocate_frame()?;
                let new_pd_virt = phys_off + pd_frame.start_address().as_u64();
                core::ptr::write_bytes(new_pd_virt.as_mut_ptr::<u8>(), 0, 4096);

                let cur_pd = &*(phys_off + p3e.addr().as_u64()).as_ptr::<PageTable>();
                let new_pd = &mut *new_pd_virt.as_mut_ptr::<PageTable>();

                // Copy all PD entries: kernel huge-page/4 KiB entries carry over;
                // user entries (USER_VADDR_MIN+) will be populated by the ELF loader.
                for k in 0usize..512 {
                    new_pd[k] = cur_pd[k].clone();
                }

                // Ensure USER_ACCESSIBLE on the intermediate entry so the CPU can
                // follow the walk to user-mapped pages within this PDPT slot.
                new_pdpt[j].set_addr(
                    pd_frame.start_address(),
                    p3e.flags()
                        | PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::USER_ACCESSIBLE,
                );
            }

            // Point PML4[0] at the private PDPT with USER_ACCESSIBLE so the CPU
            // can walk to user-mapped pages in the lower half.
            new_pml4[0].set_addr(
                pdpt_frame.start_address(),
                p4e.flags()
                    | PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::USER_ACCESSIBLE,
            );
        }
    }

    Some(pml4_frame)
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
