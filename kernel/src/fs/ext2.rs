//! ext2 filesystem driver (Phase 28, Tracks B–G).
//!
//! Provides a complete read/write ext2 volume driver backed by virtio-blk I/O.
//! Implements inode reading, block pointer traversal, directory operations,
//! file read/write, bitmap management, and native Unix metadata (VfsMetadata).

#![allow(dead_code)]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use kernel_core::fs::ext2::{
    EXT2_DIND_BLOCK, EXT2_FT_DIR, EXT2_FT_REG_FILE, EXT2_FT_SYMLINK, EXT2_IND_BLOCK,
    EXT2_NDIR_BLOCKS, EXT2_ROOT_INO, Ext2BlockGroupDescriptor, Ext2DirEntry, Ext2Error, Ext2Inode,
    Ext2Superblock, S_IFDIR, S_IFLNK, S_IFREG,
};
use spin::Mutex;

// ---------------------------------------------------------------------------
// Ext2Volume (P28-T019)
// ---------------------------------------------------------------------------

/// A mounted ext2 volume backed by virtio-blk sectors.
pub struct Ext2Volume {
    /// Absolute LBA of the partition start on the block device.
    base_lba: u64,
    /// Parsed and cached superblock.
    pub superblock: Ext2Superblock,
    /// Cached block group descriptor table.
    pub bgd_table: Vec<Ext2BlockGroupDescriptor>,
    /// Block size in bytes (1024 << log_block_size).
    pub block_size: u32,
    /// Sectors per ext2 block (block_size / 512).
    sectors_per_block: u32,
    /// Raw superblock bytes (for writeback).
    superblock_raw: Vec<u8>,
}

/// Global mounted ext2 volume (set by mount_ext2).
pub static EXT2_VOLUME: Mutex<Option<Ext2Volume>> = Mutex::new(None);

impl Ext2Volume {
    /// Mount an ext2 partition at the given base LBA (P28-T019).
    pub fn mount(base_lba: u64) -> Result<Self, Ext2Error> {
        // Superblock is at byte offset 1024 from partition start = LBA + 2 sectors.
        let sb_lba = base_lba + 2; // 1024 bytes / 512 bytes per sector
        let mut sb_raw = vec![0u8; 1024];
        crate::blk::read_sectors(sb_lba, 2, &mut sb_raw).map_err(|_| Ext2Error::IoError)?;

        let superblock = Ext2Superblock::parse(&sb_raw)?;
        let block_size = superblock.block_size();
        let sectors_per_block = block_size / 512;
        let bg_count = superblock.block_group_count();

        // Block group descriptor table starts at the block after the superblock.
        // For 4K blocks, superblock is within block 0 (at offset 1024), so BGD
        // table is at block 1 (byte offset 4096). For 1K blocks, superblock is
        // block 1, BGD table is block 2.
        let bgd_block = if block_size == 1024 { 2 } else { 1 };
        let bgd_lba = base_lba + (bgd_block as u64) * (sectors_per_block as u64);
        let bgd_size = (bg_count as usize) * 32;
        let bgd_sectors = bgd_size.div_ceil(512);
        let mut bgd_raw = vec![0u8; bgd_sectors * 512];
        crate::blk::read_sectors(bgd_lba, bgd_sectors, &mut bgd_raw)
            .map_err(|_| Ext2Error::IoError)?;

        let bgd_table = Ext2BlockGroupDescriptor::parse_table(&bgd_raw, bg_count)?;

        log::info!(
            "[ext2] mounted: base_lba={}, block_size={}, blocks={}, inodes={}, groups={}",
            base_lba,
            block_size,
            superblock.blocks_count,
            superblock.inodes_count,
            bg_count
        );

        Ok(Ext2Volume {
            base_lba,
            superblock,
            bgd_table,
            block_size,
            sectors_per_block,
            superblock_raw: sb_raw,
        })
    }

    // -----------------------------------------------------------------------
    // Low-level block I/O
    // -----------------------------------------------------------------------

    /// Convert an ext2 block number to an absolute disk LBA.
    fn block_to_lba(&self, block_num: u32) -> u64 {
        self.base_lba + (block_num as u64) * (self.sectors_per_block as u64)
    }

    /// Read an ext2 block from disk.
    fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error> {
        let lba = self.block_to_lba(block_num);
        let mut buf = vec![0u8; self.block_size as usize];
        crate::blk::read_sectors(lba, self.sectors_per_block as usize, &mut buf)
            .map_err(|_| Ext2Error::IoError)?;
        Ok(buf)
    }

    /// Write an ext2 block to disk.
    fn write_block(&self, block_num: u32, data: &[u8]) -> Result<(), Ext2Error> {
        let lba = self.block_to_lba(block_num);
        crate::blk::write_sectors(lba, self.sectors_per_block as usize, data)
            .map_err(|_| Ext2Error::IoError)
    }

    // -----------------------------------------------------------------------
    // Inode operations (P28-T010)
    // -----------------------------------------------------------------------

    /// Read an inode by number (1-based).
    pub fn read_inode(&self, inode_num: u32) -> Result<Ext2Inode, Ext2Error> {
        let group =
            kernel_core::fs::ext2::inode_block_group(inode_num, self.superblock.inodes_per_group);
        let index = kernel_core::fs::ext2::inode_index_in_group(
            inode_num,
            self.superblock.inodes_per_group,
        );

        let bgd = self
            .bgd_table
            .get(group as usize)
            .ok_or(Ext2Error::CorruptedEntry)?;

        let inode_size = self.superblock.inode_size as u32;
        let byte_offset = (index as u64) * (inode_size as u64);
        let block_offset = byte_offset / (self.block_size as u64);
        let offset_in_block = (byte_offset % (self.block_size as u64)) as usize;

        let block_num = bgd.inode_table + block_offset as u32;
        let block_data = self.read_block(block_num)?;

        Ext2Inode::parse(&block_data[offset_in_block..])
    }

