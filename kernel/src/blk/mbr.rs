//! MBR partition probing via virtio-blk (Phase 24, Track C).
//!
//! Reads sector 0, parses the MBR, and returns the FAT32 partition location.

use alloc::vec;
use kernel_core::fs::mbr;

/// Probe the virtio-blk device for an MBR partition table and return the
/// first FAT32 partition's `(lba_start, sector_count)`.
pub fn probe() -> Option<(u64, u64)> {
    if !super::virtio_blk::VIRTIO_BLK_READY.load(core::sync::atomic::Ordering::Acquire) {
        log::warn!("[mbr] virtio-blk not ready");
        return None;
    }

    let mut sector0 = vec![0u8; 512];
    if let Err(status) = super::virtio_blk::read_sectors(0, 1, &mut sector0) {
        log::error!("[mbr] failed to read sector 0: status {}", status);
        return None;
    }

    let sector_array: &[u8; 512] = sector0.as_slice().try_into().ok()?;

    let entries = match mbr::parse_mbr(sector_array) {
        Ok(e) => e,
        Err(e) => {
            log::error!("[mbr] failed to parse MBR: {:?}", e);
            return None;
        }
    };

    let result = mbr::find_fat32_partition(&entries);
    if let Some((lba, count)) = result {
        log::info!(
            "[mbr] FAT32 partition found: LBA {} + {} sectors ({} MiB)",
            lba,
            count,
            (count * 512) / (1024 * 1024)
        );
    } else {
        log::warn!("[mbr] no FAT32 partition found");
    }

    result
}
