//! NVMe controller driver — Phase 55 Track D.
//!
//! Layering:
//!
//! * **D.0 (kernel-core)** — register offsets, SQ/CQ layouts, capability
//!   accessors — live in [`kernel_core::nvme`].
//! * **D.1** — PCI discovery, BAR0 MMIO mapping, `CAP`/`VS` parsing,
//!   controller reset with bounded `CAP.TO` timeout.
//! * **D.2** — admin queue bring-up, controller enable, Identify Controller /
//!   Identify Namespace; polled completions with a bounded per-command
//!   timeout.
//! * **D.3** — one I/O queue pair via Create I/O CQ / Create I/O SQ,
//!   `nvme_read_sectors` / `nvme_write_sectors` using PRP entries (with a
//!   PRP-list overflow page for buffers spanning >2 pages). `NVME_READY` is
//!   set; the block dispatch layer ([`super::read_sectors`] /
//!   [`super::write_sectors`]) prefers NVMe when ready.
//! * **D.4 (this commit)** — MSI / MSI-X completion handler. The handler
//!   drains both admin and I/O CQs in one ISR invocation (phase-bit walk),
//!   writes the CQ-head doorbells, and wakes blocked tasks via `wake_task`.
//!   When the task context is available and IRQ registration succeeds, the
//!   submitter parks itself with `block_current_unless_woken`. Environments
//!   where MSI / MSI-X allocation fails fall back to the polled path
//!   preserved from D.3.
//!
//! # Ring-0 placement
//!
//! This driver is ring 0 (Phase 55 deliberately widens the TCB to ship a
//! real-hardware path). The public contract (`probe`, Identify, block I/O)
//! is written so a future userspace driver host can lift the call sites
//! without surgery.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use kernel_core::nvme as knvme;
use kernel_core::types::TaskId;
use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::mm::dma::DmaBuffer;
use crate::pci::bar::{BarMapping, MmioRegion};
use crate::pci::{self, DriverEntry, DriverProbeResult, PciMatch};
use crate::task::scheduler::{block_current_unless_woken, current_task_id, wake_task};

// ===========================================================================
// PCI class / subclass / programming interface
// ===========================================================================

/// NVMe class + subclass + programming interface per PCI Code and ID
/// Assignment Specification §D (0x01 = mass storage, 0x08 = NVM, 0x02 =
/// NVM Express).
const NVME_CLASS: u8 = 0x01;
const NVME_SUBCLASS: u8 = 0x08;
const NVME_PROG_IF: u8 = 0x02;

// ===========================================================================
// Driver constants
// ===========================================================================

/// Extra safety margin on top of `CAP.TO * 500 ms`. Ensures a device that
/// overshoots its advertised timeout by a small amount does not cause us to
/// falsely declare the reset wedged.
const RESET_SAFETY_MARGIN_TICKS: u64 = 1_000;

/// Upper bound on a single admin command completion, in ticks (ms).
/// Identify, Create I/O CQ/SQ should all complete in well under a second on
/// QEMU; we give a generous 5 seconds before surfacing a bounded error.
const ADMIN_COMMAND_TIMEOUT_TICKS: u64 = 5_000;

/// Queue depth for the admin queue. 64 is well above what a single-threaded
/// bring-up sequence needs (we never have more than 2-3 admin commands
/// in-flight) and stays far below every `CAP.MQES` value we expect in the
/// wild.
const ADMIN_QUEUE_ENTRIES: usize = 64;

/// Queue depth for the single I/O queue pair (Phase 55 scope). 64 entries is
/// more than enough — we serialize one request at a time under the driver
/// lock, same as virtio-blk today.
const IO_QUEUE_ENTRIES: usize = 64;

/// Hardware page size we assume for PRP arithmetic. NVMe per-spec supports
/// `2^(12 + MPSMIN) ..= 2^(12 + MPSMAX)`; we fix to 4 KiB because everything
/// else in the kernel (frame allocator, `DmaBuffer`) works in 4 KiB pages.
const NVME_PAGE_BYTES: usize = 4096;

/// Our I/O queue is always qid=1 (admin is qid=0, only one data queue for
/// Phase 55). Kept as a named constant so the doorbell math is self-explanatory.
const IO_QUEUE_ID: u16 = 1;

// ===========================================================================
// Controller state
// ===========================================================================

/// Single bound NVMe controller. One per device — a second matching device
/// is declined rather than bound.
pub struct NvmeController {
    /// PCI claim handle — held for the life of the driver.
    #[allow(dead_code)]
    pci: pci::PciDeviceHandle,
    /// BAR0 MMIO region. Size checked at probe time to cover the doorbell
    /// range (>= 0x1000 bytes).
    regs: MmioRegion,
    /// Decoded `CAP.DSTRD` stride in bytes (minimum 4).
    doorbell_stride_bytes: usize,
    /// `CAP.MQES + 1` — maximum queue entries the hardware will accept.
    max_queue_entries: u16,
    /// Raw `VS` register contents.
    #[allow(dead_code)]
    version: u32,
    /// `CAP.TO`-derived polling window in ticks (ms).
    reset_timeout_ticks: u64,
    /// Admin queue pair (D.2).
    admin: Option<AdminQueue>,
    /// I/O queue pair (D.3). `None` until `bring_up_io_queue` succeeds.
    io: Option<IoQueuePair>,
    /// Completion IRQ registration (D.4). `Some` when an MSI / MSI-X or
    /// legacy-INTx handler is installed; `None` forces the polled
    /// completion fallback. Held for the life of the driver so the
    /// dispatch stub does not land on a stale handler.
    #[allow(dead_code)]
    irq: Option<pci::DeviceIrq>,
    /// Active namespace identifier. Picked during Identify as the first
    /// active namespace.
    namespace_id: u32,
    /// Namespace capacity in LBAs.
    namespace_lbas: u64,
    /// Sector size in bytes (`2^LBADS`). Typically 512 on QEMU.
    namespace_sector_bytes: u32,
}

// SAFETY: NvmeController is only accessed under DRIVER.lock(). MMIO
// addresses stay valid for the life of the driver.
unsafe impl Send for NvmeController {}

pub(super) static DRIVER: Mutex<Option<NvmeController>> = Mutex::new(None);

/// Signals to the block dispatch layer that NVMe is ready and bound. Set in
/// D.3 once the I/O queue pair and Identify have succeeded.
#[allow(dead_code)]
pub static NVME_READY: AtomicBool = AtomicBool::new(false);

// ===========================================================================
// Admin queue
// ===========================================================================