    /// Write an inode back to disk (P28-T033).
    pub fn write_inode(&self, inode_num: u32, inode: &Ext2Inode) -> Result<(), Ext2Error> {
        let group =
            kernel_core::fs::ext2::inode_block_group(inode_num, self.superblock.inodes_per_group);
        let index = kernel_core::fs::ext2::inode_index_in_group(
            inode_num,
            self.superblock.inodes_per_group,
        );

        let bgd = self
            .bgd_table
            .get(group as usize)
            .ok_or(Ext2Error::CorruptedEntry)?;

        let inode_size = self.superblock.inode_size as u32;
        let byte_offset = (index as u64) * (inode_size as u64);
        let block_offset = byte_offset / (self.block_size as u64);
        let offset_in_block = (byte_offset % (self.block_size as u64)) as usize;

        let block_num = bgd.inode_table + block_offset as u32;
        let mut block_data = self.read_block(block_num)?;

        inode.write_into(&mut block_data[offset_in_block..]);
        self.write_block(block_num, &block_data)
    }

    // -----------------------------------------------------------------------
    // Block pointer resolution (P28-T011 through P28-T013)
    // -----------------------------------------------------------------------

    /// Resolve a logical block index to a physical block number.
    /// Returns 0 for sparse/hole blocks.
    fn resolve_block(&self, inode: &Ext2Inode, logical_block: u32) -> Result<u32, Ext2Error> {
        let ptrs_per_block = self.block_size / 4; // u32 entries per block

        // Direct blocks (0–11)
        if logical_block < EXT2_NDIR_BLOCKS as u32 {
            return Ok(inode.block[logical_block as usize]);
        }

        let adjusted = logical_block - EXT2_NDIR_BLOCKS as u32;

        // Single-indirect (P28-T012)
        if adjusted < ptrs_per_block {
            let ind_block = inode.block[EXT2_IND_BLOCK];
            if ind_block == 0 {
                return Ok(0);
            }
            let data = self.read_block(ind_block)?;
            let off = (adjusted as usize) * 4;
            return Ok(u32::from_le_bytes([
                data[off],
                data[off + 1],
                data[off + 2],
                data[off + 3],
            ]));
        }

        let adjusted = adjusted - ptrs_per_block;

        // Double-indirect (P28-T013)
        if adjusted < ptrs_per_block * ptrs_per_block {
            let dind_block = inode.block[EXT2_DIND_BLOCK];
            if dind_block == 0 {
                return Ok(0);
            }
            let dind_data = self.read_block(dind_block)?;
            let ind_index = adjusted / ptrs_per_block;
            let off = (ind_index as usize) * 4;
            let ind_block = u32::from_le_bytes([
                dind_data[off],
                dind_data[off + 1],
                dind_data[off + 2],
                dind_data[off + 3],
            ]);
            if ind_block == 0 {
                return Ok(0);
            }
            let ind_data = self.read_block(ind_block)?;
            let block_index = adjusted % ptrs_per_block;
            let off = (block_index as usize) * 4;
            return Ok(u32::from_le_bytes([
                ind_data[off],
                ind_data[off + 1],
                ind_data[off + 2],
                ind_data[off + 3],
            ]));
        }

        // Triple-indirect — deferred; files this large shouldn't exist on our 64MB filesystem.
        Err(Ext2Error::CorruptedEntry)
    }

    // -----------------------------------------------------------------------
    // File data reading (P28-T014)
    // -----------------------------------------------------------------------

    /// Read file data from an inode starting at `offset` into `buf`.
    /// Returns the number of bytes actually read.
    pub fn read_file_data(
        &self,
        inode: &Ext2Inode,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Ext2Error> {
        let file_size = inode.size as u64;
        if offset >= file_size {
            return Ok(0);
        }

        let available = (file_size - offset) as usize;
        let to_read = buf.len().min(available);
        let bs = self.block_size as u64;

        let mut bytes_read = 0;
        let mut pos = offset;

        while bytes_read < to_read {
            let logical_block = (pos / bs) as u32;
            let offset_in_block = (pos % bs) as usize;
            let remaining_in_block = (bs as usize) - offset_in_block;
            let copy_len = remaining_in_block.min(to_read - bytes_read);

            let phys_block = self.resolve_block(inode, logical_block)?;
            if phys_block == 0 {
                // Sparse hole: fill with zeros.
                buf[bytes_read..bytes_read + copy_len].fill(0);
            } else {
                let block_data = self.read_block(phys_block)?;
                buf[bytes_read..bytes_read + copy_len]
                    .copy_from_slice(&block_data[offset_in_block..offset_in_block + copy_len]);
            }

            bytes_read += copy_len;
            pos += copy_len as u64;
        }

        Ok(bytes_read)
    }

    // -----------------------------------------------------------------------
    // Directory operations (P28-T015 through P28-T018)
    // -----------------------------------------------------------------------

    /// Read all directory entries from a directory inode (P28-T015).
    pub fn read_directory_entries(
        &self,
        inode: &Ext2Inode,
    ) -> Result<Vec<(String, u32, u8)>, Ext2Error> {
        if !inode.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }

        let dir_size = inode.size as u64;
        let bs = self.block_size as u64;
        let num_blocks = dir_size.div_ceil(bs) as u32;
        let mut result = Vec::new();

        for logical_block in 0..num_blocks {
            let phys_block = self.resolve_block(inode, logical_block)?;
            if phys_block == 0 {
                continue;
            }
            let block_data = self.read_block(phys_block)?;
            let entries = Ext2DirEntry::parse_block(&block_data)?;
            for entry in entries {
                if entry.inode != 0 {
                    result.push((entry.name, entry.inode, entry.file_type));
                }
            }
        }

        Ok(result)
    }

    /// Look up a name in a directory inode (P28-T016).
    pub fn lookup_in_directory(&self, dir_inode: &Ext2Inode, name: &str) -> Result<u32, Ext2Error> {
        let entries = self.read_directory_entries(dir_inode)?;
        for (entry_name, inode_num, _) in entries {
            if entry_name == name {
                return Ok(inode_num);
            }
        }
        Err(Ext2Error::NotFound)
    }

