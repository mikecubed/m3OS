pub mod debug;
pub mod frame_allocator;
pub mod heap;
pub mod memory_map;
pub mod paging;
pub mod user_space;

use bootloader_api::BootInfo;
use spin::Once;

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
