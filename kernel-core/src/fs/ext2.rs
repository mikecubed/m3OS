//! ext2 on-disk structures and parsing (Phase 28, Track A).
//!
//! Pure parsing logic lives here (testable on the host). The kernel-side
//! `Ext2Volume` wires these to the virtio-blk driver for actual I/O.

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::string::String;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
#[cfg(feature = "std")]
use std::string::String;
#[cfg(feature = "std")]
use std::vec::Vec;

/// ext2 superblock magic number.
pub const EXT2_MAGIC: u16 = 0xEF53;

/// Inode number of the root directory (always 2 in ext2).
pub const EXT2_ROOT_INO: u32 = 2;

/// Number of direct block pointers in an inode.
pub const EXT2_NDIR_BLOCKS: usize = 12;
/// Index of single-indirect block pointer.
pub const EXT2_IND_BLOCK: usize = 12;
/// Index of double-indirect block pointer.
pub const EXT2_DIND_BLOCK: usize = 13;
/// Index of triple-indirect block pointer (not used in Phase 28).
pub const EXT2_TIND_BLOCK: usize = 14;

/// Inode type bits (upper 4 bits of `mode`).
pub const S_IFREG: u16 = 0o100000;
pub const S_IFDIR: u16 = 0o040000;
pub const S_IFLNK: u16 = 0o120000;
pub const S_IFMT: u16 = 0o170000;

/// Directory entry file type indicators.
pub const EXT2_FT_UNKNOWN: u8 = 0;
pub const EXT2_FT_REG_FILE: u8 = 1;
pub const EXT2_FT_DIR: u8 = 2;
pub const EXT2_FT_SYMLINK: u8 = 7;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors from ext2 operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ext2Error {
    /// Superblock magic is not 0xEF53.
    BadMagic,
    /// Unsupported ext2 revision (we only support rev 0).
    UnsupportedRevision,
    /// Invalid or unsupported block size.
    InvalidBlockSize,
    /// Block device I/O error.
    IoError,
    /// Filesystem is out of free blocks or inodes.
    OutOfSpace,
    /// File or directory not found.
    NotFound,
    /// Expected a directory but got a file.
    NotDirectory,
    /// Expected a file but got a directory.
    IsDirectory,
    /// On-disk structure is corrupted or inconsistent.
    CorruptedEntry,
    /// Input slice is too short for the expected structure.
    TruncatedInput,
    /// Permission denied.
    PermissionDenied,
    /// Directory is not empty.
    NotEmpty,
}

// ---------------------------------------------------------------------------
// Superblock (P28-T001, P28-T005)
// ---------------------------------------------------------------------------

/// ext2 superblock — 1024 bytes at byte offset 1024 on disk.
#[derive(Debug, Clone, Copy)]
pub struct Ext2Superblock {
    pub inodes_count: u32,
    pub blocks_count: u32,
    pub r_blocks_count: u32,
    pub free_blocks_count: u32,
    pub free_inodes_count: u32,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub log_frag_size: u32,
    pub blocks_per_group: u32,
    pub frags_per_group: u32,
    pub inodes_per_group: u32,
    pub mtime: u32,
    pub wtime: u32,
    pub mnt_count: u16,
    pub max_mnt_count: u16,
    pub magic: u16,
    pub state: u16,
    pub errors: u16,
    pub minor_rev_level: u16,
    pub lastcheck: u32,
    pub checkinterval: u32,
    pub creator_os: u32,
    pub rev_level: u32,
    pub def_resuid: u16,
    pub def_resgid: u16,
    // Rev 1 fields (used if rev_level >= 1):
    pub first_ino: u32,
    pub inode_size: u16,
}

impl Ext2Superblock {
    /// Parse a superblock from a byte slice (must be >= 1024 bytes, starting
    /// at the superblock offset — i.e. bytes 1024..2048 from the partition start,
    /// or from the beginning of the slice if the caller has already offset).
    pub fn parse(bytes: &[u8]) -> Result<Self, Ext2Error> {
        if bytes.len() < 1024 {
            return Err(Ext2Error::TruncatedInput);
        }

        let magic = u16::from_le_bytes([bytes[56], bytes[57]]);
        if magic != EXT2_MAGIC {
            return Err(Ext2Error::BadMagic);
        }

        let rev_level = u32::from_le_bytes([bytes[76], bytes[77], bytes[78], bytes[79]]);
        let log_block_size = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);

