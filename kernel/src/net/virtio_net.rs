//! virtio-net driver (legacy/transitional interface via I/O ports).
//!
//! Implements P16-T001 through P16-T012: PCI device discovery, virtqueue
//! setup, raw Ethernet frame send/receive, and interrupt-driven RX.
//!
//! Uses the virtio "legacy" (0.9.5) register layout mapped through PCI BAR0
//! I/O space, which is what QEMU's `virtio-net-pci` exposes by default.
//!
//! Phase 55 C.5 migration (now complete for IRQ handling as well):
//!   * PCI BAR0 is looked up through [`crate::pci::bar::map_bar`].
//!   * Virtqueue rings and per-descriptor buffers are allocated through
//!     [`crate::mm::dma::DmaBuffer`] instead of raw `alloc_contiguous_frames`.
//!   * The driver registers itself with [`crate::pci::register_driver`]; the
//!     kernel's [`crate::pci::probe_all_drivers`] pass binds the device.
//!   * The RX IRQ is installed through
//!     [`crate::pci::PciDeviceHandle::install_msi_irq`] (preferred) or
//!     [`crate::pci::PciDeviceHandle::install_intx_irq`] (fallback). The
//!     registered handler wakes the network task via
//!     [`crate::task::scheduler::wake_task`], and the task parks on a
//!     [`crate::task::scheduler::block_current_unless_woken`] flag — the
//!     same pattern as virtio-blk. The legacy `InterruptIndex::VirtioNet`
//!     vector 34 path is gone.

use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use kernel_core::types::TaskId;
use spin::Mutex;
use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;

use crate::mm::dma::DmaBuffer;
use crate::pci::bar::{BarMapping, PortRegion};
use crate::pci::{self, DriverEntry, DriverProbeResult, PciMatch};
use crate::task::scheduler::wake_task;

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

// MSI-X vector registers — only present in the legacy header when MSI-X
// is enabled at the PCI level (see virtio 0.9.5 §2.1.2). Enabling MSI-X
// shifts the device-specific config area forward by 4 bytes. Plain MSI
// does **not** insert these registers.
#[allow(dead_code)]
const VIRTIO_MSI_CONFIG_VECTOR: u16 = 0x14;
const VIRTIO_MSI_QUEUE_VECTOR: u16 = 0x16;
const VIRTIO_MSI_NO_VECTOR: u16 = 0xFFFF;

