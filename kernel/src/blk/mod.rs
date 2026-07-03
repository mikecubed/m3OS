//! Block device subsystem — Phase 24.
//!
//! Provides a virtio-blk driver for reading and writing disk sectors,
//! and MBR partition parsing.
//!
//! # Dispatch priority (Phase 55b)
//!
//! Phase 55b Track D.4 added `remote::RemoteBlockDevice`, a kernel-side
//! forwarding facade that speaks to the ring-3 NVMe driver process over IPC.
//! Track D.5 deleted the in-kernel NVMe driver (`kernel/src/blk/nvme.rs`).
//! The dispatch policy in [`read_sectors`] / [`write_sectors`] is:
//!
//!   1. **`RemoteBlockDevice`** — if `remote::register` has been called, all
//!      block I/O is forwarded to the ring-3 NVMe driver via IPC.
//!      Cross-reference: `userspace/drivers/nvme/` owns all device-specific
//!      NVMe logic; `kernel_core::nvme` retains shared register/command types.
//!   2. **VirtIO-blk** — if no remote driver is registered.
//!
//! The pure-logic dispatch state machine lives in
//! `kernel_core::driver_ipc::blk_dispatch` where it is host-testable.

pub mod mbr;
pub mod remote;
pub mod virtio_blk;

#[allow(unused_imports)]
pub use virtio_blk::VIRTIO_BLK_READY;

use core::sync::atomic::{AtomicU64, Ordering};

/// Phase 87 Track A — per-boot block-request counters on the kernel↔driver
/// round-trip. Every `read_sectors`/`write_sectors` call (one ring0↔ring3 IPC
/// round-trip to the ring-3 block driver, or one VirtIO-blk request) increments
/// the call counter; the sector counters track the bytes moved. These are the
/// measurement the Phase 87 batching work is judged against — they make the
/// "21 MiB → ≤512 requests" acceptance falsifiable, and prove a batched read
/// actually collapsed the round-trips. Compiled in unconditionally (cheap
/// relaxed atomics), so the regression gate works on a release image. Read via
/// `/proc/blkstats`.
pub static BLK_READ_CALLS: AtomicU64 = AtomicU64::new(0);
pub static BLK_READ_SECTORS: AtomicU64 = AtomicU64::new(0);
pub static BLK_WRITE_CALLS: AtomicU64 = AtomicU64::new(0);
pub static BLK_WRITE_SECTORS: AtomicU64 = AtomicU64::new(0);

/// A snapshot of the four counters, in (read_calls, read_sectors, write_calls,
/// write_sectors) order. Used by `/proc/blkstats` and the one-shot probe.
pub fn blkstats_snapshot() -> (u64, u64, u64, u64) {
    (
        BLK_READ_CALLS.load(Ordering::Relaxed),
        BLK_READ_SECTORS.load(Ordering::Relaxed),
        BLK_WRITE_CALLS.load(Ordering::Relaxed),
        BLK_WRITE_SECTORS.load(Ordering::Relaxed),
    )
}

/// Initialize the block subsystem: register every known driver with the
/// PCI HAL and run a probe pass so whichever controller is present binds.
pub fn init() {
    virtio_blk::register();
    crate::pci::probe_all_drivers();
}

/// Read `count` sectors starting at `start_sector` into `buf`.
///
/// Dispatch order:
/// 1. `RemoteBlockDevice` (ring-3 NVMe driver via IPC) if registered.
/// 2. VirtIO-blk otherwise.
///
/// Returns `Ok(())` on success or `Err(u8)` with a status byte on failure.
/// The VirtIO-blk surface returns a byte natively; remote errors are
/// truncated to their low 8 bits (most codes live there; full status is logged
/// by the driver).
#[allow(dead_code)]
pub fn read_sectors(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    // Phase 87 Track A — count the blk-layer dispatch call + sectors moved. This
    // deliberately counts one per `read_sectors` invocation (the kernel↔driver
    // round-trip the ext2/vfs coalescing collapses), NOT device-level requests:
    // the VirtIO backend fans a single call out to `count` per-sector requests
    // internally, but that is below the layer Phase 87 optimizes — counting it
    // here would make coalescing N one-sector calls into one N-sector call show no
    // gain. `BLK_READ_SECTORS` records the sector volume, so the per-call vs
    // per-sector dimensions stay separable regardless of backend.
    BLK_READ_CALLS.fetch_add(1, Ordering::Relaxed);
    BLK_READ_SECTORS.fetch_add(count as u64, Ordering::Relaxed);
    if remote::is_registered() {
        return remote::read_sectors(start_sector, count, buf);
    }
    virtio_blk::read_sectors(start_sector, count, buf)
}

