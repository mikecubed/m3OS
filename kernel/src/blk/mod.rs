//! Block device subsystem — Phase 24.
//!
//! Provides a virtio-blk driver for reading and writing disk sectors,
//! and MBR partition parsing. Phase 55 Track D adds an NVMe driver on the
//! same hardware-access layer; [`read_sectors`] / [`write_sectors`] below
//! dispatch between the two based on which is ready. The policy is
//! deliberately simple for Phase 55: NVMe wins if present, otherwise
//! virtio-blk. A proper multi-device block layer is a later phase.

pub mod mbr;
pub mod nvme;
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

/// Read `count` sectors starting at `start_sector` into `buf`. Dispatches
/// to the best available block device — NVMe when `NVME_READY`, virtio-blk
/// otherwise.
///
/// Returns a `u8` status for backwards compatibility with the virtio-blk
/// surface — virtio-blk always fits in a byte and NVMe status codes are
/// truncated to their low 8 bits (most errors are in that range; the full
/// status is logged at error level by the NVMe driver).
#[allow(dead_code)]
pub fn read_sectors(start_sector: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    if nvme::NVME_READY.load(Ordering::Acquire) {
        nvme::read_sectors(start_sector, count, buf).map_err(|e| (e & 0xFF) as u8)
    } else {
        virtio_blk::read_sectors(start_sector, count, buf)
    }
}

/// Write `count` sectors starting at `start_sector` from `buf`. Dispatches
/// identically to [`read_sectors`].
#[allow(dead_code)]
pub fn write_sectors(start_sector: u64, count: usize, buf: &[u8]) -> Result<(), u8> {
    if nvme::NVME_READY.load(Ordering::Acquire) {
        nvme::write_sectors(start_sector, count, buf).map_err(|e| (e & 0xFF) as u8)
    } else {
        virtio_blk::write_sectors(start_sector, count, buf)
    }
}
