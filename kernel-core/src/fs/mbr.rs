//! MBR partition table parsing (Phase 24, Track C).
//!
//! Parses the Master Boot Record from sector 0 to locate FAT32 partitions.

/// A single MBR partition entry (16 bytes at offsets 446..510 of sector 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MbrPartitionEntry {
    pub status: u8,
    pub part_type: u8,
    pub lba_start: u32,
    pub sector_count: u32,
}

/// Errors from MBR parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbrError {
    /// The 0x55AA signature at bytes 510..512 is missing.
    BadSignature,
}

/// Parse the four MBR partition entries from a 512-byte sector.
///
/// Validates the 0x55AA boot signature at bytes 510..511.
pub fn parse_mbr(sector0: &[u8; 512]) -> Result<[MbrPartitionEntry; 4], MbrError> {
    // Validate signature.
    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Err(MbrError::BadSignature);
    }

    let mut entries = [MbrPartitionEntry {
        status: 0,
        part_type: 0,
        lba_start: 0,
        sector_count: 0,
    }; 4];

    for (i, entry) in entries.iter_mut().enumerate() {
        let base = 446 + i * 16;
        *entry = MbrPartitionEntry {
            status: sector0[base],
            part_type: sector0[base + 4],
            lba_start: u32::from_le_bytes([
                sector0[base + 8],
                sector0[base + 9],
                sector0[base + 10],
                sector0[base + 11],
            ]),
            sector_count: u32::from_le_bytes([
                sector0[base + 12],
                sector0[base + 13],
                sector0[base + 14],
                sector0[base + 15],
            ]),
        };
    }

    Ok(entries)
}

/// Find the first FAT32 partition (type 0x0B or 0x0C) and return its
/// `(lba_start, sector_count)`.
pub fn find_fat32_partition(entries: &[MbrPartitionEntry; 4]) -> Option<(u64, u64)> {
    for entry in entries {
        if (entry.part_type == 0x0B || entry.part_type == 0x0C) && entry.sector_count > 0 {
            return Some((entry.lba_start as u64, entry.sector_count as u64));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sector_with_entry(part_type: u8, lba_start: u32, sector_count: u32) -> [u8; 512] {
        let mut sector = [0u8; 512];

        // Partition entry 1 at offset 446.
        sector[446] = 0x80; // active
        sector[446 + 4] = part_type;
        sector[446 + 8..446 + 12].copy_from_slice(&lba_start.to_le_bytes());
        sector[446 + 12..446 + 16].copy_from_slice(&sector_count.to_le_bytes());

        // MBR signature.
        sector[510] = 0x55;
        sector[511] = 0xAA;

        sector
    }

    #[test]
    fn parse_valid_mbr() {
        let sector = make_sector_with_entry(0x0C, 2048, 129024);
        let entries = parse_mbr(&sector).unwrap();

        assert_eq!(entries[0].status, 0x80);
        assert_eq!(entries[0].part_type, 0x0C);
        assert_eq!(entries[0].lba_start, 2048);
        assert_eq!(entries[0].sector_count, 129024);

        // Other entries should be empty.
        assert_eq!(entries[1].part_type, 0);
        assert_eq!(entries[2].part_type, 0);
        assert_eq!(entries[3].part_type, 0);
    }

    #[test]
    fn bad_signature_returns_error() {
        let sector = [0u8; 512]; // no 0x55AA
        assert_eq!(parse_mbr(&sector), Err(MbrError::BadSignature));
    }

    #[test]
    fn find_fat32_partition_type_0c() {
        let sector = make_sector_with_entry(0x0C, 2048, 129024);
        let entries = parse_mbr(&sector).unwrap();
        let result = find_fat32_partition(&entries);
        assert_eq!(result, Some((2048, 129024)));
    }

    #[test]
    fn find_fat32_partition_type_0b() {
        let sector = make_sector_with_entry(0x0B, 63, 1000);
        let entries = parse_mbr(&sector).unwrap();
        let result = find_fat32_partition(&entries);
        assert_eq!(result, Some((63, 1000)));
    }

    #[test]
    fn no_fat32_partition_returns_none() {
        // Type 0x83 = Linux
        let sector = make_sector_with_entry(0x83, 2048, 129024);
        let entries = parse_mbr(&sector).unwrap();
        assert_eq!(find_fat32_partition(&entries), None);
    }

    #[test]
    fn empty_partition_table() {
        let mut sector = [0u8; 512];
        sector[510] = 0x55;
        sector[511] = 0xAA;
        let entries = parse_mbr(&sector).unwrap();
        assert_eq!(find_fat32_partition(&entries), None);
    }

    #[test]
    fn multiple_partitions_returns_first_fat32() {
        let mut sector = [0u8; 512];

        // Entry 0: Linux (0x83)
        sector[446 + 4] = 0x83;
        sector[446 + 8..446 + 12].copy_from_slice(&100u32.to_le_bytes());
        sector[446 + 12..446 + 16].copy_from_slice(&500u32.to_le_bytes());

        // Entry 1: FAT32 (0x0C)
        let base = 446 + 16;
        sector[base + 4] = 0x0C;
        sector[base + 8..base + 12].copy_from_slice(&2048u32.to_le_bytes());
        sector[base + 12..base + 16].copy_from_slice(&4096u32.to_le_bytes());

        sector[510] = 0x55;
        sector[511] = 0xAA;

        let entries = parse_mbr(&sector).unwrap();
        assert_eq!(find_fat32_partition(&entries), Some((2048, 4096)));
    }
}
