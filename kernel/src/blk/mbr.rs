//! MBR partition probing via virtio-blk (Phase 24 / Phase 28).
//!
//! Reads sector 0, parses the MBR, and returns partition locations.

use alloc::vec;
use kernel_core::fs::mbr;

/// Read and parse the MBR partition table from sector 0.
///
/// Reads through the [`super::read_sectors`] facade, which routes to a
/// registered ring-3 block driver (`nvme.block` / `ahci.block`) when one is
/// installed and `virtio-blk` is not serving the root, and otherwise to the
/// in-kernel virtio-blk driver. This lets the ext2-root mount probe a SATA disk
/// driven by the ring-3 `ahci_driver` (Phase 82 D.3) with no separate
/// partition-table code — the same facade the ext2 data path already uses. When
/// neither back-end is ready, the read returns an error and the probe is a
/// no-op (the caller logs "no ext2/FAT32 partition found").
fn read_mbr() -> Option<[mbr::MbrPartitionEntry; 4]> {
    if !super::virtio_blk::VIRTIO_BLK_READY.load(core::sync::atomic::Ordering::Acquire)
        && !super::remote::is_registered()
    {
        log::warn!("[mbr] no block back-end ready (virtio-blk down, no ring-3 driver registered)");
        return None;
    }

    let mut sector0 = vec![0u8; 512];
    if let Err(status) = super::read_sectors(0, 1, &mut sector0) {
        log::error!("[mbr] failed to read sector 0: status {}", status);
        return None;
    }

    let sector_array: &[u8; 512] = sector0.as_slice().try_into().ok()?;
    match mbr::parse_mbr(sector_array) {
        Ok(e) => Some(e),
        Err(e) => {
            log::error!("[mbr] failed to parse MBR: {:?}", e);
            None
        }
    }
}

/// Probe for the first FAT32 partition (type 0x0B/0x0C).
pub fn probe() -> Option<(u64, u64)> {
    let entries = read_mbr()?;
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

/// Probe for the first ext2/Linux partition (type 0x83).
pub fn probe_ext2() -> Option<(u64, u64)> {
    let entries = read_mbr()?;
    let result = mbr::find_ext2_partition(&entries);
    if let Some((lba, count)) = result {
        log::info!(
            "[mbr] ext2 partition found: LBA {} + {} sectors ({} MiB)",
            lba,
            count,
            (count * 512) / (1024 * 1024)
        );
    } else {
        log::warn!("[mbr] no ext2 partition found");
    }
    result
}
