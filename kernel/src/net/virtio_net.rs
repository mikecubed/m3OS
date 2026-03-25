//! virtio-net driver (legacy/transitional interface via I/O ports).
//!
//! Implements P16-T001 through P16-T012: PCI device discovery, virtqueue
//! setup, raw Ethernet frame send/receive, and interrupt-driven RX.
//!
//! Uses the virtio "legacy" (0.9.5) register layout mapped through PCI BAR0
//! I/O space, which is what QEMU's `virtio-net-pci` exposes by default.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::mm::frame_allocator;
use crate::pci;

// ===========================================================================
// Legacy virtio I/O register offsets (common header)
// ===========================================================================

/// Offsets relative to BAR0 I/O base for the legacy virtio header.
const VIRTIO_DEVICE_FEATURES: u16 = 0x00; // 32-bit read
const VIRTIO_DRIVER_FEATURES: u16 = 0x04; // 32-bit write
const VIRTIO_QUEUE_ADDRESS: u16 = 0x08; // 32-bit write (PFN)
const VIRTIO_QUEUE_SIZE: u16 = 0x0C; // 16-bit read
const VIRTIO_QUEUE_SELECT: u16 = 0x0E; // 16-bit write
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10; // 16-bit write
const VIRTIO_DEVICE_STATUS: u16 = 0x12; // 8-bit read/write
const VIRTIO_ISR_STATUS: u16 = 0x13; // 8-bit read

// virtio-net device-specific registers start at offset 0x14 for legacy.
const VIRTIO_NET_MAC_BASE: u16 = 0x14; // 6 bytes: MAC address
#[allow(dead_code)]
const VIRTIO_NET_STATUS: u16 = 0x1A; // 16-bit: link status

// ===========================================================================
// virtio status bits
// ===========================================================================

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;

// ===========================================================================
// virtio feature bits
// ===========================================================================

const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;

// ===========================================================================
// Virtqueue structures
// ===========================================================================

#[allow(dead_code)]
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// A single virtqueue descriptor (16 bytes).
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
struct VirtqDesc {
    addr: u64,  // physical address of buffer
    len: u32,   // buffer length
    flags: u16, // VIRTQ_DESC_F_*
    next: u16,  // next descriptor index (if NEXT flag set)
}

/// Available ring header (variable-length, but we store the full thing in one
/// contiguous allocation).
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

/// Size of each RX/TX buffer in bytes (MTU 1514 + virtio-net header 10).
const BUF_SIZE: usize = 1514 + VIRTIO_NET_HDR_SIZE;

/// virtio-net header prepended to every frame (legacy, no mergeable buffers).
pub const VIRTIO_NET_HDR_SIZE: usize = 10;

/// A single virtqueue backed by physically contiguous pages.
#[allow(dead_code)]
struct Virtqueue {
    /// Base I/O port of the virtio device.
    io_base: u16,
    /// Queue index (0 = RX, 1 = TX).
    queue_index: u16,
    /// Number of entries in the queue.
    queue_size: u16,

    // Pointers into the physically contiguous virtqueue allocation.
    desc_base: *mut VirtqDesc,
    avail_base: *mut VirtqAvailHeader,
    used_base: *mut VirtqUsedHeader,

    /// Physical base address of the virtqueue allocation (page-aligned).
    phys_base: u64,
    /// Virtual base address.
    virt_base: usize,
    /// Total size of the allocation in bytes.
    #[allow(dead_code)]
    alloc_size: usize,

    /// Per-descriptor buffer virtual addresses (for reading data back).
    buffers: Vec<*mut u8>,
    /// Per-descriptor buffer physical addresses (for descriptor addr field).
    buf_phys: Vec<u64>,

    /// Our last-seen used ring index.
    last_used_idx: u16,
    /// Next free descriptor index for the available ring.
    next_avail: u16,
}

// SAFETY: Virtqueue is only accessed under the DRIVER lock.
unsafe impl Send for Virtqueue {}

impl Virtqueue {
    /// Calculate the total byte size of the virtqueue allocation for a given
    /// queue size, per the virtio 0.9.5 spec.
    fn calc_size(queue_size: u16) -> usize {
        let n = queue_size as usize;
        // Descriptor table: 16 bytes per entry.
        let desc_size = 16 * n;
        // Available ring: 2 (flags) + 2 (idx) + 2*n (ring) + 2 (used_event).
        let avail_size = 4 + 2 * n + 2;
        // Align up to next page for used ring.
        let part1 = align_up(desc_size + avail_size, 4096);
        // Used ring: 2 (flags) + 2 (idx) + 8*n (ring) + 2 (avail_event).
        let used_size = 4 + 8 * n + 2;
        let part2 = align_up(used_size, 4096);
        part1 + part2
    }

