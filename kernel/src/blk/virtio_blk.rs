//! virtio-blk driver (legacy/transitional interface via I/O ports).
//!
//! Implements P24-T005 through P24-T012: PCI device discovery, virtqueue
//! setup, sector read/write, and initialization.
//!
//! Uses the virtio "legacy" (0.9.5) register layout mapped through PCI BAR0
//! I/O space, which is what QEMU's `virtio-blk-pci` exposes by default.

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::mm::frame_allocator;
use crate::pci;

// ===========================================================================
// PCI device IDs
// ===========================================================================

/// Red Hat / virtio vendor ID.
const VIRTIO_BLK_VENDOR: u16 = 0x1AF4;
/// Legacy virtio-blk device ID.
const VIRTIO_BLK_DEVICE_LEGACY: u16 = 0x1001;
/// Transitional virtio-blk device ID.
const VIRTIO_BLK_DEVICE_TRANSITIONAL: u16 = 0x1042;

// ===========================================================================
// Legacy virtio I/O register offsets (common header)
// ===========================================================================

const VIRTIO_DEVICE_FEATURES: u16 = 0x00; // 32-bit read
const VIRTIO_DRIVER_FEATURES: u16 = 0x04; // 32-bit write
const VIRTIO_QUEUE_ADDRESS: u16 = 0x08; // 32-bit write (PFN)
const VIRTIO_QUEUE_SIZE: u16 = 0x0C; // 16-bit read
const VIRTIO_QUEUE_SELECT: u16 = 0x0E; // 16-bit write
#[allow(dead_code)]
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10; // 16-bit write
const VIRTIO_DEVICE_STATUS: u16 = 0x12; // 8-bit read/write
#[allow(dead_code)]
const VIRTIO_ISR_STATUS: u16 = 0x13; // 8-bit read

// virtio-blk device-specific config starts at offset 0x14 for legacy.
// Capacity is a u64 (number of 512-byte sectors) at offset 0x14.
const VIRTIO_BLK_CFG_CAPACITY: u16 = 0x14;

// ===========================================================================
// virtio status bits
// ===========================================================================

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;

// ===========================================================================
// virtio-blk request types
// ===========================================================================

/// Read sectors from device.
#[allow(dead_code)]
const VIRTIO_BLK_T_IN: u32 = 0;
/// Write sectors to device.
#[allow(dead_code)]
const VIRTIO_BLK_T_OUT: u32 = 1;

// ===========================================================================
// Virtqueue structures
// ===========================================================================

#[allow(dead_code)]
const VIRTQ_DESC_F_NEXT: u16 = 1;
#[allow(dead_code)]
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// A single virtqueue descriptor (16 bytes).
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Available ring header.
#[repr(C, align(2))]
#[derive(Debug, Clone, Copy)]
struct VirtqAvailHeader {
    flags: u16,
    idx: u16,
    // followed by `queue_size` u16 ring entries
}

/// Used ring entry.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

/// Used ring header.
#[repr(C, align(4))]
#[derive(Debug, Clone, Copy)]
struct VirtqUsedHeader {
    flags: u16,
    idx: u16,
    // followed by `queue_size` VirtqUsedElem entries
}

// ===========================================================================
// Virtqueue
// ===========================================================================

/// Maximum queue size we support.
const MAX_QUEUE_SIZE: u16 = 256;

/// Sector size in bytes.
const SECTOR_SIZE: usize = 512;

/// A single virtqueue backed by physically contiguous pages.
#[allow(dead_code)]
struct Virtqueue {
    io_base: u16,
    queue_index: u16,
    queue_size: u16,

    desc_base: *mut VirtqDesc,
    avail_base: *mut VirtqAvailHeader,
    used_base: *mut VirtqUsedHeader,

    #[allow(dead_code)]
    phys_base: u64,
    #[allow(dead_code)]
    virt_base: usize,
    #[allow(dead_code)]
    alloc_size: usize,

    last_used_idx: u16,
}

// SAFETY: Virtqueue is only accessed under the DRIVER lock.
unsafe impl Send for Virtqueue {}