/// Completion slot returned to the submitter. The IRQ handler writes the
/// completion in place and sets `filled`; the submitter observes the slot
/// after being woken (D.4) or after polling it directly (D.3 fallback).
#[derive(Clone, Copy)]
struct NvmeCompletionSlot {
    /// Command-specific result field from the completion entry. Stored but
    /// currently unused by our submitters.
    #[allow(dead_code)]
    result: u32,
    status_code: u16,
    filled: bool,
    /// TaskId to wake when this slot is filled. `TaskId(0)` means "no
    /// waiter" — the polled path picks it up by observing `filled`.
    waker_task: TaskId,
}

impl Default for NvmeCompletionSlot {
    fn default() -> Self {
        Self {
            result: 0,
            status_code: 0,
            filled: false,
            waker_task: TaskId(0),
        }
    }
}

struct AdminQueue {
    sq: DmaBuffer<[knvme::NvmeCommand]>,
    cq: DmaBuffer<[knvme::NvmeCompletion]>,
    queue_entries: u16,
    sq_tail: u16,
    cq_head: u16,
    /// Phase tag for the next expected completion. NVMe uses a toggling
    /// phase bit so we can detect "new" entries without a head pointer
    /// round trip.
    phase: bool,
    /// Per-CID completion slots. `slots[i]` matches a command with
    /// `CID == i as u16`.
    slots: Vec<NvmeCompletionSlot>,
    /// Monotonically increasing command id. Wrapped via `% queue_entries`
    /// before being stored in a command.
    next_cid: u16,
}

// SAFETY: AdminQueue is only accessed under DRIVER.lock().
unsafe impl Send for AdminQueue {}

// ===========================================================================
// I/O queue (D.3)
// ===========================================================================

/// One I/O queue pair — submission + completion. Phase 55 ships exactly one
/// of these; multiple pairs (per-CPU, per-namespace) are deferred.
struct IoQueuePair {
    sq: DmaBuffer<[knvme::NvmeCommand]>,
    cq: DmaBuffer<[knvme::NvmeCompletion]>,
    queue_entries: u16,
    sq_tail: u16,
    cq_head: u16,
    /// Phase tag tracker — same convention as [`AdminQueue::phase`].
    phase: bool,
    /// Per-CID slots. Mirrors the admin-queue waiter layout.
    slots: Vec<NvmeCompletionSlot>,
    next_cid: u16,
    /// Persistent PRP list page used when a request spans more than two
    /// pages. Allocated once and reused because we serialize at most one
    /// request in flight.
    prp_list: DmaBuffer<[u64]>,
}

// SAFETY: IoQueuePair is only accessed under DRIVER.lock().
unsafe impl Send for IoQueuePair {}

/// Single wake flag used by the I/O path. Requests are fully serialized
/// under DRIVER.lock(), so only one task waits at a time — reusing one
/// flag matches virtio-blk's `REQ_WOKEN` pattern.
static IO_REQ_WOKEN: AtomicBool = AtomicBool::new(false);

// ===========================================================================
// Driver registration
// ===========================================================================

/// Register the NVMe driver with the PCI HAL. Called from `blk::init()`.
pub fn register() {
    let _ = pci::register_driver(DriverEntry {
        name: "nvme",
        r#match: PciMatch::ClassSubclass {
            class: NVME_CLASS,
            subclass: NVME_SUBCLASS,
        },
        init: nvme_probe,
    });
}

/// Driver init entry invoked by `probe_all_drivers`.
fn nvme_probe(handle: pci::PciDeviceHandle) -> DriverProbeResult {
    let dev = *handle.device();

    if dev.prog_if != NVME_PROG_IF {
        return DriverProbeResult::Declined("non-NVM-Express programming interface");
    }
    if DRIVER.lock().is_some() {
        return DriverProbeResult::Declined("nvme controller already bound");
    }

    log::info!(
        "[nvme] probing {:04x}:{:04x} at {:02x}:{:02x}.{} (class {:02x}:{:02x}:{:02x})",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function,
        dev.class_code,
        dev.subclass,
        dev.prog_if
    );

    // BAR0 is mandated to be MMIO by the NVMe spec.
    let regs = match pci::bar::map_bar(&handle, 0) {
        Ok(BarMapping::Mmio { region, .. }) => region,
        Ok(_) => return DriverProbeResult::Declined("BAR0 is not MMIO"),
        Err(_) => return DriverProbeResult::Failed("failed to map BAR0"),
    };
    log::info!(
        "[nvme] BAR0 MMIO: virt {:#x} phys {:#x} size {:#x}",
        regs.virt_base(),
        regs.phys_base(),
        regs.size()
    );
    if regs.size() < 0x1000 {
        return DriverProbeResult::Failed("BAR0 too small for doorbell range");
    }

    // Enable memory space + bus mastering.
    let cmd = handle.read_config_u16(0x04);
    if cmd & 0x06 != 0x06 {
        handle.write_config_u16(0x04, cmd | 0x06);
        log::info!("[nvme] PCI command: enabled memory space + bus mastering");
    }

    let cap_raw = regs.read_reg::<u64>(knvme::NvmeRegs::CAP);
    let cap = knvme::NvmeCap(cap_raw);
    let version = regs.read_reg::<u32>(knvme::NvmeRegs::VS);
    let doorbell_stride_bytes = cap.doorbell_stride();
    let max_queue_entries = cap.mqes();
    let reset_timeout_ticks = reset_timeout_ticks(cap.timeout_500ms_units());
    log::info!(
        "[nvme] CAP={:#x} VS={:#x} MQES={} DSTRD={}B TO_budget={}ms CSS.NVM={}",
        cap_raw,
        version,
        max_queue_entries,
        doorbell_stride_bytes,
        reset_timeout_ticks,
        cap.css_nvme()
    );

    if !cap.css_nvme() {
        return DriverProbeResult::Failed("controller does not advertise NVM command set");
    }

    if let Err(e) = reset_controller(&regs, reset_timeout_ticks) {
        return DriverProbeResult::Failed(e);
    }

    *DRIVER.lock() = Some(NvmeController {
        pci: handle,
        regs,
        doorbell_stride_bytes,
        max_queue_entries,
        version,
        reset_timeout_ticks,
        admin: None,
        io: None,
        irq: None,
        namespace_id: 0,
        namespace_lbas: 0,
        namespace_sector_bytes: 0,
    });
    log::info!("[nvme] controller in RESET state; admin queue bring-up starting");

    if let Err(e) = bring_up_admin_and_identify() {
        log::error!("[nvme] admin bring-up failed: {}", e);
        *DRIVER.lock() = None;
        return DriverProbeResult::Failed(e);
    }

    if let Err(e) = bring_up_io_queue() {
        log::error!("[nvme] I/O queue bring-up failed: {}", e);
        *DRIVER.lock() = None;
        return DriverProbeResult::Failed(e);
    }

    NVME_READY.store(true, Ordering::Release);
    let (nsid, capacity) = {
        let drv = DRIVER.lock();
        drv.as_ref()
            .map(|d| (d.namespace_id, d.namespace_lbas))
            .unwrap_or((0, 0))
    };
    log::info!(
        "[nvme] driver initialized; active nsid={} capacity={} sectors",
        nsid,
        capacity
    );

    // Data-path smoke: write a sector-sized pattern to LBA 0 and read it
    // back. Catches PRP construction / doorbell / completion-decode bugs
    // early so they do not show up later as silent filesystem corruption.
    // If the smoke fails, NVMe stays bound but NVME_READY is cleared so the
    // block dispatch layer falls back to virtio-blk.
    if let Err(e) = data_path_smoke() {
        log::error!("[nvme] data-path smoke failed: {:#x}; falling back", e);
        NVME_READY.store(false, Ordering::Release);
    }
    DriverProbeResult::Bound
}

