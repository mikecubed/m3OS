use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use spin::Once;

static MEMORY_REGIONS: Once<&'static [MemoryRegion]> = Once::new();

pub fn init(regions: &'static [MemoryRegion]) {
    MEMORY_REGIONS.call_once(|| regions);

    let total = regions.len();
    let mut usable = 0usize;

    for region in regions {
        let size_kb = (region.end - region.start) / 1024;
        log::debug!(
            "[mm] region: {:?} start={:#x} end={:#x} size={}KB",
            region.kind,
            region.start,
            region.end,
            size_kb
        );
        if region.kind == MemoryRegionKind::Usable {
            usable += 1;
        }
    }

    log::info!(
        "[mm] memory map: {} usable regions out of {} total",
        usable,
        total
    );
}

pub fn regions() -> &'static [MemoryRegion] {
    MEMORY_REGIONS.get().expect("memory_map::init not called")
}
