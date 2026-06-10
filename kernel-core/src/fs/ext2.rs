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
    /// File or directory already exists.
    AlreadyExists,
    /// Expected a symlink but got a different file type.
    NotSymlink,
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

        let blocks_per_group = u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]);
        if blocks_per_group == 0 {
            return Err(Ext2Error::CorruptedEntry);
        }
        let inodes_per_group = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);
        if inodes_per_group == 0 {
            return Err(Ext2Error::CorruptedEntry);
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
            blocks_per_group,
            frags_per_group: u32::from_le_bytes([bytes[36], bytes[37], bytes[38], bytes[39]]),
            inodes_per_group,
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

/// Phase 87 Track B.1 — coalesced file-data read.
///
/// Reads up to `buf.len()` bytes of file data starting at byte `offset`,
/// **coalescing runs of physically-contiguous whole blocks into single
/// multi-block device reads** instead of one device round-trip per logical
/// block. This is the dominant Phase 87 throughput win: a 21 MiB file whose
/// blocks are laid out contiguously collapses from ~5,376 per-block reads to a
/// small multiple of its contiguous-run count.
///
/// Pure orchestration — the caller supplies block resolution and the device
/// primitives, so this is host-testable with in-memory mocks while the kernel
/// passes the real ext2 resolver + `blk::read_sectors`:
/// - `resolve(logical) -> phys`: physical block for a logical block, `0` == a
///   sparse hole.
/// - `read_run(phys, block_count, dst)`: read `block_count` physically-
///   contiguous WHOLE blocks starting at `phys` straight into `dst`
///   (`dst.len() == block_count * block_size`). **One device round-trip.**
/// - `read_partial(phys, block_off, dst)`: read `dst.len()` bytes from within
///   the single block `phys` at byte offset `block_off` — the unaligned head /
///   short tail of the range (kept on the cache-aware per-block path in-kernel).
///
/// `max_run_blocks` bounds how many blocks a single `read_run` may span — the
/// block driver caps a single request (`remote::read_sectors`'s
/// `MAX_SECTORS_PER_REQUEST` = 256 sectors = 32 blocks; the `sys_block_read`
/// path 128 sectors = 16 blocks), so a run longer than that would be rejected.
/// A contiguous file longer than the cap is split into back-to-back runs of at
/// most `max_run_blocks` (still a huge win over per-block). A value of `0` is
/// clamped up to `1` (see `max_run_blocks.max(1)` below) so the run loop always
/// makes progress.
///
/// Returns the number of bytes read (`min(buf.len(), file_size - offset)`),
/// byte-for-byte identical to a naive per-block reader over the same data.
/// Holes break a run and are zero-filled without a device request.
// The geometry (size/block_size/offset/buf/cap) plus the three device closures
// (resolve / read_run / read_partial) are all irreducible inputs of a pure
// orchestrator; bundling them into a struct would only obscure the call sites.
#[allow(clippy::too_many_arguments)]
pub fn read_file_data_coalesced<E>(
    file_size: u64,
    block_size: u32,
    offset: u64,
    buf: &mut [u8],
    max_run_blocks: u32,
    mut resolve: impl FnMut(u32) -> Result<u32, E>,
    mut read_run: impl FnMut(u32, u32, &mut [u8]) -> Result<(), E>,
    mut read_partial: impl FnMut(u32, usize, &mut [u8]) -> Result<(), E>,
) -> Result<usize, E> {
    let max_run_blocks = max_run_blocks.max(1);
    if offset >= file_size {
        return Ok(0);
    }
    let bs = block_size as u64;
    let available = (file_size - offset) as usize;
    let to_read = buf.len().min(available);

    let mut bytes_read = 0usize;
    let mut pos = offset;

    while bytes_read < to_read {
        let logical_block = (pos / bs) as u32;
        let offset_in_block = (pos % bs) as usize;
        let remaining = to_read - bytes_read;

        // Aligned whole-block region: coalesce a contiguous run read straight
        // into `buf`. Requires the position to be block-aligned and at least one
        // full block remaining; otherwise fall through to the partial path.
        if offset_in_block == 0 && remaining >= bs as usize {
            let phys = resolve(logical_block)?;
            if phys == 0 {
                // Hole: zero-fill exactly one block (a hole terminates any run).
                buf[bytes_read..bytes_read + bs as usize].fill(0);
                bytes_read += bs as usize;
                pos += bs;
                continue;
            }
            // Extend the run while the next whole block is physically contiguous,
            // a full block still fits in the remaining request, AND the run stays
            // within the device's max-sectors-per-request bound.
            let mut run_len: u32 = 1;
            loop {
                if run_len >= max_run_blocks {
                    break; // device single-request cap
                }
                if (run_len as usize + 1) * bs as usize > remaining {
                    break;
                }
                if resolve(logical_block + run_len)? != phys + run_len {
                    break; // discontiguity or hole ends the run
                }
                run_len += 1;
            }
            let run_bytes = run_len as usize * bs as usize;
            read_run(phys, run_len, &mut buf[bytes_read..bytes_read + run_bytes])?;
            bytes_read += run_bytes;
            pos += run_bytes as u64;
        } else {
            // Unaligned head, or a short tail of < one block: single-block copy.
            let copy_len = (bs as usize - offset_in_block).min(remaining);
            let phys = resolve(logical_block)?;
            if phys == 0 {
                buf[bytes_read..bytes_read + copy_len].fill(0);
            } else {
                read_partial(
                    phys,
                    offset_in_block,
                    &mut buf[bytes_read..bytes_read + copy_len],
                )?;
            }
            bytes_read += copy_len;
            pos += copy_len as u64;
        }
    }
    Ok(bytes_read)
}

// ---------------------------------------------------------------------------
// Phase 88 Track C — shared higher-level ext2 read logic over a `BlockReader`.
//
// `resolve_path` / `read_inode` / `read_file_data` / `resolve_block` /
// `read_directory_entries` were historically implemented TWICE — once in the
// kernel (`kernel/src/fs/ext2.rs`, `Ext2Volume`) and once in the ring-3
// `vfs_server` (`Ext2State`) — and they diverged at the metadata seam (the 85d
// post-mortem's finding #2). They now live here ONCE, generic over a
// `BlockReader` that supplies only the block source + geometry, so a fix in one
// is a fix in both. The kernel supplies a `crate::blk`-backed reader (with its
// block cache); the vfs_server supplies a `sys_block_read`-backed reader (with
// its own cache + write-back shadow). Only the byte-struct parsing was shared
// before; the *logic* is now shared too.
// ---------------------------------------------------------------------------

/// A source of ext2 blocks plus the geometry needed to walk inodes and paths.
///
/// Implementors own the I/O strategy (caching, multi-block device reads); the
/// shared functions below contain the filesystem logic. `read_block` returns a
/// freshly-owned `block_size`-byte buffer.
pub trait BlockReader {
    /// ext2 block size in bytes (`1024 << log_block_size`).
    fn block_size(&self) -> u32;
    /// Inodes per block group (from the superblock).
    fn inodes_per_group(&self) -> u32;
    /// On-disk inode size in bytes (128 for rev 0).
    fn inode_size(&self) -> u32;
    /// First block of the inode table for block group `group` (from the BGD).
    fn inode_table_block(&self, group: u32) -> Result<u32, Ext2Error>;
    /// Read one whole ext2 block (`block_size` bytes).
    fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error>;

    /// Maximum number of physically-contiguous blocks a single coalesced run may
    /// span (the device's per-request cap). Default 1 (no coalescing).
    fn max_run_blocks(&self) -> u32 {
        1
    }
    /// Read `count` physically-contiguous whole blocks starting at `start_block`
    /// into `dst` (`dst.len() == count * block_size`). The default reads them one
    /// at a time; implementors override for a single multi-block device read.
    fn read_block_run(
        &self,
        start_block: u32,
        count: u32,
        dst: &mut [u8],
    ) -> Result<(), Ext2Error> {
        let bs = self.block_size() as usize;
        for i in 0..count {
            let block = self.read_block(start_block + i)?;
            let off = i as usize * bs;
            dst[off..off + bs].copy_from_slice(&block);
        }
        Ok(())
    }
    /// Copy `dst.len()` bytes from within block `block_num` at byte offset
    /// `block_offset` into `dst`. The default reads the whole block then slices;
    /// implementors override for a cache-aware copy.
    fn read_block_into(
        &self,
        block_num: u32,
        block_offset: usize,
        dst: &mut [u8],
    ) -> Result<(), Ext2Error> {
        let block = self.read_block(block_num)?;
        dst.copy_from_slice(&block[block_offset..block_offset + dst.len()]);
        Ok(())
    }
}

/// Read a little-endian `u32` block pointer at pointer-index `idx`, bounds-safe.
fn read_block_ptr(block: &[u8], idx: usize) -> Result<u32, Ext2Error> {
    let off = idx * 4;
    let bytes = block.get(off..off + 4).ok_or(Ext2Error::TruncatedInput)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read inode `inode_num` (1-based) via the `BlockReader`.
pub fn read_inode<R: BlockReader + ?Sized>(r: &R, inode_num: u32) -> Result<Ext2Inode, Ext2Error> {
    let ipg = r.inodes_per_group();
    if ipg == 0 {
        return Err(Ext2Error::CorruptedEntry);
    }
    let group = inode_block_group(inode_num, ipg);
    let index = inode_index_in_group(inode_num, ipg);
    let inode_table = r.inode_table_block(group)?;
    let bs = r.block_size() as u64;
    let byte_offset = index as u64 * r.inode_size() as u64;
    let block_offset = (byte_offset / bs) as u32;
    let offset_in_block = (byte_offset % bs) as usize;
    let block = r.read_block(inode_table + block_offset)?;
    Ext2Inode::parse(&block[offset_in_block..])
}

/// Resolve logical block `logical_block` of `inode` to a physical block number,
/// walking direct / single-indirect / double-indirect pointers. Returns 0 for a
/// sparse hole. Triple-indirect is unsupported (`CorruptedEntry`).
pub fn resolve_block<R: BlockReader + ?Sized>(
    r: &R,
    inode: &Ext2Inode,
    logical_block: u32,
) -> Result<u32, Ext2Error> {
    let ptrs_per_block = r.block_size() / 4;

    if logical_block < EXT2_NDIR_BLOCKS as u32 {
        return Ok(inode.block[logical_block as usize]);
    }
    let adjusted = logical_block - EXT2_NDIR_BLOCKS as u32;

    if adjusted < ptrs_per_block {
        let ind = inode.block[EXT2_IND_BLOCK];
        if ind == 0 {
            return Ok(0);
        }
        let data = r.read_block(ind)?;
        return read_block_ptr(&data, adjusted as usize);
    }
    let adjusted = adjusted - ptrs_per_block;

    if adjusted < ptrs_per_block * ptrs_per_block {
        let dind = inode.block[EXT2_DIND_BLOCK];
        if dind == 0 {
            return Ok(0);
        }
        let dind_data = r.read_block(dind)?;
        let ind = read_block_ptr(&dind_data, (adjusted / ptrs_per_block) as usize)?;
        if ind == 0 {
            return Ok(0);
        }
        let ind_data = r.read_block(ind)?;
        return read_block_ptr(&ind_data, (adjusted % ptrs_per_block) as usize);
    }

    Err(Ext2Error::CorruptedEntry)
}

/// Read up to `buf.len()` bytes of `inode`'s data starting at byte `offset`,
/// coalescing physically-contiguous whole-block runs (see
/// [`read_file_data_coalesced`]). Returns the number of bytes read.
pub fn read_file_data<R: BlockReader + ?Sized>(
    r: &R,
    inode: &Ext2Inode,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, Ext2Error> {
    read_file_data_coalesced(
        inode.size as u64,
        r.block_size(),
        offset,
        buf,
        r.max_run_blocks(),
        |logical_block| resolve_block(r, inode, logical_block),
        |start_block, count, dst| r.read_block_run(start_block, count, dst),
        |phys_block, offset_in_block, dst| r.read_block_into(phys_block, offset_in_block, dst),
    )
}

/// Read all directory entries of `inode` (a directory), including `.` and `..`,
/// skipping deleted (inode==0) entries. Returns `(name, inode, file_type)` where
/// `file_type` is the raw ext2 dir-entry type byte (`EXT2_FT_*`).
pub fn read_directory_entries<R: BlockReader + ?Sized>(
    r: &R,
    inode: &Ext2Inode,
) -> Result<Vec<(String, u32, u8)>, Ext2Error> {
    if !inode.is_dir() {
        return Err(Ext2Error::NotDirectory);
    }
    let bs = r.block_size() as u64;
    let num_blocks = (inode.size as u64).div_ceil(bs) as u32;
    let mut result = Vec::new();
    for logical_block in 0..num_blocks {
        let phys = resolve_block(r, inode, logical_block)?;
        if phys == 0 {
            continue;
        }
        let block = r.read_block(phys)?;
        for entry in Ext2DirEntry::parse_block(&block)? {
            if entry.inode != 0 {
                result.push((entry.name, entry.inode, entry.file_type));
            }
        }
    }
    Ok(result)
}

/// Look up `name` in directory `dir_inode`, returning its inode number.
pub fn lookup_in_directory<R: BlockReader + ?Sized>(
    r: &R,
    dir_inode: &Ext2Inode,
    name: &str,
) -> Result<u32, Ext2Error> {
    for (entry_name, ino, _ft) in read_directory_entries(r, dir_inode)? {
        if entry_name == name {
            return Ok(ino);
        }
    }
    Err(Ext2Error::NotFound)
}

/// Resolve a path to an inode number, walking from the root inode. Empty
/// components and `.` are skipped (so a leading `/` is tolerated). Symlinks in
/// intermediate components are NOT followed (matches both prior implementations).
pub fn resolve_path<R: BlockReader + ?Sized>(r: &R, path: &str) -> Result<u32, Ext2Error> {
    let mut current = EXT2_ROOT_INO;
    for component in path.split('/').filter(|s| !s.is_empty()) {
        if component == "." {
            continue;
        }
        let inode = read_inode(r, current)?;
        if !inode.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }
        current = lookup_in_directory(r, &inode, component)?;
    }
    Ok(current)
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

    #[test]
    fn parse_inode_symlink() {
        let mut buf = [0u8; 128];
        let mode = S_IFLNK | 0o777;
        buf[0..2].copy_from_slice(&mode.to_le_bytes());
        buf[26..28].copy_from_slice(&1u16.to_le_bytes()); // links_count = 1

        let inode = Ext2Inode::parse(&buf).unwrap();
        assert!(inode.is_symlink());
        assert!(!inode.is_dir());
        assert!(!inode.is_regular());
        assert_eq!(inode.permission_mode(), 0o777);
        assert_eq!(inode.file_type(), S_IFLNK);
    }

    #[test]
    fn symlink_inline_roundtrip() {
        // Simulate inline symlink: target stored in block pointer bytes.
        let target = b"/usr/bin/env";
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFLNK | 0o777;
        inode.links_count = 1;
        inode.size = target.len() as u32;
        // blocks = 0 signals inline storage

        // Write target into block array (as the kernel ext2 driver does).
        let mut raw = [0u8; 60];
        raw[..target.len()].copy_from_slice(target);
        for (i, slot) in inode.block.iter_mut().enumerate() {
            let off = i * 4;
            *slot = u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
        }

        // Serialize and re-parse.
        let mut buf = [0u8; 128];
        inode.write_into(&mut buf);
        let parsed = Ext2Inode::parse(&buf).unwrap();

        assert!(parsed.is_symlink());
        assert_eq!(parsed.size, target.len() as u32);
        assert_eq!(parsed.blocks, 0);

        // Extract target back from block array.
        let mut out = [0u8; 60];
        for (i, &slot) in parsed.block.iter().enumerate() {
            let off = i * 4;
            out[off..off + 4].copy_from_slice(&slot.to_le_bytes());
        }
        assert_eq!(&out[..target.len()], target);
    }

    // -----------------------------------------------------------------------
    // Phase 87 Track B.1 — coalesced read tests
    // -----------------------------------------------------------------------

    use core::cell::Cell;

    const TEST_BS: u32 = 1024;

    /// A deterministic mock "disk": physical block `p` is filled with the byte
    /// pattern `(p + i) as u8` for byte `i`. Lets a test assert byte-for-byte
    /// equality between the coalesced reader and a naive per-block reader.
    fn mock_block_bytes(p: u32, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            *b = (p as usize).wrapping_add(i) as u8;
        }
    }

    /// The logical→physical map under test: a long contiguous run, a single
    /// hole, and a jump to a second contiguous run (a discontiguity). `0` is a
    /// hole. This crosses the 12-direct / single-indirect / double-indirect
    /// boundaries by logical-block count (for `TEST_BS`, ptrs_per_block = 256, so
    /// double-indirect begins at logical 12 + 256 = 268); the coalescer is
    /// resolution-agnostic, so what matters is the physical layout it sees.
    fn mock_resolve(lb: u32) -> u32 {
        if lb == 50 {
            0 // a sparse hole, well inside the first run
        } else if lb < 2000 {
            1000 + lb // first contiguous run (phys 1000..)
        } else {
            500_000 + (lb - 2000) // jump to a second contiguous run
        }
    }

    /// Naive per-block reference reader — exactly the pre-Phase-87 loop shape.
    fn reference_read(file_size: u64, offset: u64, buf: &mut [u8]) -> usize {
        let bs = TEST_BS as u64;
        if offset >= file_size {
            return 0;
        }
        let to_read = buf.len().min((file_size - offset) as usize);
        let mut done = 0;
        let mut pos = offset;
        while done < to_read {
            let lb = (pos / bs) as u32;
            let off = (pos % bs) as usize;
            let n = (bs as usize - off).min(to_read - done);
            let phys = mock_resolve(lb);
            if phys == 0 {
                buf[done..done + n].fill(0);
            } else {
                let mut blk = alloc::vec![0u8; bs as usize];
                mock_block_bytes(phys, &mut blk);
                buf[done..done + n].copy_from_slice(&blk[off..off + n]);
            }
            done += n;
            pos += n as u64;
        }
        done
    }

    /// Run the coalesced reader against the mocks, returning (bytes, output,
    /// run_calls, partial_calls). `run_calls` is the device-round-trip count the
    /// Phase 87 acceptance is measured on.
    fn coalesced_read(file_size: u64, offset: u64, len: usize) -> (usize, Vec<u8>, usize, usize) {
        // A generous cap so the existing run-coalescing tests are unaffected; the
        // cap behaviour itself is covered by `coalesced_read_caps_run_length`.
        coalesced_read_capped(file_size, offset, len, u32::MAX)
    }

    /// As `coalesced_read`, but with an explicit `max_run_blocks` so a test can
    /// exercise the device single-request cap.
    fn coalesced_read_capped(
        file_size: u64,
        offset: u64,
        len: usize,
        max_run_blocks: u32,
    ) -> (usize, Vec<u8>, usize, usize) {
        let bs = TEST_BS as usize;
        let runs = Cell::new(0usize);
        let partials = Cell::new(0usize);
        let mut buf = alloc::vec![0u8; len];
        let n = read_file_data_coalesced::<()>(
            file_size,
            TEST_BS,
            offset,
            &mut buf,
            max_run_blocks,
            |lb| Ok(mock_resolve(lb)),
            |phys, count, dst| {
                assert!(
                    count <= max_run_blocks,
                    "read_run issued {count} blocks, exceeding the cap {max_run_blocks}"
                );
                runs.set(runs.get() + 1);
                for k in 0..count {
                    mock_block_bytes(phys + k, &mut dst[k as usize * bs..(k as usize + 1) * bs]);
                }
                Ok(())
            },
            |phys, off, dst| {
                partials.set(partials.get() + 1);
                let mut blk = alloc::vec![0u8; bs];
                mock_block_bytes(phys, &mut blk);
                dst.copy_from_slice(&blk[off..off + dst.len()]);
                Ok(())
            },
        )
        .unwrap();
        (n, buf, runs.get(), partials.get())
    }

    #[test]
    fn coalesced_read_is_byte_identical_to_per_block() {
        let file_size = 2500 * TEST_BS as u64; // spans both runs + the hole
        for &(off, len) in &[
            (0u64, 2500 * TEST_BS as usize),               // whole file
            (0, 100 * TEST_BS as usize),                   // first run incl. the hole
            (5, 300 * TEST_BS as usize + 17),              // unaligned head + tail
            (1999 * TEST_BS as u64 - 10, 40),              // straddle the run jump
            ((TEST_BS as u64) * 50, TEST_BS as usize * 2), // start exactly on the hole
        ] {
            let (n, got, _, _) = coalesced_read(file_size, off, len);
            let mut want = alloc::vec![0u8; len];
            let want_n = reference_read(file_size, off, &mut want);
            assert_eq!(n, want_n, "byte count mismatch at off={off} len={len}");
            assert_eq!(got, want, "content mismatch at off={off} len={len}");
        }
    }

    #[test]
    fn coalesced_read_collapses_contiguous_runs_into_few_requests() {
        // Read the whole 2500-block file. Layout = run[0..50) + hole + run[51..2000)
        // + jump + run[2000..2500). A naive reader would issue ~2499 block reads;
        // the coalescer must issue only a handful (one per contiguous run), and
        // the hole must issue NO device request.
        let file_size = 2500 * TEST_BS as u64;
        let (n, _, runs, partials) = coalesced_read(file_size, 0, file_size as usize);
        assert_eq!(n, file_size as usize);
        // Three contiguous whole-block runs (pre-hole, post-hole, post-jump).
        assert!(
            runs <= 4,
            "expected <=4 coalesced runs for a near-contiguous file, got {runs}"
        );
        // The interior hole is whole-block aligned → zero-filled with no request.
        assert_eq!(
            partials, 0,
            "no partial reads expected for an aligned whole-file read"
        );
        // The whole-file read is FAR under the Phase 87 ≤512-request acceptance.
        assert!(
            runs < 512,
            "run count {runs} must be well under the 512 ceiling"
        );
    }

    #[test]
    fn coalesced_read_handles_holes_without_device_requests() {
        // A read entirely inside the hole returns zeros and issues no requests.
        let file_size = 2500 * TEST_BS as u64;
        let (n, got, runs, partials) =
            coalesced_read(file_size, 50 * TEST_BS as u64, TEST_BS as usize);
        assert_eq!(n, TEST_BS as usize);
        assert!(got.iter().all(|&b| b == 0), "hole must read as zeros");
        assert_eq!(runs, 0);
        assert_eq!(partials, 0);
    }

    #[test]
    fn coalesced_read_offset_past_eof_is_empty() {
        let file_size = 10 * TEST_BS as u64;
        let (n, _, runs, partials) = coalesced_read(file_size, file_size, 4096);
        assert_eq!(n, 0);
        assert_eq!(runs, 0);
        assert_eq!(partials, 0);
    }

    #[test]
    fn coalesced_read_caps_run_length_to_device_max() {
        // Read 64 contiguous blocks (logical 100..164 → phys 1100..1164, all in
        // the first contiguous run) with a 16-block device cap. The run must be
        // split into ceil(64/16) = 4 runs of <=16 blocks each — never one
        // 64-block request the driver would reject — and stay byte-identical.
        let file_size = 2500 * TEST_BS as u64;
        let off = 100 * TEST_BS as u64;
        let len = 64 * TEST_BS as usize;
        let (n, got, runs, partials) = coalesced_read_capped(file_size, off, len, 16);
        assert_eq!(n, len);
        assert_eq!(
            runs, 4,
            "64 blocks at a 16-block cap must split into 4 runs"
        );
        assert_eq!(partials, 0);
        // Byte-identical to the naive reader (the read_run closure also asserts
        // count <= 16 on every call).
        let mut want = alloc::vec![0u8; len];
        reference_read(file_size, off, &mut want);
        assert_eq!(got, want);
    }

    #[test]
    fn coalesced_read_byte_identical_across_real_indirect_blocks() {
        // Unlike the arithmetic `mock_resolve` above, this drives the coalescer
        // through a REAL ext2 indirect walk: the resolve closure reads
        // single- and double-indirect *pointer blocks* from an in-memory store,
        // exactly as `resolve_block` does on disk. It proves the coalescer
        // MERGES a physically-contiguous run across the direct→single-indirect
        // (logical 11→12) and single→double-indirect (logical 267→268)
        // transitions — the boundaries the Phase 87 acceptance criterion names —
        // and BREAKS at a real discontiguity (a jump) and a hole, all
        // byte-for-byte identical to a naive per-block reader over the same walk.
        use alloc::collections::BTreeMap;
        let bs = TEST_BS as usize;
        let ptrs = TEST_BS as usize / 4; // 256 pointers per 1 KiB block

        // Physical DATA-block layout for logical block `lb`:
        //   0..280  → one contiguous run (phys 2000+lb) spanning BOTH boundaries
        //   280..285 → a second run reached by a discontiguous jump
        //   285      → a sparse hole (phys 0)
        //   286..    → a third contiguous run
        let phys_for_logical = |lb: u32| -> u32 {
            if lb == 285 {
                0
            } else if lb < 280 {
                2000 + lb
            } else if lb < 285 {
                900_000 + (lb - 280)
            } else {
                700_000 + lb
            }
        };

        // Materialise the pointer-block tree. Metadata pointer blocks live at
        // 100.. (never used as a data address by `phys_for_logical`).
        let sind_phys: u32 = 100; // single-indirect block (logical 12..12+ptrs)
        let dind_phys: u32 = 101; // double-indirect block (logical 12+ptrs..)
        let dind_l1_phys: u32 = 102; // its first second-level block
        let mut store: BTreeMap<u32, Vec<u8>> = BTreeMap::new();

        let mut direct = [0u32; 12];
        for (lb, d) in direct.iter_mut().enumerate() {
            *d = phys_for_logical(lb as u32);
        }
        let mut sind = alloc::vec![0u8; bs];
        for i in 0..ptrs {
            let p = phys_for_logical(12 + i as u32);
            sind[i * 4..i * 4 + 4].copy_from_slice(&p.to_le_bytes());
        }
        store.insert(sind_phys, sind);
        let mut dind = alloc::vec![0u8; bs];
        dind[0..4].copy_from_slice(&dind_l1_phys.to_le_bytes());
        store.insert(dind_phys, dind);
        let mut dind_l1 = alloc::vec![0u8; bs];
        for k in 0..ptrs {
            let p = phys_for_logical(12 + ptrs as u32 + k as u32); // logical 268+k
            dind_l1[k * 4..k * 4 + 4].copy_from_slice(&p.to_le_bytes());
        }
        store.insert(dind_l1_phys, dind_l1);

        // The REAL indirect walk (direct / single / double-indirect), reading
        // pointer blocks from `store` — the same shape as `resolve_block`.
        let real_resolve = |lb: u32| -> u32 {
            let read_ptr = |phys: u32, idx: usize| -> u32 {
                let blk = store.get(&phys).expect("pointer block present");
                u32::from_le_bytes([
                    blk[idx * 4],
                    blk[idx * 4 + 1],
                    blk[idx * 4 + 2],
                    blk[idx * 4 + 3],
                ])
            };
            if (lb as usize) < 12 {
                direct[lb as usize]
            } else if (lb as usize) < 12 + ptrs {
                read_ptr(sind_phys, lb as usize - 12)
            } else {
                let d = lb as usize - 12 - ptrs;
                let l1 = read_ptr(dind_phys, d / ptrs);
                read_ptr(l1, d % ptrs)
            }
        };

        // The walk reproduces the intended layout at every boundary of interest.
        for lb in [0u32, 11, 12, 13, 267, 268, 279, 280, 284, 285, 286, 299] {
            assert_eq!(
                real_resolve(lb),
                phys_for_logical(lb),
                "walk mismatch at lb={lb}"
            );
        }

        let file_size = 300 * TEST_BS as u64;
        let read_with = |offset: u64, len: usize, runs: &Cell<usize>| -> Vec<u8> {
            let mut buf = alloc::vec![0u8; len];
            read_file_data_coalesced::<()>(
                file_size,
                TEST_BS,
                offset,
                &mut buf,
                u32::MAX,
                |lb| Ok(real_resolve(lb)),
                |phys, count, dst| {
                    runs.set(runs.get() + 1);
                    for k in 0..count {
                        mock_block_bytes(
                            phys + k,
                            &mut dst[k as usize * bs..(k as usize + 1) * bs],
                        );
                    }
                    Ok(())
                },
                |phys, off, dst| {
                    let mut blk = alloc::vec![0u8; bs];
                    mock_block_bytes(phys, &mut blk);
                    dst.copy_from_slice(&blk[off..off + dst.len()]);
                    Ok(())
                },
            )
            .unwrap();
            buf
        };
        let ref_read = |offset: u64, len: usize| -> Vec<u8> {
            let bs64 = TEST_BS as u64;
            let mut buf = alloc::vec![0u8; len];
            let to_read = len.min((file_size - offset) as usize);
            let (mut done, mut pos) = (0usize, offset);
            while done < to_read {
                let lb = (pos / bs64) as u32;
                let off = (pos % bs64) as usize;
                let n = (bs - off).min(to_read - done);
                let phys = real_resolve(lb);
                if phys == 0 {
                    buf[done..done + n].fill(0);
                } else {
                    let mut blk = alloc::vec![0u8; bs];
                    mock_block_bytes(phys, &mut blk);
                    buf[done..done + n].copy_from_slice(&blk[off..off + n]);
                }
                done += n;
                pos += n as u64;
            }
            buf
        };

        for &(off, len) in &[
            (0u64, 300 * bs),               // whole file across both boundaries
            (11 * TEST_BS as u64, 4 * bs),  // straddle direct→single (11→12)
            (267 * TEST_BS as u64, 4 * bs), // straddle single→double (267→268)
            (279 * TEST_BS as u64, 8 * bs), // across the jump at 280
            (284 * TEST_BS as u64, 4 * bs), // across the hole at 285
        ] {
            let runs = Cell::new(0);
            let got = read_with(off, len, &runs);
            let want = ref_read(off, len);
            assert_eq!(got, want, "content mismatch at off={off} len={len}");
        }

        // The whole-file read collapses to exactly 3 device runs (logical
        // 0..280, 280..285, 286..300) plus the zero-filled hole at 285 (no
        // request): the coalescer merged across BOTH indirect boundaries (no
        // spurious break at logical 12 or 268) yet broke at the real jump and
        // the hole.
        let runs = Cell::new(0);
        let _ = read_with(0, 300 * bs, &runs);
        assert_eq!(
            runs.get(),
            3,
            "expected exactly 3 contiguous runs across the real indirect layout, got {}",
            runs.get()
        );
    }

    // -----------------------------------------------------------------------
    // Phase 88 Track C.2 — cross-implementation ext2 parity host tests
    //
    // These tests exercise the six shared functions (`read_inode`, `resolve_block`,
    // `read_file_data`, `read_directory_entries`, `lookup_in_directory`,
    // `resolve_path`) over an entirely in-memory `MockExt2` that implements the
    // `BlockReader` trait. A fix to any shared function is immediately visible to
    // both the kernel `Ext2Volume` and the ring-3 `vfs_server` `Ext2State`.
    // -----------------------------------------------------------------------

    use alloc::collections::BTreeMap;

    // Geometry constants for MockExt2 tests.
    const MOCK_BS: u32 = 1024; // block size
    const MOCK_INODE_SIZE: u32 = 128; // inode size
    const MOCK_IPG: u32 = 1024; // inodes per group (all in group 0)
    const MOCK_INODE_TABLE: u32 = 5; // first block of inode table for group 0
    // ptrs per block: 1024 / 4 = 256
    const MOCK_PTRS: u32 = MOCK_BS / 4;

    /// In-memory ext2 block store implementing `BlockReader`.
    ///
    /// Geometry: 1 KiB blocks, 128-byte inodes, 1024 inodes per group, inode
    /// table at block 5. Only group 0 is supported.
    struct MockExt2 {
        blocks: BTreeMap<u32, Vec<u8>>,
    }

    impl MockExt2 {
        fn new() -> Self {
            MockExt2 {
                blocks: BTreeMap::new(),
            }
        }

        /// Write `inode` (1-based) into the inode table.
        ///
        /// inode_size = 128, block_size = 1024 → 8 inodes per block.
        /// Inode `n` lives at byte offset `(n-1) * 128` from the start of the
        /// inode table.  Slot falls in block `MOCK_INODE_TABLE + byte_off/1024`,
        /// at `byte_off % 1024` within that block.
        fn put_inode(&mut self, num: u32, inode: &Ext2Inode) {
            let byte_off = (num - 1) as usize * MOCK_INODE_SIZE as usize;
            let blk = MOCK_INODE_TABLE + (byte_off / MOCK_BS as usize) as u32;
            let off_in_blk = byte_off % MOCK_BS as usize;
            let block = self
                .blocks
                .entry(blk)
                .or_insert_with(|| alloc::vec![0u8; MOCK_BS as usize]);
            inode.write_into(&mut block[off_in_blk..]);
        }

        /// Store a raw data block (zero-padded / truncated to `MOCK_BS`).
        fn put_data_block(&mut self, phys: u32, data: &[u8]) {
            let mut buf = alloc::vec![0u8; MOCK_BS as usize];
            let n = data.len().min(MOCK_BS as usize);
            buf[..n].copy_from_slice(&data[..n]);
            self.blocks.insert(phys, buf);
        }

        /// Serialize a sequence of `(name, inode, file_type)` directory entries
        /// into one ext2 dir block and store it at `phys`.
        ///
        /// Entry layout (mirrors `parse_directory_entries` byte layout):
        ///   [inode u32 LE][rec_len u16 LE][name_len u8][file_type u8][name…]
        /// Each entry is 4-byte aligned via `(8 + name_len + 3) & !3`.  The last
        /// entry's `rec_len` is stretched to reach the end of the block.
        fn put_dir_block(&mut self, phys: u32, entries: &[(&str, u32, u8)]) {
            let mut buf = alloc::vec![0u8; MOCK_BS as usize];
            let mut off = 0usize;
            let n = entries.len();
            for (i, &(name, ino, ft)) in entries.iter().enumerate() {
                let name_bytes = name.as_bytes();
                let name_len = name_bytes.len() as u8;
                let natural = (8 + name_len as usize + 3) & !3;
                let rec_len = if i == n - 1 {
                    MOCK_BS as usize - off // last entry fills to block end
                } else {
                    natural
                };
                buf[off..off + 4].copy_from_slice(&ino.to_le_bytes());
                buf[off + 4..off + 6].copy_from_slice(&(rec_len as u16).to_le_bytes());
                buf[off + 6] = name_len;
                buf[off + 7] = ft;
                buf[off + 8..off + 8 + name_bytes.len()].copy_from_slice(name_bytes);
                off += rec_len;
            }
            self.blocks.insert(phys, buf);
        }

        /// Write a block whose content is a sequence of `u32 LE` block pointers.
        /// `ptrs[i] = 0` encodes a sparse hole.
        fn put_ptr_block(&mut self, phys: u32, ptrs: &[u32]) {
            let mut buf = alloc::vec![0u8; MOCK_BS as usize];
            for (i, &p) in ptrs.iter().enumerate() {
                buf[i * 4..i * 4 + 4].copy_from_slice(&p.to_le_bytes());
            }
            self.blocks.insert(phys, buf);
        }
    }

    impl BlockReader for MockExt2 {
        fn block_size(&self) -> u32 {
            MOCK_BS
        }
        fn inodes_per_group(&self) -> u32 {
            MOCK_IPG
        }
        fn inode_size(&self) -> u32 {
            MOCK_INODE_SIZE
        }
        fn inode_table_block(&self, group: u32) -> Result<u32, Ext2Error> {
            if group == 0 {
                Ok(MOCK_INODE_TABLE)
            } else {
                Err(Ext2Error::CorruptedEntry)
            }
        }
        fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error> {
            self.blocks
                .get(&block_num)
                .cloned()
                .ok_or(Ext2Error::IoError)
        }
        // Use the default `max_run_blocks` / `read_block_run` / `read_block_into`
        // so this test also exercises those trait-default paths.
    }

    // -----------------------------------------------------------------------
    // Fixture builder: small filesystem used by most tests.
    //
    //   /            inode #2  (dir)
    //   /file.txt    inode #11 (regular, 2+ blocks of data)
    //   /sub         inode #12 (dir)
    //   /link        inode #13 (symlink)
    //   /sub/deep.txt inode #16 (regular)
    //
    // Data-block numbers used (must not overlap inode table or ptr blocks):
    //   root dir block 0: phys 50
    //   file.txt block 0: phys 200, block 1: phys 201
    //   sub dir block 0:  phys 60
    //   deep.txt block 0: phys 210
    //
    // Inode table: block 5 (inodes 1-8), block 6 (inodes 9-16).
    // -----------------------------------------------------------------------

    fn build_basic_fs() -> MockExt2 {
        let mut fs = MockExt2::new();

        // ------- root inode (#2, dir) -------
        let mut root = Ext2Inode::new_empty();
        root.mode = S_IFDIR | 0o755;
        root.links_count = 2;
        root.size = MOCK_BS; // one dir block
        root.block[0] = 50; // phys block 50
        fs.put_inode(2, &root);

        // root dir block: ., .., file.txt, sub, link
        fs.put_dir_block(
            50,
            &[
                (".", 2, EXT2_FT_DIR),
                ("..", 2, EXT2_FT_DIR),
                ("file.txt", 11, EXT2_FT_REG_FILE),
                ("sub", 12, EXT2_FT_DIR),
                ("link", 13, EXT2_FT_SYMLINK),
            ],
        );

        // ------- file.txt inode (#11, regular, 2 full blocks) -------
        let mut file_inode = Ext2Inode::new_empty();
        file_inode.mode = S_IFREG | 0o644;
        file_inode.links_count = 1;
        file_inode.size = 2 * MOCK_BS; // exactly 2 blocks
        file_inode.block[0] = 200;
        file_inode.block[1] = 201;
        fs.put_inode(11, &file_inode);

        // file.txt data: block 200 = bytes 0x41..., block 201 = bytes 0x61...
        let block0: Vec<u8> = (0..MOCK_BS as usize)
            .map(|i| 0x41u8.wrapping_add(i as u8))
            .collect();
        let block1: Vec<u8> = (0..MOCK_BS as usize)
            .map(|i| 0x61u8.wrapping_add(i as u8))
            .collect();
        fs.put_data_block(200, &block0);
        fs.put_data_block(201, &block1);

        // ------- sub inode (#12, dir) -------
        let mut sub_inode = Ext2Inode::new_empty();
        sub_inode.mode = S_IFDIR | 0o755;
        sub_inode.links_count = 2;
        sub_inode.size = MOCK_BS;
        sub_inode.block[0] = 60;
        fs.put_inode(12, &sub_inode);

        // sub dir block: ., .., deep.txt
        fs.put_dir_block(
            60,
            &[
                (".", 12, EXT2_FT_DIR),
                ("..", 2, EXT2_FT_DIR),
                ("deep.txt", 16, EXT2_FT_REG_FILE),
            ],
        );

        // ------- link inode (#13, symlink) -------
        let mut link_inode = Ext2Inode::new_empty();
        link_inode.mode = S_IFLNK | 0o777;
        link_inode.links_count = 1;
        link_inode.size = 8; // short inline symlink
        fs.put_inode(13, &link_inode);

        // ------- deep.txt inode (#16, regular, 1 block) -------
        // Inode 16: index 15 (0-based) → byte offset 15*128 = 1920 → block 5 + 1920/1024 = block 6
        let mut deep_inode = Ext2Inode::new_empty();
        deep_inode.mode = S_IFREG | 0o644;
        deep_inode.links_count = 1;
        deep_inode.size = 42;
        deep_inode.block[0] = 210;
        fs.put_inode(16, &deep_inode);

        let deep_data = b"deep content for deep.txt in sub dir!!!!";
        fs.put_data_block(210, deep_data);

        fs
    }

    // -----------------------------------------------------------------------
    // Test 1: path resolution and basic inode type checks
    // -----------------------------------------------------------------------

    #[test]
    fn shared_resolve_path_walks_nested_dirs() {
        let fs = build_basic_fs();

        assert_eq!(
            resolve_path(&fs, "/").unwrap(),
            EXT2_ROOT_INO,
            "root path resolves to inode 2"
        );
        assert_eq!(
            resolve_path(&fs, "/file.txt").unwrap(),
            11,
            "/file.txt must resolve to inode 11"
        );
        assert_eq!(
            resolve_path(&fs, "/sub").unwrap(),
            12,
            "/sub must resolve to inode 12"
        );
        assert_eq!(
            resolve_path(&fs, "/sub/deep.txt").unwrap(),
            16,
            "/sub/deep.txt must resolve to inode 16"
        );

        // NotFound for a missing entry.
        assert_eq!(
            resolve_path(&fs, "/nope").unwrap_err(),
            Ext2Error::NotFound,
            "/nope must be NotFound"
        );

        // NotDirectory when traversing through a regular file.
        assert_eq!(
            resolve_path(&fs, "/file.txt/x").unwrap_err(),
            Ext2Error::NotDirectory,
            "/file.txt/x must be NotDirectory (file.txt is not a dir)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: inode type predicates
    // -----------------------------------------------------------------------

    #[test]
    fn shared_read_inode_returns_correct_types() {
        let fs = build_basic_fs();

        let root = read_inode(&fs, 2).unwrap();
        assert!(root.is_dir(), "inode 2 (root) must be a directory");
        assert!(!root.is_regular(), "root must not be regular");

        let file = read_inode(&fs, 11).unwrap();
        assert!(file.is_regular(), "inode 11 (file.txt) must be regular");
        assert!(!file.is_dir(), "file.txt must not be a dir");

        let link = read_inode(&fs, 13).unwrap();
        assert!(link.is_symlink(), "inode 13 (link) must be a symlink");
        assert!(!link.is_dir(), "link must not be a dir");
        assert!(!link.is_regular(), "link must not be regular");
    }

    // -----------------------------------------------------------------------
    // Test 3: read_directory_entries returns all entries including . and ..
    // -----------------------------------------------------------------------

    #[test]
    fn shared_read_directory_entries_returns_all_entries() {
        let fs = build_basic_fs();
        let root = read_inode(&fs, 2).unwrap();

        let entries = read_directory_entries(&fs, &root).unwrap();
        // Must have 5 entries: ., .., file.txt, sub, link
        assert_eq!(entries.len(), 5, "root dir must have 5 entries");

        // Check specific entries are present with correct metadata.
        let dot = entries.iter().find(|(n, _, _)| n == ".").expect(". entry");
        assert_eq!(dot.1, 2, ". must point to inode 2");
        assert_eq!(dot.2, EXT2_FT_DIR, ". must have dir file_type");

        let dotdot = entries
            .iter()
            .find(|(n, _, _)| n == "..")
            .expect(".. entry");
        assert_eq!(dotdot.1, 2, ".. must point to inode 2 at root");
        assert_eq!(dotdot.2, EXT2_FT_DIR, ".. must have dir file_type");

        let ftxt = entries
            .iter()
            .find(|(n, _, _)| n == "file.txt")
            .expect("file.txt entry");
        assert_eq!(ftxt.1, 11, "file.txt must be inode 11");
        assert_eq!(
            ftxt.2, EXT2_FT_REG_FILE,
            "file.txt must be EXT2_FT_REG_FILE"
        );

        let sub = entries
            .iter()
            .find(|(n, _, _)| n == "sub")
            .expect("sub entry");
        assert_eq!(sub.1, 12, "sub must be inode 12");
        assert_eq!(sub.2, EXT2_FT_DIR, "sub must be EXT2_FT_DIR");

        let lnk = entries
            .iter()
            .find(|(n, _, _)| n == "link")
            .expect("link entry");
        assert_eq!(lnk.1, 13, "link must be inode 13");
        assert_eq!(lnk.2, EXT2_FT_SYMLINK, "link must be EXT2_FT_SYMLINK");
    }

    // -----------------------------------------------------------------------
    // Test 4: lookup_in_directory finds and misses correctly
    // -----------------------------------------------------------------------

    #[test]
    fn shared_lookup_in_directory_finds_and_misses() {
        let fs = build_basic_fs();
        let root = read_inode(&fs, 2).unwrap();

        assert_eq!(
            lookup_in_directory(&fs, &root, "sub").unwrap(),
            12,
            "lookup 'sub' must return inode 12"
        );
        assert_eq!(
            lookup_in_directory(&fs, &root, "file.txt").unwrap(),
            11,
            "lookup 'file.txt' must return inode 11"
        );
        assert_eq!(
            lookup_in_directory(&fs, &root, "missing").unwrap_err(),
            Ext2Error::NotFound,
            "lookup of missing name must be NotFound"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: read_file_data with direct blocks — full read, offset read,
    //         and past-EOF read
    // -----------------------------------------------------------------------

    #[test]
    fn shared_read_file_data_direct_blocks() {
        let fs = build_basic_fs();
        let inode = read_inode(&fs, 11).unwrap();
        let file_size = 2 * MOCK_BS as usize;

        // Build the expected full content.
        let expected: Vec<u8> = (0..MOCK_BS as usize)
            .map(|i| 0x41u8.wrapping_add(i as u8))
            .chain((0..MOCK_BS as usize).map(|i| 0x61u8.wrapping_add(i as u8)))
            .collect();

        // Full read.
        let mut buf = alloc::vec![0u8; file_size];
        let n = read_file_data(&fs, &inode, 0, &mut buf).unwrap();
        assert_eq!(
            n, file_size,
            "full read must return exactly file_size bytes"
        );
        assert_eq!(&buf[..n], &expected[..], "full read must be byte-identical");

        // Offset read: start mid-way through block 0.
        let offset = 500usize;
        let read_len = 600usize; // straddles the block boundary
        let mut buf2 = alloc::vec![0u8; read_len];
        let n2 = read_file_data(&fs, &inode, offset as u64, &mut buf2).unwrap();
        assert_eq!(n2, read_len, "mid-file read must return read_len bytes");
        assert_eq!(
            &buf2[..n2],
            &expected[offset..offset + read_len],
            "mid-file read must match expected slice"
        );

        // Read past EOF returns 0.
        let mut buf3 = alloc::vec![0u8; 64];
        let n3 = read_file_data(&fs, &inode, file_size as u64 + 1, &mut buf3).unwrap();
        assert_eq!(n3, 0, "read past EOF must return 0");
    }

    // -----------------------------------------------------------------------
    // Test 6: sparse/hole file — block[k]==0 reads back as zeros
    // -----------------------------------------------------------------------

    #[test]
    fn shared_read_file_data_sparse_hole_reads_as_zeros() {
        let mut fs = MockExt2::new();

        // File with 3 blocks: real(phys 300), hole(block[1]=0), real(phys 302).
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFREG | 0o644;
        inode.links_count = 1;
        inode.size = 3 * MOCK_BS;
        inode.block[0] = 300;
        inode.block[1] = 0; // sparse hole
        inode.block[2] = 302;
        fs.put_inode(11, &inode);

        let data_a: Vec<u8> = (0..MOCK_BS as usize)
            .map(|i| 0xAAu8.wrapping_add(i as u8))
            .collect();
        let data_c: Vec<u8> = (0..MOCK_BS as usize)
            .map(|i| 0xCCu8.wrapping_add(i as u8))
            .collect();
        fs.put_data_block(300, &data_a);
        fs.put_data_block(302, &data_c);

        let inode = read_inode(&fs, 11).unwrap();
        let mut buf = alloc::vec![0u8; 3 * MOCK_BS as usize];
        let n = read_file_data(&fs, &inode, 0, &mut buf).unwrap();
        assert_eq!(n, 3 * MOCK_BS as usize);

        // Block 0 matches data_a.
        assert_eq!(
            &buf[..MOCK_BS as usize],
            data_a.as_slice(),
            "block 0 must match data_a"
        );
        // Block 1 is a hole — must be all zeros.
        assert!(
            buf[MOCK_BS as usize..2 * MOCK_BS as usize]
                .iter()
                .all(|&b| b == 0),
            "sparse hole (block[1]=0) must read as zeros"
        );
        // Block 2 matches data_c.
        assert_eq!(
            &buf[2 * MOCK_BS as usize..],
            data_c.as_slice(),
            "block 2 must match data_c"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7: resolve_block across direct→single and single→double boundaries,
    //         and read_file_data byte-correctness across indirect boundaries
    // -----------------------------------------------------------------------

    #[test]
    fn shared_read_file_data_spans_indirect_boundaries() {
        let mut fs = MockExt2::new();
        // ptrs per 1 KiB block = 256.
        // single-indirect spans logical 12..12+256 = 12..268.
        // double-indirect spans logical 268..268+256*256 = 268..65804.
        //
        // Build a file of 270 blocks (covers both boundaries):
        //   logical 0..12  → direct (phys 1000+lb)
        //   logical 12..268 → single-indirect, contiguous (phys 1000+lb)
        //   logical 268..270 → double-indirect (phys 1000+lb)
        // All blocks are in one contiguous physical run starting at phys 1000.
        //
        // Pointer block layout:
        //   block[EXT2_IND_BLOCK]  = phys 900 (single-indirect ptr block)
        //   block[EXT2_DIND_BLOCK] = phys 901 (double-indirect L0 block)
        //   phys 902 = L1 block for the first double-indirect L1 pointer block

        let file_blocks: u32 = 270;
        let phys_base: u32 = 1000;
        let sind_phys: u32 = 900;
        let dind_phys: u32 = 901;
        let dind_l1_phys: u32 = 902;

        // Direct blocks: block[0..12] = phys 1000..1012.
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFREG | 0o644;
        inode.links_count = 1;
        inode.size = file_blocks * MOCK_BS;
        for lb in 0..12u32 {
            inode.block[lb as usize] = phys_base + lb;
        }
        inode.block[EXT2_IND_BLOCK] = sind_phys;
        inode.block[EXT2_DIND_BLOCK] = dind_phys;
        fs.put_inode(2, &inode); // use any free inode slot

        // Single-indirect pointer block: logical 12..268 → phys 1012..1268.
        let sind_ptrs: Vec<u32> = (0..MOCK_PTRS).map(|i| phys_base + 12 + i).collect();
        fs.put_ptr_block(sind_phys, &sind_ptrs);

        // Double-indirect L0: first entry points to L1 block at phys 902.
        let mut dind_ptrs = alloc::vec![0u32; MOCK_PTRS as usize];
        dind_ptrs[0] = dind_l1_phys;
        fs.put_ptr_block(dind_phys, &dind_ptrs);

        // Double-indirect L1: logical 268..270 → phys 1268..1270 (only 2 entries needed).
        let mut dind_l1_ptrs = alloc::vec![0u32; MOCK_PTRS as usize];
        for i in 0..2u32 {
            dind_l1_ptrs[i as usize] = phys_base + 268 + i;
        }
        fs.put_ptr_block(dind_l1_phys, &dind_l1_ptrs);

        // Populate all 270 data blocks with a recognizable pattern.
        for lb in 0..file_blocks {
            let phys = phys_base + lb;
            let data: Vec<u8> = (0..MOCK_BS as usize)
                .map(|i| (phys as usize).wrapping_add(i) as u8)
                .collect();
            fs.put_data_block(phys, &data);
        }

        // --- resolve_block at the direct→single boundary ---
        let lb11 = resolve_block(&fs, &inode, 11).unwrap();
        assert_eq!(
            lb11,
            phys_base + 11,
            "logical 11 (last direct) must resolve to phys 1011"
        );
        let lb12 = resolve_block(&fs, &inode, 12).unwrap();
        assert_eq!(
            lb12,
            phys_base + 12,
            "logical 12 (first single-indirect) must resolve to phys 1012"
        );

        // --- resolve_block at the single→double boundary ---
        let lb267 = resolve_block(&fs, &inode, 267).unwrap();
        assert_eq!(
            lb267,
            phys_base + 267,
            "logical 267 (last single-indirect) must resolve to phys 1267"
        );
        let lb268 = resolve_block(&fs, &inode, 268).unwrap();
        assert_eq!(
            lb268,
            phys_base + 268,
            "logical 268 (first double-indirect) must resolve to phys 1268"
        );

        // --- read_file_data byte-correctness across both boundaries ---
        // Read 4 blocks straddling direct→single (lb 11..15).
        let off_a = 11 * MOCK_BS as u64;
        let len_a = 4 * MOCK_BS as usize;
        let mut buf_a = alloc::vec![0u8; len_a];
        let na = read_file_data(&fs, &inode, off_a, &mut buf_a).unwrap();
        assert_eq!(
            na, len_a,
            "read across direct→single must return len_a bytes"
        );
        for lb in 11u32..15 {
            let phys = phys_base + lb;
            let blk_off = (lb - 11) as usize * MOCK_BS as usize;
            let expected: Vec<u8> = (0..MOCK_BS as usize)
                .map(|i| (phys as usize).wrapping_add(i) as u8)
                .collect();
            assert_eq!(
                &buf_a[blk_off..blk_off + MOCK_BS as usize],
                expected.as_slice(),
                "block lb={lb} content mismatch across direct→single boundary"
            );
        }

        // Read 4 blocks straddling single→double (lb 267..271 but file only has 270).
        let off_b = 267 * MOCK_BS as u64;
        let len_b = 3 * MOCK_BS as usize; // only 3 blocks left (267, 268, 269)
        let mut buf_b = alloc::vec![0u8; len_b];
        let nb = read_file_data(&fs, &inode, off_b, &mut buf_b).unwrap();
        assert_eq!(
            nb, len_b,
            "read across single→double must return len_b bytes"
        );
        for lb in 267u32..270 {
            let phys = phys_base + lb;
            let blk_off = (lb - 267) as usize * MOCK_BS as usize;
            let expected: Vec<u8> = (0..MOCK_BS as usize)
                .map(|i| (phys as usize).wrapping_add(i) as u8)
                .collect();
            assert_eq!(
                &buf_b[blk_off..blk_off + MOCK_BS as usize],
                expected.as_slice(),
                "block lb={lb} content mismatch across single→double boundary"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 8: triple-indirect is unsupported — resolve_block returns CorruptedEntry
    // -----------------------------------------------------------------------

    #[test]
    fn shared_resolve_block_triple_indirect_unsupported() {
        // ptrs_per_block = 256; double-indirect range = 256*256 = 65536 blocks.
        // Triple-indirect begins at logical 12 + 256 + 256*256 = 65804.
        let mut fs = MockExt2::new();
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFREG | 0o644;
        inode.size = 0; // size doesn't matter for resolve_block
        inode.block[EXT2_TIND_BLOCK] = 999; // triple-indirect pointer set
        fs.put_inode(2, &inode);

        let inode = read_inode(&fs, 2).unwrap();
        // Any logical block beyond the double-indirect range (12 + 256 + 65536 = 65804)
        // must return CorruptedEntry.
        assert_eq!(
            resolve_block(&fs, &inode, 65804).unwrap_err(),
            Ext2Error::CorruptedEntry,
            "logical block 65804 (start of triple-indirect range) must be CorruptedEntry"
        );
        assert_eq!(
            resolve_block(&fs, &inode, 70000).unwrap_err(),
            Ext2Error::CorruptedEntry,
            "logical block 70000 (well into triple-indirect range) must be CorruptedEntry"
        );
    }

    // -----------------------------------------------------------------------
    // Test 9: large directory spanning multiple blocks — read_directory_entries
    //         returns all entries; lookup finds one in the last block
    // -----------------------------------------------------------------------

    #[test]
    fn shared_read_directory_entries_multi_block() {
        let mut fs = MockExt2::new();

        // How many entries fit in one 1 KiB block?
        // Each entry uses (8 + name_len + 3) & !3 bytes.
        // With a 3-char name: natural = (8+3+3)&!3 = 12 bytes → 1024/12 = 85 entries.
        // We'll use 80 entries per block to be safe, spread across 4 blocks.
        // Total: 320 entries, in 4 dir blocks (block[0..4]).
        const ENTRIES_PER_BLOCK: usize = 80;
        const NUM_BLOCKS: usize = 4;
        const TOTAL: usize = ENTRIES_PER_BLOCK * NUM_BLOCKS;

        // Assign data blocks for the dir: phys 70, 71, 72, 73.
        let phys_base = 70u32;

        let mut dir_inode = Ext2Inode::new_empty();
        dir_inode.mode = S_IFDIR | 0o755;
        dir_inode.links_count = 2;
        dir_inode.size = (NUM_BLOCKS * MOCK_BS as usize) as u32;
        for b in 0..NUM_BLOCKS {
            dir_inode.block[b] = phys_base + b as u32;
        }
        fs.put_inode(2, &dir_inode);

        // Build a 3-char name like "f00", "f01", ..., "f319".
        // We'll use 4-char names "e000"..."e319" to keep them unique and same size.
        let all_names: Vec<String> = (0..TOTAL).map(|i| alloc::format!("e{i:03}")).collect();
        // Inode numbers: start at 100.
        let all_inodes: Vec<u32> = (0..TOTAL as u32).map(|i| 100 + i).collect();

        for blk in 0..NUM_BLOCKS {
            let start = blk * ENTRIES_PER_BLOCK;
            let end = start + ENTRIES_PER_BLOCK;
            let entries: Vec<(&str, u32, u8)> = (start..end)
                .map(|i| (all_names[i].as_str(), all_inodes[i], EXT2_FT_REG_FILE))
                .collect();
            fs.put_dir_block(phys_base + blk as u32, &entries);
        }

        let dir_inode = read_inode(&fs, 2).unwrap();

        // read_directory_entries must return all TOTAL entries.
        let entries = read_directory_entries(&fs, &dir_inode).unwrap();
        assert_eq!(
            entries.len(),
            TOTAL,
            "large dir must return all {TOTAL} entries"
        );

        // Verify the first entry (block 0) and the last entry (block 3).
        assert_eq!(
            entries[0],
            (alloc::string::String::from("e000"), 100, EXT2_FT_REG_FILE),
            "first entry must be e000 → inode 100"
        );
        let last_name = alloc::format!("e{:03}", TOTAL - 1);
        let last_ino = 100 + TOTAL as u32 - 1;
        assert_eq!(
            entries[TOTAL - 1],
            (last_name.clone(), last_ino, EXT2_FT_REG_FILE),
            "last entry must be {last_name} → inode {last_ino}"
        );

        // lookup_in_directory must find an entry in the last block.
        let found = lookup_in_directory(&fs, &dir_inode, &last_name).unwrap();
        assert_eq!(
            found, last_ino,
            "lookup of last-block entry must return {last_ino}"
        );

        // lookup_in_directory must return NotFound for a non-existent name.
        assert_eq!(
            lookup_in_directory(&fs, &dir_inode, "ghost").unwrap_err(),
            Ext2Error::NotFound,
            "lookup of non-existent 'ghost' in large dir must be NotFound"
        );
    }
}