impl Virtqueue {
    /// Calculate the total byte size of the virtqueue allocation.
    fn calc_size(queue_size: u16) -> usize {
        let n = queue_size as usize;
        let desc_size = 16 * n;
        let avail_size = 4 + 2 * n + 2;
        let part1 = align_up(desc_size + avail_size, 4096);
        let used_size = 4 + 8 * n + 2;
        let part2 = align_up(used_size, 4096);
        part1 + part2
    }

    /// Initialize a virtqueue for the given queue index.
    fn init(io_base: u16, queue_index: u16) -> Option<Self> {
        unsafe {
            Port::<u16>::new(io_base + VIRTIO_QUEUE_SELECT).write(queue_index);
        }

        let queue_size = unsafe { Port::<u16>::new(io_base + VIRTIO_QUEUE_SIZE).read() };
        if queue_size < 3 {
            log::warn!(
                "[virtio-blk] queue {} size {} too small (need >= 3) — skipping",
                queue_index,
                queue_size
            );
            return None;
        }
        if queue_size > MAX_QUEUE_SIZE {
            log::warn!(
                "[virtio-blk] queue {} size {} exceeds MAX_QUEUE_SIZE {} — skipping",
                queue_index,
                queue_size,
                MAX_QUEUE_SIZE
            );
            return None;
        }

        let alloc_size = Self::calc_size(queue_size);
        let pages_needed = alloc_size.div_ceil(4096);

        let first_frame = alloc_contiguous_frames(pages_needed)?;
        let phys_base = first_frame;
        let virt_base = (crate::mm::phys_offset() + phys_base) as usize;

        // Zero the allocation.
        unsafe {
            core::ptr::write_bytes(virt_base as *mut u8, 0, alloc_size);
        }

        let n = queue_size as usize;
        let desc_base = virt_base as *mut VirtqDesc;
        let avail_offset = 16 * n;
        let avail_base = (virt_base + avail_offset) as *mut VirtqAvailHeader;
        let used_offset = align_up(avail_offset + 4 + 2 * n + 2, 4096);
        let used_base = (virt_base + used_offset) as *mut VirtqUsedHeader;

        // Tell the device the PFN.
        let pfn_u64 = phys_base / 4096;
        if pfn_u64 > u32::MAX as u64 {
            log::error!(
                "[virtio-blk] queue {}: phys {:#x} too high for 32-bit legacy PFN",
                queue_index,
                phys_base
            );
            // Free the allocated queue frames to avoid leaking them.
            for i in 0..pages_needed {
                frame_allocator::free_frame(phys_base + (i as u64) * 4096);
            }
            return None;
        }
        let pfn = pfn_u64 as u32;
        unsafe {
            Port::<u32>::new(io_base + VIRTIO_QUEUE_ADDRESS).write(pfn);
        }

        log::info!(
            "[virtio-blk] queue {}: size={}, phys={:#x}",
            queue_index,
            queue_size,
            phys_base
        );

        Some(Virtqueue {
            io_base,
            queue_index,
            queue_size,
            desc_base,
            avail_base,
            used_base,
            phys_base,
            virt_base,
            alloc_size,
            last_used_idx: 0,
        })
    }

