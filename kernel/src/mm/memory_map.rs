use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Once;

static MEMORY_REGIONS: Once<&'static [MemoryRegion]> = Once::new();
static MEMORY_MAP_INIT: AtomicBool = AtomicBool::new(false);

pub fn init(regions: &'static [MemoryRegion]) {
    assert!(
        !MEMORY_MAP_INIT.swap(true, Ordering::AcqRel),
        "memory_map::init called more than once"
    );
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
