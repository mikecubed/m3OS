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
//!
//! **Phase 92a D.4 — multi-device registry:**
//! The singleton `RemoteBlockInner` is lifted to a bounded array of
//! `RemoteBlockEntry` slots (max [`MAX_REMOTE_BLOCK`] = 4). Slot 0 is the
//! **root backend** — nvme.block/ahci.block auto-discovered on the first
//! `is_registered()` call, exactly as before. Slots 1-3 are **additional
//! devices** explicitly registered by name (e.g. "usb0.block") via
//! [`register_device`] / unregistered via [`unregister_device`], enabling a
//! USB stick to coexist with the AHCI/NVMe root without changing any root-FS
//! paths. The root API (`is_registered`, `read_sectors`, `write_sectors`,
//! `flush`) routes exclusively to slot 0 and is behaviorally unchanged.

use kernel_core::driver_ipc::blk_dispatch::{
    BlockDispatchState, GrantIdTracker, RemoteDeviceError, WaitOutcome,
};
use kernel_core::driver_ipc::block::{
    BLK_FLUSH, BLK_READ, BLK_REPLY_HEADER_SIZE, BLK_REQUEST_HEADER_SIZE, BLK_WRITE,
    BlkRequestHeader, BlockDriverError, MAX_SECTORS_PER_REQUEST, decode_blk_reply,
    encode_blk_request, restart_suspected,
};

use crate::ipc::EndpointId;
use crate::ipc::{endpoint, message::Message, registry};
use crate::task::scheduler;
use crate::task::scheduler::IrqSafeMutex;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Lazy;

// ---------------------------------------------------------------------------
// Phase 92a D.4 — multi-device registry
// ---------------------------------------------------------------------------

/// Maximum number of concurrent remote block backends (root + additional).
/// Slot 0 = root (nvme.block / ahci.block). Slots 1-3 = explicitly registered
/// secondary devices (e.g. USB mass-storage).
pub const MAX_REMOTE_BLOCK: usize = 4;

/// One entry in the remote block registry.
struct RemoteBlockEntry {
    state: BlockDispatchState,
    grants: GrantIdTracker,
    endpoint: Option<EndpointId>,
    /// IPC-registry service name used to look up this endpoint (e.g.
    /// "usb0.block"). Empty for the root slot's auto-discovered entries.
    service_name: alloc::string::String,
    /// Human-readable device name logged in diagnostics (e.g. "usb0").
    device_name: alloc::string::String,
    /// `TaskId` of the driver process that published this endpoint (0 =
    /// kernel-registered, i.e. from the auto-registration path).
    owner_task_id: u64,
    /// Whether this slot is occupied by an explicitly registered device.
    /// The root slot (index 0) uses auto-discovery and is never set via
    /// `register_device`, so this flag stays `false` for slot 0.
    explicitly_registered: bool,
}

impl RemoteBlockEntry {
    fn empty() -> Self {
        Self {
            state: BlockDispatchState::new(),
            grants: GrantIdTracker::new(),
            endpoint: None,
            service_name: alloc::string::String::new(),
            device_name: alloc::string::String::new(),
            owner_task_id: 0,
            explicitly_registered: false,
        }
    }
}

// ---------------------------------------------------------------------------
// The registry is a fixed-size array wrapped in a single IrqSafeMutex.
// `Lazy` defers construction until first use (avoids a `const`-fn heap issue).
// ---------------------------------------------------------------------------

struct RemoteBlockRegistry {
    entries: [RemoteBlockEntry; MAX_REMOTE_BLOCK],
}

impl RemoteBlockRegistry {
    fn new() -> Self {
        // Rust does not support `[expr; N]` for non-Copy types, so each slot
        // is explicitly constructed. All four entries start in the same
        // unregistered state produced by `RemoteBlockEntry::empty()`.
        Self {
            entries: [
                RemoteBlockEntry::empty(),
                RemoteBlockEntry::empty(),
                RemoteBlockEntry::empty(),
                RemoteBlockEntry::empty(),
            ],
        }
    }
}

static REMOTE_BLOCK: Lazy<IrqSafeMutex<RemoteBlockRegistry>> =
    Lazy::new(|| IrqSafeMutex::new(RemoteBlockRegistry::new()));

// ---------------------------------------------------------------------------
// Lock-free fast-path flags
// ---------------------------------------------------------------------------

/// Lock-free bitmask: bit N is set when slot N is occupied with a registered
/// driver. Allows hot-path callers (`is_registered`, `on_endpoint_closed`) to
/// skip the mutex when no drivers are present.
///
/// Bit 0 = root slot; bits 1-3 = explicit secondary devices.
static REMOTE_BLOCK_REGISTERED_MASK: AtomicU32 = AtomicU32::new(0);

