//! Phase 106 C.5 — from-scratch ext2 (rev 1) **format orchestration** plus a
//! minimal **file writer**, as pure host-testable logic.
//!
//! `kernel-core::fs::ext2` could read and write back an *existing* filesystem
//! (the structure serializers + the `BlockReader` read path) but nothing could
//! *create* one. This module adds:
//!
//! - [`BlockIo`] — the write-capable device seam. The formatter defines the
//!   block geometry; implementors map ext2 block numbers to their medium (an
//!   in-memory image in tests; the `0x117x` raw sector syscalls in the
//!   installer).
//! - [`format_ext2`] — lays down a complete rev-1 filesystem: primary +
//!   per-group backup superblocks and BGD tables (no `sparse_super`, so every
//!   group carries a backup), block/inode bitmaps, inode tables, the root
//!   directory and `lost+found`.
//! - [`Ext2Fs`] — a thin mounted-for-write handle over a freshly formatted (or
//!   any compatible) volume: block/inode allocation from the bitmaps and
//!   [`Ext2Fs::create_file`] / [`Ext2Fs::create_dir`], enough for the
//!   installer to populate a formatted partition file-by-file (the C.3 raw
//!   `dd`-copy's partition-aware alternative).
//!
//! # Feature posture
//!
//! `feature_incompat = FILETYPE (0x0002)` and nothing else. The typed
//! directory-entry byte is load-bearing across m3OS (`Ext2DirEntry::
//! parse_block` reads byte 7 as `file_type`), so the flag states what the
//! tree already assumes. No `sparse_super` keeps backup placement trivially
//! regular; no journal, no resize inode, 128-byte inodes.
//!
//! # What is deliberately NOT here
//!
//! Triple-indirect data (files > ~4 GiB at 4 KiB blocks), deletion/truncation,
//! and directory-entry removal — the installer's populate workload is
//! write-once onto a fresh volume.

use super::ext2::{
    EXT2_DIND_BLOCK, EXT2_FT_DIR, EXT2_FT_REG_FILE, EXT2_FT_SYMLINK, EXT2_IND_BLOCK, EXT2_MAGIC,
    EXT2_NDIR_BLOCKS, EXT2_ROOT_INO, Ext2BlockGroupDescriptor, Ext2Error, Ext2Inode,
    Ext2Superblock, S_IFDIR, S_IFLNK, S_IFREG, inode_block_group, inode_index_in_group,
};
use alloc::vec;
use alloc::vec::Vec;

/// First non-reserved inode number (rev 1 `s_first_ino`). Inodes 1–10 are
/// reserved; 11 is conventionally `lost+found`.
pub const EXT2_FIRST_INO: u32 = 11;

/// `lost+found`'s inode number on a freshly formatted volume.
pub const LOST_FOUND_INO: u32 = EXT2_FIRST_INO;

/// On-disk inode record size this formatter lays down (rev-1 minimum).
pub const FORMAT_INODE_SIZE: u32 = 128;

/// `s_feature_incompat` FILETYPE bit — typed directory entries.
pub const FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;

/// mke2fs-style default bytes-per-inode ratio used to size the inode tables.
const BYTES_PER_INODE: u32 = 16384;

// ---------------------------------------------------------------------------
// Device seam
// ---------------------------------------------------------------------------

/// Write-capable block device seam for the formatter and writer.
///
/// Block numbers are **ext2 block numbers** under the geometry the caller
/// passed to [`format_ext2`] (block 0 starts at byte 0 of the volume /
/// partition). `buf.len()` always equals the format block size.
pub trait BlockIo {
    /// Read one whole block into `buf`.
    fn read_block(&mut self, block: u32, buf: &mut [u8]) -> Result<(), Ext2Error>;
    /// Write one whole block from `data`.
    fn write_block(&mut self, block: u32, data: &[u8]) -> Result<(), Ext2Error>;
}

// ---------------------------------------------------------------------------
// Format parameters + derived geometry
// ---------------------------------------------------------------------------

/// Caller-chosen format parameters.
#[derive(Debug, Clone, Copy)]
pub struct FormatParams {
    /// Total volume size in ext2 blocks (of `1024 << block_size_log` bytes).
    pub total_blocks: u32,
    /// 0 → 1 KiB, 1 → 2 KiB, 2 → 4 KiB blocks (the sizes `Ext2Superblock::
    /// parse` accepts).
    pub block_size_log: u32,
    /// Volume UUID (`s_uuid`). The formatter has no entropy source — the
    /// caller supplies one (the installer can derive it from a clock).
    pub uuid: [u8; 16],
}

/// Derived layout for one format run. Also the falsifiable geometry the
/// host tests assert on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatGeometry {
    pub block_size: u32,
    pub first_data_block: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub group_count: u32,
    /// Blocks occupied by the BGD table (same in every group).
    pub bgd_blocks: u32,
    /// Blocks occupied by one group's inode table.
    pub inode_table_blocks: u32,
    /// Per-group metadata overhead: sb copy + BGD table + both bitmaps +
    /// inode table.
    pub overhead_blocks: u32,
    pub inodes_count: u32,
}

impl FormatGeometry {
    /// Compute the layout for `params`, validating that the volume is large
    /// enough to hold at least one group's metadata plus the two bootstrap
    /// directory blocks (root + `lost+found`).
    pub fn derive(params: &FormatParams) -> Result<FormatGeometry, Ext2Error> {
        if params.block_size_log > 2 {
            return Err(Ext2Error::InvalidBlockSize);
        }
        let block_size = 1024u32 << params.block_size_log;
        // Block 0 holds the boot record; with 1 KiB blocks the superblock is
        // its own block 1, with larger blocks it lives inside block 0.
        let first_data_block = if block_size == 1024 { 1 } else { 0 };
        // One block-bitmap block addresses 8 × block_size blocks.
        let blocks_per_group = 8 * block_size;

        if params.total_blocks <= first_data_block {
            return Err(Ext2Error::OutOfSpace);
        }
        let addressable = params.total_blocks - first_data_block;
        let group_count = addressable.div_ceil(blocks_per_group);

        // mke2fs-style inode budget: one inode per BYTES_PER_INODE of volume,
        // spread evenly across groups, rounded up to a whole inode-table
        // block so no table block is partially outside the table, and capped
        // at what one inode-bitmap block can address.
        let inodes_per_block = block_size / FORMAT_INODE_SIZE;
        let raw_ipg = (blocks_per_group / (BYTES_PER_INODE / block_size)).max(inodes_per_block);
        let inodes_per_group = raw_ipg
            .div_ceil(inodes_per_block)
            .saturating_mul(inodes_per_block)
            .min(8 * block_size);
        let inode_table_blocks = (inodes_per_group * FORMAT_INODE_SIZE).div_ceil(block_size);

        let bgd_blocks = (group_count * 32).div_ceil(block_size);
        // sb copy + BGD table + block bitmap + inode bitmap + inode table.
        let overhead_blocks = 1 + bgd_blocks + 1 + 1 + inode_table_blocks;

        // Group 0 must fit its metadata plus the root and lost+found data
        // blocks; refuse degenerate volumes rather than emit garbage.
        let group0_blocks = addressable.min(blocks_per_group);
        if group0_blocks < overhead_blocks + 2 {
            return Err(Ext2Error::OutOfSpace);
        }
        // Every later group must at least hold its own metadata.
        if group_count > 1 {
            let last_blocks = addressable - (group_count - 1) * blocks_per_group;
            if last_blocks <= overhead_blocks {
                return Err(Ext2Error::OutOfSpace);
            }
        }

        // `inodes_per_group * group_count` stays well under u32::MAX for every
        // volume the current inode ratio produces (a 16 TiB 4 KiB-block volume
        // lands near ~1.07e9), but the product is unchecked arithmetic on
        // caller-controlled `total_blocks` — a future ratio change or a
        // pathological param must not silently wrap into a bogus on-disk
        // `s_inodes_count`. Reject rather than overflow.
        let inodes_count = inodes_per_group
            .checked_mul(group_count)
            .ok_or(Ext2Error::OutOfSpace)?;

        Ok(FormatGeometry {
            block_size,
            first_data_block,
            blocks_per_group,
            inodes_per_group,
            group_count,
            bgd_blocks,
            inode_table_blocks,
            overhead_blocks,
            inodes_count,
        })
    }