    /// Submit a 3-descriptor chain for a block I/O request and wait for completion.
    ///
    /// Returns the status byte from the device (0 = success).
    #[allow(dead_code, clippy::too_many_arguments)]
    fn submit_request(
        &mut self,
        req_type: u32,
        sector: u64,
        data_buf_phys: u64,
        data_buf_virt: *mut u8,
        data_len: usize,
        scratch_phys: u64,
        scratch_virt: *mut u8,
    ) -> u8 {
        // We use 3 consecutive descriptor indices starting from 0.
        // Since we hold the driver lock and process one request at a time,
        // we can always reuse descriptors 0, 1, 2.
        let hdr_desc_idx: u16 = 0;
        let data_desc_idx: u16 = 1;
        let status_desc_idx: u16 = 2;

        // Place VirtioBlkReq at offset 0 of scratch page.
        let req = VirtioBlkReq {
            type_: req_type,
            reserved: 0,
            sector,
        };
        unsafe {
            core::ptr::write_volatile(scratch_virt as *mut VirtioBlkReq, req);
        }

        // Place status byte at offset 64 of scratch page (well past the 16-byte header).
        let status_phys = scratch_phys + 64;
        let status_virt = unsafe { scratch_virt.add(64) };
        unsafe {
            core::ptr::write_volatile(status_virt, 0xFFu8); // sentinel
        }

        // Determine flags for the data descriptor.
        let data_flags = if req_type == VIRTIO_BLK_T_IN {
            // Read: device writes to data buffer.
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE
        } else {
            // Write: device reads from data buffer.
            VIRTQ_DESC_F_NEXT
        };

        // --- Descriptor 0: request header (device-readable) ---
        let desc0 = self.desc_base.wrapping_add(hdr_desc_idx as usize);
        unsafe {
            core::ptr::write_volatile(&raw mut (*desc0).addr, scratch_phys);
            core::ptr::write_volatile(
                &raw mut (*desc0).len,
                core::mem::size_of::<VirtioBlkReq>() as u32,
            );
            core::ptr::write_volatile(&raw mut (*desc0).flags, VIRTQ_DESC_F_NEXT);
            core::ptr::write_volatile(&raw mut (*desc0).next, data_desc_idx);
        }

        // --- Descriptor 1: data buffer ---
        let desc1 = self.desc_base.wrapping_add(data_desc_idx as usize);
        unsafe {
            core::ptr::write_volatile(&raw mut (*desc1).addr, data_buf_phys);
            core::ptr::write_volatile(&raw mut (*desc1).len, data_len as u32);
            core::ptr::write_volatile(&raw mut (*desc1).flags, data_flags);
            core::ptr::write_volatile(&raw mut (*desc1).next, status_desc_idx);
        }

        // --- Descriptor 2: status byte (device-writable) ---
        let desc2 = self.desc_base.wrapping_add(status_desc_idx as usize);
        unsafe {
            core::ptr::write_volatile(&raw mut (*desc2).addr, status_phys);
            core::ptr::write_volatile(&raw mut (*desc2).len, 1u32);
            core::ptr::write_volatile(&raw mut (*desc2).flags, VIRTQ_DESC_F_WRITE);
            core::ptr::write_volatile(&raw mut (*desc2).next, 0u16);
        }

        // Add head descriptor to the available ring.
        let avail_idx = unsafe { core::ptr::read_volatile(&raw const (*self.avail_base).idx) };
        let ring_entry = avail_idx % self.queue_size;
        let ring_ptr = unsafe { (self.avail_base as *mut u16).add(2 + ring_entry as usize) };
        unsafe {
            core::ptr::write_volatile(ring_ptr, hdr_desc_idx);
        }

        core::sync::atomic::fence(Ordering::Release);

        unsafe {
            core::ptr::write_volatile(&raw mut (*self.avail_base).idx, avail_idx.wrapping_add(1));
        }

        // Notify the device (kick).
        unsafe {
            Port::<u16>::new(self.io_base + VIRTIO_QUEUE_NOTIFY).write(self.queue_index);
        }

        // Spin-poll the used ring for completion.
        let mut spin_count: u64 = 0;
        loop {
            let used_idx = unsafe { core::ptr::read_volatile(&raw const (*self.used_base).idx) };
            if used_idx != self.last_used_idx {
                // Consume the used entry.
                self.last_used_idx = self.last_used_idx.wrapping_add(1);
                break;
            }
            core::hint::spin_loop();
            spin_count += 1;
            if spin_count > 100_000_000 {
                log::error!("[virtio-blk] request timed out waiting for completion");
                return 0xFF;
            }
        }

        // Read status byte.
        let status = unsafe { core::ptr::read_volatile(status_virt) };

        // For reads, the data is already in data_buf_virt via DMA.
        let _ = data_buf_virt; // suppress unused warning for write path

        status
    }
}

// ===========================================================================
// VirtioBlkReq (P24-T009)
// ===========================================================================

