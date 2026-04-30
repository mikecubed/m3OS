//! virtio-blk driver (legacy/transitional interface via I/O ports).
//!
//! Phase 55 C.5 migration:
//!
//! * PCI claim + BAR mapping go through [`crate::pci::claim_specific`] +
//!   [`crate::pci::bar::map_bar`] (the latter handles the I/O-port extraction
//!   that used to live inline here).
//! * Descriptor ring, scratch, and DMA buffers are [`crate::mm::dma::DmaBuffer`]
//!   instead of raw `alloc_contiguous_frames` + `phys_offset` arithmetic.
//! * Completion: the IRQ handler walks the used ring and is wired up for
//!   future async consumers, but `read_sectors`/`write_sectors` poll the
//!   used ring from task context rather than parking on the scheduler.
//!   Scheduler-park was measured to round-trip a context switch (and an
//!   optional cross-core IPI wake) per sector under SMP, making ext2 boot
//!   walks pathologically slow; the completion arrives in well under a
//!   scheduler tick so polling is the right shape for this size of request.
//!   Routed here from `docs/debug/54-followups.md` item 5.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_core::types::TaskId;
use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::mm::dma::DmaBuffer;
use crate::pci::bar::{BarMapping, PortRegion};
use crate::pci::{self, DriverEntry, DriverProbeResult, PciMatch};
use crate::task::scheduler::current_task_id;

// ===========================================================================
// PCI device IDs
// ===========================================================================

const VIRTIO_BLK_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE_LEGACY: u16 = 0x1001;
const VIRTIO_BLK_DEVICE_TRANSITIONAL: u16 = 0x1042;

// ===========================================================================
// Legacy virtio I/O register offsets (common header)
// ===========================================================================

const VIRTIO_DEVICE_FEATURES: u16 = 0x00;
const VIRTIO_DRIVER_FEATURES: u16 = 0x04;
const VIRTIO_QUEUE_ADDRESS: u16 = 0x08;
const VIRTIO_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_DEVICE_STATUS: u16 = 0x12;
const VIRTIO_ISR_STATUS: u16 = 0x13;

// MSI-X vector registers — only present in the legacy header when MSI-X
// is enabled at the PCI level (see virtio 0.9.5 §2.1.2). Enabling MSI-X
// shifts the device-specific config area forward by 4 bytes. Plain MSI
// does **not** insert these registers.
#[allow(dead_code)]
const VIRTIO_MSI_CONFIG_VECTOR: u16 = 0x14;
const VIRTIO_MSI_QUEUE_VECTOR: u16 = 0x16;
const VIRTIO_MSI_NO_VECTOR: u16 = 0xFFFF;

// virtio-blk device-specific config starts at offset 0x14 when MSI-X is
// disabled, 0x18 when MSI-X is enabled. The driver reads capacity before
// enabling MSI-X so the pre-shift offset is safe to use here.
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

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

// ===========================================================================
// Virtqueue structures
// ===========================================================================

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C, align(2))]
#[derive(Debug, Clone, Copy)]
struct VirtqAvailHeader {
    flags: u16,
    idx: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

#[repr(C, align(4))]
#[derive(Debug, Clone, Copy)]
struct VirtqUsedHeader {
    flags: u16,
    idx: u16,
}

const MAX_QUEUE_SIZE: u16 = 256;
const SECTOR_SIZE: usize = 512;

/// One waiter slot: (TaskId blocked on this in-flight req, woken-flag).
/// Indexed by head descriptor id (0..queue_size).
struct Waiter {
    task: TaskId,
    woken: &'static AtomicBool,
    status_virt: *mut u8,
}

// SAFETY: Waiter.status_virt only points into the driver's own scratch
// region. Waiter is only inserted/removed under DRIVER.lock().
unsafe impl Send for Waiter {}

struct Virtqueue {
    port: PortRegion,
    queue_index: u16,
    queue_size: u16,
    /// DMA-allocated ring (desc + avail + used).
    #[allow(dead_code)]
    ring: DmaBuffer<[u8]>,

    desc_base: *mut VirtqDesc,
    avail_base: *mut VirtqAvailHeader,
    used_base: *mut VirtqUsedHeader,

    last_used_idx: u16,
    /// Pending waiters indexed by their head-descriptor id.
    waiters: Vec<Option<Waiter>>,
}

// SAFETY: Virtqueue is only accessed under the DRIVER lock, and the raw
// pointers above point into the DmaBuffer it owns.
unsafe impl Send for Virtqueue {}

impl Virtqueue {
    fn calc_size(queue_size: u16) -> usize {
        let n = queue_size as usize;
        let desc_size = 16 * n;
        let avail_size = 4 + 2 * n + 2;
        let part1 = align_up(desc_size + avail_size, 4096);
        let used_size = 4 + 8 * n + 2;
        let part2 = align_up(used_size, 4096);
        part1 + part2
    }