    /// Initialize a virtqueue for the given queue index.
    ///
    /// Allocates physically contiguous pages and programs the device.
    fn init(io_base: u16, queue_index: u16) -> Option<Self> {
        // Select the queue.
        unsafe {
            Port::<u16>::new(io_base + VIRTIO_QUEUE_SELECT).write(queue_index);
        }

        // Read queue size.
        let queue_size = unsafe { Port::<u16>::new(io_base + VIRTIO_QUEUE_SIZE).read() };
        if queue_size == 0 {
            log::warn!("[virtio-net] queue {} size is 0 — skipping", queue_index);
            return None;
        }
        if queue_size > MAX_QUEUE_SIZE {
            log::warn!(
                "[virtio-net] queue {} size {} exceeds MAX_QUEUE_SIZE {} — skipping",
                queue_index,
                queue_size,
                MAX_QUEUE_SIZE
            );
            return None;
        }

        let alloc_size = Self::calc_size(queue_size);
        let pages_needed = alloc_size.div_ceil(4096);

        // Allocate physically contiguous pages.
        let first_frame = alloc_contiguous_frames(pages_needed)?;
        let phys_base = first_frame;
        let virt_base = (crate::mm::phys_offset() + phys_base) as usize;

        // Zero the allocation.
        unsafe {
            core::ptr::write_bytes(virt_base as *mut u8, 0, alloc_size);
        }

        // Compute sub-structure offsets.
        let n = queue_size as usize;
        let desc_base = virt_base as *mut VirtqDesc;
        let avail_offset = 16 * n;
        let avail_base = (virt_base + avail_offset) as *mut VirtqAvailHeader;
        let used_offset = align_up(avail_offset + 4 + 2 * n + 2, 4096);
        let used_base = (virt_base + used_offset) as *mut VirtqUsedHeader;

        // Tell the device the page frame number of the queue.
        let pfn = (phys_base / 4096) as u32;
        unsafe {
            Port::<u32>::new(io_base + VIRTIO_QUEUE_ADDRESS).write(pfn);
        }

        log::info!(
            "[virtio-net] queue {}: size={}, phys={:#x}",
            queue_index,
            queue_size,
            phys_base
        );

        // Allocate per-descriptor buffers.
        let mut buffers = Vec::with_capacity(n);
        let mut buf_phys = Vec::with_capacity(n);
        for _ in 0..n {
            let buf_frame = frame_allocator::allocate_frame()?;
            let bp = buf_frame.start_address().as_u64();
            let bv = (crate::mm::phys_offset() + bp) as *mut u8;
            buffers.push(bv);
            buf_phys.push(bp);
        }

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
            buffers,
            buf_phys,
            last_used_idx: 0,
            next_avail: 0,
        })
    }

    /// Post a buffer to the available ring (for RX: device writes into it).
    fn post_recv_buffer(&mut self, desc_idx: u16) {
        let i = desc_idx as usize;
        // Set up the descriptor using volatile writes (device-visible memory).
        let desc = self.desc_base.wrapping_add(i);
        unsafe {
            core::ptr::write_volatile(&raw mut (*desc).addr, self.buf_phys[i]);
            core::ptr::write_volatile(&raw mut (*desc).len, BUF_SIZE as u32);
            core::ptr::write_volatile(&raw mut (*desc).flags, VIRTQ_DESC_F_WRITE);
            core::ptr::write_volatile(&raw mut (*desc).next, 0);
        }

        // Add to available ring.
        let avail_idx = unsafe { core::ptr::read_volatile(&raw const (*self.avail_base).idx) };
        let ring_entry = avail_idx % self.queue_size;
        let ring_ptr = unsafe { (self.avail_base as *mut u16).add(2 + ring_entry as usize) };
        unsafe {
            core::ptr::write_volatile(ring_ptr, desc_idx);
        }

        // Memory barrier before updating idx.
        core::sync::atomic::fence(Ordering::Release);

        unsafe {
            core::ptr::write_volatile(&raw mut (*self.avail_base).idx, avail_idx.wrapping_add(1));
        }
    }

    /// Send a buffer (for TX: device reads from it).
    ///
    /// Reclaims completed TX descriptors first. Drops the packet if the ring
    /// is full (no free descriptors).
    #[allow(dead_code)]
    fn send_buffer(&mut self, data: &[u8]) {
        // Reclaim completed TX descriptors so we know which are free.
        self.poll_used();

        // Check for ring-full: if we've posted `queue_size` descriptors without
        // any being consumed, the ring is full — drop the packet.
        let avail_idx = unsafe { core::ptr::read_volatile(&raw const (*self.avail_base).idx) };
        let used_idx = unsafe { core::ptr::read_volatile(&raw const (*self.used_base).idx) };
        let in_flight = avail_idx.wrapping_sub(used_idx);
        if in_flight >= self.queue_size {
            log::warn!("[virtio-net] TX ring full — dropping packet");
            return;
        }

        // Reject oversize frames instead of silently truncating.
        if data.len() > BUF_SIZE {
            log::warn!(
                "[virtio-net] TX packet too large ({} > {} bytes) — dropping",
                data.len(),
                BUF_SIZE
            );
            return;
        }

        let desc_idx = self.next_avail % self.queue_size;
        self.next_avail = self.next_avail.wrapping_add(1);
        let i = desc_idx as usize;

        // Copy data to the pre-allocated buffer.
        let copy_len = data.len();
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.buffers[i], copy_len);
        }

        // Set up the descriptor using volatile writes (device-visible memory).
        let desc = self.desc_base.wrapping_add(i);
        unsafe {
            core::ptr::write_volatile(&raw mut (*desc).addr, self.buf_phys[i]);
            core::ptr::write_volatile(&raw mut (*desc).len, copy_len as u32);
            core::ptr::write_volatile(&raw mut (*desc).flags, 0u16);
            core::ptr::write_volatile(&raw mut (*desc).next, 0u16);
        }

        // Add to available ring.
        let ring_entry = avail_idx % self.queue_size;
        let ring_ptr = unsafe { (self.avail_base as *mut u16).add(2 + ring_entry as usize) };
        unsafe {
            core::ptr::write_volatile(ring_ptr, desc_idx);
        }
        core::sync::atomic::fence(Ordering::Release);
        unsafe {
            core::ptr::write_volatile(&raw mut (*self.avail_base).idx, avail_idx.wrapping_add(1));
        }

        // Notify the device.
        unsafe {
            Port::<u16>::new(self.io_base + VIRTIO_QUEUE_NOTIFY).write(self.queue_index);
        }
    }

    /// Collect completed used-ring entries. Returns a list of (descriptor_index, length).
    #[allow(dead_code)]
    fn poll_used(&mut self) -> Vec<(u16, u32)> {
        let mut results = Vec::new();
        loop {
            let used_idx =
                unsafe { core::ptr::read_volatile(&(*self.used_base).idx as *const u16) };
            if self.last_used_idx == used_idx {
                break;
            }

            let ring_entry = self.last_used_idx % self.queue_size;
            let elem_ptr = unsafe {
                (self.used_base as *const u8).add(4 + ring_entry as usize * 8)
                    as *const VirtqUsedElem
            };
            let elem = unsafe { core::ptr::read_volatile(elem_ptr) };
            results.push((elem.id as u16, elem.len));
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
        }
        results
    }

    /// Read data from a buffer at the given descriptor index.
    #[allow(dead_code)]
    fn read_buffer(&self, desc_idx: u16, len: u32) -> Vec<u8> {
        let i = desc_idx as usize;
        let copy_len = (len as usize).min(BUF_SIZE);
        let mut data = vec![0u8; copy_len];
        unsafe {
            core::ptr::copy_nonoverlapping(self.buffers[i], data.as_mut_ptr(), copy_len);
        }
        data
    }
}