    /// First block of group `g` (the superblock-copy block).
    pub fn group_start(&self, g: u32) -> u32 {
        self.first_data_block + g * self.blocks_per_group
    }

    /// Number of blocks that really exist in group `g` (the last group of a
    /// non-multiple volume is short).
    pub fn blocks_in_group(&self, g: u32, total_blocks: u32) -> u32 {
        let start = self.group_start(g);
        (total_blocks - start).min(self.blocks_per_group)
    }

    /// Block-bitmap block of group `g`.
    pub fn block_bitmap_block(&self, g: u32) -> u32 {
        self.group_start(g) + 1 + self.bgd_blocks
    }

    /// Inode-bitmap block of group `g`.
    pub fn inode_bitmap_block(&self, g: u32) -> u32 {
        self.block_bitmap_block(g) + 1
    }

    /// First inode-table block of group `g`.
    pub fn inode_table_block(&self, g: u32) -> u32 {
        self.inode_bitmap_block(g) + 1
    }

    /// First data block of group `g` (right after the inode table).
    pub fn first_group_data_block(&self, g: u32) -> u32 {
        self.inode_table_block(g) + self.inode_table_blocks
    }
}

// ---------------------------------------------------------------------------
// Directory-entry encoding (the write-side dual of Ext2DirEntry::parse_block)
// ---------------------------------------------------------------------------

/// Byte length a directory entry occupies: 8-byte header + name, rounded up
/// to 4-byte alignment.
fn dirent_len(name_len: usize) -> usize {
    (8 + name_len + 3) & !3
}

/// Encode one directory entry at `buf[off..]` with an explicit `rec_len`.
fn encode_dirent(buf: &mut [u8], off: usize, ino: u32, rec_len: u16, file_type: u8, name: &str) {
    let name_bytes = name.as_bytes();
    buf[off..off + 4].copy_from_slice(&ino.to_le_bytes());
    buf[off + 4..off + 6].copy_from_slice(&rec_len.to_le_bytes());
    buf[off + 6] = name_bytes.len() as u8;
    buf[off + 7] = file_type;
    buf[off + 8..off + 8 + name_bytes.len()].copy_from_slice(name_bytes);
}

/// Build a fresh single-block directory containing exactly the given entries,
/// the last entry's `rec_len` stretched to the block end.
fn build_dir_block(block_size: usize, entries: &[(&str, u32, u8)]) -> Vec<u8> {
    let mut buf = vec![0u8; block_size];
    let mut off = 0usize;
    for (i, &(name, ino, ft)) in entries.iter().enumerate() {
        let rec_len = if i == entries.len() - 1 {
            block_size - off
        } else {
            dirent_len(name.len())
        };
        encode_dirent(&mut buf, off, ino, rec_len as u16, ft, name);
        off += rec_len;
    }
    buf
}

// ---------------------------------------------------------------------------
// Bitmap helpers
// ---------------------------------------------------------------------------

#[inline]
fn bitmap_set(bitmap: &mut [u8], bit: u32) {
    bitmap[(bit / 8) as usize] |= 1 << (bit % 8);
}

#[inline]
fn bitmap_get(bitmap: &[u8], bit: u32) -> bool {
    bitmap[(bit / 8) as usize] & (1 << (bit % 8)) != 0
}

/// Index of the first clear bit below `limit`, or `None`.
///
/// Scans **byte-wise**: a fully-allocated byte (`0xFF`) is skipped in one step
/// rather than eight bit tests, then the first clear bit inside the first
/// non-full byte is located. `alloc_block`/`alloc_inode` call this once per
/// allocation and (with the low-end fill pattern the writer produces) the
/// clear region always sits just past the already-allocated prefix, so byte
/// skipping keeps a large populate close to linear instead of the O(n²) a
/// bit-by-bit rescan-from-zero would cost.
fn bitmap_find_clear(bitmap: &[u8], limit: u32) -> Option<u32> {
    let full_bytes = (limit / 8) as usize;
    // First whole byte that is not fully allocated → its first 0 bit
    // (LSB-first, so `trailing_ones` is the clear bit's index within the byte).
    let in_full = bitmap
        .iter()
        .take(full_bytes)
        .enumerate()
        .find_map(|(byte_idx, &byte)| {
            (byte != 0xFF).then(|| byte_idx as u32 * 8 + byte.trailing_ones())
        });
    // Tail: the final partial byte holds bits [full_bytes*8, limit).
    in_full.or_else(|| {
        let tail_start = full_bytes as u32 * 8;
        (tail_start..limit).find(|&bit| !bitmap_get(bitmap, bit))
    })
}

// ---------------------------------------------------------------------------
// format_ext2
// ---------------------------------------------------------------------------

/// Serialize the superblock (plus the fields outside the parsed struct:
/// feature flags, UUID, backup group number) into a full block image.
fn superblock_block(
    sb: &Ext2Superblock,
    uuid: &[u8; 16],
    geo: &FormatGeometry,
    group_nr: u16,
) -> Vec<u8> {
    let mut block = vec![0u8; geo.block_size as usize];
    // Byte offset of the 1024-byte superblock within its block:
    //   - 1 KiB blocks: the superblock IS the block (offset 0), for the
    //     primary (block 1) and every backup (each group's first block).
    //   - >1 KiB blocks: the PRIMARY sits at offset 1024 of block 0 (the
    //     first 1024 bytes are the boot record); BACKUPS sit at offset 0 of
    //     their group's first block (no boot record there). This primary-vs-
    //     backup asymmetry is exactly what e2fsprogs lays down.
    let off = if geo.block_size == 1024 || group_nr != 0 {
        0
    } else {
        1024
    };
    let buf = &mut block[off..off + 1024];
    sb.write_full_into(buf);
    // s_block_group_nr — which group this copy lives in (fsck uses it when
    // recovering from a backup).
    buf[90..92].copy_from_slice(&group_nr.to_le_bytes());
    // Feature flags: FILETYPE only (see module docs).
    buf[92..96].copy_from_slice(&0u32.to_le_bytes());
    buf[96..100].copy_from_slice(&FEATURE_INCOMPAT_FILETYPE.to_le_bytes());
    buf[100..104].copy_from_slice(&0u32.to_le_bytes());
    buf[104..120].copy_from_slice(uuid);
    block
}