/// One-shot smoke test: write a known pattern to LBA 0, read it back, and
/// verify every byte matches. Runs once from `nvme_probe` before the
/// scheduler starts, so a silent I/O bug (PRP layout, doorbell offset,
/// phase-bit off-by-one) surfaces at bring-up rather than when userspace
/// first touches the device.
fn data_path_smoke() -> Result<(), u16> {
    let sector = namespace_sector_bytes() as usize;
    if sector == 0 {
        return Err(0xFFFF);
    }
    let mut tx = alloc::vec::Vec::with_capacity(sector);
    for i in 0..sector {
        tx.push(((i * 31 + 7) & 0xFF) as u8);
    }
    write_sectors(0, 1, &tx)?;
    let mut rx = alloc::vec![0u8; sector];
    read_sectors(0, 1, &mut rx)?;
    if rx != tx {
        log::error!(
            "[nvme] smoke mismatch: first differing byte at offset {}",
            rx.iter()
                .zip(tx.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(0)
        );
        return Err(0xFFFE);
    }
    log::info!(
        "[nvme] data-path smoke OK ({}B round-trip at LBA 0)",
        sector
    );
    Ok(())
}

/// Compute the reset / enable polling window in tick_count ms.
fn reset_timeout_ticks(to_500ms_units: u8) -> u64 {
    let units = to_500ms_units.max(1) as u64;
    units.saturating_mul(500) + RESET_SAFETY_MARGIN_TICKS
}

/// Spin on `f()` until it returns true or the tick budget expires.
fn wait_until<F>(mut f: F, budget_ticks: u64) -> Result<(), &'static str>
where
    F: FnMut() -> bool,
{
    const MAX_SPIN_ITERATIONS: u64 = 200_000_000;
    let start = crate::arch::x86_64::interrupts::tick_count();
    let mut iterations: u64 = 0;
    loop {
        if f() {
            return Ok(());
        }
        iterations = iterations.saturating_add(1);
        if iterations >= MAX_SPIN_ITERATIONS {
            return Err("wait_until: spin budget exceeded");
        }
        let now = crate::arch::x86_64::interrupts::tick_count();
        if now.wrapping_sub(start) >= budget_ticks {
            return Err("wait_until: tick budget exceeded");
        }
        core::hint::spin_loop();
    }
}

/// Disable the controller and wait (bounded) for `CSTS.RDY=0`.
fn reset_controller(regs: &MmioRegion, timeout_ticks: u64) -> Result<(), &'static str> {
    let cc = regs.read_reg::<u32>(knvme::NvmeRegs::CC);
    if cc & knvme::CC_EN != 0 {
        regs.write_reg::<u32>(knvme::NvmeRegs::CC, cc & !knvme::CC_EN);
    }
    wait_until(
        || regs.read_reg::<u32>(knvme::NvmeRegs::CSTS) & knvme::CSTS_RDY == 0,
        timeout_ticks,
    )
    .map_err(|_| "nvme reset timeout waiting for CSTS.RDY=0")?;
    log::info!("[nvme] controller disabled (CSTS.RDY cleared)");
    Ok(())
}

/// Enable the controller after admin queues are programmed. Bounded by
/// `timeout_ticks`.
fn enable_controller(regs: &MmioRegion, timeout_ticks: u64) -> Result<(), &'static str> {
    // CC: IOSQES=6 (64-byte SQ entry, 2^6), IOCQES=4 (16-byte CQ entry,
    // 2^4), MPS=0 (4 KiB), AMS=0 (round-robin), CSS=0 (NVM), SHN=0, EN=1.
    let cc = (6u32 << knvme::CC_IOSQES_SHIFT)
        | (4u32 << knvme::CC_IOCQES_SHIFT)
        | (0u32 << knvme::CC_MPS_SHIFT)
        | (0u32 << knvme::CC_AMS_SHIFT)
        | (0u32 << knvme::CC_CSS_SHIFT)
        | (0u32 << knvme::CC_SHN_SHIFT)
        | knvme::CC_EN;
    regs.write_reg::<u32>(knvme::NvmeRegs::CC, cc);

    wait_until(
        || {
            let csts = regs.read_reg::<u32>(knvme::NvmeRegs::CSTS);
            // CSTS.CFS means fatal status — stop waiting so the caller can
            // surface the error rather than hit the timeout.
            csts & (knvme::CSTS_RDY | knvme::CSTS_CFS) != 0
        },
        timeout_ticks,
    )
    .map_err(|_| "nvme enable timeout waiting for CSTS.RDY=1")?;

    let csts = regs.read_reg::<u32>(knvme::NvmeRegs::CSTS);
    if csts & knvme::CSTS_CFS != 0 {
        return Err("nvme controller reported fatal status during enable");
    }
    log::info!("[nvme] controller enabled (CSTS.RDY set)");
    Ok(())
}

/// Program `AQA` (queue sizes) and `ASQ` / `ACQ` (queue base addresses).
fn program_admin_queue_registers(regs: &MmioRegion, sq_phys: u64, cq_phys: u64, entries: u16) {
    // AQA.ASQS (bits 11:0) and .ACQS (bits 27:16) both encode `entries - 1`.
    let qsize = (entries.saturating_sub(1)) as u32;
    let aqa = (qsize & 0x0FFF) | ((qsize & 0x0FFF) << 16);
    regs.write_reg::<u32>(knvme::NvmeRegs::AQA, aqa);
    regs.write_reg::<u64>(knvme::NvmeRegs::ASQ, sq_phys);
    regs.write_reg::<u64>(knvme::NvmeRegs::ACQ, cq_phys);
}

// ===========================================================================
// Admin queue bring-up + Identify
// ===========================================================================