/// virtio-blk request header.
#[repr(C, packed)]
#[allow(dead_code)]
struct VirtioBlkReq {
    type_: u32,
    reserved: u32,
    sector: u64,
}

// ===========================================================================
// Global driver state
// ===========================================================================

#[allow(dead_code)]
struct VirtioBlkDriver {
    io_base: u16,
    capacity_sectors: u64,
    request_queue: Virtqueue,
    /// Persistent scratch frame for request headers and status bytes.
    scratch_phys: u64,
    scratch_virt: *mut u8,
    /// Persistent DMA frame for sector data transfers.
    dma_phys: u64,
    dma_virt: *mut u8,
}

// SAFETY: VirtioBlkDriver raw pointers are only accessed under the DRIVER lock.
unsafe impl Send for VirtioBlkDriver {}

static DRIVER: Mutex<Option<VirtioBlkDriver>> = Mutex::new(None);

/// Set to true once the driver is initialized and ready.
pub static VIRTIO_BLK_READY: AtomicBool = AtomicBool::new(false);

// ===========================================================================
// Read/Write API (P24-T010, P24-T011)
// ===========================================================================

/// Read `count` sectors starting at `start_sector` into `buf`.
///
/// `buf` must be at least `count * 512` bytes. Returns `Ok(())` on success
/// or `Err(status)` with the virtio status byte on failure.
#[allow(dead_code)]
pub fn read_sectors(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    let needed = count * SECTOR_SIZE;
    if buf.len() < needed {
        log::error!(
            "[virtio-blk] read_sectors: buffer too small ({} < {})",
            buf.len(),
            needed
        );
        return Err(0xFF);
    }

    let mut driver = DRIVER.lock();
    let driver = match driver.as_mut() {
        Some(d) => d,
        None => {
            log::error!("[virtio-blk] read_sectors: driver not initialized");
            return Err(0xFF);
        }
    };

    if start_sector + count as u64 > driver.capacity_sectors {
        log::error!(
            "[virtio-blk] read_sectors: out of bounds (sector {} + {} > {})",
            start_sector,
            count,
            driver.capacity_sectors
        );
        return Err(0xFF);
    }

    let dma_phys = driver.dma_phys;
    let dma_virt = driver.dma_virt;
    let scratch_phys = driver.scratch_phys;
    let scratch_virt = driver.scratch_virt;

    for i in 0..count {
        let sector = start_sector + i as u64;
        let status = driver.request_queue.submit_request(
            VIRTIO_BLK_T_IN,
            sector,
            dma_phys,
            dma_virt,
            SECTOR_SIZE,
            scratch_phys,
            scratch_virt,
        );
        if status != 0 {
            log::error!(
                "[virtio-blk] read_sectors: sector {} failed with status {}",
                sector,
                status
            );
            return Err(status);
        }
        // Copy from DMA buffer to caller's buffer.
        let offset = i * SECTOR_SIZE;
        unsafe {
            core::ptr::copy_nonoverlapping(dma_virt, buf[offset..].as_mut_ptr(), SECTOR_SIZE);
        }
    }

    Ok(())
}

