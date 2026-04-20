//! Ring-3 NVMe driver — Phase 55b Tracks D.1 (scaffold), D.2 (bring-up),
//! and D.3 (I/O queue pair + block IPC path).
//!
//! Phase 55b moves the NVMe driver from ring 0 (`kernel/src/blk/nvme.rs`)
//! into this userspace crate. Track D.1 landed the crate shell and the
//! four-place userspace-binary wiring (workspace member, xtask bins,
//! ramdisk embedding, future service config). Track D.2 ported the
//! controller bring-up path to [`driver_runtime`]. Track D.3 — this
//! layer — adds the I/O queue pair (Create I/O CQ / Create I/O SQ admin
//! commands, MSI-X IRQ subscription, per-request PRP construction) and
//! the `BlockServer::handle_next` loop that serialises IPC requests
//! over the I/O queue.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`init`] | Pure bring-up state machine + CC / AQA encoders (host-testable) |
//! | [`io`]   | PRP construction, Create I/O CQ / SQ encoders, Read / Write encoders, completion phase-bit drain (host-testable), plus the `IoQueuePair` wrapper and `handle_read` / `handle_write` glue (non-test) |
//!
//! # Run-time flow
//!
//! 1. `program_main` calls `bring_up_controller` to drive the Phase 55b
//!    D.2 state machine to `Identified`. On success the function
//!    returns a [`BringUpContext`] bundling the claimed `DeviceHandle`,
//!    BAR0 `Mmio`, admin queue, and namespace metadata.
//! 2. `run_io_server` allocates the I/O SQ / CQ / PRP-list DMA pages,
//!    submits Create I/O CQ and Create I/O SQ admin commands, and
//!    subscribes the MSI-X vector via
//!    [`IrqNotification::subscribe`] (best-effort — a subscription
//!    failure falls back to a polled-completion path).
//! 3. The process creates a Phase 50 endpoint, registers it as
//!    [`SERVICE_NAME`] with the IPC registry, and enters the
//!    `BlockServer::handle_next` dispatch loop. Each request is
//!    routed to `handle_read`, `handle_write`, or `BLK_STATUS`.
//!
//! On unrecoverable error the process exits non-zero so the Phase 46 /
//! 51 service manager's restart path observes the failure.
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
use driver_runtime::ipc::EndpointCap;
#[cfg(not(test))]
use driver_runtime::ipc::block::{BlkReply, BlockServer};
#[cfg(not(test))]
use driver_runtime::{
    DeviceCapKey, DeviceHandle, DmaBuffer, DriverRuntimeError, IrqNotification, Mmio,
};
#[cfg(not(test))]
use kernel_core::driver_ipc::block::{BLK_READ, BLK_STATUS, BLK_WRITE, BlockDriverError};
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
use crate::io::{
    IO_QUEUE_ID, IoQueuePair, build_create_io_cq_command, build_create_io_sq_command, handle_read,
    handle_write,
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

/// Thin newtype wrapping [`DeviceHandle`] so this crate (not
/// `driver_runtime`) can provide the
/// `driver_runtime::irq::DeviceCapHandle` impl that
/// [`IrqNotification::subscribe`] requires. The orphan rule blocks a
/// direct impl on `DeviceHandle`; wrapping in a local struct is the
/// standard escape hatch and carries no runtime overhead (the
/// wrapper is `#[repr(transparent)]` around a borrow).
#[cfg(not(test))]
struct DeviceCap<'a>(&'a DeviceHandle);

#[cfg(not(test))]
impl driver_runtime::irq::DeviceCapHandle for DeviceCap<'_> {
    fn cap_handle(&self) -> u32 {
        self.0.cap()
    }
}

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

/// Service name the D.4 `RemoteBlockDevice` kernel facade looks up to
/// route block requests at this driver. Landing the registration
/// string here in one place keeps D.4's lookup side and F.1's service
/// manifest referring to the same constant.
#[cfg(not(test))]
pub const SERVICE_NAME: &str = "nvme.block";

