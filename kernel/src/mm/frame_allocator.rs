use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{PhysFrame, Size4KiB};
use x86_64::PhysAddr;

const PAGE_SIZE: u64 = 4096;

/// Frames below 1 MiB are skipped even when the region is marked Usable.
/// Some UEFI/QEMU memory maps mark conventional low memory as Usable, but
/// those frames may hold BIOS data area remnants or be used by UEFI firmware
/// code paths that run before ExitBootServices completes.
pub(crate) const ALLOC_MIN_ADDR: u64 = 0x0010_0000; // 1 MiB

/// Magic value written to bytes 8..16 of each free frame for double-free detection.
const FREE_MAGIC: u64 = 0xDEAD_F4EE_F4EE_DEAD;

/// Free-list frame allocator.
///
/// Each free 4 KiB frame stores:
///   - bytes 0..8: physical address of the next free frame (0 = end of list)
///   - bytes 8..16: `FREE_MAGIC` sentinel for double-free detection
///
/// The allocator accesses frame memory through the bootloader's physical-memory
/// offset mapping (physical address P is at virtual address `phys_offset + P`).
struct FreeListAllocator {
    /// Physical address of the first free frame, or 0 if the list is empty.
    head: u64,
    /// Number of frames currently on the free list.
    free_count: usize,
    /// Total number of usable frames discovered at init (>= 1 MiB).
    total_frames: usize,
    /// Virtual base of the physical-memory offset mapping.
    phys_offset: u64,
}

impl FreeListAllocator {
    const fn new() -> Self {
        Self {
            head: 0,
            free_count: 0,
            total_frames: 0,
            phys_offset: 0,
        }
    }

    /// Build the free list from bootloader memory regions.
    ///
    /// Pushes every usable frame (>= 1 MiB) onto the intrusive linked list.
    fn init(&mut self, regions: &'static [MemoryRegion], phys_offset: u64) {
        self.phys_offset = phys_offset;
        self.head = 0;
        self.free_count = 0;
        self.total_frames = 0;

        for region in regions {
            if region.kind != MemoryRegionKind::Usable {
                continue;
            }

            let start = align_up(region.start.max(ALLOC_MIN_ADDR), PAGE_SIZE);
            let end = align_down(region.end, PAGE_SIZE);
            if end <= start {
                continue;
            }

            let mut addr = start;
            while addr + PAGE_SIZE <= end {
                self.push_frame(addr);
                self.total_frames += 1;
                addr += PAGE_SIZE;
            }
        }

        log::info!(
            "[mm] frame allocator: {} usable 4KiB frames on free list (>= 1 MiB)",
            self.total_frames
        );
    }

    /// Push a frame onto the head of the free list.
    ///
    /// Writes the current head pointer and magic sentinel into the frame's
    /// first 16 bytes via the physical-memory offset mapping.
    fn push_frame(&mut self, phys: u64) {
        let virt = (self.phys_offset + phys) as *mut u64;
        // SAFETY: `phys` is a valid, page-aligned physical address within a
        // Usable memory region.  The physical-memory offset mapping guarantees
        // `virt` is a valid kernel virtual address.  We have exclusive ownership
        // of the frame (it is not mapped anywhere else).
        unsafe {
            // bytes 0..8: next pointer
            virt.write(self.head);
            // bytes 8..16: magic sentinel
            virt.add(1).write(FREE_MAGIC);
        }
        self.head = phys;
        self.free_count += 1;
    }

    /// Pop a frame from the head of the free list.
    fn pop_frame(&mut self) -> Option<u64> {
        if self.head == 0 {
            return None;
        }

        let phys = self.head;
        let virt = (self.phys_offset + phys) as *mut u64;
        // SAFETY: `phys` is a frame on our free list; the physical-memory offset
        // mapping makes `virt` a valid kernel virtual address.
        unsafe {
            // Read next pointer and advance head.
            self.head = virt.read();
            // Clear the magic sentinel so double-free detection works.
            virt.add(1).write(0);
        }
        self.free_count -= 1;
        Some(phys)
    }

    /// Allocate a single 4 KiB frame.
    fn allocate(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let phys = self.pop_frame()?;
        let addr = PhysAddr::new(phys);
        Some(PhysFrame::containing_address(addr))
    }

    /// Return a frame to the free list.
    ///
    /// Panics if the frame is already on the free list (double-free).
    fn free(&mut self, phys: u64) {
        debug_assert!(
            phys >= ALLOC_MIN_ADDR,
            "free_frame: address {:#x} is below ALLOC_MIN_ADDR",
            phys
        );
        debug_assert!(
            phys.is_multiple_of(PAGE_SIZE),
            "free_frame: address {:#x} is not page-aligned",
            phys
        );

        // Double-free detection: check if the magic sentinel is present.
        let virt = (self.phys_offset + phys) as *const u64;
        // SAFETY: phys is a page-aligned address that was previously allocated;
        // the offset mapping makes virt valid.
        let magic = unsafe { virt.add(1).read() };
        if magic == FREE_MAGIC {
            panic!(
                "double-free detected: frame {:#x} is already on the free list",
                phys
            );
        }

        self.push_frame(phys);
    }
}

struct LockedFrameAllocator(Mutex<FreeListAllocator>);

static FRAME_ALLOCATOR: LockedFrameAllocator =
    LockedFrameAllocator(Mutex::new(FreeListAllocator::new()));

static FRAME_ALLOC_INIT: AtomicBool = AtomicBool::new(false);

pub fn init(regions: &'static [MemoryRegion], phys_offset: u64) {
    assert!(
        !FRAME_ALLOC_INIT.swap(true, Ordering::AcqRel),
        "frame_allocator::init called more than once"
    );
    FRAME_ALLOCATOR.0.lock().init(regions, phys_offset);
}

pub fn allocate_frame() -> Option<PhysFrame<Size4KiB>> {
    FRAME_ALLOCATOR.0.lock().allocate()
}

/// Return a frame to the allocator.
///
/// Panics on double-free (frame already on the free list).
pub fn free_frame(phys: u64) {
    FRAME_ALLOCATOR.0.lock().free(phys);
}

/// Returns the number of frames currently on the free list.
pub fn free_count() -> usize {
    FRAME_ALLOCATOR.0.lock().free_count
}

/// Returns the total number of usable frames discovered at boot.
pub fn total_frames() -> usize {
    FRAME_ALLOCATOR.0.lock().total_frames
}

#[inline]
fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

#[inline]
fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}
