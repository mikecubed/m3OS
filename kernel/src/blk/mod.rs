//! Block device subsystem — Phase 24.
//!
//! Provides a virtio-blk driver for reading and writing disk sectors,
//! and MBR partition parsing.

pub mod mbr;
pub mod virtio_blk;

#[allow(unused_imports)]
pub use virtio_blk::{init, read_sectors, write_sectors, VIRTIO_BLK_READY};