/// Default sector size assumed when Identify Namespace parsing has
/// not yet populated a real value. NVMe QEMU devices and every
/// real-world target we ship against default to 512 B; the value is
/// refined during D.3 bring-up as Identify Namespace succeeds.
#[cfg(not(test))]
pub const DEFAULT_SECTOR_BYTES: u32 = 512;

/// Default namespace ID the driver services. Matches the kernel-side
/// Phase 55 driver's choice (`nsid = 1`).
#[cfg(not(test))]
pub const DEFAULT_NSID: u32 = 1;

/// Claim the sentinel NVMe BDF, map BAR0, run controller bring-up, and
/// then enter the block-IPC server loop. The process exits only when
/// bring-up fails or the IPC loop reports an unrecoverable error.
#[cfg(not(test))]
fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: spawned\n");

    let ctx = match bring_up_controller() {
        Ok(ctx) => {
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: bring-up complete\n");
            ctx
        }
        // The target NVMe controller is not available to us. Two cases
        // collapse to the same outcome from the driver's perspective and
        // are both non-restart-worthy:
        //
        //   * `NotClaimed` — the sentinel BDF is not enumerated in PCI,
        //     i.e. QEMU was launched without `--device nvme` and the
        //     slot is empty. `sys_device_claim` returns ENODEV.
        //
        //   * `AlreadyClaimed` — the sentinel BDF is occupied by an
        //     unrelated device already owned by an in-kernel driver
        //     (e.g. QEMU's default machine places virtio-blk at slot 4,
        //     which collides with `SENTINEL_BDF`). `sys_device_claim`
        //     returns EBUSY. From the driver's perspective the NVMe
        //     controller simply isn't there — the fact that some other
        //     device occupies the slot is incidental.
        //
        // Exit cleanly in both cases so init's `on-failure` policy
        // marks the service permanently stopped rather than burning
        // the restart budget on a device that will never appear.
        Err(InitError::Runtime(DriverRuntimeError::Device(
            kernel_core::device_host::DeviceHostError::NotClaimed
            | kernel_core::device_host::DeviceHostError::AlreadyClaimed,
        ))) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "nvme_driver: no NVMe device present at sentinel BDF — exiting cleanly\n",
            );
            return 0;
        }
        Err(e) => {
            // Log the specific variant so the reader can correlate.
            let _collapsed: BlockDriverError = e.into();
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: bring-up failed\n");
            // Exit non-zero so the service manager's restart path
            // (Phase 46 / 51) observes the failure.
            return 1;
        }
    };

    match run_io_server(ctx) {
        Ok(()) => 0,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: io-server exited\n");
            2
        }
    }
}

/// Bring-up outcome the I/O server consumes. Kept internal to the
/// binary — callers outside `main.rs` never construct or observe one.
#[cfg(not(test))]
struct BringUpContext {
    device: DeviceHandle,
    mmio: Mmio<NvmeRegsTag>,
    admin: AdminQueue,
    doorbell_stride: usize,
    /// Namespace identifier the driver services (Phase 55b D.3 ships
    /// NSID = 1; future phases walk the active-namespace list).
    nsid: u32,
    /// Logical block size in bytes. D.2 logs the Identify Namespace
    /// buffer; a future refactor will parse LBAF0.LBADS to refine this
    /// from [`DEFAULT_SECTOR_BYTES`]. For Phase 55b's in-QEMU smoke the
    /// default is correct.
    sector_bytes: u32,
}

