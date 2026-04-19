//! Block device subsystem — Phase 24.
//!
//! Provides a virtio-blk driver for reading and writing disk sectors,
//! and MBR partition parsing.
//!
//! # Dispatch priority (Phase 55b)
//!
//! Phase 55b Track D.4 adds `remote::RemoteBlockDevice`, a kernel-side
//! forwarding facade that speaks to the ring-3 NVMe driver process over IPC.
//! The dispatch policy in [`read_sectors`] / [`write_sectors`] is:
//!
//!   1. **`RemoteBlockDevice`** — if `remote::register` has been called, all
//!      block I/O is forwarded to the ring-3 NVMe driver via IPC.
//!   2. **VirtIO-blk** — if no remote driver is registered.
//!
//! This matches the Phase 55 priority (NVMe beats VirtIO) with the in-kernel
//! NVMe driver replaced by the ring-3 facade. Track D.5 will delete
//! `kernel/src/blk/nvme.rs`; until then the NVMe module is still compiled but
//! its probe path is the secondary fallback only when `RemoteBlockDevice` is
//! not registered.
//!
//! The pure-logic dispatch state machine lives in
//! `kernel_core::driver_ipc::blk_dispatch` where it is host-testable.

pub mod mbr;
pub mod nvme;
pub mod remote;
pub mod virtio_blk;

use core::sync::atomic::Ordering;

#[allow(unused_imports)]
pub use virtio_blk::VIRTIO_BLK_READY;

/// Initialize the block subsystem: register every known driver with the
/// PCI HAL and run a probe pass so whichever controller is present binds.
pub fn init() {
    virtio_blk::register();
    nvme::register();
    crate::pci::probe_all_drivers();
}

/// Read `count` sectors starting at `start_sector` into `buf`.
///
/// Dispatch order:
/// 1. `RemoteBlockDevice` (ring-3 NVMe driver via IPC) if registered.
/// 2. In-kernel NVMe driver if `NVME_READY`.
/// 3. VirtIO-blk otherwise.
///
/// Returns `Ok(())` on success or `Err(u8)` with a status byte on failure.
/// The VirtIO-blk surface returns a byte natively; NVMe and remote errors are
/// truncated to their low 8 bits (most codes live there; full status is logged
/// by the driver).
#[allow(dead_code)]
pub fn read_sectors(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    if remote::is_registered() {
        return remote::read_sectors(start_sector, count, buf);
    }
    if nvme::NVME_READY.load(Ordering::Acquire) {
        nvme::read_sectors(start_sector, count, buf).map_err(|e| (e & 0xFF) as u8)
    } else {
        virtio_blk::read_sectors(start_sector, count, buf)
    }
}

/// Write `count` sectors starting at `start_sector` from `buf`.
///
/// Dispatch order mirrors [`read_sectors`]. For remote writes `payload_grant`
/// is the IPC capability grant handle carrying the bulk write data (pass `0`
/// when the caller does not use the grant path; the facade will encode it
/// accordingly). For the in-kernel paths the grant is unused.
#[allow(dead_code)]
pub fn write_sectors(start_sector: u64, count: usize, buf: &[u8]) -> Result<(), u8> {
    if remote::is_registered() {
        // No caller-supplied grant when writing through the legacy API — pass
        // `0` so the facade encodes "no separate grant payload" and embeds the
        // write data inline in the bulk buffer instead.
        return remote::write_sectors(start_sector, count, buf, 0);
    }
    if nvme::NVME_READY.load(Ordering::Acquire) {
        nvme::write_sectors(start_sector, count, buf).map_err(|e| (e & 0xFF) as u8)
    } else {
        virtio_blk::write_sectors(start_sector, count, buf)
    }
}