// virtio-net device-specific registers start at offset 0x14 when MSI-X is
// disabled, 0x18 when MSI-X is enabled. The driver reads MAC + link status
// before enabling MSI-X so the pre-shift offsets are safe to use here.
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

    /// DMA-allocated ring memory (desc + avail + used sub-tables laid out
    /// per the virtio 0.9.5 spec inside this one allocation).
    ring: DmaBuffer<[u8]>,

    // Pointers into the `ring` allocation (precomputed during init).
    desc_base: *mut VirtqDesc,
    avail_base: *mut VirtqAvailHeader,
    used_base: *mut VirtqUsedHeader,

    /// Per-descriptor DMA buffers — one 4 KiB page per slot, owned by the
    /// virtqueue.  Dropping the Vec returns all buffers to the buddy.
    buffer_dmas: Vec<DmaBuffer<[u8]>>,
    /// Cached kernel-virtual pointers — same length as buffer_dmas.
    buffers: Vec<*mut u8>,
    /// Cached physical addresses — same length as buffer_dmas.
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
    fn init(handle: &pci::PciDeviceHandle, io_base: u16, queue_index: u16) -> Option<Self> {
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

        // Phase 55a Track E: ring DMA buffer now owns an IOVA mapping
        // in the device's IOMMU domain (identity-fallback when no IOMMU
        // is translating). bus_address() is the canonical address to
        // hand to the device.
        let ring = match DmaBuffer::<[u8]>::allocate(handle, alloc_size) {
            Ok(b) => b,
            Err(e) => {
                log::error!(
                    "[virtio-net] queue {}: ring DmaBuffer alloc failed: {:?}",
                    queue_index,
                    e
                );
                return None;
            }
        };
        // Under Phase 55a IOMMU translation this value is an IOVA (bus
        // address), not a host physical address — the device sees it,
        // the host does not translate again. Named `bus_base` to match
        // that semantics rather than implying it is always a PA.
        let bus_base = ring.bus_address();
        let virt_base = ring.as_ptr() as usize;

        let n = queue_size as usize;
        let desc_base = virt_base as *mut VirtqDesc;
        let avail_offset = 16 * n;
        let avail_base = (virt_base + avail_offset) as *mut VirtqAvailHeader;
        let used_offset = align_up(avail_offset + 4 + 2 * n + 2, 4096);
        let used_base = (virt_base + used_offset) as *mut VirtqUsedHeader;

        // Legacy virtio uses a 32-bit PFN register; fail if the bus
        // address is above 4 GiB.
        let pfn_u64 = bus_base / 4096;
        if pfn_u64 > u32::MAX as u64 {
            log::error!(
                "[virtio-net] queue {}: bus {:#x} too high for 32-bit legacy PFN",
                queue_index,
                bus_base
            );
            return None;
        }
        let pfn = pfn_u64 as u32;
        unsafe {
            Port::<u32>::new(io_base + VIRTIO_QUEUE_ADDRESS).write(pfn);
        }

        log::info!(
            "[virtio-net] queue {}: size={}, bus={:#x}",
            queue_index,
            queue_size,
            bus_base
        );

        // Phase 55a Track E: per-descriptor buffers (one 4 KiB page
        // each) now each carry an IOMMU mapping. Dropping `buffer_dmas`
        // unmaps every IOVA range and returns every page to the buddy.
        let mut buffer_dmas: Vec<DmaBuffer<[u8]>> = Vec::with_capacity(n);
        let mut buffers = Vec::with_capacity(n);
        let mut buf_phys = Vec::with_capacity(n);
        for _ in 0..n {
            let dma = DmaBuffer::<[u8]>::allocate(handle, 4096).ok()?;
            buffers.push(dma.as_ptr() as *mut u8);
            buf_phys.push(dma.bus_address());
            buffer_dmas.push(dma);
        }

        Some(Virtqueue {
            io_base,
            queue_index,
            queue_size,
            ring,
            desc_base,
            avail_base,
            used_base,
            buffer_dmas,
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

pub use kernel_core::types::MacAddr;

#[allow(dead_code)]
struct VirtioNetDriver {
    /// Claim handle — held for the driver's lifetime so no other driver can
    /// re-bind to the same function.
    pci: pci::PciDeviceHandle,
    io_base: u16,
    mac: MacAddr,
    rx_queue: Virtqueue,
    tx_queue: Virtqueue,
    /// IRQ registration — must outlive the driver or the ISR stub
    /// dispatches to a stale handler. Kept around for the driver's lifetime.
    irq: Option<pci::DeviceIrq>,
}

static DRIVER: Mutex<Option<VirtioNetDriver>> = Mutex::new(None);

/// Set to true once the driver is initialized and ready.
pub static VIRTIO_NET_READY: AtomicBool = AtomicBool::new(false);

/// `TaskId` of the network processing task, registered from `kernel_main`
/// via [`set_net_task_id`]. Read by the IRQ handler to wake the task when
/// packets arrive. `0` means "not yet registered" — task IDs are allocated
/// starting at 1 in `Task::new`, so 0 is a safe sentinel. An atomic is used
/// because the ISR (which can fire on the same CPU as the setter) would
/// otherwise deadlock on a spin::Mutex held with interrupts enabled.
static NET_TASK_ID: AtomicU64 = AtomicU64::new(0);

/// `true` when the device is configured for legacy INTx IRQ delivery; `false`
/// for MSI-X. The ISR uses this to decide whether to read `ISR_STATUS` (which
/// clears it on read, required for INTx to avoid spurious continued delivery),
/// or to skip the read (which is unnecessary for MSI-X and may on some
/// QEMU / legacy-virtio interactions suppress the next MSI-X delivery).
static USING_LEGACY_INTX: AtomicBool = AtomicBool::new(false);

/// Signal flag for `block_current_unless_woken` in the network task.
/// The ISR sets it, `net_task` consumes it via swap. Public so the task
/// running in `main.rs::net_task` can use it directly.
pub static NET_IRQ_WOKEN: AtomicBool = AtomicBool::new(false);

/// Register the network task's [`TaskId`] so the IRQ handler can call
/// `wake_task` on it. Must be called from the task's own body before it
/// parks.
pub fn set_net_task_id(id: TaskId) {
    NET_TASK_ID.store(id.0, Ordering::Release);
    log::info!("[net] registered net_task id={}", id.0);
}

/// Wake the shared network task after RX/TX progress from any NIC backend.
pub fn wake_net_task() {
    crate::net::NIC_WOKEN.store(true, Ordering::Release);
    let raw = NET_TASK_ID.load(Ordering::Acquire);
    if raw != 0 {
        let _ = wake_task(TaskId(raw));
    } else {
        crate::task::scheduler::signal_reschedule();
    }
}

/// Returns the MAC address of the virtio-net device, if initialized.
#[allow(dead_code)]
pub fn mac_address() -> Option<MacAddr> {
    interrupts::without_interrupts(|| DRIVER.lock().as_ref().map(|d| d.mac))
}

/// Returns the legacy PCI interrupt line of the claimed virtio-net device,
/// or `None` if the device hasn't been initialised or has no INTx.
///
/// Used by `kernel_main` to program the I/O APIC for INTx routing now that
/// the PCI handle lives inside the driver (Phase 55 B.3).
#[allow(dead_code)]
pub fn pci_interrupt_line() -> Option<u8> {
    interrupts::without_interrupts(|| {
        DRIVER
            .lock()
            .as_ref()
            .map(|d| d.pci.device().interrupt_line)
            .filter(|&line| line != 0xFF)
    })
}

// ===========================================================================
// IRQ handler
// ===========================================================================

/// Acknowledge the device interrupt (read-to-clear ISR status) and wake the
/// network task so it can drain the RX ring. Runs in ISR context — see
/// the module-level contract in `kernel/src/arch/x86_64/interrupts.rs`.
///
/// ISR-safe lock use:
/// - `DRIVER.lock()` — plain `spin::Mutex`, but every task-context
///   acquisition (`recv_frames`, `send_frame`, `mac_address`,
///   `pci_interrupt_line`) wraps itself in `without_interrupts(…)`, so
///   a same-core ISR cannot reach a held lock. The ISR-side acquisition
///   here only reads `ISR_STATUS` and returns.
/// - `wake_task(TaskId(NET_TASK_ID))` — safe because
///   `scheduler::SCHEDULER` is an `IrqSafeMutex<Scheduler>` and
///   `enqueue_to_core` wraps its per-core `run_queue.lock()` in
///   `without_interrupts`. Prior to the 2026-04-21 post-mortem fix this
///   path deadlocked on a same-core `SCHEDULER.lock` holder; see
///   `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`.
///
/// No allocation, no blocking, no IPC.
fn virtio_net_irq_handler() {
    // Legacy INTx requires reading ISR_STATUS to clear the device-level
    // interrupt latch (per virtio 0.9.5 §2.1.2.4 — "reading this register
    // has the side effect of clearing it"). MSI-X delivers per-vector
    // without needing the shared-ISR latch; skipping the read in MSI-X
    // mode avoids a QEMU / transitional-virtio interaction where reading
    // ISR_STATUS while MSI-X is enabled can suppress the next MSI-X edge.
    if USING_LEGACY_INTX.load(Ordering::Relaxed)
        && let Some(d) = DRIVER.lock().as_ref()
    {
        // SAFETY: io_base is a valid legacy virtio I/O base the driver
        // probed and owns; reading ISR status is a side-effect-free ack
        // beyond clearing the INTx latch.
        unsafe {
            let _isr = Port::<u8>::new(d.io_base + VIRTIO_ISR_STATUS).read();
        }
    }
    // Signal the polling path and wake the task. The task consumes
    // NET_IRQ_WOKEN with `swap(false, ...)` so missed edges don't
    // accumulate; wake_task makes the task Ready so it runs on the next
    // scheduler tick. The shared `net::NIC_WOKEN` flag is what `net_task`
    // parks on, so set it alongside the driver-specific flag.
    NET_IRQ_WOKEN.store(true, Ordering::Release);
    wake_net_task();
}

// ===========================================================================
// Receive/send API
// ===========================================================================

/// Receive any pending Ethernet frames from the RX virtqueue.
///
/// Returns a vector of raw Ethernet frames (without the virtio-net header).
///
/// The `without_interrupts` region is kept as small as possible: the
/// descriptor read (`read_buffer`) must copy before we `post_recv_buffer`,
/// so that alloc stays inside; the virtio-net-header strip happens outside
/// the IF-off region to keep the worst-case interrupt latency bounded
/// (PR #113 Comment 1 — partial fix; eliminates the redundant `.to_vec()`
/// inside the critical section).
#[allow(dead_code)]
pub fn recv_frames() -> Vec<Vec<u8>> {
    // The driver lock must be taken with IF off so the ISR (which also
    // takes the lock) cannot fire on this CPU mid-critical-section. See
    // Fix 1 note in `blk/virtio_blk.rs`.
    let mut raw_frames: Vec<Vec<u8>> = interrupts::without_interrupts(|| {
        let mut driver = DRIVER.lock();
        let driver = match driver.as_mut() {
            Some(d) => d,
            None => return Vec::new(),
        };

        let completed = driver.rx_queue.poll_used();
        let mut raw = Vec::with_capacity(completed.len());
        let reposted = !completed.is_empty();

        for (desc_idx, len) in completed {
            if (len as usize) > VIRTIO_NET_HDR_SIZE {
                // One alloc-per-frame — unavoidable because the descriptor
                // buffer must be copied before we recycle it below.
                raw.push(driver.rx_queue.read_buffer(desc_idx, len));
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

        raw
    });

    // Strip the virtio-net header in-place outside the IF-off region — this
    // is a `Vec::drain` that does not reallocate, keeping the ISR-latency
    // budget untouched.
    for frame in &mut raw_frames {
        frame.drain(..VIRTIO_NET_HDR_SIZE);
    }
    raw_frames
}

/// Send a raw Ethernet frame via the TX virtqueue.
///
/// Prepends the 10-byte virtio-net header (all zeros for simple sends).
#[allow(dead_code)]
pub fn send_frame(frame: &[u8]) {
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

    // Build: virtio-net header (10 bytes of zeros) + Ethernet frame. The
    // allocation happens outside the driver lock to minimise the critical
    // section (spin::Mutex held, IF off).
    let mut buf = vec![0u8; total];
    buf[VIRTIO_NET_HDR_SIZE..].copy_from_slice(frame);

    interrupts::without_interrupts(|| {
        let mut driver = DRIVER.lock();
        let driver = match driver.as_mut() {
            Some(d) => d,
            None => {
                log::warn!("[virtio-net] send_frame: driver not initialized");
                return;
            }
        };
        driver.tx_queue.send_buffer(&buf);
    });
}

// ===========================================================================
// Initialization (P16-T001 through P16-T010)
// ===========================================================================

/// Legacy init entry — registers the driver and runs the probe pass.
///
/// Kept for `main.rs` compatibility. New code should prefer
/// `pci::probe_all_drivers` after calling `register()`.
pub fn init() {
    register();
    pci::probe_all_drivers();
}

/// Driver init body — takes a claimed PCI handle (from the driver-framework
/// probe pass).
fn init_with_handle(handle: pci::PciDeviceHandle) {
    let dev = *handle.device();

    log::info!(
        "[virtio-net] found device {:04x}:{:04x} at {:02x}:{:02x}.{}",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function
    );

    // Phase 55 C.1: BAR0 lookup goes through the HAL.  Legacy virtio-net
    // exposes an I/O port BAR; anything else is a configuration error.
    let port: PortRegion = match pci::bar::map_bar(&handle, 0) {
        Ok(BarMapping::Pio { region }) => region,
        Ok(_) => {
            log::error!("[virtio-net] BAR0 is MMIO, expected I/O port (legacy virtio)");
            return;
        }
        Err(e) => {
            log::error!("[virtio-net] BAR0 map failed: {:?}", e);
            return;
        }
    };
    let io_base = port.port_base();
    log::info!("[virtio-net] BAR0 I/O base: {:#x}", io_base);

    // Ensure PCI command register has I/O space (bit 0) and bus mastering
    // (bit 2) enabled — required for port I/O and DMA respectively.
    let cmd = handle.read_config_u16(0x04);
    if cmd & 0x05 != 0x05 {
        handle.write_config_u16(0x04, cmd | 0x05);
        log::info!("[virtio-net] PCI command: enabled I/O space + bus mastering");
    }

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
    let rx_queue = match Virtqueue::init(&handle, io_base, 0) {
        Some(q) => q,
        None => {
            log::error!("[virtio-net] failed to initialize RX queue");
            return;
        }
    };
    let tx_queue = match Virtqueue::init(&handle, io_base, 1) {
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

    // Install the RX IRQ through the Phase 55 C.3 HAL contract. Prefer
    // MSI, fall back to legacy INTx routed through the I/O APIC. The
    // handler reads ISR status, sets NET_IRQ_WOKEN, and wakes the net
    // task — all non-blocking work.
    //
    // The legacy-INTx handler contract says to check ISR status first to
    // avoid doing work for a sibling device's shared IRQ;
    // `virtio_net_irq_handler` reads ISR status, so sharing is safe.
    let dev_copy = dev;
    let irq = match handle.install_msi_irq(virtio_net_irq_handler) {
        Ok(i) => {
            log::info!("[virtio-net] MSI IRQ on vector {:#x}", i.vector());
            // Legacy virtio quirk: `install_msi_irq` enables MSI-X at the
            // PCI level and programs table entry 0, but the device still
            // has every virtqueue mapped to VIRTIO_MSI_NO_VECTOR by
            // default. Point both RX (queue 0) and TX (queue 1) at
            // MSI-X table entry 0 — we only allocated one vector, so
            // both queues share it and the single ISR drains both
            // directions. Without this write no MSI fires and the net
            // task parks forever. The queue-vector register only exists
            // when MSI-X is enabled — plain MSI does not insert it.
            if i.msi_kind() == Some(pci::MsiKind::MsiX) {
                let mut bound = true;
                for queue_index in [0u16, 1u16] {
                    port.write_reg::<u16>(VIRTIO_QUEUE_SELECT, queue_index);
                    port.write_reg::<u16>(VIRTIO_MSI_QUEUE_VECTOR, 0);
                    let readback = port.read_reg::<u16>(VIRTIO_MSI_QUEUE_VECTOR);
                    if readback == VIRTIO_MSI_NO_VECTOR {
                        log::error!(
                            "[virtio-net] device refused MSI-X vector binding for queue {} — NIC will stall",
                            queue_index
                        );
                        bound = false;
                        break;
                    }
                }
                if !bound {
                    return;
                }
                log::info!(
                    "[virtio-net] queues 0+1 bound to MSI-X table entry 0 (vector {:#x})",
                    i.vector()
                );
            }
            Some(i)
        }
        Err(_) => {
            // Legacy INTx: pick a vector from the device IRQ bank and route
            // the PCI interrupt line through the I/O APIC to that vector.
            const NET_INTX_VECTOR: u8 = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE + 3;
            let intx_result = handle.install_intx_irq(NET_INTX_VECTOR, virtio_net_irq_handler);
            if let Ok(i) = intx_result {
                // ISR handler must read ISR_STATUS in legacy INTx mode to
                // clear the shared-IRQ latch.
                USING_LEGACY_INTX.store(true, Ordering::Release);
                if dev_copy.interrupt_line != 0xFF && crate::acpi::io_apic_address().is_some() {
                    crate::arch::x86_64::apic::route_pci_irq(
                        dev_copy.interrupt_line,
                        NET_INTX_VECTOR,
                    );
                    log::info!(
                        "[virtio-net] legacy INTx line {} routed to vector {:#x}",
                        dev_copy.interrupt_line,
                        NET_INTX_VECTOR
                    );
                } else {
                    log::warn!(
                        "[virtio-net] legacy INTx registered but line is 0xFF or no I/O APIC — IRQ may not fire"
                    );
                }
                Some(i)
            } else {
                log::warn!(
                    "[virtio-net] failed to install completion IRQ — net_task will rely on periodic polling"
                );
                None
            }
        }
    };

    let mut driver = VirtioNetDriver {
        pci: handle,
        io_base,
        mac,
        rx_queue,
        tx_queue,
        irq,
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

    *DRIVER.lock() = Some(driver);
    VIRTIO_NET_READY.store(true, Ordering::Release);

    log::info!("[virtio-net] driver initialized successfully");
}

// PCI device discovery (Phase 55 C.4 + C.5) now runs through
// `register_driver` + `probe_all_drivers`. The bespoke `claim_virtio_net`
// helper used in Phase 55 B.3 has been removed.

// ===========================================================================
// Driver registration (Phase 55 C.4 / C.5)
// ===========================================================================

/// Register the virtio-net driver with the PCI discovery framework.
///
/// Uses `PciMatch::Full` so we disambiguate the 0x1AF4:0x1000 vendor/device
/// pair from other virtio devices with the same IDs (e.g. the legacy bridge).
/// The full Ethernet class/subclass 0x02:0x00 must match.
pub fn register() {
    let _ = pci::register_driver(DriverEntry {
        name: "virtio-net",
        r#match: PciMatch::Full {
            vendor: 0x1AF4,
            device: 0x1000,
            class: 0x02,
            subclass: 0x00,
        },
        init: probe,
    });
}

/// Driver probe — invoked by [`pci::probe_all_drivers`]. Wraps the original
/// `init` body so callers can continue to use the legacy `init()` entry
/// point.
fn probe(handle: pci::PciDeviceHandle) -> DriverProbeResult {
    init_with_handle(handle);
    if VIRTIO_NET_READY.load(Ordering::Acquire) {
        DriverProbeResult::Bound
    } else {
        DriverProbeResult::Failed("virtio-net init failed")
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Align `val` up to `alignment` (must be a power of two).
fn align_up(val: usize, alignment: usize) -> usize {
    (val + alignment - 1) & !(alignment - 1)
}
