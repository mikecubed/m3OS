//! Block device subsystem — Phase 24.
//!
//! Provides a virtio-blk driver for reading and writing disk sectors,
//! and MBR partition parsing. Phase 55 (D track) adds an NVMe driver on the
//! same hardware-access layer; the single-device dispatch path is decided
//! in [`init`].

pub mod mbr;
pub mod nvme;
pub mod virtio_blk;

#[allow(unused_imports)]
pub use virtio_blk::{VIRTIO_BLK_READY, read_sectors, write_sectors};

/// Initialize block subsystem: register known drivers and run a probe pass
/// so whichever controller is present can bind. Both virtio-blk and nvme
/// call `probe_all_drivers`; the second pass is a no-op because the first
/// already bound every matching device.
pub fn init() {
    virtio_blk::register();
    nvme::register();
    crate::pci::probe_all_drivers();
}