#[cfg(not(test))]
fn bring_up_controller() -> Result<BringUpContext, InitError> {
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
    // will parse them for NSID sector size in a follow-up; for now
    // the `BringUpContext` reports [`DEFAULT_SECTOR_BYTES`] which
    // matches every QEMU target we care about.
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
    let doorbell_stride = sm.doorbell_stride_bytes();
    let admin = match admin {
        Some(q) => q,
        None => return Err(InitError::BringUp(BringUpError::AdminCommandFailed)),
    };
    Ok(BringUpContext {
        device,
        mmio,
        admin,
        doorbell_stride,
        nsid: DEFAULT_NSID,
        sector_bytes: DEFAULT_SECTOR_BYTES,
    })
}

// ---------------------------------------------------------------------------
// I/O server — Create I/O CQ/SQ, subscribe IRQ, run BlockServer loop.
// ---------------------------------------------------------------------------

/// Wire up the I/O queue pair, issue the Create I/O CQ / Create I/O SQ
/// admin commands, subscribe the MSI-X vector, create the IPC
/// endpoint, and enter the `BlockServer::handle_next` loop.
///
/// Every error path collapses to an `InitError` that
/// `program_main` turns into a non-zero exit — the service manager's
/// restart path (Phase 46 / 51) observes the failure and brings the
/// driver back up.
#[cfg(not(test))]
fn run_io_server(mut ctx: BringUpContext) -> Result<(), InitError> {
    // Step 1: allocate I/O SQ / CQ / PRP-list DMA pages.
    let mut io_queue = IoQueuePair::allocate(&ctx.device, ctx.doorbell_stride)?;
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: io queue allocated\n");

    // Step 2: Create I/O CQ (admin 0x05). Must run before Create I/O SQ
    // so the SQ has a CQ to target.
    let entries = io_queue.bookkeeping.entries();
    {
        let cmd = build_create_io_cq_command(0, IO_QUEUE_ID, entries, io_queue.cq_iova(), 0);
        let status = submit_admin_command(&ctx.mmio, &mut ctx.admin, ctx.doorbell_stride, cmd);
        if status != 0 {
            return Err(InitError::BringUp(BringUpError::AdminCommandFailed));
        }
    }
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: io cq created\n");

    // Step 3: Create I/O SQ (admin 0x01).
    {
        let cmd =
            build_create_io_sq_command(0, IO_QUEUE_ID, entries, io_queue.sq_iova(), IO_QUEUE_ID);
        let status = submit_admin_command(&ctx.mmio, &mut ctx.admin, ctx.doorbell_stride, cmd);
        if status != 0 {
            return Err(InitError::BringUp(BringUpError::AdminCommandFailed));
        }
    }
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: io sq created\n");

    // Step 4: subscribe the MSI-X vector on a best-effort basis. A
    // subscription failure is logged but the driver continues on the
    // polled fallback in `IoQueuePair::wait_completion`.
    match IrqNotification::subscribe(&DeviceCap(&ctx.device), None) {
        Ok(irq) => {
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: msi-x subscribed\n");
            io_queue.set_irq(irq);
        }
        Err(_) => {
            syscall_lib::write_str(
                STDOUT_FILENO,
                "nvme_driver: msi-x subscribe failed — polled fallback\n",
            );
        }
    }

    // Step 5: Phase 55b F.4b — 512 B LBA-0 round-trip self-test.
    // Executes before the IPC endpoint is exposed so no concurrent
    // client can race with the self-test pattern on LBA 0.
    nvme_self_test(
        &ctx.mmio,
        &mut io_queue,
        &ctx.device,
        ctx.nsid,
        ctx.sector_bytes,
    );

    // Step 6: create and register the IPC endpoint.
    let endpoint = create_service_endpoint(SERVICE_NAME)?;
    syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: service registered\n");

    // Step 7: enter the block-server loop.
    let server = BlockServer::new(endpoint);
    loop {
        let result = server.handle_next(|req| {
            match req.header.kind {
                BLK_READ => {
                    let (header, bulk) = handle_read(
                        &ctx.mmio,
                        &mut io_queue,
                        &ctx.device,
                        ctx.nsid,
                        ctx.sector_bytes,
                        &req.header,
                    );
                    BlkReply {
                        header,
                        payload_grant: 0,
                        bulk,
                    }
                }
                BLK_WRITE => {
                    let header = handle_write(
                        &ctx.mmio,
                        &mut io_queue,
                        &ctx.device,
                        ctx.nsid,
                        ctx.sector_bytes,
                        &req.header,
                        &req.bulk,
                    );
                    BlkReply {
                        header,
                        payload_grant: 0,
                        bulk: alloc::vec::Vec::new(),
                    }
                }
                BLK_STATUS => {
                    // BLK_STATUS is a cheap health-check; reply Ok with no bulk.
                    BlkReply {
                        header: kernel_core::driver_ipc::block::BlkReplyHeader {
                            cmd_id: req.header.cmd_id,
                            status: BlockDriverError::Ok,
                            bytes: 0,
                        },
                        payload_grant: 0,
                        bulk: alloc::vec::Vec::new(),
                    }
                }
                _ => BlkReply {
                    header: kernel_core::driver_ipc::block::BlkReplyHeader {
                        cmd_id: req.header.cmd_id,
                        status: BlockDriverError::InvalidRequest,
                        bytes: 0,
                    },
                    payload_grant: 0,
                    bulk: alloc::vec::Vec::new(),
                },
            }
        });
        if let Err(e) = result {
            // A single recv / reply failure is not fatal — the next
            // iteration re-enters `handle_next`. But if the error
            // repeats we exit so the service manager restarts the
            // driver; Phase 50's IPC surface does not distinguish
            // transient from fatal today.
            syscall_lib::write_str(STDOUT_FILENO, "nvme_driver: handle_next error\n");
            let _ = e;
            return Ok(());
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 55b F.4b — 512 B LBA-0 round-trip self-test.
// ---------------------------------------------------------------------------

/// Sentinel byte repeated across the 512-byte self-test sector.
///
/// Chosen to be obviously non-zero so a zero-fill (uninitialized DMA
/// buffer or device returning zeros) is distinguishable from a real
/// round-trip. `0xA5` is the classic alternating-bit pattern.
#[cfg(not(test))]
const SELF_TEST_SENTINEL: u8 = 0xA5;

/// Issue a 512-byte write to LBA 0, then read it back, and verify the
/// sentinel pattern.  Prints `NVME_SMOKE:rw:PASS` on success or
/// `NVME_SMOKE:rw:FAIL` on any error (allocation, I/O, or mismatch).
///
/// Called from [`run_io_server`] after the I/O queue is active and
/// subscribed but before the IPC endpoint is registered, so no
/// concurrent client can race with the pattern on LBA 0.
///
/// Design notes:
/// - Uses [`handle_write`] and [`handle_read`] directly (the same
///   paths the IPC server loop exercises) to exercise the full DMA /
///   PRP / doorbell / completion chain in-band.
/// - LBA 0 is used deliberately: it is always present and the self-test
///   pattern is intentionally overwritten immediately afterward (this is
///   a raw smoke, not a data-preservation test).
/// - `NVME_SMOKE:rw:FAIL` is emitted on any sub-step failure so the
///   smoke harness never silently misses a broken round-trip.
#[cfg(not(test))]
fn nvme_self_test(
    mmio: &Mmio<NvmeRegsTag>,
    queue: &mut IoQueuePair,
    device: &DeviceHandle,
    nsid: u32,
    sector_bytes: u32,
) {
    use kernel_core::driver_ipc::block::{BLK_READ, BLK_WRITE, BlkRequestHeader, BlockDriverError};

    // Build a 512-byte write buffer filled with the sentinel pattern.
    let mut write_data = alloc::vec![SELF_TEST_SENTINEL; 512];

    let write_hdr = BlkRequestHeader {
        kind: BLK_WRITE,
        cmd_id: 0xF4B1,
        lba: 0,
        sector_count: 1,
        flags: 0,
    };
    let write_reply = handle_write(
        mmio,
        queue,
        device,
        nsid,
        sector_bytes,
        &write_hdr,
        &write_data,
    );
    if write_reply.status != BlockDriverError::Ok {
        syscall_lib::write_str(STDOUT_FILENO, "NVME_SMOKE:rw:FAIL write-error\n");
        return;
    }

    // Overwrite the local buffer so we know the read actually fetched
    // device data, not a residual from the write buffer.
    for b in write_data.iter_mut() {
        *b = 0;
    }

    let read_hdr = BlkRequestHeader {
        kind: BLK_READ,
        cmd_id: 0xF4B2,
        lba: 0,
        sector_count: 1,
        flags: 0,
    };
    let (read_reply, bulk) = handle_read(mmio, queue, device, nsid, sector_bytes, &read_hdr);
    if read_reply.status != BlockDriverError::Ok {
        syscall_lib::write_str(STDOUT_FILENO, "NVME_SMOKE:rw:FAIL read-error\n");
        return;
    }

    // Verify every returned byte matches the sentinel.
    if bulk.len() < 512 || bulk[..512].iter().any(|&b| b != SELF_TEST_SENTINEL) {
        syscall_lib::write_str(STDOUT_FILENO, "NVME_SMOKE:rw:FAIL pattern-mismatch\n");
        return;
    }

    syscall_lib::write_str(STDOUT_FILENO, "NVME_SMOKE:rw:PASS\n");
}

/// Create an IPC endpoint and register it under `name` with the
/// service registry. Returns a typed [`EndpointCap`] so the
/// `BlockServer` builder receives the wrapper type directly.
#[cfg(not(test))]
fn create_service_endpoint(name: &str) -> Result<EndpointCap, InitError> {
    let ep = syscall_lib::create_endpoint();
    if ep == u64::MAX {
        return Err(InitError::Runtime(DriverRuntimeError::Device(
            kernel_core::device_host::DeviceHostError::Internal,
        )));
    }
    let ep_u32 = u32::try_from(ep).map_err(|_| {
        InitError::Runtime(DriverRuntimeError::Device(
            kernel_core::device_host::DeviceHostError::Internal,
        ))
    })?;
    let rc = syscall_lib::ipc_register_service(ep_u32, name);
    if rc == u64::MAX {
        return Err(InitError::Runtime(DriverRuntimeError::Device(
            kernel_core::device_host::DeviceHostError::Internal,
        )));
    }
    Ok(EndpointCap::new(ep_u32))
}

/// Submit one admin command, ring the admin SQ doorbell, poll the
/// admin CQ for the matching CID, and return the 15-bit status code.
///
/// Thin wrapper over the D.2 `submit_identify` pattern — exists as a
/// separate helper because D.3 issues the two Create I/O Queue
/// commands *after* Identify, and we want the admin-queue bookkeeping
/// (next_cid / sq_tail / cq_head / phase) to remain owned by a single
/// function family.
#[cfg(not(test))]
fn submit_admin_command(
    mmio: &Mmio<NvmeRegsTag>,
    admin: &mut AdminQueue,
    doorbell_stride: usize,
    mut cmd: knvme::NvmeCommand,
) -> u16 {
    let cid = admin.next_cid % admin.entries;
    // Re-stamp the command's CID so callers building the command with
    // `cid = 0` don't need to know the current admin bookkeeping.
    cmd.cdw0 = (cmd.cdw0 & 0x0000_FFFF) | ((cid as u32) << 16);

    // SAFETY: sq_entry_ptr returns a pointer inside the DMA region;
    // no concurrent writer (admin bring-up is strictly sequential).
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
        // SAFETY: cq_entry_ptr is within the DMA allocation.
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
            // Synthetic non-zero status so the caller treats it as an
            // admin-command failure.
            return 0x7FFF;
        }
        core::hint::spin_loop();
        i += 1;
    }
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