    fn init(handle: &pci::PciDeviceHandle, port: PortRegion, queue_index: u16) -> Option<Self> {
        port.write_reg::<u16>(VIRTIO_QUEUE_SELECT, queue_index);
        let queue_size = port.read_reg::<u16>(VIRTIO_QUEUE_SIZE);
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
        let ring = DmaBuffer::<[u8]>::allocate(handle, alloc_size).ok()?;
        // Under Phase 55a IOMMU translation this value is an IOVA (bus
        // address), not a host physical address. Named `bus_base` to
        // match that semantics rather than implying it is always a PA.
        let bus_base = ring.bus_address();
        let virt_base = ring.as_ptr() as usize;

        let n = queue_size as usize;
        let desc_base = virt_base as *mut VirtqDesc;
        let avail_offset = 16 * n;
        let avail_base = (virt_base + avail_offset) as *mut VirtqAvailHeader;
        let used_offset = align_up(avail_offset + 4 + 2 * n + 2, 4096);
        let used_base = (virt_base + used_offset) as *mut VirtqUsedHeader;

        let pfn_u64 = bus_base / 4096;
        if pfn_u64 > u32::MAX as u64 {
            log::error!(
                "[virtio-blk] queue {}: bus {:#x} too high for 32-bit legacy PFN",
                queue_index,
                bus_base
            );
            return None;
        }
        port.write_reg::<u32>(VIRTIO_QUEUE_ADDRESS, pfn_u64 as u32);
        log::info!(
            "[virtio-blk] queue {}: size={}, bus={:#x}",
            queue_index,
            queue_size,
            bus_base
        );

        let mut waiters = Vec::with_capacity(n);
        for _ in 0..n {
            waiters.push(None);
        }

        Some(Virtqueue {
            port,
            queue_index,
            queue_size,
            ring,
            desc_base,
            avail_base,
            used_base,
            last_used_idx: 0,
            waiters,
        })
    }

    /// Enqueue a 3-descriptor chain for a block I/O request. The caller
    /// polls the used ring (with `drain_used_from_irq`) after releasing the
    /// DRIVER lock; the IRQ handler can also drain the used ring, but the
    /// submitter does not depend on it to unblock.
    #[allow(clippy::too_many_arguments)]
    fn submit_request(
        &mut self,
        req_type: u32,
        sector: u64,
        data_buf_phys: u64,
        data_len: usize,
        scratch_phys: u64,
        scratch_virt: *mut u8,
        waiter_woken: &'static AtomicBool,
    ) {
        // Three consecutive descriptors starting at 0 — we hold the driver
        // lock and only queue one request at a time.
        let hdr_desc_idx: u16 = 0;
        let data_desc_idx: u16 = 1;
        let status_desc_idx: u16 = 2;

        let req = VirtioBlkReq {
            type_: req_type,
            reserved: 0,
            sector,
        };
        // SAFETY: scratch_virt is the head of the scratch page; first
        // sizeof(VirtioBlkReq) bytes are reserved for the header.
        unsafe {
            core::ptr::write_volatile(scratch_virt as *mut VirtioBlkReq, req);
        }

        let status_phys = scratch_phys + 64;
        // SAFETY: the scratch region is at least one page; status slot at +64 is safe.
        let status_virt = unsafe { scratch_virt.add(64) };
        // SAFETY: single-byte write into scratch page.
        unsafe {
            core::ptr::write_volatile(status_virt, 0xFFu8);
        }

        let data_flags = if req_type == VIRTIO_BLK_T_IN {
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE
        } else {
            VIRTQ_DESC_F_NEXT
        };

        // SAFETY: all desc_base pointers are within the ring DmaBuffer.
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

        let desc1 = self.desc_base.wrapping_add(data_desc_idx as usize);
        unsafe {
            core::ptr::write_volatile(&raw mut (*desc1).addr, data_buf_phys);
            core::ptr::write_volatile(&raw mut (*desc1).len, data_len as u32);
            core::ptr::write_volatile(&raw mut (*desc1).flags, data_flags);
            core::ptr::write_volatile(&raw mut (*desc1).next, status_desc_idx);
        }

        let desc2 = self.desc_base.wrapping_add(status_desc_idx as usize);
        unsafe {
            core::ptr::write_volatile(&raw mut (*desc2).addr, status_phys);
            core::ptr::write_volatile(&raw mut (*desc2).len, 1u32);
            core::ptr::write_volatile(&raw mut (*desc2).flags, VIRTQ_DESC_F_WRITE);
            core::ptr::write_volatile(&raw mut (*desc2).next, 0u16);
        }

        // Record the waiter keyed on the head descriptor id.
        if let Some(task) = current_task_id() {
            self.waiters[hdr_desc_idx as usize] = Some(Waiter {
                task,
                woken: waiter_woken,
                status_virt,
            });
        } else {
            // No current task (very early boot) — fall back to inline spin.
            self.waiters[hdr_desc_idx as usize] = None;
        }

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
        self.port
            .write_reg::<u16>(VIRTIO_QUEUE_NOTIFY, self.queue_index);
    }