fn bring_up_admin_and_identify() -> Result<(), &'static str> {
    // Allocate SQ/CQ buffers, program AQA/ASQ/ACQ, install the admin queue
    // into the driver, enable the controller.
    let (sq_phys, cq_phys, entries, timeout_ticks) = {
        let mut drv = DRIVER.lock();
        let d = drv.as_mut().ok_or("driver gone during admin init")?;
        let entries = ADMIN_QUEUE_ENTRIES.min(d.max_queue_entries as usize).max(2) as u16;
        let sq = DmaBuffer::<knvme::NvmeCommand>::allocate_array(&d.pci, entries as usize)
            .map_err(|_| "admin SQ DMA alloc failed")?;
        let cq = DmaBuffer::<knvme::NvmeCompletion>::allocate_array(&d.pci, entries as usize)
            .map_err(|_| "admin CQ DMA alloc failed")?;
        let sq_phys = sq.bus_address();
        let cq_phys = cq.bus_address();
        program_admin_queue_registers(&d.regs, sq_phys, cq_phys, entries);
        let slots = alloc::vec![NvmeCompletionSlot::default(); entries as usize];
        d.admin = Some(AdminQueue {
            sq,
            cq,
            queue_entries: entries,
            sq_tail: 0,
            cq_head: 0,
            phase: true,
            slots,
            next_cid: 0,
        });
        log::info!(
            "[nvme] admin queue installed: {} entries, SQ phys {:#x}, CQ phys {:#x}",
            entries,
            sq_phys,
            cq_phys
        );
        (sq_phys, cq_phys, entries, d.reset_timeout_ticks)
    };
    let _ = (sq_phys, cq_phys, entries); // silence unused-var warning once all three have been logged.

    {
        let drv = DRIVER.lock();
        let d = drv.as_ref().ok_or("driver gone before enable")?;
        enable_controller(&d.regs, timeout_ticks)?;
    }

    // Identify Controller.
    let ident_controller_buf = {
        let drv = DRIVER.lock();
        let d = drv.as_ref().ok_or("driver gone before identify")?;
        DmaBuffer::<[u8]>::allocate(&d.pci, 4096)
            .map_err(|_| "identify controller DMA alloc failed")?
    };
    {
        let mut cmd = knvme::NvmeCommand::new(knvme::OP_IDENTIFY, 0);
        cmd.prp1 = ident_controller_buf.bus_address();
        cmd.cdw10 = knvme::IDENTIFY_CNS_CONTROLLER;
        let c = submit_admin_command(cmd)?;
        if c.status_code != 0 {
            log::error!(
                "[nvme] Identify Controller failed: status={:#x}",
                c.status_code
            );
            return Err("nvme identify controller failed");
        }
        let bytes: &[u8] = &ident_controller_buf;
        log_ident_controller_fields(bytes);
    }

    // Identify Active Namespace List (CNS=2). Pick the first non-zero NSID.
    let ident_nslist_buf = {
        let drv = DRIVER.lock();
        let d = drv.as_ref().ok_or("driver gone before nslist")?;
        DmaBuffer::<[u8]>::allocate(&d.pci, 4096)
            .map_err(|_| "identify ns-list DMA alloc failed")?
    };
    let selected_nsid = {
        let mut cmd = knvme::NvmeCommand::new(knvme::OP_IDENTIFY, 0);
        cmd.prp1 = ident_nslist_buf.bus_address();
        cmd.cdw10 = 0x02; // CNS: active namespace list
        cmd.nsid = 0;
        match submit_admin_command(cmd) {
            Ok(c) if c.status_code == 0 => {
                let bytes: &[u8] = &ident_nslist_buf;
                let mut found = 0u32;
                for chunk in bytes.chunks_exact(4).take(1024) {
                    let nsid = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    if nsid != 0 {
                        found = nsid;
                        break;
                    }
                }
                if found == 0 { 1 } else { found }
            }
            _ => {
                log::info!("[nvme] namespace list unavailable — defaulting to nsid=1");
                1
            }
        }
    };

    // Identify Namespace for the selected NSID.
    let ident_ns_buf = {
        let drv = DRIVER.lock();
        let d = drv.as_ref().ok_or("driver gone before identify namespace")?;
        DmaBuffer::<[u8]>::allocate(&d.pci, 4096)
            .map_err(|_| "identify namespace DMA alloc failed")?
    };
    let nsze;
    let sector_bytes;
    {
        let mut cmd = knvme::NvmeCommand::new(knvme::OP_IDENTIFY, 0);
        cmd.prp1 = ident_ns_buf.bus_address();
        cmd.nsid = selected_nsid;
        cmd.cdw10 = knvme::IDENTIFY_CNS_NAMESPACE;
        let c = submit_admin_command(cmd)?;
        if c.status_code != 0 {
            log::error!(
                "[nvme] Identify Namespace nsid={} failed: status={:#x}",
                selected_nsid,
                c.status_code
            );
            return Err("nvme identify namespace failed");
        }
        let bytes: &[u8] = &ident_ns_buf;
        nsze = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        let ncap = u64::from_le_bytes([
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ]);
        // LBAF0 is at offset 128. LBADS in bits 23:16 is log2 of sector
        // size.
        let lbaf0 = u32::from_le_bytes([bytes[128], bytes[129], bytes[130], bytes[131]]);
        let lbads = (lbaf0 >> 16) & 0xFF;
        sector_bytes = 1u32 << lbads;
        log::info!(
            "[nvme] nsid={}: nsze={} ncap={} sector={}B (LBADS={})",
            selected_nsid,
            nsze,
            ncap,
            sector_bytes,
            lbads
        );
    }

    {
        let mut drv = DRIVER.lock();
        let d = drv.as_mut().ok_or("driver gone after identify")?;
        d.namespace_id = selected_nsid;
        d.namespace_lbas = nsze;
        d.namespace_sector_bytes = sector_bytes;
    }
    Ok(())
}

/// Log the ASCII-ish fields of an Identify Controller data structure. We
/// trim trailing spaces/nuls for readability.
fn log_ident_controller_fields(bytes: &[u8]) {
    fn trim(s: &[u8]) -> &[u8] {
        let end = s
            .iter()
            .rposition(|&b| b != b' ' && b != 0)
            .map(|i| i + 1)
            .unwrap_or(0);
        &s[..end]
    }
    if bytes.len() < 72 {
        return;
    }
    // Layout: VID 0..2, SSVID 2..4, SN 4..24, MN 24..64, FR 64..72.
    let sn = trim(&bytes[4..24]);
    let mn = trim(&bytes[24..64]);
    let fr = trim(&bytes[64..72]);
    log::info!(
        "[nvme] model=\"{}\" serial=\"{}\" firmware=\"{}\"",
        core::str::from_utf8(mn).unwrap_or("<non-utf8>"),
        core::str::from_utf8(sn).unwrap_or("<non-utf8>"),
        core::str::from_utf8(fr).unwrap_or("<non-utf8>")
    );
}

