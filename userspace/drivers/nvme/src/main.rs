//! Ring-3 NVMe driver — Phase 55b Tracks D.1 (scaffold) and D.2 (bring-up).
//!
//! Phase 55b moves the NVMe driver from ring 0 (`kernel/src/blk/nvme.rs`)
//! into this userspace crate. Track D.1 landed the crate shell and the
//! four-place userspace-binary wiring (workspace member, xtask bins,
//! ramdisk embedding, future service config). Track D.2 — this commit —
//! ports the controller bring-up path to [`driver_runtime`]:
//! register programming runs through [`driver_runtime::Mmio`], DMA
//! allocations run through [`driver_runtime::DmaBuffer`], and the
//! reset / enable / Identify sequence drives
//! [`init::BringUpStateMachine`] so every transition is host-testable.
//!
//! Track D.3 wires the I/O queue pair, MSI-X, and the block IPC path.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`init`] | Pure bring-up state machine + CC / AQA encoders (host-testable) |
//!
//! # Exit behavior
//!
//! D.2's `program_main` drives controller bring-up to completion and
//! then returns zero. Returning rather than blocking forever is
//! deliberate: Track D.3 lands the `BlockServer::handle_next` loop, so
//! D.2's clean-exit path lets F.2's crash-and-restart regression land
//! on a predictable exit code first.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod init;
pub mod io;

#[cfg(not(test))]
use core::alloc::Layout;

#[cfg(not(test))]
use driver_runtime::{DeviceCapKey, DeviceHandle, DmaBuffer, DriverRuntimeError, Mmio};
#[cfg(not(test))]
use kernel_core::driver_ipc::block::BlockDriverError;
#[cfg(not(test))]
use kernel_core::nvme as knvme;
#[cfg(not(test))]
use syscall_lib::STDOUT_FILENO;
#[cfg(not(test))]
use syscall_lib::heap::BrkAllocator;

#[cfg(not(test))]
use crate::init::{
    ADMIN_QUEUE_DEPTH, BringUpAction, BringUpError, BringUpState, BringUpStateMachine,
    NVME_PAGE_BYTES, encode_aqa, encode_cc_enable,
};

#[cfg(not(test))]
#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// Sentinel PCI BDF the driver claims at bring-up. The real D.3
/// discovery walks PCI for class `0x01` subclass `0x08` programming
/// interface `0x02`; until that lands, D.2 targets QEMU's default
/// `-device nvme` location (`0000:00:04.0`) so a missing device is
/// observable in the boot log rather than silently skipped.
#[cfg(not(test))]
const SENTINEL_BDF: DeviceCapKey = DeviceCapKey::new(0, 0x00, 0x04, 0);

/// BAR0 length the driver asks the kernel to map. NVMe controllers
/// keep admin registers at offset 0 and per-queue doorbells at
/// `DOORBELL_BASE (0x1000) + queue_id * stride`. 64 KiB is the upper
/// bound `driver_runtime::Mmio::map` expects and fits every QEMU NVMe
/// configuration.
#[cfg(not(test))]
const BAR0_EXPECTED_BYTES: usize = 0x1_0000;

/// Bound on the outer bring-up dispatch loop. Each state either
/// advances on a single action or parks in a wait state with its own
/// bounded polling. 32 iterations covers the full state graph several
/// times over.
#[cfg(not(test))]
const OUTER_DISPATCH_BUDGET: u32 = 32;

/// Hard upper bound on the CSTS / completion polling spin. The ring-3
/// driver has no timer subsystem of its own, so bounds are in
/// iterations — 8 M spins is empirically multiple seconds on every
/// target we care about and keeps a wedged controller from stalling
/// the driver past the service-manager restart window.
#[cfg(not(test))]
const MMIO_SPIN_BUDGET: u64 = 8_000_000;

// ---------------------------------------------------------------------------
// Typestate marker for the NVMe BAR0 window.
// ---------------------------------------------------------------------------

