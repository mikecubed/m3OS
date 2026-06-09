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

/// Phase 92 Track A — per-boot block-request counters on the kernel↔driver
/// round-trip. Every `read_sectors`/`write_sectors` call (one ring0↔ring3 IPC
/// round-trip to the ring-3 block driver, or one VirtIO-blk request) increments
/// the call counter; the sector counters track the bytes moved. These are the
/// measurement the Phase 92 batching work is judged against — they make the
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
    // Phase 92 Track A — count the round-trip + sectors moved (one call == one
    // kernel↔driver request, the cost the batching work collapses).
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
    // Phase 92 Track A — count the round-trip + sectors moved.
    BLK_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
    BLK_WRITE_SECTORS.fetch_add(count as u64, Ordering::Relaxed);
    if remote::is_registered() {
        // No caller-supplied grant when writing through the legacy API — pass
        // `0` so the facade encodes "no separate grant payload" and embeds the
        // write data inline in the bulk buffer instead.
        return remote::write_sectors(start_sector, count, buf, 0);
    }
    virtio_blk::write_sectors(start_sector, count, buf)
}