    /// Resolve an absolute path to an inode number (P28-T017).
    pub fn resolve_path(&self, path: &str) -> Result<u32, Ext2Error> {
        let mut current_ino = EXT2_ROOT_INO;

        for component in path.split('/').filter(|s| !s.is_empty()) {
            if component == "." {
                continue;
            }
            let inode = self.read_inode(current_ino)?;
            if !inode.is_dir() {
                return Err(Ext2Error::NotDirectory);
            }
            current_ino = self.lookup_in_directory(&inode, component)?;
        }

        Ok(current_ino)
    }

    // -----------------------------------------------------------------------
    // Bitmap management (P28-T026 through P28-T032)
    // -----------------------------------------------------------------------

    /// Allocate a free block, preferring the given block group (P28-T027).
    pub fn allocate_block(&mut self, preferred_group: u32) -> Result<u32, Ext2Error> {
        let bg_count = self.bgd_table.len();

        for offset in 0..bg_count {
            let group = ((preferred_group as usize) + offset) % bg_count;
            let bgd = &self.bgd_table[group];
            if bgd.free_blocks_count == 0 {
                continue;
            }

            let bitmap_block = bgd.block_bitmap;
            let mut bitmap = self.read_block(bitmap_block)?;

            let blocks_in_group = if group == bg_count - 1 {
                self.superblock.blocks_count
                    - self.superblock.first_data_block
                    - (group as u32) * self.superblock.blocks_per_group
            } else {
                self.superblock.blocks_per_group
            };

            for bit in 0..blocks_in_group {
                let byte_idx = (bit / 8) as usize;
                let bit_idx = bit % 8;
                if bitmap[byte_idx] & (1 << bit_idx) == 0 {
                    // Found a free block — mark it as used.
                    bitmap[byte_idx] |= 1 << bit_idx;
                    self.write_block(bitmap_block, &bitmap)?;

                    // Update counts.
                    self.bgd_table[group].free_blocks_count -= 1;
                    self.superblock.free_blocks_count -= 1;

                    let abs_block = (group as u32) * self.superblock.blocks_per_group
                        + bit
                        + self.superblock.first_data_block;

                    self.flush_metadata()?;
                    return Ok(abs_block);
                }
            }
        }

        Err(Ext2Error::OutOfSpace)
    }

    /// Free a block (P28-T028).
    pub fn free_block(&mut self, block_num: u32) -> Result<(), Ext2Error> {
        if block_num < self.superblock.first_data_block {
            return Err(Ext2Error::CorruptedEntry);
        }
        let relative = block_num - self.superblock.first_data_block;
        let group = (relative / self.superblock.blocks_per_group) as usize;
        if group >= self.bgd_table.len() {
            return Err(Ext2Error::CorruptedEntry);
        }
        let bit = relative % self.superblock.blocks_per_group;

        let bgd = &self.bgd_table[group];
        let bitmap_block = bgd.block_bitmap;
        let mut bitmap = self.read_block(bitmap_block)?;

        let byte_idx = (bit / 8) as usize;
        let bit_idx = bit % 8;
        // Detect double-free: the bit must be set (allocated) before we clear it.
        if bitmap[byte_idx] & (1 << bit_idx) == 0 {
            return Err(Ext2Error::CorruptedEntry);
        }
        bitmap[byte_idx] &= !(1 << bit_idx);
        self.write_block(bitmap_block, &bitmap)?;

        self.bgd_table[group].free_blocks_count += 1;
        self.superblock.free_blocks_count += 1;
        self.flush_metadata()
    }

    /// Allocate a free inode, preferring the given block group (P28-T030).
    pub fn allocate_inode(&mut self, preferred_group: u32) -> Result<u32, Ext2Error> {
        let bg_count = self.bgd_table.len();

        for offset in 0..bg_count {
            let group = ((preferred_group as usize) + offset) % bg_count;
            let bgd = &self.bgd_table[group];
            if bgd.free_inodes_count == 0 {
                continue;
            }

            let bitmap_block = bgd.inode_bitmap;
            let mut bitmap = self.read_block(bitmap_block)?;

            let inodes_in_group = self.superblock.inodes_per_group;

            for bit in 0..inodes_in_group {
                let abs_inode = (group as u32) * self.superblock.inodes_per_group + bit + 1;
                if abs_inode > self.superblock.inodes_count {
                    continue; // This bit is beyond the actual inode count
                }

                let byte_idx = (bit / 8) as usize;
                let bit_idx = bit % 8;
                if bitmap[byte_idx] & (1 << bit_idx) == 0 {
                    bitmap[byte_idx] |= 1 << bit_idx;
                    self.write_block(bitmap_block, &bitmap)?;

                    self.bgd_table[group].free_inodes_count -= 1;
                    self.superblock.free_inodes_count -= 1;

                    self.flush_metadata()?;
                    return Ok(abs_inode);
                }
            }
        }

        Err(Ext2Error::OutOfSpace)
    }

    /// Free an inode (P28-T031).
    pub fn free_inode(&mut self, inode_num: u32) -> Result<(), Ext2Error> {
        if inode_num == 0 || inode_num > self.superblock.inodes_count {
            return Err(Ext2Error::CorruptedEntry);
        }
        let group =
            kernel_core::fs::ext2::inode_block_group(inode_num, self.superblock.inodes_per_group)
                as usize;
        if group >= self.bgd_table.len() {
            return Err(Ext2Error::CorruptedEntry);
        }
        let index = kernel_core::fs::ext2::inode_index_in_group(
            inode_num,
            self.superblock.inodes_per_group,
        );

        let bgd = &self.bgd_table[group];
        let bitmap_block = bgd.inode_bitmap;
        let mut bitmap = self.read_block(bitmap_block)?;

        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        // Detect double-free: the bit must be set (allocated) before we clear it.
        if bitmap[byte_idx] & (1 << bit_idx) == 0 {
            return Err(Ext2Error::CorruptedEntry);
        }
        bitmap[byte_idx] &= !(1 << bit_idx);
        self.write_block(bitmap_block, &bitmap)?;

        self.bgd_table[group].free_inodes_count += 1;
        self.superblock.free_inodes_count += 1;
        self.flush_metadata()
    }

