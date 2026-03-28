//! FAT32 on-disk structures and parsing (Phase 24, Track D).
//!
//! Pure parsing logic lives here (testable on the host). The kernel-side
//! `Fat32Volume` wires these to the virtio-blk driver for actual I/O.

/// FAT32 BIOS Parameter Block — parsed from the first sector of a partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fat32Bpb {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub total_sectors_32: u32,
    pub fat_size_32: u32,
    pub root_cluster: u32,
    pub fs_info_sector: u16,
}

/// A parsed 32-byte FAT32 directory entry.
#[derive(Debug, Clone, Copy)]
pub struct Fat32DirEntry {
    /// 8.3 name (11 bytes, space-padded).
    pub name: [u8; 11],
    pub attr: u8,
    pub cluster_hi: u16,
    pub cluster_lo: u16,
    pub file_size: u32,
}

impl Fat32DirEntry {
    /// The starting cluster number (combining hi and lo halves).
    pub fn start_cluster(&self) -> u32 {
        ((self.cluster_hi as u32) << 16) | (self.cluster_lo as u32)
    }

    /// Whether this entry is a directory.
    pub fn is_dir(&self) -> bool {
        self.attr & 0x10 != 0
    }

    /// Format the 8.3 name as a human-readable string (trimmed, with dot).
    pub fn name_str(&self, buf: &mut [u8; 13]) -> usize {
        let base = &self.name[..8];
        let ext = &self.name[8..11];

        let mut pos = 0;

        // Copy base name (trim trailing spaces).
        let base_len = base.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
        for &byte in base.iter().take(base_len) {
            buf[pos] = byte;
            pos += 1;
        }

        // Copy extension if present.
        let ext_len = ext.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
        if ext_len > 0 {
            buf[pos] = b'.';
            pos += 1;
            for &byte in ext.iter().take(ext_len) {
                buf[pos] = byte;
                pos += 1;
            }
        }

        pos
    }
}

/// Errors from FAT32 operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fat32Error {
    /// BPB signature (0xAA55 at offset 510) is invalid.
    BadSignature,
    /// Unsupported bytes_per_sector (must be 512).
    UnsupportedSectorSize,
    /// Block device I/O error.
    IoError,
    /// File or directory not found.
    NotFound,
    /// Disk is full (no free clusters).
    DiskFull,
    /// Directory is full (no free entry slots).
    DirFull,
    /// Attempted to delete a non-empty directory.
    DirNotEmpty,
    /// Chain length limit exceeded (likely corruption).
    ChainTooLong,
    /// Invalid cluster number encountered.
    InvalidCluster,
}

/// Parse a FAT32 BPB from the first 512 bytes of a partition.
pub fn parse_bpb(sector: &[u8; 512]) -> Result<Fat32Bpb, Fat32Error> {
    // Validate boot signature at offset 510.
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return Err(Fat32Error::BadSignature);
    }

    let bytes_per_sector = u16::from_le_bytes([sector[11], sector[12]]);
    if bytes_per_sector != 512 {
        return Err(Fat32Error::UnsupportedSectorSize);
    }

    let sectors_per_cluster = sector[13];
    if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
        return Err(Fat32Error::UnsupportedSectorSize);
    }
    let reserved_sectors = u16::from_le_bytes([sector[14], sector[15]]);
    let num_fats = sector[16];

    let total_sectors_32 = u32::from_le_bytes([sector[32], sector[33], sector[34], sector[35]]);
    let fat_size_32 = u32::from_le_bytes([sector[36], sector[37], sector[38], sector[39]]);
    let root_cluster = u32::from_le_bytes([sector[44], sector[45], sector[46], sector[47]]);
    let fs_info_sector = u16::from_le_bytes([sector[48], sector[49]]);

    Ok(Fat32Bpb {
        bytes_per_sector,
        sectors_per_cluster,
        reserved_sectors,
        num_fats,
        total_sectors_32,
        fat_size_32,
        root_cluster,
        fs_info_sector,
    })
}

