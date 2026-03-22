use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use spin::Mutex;
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::PhysAddr;

const PAGE_SIZE: u64 = 4096;

struct BumpAllocator {
    regions: Option<&'static [MemoryRegion]>,
    region_index: usize,
    next_addr: u64,
    frames_allocated: usize,
}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            regions: None,
            region_index: 0,
            next_addr: 0,
            frames_allocated: 0,
        }
    }

    fn init(&mut self, regions: &'static [MemoryRegion]) {
        self.regions = Some(regions);
        self.region_index = 0;
        self.next_addr = 0;
        self.frames_allocated = 0;

        // Advance to the first usable region
        self.advance_to_usable();

        // Count and log total usable frames
        let total_frames: u64 = regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| {
                let start = align_up(r.start, PAGE_SIZE);
                let end = align_down(r.end, PAGE_SIZE);
                if end > start {
                    (end - start) / PAGE_SIZE
                } else {
                    0
                }
            })
            .sum();

        log::info!("[mm] frame allocator: {} usable 4KiB frames available", total_frames);
    }

    fn advance_to_usable(&mut self) {
        let regions = match self.regions {
            Some(r) => r,
            None => return,
        };

        while self.region_index < regions.len() {
            let region = &regions[self.region_index];
            if region.kind == MemoryRegionKind::Usable {
                let start = align_up(region.start, PAGE_SIZE);
                if start < region.end {
                    self.next_addr = start;
                    return;
                }
            }
            self.region_index += 1;
        }
    }

    fn allocate(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let regions = self.regions?;

        loop {
            if self.region_index >= regions.len() {
                return None;
            }

            let region = &regions[self.region_index];

            // Only allocate from usable regions
            if region.kind != MemoryRegionKind::Usable {
                self.region_index += 1;
                self.advance_to_usable();
                continue;
            }

            let frame_start = self.next_addr;
            let frame_end = frame_start + PAGE_SIZE;

            if frame_end > region.end {
                // Current region exhausted, move to next
                self.region_index += 1;
                self.advance_to_usable();
                continue;
            }

            self.next_addr = frame_end;
            self.frames_allocated += 1;

            let addr = PhysAddr::new(frame_start);
            return Some(PhysFrame::containing_address(addr));
        }
    }
}

struct LockedFrameAllocator(Mutex<BumpAllocator>);

static FRAME_ALLOCATOR: LockedFrameAllocator =
    LockedFrameAllocator(Mutex::new(BumpAllocator::new()));

pub fn init(regions: &'static [MemoryRegion]) {
    FRAME_ALLOCATOR.0.lock().init(regions);
}

pub fn allocate_frame() -> Option<PhysFrame<Size4KiB>> {
    FRAME_ALLOCATOR.0.lock().allocate()
}

#[inline]
fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}