    /// Flush superblock and BGD table to disk (P28-T032).
    fn flush_metadata(&self) -> Result<(), Ext2Error> {
        // Write superblock.
        let mut sb_buf = self.superblock_raw.clone();
        self.superblock.write_into(&mut sb_buf);
        let sb_lba = self.base_lba + 2;
        crate::blk::write_sectors(sb_lba, 2, &sb_buf).map_err(|_| Ext2Error::IoError)?;

        // Write BGD table.
        let bgd_block = if self.block_size == 1024 { 2 } else { 1 };
        let bgd_lba = self.base_lba + (bgd_block as u64) * (self.sectors_per_block as u64);
        let bgd_bytes = self.bgd_table.len() * 32;
        let bgd_sectors = bgd_bytes.div_ceil(512);
        let mut bgd_buf = vec![0u8; bgd_sectors * 512];
        for (i, bgd) in self.bgd_table.iter().enumerate() {
            bgd.write_into(&mut bgd_buf[i * 32..(i + 1) * 32]);
        }
        crate::blk::write_sectors(bgd_lba, bgd_sectors, &bgd_buf).map_err(|_| Ext2Error::IoError)
    }

    // -----------------------------------------------------------------------
    // Block allocation for writes (P28-T034)
    // -----------------------------------------------------------------------

    /// Allocate a data block for a logical position in an inode.
    /// Updates the inode's block pointers as needed.
    fn allocate_data_block(
        &mut self,
        inode: &mut Ext2Inode,
        logical_block: u32,
    ) -> Result<u32, Ext2Error> {
        let ptrs_per_block = self.block_size / 4;
        let preferred_group = 0; // Simple: prefer group 0

        // Direct blocks
        if logical_block < EXT2_NDIR_BLOCKS as u32 {
            if inode.block[logical_block as usize] == 0 {
                let new_block = self.allocate_block(preferred_group)?;
                // Zero the new block.
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(new_block, &zero)?;
                inode.block[logical_block as usize] = new_block;
                inode.blocks += self.block_size / 512;
            }
            return Ok(inode.block[logical_block as usize]);
        }

        let adjusted = logical_block - EXT2_NDIR_BLOCKS as u32;

        // Single-indirect
        if adjusted < ptrs_per_block {
            if inode.block[EXT2_IND_BLOCK] == 0 {
                let ind = self.allocate_block(preferred_group)?;
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(ind, &zero)?;
                inode.block[EXT2_IND_BLOCK] = ind;
                inode.blocks += self.block_size / 512;
            }
            let ind_block = inode.block[EXT2_IND_BLOCK];
            let mut ind_data = self.read_block(ind_block)?;
            let off = (adjusted as usize) * 4;
            let existing = u32::from_le_bytes([
                ind_data[off],
                ind_data[off + 1],
                ind_data[off + 2],
                ind_data[off + 3],
            ]);
            if existing == 0 {
                let new_block = self.allocate_block(preferred_group)?;
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(new_block, &zero)?;
                ind_data[off..off + 4].copy_from_slice(&new_block.to_le_bytes());
                self.write_block(ind_block, &ind_data)?;
                inode.blocks += self.block_size / 512;
                return Ok(new_block);
            }
            return Ok(existing);
        }

        let adjusted = adjusted - ptrs_per_block;

        // Double-indirect
        if adjusted < ptrs_per_block * ptrs_per_block {
            if inode.block[EXT2_DIND_BLOCK] == 0 {
                let dind = self.allocate_block(preferred_group)?;
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(dind, &zero)?;
                inode.block[EXT2_DIND_BLOCK] = dind;
                inode.blocks += self.block_size / 512;
            }
            let dind_block = inode.block[EXT2_DIND_BLOCK];
            let mut dind_data = self.read_block(dind_block)?;

            let ind_index = adjusted / ptrs_per_block;
            let off = (ind_index as usize) * 4;
            let mut ind_block = u32::from_le_bytes([
                dind_data[off],
                dind_data[off + 1],
                dind_data[off + 2],
                dind_data[off + 3],
            ]);
            if ind_block == 0 {
                ind_block = self.allocate_block(preferred_group)?;
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(ind_block, &zero)?;
                dind_data[off..off + 4].copy_from_slice(&ind_block.to_le_bytes());
                self.write_block(dind_block, &dind_data)?;
                inode.blocks += self.block_size / 512;
            }

            let mut ind_data = self.read_block(ind_block)?;
            let block_index = adjusted % ptrs_per_block;
            let off = (block_index as usize) * 4;
            let existing = u32::from_le_bytes([
                ind_data[off],
                ind_data[off + 1],
                ind_data[off + 2],
                ind_data[off + 3],
            ]);
            if existing == 0 {
                let new_block = self.allocate_block(preferred_group)?;
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(new_block, &zero)?;
                ind_data[off..off + 4].copy_from_slice(&new_block.to_le_bytes());
                self.write_block(ind_block, &ind_data)?;
                inode.blocks += self.block_size / 512;
                return Ok(new_block);
            }
            return Ok(existing);
        }

        Err(Ext2Error::OutOfSpace) // Triple-indirect not supported
    }

    // -----------------------------------------------------------------------
    // File data writing (P28-T035)
    // -----------------------------------------------------------------------