/// Write `count` sectors starting at `start_sector` from `buf`.
///
/// `buf` must be at least `count * 512` bytes. Returns `Ok(())` on success
/// or `Err(status)` with the virtio status byte on failure.
#[allow(dead_code)]
pub fn write_sectors(start_sector: u64, count: usize, buf: &[u8]) -> Result<(), u8> {
    let needed = count * SECTOR_SIZE;
    if buf.len() < needed {
        log::error!(
            "[virtio-blk] write_sectors: buffer too small ({} < {})",
            buf.len(),
            needed
        );
        return Err(0xFF);
    }

    let mut driver = DRIVER.lock();
    let driver = match driver.as_mut() {
        Some(d) => d,
        None => {
            log::error!("[virtio-blk] write_sectors: driver not initialized");
            return Err(0xFF);
        }
    };

    if start_sector + count as u64 > driver.capacity_sectors {
        log::error!(
            "[virtio-blk] write_sectors: out of bounds (sector {} + {} > {})",
            start_sector,
            count,
            driver.capacity_sectors
        );
        return Err(0xFF);
    }

    let dma_phys = driver.dma_phys;
    let dma_virt = driver.dma_virt;
    let scratch_phys = driver.scratch_phys;
    let scratch_virt = driver.scratch_virt;

    for i in 0..count {
        let sector = start_sector + i as u64;
        // Copy caller's data to DMA buffer.
        let offset = i * SECTOR_SIZE;
        unsafe {
            core::ptr::copy_nonoverlapping(buf[offset..].as_ptr(), dma_virt, SECTOR_SIZE);
        }
        let status = driver.request_queue.submit_request(
            VIRTIO_BLK_T_OUT,
            sector,
            dma_phys,
            dma_virt,
            SECTOR_SIZE,
            scratch_phys,
            scratch_virt,
        );
        if status != 0 {
            log::error!(
                "[virtio-blk] write_sectors: sector {} failed with status {}",
                sector,
                status
            );
            return Err(status);
        }
    }

    Ok(())
}

// ===========================================================================
// PCI device discovery (P24-T006)
// ===========================================================================

/// Find the virtio-blk device in the PCI device list.
fn find_virtio_blk_device() -> Option<pci::PciDevice> {
    let mut index = 0;
    while let Some(dev) = pci::pci_device(index) {
        if dev.vendor_id == VIRTIO_BLK_VENDOR
            && (dev.device_id == VIRTIO_BLK_DEVICE_LEGACY
                || dev.device_id == VIRTIO_BLK_DEVICE_TRANSITIONAL)
        {
            return Some(dev);
        }
        index += 1;
    }
    None
}

// ===========================================================================
// Initialization (P24-T007, P24-T008, P24-T012)
// ===========================================================================