/// Format the volume behind `io` as a fresh rev-1 ext2 filesystem.
///
/// Returns the derived [`FormatGeometry`] so callers (and tests) can assert
/// on the layout without re-deriving it.
pub fn format_ext2<IO: BlockIo + ?Sized>(
    io: &mut IO,
    params: &FormatParams,
) -> Result<FormatGeometry, Ext2Error> {
    let geo = FormatGeometry::derive(params)?;
    let bs = geo.block_size as usize;

    // ---- Bootstrap directory data blocks (group 0) ------------------------
    let root_dir_block = geo.first_group_data_block(0);
    let lf_dir_block = root_dir_block + 1;

    // ---- Per-group free accounting ----------------------------------------
    let mut group_free_blocks = vec![0u32; geo.group_count as usize];
    let mut group_free_inodes = vec![0u32; geo.group_count as usize];
    for g in 0..geo.group_count {
        let in_group = geo.blocks_in_group(g, params.total_blocks);
        group_free_blocks[g as usize] = in_group - geo.overhead_blocks;
        group_free_inodes[g as usize] = geo.inodes_per_group;
    }
    // Group 0: reserved inodes 1..=10 + lost+found (11); root + lost+found
    // data blocks.
    group_free_blocks[0] -= 2;
    group_free_inodes[0] -= EXT2_FIRST_INO;

    let free_blocks_total: u32 = group_free_blocks.iter().sum();
    let free_inodes_total: u32 = group_free_inodes.iter().sum();

    // ---- Superblock --------------------------------------------------------
    let sb = Ext2Superblock {
        inodes_count: geo.inodes_count,
        blocks_count: params.total_blocks,
        r_blocks_count: 0,
        free_blocks_count: free_blocks_total,
        free_inodes_count: free_inodes_total,
        first_data_block: geo.first_data_block,
        log_block_size: params.block_size_log,
        log_frag_size: params.block_size_log,
        blocks_per_group: geo.blocks_per_group,
        frags_per_group: geo.blocks_per_group,
        inodes_per_group: geo.inodes_per_group,
        mtime: 0,
        wtime: 0,
        mnt_count: 0,
        max_mnt_count: 0xFFFF, // -1: never force a check by mount count
        magic: EXT2_MAGIC,
        state: 1,  // clean
        errors: 1, // continue on error
        minor_rev_level: 0,
        lastcheck: 0,
        checkinterval: 0,
        creator_os: 0, // Linux
        rev_level: 1,
        def_resuid: 0,
        def_resgid: 0,
        first_ino: EXT2_FIRST_INO,
        inode_size: FORMAT_INODE_SIZE as u16,
    };

    // ---- BGD table (identical copy written into every group) ---------------
    let mut bgd_table = vec![0u8; geo.bgd_blocks as usize * bs];
    for g in 0..geo.group_count {
        let bgd = Ext2BlockGroupDescriptor {
            block_bitmap: geo.block_bitmap_block(g),
            inode_bitmap: geo.inode_bitmap_block(g),
            inode_table: geo.inode_table_block(g),
            free_blocks_count: group_free_blocks[g as usize] as u16,
            free_inodes_count: group_free_inodes[g as usize] as u16,
            used_dirs_count: if g == 0 { 2 } else { 0 }, // root + lost+found
        };
        bgd.write_into(&mut bgd_table[g as usize * 32..]);
    }

    // ---- Write every group -------------------------------------------------
    let zero_block = vec![0u8; bs];
    for g in 0..geo.group_count {
        let start = geo.group_start(g);
        let in_group = geo.blocks_in_group(g, params.total_blocks);

        // Superblock copy (primary in group 0). For 1 KiB blocks every copy is
        // its group's own first block. For larger blocks the layout is
        // asymmetric (see `superblock_block`): group 0's PRIMARY sits at byte
        // 1024 of block 0 (after the boot record), while each BACKUP sits at
        // offset 0 of its group's first block.
        io.write_block(start, &superblock_block(&sb, &params.uuid, &geo, g as u16))?;

        // BGD table.
        for b in 0..geo.bgd_blocks {
            let off = b as usize * bs;
            io.write_block(start + 1 + b, &bgd_table[off..off + bs])?;
        }

        // Block bitmap: metadata (and group 0's two directory blocks) used,
        // tail bits past the group's real end marked used so the allocator
        // can never hand them out.
        let mut bbm = vec![0u8; bs];
        for bit in 0..geo.overhead_blocks {
            bitmap_set(&mut bbm, bit);
        }
        if g == 0 {
            bitmap_set(&mut bbm, root_dir_block - start);
            bitmap_set(&mut bbm, lf_dir_block - start);
        }
        for bit in in_group..geo.blocks_per_group {
            bitmap_set(&mut bbm, bit);
        }
        io.write_block(geo.block_bitmap_block(g), &bbm)?;

        // Inode bitmap: group 0 reserves inodes 1..=11; tail bits past
        // inodes_per_group marked used (bitmap block addresses more bits
        // than there are inodes).
        let mut ibm = vec![0u8; bs];
        if g == 0 {
            for bit in 0..EXT2_FIRST_INO {
                bitmap_set(&mut ibm, bit);
            }
        }
        for bit in geo.inodes_per_group..(8 * geo.block_size) {
            bitmap_set(&mut ibm, bit);
        }
        io.write_block(geo.inode_bitmap_block(g), &ibm)?;

        // Inode table: zeroed.
        for b in 0..geo.inode_table_blocks {
            io.write_block(geo.inode_table_block(g) + b, &zero_block)?;
        }
    }

    // ---- Root + lost+found -------------------------------------------------
    let sectors_per_block = geo.block_size / 512;

    let mut root = Ext2Inode::new_empty();
    root.mode = S_IFDIR | 0o755;
    root.size = geo.block_size;
    root.links_count = 3; // ".", "..", and lost+found's ".."
    root.blocks = sectors_per_block;
    root.block[0] = root_dir_block;
    write_inode_raw(io, &geo, EXT2_ROOT_INO, &root)?;

    let mut lf = Ext2Inode::new_empty();
    lf.mode = S_IFDIR | 0o700;
    lf.size = geo.block_size;
    lf.links_count = 2; // "." and root's entry
    lf.blocks = sectors_per_block;
    lf.block[0] = lf_dir_block;
    write_inode_raw(io, &geo, LOST_FOUND_INO, &lf)?;

    io.write_block(
        root_dir_block,
        &build_dir_block(
            bs,
            &[
                (".", EXT2_ROOT_INO, EXT2_FT_DIR),
                ("..", EXT2_ROOT_INO, EXT2_FT_DIR),
                ("lost+found", LOST_FOUND_INO, EXT2_FT_DIR),
            ],
        ),
    )?;
    io.write_block(
        lf_dir_block,
        &build_dir_block(
            bs,
            &[
                (".", LOST_FOUND_INO, EXT2_FT_DIR),
                ("..", EXT2_ROOT_INO, EXT2_FT_DIR),
            ],
        ),
    )?;

    Ok(geo)
}