/// Submit a single admin command and poll (bounded) for its completion.
///
/// The IRQ handler is not installed during admin bring-up — D.4 layers it
/// on — so submission and completion live in the same function.  The
/// polling side uses `wait_until` to keep the iteration capped even if the
/// timer tick has not yet started.
fn submit_admin_command(mut cmd: knvme::NvmeCommand) -> Result<NvmeCompletionSlot, &'static str> {
    // Phase 1: allocate a CID, write the SQ entry, kick the doorbell. All
    // under the driver lock with interrupts off so the IRQ handler (once
    // installed in D.4) cannot fire against partial state.
    let (doorbell_off, regs_virt, cid) = interrupts::without_interrupts(|| {
        let mut drv = DRIVER.lock();
        let d = drv.as_mut().ok_or("driver gone during admin submit")?;
        let stride = d.doorbell_stride_bytes;
        let regs_virt = d.regs.virt_base();
        let admin = d.admin.as_mut().ok_or("admin queue not initialized")?;
        let entries = admin.queue_entries;
        let cid = admin.next_cid % entries;
        // Place CID into CDW0 bits 31:16.
        cmd.cdw0 = (cmd.cdw0 & 0x0000_FFFF) | ((cid as u32) << 16);
        // Reset this slot so a previous stale completion cannot be picked
        // up by mistake.
        admin.slots[cid as usize] = NvmeCompletionSlot::default();
        admin.sq[admin.sq_tail as usize] = cmd;
        admin.sq_tail = (admin.sq_tail + 1) % entries;
        admin.next_cid = admin.next_cid.wrapping_add(1);
        // Publish the SQ entry before writing the doorbell (the device is
        // a separate bus master and must see the full command body).
        core::sync::atomic::fence(Ordering::Release);
        let doorbell_off = knvme::NvmeRegs::doorbell_offset(0, false, stride);
        Ok::<(usize, usize, u16), &'static str>((doorbell_off, regs_virt, cid))
    })?;

    // Write the new tail pointer to the submission-queue tail doorbell.
    // Read the tail value back under the lock so we do not publish a stale
    // pointer if two callers race (admin bring-up is single-threaded but
    // keep the code pattern consistent with the I/O path in D.3).
    let tail = interrupts::without_interrupts(|| {
        let drv = DRIVER.lock();
        drv.as_ref()
            .and_then(|d| d.admin.as_ref().map(|a| a.sq_tail))
    })
    .ok_or("admin queue gone before doorbell ring")?;
    // SAFETY: `regs_virt + doorbell_off` is inside BAR0 (verified in probe
    // that BAR0 >= 0x1000 bytes, and doorbells start at 0x1000 which is
    // within the page on every NVMe controller we target).
    unsafe {
        core::ptr::write_volatile((regs_virt + doorbell_off) as *mut u32, tail as u32);
    }

    // Phase 2: drain the CQ until our CID completes or the budget expires.
    let start = crate::arch::x86_64::interrupts::tick_count();
    loop {
        let filled = interrupts::without_interrupts(|| {
            let mut drv = DRIVER.lock();
            let d = drv.as_mut()?;
            let regs_virt = d.regs.virt_base();
            let stride = d.doorbell_stride_bytes;
            let admin = d.admin.as_mut()?;
            drain_admin_cq_locked(admin, regs_virt, stride);
            let slot = admin.slots[cid as usize];
            Some(slot)
        });
        if let Some(slot) = filled
            && slot.filled
        {
            return Ok(slot);
        }
        let now = crate::arch::x86_64::interrupts::tick_count();
        if now.wrapping_sub(start) >= ADMIN_COMMAND_TIMEOUT_TICKS {
            return Err("admin command timed out");
        }
        core::hint::spin_loop();
    }
}

/// Drain all new completion entries from the admin CQ. Called from the
/// polled path above; D.4 calls the same helper from the IRQ handler.
fn drain_admin_cq_locked(admin: &mut AdminQueue, regs_virt: usize, stride: usize) {
    loop {
        let entry = admin.cq[admin.cq_head as usize];
        let phase = knvme::completion_phase(&entry);
        if phase != admin.phase {
            break; // stale entry from previous pass
        }
        let cid = entry.cid;
        if (cid as usize) < admin.slots.len() {
            // Preserve the waker_task so the ISR can wake a parked
            // submitter — the admin path is polled during bring-up, so
            // this is a no-op in practice, but keeps the invariant
            // symmetric with the I/O drain.
            let waker = admin.slots[cid as usize].waker_task;
            admin.slots[cid as usize] = NvmeCompletionSlot {
                result: entry.result,
                status_code: knvme::completion_status_code(&entry),
                filled: true,
                waker_task: waker,
            };
        }
        admin.cq_head = (admin.cq_head + 1) % admin.queue_entries;
        if admin.cq_head == 0 {
            admin.phase = !admin.phase;
        }
        // Advance the CQ head doorbell so the device can reuse the slot.
        let doorbell = knvme::NvmeRegs::doorbell_offset(0, true, stride);
        // SAFETY: regs_virt is BAR0 base; the doorbell range is page 1.
        unsafe {
            core::ptr::write_volatile((regs_virt + doorbell) as *mut u32, admin.cq_head as u32);
        }
    }
}

// ===========================================================================
// I/O queue bring-up + read/write (D.3)
// ===========================================================================