// ===========================================================================
// Global driver state
// ===========================================================================

/// MAC address type.
pub type MacAddr = [u8; 6];

#[allow(dead_code)]
struct VirtioNetDriver {
    io_base: u16,
    mac: MacAddr,
    rx_queue: Virtqueue,
    tx_queue: Virtqueue,
}

static DRIVER: Mutex<Option<VirtioNetDriver>> = Mutex::new(None);

/// Set to true once the driver is initialized and ready.
pub static VIRTIO_NET_READY: AtomicBool = AtomicBool::new(false);

/// Lock-free copy of io_base for use in the interrupt handler.
/// Set once during init() and never changes. The ISR reads this instead of
/// taking the DRIVER mutex, avoiding deadlock when an IRQ fires while
/// send_frame/recv_frames holds the lock.
static ISR_IO_BASE: AtomicU16 = AtomicU16::new(0);

/// Returns the MAC address of the virtio-net device, if initialized.
#[allow(dead_code)]
pub fn mac_address() -> Option<MacAddr> {
    DRIVER.lock().as_ref().map(|d| d.mac)
}

// ===========================================================================
// Receive/send API
// ===========================================================================

/// Receive any pending Ethernet frames from the RX virtqueue.
///
/// Returns a vector of raw Ethernet frames (without the virtio-net header).
#[allow(dead_code)]
pub fn recv_frames() -> Vec<Vec<u8>> {
    let mut driver = DRIVER.lock();
    let driver = match driver.as_mut() {
        Some(d) => d,
        None => return Vec::new(),
    };

    let completed = driver.rx_queue.poll_used();
    let mut frames = Vec::new();
    let reposted = !completed.is_empty();

    for (desc_idx, len) in completed {
        if (len as usize) > VIRTIO_NET_HDR_SIZE {
            let raw = driver.rx_queue.read_buffer(desc_idx, len);
            // Strip the virtio-net header.
            frames.push(raw[VIRTIO_NET_HDR_SIZE..].to_vec());
        }
        // Re-post the buffer for future receives.
        driver.rx_queue.post_recv_buffer(desc_idx);
    }

    // Notify the device that new RX buffers are available so reception
    // doesn't stall once the initial batch is consumed.
    if reposted {
        unsafe {
            Port::<u16>::new(driver.io_base + VIRTIO_QUEUE_NOTIFY).write(0);
        }
    }

    frames
}