/// Write inode `ino` (1-based) into its table slot.
fn write_inode_raw<IO: BlockIo + ?Sized>(
    io: &mut IO,
    geo: &FormatGeometry,
    ino: u32,
    inode: &Ext2Inode,
) -> Result<(), Ext2Error> {
    let g = inode_block_group(ino, geo.inodes_per_group);
    let index = inode_index_in_group(ino, geo.inodes_per_group);
    let byte_off = index as u64 * FORMAT_INODE_SIZE as u64;
    let block = geo.inode_table_block(g) + (byte_off / geo.block_size as u64) as u32;
    let off = (byte_off % geo.block_size as u64) as usize;
    let mut buf = vec![0u8; geo.block_size as usize];
    io.read_block(block, &mut buf)?;
    inode.write_into(&mut buf[off..]);
    io.write_block(block, &buf)
}

// ---------------------------------------------------------------------------
// Ext2Fs — mounted-for-write handle (allocation + file/dir creation)
// ---------------------------------------------------------------------------

/// A write handle over a formatted volume: bitmap-backed block/inode
/// allocation plus write-once file and directory creation.
///
/// State (superblock + BGD free counts) is held in memory and flushed to the
/// **primary** copies by [`Ext2Fs::flush`]; per-group backups keep their
/// format-time counts (they exist for fsck recovery, not steady-state reads —
/// the same policy in-kernel writeback follows).
///
/// # Failure model — no rollback (abort-and-reformat)
///
/// The `create_*` methods mutate on-disk bitmaps, inode tables, and directory
/// blocks **incrementally with no journal or undo**. If any step fails
/// partway — an [`BlockIo`] I/O error, or [`Ext2Error::OutOfSpace`] when a
/// data-block allocation or a directory-block extension can't be satisfied —
/// the volume is left **partially modified** (e.g. an inode allocated in the
/// bitmap but with no directory entry, or a directory grown by one block whose
/// inode `i_size` update never landed). There is no transactional recovery.
///
/// Callers (the installer's populate path) MUST treat any error from a
/// `create_*`/`alloc_*`/`flush` call as **fatal for the whole volume**: abort
/// the install and re-run [`format_ext2`] before retrying, rather than
/// continuing to write into a half-updated filesystem. The in-memory free
/// counts track the on-disk bitmaps (each is decremented only after its bitmap
/// write succeeds), so the hazard is not a lost count but a **logical
/// inconsistency** — an orphaned inode or data block, or a stale link count /
/// `i_size` — that a later [`Ext2Fs::flush`] would then durably record.
pub struct Ext2Fs {
    pub sb: Ext2Superblock,
    pub geo: FormatGeometry,
    total_blocks: u32,
    bgds: Vec<Ext2BlockGroupDescriptor>,
}

impl Ext2Fs {
    /// Open a volume previously laid down by [`format_ext2`] (or any volume
    /// with the same fixed layout assumptions: 128-byte inodes, backup-in-
    /// every-group geometry).
    pub fn open<IO: BlockIo + ?Sized>(io: &mut IO, block_size_log: u32) -> Result<Self, Ext2Error> {
        if block_size_log > 2 {
            return Err(Ext2Error::InvalidBlockSize);
        }
        let bs = 1024u32 << block_size_log;
        // The superblock lives at byte 1024: block 1 for 1 KiB blocks, inside
        // block 0 otherwise.
        let mut buf = vec![0u8; bs as usize];
        let (sb_block, sb_off) = if bs == 1024 { (1, 0) } else { (0, 1024) };
        io.read_block(sb_block, &mut buf)?;
        let sb = Ext2Superblock::parse(&buf[sb_off..])?;
        if sb.block_size() != bs {
            return Err(Ext2Error::InvalidBlockSize);
        }
        let geo = FormatGeometry::derive(&FormatParams {
            total_blocks: sb.blocks_count,
            block_size_log,
            uuid: [0; 16],
        })?;
        // The volume must agree with the derived layout — this handle's
        // allocators assume it.
        if geo.blocks_per_group != sb.blocks_per_group
            || geo.inodes_per_group != sb.inodes_per_group
            || geo.group_count != sb.block_group_count()
        {
            return Err(Ext2Error::CorruptedEntry);
        }

        // Load the primary BGD table.
        let mut table = vec![0u8; geo.bgd_blocks as usize * bs as usize];
        for b in 0..geo.bgd_blocks {
            let off = b as usize * bs as usize;
            let mut blk = vec![0u8; bs as usize];
            io.read_block(geo.group_start(0) + 1 + b, &mut blk)?;
            table[off..off + bs as usize].copy_from_slice(&blk);
        }
        let bgds = Ext2BlockGroupDescriptor::parse_table(&table, geo.group_count)?;

        Ok(Ext2Fs {
            total_blocks: sb.blocks_count,
            sb,
            geo,
            bgds,
        })
    }

