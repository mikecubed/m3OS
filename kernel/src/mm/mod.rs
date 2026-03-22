pub mod debug;
pub mod frame_allocator;
pub mod memory_map;

use bootloader_api::BootInfo;

pub fn init(boot_info: &'static mut BootInfo) {
    let regions = &*boot_info.memory_regions;

    // SAFETY: The bootloader guarantees this slice is valid for the lifetime of the kernel.
    // We transmute to 'static because BootInfo is &'static mut BootInfo.
    let static_regions: &'static [bootloader_api::info::MemoryRegion] =
        unsafe { core::mem::transmute(regions) };

    memory_map::init(static_regions);
    frame_allocator::init(static_regions);

    log::info!("[mm] Memory subsystem initialized");
}