/// Send a raw Ethernet frame via the TX virtqueue.
///
/// Prepends the 10-byte virtio-net header (all zeros for simple sends).
#[allow(dead_code)]
pub fn send_frame(frame: &[u8]) {
    let mut driver = DRIVER.lock();
    let driver = match driver.as_mut() {
        Some(d) => d,
        None => {
            log::warn!("[virtio-net] send_frame: driver not initialized");
            return;
        }
    };

    // Reject oversize frames before allocating to avoid wasteful allocations
    // that send_buffer() would drop anyway.
    let total = VIRTIO_NET_HDR_SIZE + frame.len();
    if total > BUF_SIZE {
        log::warn!(
            "[virtio-net] send_frame: frame too large ({} + {} > {} bytes) — dropping",
            VIRTIO_NET_HDR_SIZE,
            frame.len(),
            BUF_SIZE
        );
        return;
    }

    // Build: virtio-net header (10 bytes of zeros) + Ethernet frame.
    let mut buf = vec![0u8; total];
    buf[VIRTIO_NET_HDR_SIZE..].copy_from_slice(frame);

    driver.tx_queue.send_buffer(&buf);
}

/// Read and clear the ISR status register. Called from the interrupt handler.
///
/// This is lock-free — reads io_base from an atomic rather than taking the
/// DRIVER mutex, so it is safe to call from an ISR context.
pub fn isr_status() -> u8 {
    let base = ISR_IO_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    unsafe { Port::<u8>::new(base + VIRTIO_ISR_STATUS).read() }
}

// ===========================================================================
// Initialization (P16-T001 through P16-T010)
// ===========================================================================