/// Parse a 32-byte FAT32 directory entry from raw bytes.
///
/// Returns `None` for deleted entries (0xE5), LFN entries (attr 0x0F),
/// or the end-of-directory marker (0x00).
pub fn parse_dir_entry(raw: &[u8; 32]) -> Option<Fat32DirEntry> {
    // End-of-directory marker.
    if raw[0] == 0x00 {
        return None;
    }
    // Deleted entry.
    if raw[0] == 0xE5 {
        return None;
    }
    // LFN entry.
    let attr = raw[11];
    if attr == 0x0F {
        return None;
    }

    let mut name = [0u8; 11];
    name.copy_from_slice(&raw[..11]);

    let cluster_hi = u16::from_le_bytes([raw[20], raw[21]]);
    let cluster_lo = u16::from_le_bytes([raw[26], raw[27]]);
    let file_size = u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]);

    Some(Fat32DirEntry {
        name,
        attr,
        cluster_hi,
        cluster_lo,
        file_size,
    })
}

/// Format a filename as 8.3 (space-padded, uppercase).
///
/// Returns a fixed 11-byte array suitable for a FAT32 directory entry name field.
pub fn format_8_3(name: &str) -> [u8; 11] {
    let mut result = [b' '; 11];
    let name_upper: alloc::vec::Vec<u8> = name.bytes().map(|b| b.to_ascii_uppercase()).collect();

    if let Some(dot_pos) = name_upper.iter().position(|&b| b == b'.') {
        let base = &name_upper[..dot_pos];
        let ext = &name_upper[dot_pos + 1..];
        let base_len = base.len().min(8);
        result[..base_len].copy_from_slice(&base[..base_len]);
        let ext_len = ext.len().min(3);
        result[8..8 + ext_len].copy_from_slice(&ext[..ext_len]);
    } else {
        let len = name_upper.len().min(8);
        result[..len].copy_from_slice(&name_upper[..len]);
    }

    result
}

