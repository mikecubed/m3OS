use spin::Once;
use x86_64::VirtAddr;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, OffsetPageTable, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};

static INIT_GUARD: Once<()> = Once::new();

/// Initialise the kernel's page-table mapper.
///
/// # Safety
/// - `physical_memory_offset` must be the value from `BootInfo` — it must be
///   the virtual base address of the bootloader's physical-memory offset mapping
///   (i.e. a physical address `P` is accessible at `physical_memory_offset + P`).
/// - **Must be called exactly once.** A second call panics to prevent aliased
///   `&'static mut` references to the active L4 table (which would be UB).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    unsafe {
        assert!(
            INIT_GUARD.get().is_none(),
            "paging::init called more than once"
        );
        INIT_GUARD.call_once(|| ());

        let level_4_table = active_level_4_table(physical_memory_offset);
        let mapper = OffsetPageTable::new(level_4_table, physical_memory_offset);
        log::info!("[mm] page tables initialized");
        mapper
    }
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    unsafe {
        use x86_64::registers::control::Cr3;
        let (level_4_table_frame, _) = Cr3::read();
        let phys = level_4_table_frame.start_address();
        let virt = physical_memory_offset + phys.as_u64();
        let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
        &mut *page_table_ptr
    }
}

/// Reconstruct the page-table mapper from the stored physical memory offset.
///
/// `OffsetPageTable::new` wraps the currently-active CR3 page table.  Only one
/// mapper may be live at a time — the caller must not hold another mapper when
/// calling this function.
///
/// # Safety
///
/// Aliasing `&mut PageTable` is UB.  Call only when no other `OffsetPageTable`
/// is alive (e.g. after `mm::init` has returned and dropped its local mapper).
pub unsafe fn get_mapper() -> OffsetPageTable<'static> {
    unsafe {
        let phys_offset = x86_64::VirtAddr::new(super::phys_offset());
        let level_4_table = active_level_4_table(phys_offset);
        OffsetPageTable::new(level_4_table, phys_offset)
    }
}

unsafe fn zero_frame(phys_offset: VirtAddr, frame: PhysFrame<Size4KiB>) {
    unsafe {
        core::ptr::write_bytes(
            (phys_offset + frame.start_address().as_u64()).as_mut_ptr::<u8>(),
            0,
            4096,
        );
    }
}

unsafe fn table_mut(phys_offset: VirtAddr, table_phys: u64) -> &'static mut PageTable {
    unsafe { &mut *((phys_offset + table_phys).as_mut_ptr::<PageTable>()) }
}

fn free_unlinked_table(frame: Option<PhysFrame<Size4KiB>>) {
    if let Some(frame) = frame {
        super::frame_allocator::free_frame(frame.start_address().as_u64());
    }
}

unsafe fn map_current_user_page_inner(
    vaddr: VirtAddr,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    unsafe {
        let phys_offset = VirtAddr::new(super::phys_offset());
        let (cr3_frame, _) = Cr3::read();
        let pml4_phys = cr3_frame.start_address().as_u64();

        let vaddr_u64 = vaddr.as_u64();
        let p4_idx = ((vaddr_u64 >> 39) & 0x1FF) as usize;
        let p3_idx = ((vaddr_u64 >> 30) & 0x1FF) as usize;
        let p2_idx = ((vaddr_u64 >> 21) & 0x1FF) as usize;
        let p1_idx = ((vaddr_u64 >> 12) & 0x1FF) as usize;

        let user_flags =
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

        let pml4 = table_mut(phys_offset, pml4_phys);

        let new_pdpt = if pml4[p4_idx].flags().contains(PageTableFlags::PRESENT) {
            None
        } else {
            let frame = match super::frame_allocator::allocate_frame() {
                Some(frame) => frame,
                None => return Err("out of physical frames"),
            };
            zero_frame(phys_offset, frame);
            Some(frame)
        };
        let pdpt_phys = new_pdpt
            .as_ref()
            .map(|frame| frame.start_address().as_u64())
            .unwrap_or_else(|| pml4[p4_idx].addr().as_u64());
        let pdpt = table_mut(phys_offset, pdpt_phys);

        let new_pd = if pdpt[p3_idx].flags().contains(PageTableFlags::PRESENT) {
            None
        } else {
            let frame = match super::frame_allocator::allocate_frame() {
                Some(frame) => frame,
                None => {
                    free_unlinked_table(new_pdpt);
                    return Err("out of physical frames");
                }
            };
            zero_frame(phys_offset, frame);
            Some(frame)
        };
        let pd_phys = new_pd
            .as_ref()
            .map(|frame| frame.start_address().as_u64())
            .unwrap_or_else(|| pdpt[p3_idx].addr().as_u64());
        let pd = table_mut(phys_offset, pd_phys);

        let new_pt = if pd[p2_idx].flags().contains(PageTableFlags::PRESENT) {
            None
        } else {
            let frame = match super::frame_allocator::allocate_frame() {
                Some(frame) => frame,
                None => {
                    free_unlinked_table(new_pd);
                    free_unlinked_table(new_pdpt);
                    return Err("out of physical frames");
                }
            };
            zero_frame(phys_offset, frame);
            Some(frame)
        };
        let pt_phys = new_pt
            .as_ref()
            .map(|frame| frame.start_address().as_u64())
            .unwrap_or_else(|| pd[p2_idx].addr().as_u64());
        let pt = table_mut(phys_offset, pt_phys);

        let existing_flags = pt[p1_idx].flags();
        if existing_flags.contains(PageTableFlags::PRESENT)
            || existing_flags.contains(PageTableFlags::BIT_10)
        {
            free_unlinked_table(new_pt);
            free_unlinked_table(new_pd);
            free_unlinked_table(new_pdpt);
            return Err("page already mapped");
        }

        pt[p1_idx].set_addr(frame.start_address(), flags);
        if let Some(frame) = new_pt {
            pd[p2_idx].set_addr(frame.start_address(), user_flags);
        }
        if let Some(frame) = new_pd {
            pdpt[p3_idx].set_addr(frame.start_address(), user_flags);
        }
        if let Some(frame) = new_pdpt {
            pml4[p4_idx].set_addr(frame.start_address(), user_flags);
        }

        x86_64::instructions::tlb::flush(vaddr);
        Ok(())
    }
}

/// Transactionally map a single user page into the current CR3 while the
/// caller already holds the shared address-space mutation lock.
///
/// # Safety
///
/// The caller must hold `AddressSpace::lock_page_tables()` for the current CR3
/// and ensure no other live `OffsetPageTable` aliases this page table walk.
pub unsafe fn map_current_user_page_locked(
    vaddr: VirtAddr,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
) -> Result<(), &'static str> {
    unsafe { map_current_user_page_inner(vaddr, frame, flags) }
}

/// A frame allocator wrapper that delegates to the global bump allocator.
/// Used during heap setup so `map_to` has a frame allocator to call.
pub struct GlobalFrameAlloc;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAlloc {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        super::frame_allocator::allocate_frame()
    }
}