/// Initialize the virtio-net driver.
///
/// Finds the device on PCI, performs virtio reset + feature negotiation,
/// sets up RX and TX virtqueues, and reads the MAC address.
pub fn init() {
    // P16-T001: Find the virtio-net device.
    let dev = find_virtio_net_device();
    let dev = match dev {
        Some(d) => d,
        None => {
            log::warn!("[virtio-net] no virtio-net device found on PCI bus");
            return;
        }
    };

    log::info!(
        "[virtio-net] found device {:04x}:{:04x} at {:02x}:{:02x}.{}",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function
    );

    // P16-T002: Read BAR0 for legacy I/O port base.
    let bar0 = dev.bars[0];
    if bar0 & 1 == 0 {
        log::error!("[virtio-net] BAR0 is MMIO, expected I/O port (legacy virtio)");
        return;
    }
    let io_base = (bar0 & 0xFFFF_FFFC) as u16;
    log::info!("[virtio-net] BAR0 I/O base: {:#x}", io_base);

    // P16-T003: Reset sequence.
    unsafe {
        // Reset the device.
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(0);
        // Set ACKNOWLEDGE.
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(VIRTIO_STATUS_ACKNOWLEDGE);
        // Set DRIVER.
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS)
            .write(VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER);
    }

    // P16-T004: Feature negotiation.
    let device_features = unsafe { Port::<u32>::new(io_base + VIRTIO_DEVICE_FEATURES).read() };
    log::info!("[virtio-net] device features: {:#010x}", device_features);

    // We support MAC and STATUS features.
    let our_features = device_features & (VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS);
    unsafe {
        Port::<u32>::new(io_base + VIRTIO_DRIVER_FEATURES).write(our_features);
    }

    // For legacy devices, FEATURES_OK is not required but we try to set it
    // for transitional devices.
    unsafe {
        let status = Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).read();
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(status | VIRTIO_STATUS_FEATURES_OK);
        // Check if FEATURES_OK is still set (transitional device will set it).
        let status = Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).read();
        if status & VIRTIO_STATUS_FEATURES_OK == 0 {
            // Legacy-only device — that's fine, proceed without FEATURES_OK.
            log::info!("[virtio-net] legacy device (no FEATURES_OK) — continuing");
        }
    }

    // P16-T005, P16-T006, P16-T007: Initialize RX and TX virtqueues.
    let rx_queue = match Virtqueue::init(io_base, 0) {
        Some(q) => q,
        None => {
            log::error!("[virtio-net] failed to initialize RX queue");
            return;
        }
    };
    let tx_queue = match Virtqueue::init(io_base, 1) {
        Some(q) => q,
        None => {
            log::error!("[virtio-net] failed to initialize TX queue");
            return;
        }
    };

    // P16-T010: Read MAC address.
    let mut mac = [0u8; 6];
    if device_features & VIRTIO_NET_F_MAC != 0 {
        for (i, byte) in mac.iter_mut().enumerate() {
            *byte = unsafe { Port::<u8>::new(io_base + VIRTIO_NET_MAC_BASE + i as u16).read() };
        }
    }
    log::info!(
        "[virtio-net] MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );

    // Set DRIVER_OK to tell the device we're ready.
    unsafe {
        let status = Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).read();
        Port::<u8>::new(io_base + VIRTIO_DEVICE_STATUS).write(status | VIRTIO_STATUS_DRIVER_OK);
    }

    let mut driver = VirtioNetDriver {
        io_base,
        mac,
        rx_queue,
        tx_queue,
    };

    // P16-T008: Post initial receive buffers.
    let rx_count = driver.rx_queue.queue_size;
    for i in 0..rx_count {
        driver.rx_queue.post_recv_buffer(i);
    }
    // Notify the device that RX buffers are available.
    unsafe {
        Port::<u16>::new(io_base + VIRTIO_QUEUE_NOTIFY).write(0);
    }

    // Store io_base for lock-free ISR access before publishing driver state.
    ISR_IO_BASE.store(io_base, Ordering::Release);

    *DRIVER.lock() = Some(driver);
    VIRTIO_NET_READY.store(true, Ordering::Release);

    log::info!("[virtio-net] driver initialized successfully");
}

// ===========================================================================
// PCI device discovery (P16-T001)
// ===========================================================================

/// Find the virtio-net device in the PCI device list.
///
/// Only matches vendor 0x1AF4, device 0x1000 (legacy/transitional virtio-net).
/// Device 0x1041 (modern virtio-net) is not supported — the driver only
/// implements the legacy I/O-port register layout.
pub fn find_virtio_net_device() -> Option<pci::PciDevice> {
    let mut index = 0;
    while let Some(dev) = pci::pci_device(index) {
        if dev.vendor_id == 0x1AF4
            && dev.device_id == 0x1000
            && dev.class_code == 0x02
            && dev.subclass == 0x00
        {
            return Some(dev);
        }
        index += 1;
    }
    None
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
/// Returns the physical base address of the first frame, or `None` if
/// allocation fails. Uses the bump allocator which already allocates
/// sequentially from contiguous regions.
fn alloc_contiguous_frames(count: usize) -> Option<u64> {
    // The bump allocator hands out frames sequentially within each
    // contiguous usable region, so allocating `count` frames in a row
    // gives us a contiguous block as long as the region has enough space.
    let first = frame_allocator::allocate_frame()?;
    let base = first.start_address().as_u64();

    for i in 1..count {
        let frame = frame_allocator::allocate_frame()?;
        let expected = base + (i as u64) * 4096;
        if frame.start_address().as_u64() != expected {
            log::error!(
                "[virtio-net] frame {} not contiguous: got {:#x}, expected {:#x} — \
                 virtqueue requires contiguous physical memory",
                i,
                frame.start_address().as_u64(),
                expected
            );
            return None;
        }
    }
    Some(base)
}