/// Initialize the virtio-blk driver.
///
/// Finds the device on PCI, performs virtio reset + feature negotiation,
/// sets up the request virtqueue, and reads the disk capacity.
pub fn init() {
    // P24-T006: Find the virtio-blk device.
    let dev = match find_virtio_blk_device() {
        Some(d) => d,
        None => {
            log::warn!("[virtio-blk] no virtio-blk device found on PCI bus");
            return;
        }
    };

    log::info!(
        "[virtio-blk] found device {:04x}:{:04x} at {:02x}:{:02x}.{}",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function
    );

    // P24-T007: Read BAR0 for legacy I/O port base.
    let bar0 = dev.bars[0];
    if bar0 & 1 == 0 {
        log::error!("[virtio-blk] BAR0 is MMIO, expected I/O port (legacy virtio)");
        return;
    }
    let io_base = (bar0 & 0xFFFF_FFFC) as u16;
    log::info!("[virtio-blk] BAR0 I/O base: {:#x}", io_base);

    // Ensure PCI command register has I/O space (bit 0) and bus mastering (bit 2).
    let cmd = pci::pci_config_read_u16(dev.bus, dev.device, dev.function, 0x04);
    if cmd & 0x05 != 0x05 {
        pci::pci_config_write_u16(dev.bus, dev.device, dev.function, 0x04, cmd | 0x05);
        log::info!("[virtio-blk] PCI command: enabled I/O space + bus mastering");
    }

    // Reset sequence.
    unsafe {
        // Reset the device.
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(0);
        // Set ACKNOWLEDGE.
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(VIRTIO_STATUS_ACKNOWLEDGE);
        // Set DRIVER.
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS)
            .write(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);
    }

    // Feature negotiation — we don't need any special features for basic block I/O.
    let device_features = unsafe { Port::<u32>::new(io_base + VIRTIO_DEVICE_FEATURES).read() };
    log::info!("[virtio-blk] device features: {:#010x}", device_features);

    // Accept no optional features for now (basic read/write is always available).
    unsafe {
        Port::<u32>::new(io_base + VIRTIO_DRIVER_FEATURES).write(0);
    }

    // Try to set FEATURES_OK for transitional devices.
    unsafe {
        let status = Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).read();
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(status | VIRTIO_STATUS_FEATURES_OK);
        let status = Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).read();
        if status & VIRTIO_STATUS_FEATURES_OK == 0 {
            log::info!("[virtio-blk] legacy device (no FEATURES_OK) — continuing");
        }
    }

    // P24-T008: Initialize request virtqueue (queue 0).
    let request_queue = match Virtqueue::init(io_base, 0) {
        Some(q) => q,
        None => {
            log::error!("[virtio-blk] failed to initialize request queue");
            return;
        }
    };

    // Set DRIVER_OK.
    unsafe {
        let status = Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).read();
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(status | VIRTIO_STATUS_DRIVER_OK);
    }

    // Read capacity from device-specific config (BAR + 0x14, u64 in little-endian).
    let capacity_lo = unsafe { Port::<u32>::new(io_base + VIRTIO_BLK_CFG_CAPACITY).read() } as u64;
    let capacity_hi =
        unsafe { Port::<u32>::new(io_base + VIRTIO_BLK_CFG_CAPACITY + 4).read() } as u64;
    let capacity_sectors = capacity_lo | (capacity_hi << 32);

    log::info!(
        "[virtio-blk] capacity: {} sectors ({} MiB)",
        capacity_sectors,
        (capacity_sectors * SECTOR_SIZE as u64) / (1024 * 1024)
    );

    // Allocate persistent scratch and DMA frames (reused across all requests).
    let scratch_frame = match frame_allocator::allocate_frame() {
        Some(f) => f,
        None => {
            log::error!("[virtio-blk] failed to allocate scratch frame");
            return;
        }
    };
    let scratch_phys = scratch_frame.start_address().as_u64();
    let scratch_virt = (crate::mm::phys_offset() + scratch_phys) as *mut u8;

    let dma_frame = match frame_allocator::allocate_frame() {
        Some(f) => f,
        None => {
            log::error!("[virtio-blk] failed to allocate DMA frame");
            // Free the scratch frame we already allocated.
            frame_allocator::free_frame(scratch_phys);
            return;
        }
    };
    let dma_phys = dma_frame.start_address().as_u64();
    let dma_virt = (crate::mm::phys_offset() + dma_phys) as *mut u8;

    let driver = VirtioBlkDriver {
        io_base,
        capacity_sectors,
        request_queue,
        scratch_phys,
        scratch_virt,
        dma_phys,
        dma_virt,
    };

    *DRIVER.lock() = Some(driver);
    VIRTIO_BLK_READY.store(true, Ordering::Release);

    log::info!("[virtio-blk] driver initialized successfully");
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Align `val` up to `alignment` (must be a power of two).
fn align_up(val: usize, alignment: usize) -> usize {
    (val + alignment - 1) & !(alignment - 1)
}

/// Allocate `count` physically contiguous 4 KiB frames.
///
/// Allocates all frames, sorts by physical address, and checks for
/// contiguity. This works regardless of allocator ordering (LIFO,
/// ascending, etc.). Returns the base physical address on success.
fn alloc_contiguous_frames(count: usize) -> Option<u64> {
    use alloc::vec::Vec;

    // Allocate all requested frames.
    let mut frames: Vec<u64> = Vec::with_capacity(count);
    for _ in 0..count {
        match frame_allocator::allocate_frame() {
            Some(f) => frames.push(f.start_address().as_u64()),
            None => {
                // OOM: free everything we allocated so far.
                for &phys in &frames {
                    frame_allocator::free_frame(phys);
                }
                return None;
            }
        }
    }

    // Sort by physical address and check contiguity.
    frames.sort_unstable();
    let base = frames[0];
    for (i, &phys) in frames.iter().enumerate() {
        if phys != base + (i as u64) * 4096 {
            log::error!(
                "[virtio-blk] frames not contiguous: frame {} at {:#x}, expected {:#x}",
                i,
                phys,
                base + (i as u64) * 4096
            );
            for &p in &frames {
                frame_allocator::free_frame(p);
            }
            return None;
        }
    }

    Some(base)
}