/// Allocate the I/O SQ/CQ, issue Create I/O CQ (0x05) and Create I/O SQ
/// (0x01), and install the queue pair on the controller.
///
/// D.4 will replace the polled completion path with an IRQ-driven one; the
/// PRP + command-building code here stays unchanged.
fn bring_up_io_queue() -> Result<(), &'static str> {
    let (sq_phys, cq_phys, entries) = {
        let mut drv = DRIVER.lock();
        let d = drv.as_mut().ok_or("driver gone during I/O init")?;
        let entries = IO_QUEUE_ENTRIES.min(d.max_queue_entries as usize).max(2) as u16;
        let sq = DmaBuffer::<knvme::NvmeCommand>::allocate_array(&d.pci, entries as usize)
            .map_err(|_| "I/O SQ DMA alloc failed")?;
        let cq = DmaBuffer::<knvme::NvmeCompletion>::allocate_array(&d.pci, entries as usize)
            .map_err(|_| "I/O CQ DMA alloc failed")?;
        let prp_list = DmaBuffer::<u64>::allocate_array(&d.pci, 512)
            .map_err(|_| "I/O PRP list DMA alloc failed")?;
        let sq_phys = sq.bus_address();
        let cq_phys = cq.bus_address();
        let slots = alloc::vec![NvmeCompletionSlot::default(); entries as usize];
        d.io = Some(IoQueuePair {
            sq,
            cq,
            queue_entries: entries,
            sq_tail: 0,
            cq_head: 0,
            phase: true,
            slots,
            next_cid: 0,
            prp_list,
        });
        (sq_phys, cq_phys, entries)
    };

    // Create I/O Completion Queue (opcode 0x05).
    //   CDW10 = ((size-1) << 16) | qid
    //   CDW11 = (vector << 16) | IEN(bit1) | PC(bit0)
    // Vector 0 during D.3 — D.4 programs the actual MSI/MSI-X vector via
    // the HAL and the MSI-X table entry 0 matches the admin vector, which
    // QEMU accepts.
    {
        let mut cmd = knvme::NvmeCommand::new(knvme::OP_CREATE_IO_CQ, 0);
        cmd.prp1 = cq_phys;
        cmd.cdw10 = ((entries.saturating_sub(1) as u32) << 16) | (IO_QUEUE_ID as u32);
        cmd.cdw11 = 0b11; // IEN=1, PC=1, vector=0
        let c = submit_admin_command(cmd)?;
        if c.status_code != 0 {
            log::error!("[nvme] Create I/O CQ failed: status={:#x}", c.status_code);
            return Err("create I/O CQ failed");
        }
    }

    // Create I/O Submission Queue (opcode 0x01).
    //   CDW10 = ((size-1) << 16) | qid
    //   CDW11 = (CQ id << 16) | QPRIO(14:13) | PC(bit0)
    {
        let mut cmd = knvme::NvmeCommand::new(knvme::OP_CREATE_IO_SQ, 0);
        cmd.prp1 = sq_phys;
        cmd.cdw10 = ((entries.saturating_sub(1) as u32) << 16) | (IO_QUEUE_ID as u32);
        cmd.cdw11 = ((IO_QUEUE_ID as u32) << 16) | 1u32; // PC=1, QPRIO=00 (urgent/medium)
        let c = submit_admin_command(cmd)?;
        if c.status_code != 0 {
            log::error!("[nvme] Create I/O SQ failed: status={:#x}", c.status_code);
            return Err("create I/O SQ failed");
        }
    }

    log::info!(
        "[nvme] I/O queue pair ready: qid={} entries={}",
        IO_QUEUE_ID,
        entries
    );

    // D.4 — install the completion IRQ. Best-effort: a failure here leaves
    // `irq = None` and the submit path falls back to polled completions.
    install_completion_irq();
    Ok(())
}

/// Try to install an MSI / MSI-X handler for the completion queues; fall
/// back to legacy INTx; log and leave `irq = None` if nothing is available.
fn install_completion_irq() {
    let mut drv = DRIVER.lock();
    let Some(d) = drv.as_mut() else {
        return;
    };
    // Prefer MSI-X (which the HAL's install_msi_irq routes through MSI or
    // MSI-X depending on what the device advertises).
    match d.pci.install_msi_irq(nvme_completion_handler) {
        Ok(irq) => {
            log::info!(
                "[nvme] MSI/MSI-X IRQ installed on vector {:#x}",
                irq.vector()
            );
            d.irq = Some(irq);
            return;
        }
        Err(_) => {
            log::info!("[nvme] no MSI/MSI-X capability — trying legacy INTx");
        }
    }
    // Legacy INTx fallback: pick a dedicated vector from the device-IRQ
    // bank. virtio-blk uses BASE+2 and virtio-net uses BASE+0 (C.5), so we
    // reserve BASE+4 for NVMe to avoid collisions.
    const NVME_INTX_VECTOR: u8 = crate::arch::x86_64::interrupts::DEVICE_IRQ_VECTOR_BASE + 4;
    match d
        .pci
        .install_intx_irq(NVME_INTX_VECTOR, nvme_completion_handler)
    {
        Ok(irq) => {
            let dev = *d.pci.device();
            if dev.interrupt_line != 0xFF && crate::acpi::io_apic_address().is_some() {
                crate::arch::x86_64::apic::route_pci_irq(dev.interrupt_line, NVME_INTX_VECTOR);
                log::info!(
                    "[nvme] legacy INTx line {} routed to vector {:#x}",
                    dev.interrupt_line,
                    NVME_INTX_VECTOR
                );
            } else {
                log::warn!(
                    "[nvme] legacy INTx registered but no I/O APIC — polled fallback will fire"
                );
            }
            d.irq = Some(irq);
        }
        Err(_) => {
            log::warn!("[nvme] no IRQ available — requests will complete via the polled fallback");
        }
    }
}

/// NVMe completion IRQ handler. Runs in ISR context.
///
/// Contract (per AGENTS.md interrupt-handler rules):
///   * no allocation, no blocking, no IPC;
///   * drain every pending entry in both the admin CQ and the I/O CQ
///     (phase-bit walk);
///   * wake any registered waiter task via `wake_task`.
///
/// EOI is sent by the device-IRQ dispatch stub after we return.
fn nvme_completion_handler() {
    let mut drv = DRIVER.lock();
    let Some(d) = drv.as_mut() else {
        return;
    };
    let regs_virt = d.regs.virt_base();
    let stride = d.doorbell_stride_bytes;
    if let Some(admin) = d.admin.as_mut() {
        drain_admin_cq_locked(admin, regs_virt, stride);
    }
    if let Some(io) = d.io.as_mut() {
        drain_io_cq_locked(io, regs_virt, stride);
    }
}

/// Read `count` logical blocks starting at `start_lba` into `buf`. `buf`
/// must be at least `count * sector_bytes` bytes. Returns `Err` with the
/// raw NVMe status code on device error; `0xFFFF` signals a driver-level
/// failure (no NVMe, buffer too small, timeout).
#[allow(dead_code)]
pub fn read_sectors(start_lba: u64, count: usize, buf: &mut [u8]) -> Result<(), u16> {
    if !NVME_READY.load(Ordering::Acquire) {
        return Err(0xFFFF);
    }
    let sector_bytes = namespace_sector_bytes_nonzero()?;
    let needed = count.checked_mul(sector_bytes).ok_or(0xFFFEu16)?;
    if buf.len() < needed {
        log::error!(
            "[nvme] read_sectors: buffer too small ({} < {})",
            buf.len(),
            needed
        );
        return Err(0xFFFE);
    }
    // Build a DMA staging buffer large enough for the whole transfer; use
    // either the persistent single-page data buffer (fast path) or a fresh
    // allocation when the request spans more than one page.
    do_io_to_user_buffer(knvme::OP_IO_READ, start_lba, count, sector_bytes, buf)
}

/// Write `count` logical blocks starting at `start_lba` from `buf` to the
/// default namespace.
#[allow(dead_code)]
pub fn write_sectors(start_lba: u64, count: usize, buf: &[u8]) -> Result<(), u16> {
    if !NVME_READY.load(Ordering::Acquire) {
        return Err(0xFFFF);
    }
    let sector_bytes = namespace_sector_bytes_nonzero()?;
    let needed = count.checked_mul(sector_bytes).ok_or(0xFFFEu16)?;
    if buf.len() < needed {
        log::error!(
            "[nvme] write_sectors: buffer too small ({} < {})",
            buf.len(),
            needed
        );
        return Err(0xFFFE);
    }
    // Stage into a DMA buffer, then issue the write.
    let dma = {
        let drv = DRIVER.lock();
        let d = drv.as_ref().ok_or(0xFFFFu16)?;
        DmaBuffer::<[u8]>::allocate(&d.pci, needed.max(NVME_PAGE_BYTES)).map_err(|_| 0xFFFFu16)?
    };
    // SAFETY: dma is a fresh buffer we own; source is a caller-provided
    // slice of `needed` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), dma.as_ptr() as *mut u8, needed);
    }
    do_io_with_dma(knvme::OP_IO_WRITE, start_lba, count as u32, &dma, needed)
}