/// Write `count` sectors starting at `start_sector` from `buf`.
///
/// Dispatch order mirrors [`read_sectors`].
///
/// This legacy API does not expose any caller-supplied IPC grant handle.
/// When writes are forwarded to `RemoteBlockDevice`, the facade encodes
/// "no separate grant payload" and embeds the write data inline in the
/// bulk buffer.
#[allow(dead_code)]
pub fn write_sectors(start_sector: u64, count: usize, buf: &[u8]) -> Result<(), u8> {
    // Phase 87 Track A — count the blk-layer dispatch call + sectors moved (see
    // `read_sectors`: one per invocation — the round-trip coalescing collapses,
    // NOT device-level requests, which the VirtIO backend fans out per sector).
    BLK_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
    BLK_WRITE_SECTORS.fetch_add(count as u64, Ordering::Relaxed);
    // Phase 89: backstop the kernel path-metadata (stat) cache. The AUTHORITATIVE
    // bump for vfs-routed mutations is the syscall layer's `ext2::invalidate_cache`
    // (called after every routed write/create/unlink/rename/setattr); this is
    // defense-in-depth for the KERNEL ext2 engine, whose every block write (data,
    // inode table, bitmaps, sb/BGD) funnels through this single choke point — so a
    // direct-engine fallback mutation (boot window) or any future kernel-side
    // mutation path that forgets to invalidate explicitly is still caught when it
    // reaches the disk. `bump()` is a lock-free atomic increment (no map lock),
    // safe to call from here in any context.
    crate::fs::metacache::bump();
    if remote::is_registered() {
        // No caller-supplied grant when writing through the legacy API — pass
        // `0` so the facade encodes "no separate grant payload" and embeds the
        // write data inline in the bulk buffer instead.
        return remote::write_sectors(start_sector, count, buf, 0);
    }
    virtio_blk::write_sectors(start_sector, count, buf)
}

/// Commit any device write-back cache to media. Called at clean shutdown
/// (`kernel_shutdown`) so buffered writes persist across a poweroff/restart.
///
/// Flushes whichever block backend is active: the ring-3 `RemoteBlockDevice`
/// (NVMe/AHCI) via a `BLK_FLUSH` IPC when one is registered, otherwise the
/// in-kernel virtio-blk device (which self-guards — a no-op unless
/// `VIRTIO_BLK_F_FLUSH` was negotiated and the device is ready). Best-effort: a
/// flush failure is logged, never fatal, so it cannot wedge shutdown.
#[allow(dead_code)]
pub fn flush() {
    if remote::is_registered() {
        if let Err(status) = remote::flush() {
            log::warn!(
                "[blk] remote block flush failed (status {status}) — buffered writes may be lost"
            );
        }
        return;
    }
    if let Err(status) = virtio_blk::flush() {
        log::warn!("[blk] virtio-blk flush failed (status {status}) — buffered writes may be lost");
    }
}

// ---------------------------------------------------------------------------
// Phase 92a D.4 — secondary device (dev_id >= 1) forwarding surface
// ---------------------------------------------------------------------------

/// Register an additional remote block device by service name (e.g.
/// `"usb0.block"`). Returns the assigned `dev_id` (1-based), or `None` if
/// the registry is full or the service is unknown / untrusted.
///
/// This is the coordinator entry point — the USB mass-storage daemon
/// publishes its endpoint then calls this (or the coordinator calls it)
/// after confirming the device is ready.
#[allow(dead_code)]
pub fn register_remote_device(service_name: &str, device_name: &str) -> Option<u32> {
    remote::register_device(service_name, device_name)
}

/// Release a secondary device slot on hot-unplug.
///
/// `dev_id` must be >= 1 (the root slot is never released via this path).
#[allow(dead_code)]
pub fn unregister_remote_device(dev_id: u32) {
    remote::unregister_device(dev_id);
}

/// `true` when `dev_id` is in-range and its slot holds a live driver.
#[allow(dead_code)]
pub fn is_remote_device_registered(dev_id: u32) -> bool {
    remote::is_registered_dev(dev_id)
}

/// Read `count` sectors from the device identified by `dev_id`.
///
/// `dev_id` must be the value returned by [`register_remote_device`]. Passing
/// `dev_id=0` here is intentionally unsupported — callers that need the root
/// device use [`read_sectors`] directly. Returns `Err(0xFF)` for an
/// out-of-range or unregistered `dev_id`.
#[allow(dead_code)]
pub fn read_sectors_dev(
    dev_id: u32,
    start_sector: u64,
    count: usize,
    buf: &mut [u8],
) -> Result<(), u8> {
    remote::read_sectors_dev(dev_id, start_sector, count, buf)
}

/// Write `count` sectors to the device identified by `dev_id`.
///
/// Same preconditions as [`read_sectors_dev`].
#[allow(dead_code)]
pub fn write_sectors_dev(
    dev_id: u32,
    start_sector: u64,
    count: usize,
    buf: &[u8],
) -> Result<(), u8> {
    remote::write_sectors_dev(dev_id, start_sector, count, buf)
}

/// Flush the write-back cache of the device identified by `dev_id`.
///
/// Same preconditions as [`read_sectors_dev`]. Best-effort: a failure is
/// returned as `Err(u8)` so the caller can decide whether to log or ignore.
#[allow(dead_code)]
pub fn flush_dev(dev_id: u32) -> Result<(), u8> {
    remote::flush_dev(dev_id)
}

/// Phase 106 C.3 — release the auto-adopted root slot after a failed
/// root mount and skip its service next time (see
/// [`remote::release_root_and_skip`]). Returns `true` when a service was
/// released.
pub fn release_root_and_skip() -> bool {
    remote::release_root_and_skip()
}