    /// Write data to a file inode at the given offset.
    /// Allocates new blocks as needed. Updates inode size and block count.
    /// Returns the number of bytes written.
    pub fn write_file_data(
        &mut self,
        inode_num: u32,
        inode: &mut Ext2Inode,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, Ext2Error> {
        if data.is_empty() {
            return Ok(0);
        }

        let bs = self.block_size as u64;
        let end_offset = offset + data.len() as u64;
        let mut written = 0;
        let mut pos = offset;

        while written < data.len() {
            let logical_block = (pos / bs) as u32;
            let offset_in_block = (pos % bs) as usize;
            let remaining_in_block = (bs as usize) - offset_in_block;
            let copy_len = remaining_in_block.min(data.len() - written);

            let phys_block = self.allocate_data_block(inode, logical_block)?;

            // Read-modify-write for partial blocks.
            let mut block_data = if offset_in_block > 0 || copy_len < bs as usize {
                self.read_block(phys_block)?
            } else {
                vec![0u8; bs as usize]
            };

            block_data[offset_in_block..offset_in_block + copy_len]
                .copy_from_slice(&data[written..written + copy_len]);
            self.write_block(phys_block, &block_data)?;

            written += copy_len;
            pos += copy_len as u64;
        }

        // Update inode size if we wrote past the end.
        if end_offset > inode.size as u64 {
            inode.size = end_offset as u32;
        }

        self.write_inode(inode_num, inode)?;
        Ok(written)
    }

    // -----------------------------------------------------------------------
    // Directory write operations (P28-T036 through P28-T042)
    // -----------------------------------------------------------------------

    /// Add a directory entry to a directory inode (P28-T036).
    pub fn add_directory_entry(
        &mut self,
        dir_inode_num: u32,
        dir_inode: &mut Ext2Inode,
        name: &str,
        child_inode: u32,
        file_type: u8,
    ) -> Result<(), Ext2Error> {
        let name_bytes = name.as_bytes();
        // Required size: 8 (header) + name_len, rounded up to 4-byte alignment.
        let needed_size = (8 + name_bytes.len()).div_ceil(4) * 4;

        let dir_size = dir_inode.size as u64;
        let bs = self.block_size as u64;
        let num_blocks = dir_size.div_ceil(bs) as u32;

        // Try to find space in existing blocks by splitting the last entry's rec_len.
        for logical_block in 0..num_blocks {
            let phys_block = self.resolve_block(dir_inode, logical_block)?;
            if phys_block == 0 {
                continue;
            }
            let mut block_data = self.read_block(phys_block)?;
            let mut offset = 0;

            while offset + 8 <= block_data.len() {
                let rec_len =
                    u16::from_le_bytes([block_data[offset + 4], block_data[offset + 5]]) as usize;
                if rec_len == 0 {
                    break;
                }

                let entry_name_len = block_data[offset + 6] as usize;
                let actual_size = (8 + entry_name_len).div_ceil(4) * 4;
                if rec_len < actual_size {
                    offset += rec_len;
                    continue;
                }
                let slack = rec_len - actual_size;

                if slack >= needed_size {
                    // Shrink current entry's rec_len to its actual size.
                    block_data[offset + 4..offset + 6]
                        .copy_from_slice(&(actual_size as u16).to_le_bytes());

                    // Write new entry in the slack space.
                    let new_offset = offset + actual_size;
                    let new_rec_len = slack as u16;
                    block_data[new_offset..new_offset + 4]
                        .copy_from_slice(&child_inode.to_le_bytes());
                    block_data[new_offset + 4..new_offset + 6]
                        .copy_from_slice(&new_rec_len.to_le_bytes());
                    block_data[new_offset + 6] = name_bytes.len() as u8;
                    block_data[new_offset + 7] = file_type;
                    block_data[new_offset + 8..new_offset + 8 + name_bytes.len()]
                        .copy_from_slice(name_bytes);

                    self.write_block(phys_block, &block_data)?;
                    return Ok(());
                }

                offset += rec_len;
            }
        }

        // No space found — allocate a new block for the directory.
        let new_block = self.allocate_data_block(dir_inode, num_blocks)?;
        let mut block_data = vec![0u8; bs as usize];

        // The new entry fills the entire block.
        block_data[0..4].copy_from_slice(&child_inode.to_le_bytes());
        block_data[4..6].copy_from_slice(&(bs as u16).to_le_bytes());
        block_data[6] = name_bytes.len() as u8;
        block_data[7] = file_type;
        block_data[8..8 + name_bytes.len()].copy_from_slice(name_bytes);

        self.write_block(new_block, &block_data)?;
        dir_inode.size += bs as u32;
        self.write_inode(dir_inode_num, dir_inode)?;
        Ok(())
    }

    /// Create a new regular file (P28-T037).
    pub fn create_file(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32, Ext2Error> {
        let parent_inode = self.read_inode(parent_inode_num)?;
        if !parent_inode.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }

        let parent_group = kernel_core::fs::ext2::inode_block_group(
            parent_inode_num,
            self.superblock.inodes_per_group,
        );
        let new_ino = self.allocate_inode(parent_group)?;

        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFREG | (mode & 0o7777);
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        inode.links_count = 1;
        self.write_inode(new_ino, &inode)?;

        let mut parent_inode = self.read_inode(parent_inode_num)?;
        self.add_directory_entry(
            parent_inode_num,
            &mut parent_inode,
            name,
            new_ino,
            EXT2_FT_REG_FILE,
        )?;

        Ok(new_ino)
    }

    /// Create a new directory (P28-T038).
    pub fn create_directory(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32, Ext2Error> {
        let parent_inode = self.read_inode(parent_inode_num)?;
        if !parent_inode.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }

        // Check if an entry with this name already exists.
        if self.lookup_in_directory(&parent_inode, name).is_ok() {
            return Err(Ext2Error::AlreadyExists);
        }

        let parent_group = kernel_core::fs::ext2::inode_block_group(
            parent_inode_num,
            self.superblock.inodes_per_group,
        );
        let new_ino = self.allocate_inode(parent_group)?;

        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFDIR | (mode & 0o7777);
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        inode.links_count = 2; // . and parent's entry

        // Allocate one data block for . and .. entries.
        let data_block = self.allocate_block(parent_group)?;
        let bs = self.block_size as usize;
        let mut block_data = vec![0u8; bs];

        // "." entry — points to self
        block_data[0..4].copy_from_slice(&new_ino.to_le_bytes());
        block_data[4..6].copy_from_slice(&12u16.to_le_bytes()); // rec_len = 12
        block_data[6] = 1; // name_len
        block_data[7] = EXT2_FT_DIR;
        block_data[8] = b'.';

        // ".." entry — points to parent, fills rest of block
        let dotdot_rec_len = (bs - 12) as u16;
        block_data[12..16].copy_from_slice(&parent_inode_num.to_le_bytes());
        block_data[16..18].copy_from_slice(&dotdot_rec_len.to_le_bytes());
        block_data[18] = 2; // name_len
        block_data[19] = EXT2_FT_DIR;
        block_data[20] = b'.';
        block_data[21] = b'.';

        self.write_block(data_block, &block_data)?;

        inode.block[0] = data_block;
        inode.size = bs as u32;
        inode.blocks = self.block_size / 512;
        self.write_inode(new_ino, &inode)?;

        // Add entry in parent directory.
        let mut parent_inode = self.read_inode(parent_inode_num)?;
        self.add_directory_entry(
            parent_inode_num,
            &mut parent_inode,
            name,
            new_ino,
            EXT2_FT_DIR,
        )?;

        // Increment parent's link count (for the ".." entry).
        parent_inode.links_count += 1;
        self.write_inode(parent_inode_num, &parent_inode)?;

        // Update used_dirs_count.
        let group =
            kernel_core::fs::ext2::inode_block_group(new_ino, self.superblock.inodes_per_group)
                as usize;
        self.bgd_table[group].used_dirs_count += 1;
        self.flush_metadata()?;

        Ok(new_ino)
    }

    /// Truncate a file: free all data blocks (P28-T039).
    pub fn truncate_file(
        &mut self,
        inode_num: u32,
        inode: &mut Ext2Inode,
    ) -> Result<(), Ext2Error> {
        let ptrs_per_block = self.block_size / 4;

        // Free direct blocks.
        for i in 0..EXT2_NDIR_BLOCKS {
            if inode.block[i] != 0 {
                self.free_block(inode.block[i])?;
                inode.block[i] = 0;
            }
        }

        // Free single-indirect block and its children.
        if inode.block[EXT2_IND_BLOCK] != 0 {
            let ind_data = self.read_block(inode.block[EXT2_IND_BLOCK])?;
            for i in 0..ptrs_per_block {
                let off = (i as usize) * 4;
                let blk = u32::from_le_bytes([
                    ind_data[off],
                    ind_data[off + 1],
                    ind_data[off + 2],
                    ind_data[off + 3],
                ]);
                if blk != 0 {
                    self.free_block(blk)?;
                }
            }
            self.free_block(inode.block[EXT2_IND_BLOCK])?;
            inode.block[EXT2_IND_BLOCK] = 0;
        }

        // Free double-indirect block and its children.
        if inode.block[EXT2_DIND_BLOCK] != 0 {
            let dind_data = self.read_block(inode.block[EXT2_DIND_BLOCK])?;
            for i in 0..ptrs_per_block {
                let off = (i as usize) * 4;
                let ind_blk = u32::from_le_bytes([
                    dind_data[off],
                    dind_data[off + 1],
                    dind_data[off + 2],
                    dind_data[off + 3],
                ]);
                if ind_blk != 0 {
                    let ind_data = self.read_block(ind_blk)?;
                    for j in 0..ptrs_per_block {
                        let off2 = (j as usize) * 4;
                        let blk = u32::from_le_bytes([
                            ind_data[off2],
                            ind_data[off2 + 1],
                            ind_data[off2 + 2],
                            ind_data[off2 + 3],
                        ]);
                        if blk != 0 {
                            self.free_block(blk)?;
                        }
                    }
                    self.free_block(ind_blk)?;
                }
            }
            self.free_block(inode.block[EXT2_DIND_BLOCK])?;
            inode.block[EXT2_DIND_BLOCK] = 0;
        }

        inode.size = 0;
        inode.blocks = 0;
        self.write_inode(inode_num, inode)
    }

    /// Remove a directory entry by name (P28-T040).
    pub fn remove_directory_entry(
        &mut self,
        dir_inode: &Ext2Inode,
        name: &str,
    ) -> Result<(), Ext2Error> {
        let name_bytes = name.as_bytes();
        let dir_size = dir_inode.size as u64;
        let bs = self.block_size as u64;
        let num_blocks = dir_size.div_ceil(bs) as u32;

        for logical_block in 0..num_blocks {
            let phys_block = self.resolve_block(dir_inode, logical_block)?;
            if phys_block == 0 {
                continue;
            }
            let mut block_data = self.read_block(phys_block)?;
            let mut offset = 0;
            let mut prev_offset: Option<usize> = None;

            while offset + 8 <= block_data.len() {
                let rec_len =
                    u16::from_le_bytes([block_data[offset + 4], block_data[offset + 5]]) as usize;
                if rec_len == 0 {
                    break;
                }

                let entry_name_len = block_data[offset + 6] as usize;
                let entry_inode = u32::from_le_bytes([
                    block_data[offset],
                    block_data[offset + 1],
                    block_data[offset + 2],
                    block_data[offset + 3],
                ]);

                if entry_inode != 0
                    && entry_name_len == name_bytes.len()
                    && &block_data[offset + 8..offset + 8 + entry_name_len] == name_bytes
                {
                    if let Some(prev) = prev_offset {
                        // Merge with previous entry.
                        let prev_rec_len =
                            u16::from_le_bytes([block_data[prev + 4], block_data[prev + 5]])
                                as usize;
                        let new_prev_rec_len = prev_rec_len + rec_len;
                        block_data[prev + 4..prev + 6]
                            .copy_from_slice(&(new_prev_rec_len as u16).to_le_bytes());
                    } else {
                        // First entry in block — just zero the inode.
                        block_data[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    self.write_block(phys_block, &block_data)?;
                    return Ok(());
                }

                prev_offset = Some(offset);
                offset += rec_len;
            }
        }

        Err(Ext2Error::NotFound)
    }

    /// Delete a regular file (P28-T041).
    pub fn delete_file(&mut self, parent_inode_num: u32, name: &str) -> Result<(), Ext2Error> {
        let parent_inode = self.read_inode(parent_inode_num)?;
        let child_ino = self.lookup_in_directory(&parent_inode, name)?;
        let mut child_inode = self.read_inode(child_ino)?;

        if child_inode.is_dir() {
            return Err(Ext2Error::IsDirectory);
        }

        child_inode.links_count = child_inode.links_count.saturating_sub(1);
        let open_count = crate::process::ext2_inode_open_count(child_ino);
        if child_inode.links_count != 0 || open_count != 0 {
            self.write_inode(child_ino, &child_inode)?;
        }

        self.remove_directory_entry(&parent_inode, name)?;

        if child_inode.links_count == 0 && open_count == 0 {
            self.truncate_file(child_ino, &mut child_inode)?;
            self.free_inode(child_ino)?;
        }

        Ok(())
    }

    /// Create a hard link to an existing non-directory inode.
    pub fn create_hard_link(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        target_ino: u32,
    ) -> Result<(), Ext2Error> {
        let parent_inode = self.read_inode(parent_inode_num)?;
        if !parent_inode.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }
        if self.lookup_in_directory(&parent_inode, name).is_ok() {
            return Err(Ext2Error::AlreadyExists);
        }

        let mut target_inode = self.read_inode(target_ino)?;
        if target_inode.is_dir() {
            return Err(Ext2Error::IsDirectory);
        }

        target_inode.links_count = target_inode.links_count.saturating_add(1);
        self.write_inode(target_ino, &target_inode)?;

        let file_type = if target_inode.is_symlink() {
            EXT2_FT_SYMLINK
        } else {
            EXT2_FT_REG_FILE
        };
        let mut parent_inode = self.read_inode(parent_inode_num)?;
        if let Err(err) = self.add_directory_entry(
            parent_inode_num,
            &mut parent_inode,
            name,
            target_ino,
            file_type,
        ) {
            target_inode.links_count = target_inode.links_count.saturating_sub(1);
            let _ = self.write_inode(target_ino, &target_inode);
            return Err(err);
        }

        Ok(())
    }

    /// Delete an empty directory (P28-T042).
    pub fn delete_directory(&mut self, parent_inode_num: u32, name: &str) -> Result<(), Ext2Error> {
        let parent_inode = self.read_inode(parent_inode_num)?;
        let child_ino = self.lookup_in_directory(&parent_inode, name)?;
        let mut child_inode = self.read_inode(child_ino)?;

        if !child_inode.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }

        // Verify directory is empty (only . and ..).
        let entries = self.read_directory_entries(&child_inode)?;
        for (entry_name, _, _) in &entries {
            if entry_name != "." && entry_name != ".." {
                return Err(Ext2Error::NotEmpty); // Not empty
            }
        }

        self.truncate_file(child_ino, &mut child_inode)?;
        self.free_inode(child_ino)?;
        self.remove_directory_entry(&parent_inode, name)?;

        // Decrement parent's link count.
        let mut parent_inode = self.read_inode(parent_inode_num)?;
        parent_inode.links_count = parent_inode.links_count.saturating_sub(1);
        self.write_inode(parent_inode_num, &parent_inode)?;

        // Update used_dirs_count.
        let group =
            kernel_core::fs::ext2::inode_block_group(child_ino, self.superblock.inodes_per_group)
                as usize;
        if self.bgd_table[group].used_dirs_count > 0 {
            self.bgd_table[group].used_dirs_count -= 1;
        }
        self.flush_metadata()
    }

    // -----------------------------------------------------------------------
    // VFS metadata operations (P28-T044 through P28-T046)
    // -----------------------------------------------------------------------

    /// Get metadata for a path (P28-T044).
    pub fn metadata(&self, path: &str) -> Result<(u32, u32, u16, u32, u32), Ext2Error> {
        let ino = self.resolve_path(path)?;
        let inode = self.read_inode(ino)?;
        Ok((
            inode.uid as u32,
            inode.gid as u32,
            inode.mode,
            inode.size,
            inode.mtime,
        ))
    }

    /// Set ownership and permission mode on a path (P28-T045).
    pub fn set_metadata(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        mode: u16,
    ) -> Result<(), Ext2Error> {
        let ino = self.resolve_path(path)?;
        let mut inode = self.read_inode(ino)?;
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        // Preserve the file type bits, only update permission bits.
        inode.mode = (inode.mode & 0xF000) | (mode & 0o7777);
        self.write_inode(ino, &inode)
    }

    /// List files in a directory, returning (name, is_dir) pairs.
    pub fn list_dir(&self, path: &str) -> Result<Vec<(String, bool)>, Ext2Error> {
        let ino = self.resolve_path(path)?;
        let inode = self.read_inode(ino)?;
        let entries = self.read_directory_entries(&inode)?;

        let mut result = Vec::new();
        for (name, _, file_type) in entries {
            if name == "." || name == ".." {
                continue;
            }
            result.push((name, file_type == EXT2_FT_DIR));
        }
        Ok(result)
    }

    /// Check if a path exists.
    pub fn exists(&self, path: &str) -> bool {
        self.resolve_path(path).is_ok()
    }

    /// Check if a path is a directory.
    pub fn is_dir(&self, path: &str) -> bool {
        match self.resolve_path(path) {
            Ok(ino) => match self.read_inode(ino) {
                Ok(inode) => inode.is_dir(),
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    // -----------------------------------------------------------------------
    // Symlink operations (Phase 38)
    // -----------------------------------------------------------------------

    /// Maximum symlink target length stored inline in the inode's block array.
    const SYMLINK_INLINE_MAX: usize = 60; // 15 × 4 bytes

    /// Create a symbolic link in directory `parent_inode_num` with the given
    /// `name`, pointing at `target`.
    ///
    /// Short targets (≤60 bytes) are stored inline in the inode block pointers;
    /// longer targets are stored in an allocated data block.
    pub fn create_symlink(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> Result<u32, Ext2Error> {
        let parent_inode = self.read_inode(parent_inode_num)?;
        if !parent_inode.is_dir() {
            return Err(Ext2Error::NotDirectory);
        }

        if self.lookup_in_directory(&parent_inode, name).is_ok() {
            return Err(Ext2Error::AlreadyExists);
        }

        let parent_group = kernel_core::fs::ext2::inode_block_group(
            parent_inode_num,
            self.superblock.inodes_per_group,
        );
        let new_ino = self.allocate_inode(parent_group)?;

        let target_bytes = target.as_bytes();
        if target_bytes.len() > self.block_size as usize {
            self.free_inode(new_ino)?;
            return Err(Ext2Error::OutOfSpace);
        }
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFLNK | 0o777;
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        inode.links_count = 1;
        inode.size = target_bytes.len() as u32;
        let mut allocated_block = None;

        if target_bytes.len() <= Self::SYMLINK_INLINE_MAX {
            // Inline: store target bytes directly in the block pointer array.
            let mut raw = [0u8; 60];
            raw[..target_bytes.len()].copy_from_slice(target_bytes);
            for (i, slot) in inode.block.iter_mut().enumerate() {
                let off = i * 4;
                *slot = u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
            }
            // blocks stays 0 for inline symlinks
        } else {
            // Block-backed: allocate a data block and write the target into it.
            let data_block = self.allocate_block(parent_group)?;
            allocated_block = Some(data_block);
            let bs = self.block_size as usize;
            let mut block_data = vec![0u8; bs];
            block_data[..target_bytes.len()].copy_from_slice(target_bytes);
            if let Err(err) = self.write_block(data_block, &block_data) {
                if let Some(block) = allocated_block.take()
                    && let Err(cleanup_err) = self.free_block(block)
                {
                    log::warn!(
                        "[ext2] create_symlink cleanup failed freeing block {} after write error: {:?}",
                        block,
                        cleanup_err
                    );
                }
                if let Err(cleanup_err) = self.free_inode(new_ino) {
                    log::warn!(
                        "[ext2] create_symlink cleanup failed freeing inode {} after write error: {:?}",
                        new_ino,
                        cleanup_err
                    );
                }
                return Err(err);
            }

            inode.block[0] = data_block;
            inode.blocks = self.block_size / 512;
        }

        if let Err(err) = self.write_inode(new_ino, &inode) {
            if let Some(block) = allocated_block.take()
                && let Err(cleanup_err) = self.free_block(block)
            {
                log::warn!(
                    "[ext2] create_symlink cleanup failed freeing block {} after inode write error: {:?}",
                    block,
                    cleanup_err
                );
            }
            if let Err(cleanup_err) = self.free_inode(new_ino) {
                log::warn!(
                    "[ext2] create_symlink cleanup failed freeing inode {} after inode write error: {:?}",
                    new_ino,
                    cleanup_err
                );
            }
            return Err(err);
        }

        // Add directory entry with EXT2_FT_SYMLINK type.
        let mut parent_inode = self.read_inode(parent_inode_num)?;
        if let Err(err) = self.add_directory_entry(
            parent_inode_num,
            &mut parent_inode,
            name,
            new_ino,
            EXT2_FT_SYMLINK,
        ) {
            if let Some(block) = allocated_block
                && let Err(cleanup_err) = self.free_block(block)
            {
                log::warn!(
                    "[ext2] create_symlink cleanup failed freeing block {} after dir entry error: {:?}",
                    block,
                    cleanup_err
                );
            }
            if let Err(cleanup_err) = self.free_inode(new_ino) {
                log::warn!(
                    "[ext2] create_symlink cleanup failed freeing inode {} after dir entry error: {:?}",
                    new_ino,
                    cleanup_err
                );
            }
            return Err(err);
        }

        Ok(new_ino)
    }

    /// Read the target of a symbolic link inode.
    ///
    /// Returns `Ext2Error::NotSymlink` if the inode is not a symlink.
    pub fn read_symlink(&self, inode_num: u32) -> Result<String, Ext2Error> {
        let inode = self.read_inode(inode_num)?;
        if !inode.is_symlink() {
            return Err(Ext2Error::NotSymlink);
        }

        let target_len = inode.size as usize;

        if inode.blocks == 0 && target_len <= Self::SYMLINK_INLINE_MAX {
            // Inline: target is stored in the block pointer array bytes.
            let mut raw = [0u8; 60];
            for (i, &slot) in inode.block.iter().enumerate() {
                let off = i * 4;
                raw[off..off + 4].copy_from_slice(&slot.to_le_bytes());
            }
            let bytes = &raw[..target_len];
            String::from_utf8(bytes.to_vec()).map_err(|_| Ext2Error::CorruptedEntry)
        } else {
            // Block-backed: read from the first data block.
            let block_num = inode.block[0];
            if block_num == 0 {
                return Err(Ext2Error::CorruptedEntry);
            }
            let block_data = self.read_block(block_num)?;
            if target_len > block_data.len() {
                return Err(Ext2Error::CorruptedEntry);
            }
            String::from_utf8(block_data[..target_len].to_vec())
                .map_err(|_| Ext2Error::CorruptedEntry)
        }
    }
}

// ---------------------------------------------------------------------------
// Module-level API (P28-T020, P28-T053)
// ---------------------------------------------------------------------------

/// Mount an ext2 volume at the given base LBA into the global static.
pub fn mount_ext2(base_lba: u64) -> Result<(), Ext2Error> {
    let vol = Ext2Volume::mount(base_lba)?;
    *EXT2_VOLUME.lock() = Some(vol);
    log::info!("[ext2] volume mounted at base LBA {}", base_lba);
    Ok(())
}

/// Check if the ext2 volume is mounted.
pub fn is_mounted() -> bool {
    EXT2_VOLUME.lock().is_some()
}

/// Get uid/gid/mode for an ext2 file by its root-relative path.
/// Returns `None` if the file is not found or the volume is not mounted.
pub fn get_ext2_meta(path: &str) -> Option<(u32, u32, u16)> {
    let vol = EXT2_VOLUME.lock();
    match vol.as_ref() {
        Some(vol) => match vol.metadata(path) {
            Ok((uid, gid, mode, _, _)) => Some((uid, gid, mode & 0o7777)),
            Err(_) => None,
        },
        None => None,
    }
}
