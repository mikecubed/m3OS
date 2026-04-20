//! `RemoteBlockDevice` — kernel-side forwarding facade — Phase 55b Track D.4.
//!
//! Dispatch priority (matches Phase 55 with in-kernel NVMe removed):
//!   1. `RemoteBlockDevice` — if [`register`] has been called.
//!   2. VirtIO-blk (in-kernel) — otherwise.
//!
//! Pure-logic state (`BlockDispatchState`, `GrantIdTracker`) lives in
//! `kernel_core::driver_ipc::blk_dispatch` (host-testable). This module
//! holds only the IPC-wiring glue that requires kernel primitives.
//!
//! **Restart semantics (D.4):** When an IPC call fails or the driver is found
//! mid-restart, the facade enters a bounded timed-wait loop:
//!   - Uses `tick_count()` (1 tick = 1 ms, 1000 Hz BSP timer) as the
//!     monotonic clock source. The budget is `BlockDispatchState::restart_deadline_ms`
//!     (default: `DRIVER_RESTART_TIMEOUT_MS = 1000 ms`, see A.1).
//!   - Yields via `scheduler::yield_now()` between poll iterations so other
//!     tasks can run while the facade waits. The lock is NOT held across yields.
//!   - When `is_restarting()` clears (driver re-registered) within the budget,
//!     the IPC call is retried **once** and its result propagated to the caller.
//!   - When the budget expires without recovery, returns `Err(0xFF)` (EIO).
//!
//! **Grant single-use (Phase 50):** `GrantIdTracker` rejects replay of any
//! write-payload grant handle before the IPC call is attempted.

use kernel_core::driver_ipc::blk_dispatch::{
    BlockDispatchState, GrantIdTracker, RemoteDeviceError, WaitOutcome,
};
use kernel_core::driver_ipc::block::{
    BLK_READ, BLK_REPLY_HEADER_SIZE, BLK_REQUEST_HEADER_SIZE, BLK_WRITE, BlkRequestHeader,
    BlockDriverError, MAX_SECTORS_PER_REQUEST, decode_blk_reply, encode_blk_request,
};

use crate::ipc::EndpointId;
use crate::ipc::{endpoint, message::Message, registry};
use crate::task::scheduler;
use spin::{Lazy, Mutex};

static REMOTE_BLOCK: Lazy<Mutex<RemoteBlockInner>> =
    Lazy::new(|| Mutex::new(RemoteBlockInner::new()));

struct RemoteBlockInner {
    state: BlockDispatchState,
    grants: GrantIdTracker,
    endpoint: Option<EndpointId>,
}