/// Convenience: `true` when the root slot (bit 0) is registered.
/// Kept for the existing `on_endpoint_closed` fast-path (unchanged call site).
static REMOTE_BLOCK_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Phase 106 C.3 — root-service skip mask. When a root mount fails to
/// find ext2 on the device a service auto-adopted into slot 0 (e.g. a
/// blank internal NVMe present during a USB-image install boot),
/// [`release_root_and_skip`] sets that service's bit here so the next
/// [`is_registered`] cold-path re-evaluation moves on to the next
/// candidate instead of re-adopting the same unmountable device. Only
/// the three auto-discovered root services have bits; secondary
/// (explicitly-registered) slots are untouched.
static ROOT_SKIP_MASK: AtomicU32 = AtomicU32::new(0);
const SKIP_NVME: u32 = 1 << 0;
const SKIP_AHCI: u32 = 1 << 1;
const SKIP_USB: u32 = 1 << 2;

/// Map a root service name to its [`ROOT_SKIP_MASK`] bit.
fn root_service_skip_bit(service: &str) -> u32 {
    match service {
        "nvme.block" => SKIP_NVME,
        "ahci.block" => SKIP_AHCI,
        "usb0.block" => SKIP_USB,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Root-slot public API (unchanged signatures — preserves all existing callers)
// ---------------------------------------------------------------------------

/// Register a ring-3 block driver endpoint.  Called by Track F.1 and Track
/// D.5; `#[allow(dead_code)]` until those tracks land.
#[allow(dead_code)]
pub fn register(endpoint_name: &str, device_name: &str) -> Result<(), ()> {
    let ep = registry::lookup_endpoint_id(endpoint_name).ok_or(())?;
    let mut g = REMOTE_BLOCK.lock();
    let slot = &mut g.entries[0];
    slot.state.register(device_name);
    slot.endpoint = Some(ep);
    slot.service_name = alloc::string::String::from(endpoint_name);
    slot.device_name = alloc::string::String::from(device_name);
    slot.owner_task_id = 0;
    slot.explicitly_registered = false;
    REMOTE_BLOCK_REGISTERED.store(true, Ordering::Release);
    REMOTE_BLOCK_REGISTERED_MASK.fetch_or(1, Ordering::Release);
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
/// `"nvme.block"` and then `"ahci.block"` in the IPC service registry.  If a
/// ring-3 NVMe **or** AHCI/SATA driver has published its endpoint under one of
/// those names **and** the publishing task is a trusted `/drivers/` process
/// (its `exec_path` starts with `/drivers/` — see the owner gate below; note
/// this includes a driver init spawns directly, not only service-manager
/// supervised ones), the facade installs it and returns `true` — so the block
/// dispatch layer immediately starts routing through the ring-3 path without
/// any explicit boot-time wiring call. `"nvme.block"` is preferred when both
/// are present.
///
/// **Owner gate:** the auto-registration only fires when the owner of the
/// `nvme.block` / `ahci.block` service registration is a trusted driver process — its
/// `exec_path` must live under `/drivers/` (the same prefix
/// `sys_device_claim` uses to authorize PCI claims). An arbitrary ring-3
/// task that grabs the name first is ignored: `ipc_register_service`
/// accepts any non-private name, so without this check, a spoofed
/// `nvme.block` endpoint would steer kernel filesystem I/O through an
/// untrusted process. Explicit [`register`] calls from a caller that
/// knows better bypass this gate because they write `g.state` directly
/// and the fast path above catches them before the registry lookup.
///
/// **Cold-path gate:** the auto-registration only fires when the in-kernel
/// virtio-blk driver is *not* serving the root filesystem
/// (`VIRTIO_BLK_READY == false`). When virtio-blk is active the VFS's
/// block I/O targets the virtio data disk — a ring-3 NVMe driver attached
/// to a separate physical device (e.g. QEMU's `--device nvme`) must not
/// hijack that path, otherwise reads of `/etc/shadow` et al. would be
/// misrouted to a device that does not contain the filesystem.
///
/// Subsequent calls are fast: once `g.endpoint` is `Some`, the registry
/// lookup is skipped entirely.
pub fn is_registered() -> bool {
    // Fast path — already cached.
    {
        let g = REMOTE_BLOCK.lock();
        if g.entries[0].state.is_registered() {
            return true;
        }
    }
    // Cold-path gate: defer to virtio-blk when it is the root block device.
    // See function docs for rationale. An explicit `register` call bypasses
    // this gate because it writes to `g.state` directly and the fast path
    // above catches it before we ever reach the service-registry lookup.
    if crate::blk::virtio_blk::VIRTIO_BLK_READY.load(core::sync::atomic::Ordering::Acquire) {
        return false;
    }
    // Cold path — attempt a one-shot service-registry lookup *with owner*. A
    // trusted `/drivers/` process may publish either `"nvme.block"` (Phase 55b
    // NVMe) or `"ahci.block"` (Phase 82 AHCI/SATA); `"nvme.block"` takes priority
    // when both are present (it is looked up first regardless of which driver
    // registered earlier). This is the one scoped data-path kernel change the
    // AHCI phase makes (Phase 82 D.2) — the analog of Phase 81's
    // `default_route_index_by_link`.
    // Phase 106 C.3 — honor the skip mask: a candidate whose bit is set
    // failed a prior root mount (no ext2 on it), so fall through to the
    // next-priority service instead of re-adopting it.
    let skip = ROOT_SKIP_MASK.load(Ordering::Acquire);
    let try_lookup = |service: &'static str, dev: &'static str, bit: u32| {
        if skip & bit != 0 {
            return None;
        }
        registry::lookup_endpoint_with_owner(service).map(|(ep, owner)| (service, dev, ep, owner))
    };
    let (service_name, device_name, ep, owner_task_id) =
        match try_lookup("nvme.block", "nvme0", SKIP_NVME)
            .or_else(|| try_lookup("ahci.block", "ahci0", SKIP_AHCI))
            // Phase 106 A.2 — last-resort root backend: the boot USB stick's
            // mass-storage device (the combined GPT image). Strictly lowest
            // priority so an internal NVMe/AHCI disk always wins when present.
            .or_else(|| try_lookup("usb0.block", "usb0", SKIP_USB))
        {
            Some(v) => v,
            None => return false,
        };
    // Owner gate: reject registrations from processes whose `exec_path` is not
    // under `/drivers/` (see `is_trusted_driver_task`). `owner == 0`
    // (kernel-registered) is treated as trusted so the boot-time wiring path
    // still works.
    if owner_task_id != 0 && !is_trusted_driver_task(owner_task_id) {
        // Log once per cold-path miss so a spoofed registration is
        // visible in the boot log without spamming every VFS call.
        log::warn!(
            "[blk::remote] ignoring '{}' registration from untrusted \
             task_id={} (not a /drivers/ process)",
            service_name,
            owner_task_id
        );
        return false;
    }
    let mut g = REMOTE_BLOCK.lock();
    // Guard against a race where two callers both hit the cold path.
    let slot = &mut g.entries[0];
    if !slot.state.is_registered() {
        slot.state.register(device_name);
        slot.endpoint = Some(ep);
        slot.service_name = alloc::string::String::from(service_name);
        slot.device_name = alloc::string::String::from(device_name);
        slot.owner_task_id = owner_task_id;
        slot.explicitly_registered = false;
        REMOTE_BLOCK_REGISTERED.store(true, Ordering::Release);
        REMOTE_BLOCK_REGISTERED_MASK.fetch_or(1, Ordering::Release);
        log::info!(
            "[blk::remote] auto-registered ring-3 '{}' driver via service \
             registry ({} → endpoint {:?}, owner task_id={})",
            device_name,
            service_name,
            ep,
            owner_task_id
        );
    }
    true
}

/// `true` when `owner_task_id` belongs to a trusted `/drivers/` process —
/// i.e. its `exec_path` starts with `/drivers/`. Mirrors the authorization
/// gate in `sys_device_claim` so the kernel's trust classification is
/// consistent across the device-host and the block-dispatch entry points.
fn is_trusted_driver_task(owner_task_id: u64) -> bool {
    use kernel_core::types::TaskId;
    let task_id = TaskId(owner_task_id);
    let Some(pid) = scheduler::pid_for_task_id(task_id) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    let table = crate::process::PROCESS_TABLE.lock();
    match table.find(pid) {
        Some(p) => p.exec_path.starts_with("/drivers/"),
        None => false,
    }
}

/// Re-register after a driver restart; clears the mid-restart flag.
/// `#[allow(dead_code)]` until Track F.2 lands.
#[allow(dead_code)]
pub fn mark_driver_ready(endpoint_name: &str, device_name: &str) -> Result<(), ()> {
    let ep = registry::lookup_endpoint_id(endpoint_name).ok_or(())?;
    let mut g = REMOTE_BLOCK.lock();
    let slot = &mut g.entries[0];
    slot.state.mark_ready();
    slot.endpoint = Some(ep);
    log::info!(
        "[blk::remote] driver '{}' recovered — cleared restart flag",
        device_name
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 92a D.4 — additional-device (dev_id >= 1) public API
// ---------------------------------------------------------------------------

/// Register an additional remote block device by explicit service name.
///
/// Looks up `service_name` in the IPC registry (e.g. `"usb0.block"`),
/// validates the `/drivers/` owner gate, and installs it in the first free
/// slot with index >= 1. Returns the assigned `dev_id` on success, or `None`
/// when the registry is full or the service name is unknown / untrusted.
///
/// The root slot (index 0 / nvme.block / ahci.block) is managed exclusively
/// by `is_registered()` auto-discovery; never assign it via this function.
#[allow(dead_code)]
pub fn register_device(service_name: &str, device_name: &str) -> Option<u32> {
    let (ep, owner_task_id) = registry::lookup_endpoint_with_owner(service_name)?;
    // Owner gate: same rule as the root auto-registration.
    if owner_task_id != 0 && !is_trusted_driver_task(owner_task_id) {
        log::warn!(
            "[blk::remote] register_device: ignoring '{}' from untrusted task_id={}",
            service_name,
            owner_task_id
        );
        return None;
    }
    let mut g = REMOTE_BLOCK.lock();
    // Find a free slot with index >= 1.
    let slot_idx = g
        .entries
        .iter()
        .enumerate()
        .skip(1) // never touch the root slot
        .find(|(_, e)| e.endpoint.is_none())
        .map(|(i, _)| i)?;
    let slot = &mut g.entries[slot_idx];
    slot.state.register(device_name);
    slot.endpoint = Some(ep);
    slot.service_name = alloc::string::String::from(service_name);
    slot.device_name = alloc::string::String::from(device_name);
    slot.owner_task_id = owner_task_id;
    slot.explicitly_registered = true;
    let dev_id = slot_idx as u32;
    REMOTE_BLOCK_REGISTERED_MASK.fetch_or(1u32 << dev_id, Ordering::Release);
    log::info!(
        "[blk::remote] register_device: '{}' → dev_id={} endpoint={:?} owner={}",
        device_name,
        dev_id,
        ep,
        owner_task_id
    );
    Some(dev_id)
}

/// Release a secondary device slot (for hot-unplug).
///
/// Clears `dev_id`'s slot in the registry. The root slot (dev_id=0) may not
/// be unregistered via this call — it is managed by the auto-registration path.
#[allow(dead_code)]
pub fn unregister_device(dev_id: u32) {
    if dev_id == 0 || dev_id as usize >= MAX_REMOTE_BLOCK {
        log::warn!("[blk::remote] unregister_device: invalid dev_id={}", dev_id);
        return;
    }
    let mut g = REMOTE_BLOCK.lock();
    let slot = &mut g.entries[dev_id as usize];
    if slot.endpoint.is_none() {
        return;
    }
    log::info!(
        "[blk::remote] unregister_device: releasing dev_id={} ('{}')",
        dev_id,
        slot.device_name
    );
    *slot = RemoteBlockEntry::empty();
    REMOTE_BLOCK_REGISTERED_MASK.fetch_and(!(1u32 << dev_id), Ordering::Release);
}

/// Phase 106 C.3 — release the auto-adopted root slot after a failed
/// root mount and mark its service to be skipped next time.
///
/// Called by the `VFS_MOUNT_EXT2_ROOT` path when it finds no ext2 on the
/// device an auto-discovery service adopted into slot 0 (the classic
/// case: a blank internal NVMe present while booting a USB installer
/// image — NVMe out-priorities USB but has no filesystem). Clearing the
/// slot + skipping the service lets init's retry loop re-evaluate down
/// to the next candidate (AHCI, then the bootable USB) without a new
/// syscall. A no-op when slot 0 holds no auto-discovered remote service
/// (e.g. a virtio-blk root, or an explicitly-registered device). Returns
/// `true` when it released+skipped a service.
pub fn release_root_and_skip() -> bool {
    let mut g = REMOTE_BLOCK.lock();
    let slot = &mut g.entries[0];
    // Only auto-discovered root services participate; a `register`d root
    // or an empty slot is left alone.
    if slot.explicitly_registered {
        return false;
    }
    let bit = root_service_skip_bit(slot.service_name.as_str());
    if bit == 0 {
        return false;
    }
    log::info!(
        "[blk::remote] root mount found no ext2 on '{}' — releasing + skipping it",
        slot.service_name
    );
    *slot = RemoteBlockEntry::empty();
    REMOTE_BLOCK_REGISTERED.store(false, Ordering::Release);
    REMOTE_BLOCK_REGISTERED_MASK.fetch_and(!1u32, Ordering::Release);
    ROOT_SKIP_MASK.fetch_or(bit, Ordering::Release);
    true
}

/// `true` when `dev_id` is in-range and its slot is occupied.
#[allow(dead_code)]
pub fn is_registered_dev(dev_id: u32) -> bool {
    if dev_id as usize >= MAX_REMOTE_BLOCK {
        return false;
    }
    // Fast lock-free check first.
    if REMOTE_BLOCK_REGISTERED_MASK.load(Ordering::Acquire) & (1u32 << dev_id) == 0 {
        return false;
    }
    // Confirm under the lock (the mask is set before endpoint is `Some`, so
    // this is technically redundant, but keeps the semantics tight).
    REMOTE_BLOCK.lock().entries[dev_id as usize]
        .endpoint
        .is_some()
}

/// Read sectors from an additional device (dev_id >= 1).
#[allow(dead_code)]
pub fn read_sectors_dev(
    dev_id: u32,
    start_sector: u64,
    count: usize,
    buf: &mut [u8],
) -> Result<(), u8> {
    if dev_id as usize >= MAX_REMOTE_BLOCK {
        return Err(0xFF);
    }
    if count > MAX_SECTORS_PER_REQUEST as usize {
        return Err(0xFF);
    }
    if REMOTE_BLOCK.lock().entries[dev_id as usize]
        .state
        .is_restarting()
        && !wait_for_driver_restart_dev(dev_id)
    {
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    match do_read_ipc_dev(dev_id, start_sector, count, buf) {
        Ok(()) => Ok(()),
        // Live-driver status error — pass through, no restart dance (see
        // `read_sectors` / `restart_suspected`). The installer's capacity
        // probe legitimately reads past the end of the device; before this
        // guard, that single error reply wedged the whole `dev_id`.
        Err(e) if !restart_suspected(e) => Err(e),
        Err(_) => {
            on_ipc_error_dev(dev_id);
            if wait_for_driver_restart_dev(dev_id) {
                do_read_ipc_dev(dev_id, start_sector, count, buf)
            } else {
                Err(BlockDriverError::DriverRestarting.to_byte())
            }
        }
    }
}

/// Write sectors to an additional device (dev_id >= 1).
#[allow(dead_code)]
pub fn write_sectors_dev(
    dev_id: u32,
    start_sector: u64,
    count: usize,
    buf: &[u8],
) -> Result<(), u8> {
    if dev_id as usize >= MAX_REMOTE_BLOCK {
        return Err(0xFF);
    }
    if count > MAX_SECTORS_PER_REQUEST as usize {
        return Err(0xFF);
    }
    if REMOTE_BLOCK.lock().entries[dev_id as usize]
        .state
        .is_restarting()
        && !wait_for_driver_restart_dev(dev_id)
    {
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    // Pass payload_grant=0 (inline-bulk path) for dev writes.
    match do_write_ipc_dev(dev_id, start_sector, count, buf, 0, true) {
        Ok(()) => Ok(()),
        // Live-driver status error — pass through, no restart dance (see
        // `read_sectors` / `restart_suspected`).
        Err(e) if !restart_suspected(e) => Err(e),
        Err(_) => {
            on_ipc_error_dev(dev_id);
            if wait_for_driver_restart_dev(dev_id) {
                do_write_ipc_dev(dev_id, start_sector, count, buf, 0, false)
            } else {
                Err(BlockDriverError::DriverRestarting.to_byte())
            }
        }
    }
}

/// Flush an additional device's write-back cache.
#[allow(dead_code)]
pub fn flush_dev(dev_id: u32) -> Result<(), u8> {
    if dev_id as usize >= MAX_REMOTE_BLOCK {
        return Err(0xFF);
    }
    let (ep, task) = endpoint_and_task_dev(dev_id)?;
    let hdr = BlkRequestHeader {
        kind: BLK_FLUSH,
        cmd_id: 0,
        lba: 0,
        sector_count: 0,
        flags: 0,
    };
    let encoded = encode_blk_request(hdr, 0u32);
    scheduler::deliver_bulk(task, alloc::vec::Vec::from(encoded.as_slice()));
    let mut msg = Message::new(BLK_FLUSH as u64);
    msg.data[0] = 0;
    msg.data[1] = BLK_REQUEST_HEADER_SIZE as u64;
    let reply = endpoint::call_msg(task, ep, msg);
    if reply.label == u64::MAX {
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    let bulk = scheduler::take_bulk_data(task).ok_or(0xFFu8)?;
    let (reply_hdr, _) =
        decode_blk_reply(bulk.get(..BLK_REPLY_HEADER_SIZE).ok_or(0xFFu8)?).map_err(|_| 0xFFu8)?;
    if reply_hdr.status != BlockDriverError::Ok {
        return Err(reply_hdr.status.to_byte());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Root-slot I/O forwarding (unchanged from the singleton era)
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
    if REMOTE_BLOCK.lock().entries[0].state.is_restarting() && !wait_for_driver_restart() {
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    // Attempt the IPC call; on TRANSPORT failure wait + retry once. A
    // decoded device-status error (IoError / InvalidLba / ...) comes from a
    // LIVE driver and passes through — treating it as a restart signal
    // latches `is_restarting()` on a healthy driver forever (it never
    // re-registers), wedging every later request behind the full restart
    // budget (the Phase 106 C.4 capacity-probe whole-device wedge).
    match do_read_ipc(start_sector, count, buf) {
        Ok(()) => Ok(()),
        Err(e) if !restart_suspected(e) => Err(e),
        Err(_) => {
            on_ipc_error();
            if wait_for_driver_restart() {
                do_read_ipc(start_sector, count, buf)
            } else {
                Err(BlockDriverError::DriverRestarting.to_byte())
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
    // If the driver is already mid-restart on entry, wait for it first.
    // On timeout, surface DriverRestarting so the caller can distinguish
    // "driver still down" from a generic I/O error (Phase 55b Track F.2b).
    //
    // The grant is deliberately NOT consumed before this check: if the
    // driver is still down when the budget expires, no write ever
    // reached the wire, and burning the grant here would turn a
    // legitimate retry from the caller into a spurious GrantReplayed.
    // The grant is consumed in `do_write_ipc` immediately before the
    // IPC call so the single-use contract is still enforced against
    // concurrent writers racing on the same grant id.
    if REMOTE_BLOCK.lock().entries[0].state.is_restarting() && !wait_for_driver_restart() {
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    // Attempt the IPC call with the grant consumed as part of the first
    // attempt. On failure wait + retry once; the retry must NOT re-consume
    // the grant because (a) the tracker would now reject it as replayed
    // and (b) the first attempt has already claimed the single-use slot —
    // this is the same logical write retrying, not a fresh use.
    match do_write_ipc(start_sector, count, buf, payload_grant, true) {
        Ok(()) => Ok(()),
        // Live-driver status error — pass through, no restart dance (see
        // `read_sectors` / `restart_suspected`).
        Err(e) if !restart_suspected(e) => Err(e),
        Err(_) => {
            on_ipc_error();
            if wait_for_driver_restart() {
                do_write_ipc(start_sector, count, buf, payload_grant, false)
            } else {
                Err(BlockDriverError::DriverRestarting.to_byte())
            }
        }
    }
}

/// Inner IPC call for writes — no restart logic.
///
/// When `consume_grant` is `true`, the Phase 50 single-use grant contract
/// is enforced by calling [`GrantIdTracker::consume`] before building the
/// request. When `false`, the caller has already consumed the grant on an
/// earlier attempt and this is an internal restart retry — the grant id
/// is passed unchanged over the wire so the driver reassembles the same
/// logical write.
fn do_write_ipc(
    start_sector: u64,
    count: usize,
    buf: &[u8],
    payload_grant: u32,
    consume_grant: bool,
) -> Result<(), u8> {
    if consume_grant {
        let mut g = REMOTE_BLOCK.lock();
        match g.entries[0].grants.consume(payload_grant) {
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
    let bulk_len = bulk.len();
    scheduler::deliver_bulk(task, bulk);
    let mut msg = Message::new(BLK_WRITE as u64);
    msg.data[0] = start_sector;
    // `data[1]` is the bulk length the driver's `decode_recv_result` truncates
    // the received request to (same convention as `do_read_ipc` / `flush`).
    // Without it the write request — header + payload — was truncated to an
    // empty slice, so the ring-3 driver rejected every write-with-payload as a
    // malformed frame. Latent since Phase 55b: only the in-kernel virtio-blk
    // path (which does not use this IPC) and read-only AHCI/NVMe boots masked
    // it; no gate exercises a payload write over `blk::remote`.
    msg.data[1] = bulk_len as u64;
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

/// Ask the ring-3 driver to commit its volatile write-back cache to media
/// (`BLK_FLUSH`). Issued at the clean-shutdown boundary so a write-back driver's
/// buffered writes persist across a poweroff/restart. Best-effort: a driver that
/// is write-through (or does not implement `BLK_FLUSH`) may reply Ok or an error;
/// the caller (`blk::flush`) only logs a warning, never blocks shutdown. No
/// payload, no retry — shutdown is not the place to spin on a restart window.
pub fn flush() -> Result<(), u8> {
    let (ep, task) = endpoint_and_task()?;
    let hdr = BlkRequestHeader {
        kind: BLK_FLUSH,
        cmd_id: 0,
        lba: 0,
        sector_count: 0,
        flags: 0,
    };
    let encoded = encode_blk_request(hdr, 0u32);
    scheduler::deliver_bulk(task, alloc::vec::Vec::from(encoded.as_slice()));
    let mut msg = Message::new(BLK_FLUSH as u64);
    // `data[0]` is the per-request identifier (0 — flush has no LBA); `data[1]`
    // is the bulk length the driver's `decode_recv_result` truncates the
    // received request buffer to (the header-only payload, exactly the
    // `do_read_ipc` convention above). Putting the length in `data[0]` left
    // `data[1] = 0`, which truncated the flush request to an empty slice so the
    // driver rejected it as a malformed frame before the cache flush ran.
    msg.data[0] = 0;
    msg.data[1] = BLK_REQUEST_HEADER_SIZE as u64;
    let reply = endpoint::call_msg(task, ep, msg);
    if reply.label == u64::MAX {
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    let bulk = scheduler::take_bulk_data(task).ok_or(0xFFu8)?;
    let (reply_hdr, _) =
        decode_blk_reply(bulk.get(..BLK_REPLY_HEADER_SIZE).ok_or(0xFFu8)?).map_err(|_| 0xFFu8)?;
    if reply_hdr.status != BlockDriverError::Ok {
        return Err(reply_hdr.status.to_byte());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers — root slot (slot 0)
// ---------------------------------------------------------------------------

/// Snapshot the current endpoint + task ID for slot 0, or return `Err(0xFF)`.
fn endpoint_and_task() -> Result<(EndpointId, crate::task::TaskId), u8> {
    let g = REMOTE_BLOCK.lock();
    let ep = g.entries[0].endpoint.ok_or(0xFFu8)?;
    let task = scheduler::current_task_id().ok_or(0xFFu8)?;
    Ok((ep, task))
}

/// Mark the root driver mid-restart and emit one `driver.absent` warn.
fn on_ipc_error() {
    let mut g = REMOTE_BLOCK.lock();
    let slot = &mut g.entries[0];
    if !slot.state.is_restarting() {
        slot.state.mark_restarting();
        log::warn!(
            "[blk::remote] driver '{}' unreachable — marking mid-restart",
            slot.state.device_name().unwrap_or("<unknown>")
        );
    }
}

/// Driver-death detection hook fired by `cleanup_task_ipc` when an endpoint
/// owned by a dying task is closed.
///
/// Mirrors `RemoteNic::on_endpoint_closed`: latches `is_restarting()` on
/// driver process exit so consumers see the restart window immediately
/// instead of waiting for the next IPC call to fail. No-op when no driver
/// is registered or the closed endpoint is not the registered one.
///
/// Phase 92a: scans all occupied slots (not just slot 0) so a secondary USB
/// device endpoint closing is also caught.
pub fn on_endpoint_closed(ep_id: EndpointId) {
    // Fast-path: skip the mutex acquisition entirely when no driver is
    // registered. Mirrors `RemoteNic::on_endpoint_closed` so cleanup
    // overhead during boot and stop_service stays lock-free in the
    // no-driver configuration exercised by `serverization-fallback`.
    if REMOTE_BLOCK_REGISTERED_MASK.load(Ordering::Acquire) == 0 {
        return;
    }
    // Keep backward-compatible fast check for slot 0.
    if !REMOTE_BLOCK_REGISTERED.load(Ordering::Acquire) {
        // No root driver; only check secondary slots.
        let mut g = REMOTE_BLOCK.lock();
        for idx in 1..MAX_REMOTE_BLOCK {
            let slot = &mut g.entries[idx];
            if slot.endpoint == Some(ep_id) {
                log::warn!(
                    "[blk::remote] dev_id={} endpoint {:?} closed — marking mid-restart",
                    idx,
                    ep_id,
                );
                on_ipc_error_dev_inner(slot);
            }
        }
        return;
    }
    // Check root slot first (common case).
    {
        let root_ep = REMOTE_BLOCK.lock().entries[0].endpoint;
        if root_ep == Some(ep_id) {
            log::warn!(
                "[blk::remote] driver endpoint {:?} closed by owner exit — \
                 marking mid-restart",
                ep_id,
            );
            on_ipc_error();
            return;
        }
    }
    // Check secondary slots.
    let mut g = REMOTE_BLOCK.lock();
    for idx in 1..MAX_REMOTE_BLOCK {
        let slot = &mut g.entries[idx];
        if slot.endpoint == Some(ep_id) {
            log::warn!(
                "[blk::remote] dev_id={} endpoint {:?} closed — marking mid-restart",
                idx,
                ep_id,
            );
            on_ipc_error_dev_inner(slot);
        }
    }
}

/// Block up to `DRIVER_RESTART_TIMEOUT_MS` for the root driver to re-register.
///
/// Called when the driver is found mid-restart (either because `is_registered()`
/// was false, or because an IPC call returned a failure sentinel). The function
/// polls `is_restarting()` at each scheduler yield until either:
///
/// - The flag clears → returns `true` (caller should retry IPC).
/// - The budget expires → returns `false` (caller returns `DriverRestarting`).
///
/// The pure-logic [`WaitOutcome::Waiting`] variant never escapes this loop —
/// it is the keep-yielding signal, which is why callers only see a two-way
/// ready/timed-out decision. Folding that into a `bool` at this boundary makes
/// the call sites exhaustive without unreachable match arms.
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
fn wait_for_driver_restart() -> bool {
    // Snapshot the restart budget without holding the lock across yields.
    let budget_ms = {
        let g = REMOTE_BLOCK.lock();
        g.entries[0].state.restart_deadline_ms as u64
    };
    let start_tick = crate::arch::x86_64::interrupts::tick_count();
    let deadline_tick = start_tick.saturating_add(budget_ms);

    loop {
        let now_tick = crate::arch::x86_64::interrupts::tick_count();
        let is_ready = {
            let g = REMOTE_BLOCK.lock();
            !g.entries[0].state.is_restarting()
        };
        match BlockDispatchState::check_restart_wait(now_tick, deadline_tick, is_ready) {
            WaitOutcome::Ready => return true,
            WaitOutcome::TimedOut => return false,
            // Within budget, driver still absent: yield and retry.
            WaitOutcome::Waiting => {
                scheduler::yield_now();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers — secondary device slots (dev_id >= 1)
// ---------------------------------------------------------------------------

fn endpoint_and_task_dev(dev_id: u32) -> Result<(EndpointId, crate::task::TaskId), u8> {
    let g = REMOTE_BLOCK.lock();
    let ep = g.entries[dev_id as usize].endpoint.ok_or(0xFFu8)?;
    let task = scheduler::current_task_id().ok_or(0xFFu8)?;
    Ok((ep, task))
}

fn on_ipc_error_dev(dev_id: u32) {
    let mut g = REMOTE_BLOCK.lock();
    let slot = &mut g.entries[dev_id as usize];
    on_ipc_error_dev_inner(slot);
}

fn on_ipc_error_dev_inner(slot: &mut RemoteBlockEntry) {
    if !slot.state.is_restarting() {
        slot.state.mark_restarting();
        log::warn!(
            "[blk::remote] device '{}' unreachable — marking mid-restart",
            slot.state.device_name().unwrap_or("<unknown>")
        );
    }
}

fn wait_for_driver_restart_dev(dev_id: u32) -> bool {
    let budget_ms = {
        let g = REMOTE_BLOCK.lock();
        g.entries[dev_id as usize].state.restart_deadline_ms as u64
    };
    let start_tick = crate::arch::x86_64::interrupts::tick_count();
    let deadline_tick = start_tick.saturating_add(budget_ms);

    loop {
        let now_tick = crate::arch::x86_64::interrupts::tick_count();
        let is_ready = {
            let g = REMOTE_BLOCK.lock();
            !g.entries[dev_id as usize].state.is_restarting()
        };
        match BlockDispatchState::check_restart_wait(now_tick, deadline_tick, is_ready) {
            WaitOutcome::Ready => return true,
            WaitOutcome::TimedOut => return false,
            WaitOutcome::Waiting => {
                scheduler::yield_now();
            }
        }
    }
}

fn do_read_ipc_dev(dev_id: u32, start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    let (ep, task) = endpoint_and_task_dev(dev_id)?;
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
        on_ipc_error_dev(dev_id);
        return Err(BlockDriverError::DriverRestarting.to_byte());
    }
    let bulk = scheduler::take_bulk_data(task).ok_or(0xFFu8)?;
    let (reply_hdr, _) =
        decode_blk_reply(bulk.get(..BLK_REPLY_HEADER_SIZE).ok_or(0xFFu8)?).map_err(|_| 0xFFu8)?;
    if reply_hdr.status != BlockDriverError::Ok {
        return Err(reply_hdr.status.to_byte());
    }
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

fn do_write_ipc_dev(
    dev_id: u32,
    start_sector: u64,
    count: usize,
    buf: &[u8],
    payload_grant: u32,
    consume_grant: bool,
) -> Result<(), u8> {
    if consume_grant {
        let mut g = REMOTE_BLOCK.lock();
        match g.entries[dev_id as usize].grants.consume(payload_grant) {
            Ok(()) => {}
            Err(RemoteDeviceError::GrantReplayed) => {
                log::error!(
                    "[blk::remote] dev_id={} grant 0x{:08x} replayed — Phase 50 violation",
                    dev_id,
                    payload_grant
                );
                return Err(0xFF);
            }
            Err(_) => return Err(0xFF),
        }
    }
    let (ep, task) = endpoint_and_task_dev(dev_id)?;
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
    let bulk_len = bulk.len();
    scheduler::deliver_bulk(task, bulk);
    let mut msg = Message::new(BLK_WRITE as u64);
    msg.data[0] = start_sector;
    msg.data[1] = bulk_len as u64;
    let reply = endpoint::call_msg(task, ep, msg);
    if reply.label == u64::MAX {
        on_ipc_error_dev(dev_id);
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