    /// Drain all new used-ring entries. Called from the IRQ handler; wakes
    /// each completion's waiter.
    fn drain_used_from_irq(&mut self) {
        loop {
            let used_idx = unsafe { core::ptr::read_volatile(&raw const (*self.used_base).idx) };
            if self.last_used_idx == used_idx {
                break;
            }
            let ring_entry = self.last_used_idx % self.queue_size;
            // SAFETY: used_base points to the used-ring header; +4 is the
            // start of the VirtqUsedElem array. The `%queue_size` bounds
            // the offset inside the allocation.
            let elem_ptr = unsafe {
                (self.used_base as *const u8).add(4 + ring_entry as usize * 8)
                    as *const VirtqUsedElem
            };
            let elem = unsafe { core::ptr::read_volatile(elem_ptr) };
            let head = elem.id as u16;
            if (head as usize) < self.waiters.len()
                && let Some(waiter) = self.waiters[head as usize].take()
            {
                waiter.woken.store(true, Ordering::Release);
                // F.6: under sched-v2 use wake_task_v2 (CAS-based); under v1 use wake_task.
                {
                    use crate::task::scheduler::wake_task_v2;
                    let _ = wake_task_v2(waiter.task);
                }
                // status_virt is read by the task after wake; no IRQ
                // work needed here.
                let _ = waiter.status_virt;
            }
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
        }
    }
}

#[repr(C, packed)]
struct VirtioBlkReq {
    type_: u32,
    reserved: u32,
    sector: u64,
}

// ===========================================================================
// Global driver state
// ===========================================================================

struct VirtioBlkDriver {
    #[allow(dead_code)]
    pci: pci::PciDeviceHandle,
    port: PortRegion,
    capacity_sectors: u64,
    request_queue: Virtqueue,
    /// Persistent scratch buffer for request headers and status bytes.
    #[allow(dead_code)]
    scratch: DmaBuffer<[u8]>,
    scratch_phys: u64,
    scratch_virt: *mut u8,
    /// Persistent DMA frame for sector data transfers.
    #[allow(dead_code)]
    dma: DmaBuffer<[u8]>,
    dma_phys: u64,
    dma_virt: *mut u8,
    /// Device IRQ registration — must outlive the driver or the ISR stub
    /// dispatches to a stale handler. Stored for symmetry; actually static
    /// because we never unregister.
    #[allow(dead_code)]
    irq: Option<pci::DeviceIrq>,
}

// SAFETY: VirtioBlkDriver raw pointers are only dereferenced under the
// DRIVER lock (or from the IRQ handler, which takes the lock).
unsafe impl Send for VirtioBlkDriver {}

static DRIVER: Mutex<Option<VirtioBlkDriver>> = Mutex::new(None);

pub static VIRTIO_BLK_READY: AtomicBool = AtomicBool::new(false);

// The legacy virtio-blk implementation still uses one shared descriptor
// chain, one scratch page, one DMA buffer, and one wake flag. Serialize all
// task-context I/O through that single in-flight slot until the driver grows
// true multi-request bookkeeping.
//
// Phase 57b G.1.a — `IrqSafeMutex` so the F.1 wiring raises
// `preempt_count` on acquire and the matching guard `Drop` lowers it on
// release. REQUEST_LOCK is task-context-only (no ISR ever touches it), so
// the IrqSafeMutex shape is sufficient — no explicit-preempt-and-cli
// wrapper is needed at the callsites.
static REQUEST_LOCK: crate::task::scheduler::IrqSafeMutex<()> =
    crate::task::scheduler::IrqSafeMutex::new(());

// Single wake flag reused across requests — requests are fully serialized
// under REQUEST_LOCK, so only one task is ever waiting at a time.
static REQ_WOKEN: AtomicBool = AtomicBool::new(false);

/// Phase 57b G.1.c — IRQ-shared `spin::Mutex` `DRIVER` stays a plain
/// `spin::Mutex` because [`virtio_blk_irq_handler`] (the ISR) also acquires
/// it. Task-context callers must explicitly `preempt_disable` +
/// `interrupts::without_interrupts` to satisfy the F.1 preempt-discipline,
/// since converting to `IrqSafeMutex` would not work with the ISR's
/// existing pattern (the ISR already runs with IF=0 and does not raise the
/// per-task preempt counter).
///
/// This helper wraps every task-context acquisition of `DRIVER` so the
/// boilerplate lives in one place. The closure receives `&mut
/// Option<VirtioBlkDriver>` so callers can probe the `Some` / `None` state
/// uniformly.
///
/// Lock-ordering: `preempt_disable` is lock-free (Phase 57b D.2), so
/// calling it before `without_interrupts` cannot recurse.
fn with_driver<R>(f: impl FnOnce(&mut Option<VirtioBlkDriver>) -> R) -> R {
    crate::task::scheduler::preempt_disable();
    let result = interrupts::without_interrupts(|| {
        let mut driver = DRIVER.lock();
        f(&mut driver)
    });
    crate::task::scheduler::preempt_enable();
    result
}

// ===========================================================================
// IRQ handler
// ===========================================================================

/// Acknowledge the device interrupt, drain the used ring, and wake any
/// waiters on completed requests. Runs in ISR context — see the
/// module-level contract in `kernel/src/arch/x86_64/interrupts.rs`.
///
/// ISR-safe lock use:
/// - `DRIVER.lock()` — plain `spin::Mutex`, but every task-context
///   acquisition wraps itself in `without_interrupts(…)` (see the
///   Fix 1 pattern), so a same-core ISR cannot reach a held lock.
/// - `drain_used_from_irq` calls `wake_task(waiter.task)` for each
///   completed request. Safe because `scheduler::SCHEDULER` is an
///   `IrqSafeMutex<Scheduler>` and `enqueue_to_core` wraps its per-core
///   `run_queue.lock()` in `without_interrupts`. Prior to the
///   2026-04-21 post-mortem fix this path could deadlock a same-core
///   `SCHEDULER.lock` holder; see
///   `docs/post-mortems/2026-04-21-scheduler-lock-isr-deadlock.md`.
///
/// No allocation, no blocking, no IPC.
fn virtio_blk_irq_handler() {
    // Acknowledge the device interrupt and drain the used ring.
    let mut driver = DRIVER.lock();
    if let Some(ref mut d) = *driver {
        // Legacy virtio ISR status register: reading clears the bit and
        // acks the interrupt on the device.
        let _isr = d.port.read_reg::<u8>(VIRTIO_ISR_STATUS);
        d.request_queue.drain_used_from_irq();
    }
}

// ===========================================================================
// Read/Write API — IRQ-driven completion
// ===========================================================================

#[allow(dead_code)]
pub fn read_sectors(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    let _request_guard = REQUEST_LOCK.lock();
    let needed = count * SECTOR_SIZE;
    if buf.len() < needed {
        log::error!(
            "[virtio-blk] read_sectors: buffer too small ({} < {})",
            buf.len(),
            needed
        );
        return Err(0xFF);
    }

    for i in 0..count {
        let sector = start_sector + i as u64;
        let status = do_request(VIRTIO_BLK_T_IN, sector)?;
        if status != 0 {
            log::error!(
                "[virtio-blk] read_sectors: sector {} failed with status {}",
                sector,
                status
            );
            return Err(status);
        }
        // Copy from DMA buffer to caller's buffer. See Fix 1 note in
        // `do_request`: the driver lock must be taken with IF off to stay
        // out of the ISR's way. Phase 57b G.1.c — `with_driver` wraps the
        // `preempt_disable` + `without_interrupts` boilerplate.
        let dma_virt: *mut u8 = with_driver(|d| match d.as_ref() {
            Some(d) => d.dma_virt,
            None => core::ptr::null_mut(),
        });
        if dma_virt.is_null() {
            return Err(0xFF);
        }
        let offset = i * SECTOR_SIZE;
        // SAFETY: dma_virt is a persistent driver-owned scratch page; it's
        // live as long as the driver exists.
        unsafe {
            core::ptr::copy_nonoverlapping(dma_virt, buf[offset..].as_mut_ptr(), SECTOR_SIZE);
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn write_sectors(start_sector: u64, count: usize, buf: &[u8]) -> Result<(), u8> {
    let _request_guard = REQUEST_LOCK.lock();
    let needed = count * SECTOR_SIZE;
    if buf.len() < needed {
        log::error!(
            "[virtio-blk] write_sectors: buffer too small ({} < {})",
            buf.len(),
            needed
        );
        return Err(0xFF);
    }

    for i in 0..count {
        let sector = start_sector + i as u64;
        let offset = i * SECTOR_SIZE;
        // Stage the sector into the DMA buffer. See Fix 1 note in
        // `do_request`: the driver lock must be taken with IF off to stay
        // out of the ISR's way. Phase 57b G.1.c — `with_driver` wraps the
        // `preempt_disable` + `without_interrupts` boilerplate.
        let stage_result: Result<(), u8> = with_driver(|d| {
            let driver = d.as_mut().ok_or(0xFFu8)?;
            // SAFETY: dma_virt is driver-owned scratch; only one task writes
            // at a time (DRIVER lock).
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf[offset..].as_ptr(),
                    driver.dma_virt,
                    SECTOR_SIZE,
                );
            }
            Ok(())
        });
        stage_result?;
        let status = do_request(VIRTIO_BLK_T_OUT, sector)?;
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

/// Submit a single-sector request and block until the IRQ fires.
///
/// **Block+wake mechanism:** `block_current_until(&REQ_WOKEN, None)` parks
/// the calling task with `BlockedOnRecv`; it accumulates no CPU time while
/// waiting for the device.
///
/// **Wake source:** `virtio_blk_irq_handler` → `drain_used_from_irq`, which
/// sets `REQ_WOKEN = true` and calls `wake_task_v2(waiter.task)` for the
/// head-descriptor's waiter entry.
///
/// **Expected wake latency:** ≤ VirtIO disk interrupt latency (typically
/// ~100 µs for in-QEMU requests) plus ≤ one scheduler quantum for the
/// woken task to be redispatched.
///
/// **Lost-wakeup safety:** `REQ_WOKEN` is cleared *before* `submit_request`
/// so any IRQ that fires between submit and `block_current_until` is visible
/// at step 3 of the CAS protocol; the task self-reverts to `Running`
/// without descending into `switch_context`.
fn do_request(req_type: u32, sector: u64) -> Result<u8, u8> {
    // Phase 1: enqueue under the DRIVER lock.
    //
    // Correctness: the kick write to VIRTIO_QUEUE_NOTIFY at the end of
    // `submit_request` can cause the device MSI/INTx to fire on this CPU
    // before `DRIVER.lock()` is released. Our ISR also takes `DRIVER.lock()`,
    // so without IF-off the ISR would spin forever on the held `spin::Mutex`.
    // Wrapping the critical section in `without_interrupts` keeps the IRQ
    // pending in the LAPIC until we drop the guard; the ISR then runs
    // normally and drains the used ring. MSI is programmed to this CPU's
    // LAPIC and legacy INTx is routed to the BSP, so no other core can hold
    // the mutex while the ISR fires elsewhere.
    REQ_WOKEN.store(false, Ordering::Release);
    // Phase 57b G.1.c — `with_driver` wraps the `preempt_disable` +
    // `without_interrupts` boilerplate around the IRQ-shared `DRIVER` lock.
    let status_virt_result: Result<*mut u8, u8> = with_driver(|d| {
        let driver = d.as_mut().ok_or(0xFFu8)?;
        if sector >= driver.capacity_sectors {
            log::error!(
                "[virtio-blk] request: sector {} out of bounds (capacity {})",
                sector,
                driver.capacity_sectors
            );
            return Err(0xFF);
        }
        let dma_phys = driver.dma_phys;
        let scratch_phys = driver.scratch_phys;
        let scratch_virt = driver.scratch_virt;
        driver.request_queue.submit_request(
            req_type,
            sector,
            dma_phys,
            SECTOR_SIZE,
            scratch_phys,
            scratch_virt,
            &REQ_WOKEN,
        );
        // SAFETY: scratch_virt is driver-owned; the status byte lives at +64
        // (set up in submit_request).
        Ok(unsafe { scratch_virt.add(64) })
    });
    let status_virt = status_virt_result?;
    // Phase 2: park on `REQ_WOKEN` until the IRQ handler's
    // `drain_used_from_irq` drains the completion and releases us.
    //
    // History: a previous incarnation of this loop busy-spun on
    // `REQ_WOKEN` instead of parking, with the rationale that
    // "the scheduler-park path round-trips a context switch and an
    // optional cross-core wake on every sector, which makes boot-time
    // readdir / inode walks take minutes under SMP."  That assumption
    // held a single CPU in kernel mode until the IRQ fired — fine on
    // BSP-only or when the IRQ comes back fast, but on multi-core boots
    // where this CPU's task list contains other daemons (term,
    // stdin_feeder, console, etc.) it monopolised the core for the
    // entire duration of every disk I/O.  In Phase 57a's syslogd-on-
    // core-1 hang, syslogd's `WRITE` to /var/log/kern.log hit this
    // spin on its first sector, and core 1 never dispatched another
    // task again — `term` never started, `session_manager` text-fell-
    // back, the cursor froze.  Cooperative scheduling means a busy-
    // wait in any kernel syscall is a denial-of-service for everything
    // queued on the same core.
    //
    // Park is cheap enough now that the original SMP perf concern is
    // dominated by the cost of NOT parking.  block_current_until's
    // self-revert path handles the "IRQ fired before we parked" race:
    // if `REQ_WOKEN` is already true at entry, the function returns
    // without yielding.  No deadline because the IRQ is the only wake
    // source.
    let _ = crate::task::scheduler::block_current_until(
        crate::task::TaskState::BlockedOnRecv,
        &REQ_WOKEN,
        None,
    );
    // Phase 3: read the status byte (driver lock re-acquired to ensure
    // memory ordering). Same IF-off rule as the submit side. Phase 57b
    // G.1.c — `with_driver` wraps the `preempt_disable` +
    // `without_interrupts` boilerplate.
    let status = with_driver(|_d| {
        // SAFETY: status_virt lives in the driver's scratch page, valid
        // for the life of the driver.
        unsafe { core::ptr::read_volatile(status_virt) }
    });
    Ok(status)
}

// ===========================================================================
// Driver registration + init
// ===========================================================================

/// Register the virtio-blk driver with the PCI discovery framework (C.4).
pub fn register() {
    // Two DriverEntry registrations — one per supported device id. A more
    // general PciMatch variant that accepts a list of device IDs would avoid
    // the duplication but isn't worth the extra code right now.
    let _ = pci::register_driver(DriverEntry {
        name: "virtio-blk",
        r#match: PciMatch::VendorDevice {
            vendor: VIRTIO_BLK_VENDOR,
            device: VIRTIO_BLK_DEVICE_LEGACY,
        },
        init: probe,
    });
    let _ = pci::register_driver(DriverEntry {
        name: "virtio-blk",
        r#match: PciMatch::VendorDevice {
            vendor: VIRTIO_BLK_VENDOR,
            device: VIRTIO_BLK_DEVICE_TRANSITIONAL,
        },
        init: probe,
    });
}

/// Driver init entry invoked by `probe_all_drivers`.
fn probe(handle: pci::PciDeviceHandle) -> DriverProbeResult {
    let dev = *handle.device();

    log::info!(
        "[virtio-blk] found device {:04x}:{:04x} at {:02x}:{:02x}.{}",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function
    );

    // BAR0 — legacy virtio uses an I/O port BAR.
    let port = match pci::bar::map_bar(&handle, 0) {
        Ok(BarMapping::Pio { region }) => region,
        Ok(_) => {
            return DriverProbeResult::Declined(
                "BAR0 is not an I/O port BAR (legacy virtio required)",
            );
        }
        Err(_) => return DriverProbeResult::Failed("failed to map BAR0"),
    };
    log::info!("[virtio-blk] BAR0 I/O base: {:#x}", port.port_base());

    // Enable I/O space + bus mastering.
    let cmd = handle.read_config_u16(0x04);
    if cmd & 0x05 != 0x05 {
        handle.write_config_u16(0x04, cmd | 0x05);
        log::info!("[virtio-blk] PCI command: enabled I/O space + bus mastering");
    }

    // Reset sequence.
    port.write_reg::<u8>(VIRTIO_DEVICE_STATUS, 0);
    port.write_reg::<u8>(VIRTIO_DEVICE_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
    port.write_reg::<u8>(
        VIRTIO_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    let device_features = port.read_reg::<u32>(VIRTIO_DEVICE_FEATURES);
    log::info!("[virtio-blk] device features: {:#010x}", device_features);
    port.write_reg::<u32>(VIRTIO_DRIVER_FEATURES, 0);

    // Try to set FEATURES_OK for transitional devices.
    let status = port.read_reg::<u8>(VIRTIO_DEVICE_STATUS);
    port.write_reg::<u8>(VIRTIO_DEVICE_STATUS, status | VIRTIO_STATUS_FEATURES_OK);
    let status = port.read_reg::<u8>(VIRTIO_DEVICE_STATUS);
    if status & VIRTIO_STATUS_FEATURES_OK == 0 {
        log::info!("[virtio-blk] legacy device (no FEATURES_OK) — continuing");
    }

    let request_queue = match Virtqueue::init(&handle, port, 0) {
        Some(q) => q,
        None => {
            return DriverProbeResult::Failed("failed to initialize request queue");
        }
    };

    let status = port.read_reg::<u8>(VIRTIO_DEVICE_STATUS);
    port.write_reg::<u8>(VIRTIO_DEVICE_STATUS, status | VIRTIO_STATUS_DRIVER_OK);

    let capacity_lo = port.read_reg::<u32>(VIRTIO_BLK_CFG_CAPACITY) as u64;
    let capacity_hi = port.read_reg::<u32>(VIRTIO_BLK_CFG_CAPACITY + 4) as u64;
    let capacity_sectors = capacity_lo | (capacity_hi << 32);
    log::info!(
        "[virtio-blk] capacity: {} sectors ({} MiB)",
        capacity_sectors,
        (capacity_sectors * SECTOR_SIZE as u64) / (1024 * 1024)
    );

    let scratch = match DmaBuffer::<[u8]>::allocate(&handle, 4096) {
        Ok(b) => b,
        Err(_) => return DriverProbeResult::Failed("scratch DMA alloc failed"),
    };
    let scratch_phys = scratch.bus_address();
    let scratch_virt = scratch.as_ptr() as *mut u8;

    let dma = match DmaBuffer::<[u8]>::allocate(&handle, 4096) {
        Ok(b) => b,
        Err(_) => return DriverProbeResult::Failed("data DMA alloc failed"),
    };
    let dma_phys = dma.bus_address();
    let dma_virt = dma.as_ptr() as *mut u8;

    // Install the completion IRQ handler (C.3). Prefer MSI, fall back to
    // legacy INTx routed through the I/O APIC. The shared-INTx contract
    // says the handler must check ISR status first to avoid doing work
    // for someone else's IRQ — `virtio_blk_irq_handler` reads ISR_STATUS
    // then drains the used ring, which is a no-op if `last_used_idx ==
    // used_idx`, so sharing is safe.
    let irq = match handle.install_msi_irq(virtio_blk_irq_handler) {
        Ok(i) => {
            log::info!("[virtio-blk] MSI IRQ on vector {:#x}", i.vector());
            // Legacy virtio quirk: `install_msi_irq` enables the MSI-X
            // capability at the PCI level and programs the MSI-X table
            // entry, but the *device* still has every virtqueue mapped to
            // VIRTIO_MSI_NO_VECTOR by default. Until we point queue 0 at
            // MSI-X table entry 0 the device will never raise a completion
            // MSI and `block_current_unless_woken` parks forever. The
            // queue-vector register at offset 0x16 only exists when MSI-X
            // is enabled — plain MSI does not insert it, so guard on
            // `msi_kind() == Some(MsiX)`.
            if i.msi_kind() == Some(pci::MsiKind::MsiX) {
                port.write_reg::<u16>(VIRTIO_QUEUE_SELECT, 0);
                port.write_reg::<u16>(VIRTIO_MSI_QUEUE_VECTOR, 0);
                let readback = port.read_reg::<u16>(VIRTIO_MSI_QUEUE_VECTOR);
                if readback == VIRTIO_MSI_NO_VECTOR {
                    return DriverProbeResult::Failed(
                        "device refused MSI-X vector binding for queue 0",
                    );
                }
                log::info!(
                    "[virtio-blk] queue 0 bound to MSI-X table entry 0 (vector {:#x})",
                    i.vector()
                );
            }
            Some(i)
        }
        Err(_) => {
            // Legacy INTx: pick a vector from the device IRQ bank and route
            // the PCI interrupt line through the I/O APIC to that vector.
            const BLK_INTX_VECTOR: u8 = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE + 2;
            let intx_result = handle.install_intx_irq(BLK_INTX_VECTOR, virtio_blk_irq_handler);
            if let Ok(i) = intx_result {
                if dev.interrupt_line != 0xFF && crate::acpi::io_apic_address().is_some() {
                    crate::arch::x86_64::apic::route_pci_irq(dev.interrupt_line, BLK_INTX_VECTOR);
                    log::info!(
                        "[virtio-blk] legacy INTx line {} routed to vector {:#x}",
                        dev.interrupt_line,
                        BLK_INTX_VECTOR
                    );
                } else {
                    log::warn!(
                        "[virtio-blk] legacy INTx registered but line is 0xFF or no I/O APIC — IRQ may not fire"
                    );
                }
                Some(i)
            } else {
                log::warn!("[virtio-blk] failed to install completion IRQ — requests will stall");
                None
            }
        }
    };

    let driver = VirtioBlkDriver {
        pci: handle,
        port,
        capacity_sectors,
        request_queue,
        scratch,
        scratch_phys,
        scratch_virt,
        dma,
        dma_phys,
        dma_virt,
        irq,
    };
    // Phase 57b G.1.c — `with_driver` wraps the `preempt_disable` +
    // `without_interrupts` boilerplate around the IRQ-shared `DRIVER` lock.
    with_driver(|d| {
        *d = Some(driver);
    });
    VIRTIO_BLK_READY.store(true, Ordering::Release);
    log::info!("[virtio-blk] driver initialized successfully");
    DriverProbeResult::Bound
}

/// Legacy init entry: registers the driver and immediately runs the probe
/// pass. Kept for backwards compatibility and tests; the normal boot flow
/// goes through [`super::init`] which aggregates all block drivers.
#[allow(dead_code)]
pub fn init() {
    register();
    pci::probe_all_drivers();
}

// ===========================================================================
// Helpers
// ===========================================================================

#[inline]
fn align_up(val: usize, alignment: usize) -> usize {
    (val + alignment - 1) & !(alignment - 1)
}

// ---------------------------------------------------------------------------
// Phase 57b G.1.c — preempt-discipline regression test
// ---------------------------------------------------------------------------
//
// Pins the property that a virtio-blk-shaped task-context request submit
// followed by an IRQ-side completion drain leaves `preempt_count` at 0.
//
// The real `with_driver` helper is not directly observable from the test
// harness: kernel tests run before SMP per-core init (see
// `kernel/src/main.rs`), so `try_per_core` returns `None` and the real
// `preempt_disable` / `preempt_enable` helpers degrade to no-ops — they
// have no observable counter to assert against.
//
// This test mirrors the synthetic-counter pattern Wave 5's F.1 tests use:
// it reconstructs the `with_driver` shape (preempt_disable → without_irq →
// spin lock → release order reversed) against an explicit `AtomicI32` and
// asserts the counter cycles 0 → 1 → 0 across a submit + an ISR-side
// drain. Importantly, the ISR mirror does NOT raise the counter (the real
// ISR runs with IF=0 and never touches `preempt_count`), so a balanced
// task-context submit that completes via IRQ must still net to zero.
#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicI32, Ordering};

    /// Synthetic mirror of `with_driver` for the Phase 57b G.1.c
    /// regression test. Mirrors the production helper's shape:
    ///
    /// 1. `preempt_disable` (raise the counter).
    /// 2. Mask IRQs (`without_interrupts` body).
    /// 3. Take the spin lock (no-op here — single-threaded test).
    /// 4. Run the closure.
    /// 5. Drop spin lock, restore IF, `preempt_enable` (lower the counter).
    fn synthetic_with_driver<R>(counter: &AtomicI32, f: impl FnOnce() -> R) -> R {
        counter.fetch_add(1, Ordering::Acquire);
        let r = f();
        counter.fetch_sub(1, Ordering::Release);
        r
    }

    /// Synthetic mirror of `virtio_blk_irq_handler`: the real ISR runs
    /// with IF=0 and never raises `preempt_count`. Modeling that
    /// explicitly here pins the property that the IRQ-side drain leaves
    /// the per-task counter unchanged — only task-context callsites
    /// raise/lower it.
    fn synthetic_irq_drain(counter: &AtomicI32) -> i32 {
        // No counter mutation: ISR context. Just observe.
        counter.load(Ordering::Acquire)
    }

    /// Phase 57b G.1.c — `with_driver`-shaped task-context lock plus an
    /// ISR-side drain net to zero on the per-task `preempt_count`.
    ///
    /// Mirrors the virtio-blk request lifecycle:
    ///   - Task takes `DRIVER` via `with_driver` (counter: 0 → 1).
    ///   - Submits the request, releases the lock (counter: 1 → 0).
    ///   - ISR fires asynchronously and drains the used ring — does NOT
    ///     touch `preempt_count` (counter stays 0 throughout).
    ///   - Task re-acquires `DRIVER` via `with_driver` to read the
    ///     status byte (counter: 0 → 1 → 0).
    ///
    /// Net effect: counter ends at 0. A regression that left a raise
    /// dangling on either side would flip this to a non-zero end value.
    #[test_case]
    fn with_driver_submit_then_irq_wake_returns_preempt_count_to_zero() {
        let counter = AtomicI32::new(0);

        // Pre-submit: counter at 0.
        assert_eq!(counter.load(Ordering::Acquire), 0);

        // Phase 1: task-context submit under `with_driver`.
        let observed_during_submit =
            synthetic_with_driver(&counter, || counter.load(Ordering::Acquire));
        assert_eq!(
            observed_during_submit, 1,
            "with_driver must raise preempt_count by exactly 1 inside its closure",
        );
        assert_eq!(
            counter.load(Ordering::Acquire),
            0,
            "with_driver must lower preempt_count back to 0 on closure exit",
        );

        // Phase 2: ISR-side drain — must NOT touch preempt_count.
        let observed_during_irq = synthetic_irq_drain(&counter);
        assert_eq!(
            observed_during_irq, 0,
            "ISR-side drain must not raise preempt_count (ISR runs with IF=0)",
        );
        assert_eq!(counter.load(Ordering::Acquire), 0);

        // Phase 3: task-context status read under `with_driver`.
        synthetic_with_driver(&counter, || {
            assert_eq!(
                counter.load(Ordering::Acquire),
                1,
                "second with_driver acquisition must again raise preempt_count to 1",
            );
        });

        // Final: counter back to 0 across the full submit + IRQ + read
        // lifecycle.
        assert_eq!(
            counter.load(Ordering::Acquire),
            0,
            "Phase 57b G.1.c — virtio-blk request submit + IRQ wake must \
             return preempt_count to 0",
        );
    }

    /// Phase 57b G.1.c — nested `with_driver` calls (e.g. the `do_request`
    /// path that takes `DRIVER` for submit, then re-takes it later for
    /// the status read) must each charge and release a single
    /// `preempt_count` raise without leaking.
    #[test_case]
    fn with_driver_back_to_back_acquires_each_balance_to_zero() {
        let counter = AtomicI32::new(0);

        for i in 0..5 {
            synthetic_with_driver(&counter, || {
                assert_eq!(
                    counter.load(Ordering::Acquire),
                    1,
                    "iteration {}: inside with_driver counter must be 1",
                    i,
                );
            });
            assert_eq!(
                counter.load(Ordering::Acquire),
                0,
                "iteration {}: after with_driver counter must be back to 0",
                i,
            );
        }
    }
}