impl RemoteBlockInner {
    fn new() -> Self {
        Self {
            state: BlockDispatchState::new(),
            grants: GrantIdTracker::new(),
            endpoint: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register a ring-3 block driver endpoint.  Called by Track F.1 and Track
/// D.5; `#[allow(dead_code)]` until those tracks land.
#[allow(dead_code)]
pub fn register(endpoint_name: &str, device_name: &str) -> Result<(), ()> {
    let ep = registry::lookup_endpoint_id(endpoint_name).ok_or(())?;
    let mut g = REMOTE_BLOCK.lock();
    g.state.register(device_name);
    g.endpoint = Some(ep);
    log::info!(
        "[blk::remote] registered '{}' on endpoint '{}'",
        device_name,
        endpoint_name
    );
    Ok(())
}

/// `true` when a remote driver is installed and ready.
///
/// On the cold path (no endpoint cached yet) performs a one-shot lookup of
/// `"nvme.block"` in the IPC service registry.  If the ring-3 NVMe driver
/// has published its endpoint under that name the facade installs it and
/// returns `true` — so the block dispatch layer immediately starts routing
/// through the ring-3 path without any explicit boot-time wiring call.
///
/// Subsequent calls are fast: once `g.endpoint` is `Some`, the registry
/// lookup is skipped entirely.
pub fn is_registered() -> bool {
    // Fast path — already cached.
    {
        let g = REMOTE_BLOCK.lock();
        if g.state.is_registered() {
            return true;
        }
    }
    // Cold path — attempt a one-shot service-registry lookup.
    if let Some(ep) = registry::lookup_endpoint_id("nvme.block") {
        let mut g = REMOTE_BLOCK.lock();
        // Guard against a race where two callers both hit the cold path.
        if !g.state.is_registered() {
            g.state.register("nvme0");
            g.endpoint = Some(ep);
            log::info!(
                "[blk::remote] auto-registered ring-3 NVMe driver via service \
                 registry ('nvme.block' → endpoint {:?})",
                ep
            );
        }
        return true;
    }
    false
}

/// Re-register after a driver restart; clears the mid-restart flag.
/// `#[allow(dead_code)]` until Track F.2 lands.
#[allow(dead_code)]
pub fn mark_driver_ready(endpoint_name: &str, device_name: &str) -> Result<(), ()> {
    let ep = registry::lookup_endpoint_id(endpoint_name).ok_or(())?;
    let mut g = REMOTE_BLOCK.lock();
    g.state.mark_ready();
    g.endpoint = Some(ep);
    log::info!(
        "[blk::remote] driver '{}' recovered — cleared restart flag",
        device_name
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// I/O forwarding
// ---------------------------------------------------------------------------

/// Forward a read to the ring-3 NVMe driver via IPC.
///
/// If the driver is mid-restart at call time, blocks up to
/// `DRIVER_RESTART_TIMEOUT_MS` for recovery before attempting IPC. On IPC
/// failure, marks the driver mid-restart, waits, and retries the IPC call
/// once if the driver re-registers within the budget. Returns `Err(0xFF)` on
/// timeout.
pub fn read_sectors(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    if count > MAX_SECTORS_PER_REQUEST as usize {
        return Err(0xFF);
    }
    // If the driver is already mid-restart on entry, wait for it first.
    // On timeout, surface DriverRestarting so the caller can distinguish
    // "driver still down" from a generic I/O error (Phase 55b Track F.2b).
    if REMOTE_BLOCK.lock().state.is_restarting() {
        match wait_for_driver_restart() {
            WaitOutcome::Ready => {}
            WaitOutcome::TimedOut | WaitOutcome::Waiting => {
                return Err(BlockDriverError::DriverRestarting.to_byte());
            }
        }
    }
    // Attempt the IPC call; on failure wait + retry once.
    match do_read_ipc(start_sector, count, buf) {
        Ok(()) => Ok(()),
        Err(_) => {
            on_ipc_error();
            match wait_for_driver_restart() {
                WaitOutcome::Ready => do_read_ipc(start_sector, count, buf),
                WaitOutcome::TimedOut | WaitOutcome::Waiting => {
                    Err(BlockDriverError::DriverRestarting.to_byte())
                }
            }
        }
    }
}

/// Inner IPC call for reads — no restart logic.
fn do_read_ipc(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    let (ep, task) = endpoint_and_task()?;
    let hdr = BlkRequestHeader {
        kind: BLK_READ,
        cmd_id: start_sector,
        lba: start_sector,
        sector_count: count as u32,
        flags: 0,
    };
    let encoded = encode_blk_request(hdr, 0u32);
    scheduler::deliver_bulk(task, alloc::vec::Vec::from(encoded.as_slice()));
    let mut msg = Message::new(BLK_READ as u64);
    msg.data[0] = start_sector;
    msg.data[1] = BLK_REQUEST_HEADER_SIZE as u64;
    let reply = endpoint::call_msg(task, ep, msg);
    if reply.label == u64::MAX {
        on_ipc_error();
        // Surface DriverRestarting to the caller so it can distinguish a
        // mid-restart error from a generic I/O error (Phase 55b Tracks D.4b
        // and F.2b). The outer wait-retry loop sees this as a
        // restart-suspected signal and decides whether to block or bail.
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    let bulk = scheduler::take_bulk_data(task).ok_or(0xFFu8)?;
    let (reply_hdr, _) =
        decode_blk_reply(bulk.get(..BLK_REPLY_HEADER_SIZE).ok_or(0xFFu8)?).map_err(|_| 0xFFu8)?;
    if reply_hdr.status != BlockDriverError::Ok {
        return Err(reply_hdr.status.to_byte());
    }
    // A short payload after status=Ok is corrupt/truncated data; fail the
    // read rather than silently hand partial sectors to the VFS.
    const SECTOR_SIZE: usize = 512;
    let expected_len = count.checked_mul(SECTOR_SIZE).ok_or(0xFFu8)?;
    if buf.len() < expected_len {
        return Err(0xFFu8);
    }
    let payload = &bulk[BLK_REPLY_HEADER_SIZE..];
    if payload.len() < expected_len {
        return Err(0xFFu8);
    }
    buf[..expected_len].copy_from_slice(&payload[..expected_len]);
    Ok(())
}

/// Forward a write to the ring-3 NVMe driver via IPC.
///
/// `payload_grant` is the Phase 50 single-use IPC grant handle carrying the
/// write data (pass `0` for the inline-bulk legacy path).
///
/// If the driver is mid-restart at call time, blocks up to
/// `DRIVER_RESTART_TIMEOUT_MS` for recovery before attempting IPC. On IPC
/// failure, marks the driver mid-restart, waits, and retries the IPC call
/// once if the driver re-registers within the budget. Returns `Err(0xFF)` on
/// timeout.
pub fn write_sectors(
    start_sector: u64,
    count: usize,
    buf: &[u8],
    payload_grant: u32,
) -> Result<(), u8> {
    if count > MAX_SECTORS_PER_REQUEST as usize {
        return Err(0xFF);
    }
    // Enforce Phase 50 single-use grant contract before any IPC.
    {
        let mut g = REMOTE_BLOCK.lock();
        match g.grants.consume(payload_grant) {
            Ok(()) => {}
            Err(RemoteDeviceError::GrantReplayed) => {
                log::error!(
                    "[blk::remote] grant 0x{:08x} replayed — Phase 50 violation",
                    payload_grant
                );
                return Err(0xFF);
            }
            Err(_) => return Err(0xFF),
        }
    }
    // If the driver is already mid-restart on entry, wait for it first.
    // On timeout, surface DriverRestarting so the caller can distinguish
    // "driver still down" from a generic I/O error (Phase 55b Track F.2b).
    if REMOTE_BLOCK.lock().state.is_restarting() {
        match wait_for_driver_restart() {
            WaitOutcome::Ready => {}
            WaitOutcome::TimedOut | WaitOutcome::Waiting => {
                return Err(BlockDriverError::DriverRestarting.to_byte());
            }
        }
    }
    // Attempt the IPC call; on failure wait + retry once.
    match do_write_ipc(start_sector, count, buf, payload_grant) {
        Ok(()) => Ok(()),
        Err(_) => {
            on_ipc_error();
            match wait_for_driver_restart() {
                WaitOutcome::Ready => do_write_ipc(start_sector, count, buf, payload_grant),
                WaitOutcome::TimedOut | WaitOutcome::Waiting => {
                    Err(BlockDriverError::DriverRestarting.to_byte())
                }
            }
        }
    }
}

/// Inner IPC call for writes — no restart logic.
fn do_write_ipc(start_sector: u64, count: usize, buf: &[u8], payload_grant: u32) -> Result<(), u8> {
    let (ep, task) = endpoint_and_task()?;
    let hdr = BlkRequestHeader {
        kind: BLK_WRITE,
        cmd_id: start_sector,
        lba: start_sector,
        sector_count: count as u32,
        flags: 0,
    };
    let encoded = encode_blk_request(hdr, payload_grant);
    let mut bulk = alloc::vec![0u8; BLK_REQUEST_HEADER_SIZE + buf.len()];
    bulk[..BLK_REQUEST_HEADER_SIZE].copy_from_slice(&encoded);
    bulk[BLK_REQUEST_HEADER_SIZE..].copy_from_slice(buf);
    scheduler::deliver_bulk(task, bulk);
    let mut msg = Message::new(BLK_WRITE as u64);
    msg.data[0] = start_sector;
    let reply = endpoint::call_msg(task, ep, msg);
    if reply.label == u64::MAX {
        on_ipc_error();
        // Surface DriverRestarting to the caller so it can distinguish a
        // mid-restart error from a generic I/O error (Phase 55b Tracks D.4b
        // and F.2b). The outer wait-retry loop sees this as a
        // restart-suspected signal and decides whether to block or bail.
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    let bulk_r = scheduler::take_bulk_data(task).ok_or(0xFFu8)?;
    let (reply_hdr, _) =
        decode_blk_reply(bulk_r.get(..BLK_REPLY_HEADER_SIZE).ok_or(0xFFu8)?).map_err(|_| 0xFFu8)?;
    if reply_hdr.status != BlockDriverError::Ok {
        return Err(reply_hdr.status.to_byte());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Snapshot the current endpoint + task ID, or return `Err(0xFF)`.
fn endpoint_and_task() -> Result<(EndpointId, crate::task::TaskId), u8> {
    let g = REMOTE_BLOCK.lock();
    let ep = g.endpoint.ok_or(0xFFu8)?;
    let task = scheduler::current_task_id().ok_or(0xFFu8)?;
    Ok((ep, task))
}

/// Mark the driver mid-restart and emit one `driver.absent` warn.
fn on_ipc_error() {
    let mut g = REMOTE_BLOCK.lock();
    if !g.state.is_restarting() {
        g.state.mark_restarting();
        log::warn!(
            "[blk::remote] driver '{}' unreachable — marking mid-restart",
            g.state.device_name().unwrap_or("<unknown>")
        );
    }
}

/// Block up to `DRIVER_RESTART_TIMEOUT_MS` for the driver to re-register.
///
/// Called when the driver is found mid-restart (either because `is_registered()`
/// was false, or because an IPC call returned a failure sentinel). The function
/// polls `is_restarting()` at each scheduler yield until either:
///
/// - The flag clears → returns `WaitOutcome::Ready` (caller should retry IPC).
/// - The budget expires → returns `WaitOutcome::TimedOut` (caller returns EIO).
///
/// **Lock discipline:** the `REMOTE_BLOCK` mutex is acquired only for a brief
/// snapshot on each iteration and is released before `yield_now()`. This
/// prevents priority inversion and satisfies the documented lock-ordering rule
/// (no locks held across a yield point).
///
/// **Clock source:** `tick_count()` from `arch::x86_64::interrupts` gives a
/// monotonically increasing u64 at 1 tick per millisecond (1000 Hz BSP timer).
/// The restart-deadline budget is read once at the start of the wait from
/// `state.restart_deadline_ms` (defaults to `DRIVER_RESTART_TIMEOUT_MS`).
fn wait_for_driver_restart() -> WaitOutcome {
    // Snapshot the restart budget without holding the lock across yields.
    let budget_ms = {
        let g = REMOTE_BLOCK.lock();
        g.state.restart_deadline_ms as u64
    };
    let start_tick = crate::arch::x86_64::interrupts::tick_count();
    let deadline_tick = start_tick.saturating_add(budget_ms);

    loop {
        let now_tick = crate::arch::x86_64::interrupts::tick_count();
        let is_ready = {
            let g = REMOTE_BLOCK.lock();
            !g.state.is_restarting()
        };
        match BlockDispatchState::check_restart_wait(now_tick, deadline_tick, is_ready) {
            WaitOutcome::Ready => return WaitOutcome::Ready,
            WaitOutcome::TimedOut => return WaitOutcome::TimedOut,
            // Within budget, driver still absent: yield and retry.
            WaitOutcome::Waiting => {
                scheduler::yield_now();
            }
        }
    }
}
