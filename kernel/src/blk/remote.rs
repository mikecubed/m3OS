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
//! **Restart semantics:** IPC failure marks the driver mid-restart; one
//! `driver.absent` warn is logged; subsequent calls return `Err(0xFF)` until
//! the driver re-registers via [`mark_driver_ready`].
//!
//! **Grant single-use (Phase 50):** `GrantIdTracker` rejects replay of any
//! write-payload grant handle before the IPC call is attempted.

use kernel_core::driver_ipc::blk_dispatch::{
    BlockDispatchState, GrantIdTracker, RemoteDeviceError,
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
pub fn is_registered() -> bool {
    REMOTE_BLOCK.lock().state.is_registered()
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
pub fn read_sectors(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    if count > MAX_SECTORS_PER_REQUEST as usize {
        return Err(0xFF);
    }
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
        return Err(0xFF);
    }
    let bulk = scheduler::take_bulk_data(task).ok_or(0xFFu8)?;
    let (reply_hdr, _) =
        decode_blk_reply(bulk.get(..BLK_REPLY_HEADER_SIZE).ok_or(0xFFu8)?).map_err(|_| 0xFFu8)?;
    if reply_hdr.status != BlockDriverError::Ok {
        return Err(reply_hdr.status.to_byte());
    }
    let payload = &bulk[BLK_REPLY_HEADER_SIZE..];
    let n = payload.len().min(buf.len());
    buf[..n].copy_from_slice(&payload[..n]);
    Ok(())
}

/// Forward a write to the ring-3 NVMe driver via IPC.
///
/// `payload_grant` is the Phase 50 single-use IPC grant handle carrying the
/// write data (pass `0` for the inline-bulk legacy path).
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
        return Err(0xFF);
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