fn namespace_sector_bytes_nonzero() -> Result<usize, u16> {
    let bytes = namespace_sector_bytes();
    if bytes == 0 {
        Err(0xFFFF)
    } else {
        Ok(bytes as usize)
    }
}

/// Issue a Read and copy the DMA page's contents into `buf`.
fn do_io_to_user_buffer(
    opcode: u8,
    start_lba: u64,
    count: usize,
    sector_bytes: usize,
    buf: &mut [u8],
) -> Result<(), u16> {
    let needed = count * sector_bytes;
    let dma = {
        let drv = DRIVER.lock();
        let d = drv.as_ref().ok_or(0xFFFFu16)?;
        DmaBuffer::<[u8]>::allocate(&d.pci, needed.max(NVME_PAGE_BYTES)).map_err(|_| 0xFFFFu16)?
    };
    do_io_with_dma(opcode, start_lba, count as u32, &dma, needed)?;
    // SAFETY: dma is driver-owned and lives until this function returns.
    unsafe {
        core::ptr::copy_nonoverlapping(dma.as_ptr(), buf.as_mut_ptr(), needed);
    }
    Ok(())
}

/// Submit an I/O Read/Write covering the full `dma` buffer and wait for the
/// completion. Handles PRP list construction for requests spanning more
/// than two pages.
fn do_io_with_dma(
    opcode: u8,
    start_lba: u64,
    lba_count: u32,
    dma: &DmaBuffer<[u8]>,
    byte_len: usize,
) -> Result<(), u16> {
    // Reset the wake flag before publishing the request. Subsequent wake
    // from the ISR will set it; the order of "reset -> submit -> block" is
    // important: if the ISR fires between submit and block, `woken` is
    // already true and `block_current_unless_woken` returns immediately.
    IO_REQ_WOKEN.store(false, Ordering::Release);
    // `current_task_id` dereferences per-core data via `gs_base`; calling
    // it before `smp::init_bsp_per_core` panics. During driver bring-up
    // (the data-path smoke in `nvme_probe`) SMP is not yet initialized, so
    // we fall back to the polled completion path by reporting "no waiter".
    let waker = if crate::smp::is_per_core_ready() {
        current_task_id().unwrap_or(TaskId(0))
    } else {
        TaskId(0)
    };

    // Phase 1: allocate a CID, copy the command into the ring, advance
    // sq_tail under the driver lock with interrupts off. The ISR also
    // takes DRIVER.lock() so IF-off is required to avoid the ISR spinning
    // on our held `spin::Mutex`. Same rule as virtio-blk (documented in
    // its Fix 1 comment).
    let (doorbell_off, regs_virt, cid, irq_installed) = interrupts::without_interrupts(|| {
        let mut drv = DRIVER.lock();
        let d = drv.as_mut().ok_or(0xFFFFu16)?;
        let nsid = d.namespace_id;
        let capacity = d.namespace_lbas;
        if capacity == 0
            || start_lba >= capacity
            || start_lba.saturating_add(lba_count as u64) > capacity
        {
            log::error!(
                "[nvme] I/O LBA {}+{} out of bounds (capacity {})",
                start_lba,
                lba_count,
                capacity
            );
            return Err(0xFFFFu16);
        }
        let stride = d.doorbell_stride_bytes;
        let regs_virt = d.regs.virt_base();
        let irq_installed = d.irq.is_some();
        let dma_phys = dma.bus_address();
        let (prp1, prp2) =
            build_prp_pair(d.io.as_mut().ok_or(0xFFFFu16)?, dma_phys, byte_len).ok_or(0xFFFFu16)?;
        let io = d.io.as_mut().ok_or(0xFFFFu16)?;
        let entries = io.queue_entries;
        let cid = io.next_cid % entries;
        let mut cmd = knvme::NvmeCommand::new(opcode, cid);
        cmd.nsid = nsid;
        cmd.prp1 = prp1;
        cmd.prp2 = prp2;
        cmd.cdw10 = (start_lba & 0xFFFF_FFFF) as u32;
        cmd.cdw11 = (start_lba >> 32) as u32;
        cmd.cdw12 = lba_count.saturating_sub(1) & 0xFFFF;
        io.slots[cid as usize] = NvmeCompletionSlot {
            waker_task: waker,
            ..NvmeCompletionSlot::default()
        };
        io.sq[io.sq_tail as usize] = cmd;
        io.sq_tail = (io.sq_tail + 1) % entries;
        io.next_cid = io.next_cid.wrapping_add(1);
        core::sync::atomic::fence(Ordering::Release);
        let doorbell = knvme::NvmeRegs::doorbell_offset(IO_QUEUE_ID, false, stride);
        Ok::<(usize, usize, u16, bool), u16>((doorbell, regs_virt, cid, irq_installed))
    })?;

    let tail = interrupts::without_interrupts(|| {
        let drv = DRIVER.lock();
        drv.as_ref()
            .and_then(|d| d.io.as_ref().map(|io| io.sq_tail))
    })
    .ok_or(0xFFFFu16)?;
    // SAFETY: `regs_virt + doorbell_off` is within BAR0 (verified at probe).
    unsafe {
        core::ptr::write_volatile((regs_virt + doorbell_off) as *mut u32, tail as u32);
    }

    // Phase 2: wait for the completion. IRQ-driven path parks the task;
    // polled fallback walks the CQ under a bounded budget.
    if irq_installed && waker.0 != 0 {
        // Park the task until the ISR wakes us. `block_current_unless_woken`
        // re-checks `IO_REQ_WOKEN` under the scheduler lock so a wake that
        // races the block cannot be lost.
        block_current_unless_woken(&IO_REQ_WOKEN);
        // After waking, drain once under the lock in case more than one
        // completion arrived between the IRQ and our wake-up (rare with a
        // single request in flight, but cheap to guard).
        interrupts::without_interrupts(|| {
            if let Some(d) = DRIVER.lock().as_mut() {
                let regs_virt = d.regs.virt_base();
                let stride = d.doorbell_stride_bytes;
                if let Some(io) = d.io.as_mut() {
                    drain_io_cq_locked(io, regs_virt, stride);
                }
            }
        });
    } else {
        // Polled fallback: drain periodically until the slot fills or the
        // bounded budget expires.
        let start = crate::arch::x86_64::interrupts::tick_count();
        loop {
            let slot = interrupts::without_interrupts(|| {
                let mut drv = DRIVER.lock();
                let d = drv.as_mut()?;
                let regs_virt = d.regs.virt_base();
                let stride = d.doorbell_stride_bytes;
                let io = d.io.as_mut()?;
                drain_io_cq_locked(io, regs_virt, stride);
                Some(io.slots[cid as usize])
            });
            if let Some(slot) = slot
                && slot.filled
            {
                return if slot.status_code == 0 {
                    Ok(())
                } else {
                    Err(slot.status_code)
                };
            }
            let now = crate::arch::x86_64::interrupts::tick_count();
            if now.wrapping_sub(start) >= ADMIN_COMMAND_TIMEOUT_TICKS {
                log::error!("[nvme] I/O command timed out (polled)");
                return Err(0xFFFF);
            }
            core::hint::spin_loop();
        }
    }

    // Phase 3: read the completion status out of the slot. Re-acquire the
    // lock with IF off so the ISR cannot race us on the same slot.
    let slot = interrupts::without_interrupts(|| {
        DRIVER
            .lock()
            .as_ref()
            .and_then(|d| d.io.as_ref().map(|io| io.slots[cid as usize]))
    })
    .ok_or(0xFFFFu16)?;
    if !slot.filled {
        // ISR fired but the slot was overwritten or we looked before the
        // completion drained. Fall through to a short polled wait.
        let start = crate::arch::x86_64::interrupts::tick_count();
        loop {
            let (filled, sc) = interrupts::without_interrupts(|| {
                let mut drv = DRIVER.lock();
                let Some(d) = drv.as_mut() else {
                    return (false, 0xFFFFu16);
                };
                let regs_virt = d.regs.virt_base();
                let stride = d.doorbell_stride_bytes;
                let Some(io) = d.io.as_mut() else {
                    return (false, 0xFFFFu16);
                };
                drain_io_cq_locked(io, regs_virt, stride);
                let s = io.slots[cid as usize];
                (s.filled, s.status_code)
            });
            if filled {
                return if sc == 0 { Ok(()) } else { Err(sc) };
            }
            let now = crate::arch::x86_64::interrupts::tick_count();
            if now.wrapping_sub(start) >= ADMIN_COMMAND_TIMEOUT_TICKS {
                log::error!("[nvme] I/O completion slot never filled");
                return Err(0xFFFF);
            }
            core::hint::spin_loop();
        }
    }
    if slot.status_code == 0 {
        Ok(())
    } else {
        Err(slot.status_code)
    }
}