    /// Allocate one data block, preferring `hint_group`. Returns the absolute
    /// block number.
    pub fn alloc_block<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        hint_group: u32,
    ) -> Result<u32, Ext2Error> {
        let gc = self.geo.group_count;
        for i in 0..gc {
            let g = (hint_group + i) % gc;
            if self.bgds[g as usize].free_blocks_count == 0 {
                continue;
            }
            let bbm_block = self.geo.block_bitmap_block(g);
            let mut bbm = vec![0u8; self.geo.block_size as usize];
            io.read_block(bbm_block, &mut bbm)?;
            let in_group = self.geo.blocks_in_group(g, self.total_blocks);
            if let Some(bit) = bitmap_find_clear(&bbm, in_group) {
                bitmap_set(&mut bbm, bit);
                io.write_block(bbm_block, &bbm)?;
                self.bgds[g as usize].free_blocks_count -= 1;
                self.sb.free_blocks_count -= 1;
                return Ok(self.geo.group_start(g) + bit);
            }
        }
        Err(Ext2Error::OutOfSpace)
    }

    /// Allocate one inode, preferring `hint_group`. Returns the inode number
    /// (1-based).
    pub fn alloc_inode<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        hint_group: u32,
        is_dir: bool,
    ) -> Result<u32, Ext2Error> {
        let gc = self.geo.group_count;
        for i in 0..gc {
            let g = (hint_group + i) % gc;
            if self.bgds[g as usize].free_inodes_count == 0 {
                continue;
            }
            let ibm_block = self.geo.inode_bitmap_block(g);
            let mut ibm = vec![0u8; self.geo.block_size as usize];
            io.read_block(ibm_block, &mut ibm)?;
            if let Some(bit) = bitmap_find_clear(&ibm, self.geo.inodes_per_group) {
                bitmap_set(&mut ibm, bit);
                io.write_block(ibm_block, &ibm)?;
                self.bgds[g as usize].free_inodes_count -= 1;
                if is_dir {
                    self.bgds[g as usize].used_dirs_count += 1;
                }
                self.sb.free_inodes_count -= 1;
                return Ok(g * self.geo.inodes_per_group + bit + 1);
            }
        }
        Err(Ext2Error::OutOfSpace)
    }

    /// Write inode `ino`'s on-disk record.
    pub fn write_inode<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        ino: u32,
        inode: &Ext2Inode,
    ) -> Result<(), Ext2Error> {
        write_inode_raw(io, &self.geo, ino, inode)
    }

    /// Read inode `ino`'s on-disk record.
    pub fn read_inode<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        ino: u32,
    ) -> Result<Ext2Inode, Ext2Error> {
        let g = inode_block_group(ino, self.geo.inodes_per_group);
        let index = inode_index_in_group(ino, self.geo.inodes_per_group);
        let byte_off = index as u64 * FORMAT_INODE_SIZE as u64;
        let block = self.geo.inode_table_block(g) + (byte_off / self.geo.block_size as u64) as u32;
        let off = (byte_off % self.geo.block_size as u64) as usize;
        let mut buf = vec![0u8; self.geo.block_size as usize];
        io.read_block(block, &mut buf)?;
        Ext2Inode::parse(&buf[off..])
    }

    /// Store `data` as the block list of a fresh inode: direct, then
    /// single-indirect, then double-indirect. Returns `(block_ptrs, total_
    /// allocated_blocks)` — the pointer array for the inode plus the count of
    /// every block allocated (data + indirect), for `i_blocks`.
    fn write_file_blocks<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        hint_group: u32,
        data: &[u8],
    ) -> Result<([u32; 15], u32), Ext2Error> {
        let bs = self.geo.block_size as usize;
        let ptrs_per_block = bs / 4;
        let data_blocks = data.len().div_ceil(bs);
        let mut block_ptrs = [0u32; 15];
        let mut allocated = 0u32;

        // Cap: direct + single + double indirect.
        let max_blocks = EXT2_NDIR_BLOCKS + ptrs_per_block + ptrs_per_block * ptrs_per_block;
        if data_blocks > max_blocks {
            return Err(Ext2Error::OutOfSpace);
        }

        let write_data_block = |fs: &mut Self, io: &mut IO, i: usize| -> Result<u32, Ext2Error> {
            let blk = fs.alloc_block(io, hint_group)?;
            let start = i * bs;
            let end = (start + bs).min(data.len());
            let mut buf = vec![0u8; bs];
            buf[..end - start].copy_from_slice(&data[start..end]);
            io.write_block(blk, &buf)?;
            Ok(blk)
        };

        // Direct.
        for (i, ptr) in block_ptrs
            .iter_mut()
            .take(EXT2_NDIR_BLOCKS.min(data_blocks))
            .enumerate()
        {
            *ptr = write_data_block(self, io, i)?;
            allocated += 1;
        }

        // Single indirect.
        if data_blocks > EXT2_NDIR_BLOCKS {
            let count = (data_blocks - EXT2_NDIR_BLOCKS).min(ptrs_per_block);
            let ind_block = self.alloc_block(io, hint_group)?;
            allocated += 1;
            let mut ind = vec![0u8; bs];
            for j in 0..count {
                let blk = write_data_block(self, io, EXT2_NDIR_BLOCKS + j)?;
                allocated += 1;
                ind[j * 4..j * 4 + 4].copy_from_slice(&blk.to_le_bytes());
            }
            io.write_block(ind_block, &ind)?;
            block_ptrs[EXT2_IND_BLOCK] = ind_block;
        }

        // Double indirect.
        let dind_base = EXT2_NDIR_BLOCKS + ptrs_per_block;
        if data_blocks > dind_base {
            let remaining = data_blocks - dind_base;
            let dind_block = self.alloc_block(io, hint_group)?;
            allocated += 1;
            let mut dind = vec![0u8; bs];
            let ind_count = remaining.div_ceil(ptrs_per_block);
            for k in 0..ind_count {
                let ind_block = self.alloc_block(io, hint_group)?;
                allocated += 1;
                let mut ind = vec![0u8; bs];
                let count = (remaining - k * ptrs_per_block).min(ptrs_per_block);
                for j in 0..count {
                    let blk = write_data_block(self, io, dind_base + k * ptrs_per_block + j)?;
                    allocated += 1;
                    ind[j * 4..j * 4 + 4].copy_from_slice(&blk.to_le_bytes());
                }
                io.write_block(ind_block, &ind)?;
                dind[k * 4..k * 4 + 4].copy_from_slice(&ind_block.to_le_bytes());
            }
            io.write_block(dind_block, &dind)?;
            block_ptrs[EXT2_DIND_BLOCK] = dind_block;
        }

        Ok((block_ptrs, allocated))
    }

    /// Insert `(name → ino)` into directory `parent_ino`, extending the
    /// directory by one block when no slot fits.
    fn add_dir_entry<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        parent_ino: u32,
        name: &str,
        ino: u32,
        file_type: u8,
    ) -> Result<(), Ext2Error> {
        if name.is_empty() || name.len() > 255 {
            return Err(Ext2Error::CorruptedEntry);
        }
        let bs = self.geo.block_size as usize;
        let mut parent = self.read_inode(io, parent_ino)?;
        if !parent.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }
        let need = dirent_len(name.len());
        let dir_blocks = (parent.size as usize).div_ceil(bs);
        // Directories this writer produces stay within the 12 direct
        // pointers (a 4 KiB-block directory holds ~48 K entries by then).
        for lb in 0..dir_blocks.min(EXT2_NDIR_BLOCKS) {
            let phys = parent.block[lb];
            if phys == 0 {
                continue;
            }
            let mut buf = vec![0u8; bs];
            io.read_block(phys, &mut buf)?;
            // Walk the entry chain looking for trailing slack to split.
            let mut off = 0usize;
            while off + 8 <= bs {
                let rec_len = u16::from_le_bytes([buf[off + 4], buf[off + 5]]) as usize;
                if rec_len < 8 || off + rec_len > bs {
                    return Err(Ext2Error::CorruptedEntry);
                }
                let entry_ino =
                    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                let name_len = buf[off + 6] as usize;
                let used = if entry_ino == 0 {
                    0
                } else {
                    dirent_len(name_len)
                };
                if rec_len - used >= need {
                    // Split: shrink the live entry to its natural size, put
                    // the new entry in the slack.
                    let (new_off, new_rec) = if entry_ino == 0 {
                        (off, rec_len)
                    } else {
                        buf[off + 4..off + 6].copy_from_slice(&(used as u16).to_le_bytes());
                        (off + used, rec_len - used)
                    };
                    encode_dirent(&mut buf, new_off, ino, new_rec as u16, file_type, name);
                    return io.write_block(phys, &buf);
                }
                off += rec_len;
            }
        }
        // No slack anywhere — extend the directory by one block.
        if dir_blocks >= EXT2_NDIR_BLOCKS {
            return Err(Ext2Error::OutOfSpace);
        }
        let hint = inode_block_group(parent_ino, self.geo.inodes_per_group);
        let blk = self.alloc_block(io, hint)?;
        io.write_block(blk, &build_dir_block(bs, &[(name, ino, file_type)]))?;
        parent.block[dir_blocks] = blk;
        parent.size += bs as u32;
        parent.blocks += self.geo.block_size / 512;
        self.write_inode(io, parent_ino, &parent)
    }

    /// Create a regular file `name` under `parent_ino` with `data` and
    /// permission bits `perm`. Returns the new inode number.
    pub fn create_file<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        parent_ino: u32,
        name: &str,
        data: &[u8],
        perm: u16,
    ) -> Result<u32, Ext2Error> {
        let hint = inode_block_group(parent_ino, self.geo.inodes_per_group);
        let ino = self.alloc_inode(io, hint, false)?;
        let (block_ptrs, allocated) = self.write_file_blocks(io, hint, data)?;
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFREG | (perm & 0o7777);
        inode.size = data.len() as u32;
        inode.links_count = 1;
        inode.blocks = allocated * (self.geo.block_size / 512);
        inode.block = block_ptrs;
        self.write_inode(io, ino, &inode)?;
        self.add_dir_entry(io, parent_ino, name, ino, EXT2_FT_REG_FILE)?;
        Ok(ino)
    }

    /// Create a directory `name` under `parent_ino`. Returns the new inode
    /// number.
    pub fn create_dir<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        parent_ino: u32,
        name: &str,
        perm: u16,
    ) -> Result<u32, Ext2Error> {
        let hint = inode_block_group(parent_ino, self.geo.inodes_per_group);
        let ino = self.alloc_inode(io, hint, true)?;
        let blk = self.alloc_block(io, hint)?;
        io.write_block(
            blk,
            &build_dir_block(
                self.geo.block_size as usize,
                &[(".", ino, EXT2_FT_DIR), ("..", parent_ino, EXT2_FT_DIR)],
            ),
        )?;
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFDIR | (perm & 0o7777);
        inode.size = self.geo.block_size;
        inode.links_count = 2;
        inode.blocks = self.geo.block_size / 512;
        inode.block[0] = blk;
        self.write_inode(io, ino, &inode)?;
        self.add_dir_entry(io, parent_ino, name, ino, EXT2_FT_DIR)?;
        // ".." adds a link to the parent.
        let mut parent = self.read_inode(io, parent_ino)?;
        parent.links_count += 1;
        self.write_inode(io, parent_ino, &parent)?;
        Ok(ino)
    }

    /// Create a symlink `name → target` under `parent_ino` (inline target for
    /// ≤ 60 bytes, one data block otherwise). Returns the new inode number.
    pub fn create_symlink<IO: BlockIo + ?Sized>(
        &mut self,
        io: &mut IO,
        parent_ino: u32,
        name: &str,
        target: &str,
    ) -> Result<u32, Ext2Error> {
        let hint = inode_block_group(parent_ino, self.geo.inodes_per_group);
        let ino = self.alloc_inode(io, hint, false)?;
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFLNK | 0o777;
        inode.size = target.len() as u32;
        inode.links_count = 1;
        let tb = target.as_bytes();
        if tb.len() <= super::ext2::SYMLINK_INLINE_MAX {
            // Inline: the target bytes live in the block-pointer array.
            let mut raw = [0u8; 60];
            raw[..tb.len()].copy_from_slice(tb);
            for (i, ptr) in inode.block.iter_mut().enumerate() {
                let off = i * 4;
                *ptr = u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
            }
        } else {
            let (block_ptrs, allocated) = self.write_file_blocks(io, hint, tb)?;
            inode.block = block_ptrs;
            inode.blocks = allocated * (self.geo.block_size / 512);
        }
        self.write_inode(io, ino, &inode)?;
        self.add_dir_entry(io, parent_ino, name, ino, EXT2_FT_SYMLINK)?;
        Ok(ino)
    }

    /// Flush the in-memory superblock free counts + BGD table back to their
    /// primary on-disk copies.
    pub fn flush<IO: BlockIo + ?Sized>(&mut self, io: &mut IO) -> Result<(), Ext2Error> {
        let bs = self.geo.block_size as usize;
        // Superblock: read-modify-write only the mutable fields.
        let (sb_block, sb_off) = if bs == 1024 { (1, 0) } else { (0, 1024) };
        let mut buf = vec![0u8; bs];
        io.read_block(sb_block, &mut buf)?;
        self.sb.write_into(&mut buf[sb_off..]);
        io.write_block(sb_block, &buf)?;
        // BGD table.
        let mut table = vec![0u8; self.geo.bgd_blocks as usize * bs];
        for (g, bgd) in self.bgds.iter().enumerate() {
            bgd.write_into(&mut table[g * 32..]);
        }
        for b in 0..self.geo.bgd_blocks {
            let off = b as usize * bs;
            io.write_block(self.geo.group_start(0) + 1 + b, &table[off..off + bs])?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::ext2::{
        BlockReader, lookup_in_directory, read_directory_entries, read_file_data, read_inode,
        read_symlink_target, resolve_path,
    };
    use super::*;

    /// In-memory volume backing both the write seam ([`BlockIo`]) and the
    /// existing read path ([`BlockReader`]), so round-trip tests exercise the
    /// REAL reader against freshly formatted bytes.
    struct MemVolume {
        data: Vec<u8>,
        block_size: u32,
    }

    impl MemVolume {
        fn new(total_blocks: u32, block_size: u32) -> Self {
            MemVolume {
                data: vec![0u8; (total_blocks * block_size) as usize],
                block_size,
            }
        }
        fn range(&self, block: u32) -> core::ops::Range<usize> {
            let bs = self.block_size as usize;
            let start = block as usize * bs;
            start..start + bs
        }
    }

    impl BlockIo for MemVolume {
        fn read_block(&mut self, block: u32, buf: &mut [u8]) -> Result<(), Ext2Error> {
            let r = self.range(block);
            if r.end > self.data.len() {
                return Err(Ext2Error::TruncatedInput);
            }
            buf.copy_from_slice(&self.data[r]);
            Ok(())
        }
        fn write_block(&mut self, block: u32, data: &[u8]) -> Result<(), Ext2Error> {
            let r = self.range(block);
            if r.end > self.data.len() {
                return Err(Ext2Error::TruncatedInput);
            }
            self.data[r].copy_from_slice(data);
            Ok(())
        }
    }

    /// Fresh-mount view: parses the superblock + BGDs from the volume bytes
    /// (no state shared with the writer) and serves the existing read path.
    struct MountView<'a> {
        vol: &'a MemVolume,
        sb: Ext2Superblock,
        bgds: Vec<Ext2BlockGroupDescriptor>,
    }

    impl<'a> MountView<'a> {
        fn mount(vol: &'a MemVolume) -> Self {
            let sb = Ext2Superblock::parse(&vol.data[1024..2048]).expect("superblock parses");
            let bs = sb.block_size() as usize;
            assert_eq!(bs as u32, vol.block_size, "geometry agrees");
            let bgd_start = if bs == 1024 { 2 * bs } else { bs };
            let count = sb.block_group_count();
            let table = &vol.data[bgd_start..bgd_start + count as usize * 32];
            let bgds = Ext2BlockGroupDescriptor::parse_table(table, count).expect("BGDs parse");
            MountView { vol, sb, bgds }
        }
    }

    impl BlockReader for MountView<'_> {
        fn block_size(&self) -> u32 {
            self.sb.block_size()
        }
        fn inodes_per_group(&self) -> u32 {
            self.sb.inodes_per_group
        }
        fn inode_size(&self) -> u32 {
            self.sb.inode_size as u32
        }
        fn inode_table_block(&self, group: u32) -> Result<u32, Ext2Error> {
            self.bgds
                .get(group as usize)
                .map(|b| b.inode_table)
                .ok_or(Ext2Error::CorruptedEntry)
        }
        fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error> {
            let bs = self.sb.block_size() as usize;
            let start = block_num as usize * bs;
            if start + bs > self.vol.data.len() {
                return Err(Ext2Error::TruncatedInput);
            }
            Ok(self.vol.data[start..start + bs].to_vec())
        }
    }

    #[test]
    fn bitmap_find_clear_byte_scan_matches_bit_scan() {
        // All clear → bit 0.
        assert_eq!(bitmap_find_clear(&[0x00, 0x00], 16), Some(0));
        // First byte partially full (bits 0..3 set) → first clear is bit 3.
        assert_eq!(bitmap_find_clear(&[0b0000_0111, 0x00], 16), Some(3));
        // First byte fully allocated → skipped; next byte's bit 0 = global 8.
        assert_eq!(bitmap_find_clear(&[0xFF, 0x00], 16), Some(8));
        // All full bytes allocated, clear bit only in the partial tail byte:
        // limit 12 → full_bytes = 1 (byte 0), tail bits [8,12). Byte 1 =
        // 0b0000_0010 sets bit 9; bit 8 is clear → global 8.
        assert_eq!(bitmap_find_clear(&[0xFF, 0b0000_0010], 12), Some(8));
        // Every bit below limit set → None (tail byte's high bits are ≥ limit
        // and must not be returned).
        assert_eq!(bitmap_find_clear(&[0xFF, 0b0000_1111], 12), None);
        // Full byte + fully-set tail region → None.
        assert_eq!(bitmap_find_clear(&[0xFF, 0xFF], 16), None);
        // Cross-check the byte-scan against a naive bit-scan over a varied map.
        let map = [0xFF, 0xFF, 0b1010_1011, 0x00];
        let naive = (0..32u32).find(|&b| map[(b / 8) as usize] & (1 << (b % 8)) == 0);
        assert_eq!(bitmap_find_clear(&map, 32), naive);
    }

    fn params(total_blocks: u32, block_size_log: u32) -> FormatParams {
        FormatParams {
            total_blocks,
            block_size_log,
            uuid: *b"m3os-test-uuid-0",
        }
    }

    /// Read a whole file's contents through the existing read path.
    fn read_all<R: BlockReader>(r: &R, ino: u32) -> Vec<u8> {
        let inode = read_inode(r, ino).expect("inode reads");
        let mut out = vec![0u8; inode.size as usize];
        let n = read_file_data(r, &inode, 0, &mut out).expect("data reads");
        assert_eq!(n, out.len());
        out
    }

    // -- geometry + structure -------------------------------------------------

    #[test]
    fn format_superblock_parses_with_expected_geometry() {
        let mut vol = MemVolume::new(4096, 1024);
        let geo = format_ext2(&mut vol, &params(4096, 0)).expect("format");
        let sb = Ext2Superblock::parse(&vol.data[1024..2048]).expect("parse");
        assert_eq!(sb.magic, EXT2_MAGIC);
        assert_eq!(sb.rev_level, 1);
        assert_eq!(sb.first_ino, EXT2_FIRST_INO);
        assert_eq!(sb.inode_size, 128);
        assert_eq!(sb.blocks_count, 4096);
        assert_eq!(sb.first_data_block, 1);
        assert_eq!(sb.blocks_per_group, 8192);
        assert_eq!(sb.block_group_count(), 1);
        assert_eq!(geo.group_count, 1);
        // FILETYPE-only incompat feature set.
        let incompat = u32::from_le_bytes([
            vol.data[1024 + 96],
            vol.data[1024 + 97],
            vol.data[1024 + 98],
            vol.data[1024 + 99],
        ]);
        assert_eq!(incompat, FEATURE_INCOMPAT_FILETYPE);
        // UUID landed.
        assert_eq!(&vol.data[1024 + 104..1024 + 120], b"m3os-test-uuid-0");
    }

    #[test]
    fn format_multi_group_writes_parseable_backups() {
        // 20000 blocks at 1 KiB → 3 groups of 8192 (last short).
        let mut vol = MemVolume::new(20_000, 1024);
        let geo = format_ext2(&mut vol, &params(20_000, 0)).expect("format");
        assert_eq!(geo.group_count, 3);
        let primary = Ext2Superblock::parse(&vol.data[1024..2048]).expect("primary");
        for g in 1..3u32 {
            let start = geo.group_start(g) as usize * 1024;
            let backup =
                Ext2Superblock::parse(&vol.data[start..start + 1024]).expect("backup parses");
            assert_eq!(backup.blocks_count, primary.blocks_count);
            assert_eq!(backup.inodes_per_group, primary.inodes_per_group);
            // s_block_group_nr identifies the copy.
            let nr = u16::from_le_bytes([vol.data[start + 90], vol.data[start + 91]]);
            assert_eq!(nr, g as u16);
        }
    }

    #[test]
    fn format_free_counts_are_consistent() {
        let mut vol = MemVolume::new(20_000, 1024);
        let geo = format_ext2(&mut vol, &params(20_000, 0)).expect("format");
        let view = MountView::mount(&vol);
        let sum_blocks: u32 = view.bgds.iter().map(|b| b.free_blocks_count as u32).sum();
        let sum_inodes: u32 = view.bgds.iter().map(|b| b.free_inodes_count as u32).sum();
        assert_eq!(view.sb.free_blocks_count, sum_blocks);
        assert_eq!(view.sb.free_inodes_count, sum_inodes);
        assert_eq!(view.sb.free_inodes_count, geo.inodes_count - EXT2_FIRST_INO);
        // Every group's used-dirs: root + lost+found in group 0 only.
        assert_eq!(view.bgds[0].used_dirs_count, 2);
        assert!(view.bgds[1..].iter().all(|b| b.used_dirs_count == 0));
    }

    #[test]
    fn format_root_and_lost_found_resolve_through_reader() {
        let mut vol = MemVolume::new(4096, 1024);
        format_ext2(&mut vol, &params(4096, 0)).expect("format");
        let view = MountView::mount(&vol);
        assert_eq!(resolve_path(&view, "/").expect("root"), EXT2_ROOT_INO);
        assert_eq!(
            resolve_path(&view, "/lost+found").expect("lost+found"),
            LOST_FOUND_INO
        );
        let root = read_inode(&view, EXT2_ROOT_INO).expect("root inode");
        assert!(root.is_dir());
        assert_eq!(root.links_count, 3);
        let entries = read_directory_entries(&view, &root).expect("entries");
        let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(names, [".", "..", "lost+found"]);
        let lf = read_inode(&view, LOST_FOUND_INO).expect("lf inode");
        assert!(lf.is_dir());
        assert_eq!(lf.links_count, 2);
    }

    #[test]
    fn format_rejects_degenerate_volumes() {
        // Too small to hold group 0's metadata.
        let mut vol = MemVolume::new(16, 1024);
        assert!(format_ext2(&mut vol, &params(16, 0)).is_err());
        // Bad block size log.
        let mut vol = MemVolume::new(4096, 1024);
        assert!(
            format_ext2(
                &mut vol,
                &FormatParams {
                    total_blocks: 4096,
                    block_size_log: 3,
                    uuid: [0; 16],
                }
            )
            .is_err()
        );
    }

    // -- the acceptance round trip -------------------------------------------

    #[test]
    fn round_trip_small_file_through_fresh_mount() {
        let mut vol = MemVolume::new(4096, 1024);
        format_ext2(&mut vol, &params(4096, 0)).expect("format");
        let data = b"phase 106 C.5: format -> write -> fresh mount -> read".to_vec();

        let mut fs = Ext2Fs::open(&mut vol, 0).expect("open");
        let ino = fs
            .create_file(&mut vol, EXT2_ROOT_INO, "hello.txt", &data, 0o644)
            .expect("create");
        fs.flush(&mut vol).expect("flush");

        // Fresh mount: everything parsed back from the volume bytes.
        let view = MountView::mount(&vol);
        assert_eq!(
            resolve_path(&view, "/hello.txt").expect("path resolves"),
            ino
        );
        assert_eq!(read_all(&view, ino), data);
    }

    #[test]
    fn round_trip_indirect_and_double_indirect_file() {
        // 1 KiB blocks: direct covers 12 KiB, single-indirect the next
        // 256 KiB — 400 KiB of patterned data forces double-indirect.
        let total_blocks = 2048 + 512 * 2; // plenty of data room
        let mut vol = MemVolume::new(8192, 1024);
        let _ = total_blocks;
        format_ext2(&mut vol, &params(8192, 0)).expect("format");
        let data: Vec<u8> = (0..400 * 1024u32).map(|i| (i * 31 % 251) as u8).collect();

        let mut fs = Ext2Fs::open(&mut vol, 0).expect("open");
        let free_before = fs.sb.free_blocks_count;
        let ino = fs
            .create_file(&mut vol, EXT2_ROOT_INO, "big.bin", &data, 0o644)
            .expect("create");
        fs.flush(&mut vol).expect("flush");

        let view = MountView::mount(&vol);
        assert_eq!(resolve_path(&view, "/big.bin").expect("resolve"), ino);
        assert_eq!(read_all(&view, ino), data);

        // Free-count bookkeeping. 400 data blocks: 12 direct, 256 through the
        // single-indirect block, 132 through the double-indirect (dind block +
        // one second-level indirect block, since 132 <= 256 pointers/block).
        // So 400 data + 1 single-indirect + 1 dind + 1 second-level = 403.
        let inode = read_inode(&view, ino).expect("inode");
        assert_ne!(inode.block[EXT2_IND_BLOCK], 0);
        assert_ne!(inode.block[EXT2_DIND_BLOCK], 0);
        let used = free_before - view.sb.free_blocks_count;
        assert_eq!(used, 400 + 1 + 1 + 1);
        assert_eq!(inode.blocks, used * 2); // 512-byte units at 1 KiB blocks
    }

    #[test]
    fn round_trip_directory_tree_and_symlink() {
        let mut vol = MemVolume::new(4096, 1024);
        format_ext2(&mut vol, &params(4096, 0)).expect("format");
        let mut fs = Ext2Fs::open(&mut vol, 0).expect("open");
        let etc = fs
            .create_dir(&mut vol, EXT2_ROOT_INO, "etc", 0o755)
            .expect("mkdir etc");
        fs.create_file(
            &mut vol,
            etc,
            "passwd",
            b"root:x:0:0::/root:/bin/sh\n",
            0o644,
        )
        .expect("create passwd");
        fs.create_symlink(&mut vol, EXT2_ROOT_INO, "cfg", "/etc/passwd")
            .expect("symlink");
        fs.flush(&mut vol).expect("flush");

        let view = MountView::mount(&vol);
        let passwd = resolve_path(&view, "/etc/passwd").expect("resolve");
        assert_eq!(
            read_all(&view, passwd),
            b"root:x:0:0::/root:/bin/sh\n".to_vec()
        );
        // ".." from /etc points back at root; root links grew to 4.
        let root = read_inode(&view, EXT2_ROOT_INO).expect("root");
        assert_eq!(root.links_count, 4);
        let etc_inode = read_inode(&view, etc).expect("etc");
        assert_eq!(
            lookup_in_directory(&view, &etc_inode, "..").expect("dotdot"),
            EXT2_ROOT_INO
        );
        // Symlink target reads back through the existing reader.
        let link_ino = resolve_path(&view, "/cfg").expect("cfg");
        let link = read_inode(&view, link_ino).expect("link inode");
        assert_eq!(
            read_symlink_target(&view, &link).expect("target"),
            b"/etc/passwd".to_vec()
        );
    }

    #[test]
    fn round_trip_4k_blocks() {
        let mut vol = MemVolume::new(4096, 4096);
        let geo = format_ext2(&mut vol, &params(4096, 2)).expect("format");
        assert_eq!(geo.first_data_block, 0);
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
        let mut fs = Ext2Fs::open(&mut vol, 2).expect("open");
        let ino = fs
            .create_file(&mut vol, EXT2_ROOT_INO, "f", &data, 0o600)
            .expect("create");
        fs.flush(&mut vol).expect("flush");
        let view = MountView::mount(&vol);
        assert_eq!(resolve_path(&view, "/f").expect("resolve"), ino);
        assert_eq!(read_all(&view, ino), data);
    }

    #[test]
    fn many_files_fill_dir_block_and_spill_to_second() {
        let mut vol = MemVolume::new(8192, 1024);
        format_ext2(&mut vol, &params(8192, 0)).expect("format");
        let mut fs = Ext2Fs::open(&mut vol, 0).expect("open");
        // 60 × ~16-byte entries ≈ 1.5 dir blocks — forces the extend path.
        let mut inos = Vec::new();
        for i in 0..60 {
            let name = alloc::format!("file-{i:03}");
            let body = alloc::format!("contents of {i}");
            inos.push((
                name.clone(),
                fs.create_file(&mut vol, EXT2_ROOT_INO, &name, body.as_bytes(), 0o644)
                    .expect("create"),
                body,
            ));
        }
        fs.flush(&mut vol).expect("flush");
        let view = MountView::mount(&vol);
        for (name, ino, body) in inos {
            let path = alloc::format!("/{name}");
            assert_eq!(resolve_path(&view, &path).expect("resolve"), ino);
            assert_eq!(read_all(&view, ino), body.into_bytes());
        }
    }

    /// Falsifiable external check: if the host has `e2fsck`, the freshly
    /// formatted + populated image must pass `e2fsck -fn` clean (exit 0).
    /// Skips silently when the tool is absent (CI parity with the pure-Rust
    /// assertions above).
    #[test]
    fn e2fsck_accepts_formatted_image_when_available() {
        use std::io::Write as _;
        use std::process::Command;

        let which = Command::new("sh")
            .args(["-c", "command -v e2fsck"])
            .output();
        let Ok(out) = which else { return };
        if !out.status.success() {
            return; // no e2fsck on this host — skip
        }

        let mut vol = MemVolume::new(8192, 1024);
        format_ext2(&mut vol, &params(8192, 0)).expect("format");
        let mut fs = Ext2Fs::open(&mut vol, 0).expect("open");
        let etc = fs
            .create_dir(&mut vol, EXT2_ROOT_INO, "etc", 0o755)
            .expect("mkdir");
        fs.create_file(&mut vol, etc, "hostname", b"m3os\n", 0o644)
            .expect("create");
        let big: Vec<u8> = (0..300 * 1024u32).map(|i| (i % 251) as u8).collect();
        fs.create_file(&mut vol, EXT2_ROOT_INO, "big.bin", &big, 0o644)
            .expect("create big");
        fs.flush(&mut vol).expect("flush");

        let dir = std::env::temp_dir();
        let path = dir.join(format!("m3os-c5-fsck-{}.img", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("temp image");
        f.write_all(&vol.data).expect("write image");
        drop(f);

        let fsck = Command::new("e2fsck")
            .args(["-fn", path.to_str().expect("utf8 path")])
            .output()
            .expect("run e2fsck");
        let _ = std::fs::remove_file(&path);
        assert!(
            fsck.status.success(),
            "e2fsck rejected the image:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fsck.stdout),
            String::from_utf8_lossy(&fsck.stderr),
        );
    }
}