        // Block size = 1024 << log_block_size. Only support 1K, 2K, 4K.
        if log_block_size > 2 {
            return Err(Ext2Error::InvalidBlockSize);
        }

        let (first_ino, inode_size) = if rev_level >= 1 {
            (
                u32::from_le_bytes([bytes[84], bytes[85], bytes[86], bytes[87]]),
                u16::from_le_bytes([bytes[88], bytes[89]]),
            )
        } else {
            (11, 128) // Rev 0 defaults
        };

        Ok(Ext2Superblock {
            inodes_count: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            blocks_count: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            r_blocks_count: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            free_blocks_count: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            free_inodes_count: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            first_data_block: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            log_block_size,
            log_frag_size: u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
            blocks_per_group: u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]),
            frags_per_group: u32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]),
            inodes_per_group: u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]),
            mtime: u32::from_le_bytes([bytes[44], bytes[45], bytes[46], bytes[47]]),
            wtime: u32::from_le_bytes([bytes[48], bytes[49], bytes[50], bytes[51]]),
            mnt_count: u16::from_le_bytes([bytes[52], bytes[53]]),
            max_mnt_count: u16::from_le_bytes([bytes[54], bytes[55]]),
            magic,
            state: u16::from_le_bytes([bytes[58], bytes[59]]),
            errors: u16::from_le_bytes([bytes[60], bytes[61]]),
            minor_rev_level: u16::from_le_bytes([bytes[62], bytes[63]]),
            lastcheck: u32::from_le_bytes([bytes[64], bytes[65], bytes[66], bytes[67]]),
            checkinterval: u32::from_le_bytes([bytes[68], bytes[69], bytes[70], bytes[71]]),
            creator_os: u32::from_le_bytes([bytes[72], bytes[73], bytes[74], bytes[75]]),
            rev_level,
            def_resuid: u16::from_le_bytes([bytes[80], bytes[81]]),
            def_resgid: u16::from_le_bytes([bytes[82], bytes[83]]),
            first_ino,
            inode_size,
        })
    }

    /// Block size in bytes: `1024 << log_block_size`.
    pub fn block_size(&self) -> u32 {
        1024 << self.log_block_size
    }

    /// Number of block groups on this volume.
    pub fn block_group_count(&self) -> u32 {
        (self.blocks_count - self.first_data_block).div_ceil(self.blocks_per_group)
    }

    /// Serialize the superblock back to bytes (for writeback).
    /// Only updates the mutable fields we care about.
    pub fn write_into(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= 1024);
        buf[12..16].copy_from_slice(&self.free_blocks_count.to_le_bytes());
        buf[16..20].copy_from_slice(&self.free_inodes_count.to_le_bytes());
        buf[48..52].copy_from_slice(&self.wtime.to_le_bytes());
        buf[52..54].copy_from_slice(&self.mnt_count.to_le_bytes());
        buf[56..58].copy_from_slice(&self.magic.to_le_bytes());
        buf[58..60].copy_from_slice(&self.state.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Block Group Descriptor (P28-T002, P28-T006)
// ---------------------------------------------------------------------------

/// ext2 block group descriptor — 32 bytes each in the descriptor table.
#[derive(Debug, Clone, Copy)]
pub struct Ext2BlockGroupDescriptor {
    pub block_bitmap: u32,
    pub inode_bitmap: u32,
    pub inode_table: u32,
    pub free_blocks_count: u16,
    pub free_inodes_count: u16,
    pub used_dirs_count: u16,
    // 14 bytes of padding/reserved fields (ignored).
}

impl Ext2BlockGroupDescriptor {
    /// Parse a single block group descriptor from 32 bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, Ext2Error> {
        if bytes.len() < 32 {
            return Err(Ext2Error::TruncatedInput);
        }
        Ok(Ext2BlockGroupDescriptor {
            block_bitmap: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            inode_bitmap: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            inode_table: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            free_blocks_count: u16::from_le_bytes([bytes[12], bytes[13]]),
            free_inodes_count: u16::from_le_bytes([bytes[14], bytes[15]]),
            used_dirs_count: u16::from_le_bytes([bytes[16], bytes[17]]),
        })
    }

    /// Parse the entire block group descriptor table.
    pub fn parse_table(
        bytes: &[u8],
        count: u32,
    ) -> Result<Vec<Ext2BlockGroupDescriptor>, Ext2Error> {
        let count = count as usize;
        if bytes.len() < count * 32 {
            return Err(Ext2Error::TruncatedInput);
        }
        let mut descriptors = Vec::with_capacity(count);
        for i in 0..count {
            let offset = i * 32;
            descriptors.push(Self::parse(&bytes[offset..offset + 32])?);
        }
        Ok(descriptors)
    }

    /// Serialize this descriptor back to bytes (for writeback).
    pub fn write_into(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= 32);
        buf[0..4].copy_from_slice(&self.block_bitmap.to_le_bytes());
        buf[4..8].copy_from_slice(&self.inode_bitmap.to_le_bytes());
        buf[8..12].copy_from_slice(&self.inode_table.to_le_bytes());
        buf[12..14].copy_from_slice(&self.free_blocks_count.to_le_bytes());
        buf[14..16].copy_from_slice(&self.free_inodes_count.to_le_bytes());
        buf[16..18].copy_from_slice(&self.used_dirs_count.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Inode (P28-T003, P28-T007)
// ---------------------------------------------------------------------------

/// ext2 inode — 128 bytes for rev 0 (inode_size may be larger for rev 1+).
#[derive(Debug, Clone, Copy)]
pub struct Ext2Inode {
    /// File type and permission bits.
    pub mode: u16,
    /// Owner user ID.
    pub uid: u16,
    /// File size (low 32 bits).
    pub size: u32,
    /// Last access time (Unix timestamp).
    pub atime: u32,
    /// Creation/change time (Unix timestamp).
    pub ctime: u32,
    /// Last modification time (Unix timestamp).
    pub mtime: u32,
    /// Deletion time (0 if not deleted).
    pub dtime: u32,
    /// Owner group ID.
    pub gid: u16,
    /// Hard link count.
    pub links_count: u16,
    /// Count of 512-byte blocks allocated to this inode.
    pub blocks: u32,
    /// Flags.
    pub flags: u32,
    /// Block pointers: 12 direct + 1 indirect + 1 double-indirect + 1 triple-indirect.
    pub block: [u32; 15],
    /// Generation number (for NFS).
    pub generation: u32,
    /// File ACL (rev 1).
    pub file_acl: u32,
    /// Size high bits (rev 1, regular files only).
    pub size_high: u32,
}

impl Ext2Inode {
    /// Parse an inode from a byte slice (at least 128 bytes).
    pub fn parse(bytes: &[u8]) -> Result<Self, Ext2Error> {
        if bytes.len() < 128 {
            return Err(Ext2Error::TruncatedInput);
        }

        let mut block = [0u32; 15];
        for (i, b) in block.iter_mut().enumerate() {
            let off = 40 + i * 4;
            *b = u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
        }

        Ok(Ext2Inode {
            mode: u16::from_le_bytes([bytes[0], bytes[1]]),
            uid: u16::from_le_bytes([bytes[2], bytes[3]]),
            size: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            atime: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            ctime: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            mtime: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            dtime: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
            gid: u16::from_le_bytes([bytes[24], bytes[25]]),
            links_count: u16::from_le_bytes([bytes[26], bytes[27]]),
            blocks: u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
            flags: u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]),
            // bytes[36..40] = osd1 (OS-dependent, ignored)
            block,
            generation: u32::from_le_bytes([bytes[100], bytes[101], bytes[102], bytes[103]]),
            file_acl: u32::from_le_bytes([bytes[104], bytes[105], bytes[106], bytes[107]]),
            size_high: u32::from_le_bytes([bytes[108], bytes[109], bytes[110], bytes[111]]),
        })
    }

    /// Whether this inode is a directory.
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    /// Whether this inode is a regular file.
    pub fn is_regular(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    /// Whether this inode is a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }

    /// Lower 12 bits: rwxrwxrwx + setuid/setgid/sticky.
    pub fn permission_mode(&self) -> u16 {
        self.mode & 0o7777
    }

    /// Upper 4 bits: file type (S_IFREG, S_IFDIR, etc.).
    pub fn file_type(&self) -> u16 {
        self.mode & S_IFMT
    }

    /// Serialize this inode back to bytes (for writeback).
    pub fn write_into(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= 128);
        buf[0..2].copy_from_slice(&self.mode.to_le_bytes());
        buf[2..4].copy_from_slice(&self.uid.to_le_bytes());
        buf[4..8].copy_from_slice(&self.size.to_le_bytes());
        buf[8..12].copy_from_slice(&self.atime.to_le_bytes());
        buf[12..16].copy_from_slice(&self.ctime.to_le_bytes());
        buf[16..20].copy_from_slice(&self.mtime.to_le_bytes());
        buf[20..24].copy_from_slice(&self.dtime.to_le_bytes());
        buf[24..26].copy_from_slice(&self.gid.to_le_bytes());
        buf[26..28].copy_from_slice(&self.links_count.to_le_bytes());
        buf[28..32].copy_from_slice(&self.blocks.to_le_bytes());
        buf[32..36].copy_from_slice(&self.flags.to_le_bytes());
        buf[36..40].copy_from_slice(&[0u8; 4]); // osd1
        for (i, &b) in self.block.iter().enumerate() {
            let off = 40 + i * 4;
            buf[off..off + 4].copy_from_slice(&b.to_le_bytes());
        }
        buf[100..104].copy_from_slice(&self.generation.to_le_bytes());
        buf[104..108].copy_from_slice(&self.file_acl.to_le_bytes());
        buf[108..112].copy_from_slice(&self.size_high.to_le_bytes());
    }

    /// Create a zeroed inode (for new files/directories).
    pub fn new_empty() -> Self {
        Ext2Inode {
            mode: 0,
            uid: 0,
            size: 0,
            atime: 0,
            ctime: 0,
            mtime: 0,
            dtime: 0,
            gid: 0,
            links_count: 0,
            blocks: 0,
            flags: 0,
            block: [0; 15],
            generation: 0,
            file_acl: 0,
            size_high: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Directory Entry (P28-T004)
// ---------------------------------------------------------------------------

/// A parsed ext2 directory entry.
#[derive(Debug, Clone)]
pub struct Ext2DirEntry {
    /// Inode number (0 = deleted entry).
    pub inode: u32,
    /// Total size of this entry including padding.
    pub rec_len: u16,
    /// Length of the name in bytes.
    pub name_len: u8,
    /// File type indicator (EXT2_FT_*).
    pub file_type: u8,
    /// File name (up to 255 bytes).
    pub name: String,
}

impl Ext2DirEntry {
    /// Minimum valid directory entry size: 8 bytes header + 1 byte name.
    const MIN_SIZE: usize = 8;

    /// Parse directory entries from a data block.
    /// Returns all entries (including deleted ones with inode==0).
    pub fn parse_block(block_data: &[u8]) -> Result<Vec<Ext2DirEntry>, Ext2Error> {
        let mut entries = Vec::new();
        let mut offset = 0;

        while offset + Self::MIN_SIZE <= block_data.len() {
            let inode = u32::from_le_bytes([
                block_data[offset],
                block_data[offset + 1],
                block_data[offset + 2],
                block_data[offset + 3],
            ]);
            let rec_len = u16::from_le_bytes([block_data[offset + 4], block_data[offset + 5]]);
            let name_len = block_data[offset + 6];
            let file_type = block_data[offset + 7];

            if rec_len == 0 {
                break; // Prevent infinite loop on corrupted data.
            }

            if (rec_len as usize) < Self::MIN_SIZE || offset + rec_len as usize > block_data.len() {
                return Err(Ext2Error::CorruptedEntry);
            }

            let name_end = offset + 8 + name_len as usize;
            if name_end > offset + rec_len as usize {
                return Err(Ext2Error::CorruptedEntry);
            }

            let name = core::str::from_utf8(&block_data[offset + 8..name_end])
                .map(String::from)
                .map_err(|_| Ext2Error::CorruptedEntry)?;

            entries.push(Ext2DirEntry {
                inode,
                rec_len,
                name_len,
                file_type,
                name,
            });

            offset += rec_len as usize;
        }

        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// Inode location helpers (P28-T009)
// ---------------------------------------------------------------------------

/// Compute the block group index for a given inode number (1-based).
pub fn inode_block_group(inode_num: u32, inodes_per_group: u32) -> u32 {
    (inode_num - 1) / inodes_per_group
}

/// Compute the index of an inode within its block group (0-based).
pub fn inode_index_in_group(inode_num: u32, inodes_per_group: u32) -> u32 {
    (inode_num - 1) % inodes_per_group
}

// ---------------------------------------------------------------------------
// Tests (P28-T008, P28-T009)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid superblock byte array.
    fn make_superblock() -> [u8; 1024] {
        let mut buf = [0u8; 1024];
        // inodes_count = 128
        buf[0..4].copy_from_slice(&128u32.to_le_bytes());
        // blocks_count = 1024
        buf[4..8].copy_from_slice(&1024u32.to_le_bytes());
        // free_blocks_count = 900
        buf[12..16].copy_from_slice(&900u32.to_le_bytes());
        // free_inodes_count = 100
        buf[16..20].copy_from_slice(&100u32.to_le_bytes());
        // first_data_block = 0 (for 4K blocks)
        buf[20..24].copy_from_slice(&0u32.to_le_bytes());
        // log_block_size = 2 (4K blocks: 1024 << 2 = 4096)
        buf[24..28].copy_from_slice(&2u32.to_le_bytes());
        // blocks_per_group = 8192
        buf[32..36].copy_from_slice(&8192u32.to_le_bytes());
        // inodes_per_group = 128
        buf[40..44].copy_from_slice(&128u32.to_le_bytes());
        // magic = 0xEF53
        buf[56..58].copy_from_slice(&EXT2_MAGIC.to_le_bytes());
        // rev_level = 0
        buf[76..80].copy_from_slice(&0u32.to_le_bytes());
        buf
    }

    #[test]
    fn parse_superblock_valid() {
        let buf = make_superblock();
        let sb = Ext2Superblock::parse(&buf).unwrap();
        assert_eq!(sb.magic, EXT2_MAGIC);
        assert_eq!(sb.inodes_count, 128);
        assert_eq!(sb.blocks_count, 1024);
        assert_eq!(sb.free_blocks_count, 900);
        assert_eq!(sb.free_inodes_count, 100);
        assert_eq!(sb.block_size(), 4096);
        assert_eq!(sb.log_block_size, 2);
        assert_eq!(sb.blocks_per_group, 8192);
        assert_eq!(sb.inodes_per_group, 128);
        assert_eq!(sb.block_group_count(), 1); // 1024/8192 rounds up to 1
                                               // Rev 0 defaults
        assert_eq!(sb.first_ino, 11);
        assert_eq!(sb.inode_size, 128);
    }

    #[test]
    fn parse_superblock_bad_magic() {
        let mut buf = make_superblock();
        buf[56] = 0x00;
        buf[57] = 0x00;
        assert_eq!(
            Ext2Superblock::parse(&buf).unwrap_err(),
            Ext2Error::BadMagic
        );
    }

    #[test]
    fn parse_superblock_truncated() {
        let buf = [0u8; 512];
        assert_eq!(
            Ext2Superblock::parse(&buf).unwrap_err(),
            Ext2Error::TruncatedInput
        );
    }

    #[test]
    fn parse_superblock_invalid_block_size() {
        let mut buf = make_superblock();
        // log_block_size = 3 → 8K blocks (unsupported)
        buf[24..28].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(
            Ext2Superblock::parse(&buf).unwrap_err(),
            Ext2Error::InvalidBlockSize
        );
    }

    #[test]
    fn parse_superblock_rev1() {
        let mut buf = make_superblock();
        // rev_level = 1
        buf[76..80].copy_from_slice(&1u32.to_le_bytes());
        // first_ino = 11
        buf[84..88].copy_from_slice(&11u32.to_le_bytes());
        // inode_size = 256
        buf[88..90].copy_from_slice(&256u16.to_le_bytes());
        let sb = Ext2Superblock::parse(&buf).unwrap();
        assert_eq!(sb.rev_level, 1);
        assert_eq!(sb.first_ino, 11);
        assert_eq!(sb.inode_size, 256);
    }

    #[test]
    fn parse_block_group_descriptor() {
        let mut buf = [0u8; 32];
        buf[0..4].copy_from_slice(&5u32.to_le_bytes()); // block_bitmap
        buf[4..8].copy_from_slice(&6u32.to_le_bytes()); // inode_bitmap
        buf[8..12].copy_from_slice(&7u32.to_le_bytes()); // inode_table
        buf[12..14].copy_from_slice(&800u16.to_le_bytes()); // free_blocks_count
        buf[14..16].copy_from_slice(&100u16.to_le_bytes()); // free_inodes_count
        buf[16..18].copy_from_slice(&3u16.to_le_bytes()); // used_dirs_count

        let bgd = Ext2BlockGroupDescriptor::parse(&buf).unwrap();
        assert_eq!(bgd.block_bitmap, 5);
        assert_eq!(bgd.inode_bitmap, 6);
        assert_eq!(bgd.inode_table, 7);
        assert_eq!(bgd.free_blocks_count, 800);
        assert_eq!(bgd.free_inodes_count, 100);
        assert_eq!(bgd.used_dirs_count, 3);
    }

    #[test]
    fn parse_block_group_descriptor_truncated() {
        let buf = [0u8; 16];
        assert_eq!(
            Ext2BlockGroupDescriptor::parse(&buf).unwrap_err(),
            Ext2Error::TruncatedInput
        );
    }

    #[test]
    fn parse_block_group_descriptor_table() {
        let mut buf = [0u8; 64];
        // BGD 0
        buf[0..4].copy_from_slice(&10u32.to_le_bytes());
        buf[4..8].copy_from_slice(&11u32.to_le_bytes());
        buf[8..12].copy_from_slice(&12u32.to_le_bytes());
        // BGD 1
        buf[32..36].copy_from_slice(&20u32.to_le_bytes());
        buf[36..40].copy_from_slice(&21u32.to_le_bytes());
        buf[40..44].copy_from_slice(&22u32.to_le_bytes());

        let table = Ext2BlockGroupDescriptor::parse_table(&buf, 2).unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].block_bitmap, 10);
        assert_eq!(table[1].block_bitmap, 20);
    }

    #[test]
    fn parse_inode() {
        let mut buf = [0u8; 128];
        // mode = directory + 0o755
        let mode = S_IFDIR | 0o755;
        buf[0..2].copy_from_slice(&mode.to_le_bytes());
        // uid = 1000
        buf[2..4].copy_from_slice(&1000u16.to_le_bytes());
        // size = 4096
        buf[4..8].copy_from_slice(&4096u32.to_le_bytes());
        // mtime = 1234567890
        buf[16..20].copy_from_slice(&1234567890u32.to_le_bytes());
        // gid = 1000
        buf[24..26].copy_from_slice(&1000u16.to_le_bytes());
        // links_count = 2
        buf[26..28].copy_from_slice(&2u16.to_le_bytes());
        // block[0] = 42
        buf[40..44].copy_from_slice(&42u32.to_le_bytes());

        let inode = Ext2Inode::parse(&buf).unwrap();
        assert!(inode.is_dir());
        assert!(!inode.is_regular());
        assert!(!inode.is_symlink());
        assert_eq!(inode.permission_mode(), 0o755);
        assert_eq!(inode.file_type(), S_IFDIR);
        assert_eq!(inode.uid, 1000);
        assert_eq!(inode.gid, 1000);
        assert_eq!(inode.size, 4096);
        assert_eq!(inode.mtime, 1234567890);
        assert_eq!(inode.links_count, 2);
        assert_eq!(inode.block[0], 42);
    }

    #[test]
    fn parse_inode_regular_file() {
        let mut buf = [0u8; 128];
        let mode = S_IFREG | 0o644;
        buf[0..2].copy_from_slice(&mode.to_le_bytes());

        let inode = Ext2Inode::parse(&buf).unwrap();
        assert!(inode.is_regular());
        assert!(!inode.is_dir());
        assert_eq!(inode.permission_mode(), 0o644);
    }

    #[test]
    fn parse_inode_truncated() {
        let buf = [0u8; 64];
        assert_eq!(
            Ext2Inode::parse(&buf).unwrap_err(),
            Ext2Error::TruncatedInput
        );
    }

    #[test]
    fn inode_write_roundtrip() {
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFREG | 0o644;
        inode.uid = 500;
        inode.gid = 500;
        inode.size = 12345;
        inode.mtime = 99999;
        inode.links_count = 1;
        inode.block[0] = 100;
        inode.block[12] = 200;

        let mut buf = [0u8; 128];
        inode.write_into(&mut buf);

        let parsed = Ext2Inode::parse(&buf).unwrap();
        assert_eq!(parsed.mode, inode.mode);
        assert_eq!(parsed.uid, inode.uid);
        assert_eq!(parsed.gid, inode.gid);
        assert_eq!(parsed.size, inode.size);
        assert_eq!(parsed.mtime, inode.mtime);
        assert_eq!(parsed.links_count, inode.links_count);
        assert_eq!(parsed.block[0], 100);
        assert_eq!(parsed.block[12], 200);
    }

    #[test]
    fn parse_directory_entries() {
        // Build a block with two entries: "." and "hello.txt"
        let mut block = [0u8; 4096];

        // Entry 1: "." (inode 2, rec_len=12)
        block[0..4].copy_from_slice(&2u32.to_le_bytes()); // inode
        block[4..6].copy_from_slice(&12u16.to_le_bytes()); // rec_len
        block[6] = 1; // name_len
        block[7] = EXT2_FT_DIR; // file_type
        block[8] = b'.';

        // Entry 2: "hello.txt" (inode 12, rec_len=4084 to fill the block)
        let off = 12;
        block[off..off + 4].copy_from_slice(&12u32.to_le_bytes()); // inode
        block[off + 4..off + 6].copy_from_slice(&(4096 - 12_u16).to_le_bytes()); // rec_len
        block[off + 6] = 9; // name_len
        block[off + 7] = EXT2_FT_REG_FILE;
        block[off + 8..off + 17].copy_from_slice(b"hello.txt");

        let entries = Ext2DirEntry::parse_block(&block).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].inode, 2);
        assert_eq!(entries[0].name, ".");
        assert_eq!(entries[0].file_type, EXT2_FT_DIR);
        assert_eq!(entries[1].inode, 12);
        assert_eq!(entries[1].name, "hello.txt");
        assert_eq!(entries[1].file_type, EXT2_FT_REG_FILE);
    }

    #[test]
    fn parse_directory_entry_deleted() {
        let mut block = [0u8; 4096];
        // A deleted entry: inode = 0, rec_len = 4096
        block[0..4].copy_from_slice(&0u32.to_le_bytes());
        block[4..6].copy_from_slice(&4096u16.to_le_bytes());
        block[6] = 4;
        block[7] = EXT2_FT_REG_FILE;
        block[8..12].copy_from_slice(b"test");

        let entries = Ext2DirEntry::parse_block(&block).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].inode, 0); // deleted
        assert_eq!(entries[0].name, "test");
    }

    #[test]
    fn parse_directory_entry_zero_reclen_stops() {
        let block = [0u8; 4096]; // All zeros — rec_len=0 should stop
        let entries = Ext2DirEntry::parse_block(&block).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn inode_block_group_helpers() {
        // Inode 1 → group 0, index 0
        assert_eq!(inode_block_group(1, 128), 0);
        assert_eq!(inode_index_in_group(1, 128), 0);

        // Inode 2 (root) → group 0, index 1
        assert_eq!(inode_block_group(2, 128), 0);
        assert_eq!(inode_index_in_group(2, 128), 1);

        // Inode 128 → group 0, index 127
        assert_eq!(inode_block_group(128, 128), 0);
        assert_eq!(inode_index_in_group(128, 128), 127);

        // Inode 129 → group 1, index 0
        assert_eq!(inode_block_group(129, 128), 1);
        assert_eq!(inode_index_in_group(129, 128), 0);

        // Inode 256 → group 1, index 127
        assert_eq!(inode_block_group(256, 128), 1);
        assert_eq!(inode_index_in_group(256, 128), 127);
    }

    #[test]
    fn superblock_write_roundtrip() {
        let mut buf = make_superblock();
        let sb = Ext2Superblock::parse(&buf).unwrap();

        // Modify some fields
        let mut sb2 = sb;
        sb2.free_blocks_count = 850;
        sb2.free_inodes_count = 90;
        sb2.write_into(&mut buf);

        let sb3 = Ext2Superblock::parse(&buf).unwrap();
        assert_eq!(sb3.free_blocks_count, 850);
        assert_eq!(sb3.free_inodes_count, 90);
    }

    #[test]
    fn bgd_write_roundtrip() {
        let bgd = Ext2BlockGroupDescriptor {
            block_bitmap: 3,
            inode_bitmap: 4,
            inode_table: 5,
            free_blocks_count: 700,
            free_inodes_count: 50,
            used_dirs_count: 10,
        };
        let mut buf = [0u8; 32];
        bgd.write_into(&mut buf);

        let parsed = Ext2BlockGroupDescriptor::parse(&buf).unwrap();
        assert_eq!(parsed.block_bitmap, 3);
        assert_eq!(parsed.inode_bitmap, 4);
        assert_eq!(parsed.inode_table, 5);
        assert_eq!(parsed.free_blocks_count, 700);
        assert_eq!(parsed.free_inodes_count, 50);
        assert_eq!(parsed.used_dirs_count, 10);
    }
}