/// Build PRP1 and PRP2 for a DMA buffer covering `byte_len` bytes.
///
/// Layout rules (NVMe spec §4.3):
///   * Transfer <= 1 page: PRP1 = buffer PA, PRP2 unused.
///   * Transfer <= 2 pages: PRP1 = buffer PA, PRP2 = buffer_pa + PAGE_BYTES.
///   * Transfer > 2 pages: PRP1 = buffer PA, PRP2 = PRP-list PA; the PRP
///     list is an array of u64 PAs, one per subsequent page.
fn build_prp_pair(io: &mut IoQueuePair, buffer_pa: u64, byte_len: usize) -> Option<(u64, u64)> {
    let page = NVME_PAGE_BYTES as u64;
    if byte_len == 0 {
        return None;
    }
    let pages = byte_len.div_ceil(NVME_PAGE_BYTES);
    if pages <= 1 {
        return Some((buffer_pa, 0));
    }
    if pages == 2 {
        return Some((buffer_pa, buffer_pa + page));
    }
    // > 2 pages — fill the PRP list page with PAs for pages 2..N.
    let remaining_pages = pages - 1;
    if remaining_pages > io.prp_list.len() {
        log::error!(
            "[nvme] PRP list too small: need {} entries, have {}",
            remaining_pages,
            io.prp_list.len()
        );
        return None;
    }
    for i in 0..remaining_pages {
        io.prp_list[i] = buffer_pa + ((i as u64) + 1) * page;
    }
    Some((buffer_pa, io.prp_list.bus_address()))
}

/// Drain all new completions from the I/O CQ. Same pattern as
/// [`drain_admin_cq_locked`], called from both the polled path above (D.3)
/// and the IRQ handler (D.4).
fn drain_io_cq_locked(io: &mut IoQueuePair, regs_virt: usize, stride: usize) {
    loop {
        let entry = io.cq[io.cq_head as usize];
        let phase = knvme::completion_phase(&entry);
        if phase != io.phase {
            break;
        }
        let cid = entry.cid;
        if (cid as usize) < io.slots.len() {
            let waker = io.slots[cid as usize].waker_task;
            io.slots[cid as usize] = NvmeCompletionSlot {
                result: entry.result,
                status_code: knvme::completion_status_code(&entry),
                filled: true,
                waker_task: waker,
            };
            // Publish + wake the blocked task (D.4). The flag is consumed
            // by `block_current_unless_woken`; the wake_task call delivers
            // the reschedule IPI so the blocked task re-enters the
            // scheduler and observes the filled slot.
            IO_REQ_WOKEN.store(true, Ordering::Release);
            if waker.0 != 0 {
                wake_task(waker);
            }
        }
        io.cq_head = (io.cq_head + 1) % io.queue_entries;
        if io.cq_head == 0 {
            io.phase = !io.phase;
        }
        let doorbell = knvme::NvmeRegs::doorbell_offset(IO_QUEUE_ID, true, stride);
        // SAFETY: regs_virt is BAR0 base; doorbells live on page 1.
        unsafe {
            core::ptr::write_volatile((regs_virt + doorbell) as *mut u32, io.cq_head as u32);
        }
    }
}

// ===========================================================================
// Public entry points
// ===========================================================================

/// Register the NVMe driver and run a probe pass. Kept for backwards
/// compatibility; the normal boot flow uses [`super::init`].
#[allow(dead_code)]
pub fn init() {
    register();
    pci::probe_all_drivers();
}

/// Controller version (raw `VS` register).
#[allow(dead_code)]
pub fn controller_version() -> u32 {
    let drv = DRIVER.lock();
    drv.as_ref().map(|d| d.version).unwrap_or(0)
}

/// Capacity of the active namespace in LBAs.
#[allow(dead_code)]
pub fn namespace_capacity_lbas() -> u64 {
    let drv = DRIVER.lock();
    drv.as_ref().map(|d| d.namespace_lbas).unwrap_or(0)
}

/// Sector size (LBA format) of the active namespace in bytes.
#[allow(dead_code)]
pub fn namespace_sector_bytes() -> u32 {
    let drv = DRIVER.lock();
    drv.as_ref().map(|d| d.namespace_sector_bytes).unwrap_or(0)
}
