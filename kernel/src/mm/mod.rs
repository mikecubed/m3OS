pub mod debug;
pub mod frame_allocator;
pub mod heap;
pub mod memory_map;
pub mod paging;

use bootloader_api::BootInfo;

pub fn init(boot_info: &'static mut BootInfo) {
    let regions = &*boot_info.memory_regions;

    // SAFETY: The bootloader guarantees this slice is valid for the lifetime of the kernel.
    // We transmute to 'static because BootInfo is &'static mut BootInfo.
    let static_regions: &'static [bootloader_api::info::MemoryRegion] =
        unsafe { core::mem::transmute(regions) };

    let phys_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("[mm] bootloader did not provide physical memory offset");

    memory_map::init(static_regions);
    frame_allocator::init(static_regions);

    let mut mapper = unsafe { paging::init(x86_64::VirtAddr::new(phys_offset)) };
    heap::init_heap(&mut mapper, &mut paging::GlobalFrameAlloc);

    log::info!("[mm] Memory subsystem initialized");
}