/// Compare a directory entry's 8.3 name against a filename (case-insensitive).
pub fn name_matches(entry_name: &[u8; 11], filename: &str) -> bool {
    let formatted = format_8_3(filename);
    entry_name
        .iter()
        .zip(formatted.iter())
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// FAT entry end-of-chain marker threshold.
pub const FAT_EOC: u32 = 0x0FFF_FFF8;

/// Mask for the 28 data bits of a FAT32 entry.
pub const FAT_ENTRY_MASK: u32 = 0x0FFF_FFFF;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bpb_sector(
        bytes_per_sector: u16,
        sectors_per_cluster: u8,
        reserved_sectors: u16,
        num_fats: u8,
        total_sectors_32: u32,
        fat_size_32: u32,
        root_cluster: u32,
    ) -> [u8; 512] {
        let mut s = [0u8; 512];
        s[11..13].copy_from_slice(&bytes_per_sector.to_le_bytes());
        s[13] = sectors_per_cluster;
        s[14..16].copy_from_slice(&reserved_sectors.to_le_bytes());
        s[16] = num_fats;
        s[32..36].copy_from_slice(&total_sectors_32.to_le_bytes());
        s[36..40].copy_from_slice(&fat_size_32.to_le_bytes());
        s[44..48].copy_from_slice(&root_cluster.to_le_bytes());
        s[48..50].copy_from_slice(&1u16.to_le_bytes()); // fs_info_sector
                                                        // "FAT32   " at offset 82 (optional but common).
        s[82..90].copy_from_slice(b"FAT32   ");
        // Boot signature.
        s[510] = 0x55;
        s[511] = 0xAA;
        s
    }

    #[test]
    fn parse_valid_bpb() {
        let s = make_bpb_sector(512, 8, 32, 2, 131072, 128, 2);
        let bpb = parse_bpb(&s).unwrap();
        assert_eq!(bpb.bytes_per_sector, 512);
        assert_eq!(bpb.sectors_per_cluster, 8);
        assert_eq!(bpb.reserved_sectors, 32);
        assert_eq!(bpb.num_fats, 2);
        assert_eq!(bpb.total_sectors_32, 131072);
        assert_eq!(bpb.fat_size_32, 128);
        assert_eq!(bpb.root_cluster, 2);
        assert_eq!(bpb.fs_info_sector, 1);
    }

    #[test]
    fn bad_signature() {
        let s = [0u8; 512];
        assert_eq!(parse_bpb(&s), Err(Fat32Error::BadSignature));
    }

    #[test]
    fn unsupported_sector_size() {
        let s = make_bpb_sector(1024, 8, 32, 2, 131072, 128, 2);
        assert_eq!(parse_bpb(&s), Err(Fat32Error::UnsupportedSectorSize));
    }

    #[test]
    fn parse_valid_dir_entry() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"HELLO   TXT");
        raw[11] = 0x20; // ARCHIVE
        raw[20..22].copy_from_slice(&0u16.to_le_bytes()); // cluster_hi
        raw[26..28].copy_from_slice(&5u16.to_le_bytes()); // cluster_lo
        raw[28..32].copy_from_slice(&1234u32.to_le_bytes()); // file_size

        let entry = parse_dir_entry(&raw).unwrap();
        assert_eq!(&entry.name, b"HELLO   TXT");
        assert_eq!(entry.attr, 0x20);
        assert_eq!(entry.start_cluster(), 5);
        assert_eq!(entry.file_size, 1234);
        assert!(!entry.is_dir());
    }

    #[test]
    fn parse_dir_entry_directory() {
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(b"SUBDIR     ");
        raw[11] = 0x10; // DIRECTORY
        raw[20..22].copy_from_slice(&0u16.to_le_bytes());
        raw[26..28].copy_from_slice(&10u16.to_le_bytes());

        let entry = parse_dir_entry(&raw).unwrap();
        assert!(entry.is_dir());
        assert_eq!(entry.start_cluster(), 10);
    }

    #[test]
    fn skip_deleted_entry() {
        let mut raw = [0u8; 32];
        raw[0] = 0xE5;
        assert!(parse_dir_entry(&raw).is_none());
    }

    #[test]
    fn skip_lfn_entry() {
        let mut raw = [0u8; 32];
        raw[0] = b'A';
        raw[11] = 0x0F; // LFN
        assert!(parse_dir_entry(&raw).is_none());
    }

    #[test]
    fn end_of_directory() {
        let raw = [0u8; 32];
        assert!(parse_dir_entry(&raw).is_none());
    }

    #[test]
    fn format_8_3_simple() {
        assert_eq!(&format_8_3("hello.txt"), b"HELLO   TXT");
    }

    #[test]
    fn format_8_3_no_extension() {
        assert_eq!(&format_8_3("readme"), b"README     ");
    }

    #[test]
    fn format_8_3_short_ext() {
        assert_eq!(&format_8_3("data.c"), b"DATA    C  ");
    }

    #[test]
    fn name_matches_case_insensitive() {
        let entry_name = b"HELLO   TXT";
        assert!(name_matches(entry_name, "hello.txt"));
        assert!(name_matches(entry_name, "HELLO.TXT"));
        assert!(!name_matches(entry_name, "world.txt"));
    }

    #[test]
    fn dir_entry_name_str() {
        let entry = Fat32DirEntry {
            name: *b"HELLO   TXT",
            attr: 0x20,
            cluster_hi: 0,
            cluster_lo: 5,
            file_size: 100,
        };
        let mut buf = [0u8; 13];
        let len = entry.name_str(&mut buf);
        assert_eq!(&buf[..len], b"HELLO.TXT");
    }

    #[test]
    fn dir_entry_name_str_no_ext() {
        let entry = Fat32DirEntry {
            name: *b"README     ",
            attr: 0x20,
            cluster_hi: 0,
            cluster_lo: 3,
            file_size: 50,
        };
        let mut buf = [0u8; 13];
        let len = entry.name_str(&mut buf);
        assert_eq!(&buf[..len], b"README");
    }

    #[test]
    fn high_cluster_number() {
        let entry = Fat32DirEntry {
            name: *b"BIG     DAT",
            attr: 0x20,
            cluster_hi: 0x0012,
            cluster_lo: 0x3456,
            file_size: 999,
        };
        assert_eq!(entry.start_cluster(), 0x0012_3456);
    }
}
