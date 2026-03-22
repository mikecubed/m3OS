use spin::Once;
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

static INIT_GUARD: Once<()> = Once::new();

/// Initialise the kernel's page-table mapper.
///
/// # Safety
/// - `physical_memory_offset` must be the value from `BootInfo` — it must be
///   the virtual address at which all physical memory is identity-mapped by the
///   bootloader.
/// - **Must be called exactly once.** A second call panics to prevent aliased
///   `&'static mut` references to the active L4 table (which would be UB).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
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

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;
    let (level_4_table_frame, _) = Cr3::read();
    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    &mut *page_table_ptr
}

/// A frame allocator wrapper that delegates to the global bump allocator.
/// Used during heap setup so `map_to` has a frame allocator to call.
pub struct GlobalFrameAlloc;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAlloc {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        super::frame_allocator::allocate_frame()
    }
}
