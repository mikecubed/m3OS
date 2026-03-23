use spin::Once;
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB};
use x86_64::VirtAddr;

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
#[allow(dead_code)] // used by userspace setup (Phase 5 / Phase 7+), not Phase 6
pub unsafe fn get_mapper() -> OffsetPageTable<'static> {
    let phys_offset = x86_64::VirtAddr::new(super::phys_offset());
    let level_4_table = active_level_4_table(phys_offset);
    OffsetPageTable::new(level_4_table, phys_offset)
}

/// A frame allocator wrapper that delegates to the global bump allocator.
/// Used during heap setup so `map_to` has a frame allocator to call.
pub struct GlobalFrameAlloc;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAlloc {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        super::frame_allocator::allocate_frame()
    }
}