/// Phantom marker so `Mmio<NvmeRegsTag>` cannot be confused with
/// another driver's BAR at compile time.
#[cfg(not(test))]
pub struct NvmeRegsTag;

// ---------------------------------------------------------------------------
// Error surface.
// ---------------------------------------------------------------------------

/// Every fallible step inside `program_main` returns one of these. Kept
/// internal to the binary so D.3 can extend it without reshaping the
/// public `driver_runtime` error surface.
///
/// `allow(dead_code)` on the data fields: Phase 55b's serial-output
/// facade (`write_str`) takes a `&str` and cannot format enums without
/// pulling in `core::fmt` machinery, so the variant data is recorded
/// for pattern matching today but only surfaces in logs once D.3
/// wires up a typed formatter.
#[cfg(not(test))]
#[derive(Debug)]
enum InitError {
    Runtime(#[allow(dead_code)] DriverRuntimeError),
    BringUp(#[allow(dead_code)] BringUpError),
}

#[cfg(not(test))]
impl From<DriverRuntimeError> for InitError {
    fn from(e: DriverRuntimeError) -> Self {
        InitError::Runtime(e)
    }
}

#[cfg(not(test))]
impl From<BringUpError> for InitError {
    fn from(e: BringUpError) -> Self {
        InitError::BringUp(e)
    }
}

#[cfg(not(test))]
impl From<InitError> for BlockDriverError {
    /// Collapse any bring-up error to a single `IoError` so IPC clients
    /// see a uniform failure surface per Phase 55b D.2 acceptance bullet
    /// 5 ("failure modes return `BlockDriverError::IoError` to clients
    /// via the IPC protocol rather than panicking"). The specific
    /// variant is logged separately so post-mortem has full detail
    /// without exposing driver internals over the wire.
    fn from(_: InitError) -> Self {
        BlockDriverError::IoError
    }
}

// ---------------------------------------------------------------------------
// program_main
// ---------------------------------------------------------------------------

/// Claim the sentinel NVMe BDF, map BAR0, run controller bring-up, and
/// return. Track D.3 extends this to enter a `BlockServer` loop.
#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: spawned\n");

    match bring_up_controller() {
        Ok(()) => {
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: bring-up complete\n");
            0
        }
        Err(e) => {
            // Log the specific variant so the reader can correlate.
            let _collapsed: BlockDriverError = e.into();
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: bring-up failed\n");
            // Exit non-zero so the service manager's restart path
            // (Phase 46 / 51) observes the failure.
            1
        }
    }
}

#[cfg(not(test))]
fn bring_up_controller() -> Result<(), InitError> {
    // Step 1: claim the device capability.
    let device = DeviceHandle::claim(SENTINEL_BDF)?;
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: claimed BDF\n");

    // Step 2: map BAR0 as an uncacheable MMIO window.
    let mmio: Mmio<NvmeRegsTag> = Mmio::map(&device, 0, BAR0_EXPECTED_BYTES)?;
    if mmio.len() < knvme::NvmeRegs::DOORBELL_BASE {
        // Audited: returning a typed error rather than panicking keeps
        // Phase 55b's "no panic in non-test code" discipline intact.
        return Err(InitError::BringUp(BringUpError::BarTooSmall));
    }
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: BAR0 mapped\n");

    // Step 3: read CAP, build the state machine.
    let cap_raw = mmio.read_reg::<u64>(knvme::NvmeRegs::CAP);
    let cap = knvme::NvmeCap(cap_raw);
    let mut sm = BringUpStateMachine::new(cap)?;

    // Step 4: walk the state machine to completion. Admin SQ / CQ and
    // Identify buffers live across several iterations; allocate lazily
    // when the matching action fires.
    let mut admin: Option<AdminQueue> = None;
    let mut ident_controller: Option<DmaBuffer<[u8; NVME_PAGE_BYTES]>> = None;
    let mut ident_namespace: Option<DmaBuffer<[u8; NVME_PAGE_BYTES]>> = None;

    let mut safety: u32 = OUTER_DISPATCH_BUDGET;
    while !sm.is_terminal() && safety > 0 {
        safety -= 1;
        match sm.next_action() {
            BringUpAction::WriteCcDisable => {
                let cc = mmio.read_reg::<u32>(knvme::NvmeRegs::CC);
                if cc & knvme::CC_EN != 0 {
                    mmio.write_reg::<u32>(knvme::NvmeRegs::CC, cc & !knvme::CC_EN);
                }
                sm.notify_cc_disabled();
            }
            BringUpAction::AwaitCstsReset | BringUpAction::AwaitCstsReady => {
                poll_csts(&mmio, &mut sm);
            }
            BringUpAction::ProgramAdminRegisters => {
                let entries = clamp_admin_entries(sm.max_queue_entries());
                let q = AdminQueue::allocate(&device, entries)?;
                mmio.write_reg::<u32>(knvme::NvmeRegs::AQA, encode_aqa(entries));
                mmio.write_reg::<u64>(knvme::NvmeRegs::ASQ, q.sq.iova());
                mmio.write_reg::<u64>(knvme::NvmeRegs::ACQ, q.cq.iova());
                admin = Some(q);
                sm.notify_admin_programmed();
                syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: admin queue programmed\n");
            }
            BringUpAction::WriteCcEnable => {
                mmio.write_reg::<u32>(knvme::NvmeRegs::CC, encode_cc_enable());
                sm.notify_cc_enabled();
            }
            BringUpAction::SubmitIdentifyController => {
                let Some(admin_q) = admin.as_mut() else {
                    // State machine should never reach this action
                    // without admin in place; treat as admin failure.
                    return Err(InitError::BringUp(BringUpError::AdminCommandFailed));
                };
                let buf: DmaBuffer<[u8; NVME_PAGE_BYTES]> =
                    DmaBuffer::allocate(&device, NVME_PAGE_BYTES, NVME_PAGE_BYTES)?;
                let status = submit_identify(
                    &mmio,
                    admin_q,
                    &buf,
                    knvme::IDENTIFY_CNS_CONTROLLER,
                    0,
                    sm.doorbell_stride_bytes(),
                );
                log_identify_controller(&buf);
                ident_controller = Some(buf);
                sm.notify_identify_controller(status);
            }
            BringUpAction::SubmitIdentifyNamespace => {
                let Some(admin_q) = admin.as_mut() else {
                    return Err(InitError::BringUp(BringUpError::AdminCommandFailed));
                };
                let buf: DmaBuffer<[u8; NVME_PAGE_BYTES]> =
                    DmaBuffer::allocate(&device, NVME_PAGE_BYTES, NVME_PAGE_BYTES)?;
                let status = submit_identify(
                    &mmio,
                    admin_q,
                    &buf,
                    knvme::IDENTIFY_CNS_NAMESPACE,
                    1, // nsid 1 — D.3 walks the active namespace list
                    sm.doorbell_stride_bytes(),
                );
                log_identify_namespace(&buf);
                ident_namespace = Some(buf);
                sm.notify_identify_namespace(status);
            }
            BringUpAction::Idle => break,
        }
    }

    // Keep the identify buffers alive for the full bring-up — D.3
    // extends this to record the model / serial / capacity into a
    // persistent controller struct.
    let _ = (ident_controller, ident_namespace);

    if let Some(err) = sm.error() {
        return Err(InitError::BringUp(err));
    }
    if !sm.is_complete() {
        // Safety budget exhausted without reaching a terminal state —
        // treat as an admin-command failure so the IPC surface
        // collapses to IoError.
        return Err(InitError::BringUp(BringUpError::AdminCommandFailed));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AdminQueue — ring-3 port of Phase 55 D.1's AdminQueue.
// ---------------------------------------------------------------------------

/// Admin submission + completion queue pair.
///
/// DMA buffers use the const-generic array types (`Option B` in the
/// Track D.2 task guidance): `[NvmeCommand; ADMIN_QUEUE_DEPTH]` and
/// `[NvmeCompletion; ADMIN_QUEUE_DEPTH]` give the allocator a `Sized`
/// type while keeping the layout the NVMe spec mandates.
#[cfg(not(test))]
struct AdminQueue {
    sq: DmaBuffer<[knvme::NvmeCommand; ADMIN_QUEUE_DEPTH]>,
    cq: DmaBuffer<[knvme::NvmeCompletion; ADMIN_QUEUE_DEPTH]>,
    entries: u16,
    sq_tail: u16,
    cq_head: u16,
    /// Phase tag for the next expected completion. Toggles each lap
    /// of the ring.
    phase: bool,
    next_cid: u16,
}

#[cfg(not(test))]
impl AdminQueue {
    fn allocate(device: &DeviceHandle, entries: u16) -> Result<Self, DriverRuntimeError> {
        // SQ: 64 * 64 = 4 096 bytes (exactly one page). Page-alignment
        // matches NVMe §3.1.10 / §3.1.11.
        let sq_bytes = core::mem::size_of::<[knvme::NvmeCommand; ADMIN_QUEUE_DEPTH]>();
        let sq: DmaBuffer<[knvme::NvmeCommand; ADMIN_QUEUE_DEPTH]> =
            DmaBuffer::allocate(device, sq_bytes, NVME_PAGE_BYTES)?;
        // CQ: 64 * 16 = 1 024 bytes; pad up to a full page so the
        // allocation satisfies the NVMe page-alignment rule.
        let cq: DmaBuffer<[knvme::NvmeCompletion; ADMIN_QUEUE_DEPTH]> =
            DmaBuffer::allocate(device, NVME_PAGE_BYTES, NVME_PAGE_BYTES)?;
        Ok(Self {
            sq,
            cq,
            entries,
            sq_tail: 0,
            cq_head: 0,
            phase: true,
            next_cid: 0,
        })
    }

    /// Raw pointer to the SQ ring element at index `i`.
    fn sq_entry_ptr(&self, i: usize) -> *mut knvme::NvmeCommand {
        let base = self.sq.user_ptr() as *mut knvme::NvmeCommand;
        // SAFETY: `i < self.entries <= ADMIN_QUEUE_DEPTH` at every
        // call site, so the offset is within the allocation.
        unsafe { base.add(i) }
    }

    /// Raw pointer to the CQ ring element at index `i`.
    fn cq_entry_ptr(&self, i: usize) -> *const knvme::NvmeCompletion {
        let base = self.cq.user_ptr() as *const knvme::NvmeCompletion;
        // SAFETY: bounds mirror `sq_entry_ptr`.
        unsafe { base.add(i) }
    }
}

// ---------------------------------------------------------------------------
// Helpers — polled CSTS read + Identify submission.
// ---------------------------------------------------------------------------

/// Poll `CSTS` until the state machine leaves the current wait state
/// or the spin budget expires.
#[cfg(not(test))]
fn poll_csts(mmio: &Mmio<NvmeRegsTag>, sm: &mut BringUpStateMachine) {
    let mut i: u64 = 0;
    while i < MMIO_SPIN_BUDGET {
        let csts = mmio.read_reg::<u32>(knvme::NvmeRegs::CSTS);
        sm.observe_csts(csts);
        if sm.is_terminal() {
            return;
        }
        match sm.state() {
            BringUpState::ResetWait | BringUpState::EnableWait => {}
            _ => return,
        }
        core::hint::spin_loop();
        i += 1;
    }
    sm.timeout();
}

/// Clamp the admin queue depth against `CAP.MQES`. Mirrors Phase 55
/// D.1's `ADMIN_QUEUE_ENTRIES.min(max).max(2)` pattern.
#[cfg(not(test))]
fn clamp_admin_entries(mqes: u16) -> u16 {
    let desired = ADMIN_QUEUE_DEPTH as u16;
    core::cmp::min(desired, mqes).max(2)
}

/// Build an Identify command with `cns` / `nsid`, push it onto the
/// admin SQ, ring the doorbell, and poll the CQ for the matching CID.
/// Returns the 15-bit completion status code, or `0x7FFF` if the poll
/// budget expired (the state machine treats any non-zero status as an
/// admin failure).
#[cfg(not(test))]
fn submit_identify(
    mmio: &Mmio<NvmeRegsTag>,
    admin: &mut AdminQueue,
    buf: &DmaBuffer<[u8; NVME_PAGE_BYTES]>,
    cns: u32,
    nsid: u32,
    doorbell_stride: usize,
) -> u16 {
    let cid = admin.next_cid % admin.entries;
    let mut cmd = knvme::NvmeCommand::new(knvme::OP_IDENTIFY, cid);
    cmd.nsid = nsid;
    cmd.prp1 = buf.iova();
    cmd.cdw10 = cns;

    // SAFETY: `sq_entry_ptr` returns a pointer into the DMA region
    // owned by `admin.sq`. No concurrent writer — admin bring-up is
    // strictly sequential inside `bring_up_controller`.
    unsafe {
        core::ptr::write_volatile(admin.sq_entry_ptr(admin.sq_tail as usize), cmd);
    }
    admin.sq_tail = (admin.sq_tail + 1) % admin.entries;
    admin.next_cid = admin.next_cid.wrapping_add(1);
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

    let sq_doorbell = knvme::NvmeRegs::doorbell_offset(0, false, doorbell_stride);
    mmio.write_reg::<u32>(sq_doorbell, admin.sq_tail as u32);

    let mut i: u64 = 0;
    loop {
        // SAFETY: `cq_entry_ptr` is within the DMA allocation; the
        // device writes the entry before setting the phase bit.
        let entry = unsafe { core::ptr::read_volatile(admin.cq_entry_ptr(admin.cq_head as usize)) };
        let phase = knvme::completion_phase(&entry);
        if phase == admin.phase && entry.cid == cid {
            let status = knvme::completion_status_code(&entry);
            admin.cq_head = (admin.cq_head + 1) % admin.entries;
            if admin.cq_head == 0 {
                admin.phase = !admin.phase;
            }
            let cq_doorbell = knvme::NvmeRegs::doorbell_offset(0, true, doorbell_stride);
            mmio.write_reg::<u32>(cq_doorbell, admin.cq_head as u32);
            return status;
        }
        if i >= MMIO_SPIN_BUDGET {
            // Synthetic non-zero status so the state machine lands in
            // `AdminCommandFailed`.
            return 0x7FFF;
        }
        core::hint::spin_loop();
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Identify-response logging.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn log_identify_controller(buf: &DmaBuffer<[u8; NVME_PAGE_BYTES]>) {
    // SAFETY: the DMA region is live for the buffer's lifetime and is
    // page-sized.
    let _bytes: &[u8] =
        unsafe { core::slice::from_raw_parts(buf.user_ptr() as *const u8, NVME_PAGE_BYTES) };
    // D.3 will parse SN / MN / FR out of this buffer; D.2 only logs a
    // single completion line so the boot trace records the step.
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: identify controller ok\n");
}

#[cfg(not(test))]
fn log_identify_namespace(buf: &DmaBuffer<[u8; NVME_PAGE_BYTES]>) {
    // SAFETY: see `log_identify_controller`.
    let _bytes: &[u8] =
        unsafe { core::slice::from_raw_parts(buf.user_ptr() as *const u8, NVME_PAGE_BYTES) };
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: identify namespace ok\n");
}
