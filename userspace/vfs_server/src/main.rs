//! Userspace VFS service for m3OS (Phase 54).
//!
//! Owns the migrated ext2 pathname authority for the Phase 54 storage slice.
//! The kernel keeps per-process fd bookkeeping and virtual-filesystem carveouts,
//! while this service answers ext2-backed pathname, metadata, directory, and
//! mount-policy requests via IPC.
//!
//! # Architecture
//!
//! ```text
//! app → open("/etc/passwd") → kernel syscall handler
//!       → detects /etc/ + O_RDONLY + "vfs" registered
//!       → IPC call_msg(vfs_ep, VFS_OPEN, path)
//!       → this server: resolve path, open handle, reply
//!
//! app → read(fd, buf, n) → kernel sees FdBackend::VfsService
//!       → IPC call_msg(vfs_ep, VFS_READ, handle, offset, count)
//!       → this server: read data, store reply bulk, reply
//! ```
//!
//! Raw disk sectors are read via `sys_block_read` (Phase 54 syscall).
//! Ext2 parsing uses `kernel_core::fs::ext2` types.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::cell::RefCell;
use kernel_core::fs::ext2::{
    EXT2_DIND_BLOCK, EXT2_FT_DIR, EXT2_FT_REG_FILE, EXT2_FT_SYMLINK, EXT2_IND_BLOCK,
    EXT2_NDIR_BLOCKS, EXT2_ROOT_INO, Ext2BlockGroupDescriptor, Ext2DirEntry, Ext2Inode,
    Ext2Superblock, S_IFDIR, S_IFLNK, S_IFREG, inode_block_group, inode_index_in_group,
};
use kernel_core::fs::mbr;
use kernel_core::fs::vfs_protocol::{
    VFS_ACCESS_PATH, VFS_CLOSE, VFS_CREATE, VFS_CREATE_KIND_SHIFT, VFS_LINK, VFS_LIST_DIR,
    VFS_MAX_PREAD, VFS_MAX_PWRITE, VFS_MOUNT_EXT2_ROOT, VFS_MOUNT_POLICY, VFS_MOUNT_VFAT_DATA,
    VFS_NODE_DIR, VFS_NODE_FILE, VFS_NODE_SYMLINK, VFS_OPEN, VFS_PREAD, VFS_READ, VFS_RENAME,
    VFS_STAT_PATH, VFS_STAT_REPLY_SIZE, VFS_TRUNCATE, VFS_UMOUNT_EXT2_ROOT, VFS_UMOUNT_POLICY,
    VFS_UMOUNT_VFAT_DATA, VFS_UNLINK, VFS_WRITE,
};
use syscall_lib::STDOUT_FILENO;
use syscall_lib::heap::BrkAllocator;

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

// ---------------------------------------------------------------------------
// Negative errno constants (returned as reply labels)
// ---------------------------------------------------------------------------

const NEG_ENOENT: u64 = (-2i64) as u64;
const NEG_EIO: u64 = (-5i64) as u64;
const NEG_EBADF: u64 = (-9i64) as u64;
const NEG_EEXIST: u64 = (-17i64) as u64;
const NEG_ENOTDIR: u64 = (-20i64) as u64;
const NEG_EISDIR: u64 = (-21i64) as u64;
const NEG_EINVAL: u64 = (-22i64) as u64;
const NEG_ENFILE: u64 = (-23i64) as u64;
const NEG_ENOSPC: u64 = (-28i64) as u64;
const NEG_ENOTEMPTY: u64 = (-39i64) as u64;

// ---------------------------------------------------------------------------
// Ext2 volume state (server-local)
// ---------------------------------------------------------------------------

/// In-process ext2 volume state — replaces `Ext2Volume` from the kernel.
struct Ext2State {
    base_lba: u64,
    superblock: Ext2Superblock,
    bgd_table: Vec<Ext2BlockGroupDescriptor>,
    block_size: u32,
    sectors_per_block: u32,
    /// Phase 87 — bounded read-through block cache. vfs_server was previously
    /// uncached, so every path-resolution re-read its directory / inode / bitmap
    /// / indirect blocks from disk (a `pkg install` issued tens of thousands of
    /// per-block `block_read` round-trips). This caches recently-read ext2 blocks
    /// by block number; `write_sectors` invalidates any overlapping block so the
    /// write authority never serves stale data (mirrors the kernel engine's
    /// `block_cache` + invalidate-on-write). Bounded at `BLOCK_CACHE_MAX` blocks.
    block_cache: RefCell<BTreeMap<u32, Vec<u8>>>,
    /// Phase 87 — metadata write-back. Count of alloc/free ops since the last
    /// superblock+BGD flush (see `mark_meta_dirty`). Deferring the summary flush
    /// off the per-allocation path removes ~2 device reads + 2 device writes per
    /// allocated block.
    meta_dirty_ops: u32,
}

/// Max ext2 blocks held in the vfs_server read-through cache (4 KiB blocks →
/// ~16 MiB ceiling). Large enough to hold a package's metadata working set
/// (dir/inode/bitmap/indirect blocks) across an install without unbounded growth.
const BLOCK_CACHE_MAX: usize = 4096;

/// Phase 87 — flush the superblock + BGD free-count summaries to disk after at
/// most this many alloc/free ops (instead of on every one). Bounds a crash's
/// free-count drift to this many ops, which `fsck` reconciles from the
/// already-persisted bitmaps.
const META_FLUSH_THRESHOLD: u32 = 256;

impl Ext2State {
    /// Read raw sectors from disk via the sys_block_read syscall.
    fn read_sectors(&self, start_lba: u64, count: usize, buf: &mut [u8]) -> Result<(), ()> {
        let ret = syscall_lib::block_read(start_lba, count, buf);
        if ret < 0 { Err(()) } else { Ok(()) }
    }

    /// First-block LBA of ext2 block `block_num`.
    fn block_to_lba(&self, block_num: u32) -> u64 {
        self.base_lba + (block_num as u64) * (self.sectors_per_block as u64)
    }

    /// Read one ext2 block into a freshly allocated buffer (Phase 87: cached).
    ///
    /// Cache hit returns a clone of the cached block — no `block_read` syscall,
    /// which is the whole win (the ring0↔ring3↔driver round-trip, not the
    /// userspace copy). On miss, read from disk and insert (bounded by
    /// `BLOCK_CACHE_MAX`). The cache holds clean disk content: `write_sectors`
    /// invalidates any overlapping block, so a hit never serves stale data.
    fn read_block(&self, block_num: u32) -> Result<Vec<u8>, ()> {
        if let Some(cached) = self.block_cache.borrow().get(&block_num) {
            return Ok(cached.clone());
        }
        let lba = self.block_to_lba(block_num);
        let mut buf = vec![0u8; self.block_size as usize];
        let sector_count = self.sectors_per_block as usize;
        self.read_sectors(lba, sector_count, &mut buf)?;
        {
            let mut cache = self.block_cache.borrow_mut();
            if cache.len() < BLOCK_CACHE_MAX {
                cache.insert(block_num, buf.clone());
            }
        }
        Ok(buf)
    }

    /// Invalidate any cached blocks overlapping the raw LBA range
    /// `[start_lba, start_lba + count)`. The single choke point for cache
    /// coherence: every write (block-level via `write_block`, or raw
    /// superblock/BGD via `write_sectors`) routes through `write_sectors`, so
    /// dropping the overlapping blocks here keeps the read-through cache coherent
    /// with the on-disk state the write authority just produced.
    fn invalidate_lba_range(&self, start_lba: u64, count: usize) {
        let spb = self.sectors_per_block as u64;
        if spb == 0 || start_lba < self.base_lba {
            // Below the data region (shouldn't happen) — clear all to be safe.
            self.block_cache.borrow_mut().clear();
            return;
        }
        let first = (start_lba - self.base_lba) / spb;
        let end_lba = start_lba + count as u64;
        let last = (end_lba - self.base_lba).div_ceil(spb);
        let mut cache = self.block_cache.borrow_mut();
        for b in first..last {
            cache.remove(&(b as u32));
        }
    }

    // -----------------------------------------------------------------------
    // Phase 88 — write side. Ports the kernel engine (`kernel/src/fs/ext2.rs`)
    // faithfully, swapping the block backend from `crate::blk` to the
    // `block_read`/`block_write` syscalls. Phase 87 added a read-through block
    // cache, so `write_sectors` now invalidates overlapping blocks (the single
    // coherence choke point — `write_block` routes through here).
    // -----------------------------------------------------------------------

    /// Write raw sectors to disk via the sys_block_write syscall.
    ///
    /// Phase 87 — only the raw superblock / block-group-descriptor writes use
    /// this directly now (block writes go through `write_block`, which is
    /// write-through). Since this can land sub-block / multi-block content the
    /// cache can't refresh in place, it invalidates any overlapping cached block
    /// so a later read re-reads the new on-disk content rather than stale data.
    fn write_sectors(&self, start_lba: u64, count: usize, buf: &[u8]) -> Result<(), ()> {
        self.invalidate_lba_range(start_lba, count);
        let ret = syscall_lib::block_write(start_lba, count, buf);
        if ret < 0 { Err(()) } else { Ok(()) }
    }

    /// Write one ext2 block to disk and **write-through update** the cache with
    /// the new content (LBA math mirrors `read_block`).
    ///
    /// Phase 87 — refreshing the cache instead of invalidating it is the whole
    /// point for metadata: ext2 updates are sub-block (one allocation-bitmap bit,
    /// one directory entry, one inode in a shared table block), so the code reads
    /// the whole block, modifies it, and writes it back. Across a burst of
    /// allocations the *same* bitmap / table / directory block is rewritten over
    /// and over; keeping the just-written copy in cache means the read half of
    /// each read-modify-write is a cache hit instead of a fresh disk round-trip
    /// (invalidate-on-write would force ~one re-read per allocation). The cache
    /// stays coherent — it holds exactly what was just written to disk.
    fn write_block(&self, block_num: u32, data: &[u8]) -> Result<(), ()> {
        let lba = self.block_to_lba(block_num);
        let ret = syscall_lib::block_write(lba, self.sectors_per_block as usize, data);
        if ret < 0 {
            // Write failed — drop any cached copy so a later read re-reads the
            // actual (possibly partially-written) on-disk state, never a value
            // we cannot prove landed.
            self.block_cache.borrow_mut().remove(&block_num);
            return Err(());
        }
        let mut cache = self.block_cache.borrow_mut();
        if data.len() == self.block_size as usize {
            // Update if present (the hot path); otherwise insert while bounded.
            if cache.contains_key(&block_num) || cache.len() < BLOCK_CACHE_MAX {
                cache.insert(block_num, data.to_vec());
            }
        } else {
            // Defensive: a short/odd write can't refresh a full cached block, so
            // drop it rather than cache a malformed entry (no caller does this).
            cache.remove(&block_num);
        }
        Ok(())
    }

    /// Phase 87 — write a contiguous run of `count` whole blocks in one
    /// multi-block `block_write` (the write analog of the read coalescer).
    /// `data.len()` must be exactly `count * block_size`. The run is file
    /// payload, not hot metadata, so its blocks are *invalidated* from the cache
    /// (a later read re-reads the fresh disk content) rather than refreshed —
    /// caching a multi-block sequential write would thrash the metadata-oriented
    /// cache, exactly as the read path bypasses the cache for bulk runs.
    /// `sys_block_write` caps a request at 128 sectors, so callers bound `count`
    /// to `128 / sectors_per_block`.
    fn write_block_run(&self, start_block: u32, count: usize, data: &[u8]) -> Result<(), ()> {
        let lba = self.block_to_lba(start_block);
        let sectors = count * self.sectors_per_block as usize;
        let ret = syscall_lib::block_write(lba, sectors, data);
        // Invalidate every block in the run (on success the disk now holds the
        // new payload and the cache is clear so reads re-read it; on failure we
        // never serve a value we cannot prove landed).
        let mut cache = self.block_cache.borrow_mut();
        for i in 0..count as u32 {
            cache.remove(&(start_block + i));
        }
        if ret < 0 { Err(()) } else { Ok(()) }
    }

    /// Write an inode back to disk (mirrors kernel `write_inode`).
    fn write_inode(&self, inode_num: u32, inode: &Ext2Inode) -> Result<(), ()> {
        let group = inode_block_group(inode_num, self.superblock.inodes_per_group);
        let index = inode_index_in_group(inode_num, self.superblock.inodes_per_group);
        let bgd = self.bgd_table.get(group as usize).ok_or(())?;

        let inode_size = self.superblock.inode_size as u32;
        let byte_offset = (index as u64) * (inode_size as u64);
        let block_offset = byte_offset / (self.block_size as u64);
        let offset_in_block = (byte_offset % (self.block_size as u64)) as usize;

        let block_num = bgd.inode_table + block_offset as u32;
        let mut block_data = self.read_block(block_num)?;
        inode.write_into(&mut block_data[offset_in_block..]);
        self.write_block(block_num, &block_data)
    }

    /// Flush the in-memory superblock + BGD counters to disk using a
    /// **read-modify-write** splice: re-read the on-disk superblock and BGD
    /// table, overlay only the counter fields this engine tracks via
    /// `write_into`, then write back. This prevents a stale cached metadata
    /// image from clobbering newer on-disk state — the latent hazard the
    /// kernel engine's clone-and-overwrite `flush_metadata` carries.
    fn flush_metadata(&self) -> Result<(), ()> {
        // Superblock at byte offset 1024 = base_lba + 2 sectors.
        let sb_lba = self.base_lba + 2;
        let mut sb_buf = vec![0u8; 1024];
        self.read_sectors(sb_lba, 2, &mut sb_buf)?;
        self.superblock.write_into(&mut sb_buf);
        self.write_sectors(sb_lba, 2, &sb_buf)?;

        // BGD table starts at the block after the superblock.
        let bgd_block = if self.block_size == 1024 { 2 } else { 1 };
        let bgd_lba = self.base_lba + (bgd_block as u64) * (self.sectors_per_block as u64);
        let bgd_bytes = self.bgd_table.len() * 32;
        let bgd_sectors = bgd_bytes.div_ceil(512);
        let mut bgd_buf = vec![0u8; bgd_sectors * 512];
        // Read-modify-write: start from the on-disk image, then overlay our
        // tracked counter fields (write_into only touches the mutable subset).
        self.read_sectors(bgd_lba, bgd_sectors, &mut bgd_buf)?;
        for (i, bgd) in self.bgd_table.iter().enumerate() {
            bgd.write_into(&mut bgd_buf[i * 32..(i + 1) * 32]);
        }
        self.write_sectors(bgd_lba, bgd_sectors, &bgd_buf)
    }

    /// Phase 87 — record an alloc/free against the in-memory superblock+BGD and
    /// flush the on-disk summaries only once per `META_FLUSH_THRESHOLD` ops. The
    /// in-memory `superblock`/`bgd_table` are updated by the caller and remain
    /// authoritative; the bitmaps are persisted immediately by `write_block`, so
    /// the deferred summary flush never affects allocation correctness — only how
    /// often the expensive sb+BGD read-modify-write hits the disk.
    fn mark_meta_dirty(&mut self) -> Result<(), ()> {
        self.meta_dirty_ops += 1;
        if self.meta_dirty_ops >= META_FLUSH_THRESHOLD {
            self.flush_metadata()?;
            self.meta_dirty_ops = 0;
        }
        Ok(())
    }

    /// Flush any pending superblock+BGD summary changes (at a clean boundary —
    /// e.g. unmount). No-op when nothing is dirty.
    fn flush_metadata_if_dirty(&mut self) -> Result<(), ()> {
        if self.meta_dirty_ops > 0 {
            self.flush_metadata()?;
            self.meta_dirty_ops = 0;
        }
        Ok(())
    }

    /// Read an inode by number.
    fn read_inode(&self, inode_num: u32) -> Result<Ext2Inode, ()> {
        let bg = inode_block_group(inode_num, self.superblock.inodes_per_group);
        let idx = inode_index_in_group(inode_num, self.superblock.inodes_per_group);
        let bgd = self.bgd_table.get(bg as usize).ok_or(())?;

        let inode_size = self.superblock.inode_size as u32;
        let byte_offset = idx * inode_size;
        let block_offset = byte_offset / self.block_size;
        let offset_in_block = (byte_offset % self.block_size) as usize;

        let inode_table_block = bgd.inode_table + block_offset;
        let block_data = self.read_block(inode_table_block)?;

        Ext2Inode::parse(&block_data[offset_in_block..]).map_err(|_| ())
    }

    /// Resolve a block pointer from an inode, handling indirect blocks.
    fn resolve_block(&self, inode: &Ext2Inode, file_block: u32) -> Result<u32, ()> {
        let ptrs_per_block = self.block_size / 4;

        if file_block < 12 {
            return Ok(inode.block[file_block as usize]);
        }

        let file_block = file_block - 12;
        if file_block < ptrs_per_block {
            // Single indirect.
            let indirect_block = inode.block[12];
            if indirect_block == 0 {
                return Ok(0);
            }
            let data = self.read_block(indirect_block)?;
            let off = (file_block as usize) * 4;
            return Ok(u32::from_le_bytes([
                data[off],
                data[off + 1],
                data[off + 2],
                data[off + 3],
            ]));
        }

        let file_block = file_block - ptrs_per_block;
        if file_block < ptrs_per_block * ptrs_per_block {
            // Double indirect.
            let dind_block = inode.block[13];
            if dind_block == 0 {
                return Ok(0);
            }
            let dind_data = self.read_block(dind_block)?;
            let idx1 = (file_block / ptrs_per_block) as usize;
            let off1 = idx1 * 4;
            let ind_block = u32::from_le_bytes([
                dind_data[off1],
                dind_data[off1 + 1],
                dind_data[off1 + 2],
                dind_data[off1 + 3],
            ]);
            if ind_block == 0 {
                return Ok(0);
            }
            let ind_data = self.read_block(ind_block)?;
            let idx2 = (file_block % ptrs_per_block) as usize;
            let off2 = idx2 * 4;
            return Ok(u32::from_le_bytes([
                ind_data[off2],
                ind_data[off2 + 1],
                ind_data[off2 + 2],
                ind_data[off2 + 3],
            ]));
        }

        // Triple indirect — not needed for /etc/ config files.
        Err(())
    }

    /// Resolve a path like "/etc/passwd" to its inode number.
    ///
    /// `path` must start with "/" — relative paths are rejected with
    /// `NEG_EINVAL`. Walks from root inode (2).
    fn resolve_path(&self, path: &str) -> Result<u32, u64> {
        let path = path.strip_prefix('/').ok_or(NEG_EINVAL)?;
        let mut current_inode_num: u32 = 2; // root inode

        for component in path.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            // Read current inode — must be a directory.
            let inode = self.read_inode(current_inode_num).map_err(|_| NEG_EIO)?;
            if !inode.is_dir() {
                return Err(NEG_ENOTDIR);
            }
            // Scan directory entries.
            let mut found = false;
            let mut file_block = 0u32;
            let blocks_count = inode.size.div_ceil(self.block_size);
            while file_block < blocks_count {
                let block_num = self
                    .resolve_block(&inode, file_block)
                    .map_err(|_| NEG_EIO)?;
                if block_num == 0 {
                    file_block += 1;
                    continue;
                }
                let block_data = self.read_block(block_num).map_err(|_| NEG_EIO)?;
                let entries = Ext2DirEntry::parse_block(&block_data).map_err(|_| NEG_EIO)?;
                for entry in &entries {
                    if entry.inode != 0 && entry.name == component {
                        current_inode_num = entry.inode;
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
                file_block += 1;
            }
            if !found {
                return Err(NEG_ENOENT);
            }
        }

        Ok(current_inode_num)
    }

    /// Read file data from an inode at a given byte offset.
    fn read_file_data(
        &self,
        inode: &Ext2Inode,
        offset: usize,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ()> {
        let file_size = inode.size as u64;
        if offset as u64 >= file_size {
            return Ok(Vec::new()); // EOF
        }
        let available = file_size - offset as u64;
        let to_read = (max_bytes as u64).min(available) as usize;

        // Phase 87 — coalesce contiguous whole blocks into multi-block
        // `block_read`s (shared with the kernel engine). The bulk run path
        // bypasses the block cache (a multi-MiB sequential read would thrash it,
        // and the cache is for hot metadata, not file payload); the unaligned
        // head/tail keeps the cache-aware `read_block`. `sys_block_read` caps a
        // request at 128 sectors, so a run spans at most 128/sectors_per_block
        // blocks.
        let max_run_blocks = (128 / self.sectors_per_block).max(1);
        let mut result = vec![0u8; to_read];
        kernel_core::fs::ext2::read_file_data_coalesced::<()>(
            file_size,
            self.block_size,
            offset as u64,
            &mut result,
            max_run_blocks,
            |logical_block| self.resolve_block(inode, logical_block),
            |start_block, count, dst| {
                let lba = self.block_to_lba(start_block);
                self.read_sectors(lba, count as usize * self.sectors_per_block as usize, dst)
            },
            |phys_block, offset_in_block, dst| {
                let block = self.read_block(phys_block)?;
                dst.copy_from_slice(&block[offset_in_block..offset_in_block + dst.len()]);
                Ok(())
            },
        )?;
        Ok(result)
    }

    fn read_symlink_target(&self, inode: &Ext2Inode) -> Result<Vec<u8>, ()> {
        if !inode.is_symlink() {
            return Err(());
        }
        let target_len = inode.size as usize;
        if inode.blocks == 0 && target_len <= 60 {
            let mut raw = [0u8; 60];
            for (i, &slot) in inode.block.iter().enumerate() {
                let start = i * 4;
                raw[start..start + 4].copy_from_slice(&slot.to_le_bytes());
            }
            Ok(raw[..target_len].to_vec())
        } else {
            self.read_file_data(inode, 0, target_len)
        }
    }

    fn read_dir_entries(&self, inode: &Ext2Inode) -> Result<Vec<(u32, String, u8)>, u64> {
        let mut entries = Vec::new();
        let mut file_block = 0u32;
        let blocks_count = inode.size.div_ceil(self.block_size);
        while file_block < blocks_count {
            let block_num = self.resolve_block(inode, file_block).map_err(|_| NEG_EIO)?;
            if block_num == 0 {
                file_block += 1;
                continue;
            }
            let block_data = self.read_block(block_num).map_err(|_| NEG_EIO)?;
            let block_entries = Ext2DirEntry::parse_block(&block_data).map_err(|_| NEG_EIO)?;
            for entry in block_entries {
                if entry.inode == 0 {
                    continue;
                }
                let entry_inode = self.read_inode(entry.inode).map_err(|_| NEG_EIO)?;
                entries.push((
                    entry.inode,
                    entry.name,
                    inode_kind_to_dirent_type(&entry_inode),
                ));
            }
            file_block += 1;
        }
        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// Phase 88 — write orchestration (ported faithfully from kernel/src/fs/ext2.rs)
// ---------------------------------------------------------------------------

impl Ext2State {
    /// Resolve an absolute path to an inode number (mirrors the kernel engine's
    /// `Ext2Volume::resolve_path`; `resolve_path` above returns `u64` errnos and
    /// is kept for the read handlers).
    fn lookup_inode(&self, path: &str) -> Result<u32, u64> {
        self.resolve_path(path)
    }

    /// Look up a name in a directory inode, returning the child inode number.
    fn lookup_in_directory(&self, dir_inode: &Ext2Inode, name: &str) -> Result<u32, u64> {
        let mut file_block = 0u32;
        let blocks_count = dir_inode.size.div_ceil(self.block_size);
        while file_block < blocks_count {
            let block_num = self
                .resolve_block(dir_inode, file_block)
                .map_err(|_| NEG_EIO)?;
            if block_num == 0 {
                file_block += 1;
                continue;
            }
            let block_data = self.read_block(block_num).map_err(|_| NEG_EIO)?;
            let entries = Ext2DirEntry::parse_block(&block_data).map_err(|_| NEG_EIO)?;
            for entry in &entries {
                if entry.inode != 0 && entry.name == name {
                    return Ok(entry.inode);
                }
            }
            file_block += 1;
        }
        Err(NEG_ENOENT)
    }

    /// Allocate a free block, preferring `preferred_group`.
    fn allocate_block(&mut self, preferred_group: u32) -> Result<u32, u64> {
        let bg_count = self.bgd_table.len();
        for offset in 0..bg_count {
            let group = ((preferred_group as usize) + offset) % bg_count;
            if self.bgd_table[group].free_blocks_count == 0 {
                continue;
            }
            let bitmap_block = self.bgd_table[group].block_bitmap;
            let mut bitmap = self.read_block(bitmap_block).map_err(|_| NEG_EIO)?;

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
                    bitmap[byte_idx] |= 1 << bit_idx;
                    self.write_block(bitmap_block, &bitmap)
                        .map_err(|_| NEG_EIO)?;

                    self.bgd_table[group].free_blocks_count -= 1;
                    self.superblock.free_blocks_count -= 1;

                    let abs_block = (group as u32) * self.superblock.blocks_per_group
                        + bit
                        + self.superblock.first_data_block;

                    self.mark_meta_dirty().map_err(|_| NEG_EIO)?;
                    return Ok(abs_block);
                }
            }
        }
        Err(NEG_ENOSPC)
    }

    /// Free a block.
    fn free_block(&mut self, block_num: u32) -> Result<(), u64> {
        if block_num < self.superblock.first_data_block {
            return Err(NEG_EIO);
        }
        let relative = block_num - self.superblock.first_data_block;
        let group = (relative / self.superblock.blocks_per_group) as usize;
        if group >= self.bgd_table.len() {
            return Err(NEG_EIO);
        }
        let bit = relative % self.superblock.blocks_per_group;

        let bitmap_block = self.bgd_table[group].block_bitmap;
        let mut bitmap = self.read_block(bitmap_block).map_err(|_| NEG_EIO)?;
        let byte_idx = (bit / 8) as usize;
        let bit_idx = bit % 8;
        if bitmap[byte_idx] & (1 << bit_idx) == 0 {
            return Err(NEG_EIO); // double-free guard
        }
        bitmap[byte_idx] &= !(1 << bit_idx);
        self.write_block(bitmap_block, &bitmap)
            .map_err(|_| NEG_EIO)?;

        self.bgd_table[group].free_blocks_count += 1;
        self.superblock.free_blocks_count += 1;
        self.mark_meta_dirty().map_err(|_| NEG_EIO)
    }

    /// Allocate a free inode, preferring `preferred_group`.
    fn allocate_inode(&mut self, preferred_group: u32) -> Result<u32, u64> {
        let bg_count = self.bgd_table.len();
        for offset in 0..bg_count {
            let group = ((preferred_group as usize) + offset) % bg_count;
            if self.bgd_table[group].free_inodes_count == 0 {
                continue;
            }
            let bitmap_block = self.bgd_table[group].inode_bitmap;
            let mut bitmap = self.read_block(bitmap_block).map_err(|_| NEG_EIO)?;
            let inodes_in_group = self.superblock.inodes_per_group;

            for bit in 0..inodes_in_group {
                let abs_inode = (group as u32) * self.superblock.inodes_per_group + bit + 1;
                if abs_inode > self.superblock.inodes_count {
                    continue;
                }
                let byte_idx = (bit / 8) as usize;
                let bit_idx = bit % 8;
                if bitmap[byte_idx] & (1 << bit_idx) == 0 {
                    bitmap[byte_idx] |= 1 << bit_idx;
                    self.write_block(bitmap_block, &bitmap)
                        .map_err(|_| NEG_EIO)?;

                    self.bgd_table[group].free_inodes_count -= 1;
                    self.superblock.free_inodes_count -= 1;
                    self.mark_meta_dirty().map_err(|_| NEG_EIO)?;
                    return Ok(abs_inode);
                }
            }
        }
        Err(NEG_ENOSPC)
    }

    /// Free an inode.
    fn free_inode(&mut self, inode_num: u32) -> Result<(), u64> {
        if inode_num == 0 || inode_num > self.superblock.inodes_count {
            return Err(NEG_EIO);
        }
        let group = inode_block_group(inode_num, self.superblock.inodes_per_group) as usize;
        if group >= self.bgd_table.len() {
            return Err(NEG_EIO);
        }
        let index = inode_index_in_group(inode_num, self.superblock.inodes_per_group);
        let bitmap_block = self.bgd_table[group].inode_bitmap;
        let mut bitmap = self.read_block(bitmap_block).map_err(|_| NEG_EIO)?;
        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        if bitmap[byte_idx] & (1 << bit_idx) == 0 {
            return Err(NEG_EIO);
        }
        bitmap[byte_idx] &= !(1 << bit_idx);
        self.write_block(bitmap_block, &bitmap)
            .map_err(|_| NEG_EIO)?;

        self.bgd_table[group].free_inodes_count += 1;
        self.superblock.free_inodes_count += 1;
        self.mark_meta_dirty().map_err(|_| NEG_EIO)
    }

    /// Allocate a data block for a logical position in an inode, wiring up
    /// direct / single-indirect / double-indirect pointers as needed.
    ///
    /// Phase 87 — `zero_fill` controls whether a *newly allocated data block* is
    /// zeroed on disk. A caller that immediately writes the **whole** block
    /// (`write_file_data`'s coalesced full-block path) passes `false` to skip a
    /// redundant zero-write that the payload overwrites anyway — this halves the
    /// per-data-block write count. A caller that only partially fills the block
    /// (unaligned head/tail, or a fresh directory block) passes `true` so the
    /// unwritten remainder reads back as zero rather than stale freed content.
    /// Indirect *pointer* blocks are always zeroed regardless — they are
    /// metadata and garbage pointers would corrupt the file.
    fn allocate_data_block(
        &mut self,
        inode: &mut Ext2Inode,
        logical_block: u32,
        zero_fill: bool,
    ) -> Result<u32, u64> {
        let ptrs_per_block = self.block_size / 4;
        let preferred_group = 0;

        if logical_block < EXT2_NDIR_BLOCKS as u32 {
            if inode.block[logical_block as usize] == 0 {
                let new_block = self.allocate_block(preferred_group)?;
                if zero_fill {
                    let zero = vec![0u8; self.block_size as usize];
                    self.write_block(new_block, &zero).map_err(|_| NEG_EIO)?;
                }
                inode.block[logical_block as usize] = new_block;
                inode.blocks += self.block_size / 512;
            }
            return Ok(inode.block[logical_block as usize]);
        }

        let adjusted = logical_block - EXT2_NDIR_BLOCKS as u32;

        if adjusted < ptrs_per_block {
            if inode.block[EXT2_IND_BLOCK] == 0 {
                let ind = self.allocate_block(preferred_group)?;
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(ind, &zero).map_err(|_| NEG_EIO)?;
                inode.block[EXT2_IND_BLOCK] = ind;
                inode.blocks += self.block_size / 512;
            }
            let ind_block = inode.block[EXT2_IND_BLOCK];
            let mut ind_data = self.read_block(ind_block).map_err(|_| NEG_EIO)?;
            let off = (adjusted as usize) * 4;
            let existing = u32::from_le_bytes([
                ind_data[off],
                ind_data[off + 1],
                ind_data[off + 2],
                ind_data[off + 3],
            ]);
            if existing == 0 {
                let new_block = self.allocate_block(preferred_group)?;
                if zero_fill {
                    let zero = vec![0u8; self.block_size as usize];
                    self.write_block(new_block, &zero).map_err(|_| NEG_EIO)?;
                }
                ind_data[off..off + 4].copy_from_slice(&new_block.to_le_bytes());
                self.write_block(ind_block, &ind_data)
                    .map_err(|_| NEG_EIO)?;
                inode.blocks += self.block_size / 512;
                return Ok(new_block);
            }
            return Ok(existing);
        }

        let adjusted = adjusted - ptrs_per_block;

        if adjusted < ptrs_per_block * ptrs_per_block {
            if inode.block[EXT2_DIND_BLOCK] == 0 {
                let dind = self.allocate_block(preferred_group)?;
                let zero = vec![0u8; self.block_size as usize];
                self.write_block(dind, &zero).map_err(|_| NEG_EIO)?;
                inode.block[EXT2_DIND_BLOCK] = dind;
                inode.blocks += self.block_size / 512;
            }
            let dind_block = inode.block[EXT2_DIND_BLOCK];
            let mut dind_data = self.read_block(dind_block).map_err(|_| NEG_EIO)?;

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
                self.write_block(ind_block, &zero).map_err(|_| NEG_EIO)?;
                dind_data[off..off + 4].copy_from_slice(&ind_block.to_le_bytes());
                self.write_block(dind_block, &dind_data)
                    .map_err(|_| NEG_EIO)?;
                inode.blocks += self.block_size / 512;
            }

            let mut ind_data = self.read_block(ind_block).map_err(|_| NEG_EIO)?;
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
                if zero_fill {
                    let zero = vec![0u8; self.block_size as usize];
                    self.write_block(new_block, &zero).map_err(|_| NEG_EIO)?;
                }
                ind_data[off..off + 4].copy_from_slice(&new_block.to_le_bytes());
                self.write_block(ind_block, &ind_data)
                    .map_err(|_| NEG_EIO)?;
                inode.blocks += self.block_size / 512;
                return Ok(new_block);
            }
            return Ok(existing);
        }

        Err(NEG_ENOSPC) // triple-indirect not supported
    }

    /// Write data to a file inode at `offset`. Allocates blocks as needed,
    /// updates inode size/blocks, and writes the inode back. Returns bytes
    /// written.
    fn write_file_data(
        &mut self,
        inode_num: u32,
        inode: &mut Ext2Inode,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, u64> {
        if data.is_empty() {
            return Ok(0);
        }
        let bs = self.block_size as u64;
        let bs_usize = self.block_size as usize;
        let end_offset = offset + data.len() as u64;
        // sys_block_write caps a request at 128 sectors.
        let max_run_blocks = (128 / self.sectors_per_block).max(1) as usize;
        let mut written = 0usize;
        let mut pos = offset;

        // Phase 87 — accumulate a run of physically-contiguous WHOLE blocks and
        // flush it in one multi-block `write_block_run`, instead of one
        // `write_block` per block. Full blocks are also allocated with
        // `zero_fill = false` (the run write covers the whole block, so the
        // separate zero-write is redundant). The unaligned head/tail keep the
        // single-block read-modify-write path. Tuple: (start_phys, data_offset
        // of the run start, run_len in blocks).
        let mut run: Option<(u32, usize, usize)> = None;

        while written < data.len() {
            let logical_block = (pos / bs) as u32;
            let offset_in_block = (pos % bs) as usize;
            let remaining_in_block = bs_usize - offset_in_block;
            let copy_len = remaining_in_block.min(data.len() - written);

            if offset_in_block != 0 || copy_len < bs_usize {
                // Partial (head/tail) block — read-modify-write a single block.
                // Flush any pending whole-block run first so on-disk ordering
                // matches the logical write order.
                if let Some((rp, rstart, rlen)) = run.take() {
                    self.write_block_run(rp, rlen, &data[rstart..rstart + rlen * bs_usize])
                        .map_err(|_| NEG_EIO)?;
                }
                let phys_block = self.allocate_data_block(inode, logical_block, true)?;
                let mut block_data = self.read_block(phys_block).map_err(|_| NEG_EIO)?;
                block_data[offset_in_block..offset_in_block + copy_len]
                    .copy_from_slice(&data[written..written + copy_len]);
                self.write_block(phys_block, &block_data)
                    .map_err(|_| NEG_EIO)?;
                written += copy_len;
                pos += copy_len as u64;
                continue;
            }

            // Whole block — allocate (no zero-fill, the run write covers it) and
            // extend the contiguous run, or flush + start a new one.
            let phys_block = self.allocate_data_block(inode, logical_block, false)?;
            match run {
                Some((rp, rstart, rlen))
                    if phys_block == rp + rlen as u32 && rlen < max_run_blocks =>
                {
                    run = Some((rp, rstart, rlen + 1));
                }
                _ => {
                    if let Some((rp, rstart, rlen)) = run.take() {
                        self.write_block_run(rp, rlen, &data[rstart..rstart + rlen * bs_usize])
                            .map_err(|_| NEG_EIO)?;
                    }
                    run = Some((phys_block, written, 1));
                }
            }
            written += bs_usize;
            pos += bs;
        }
        if let Some((rp, rstart, rlen)) = run.take() {
            self.write_block_run(rp, rlen, &data[rstart..rstart + rlen * bs_usize])
                .map_err(|_| NEG_EIO)?;
        }

        if end_offset > inode.size as u64 {
            inode.size = end_offset as u32;
        }
        self.write_inode(inode_num, inode).map_err(|_| NEG_EIO)?;
        Ok(written)
    }

    /// Add a directory entry to a directory inode.
    fn add_directory_entry(
        &mut self,
        dir_inode_num: u32,
        dir_inode: &mut Ext2Inode,
        name: &str,
        child_inode: u32,
        file_type: u8,
    ) -> Result<(), u64> {
        let name_bytes = name.as_bytes();
        // ext2 stores `name_len` in a single byte; a longer name would truncate
        // via the `as u8` writes below and corrupt the directory. Reject it.
        if name_bytes.len() > 255 {
            return Err(NEG_EINVAL);
        }
        let needed_size = (8 + name_bytes.len()).div_ceil(4) * 4;
        let dir_size = dir_inode.size as u64;
        let bs = self.block_size as u64;
        let num_blocks = dir_size.div_ceil(bs) as u32;

        for logical_block in 0..num_blocks {
            let phys_block = self
                .resolve_block(dir_inode, logical_block)
                .map_err(|_| NEG_EIO)?;
            if phys_block == 0 {
                continue;
            }
            let mut block_data = self.read_block(phys_block).map_err(|_| NEG_EIO)?;
            let mut off = 0;
            while off + 8 <= block_data.len() {
                let rec_len =
                    u16::from_le_bytes([block_data[off + 4], block_data[off + 5]]) as usize;
                if rec_len == 0 {
                    break;
                }
                let entry_name_len = block_data[off + 6] as usize;
                let actual_size = (8 + entry_name_len).div_ceil(4) * 4;
                if rec_len < actual_size {
                    off += rec_len;
                    continue;
                }
                let slack = rec_len - actual_size;
                if slack >= needed_size {
                    block_data[off + 4..off + 6]
                        .copy_from_slice(&(actual_size as u16).to_le_bytes());
                    let new_off = off + actual_size;
                    let new_rec_len = slack as u16;
                    block_data[new_off..new_off + 4].copy_from_slice(&child_inode.to_le_bytes());
                    block_data[new_off + 4..new_off + 6]
                        .copy_from_slice(&new_rec_len.to_le_bytes());
                    block_data[new_off + 6] = name_bytes.len() as u8;
                    block_data[new_off + 7] = file_type;
                    block_data[new_off + 8..new_off + 8 + name_bytes.len()]
                        .copy_from_slice(name_bytes);
                    self.write_block(phys_block, &block_data)
                        .map_err(|_| NEG_EIO)?;
                    return Ok(());
                }
                off += rec_len;
            }
        }

        // No space — allocate a new directory block (zero-filled: the entry
        // below fills only the head, and the remainder must read back as zero).
        let new_block = self.allocate_data_block(dir_inode, num_blocks, true)?;
        let mut block_data = vec![0u8; bs as usize];
        block_data[0..4].copy_from_slice(&child_inode.to_le_bytes());
        block_data[4..6].copy_from_slice(&(bs as u16).to_le_bytes());
        block_data[6] = name_bytes.len() as u8;
        block_data[7] = file_type;
        block_data[8..8 + name_bytes.len()].copy_from_slice(name_bytes);
        self.write_block(new_block, &block_data)
            .map_err(|_| NEG_EIO)?;
        dir_inode.size += bs as u32;
        self.write_inode(dir_inode_num, dir_inode)
            .map_err(|_| NEG_EIO)?;
        Ok(())
    }

    /// Repoint a directory's ".." entry to `new_parent`. Used after a directory
    /// is moved across parents so `<new_parent>/<dir>/..` resolves correctly
    /// (`resolve_path` treats ".." as a normal directory entry).
    fn update_dotdot(&mut self, dir_inode: &Ext2Inode, new_parent: u32) -> Result<(), u64> {
        let bs = self.block_size as u64;
        let num_blocks = (dir_inode.size as u64).div_ceil(bs) as u32;

        for logical_block in 0..num_blocks {
            let phys_block = self
                .resolve_block(dir_inode, logical_block)
                .map_err(|_| NEG_EIO)?;
            if phys_block == 0 {
                continue;
            }
            let mut block_data = self.read_block(phys_block).map_err(|_| NEG_EIO)?;
            let mut off = 0;
            while off + 8 <= block_data.len() {
                let rec_len =
                    u16::from_le_bytes([block_data[off + 4], block_data[off + 5]]) as usize;
                if rec_len == 0 {
                    break;
                }
                let entry_name_len = block_data[off + 6] as usize;
                if entry_name_len == 2
                    && off + 10 <= block_data.len()
                    && &block_data[off + 8..off + 10] == b".."
                {
                    block_data[off..off + 4].copy_from_slice(&new_parent.to_le_bytes());
                    self.write_block(phys_block, &block_data)
                        .map_err(|_| NEG_EIO)?;
                    return Ok(());
                }
                off += rec_len;
            }
        }
        Err(NEG_EIO)
    }

    /// Remove a directory entry by name (merging the slot into its predecessor).
    fn remove_directory_entry(&mut self, dir_inode: &Ext2Inode, name: &str) -> Result<(), u64> {
        let name_bytes = name.as_bytes();
        let bs = self.block_size as u64;
        let num_blocks = (dir_inode.size as u64).div_ceil(bs) as u32;

        for logical_block in 0..num_blocks {
            let phys_block = self
                .resolve_block(dir_inode, logical_block)
                .map_err(|_| NEG_EIO)?;
            if phys_block == 0 {
                continue;
            }
            let mut block_data = self.read_block(phys_block).map_err(|_| NEG_EIO)?;
            let mut off = 0;
            let mut prev_off: Option<usize> = None;
            while off + 8 <= block_data.len() {
                let rec_len =
                    u16::from_le_bytes([block_data[off + 4], block_data[off + 5]]) as usize;
                if rec_len == 0 {
                    break;
                }
                let entry_name_len = block_data[off + 6] as usize;
                let entry_inode = u32::from_le_bytes([
                    block_data[off],
                    block_data[off + 1],
                    block_data[off + 2],
                    block_data[off + 3],
                ]);
                if entry_inode != 0
                    && entry_name_len == name_bytes.len()
                    && &block_data[off + 8..off + 8 + entry_name_len] == name_bytes
                {
                    if let Some(prev) = prev_off {
                        let prev_rec_len =
                            u16::from_le_bytes([block_data[prev + 4], block_data[prev + 5]])
                                as usize;
                        let merged = prev_rec_len + rec_len;
                        block_data[prev + 4..prev + 6]
                            .copy_from_slice(&(merged as u16).to_le_bytes());
                    } else {
                        block_data[off..off + 4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    self.write_block(phys_block, &block_data)
                        .map_err(|_| NEG_EIO)?;
                    return Ok(());
                }
                prev_off = Some(off);
                off += rec_len;
            }
        }
        Err(NEG_ENOENT)
    }

    /// Truncate a file: free all data blocks and reset size/blocks to zero.
    fn truncate_file(&mut self, inode_num: u32, inode: &mut Ext2Inode) -> Result<(), u64> {
        let ptrs_per_block = self.block_size / 4;

        for i in 0..EXT2_NDIR_BLOCKS {
            if inode.block[i] != 0 {
                self.free_block(inode.block[i])?;
                inode.block[i] = 0;
            }
        }

        if inode.block[EXT2_IND_BLOCK] != 0 {
            let ind_data = self
                .read_block(inode.block[EXT2_IND_BLOCK])
                .map_err(|_| NEG_EIO)?;
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

        if inode.block[EXT2_DIND_BLOCK] != 0 {
            let dind_data = self
                .read_block(inode.block[EXT2_DIND_BLOCK])
                .map_err(|_| NEG_EIO)?;
            for i in 0..ptrs_per_block {
                let off = (i as usize) * 4;
                let ind_blk = u32::from_le_bytes([
                    dind_data[off],
                    dind_data[off + 1],
                    dind_data[off + 2],
                    dind_data[off + 3],
                ]);
                if ind_blk != 0 {
                    let ind_data = self.read_block(ind_blk).map_err(|_| NEG_EIO)?;
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
        self.write_inode(inode_num, inode).map_err(|_| NEG_EIO)
    }

    /// Create a new regular file in `parent_inode_num`.
    fn create_file(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32, u64> {
        let parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        if !parent_inode.is_dir() {
            return Err(NEG_ENOTDIR);
        }
        if self.lookup_in_directory(&parent_inode, name).is_ok() {
            return Err(NEG_EEXIST);
        }

        let parent_group = inode_block_group(parent_inode_num, self.superblock.inodes_per_group);
        let new_ino = self.allocate_inode(parent_group)?;

        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFREG | (mode & 0o7777);
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        inode.links_count = 1;
        self.write_inode(new_ino, &inode).map_err(|_| NEG_EIO)?;

        let mut parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        self.add_directory_entry(
            parent_inode_num,
            &mut parent_inode,
            name,
            new_ino,
            EXT2_FT_REG_FILE,
        )?;
        Ok(new_ino)
    }

    /// Create a new directory in `parent_inode_num`.
    fn create_directory(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32, u64> {
        let parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        if !parent_inode.is_dir() {
            return Err(NEG_ENOTDIR);
        }
        if self.lookup_in_directory(&parent_inode, name).is_ok() {
            return Err(NEG_EEXIST);
        }

        let parent_group = inode_block_group(parent_inode_num, self.superblock.inodes_per_group);
        let new_ino = self.allocate_inode(parent_group)?;

        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFDIR | (mode & 0o7777);
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        inode.links_count = 2;

        let data_block = self.allocate_block(parent_group)?;
        let bs = self.block_size as usize;
        let mut block_data = vec![0u8; bs];

        block_data[0..4].copy_from_slice(&new_ino.to_le_bytes());
        block_data[4..6].copy_from_slice(&12u16.to_le_bytes());
        block_data[6] = 1;
        block_data[7] = EXT2_FT_DIR;
        block_data[8] = b'.';

        let dotdot_rec_len = (bs - 12) as u16;
        block_data[12..16].copy_from_slice(&parent_inode_num.to_le_bytes());
        block_data[16..18].copy_from_slice(&dotdot_rec_len.to_le_bytes());
        block_data[18] = 2;
        block_data[19] = EXT2_FT_DIR;
        block_data[20] = b'.';
        block_data[21] = b'.';
        self.write_block(data_block, &block_data)
            .map_err(|_| NEG_EIO)?;

        inode.block[0] = data_block;
        inode.size = bs as u32;
        inode.blocks = self.block_size / 512;
        self.write_inode(new_ino, &inode).map_err(|_| NEG_EIO)?;

        let mut parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        self.add_directory_entry(
            parent_inode_num,
            &mut parent_inode,
            name,
            new_ino,
            EXT2_FT_DIR,
        )?;

        parent_inode.links_count += 1;
        self.write_inode(parent_inode_num, &parent_inode)
            .map_err(|_| NEG_EIO)?;

        let group = inode_block_group(new_ino, self.superblock.inodes_per_group) as usize;
        self.bgd_table[group].used_dirs_count += 1;
        self.mark_meta_dirty().map_err(|_| NEG_EIO)?;
        Ok(new_ino)
    }

    /// Maximum symlink target length stored inline in the inode block array.
    const SYMLINK_INLINE_MAX: usize = 60;

    /// Create a symbolic link in `parent_inode_num` pointing at `target`.
    fn create_symlink(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> Result<u32, u64> {
        let parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        if !parent_inode.is_dir() {
            return Err(NEG_ENOTDIR);
        }
        if self.lookup_in_directory(&parent_inode, name).is_ok() {
            return Err(NEG_EEXIST);
        }

        let parent_group = inode_block_group(parent_inode_num, self.superblock.inodes_per_group);
        let new_ino = self.allocate_inode(parent_group)?;

        let target_bytes = target.as_bytes();
        if target_bytes.len() > self.block_size as usize {
            let _ = self.free_inode(new_ino);
            return Err(NEG_ENOSPC);
        }
        let mut inode = Ext2Inode::new_empty();
        inode.mode = S_IFLNK | 0o777;
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        inode.links_count = 1;
        inode.size = target_bytes.len() as u32;

        if target_bytes.len() <= Self::SYMLINK_INLINE_MAX {
            let mut raw = [0u8; 60];
            raw[..target_bytes.len()].copy_from_slice(target_bytes);
            for (i, slot) in inode.block.iter_mut().enumerate() {
                let off = i * 4;
                *slot = u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
            }
        } else {
            let data_block = self.allocate_block(parent_group)?;
            let bs = self.block_size as usize;
            let mut block_data = vec![0u8; bs];
            block_data[..target_bytes.len()].copy_from_slice(target_bytes);
            if self.write_block(data_block, &block_data).is_err() {
                let _ = self.free_block(data_block);
                let _ = self.free_inode(new_ino);
                return Err(NEG_EIO);
            }
            inode.block[0] = data_block;
            inode.blocks = self.block_size / 512;
        }

        if self.write_inode(new_ino, &inode).is_err() {
            if inode.block[0] != 0 && inode.blocks != 0 {
                let _ = self.free_block(inode.block[0]);
            }
            let _ = self.free_inode(new_ino);
            return Err(NEG_EIO);
        }

        let mut parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        if let Err(err) = self.add_directory_entry(
            parent_inode_num,
            &mut parent_inode,
            name,
            new_ino,
            EXT2_FT_SYMLINK,
        ) {
            if inode.block[0] != 0 && inode.blocks != 0 {
                let _ = self.free_block(inode.block[0]);
            }
            let _ = self.free_inode(new_ino);
            return Err(err);
        }
        Ok(new_ino)
    }

    /// Create a hard link `name` in `parent_inode_num` to the existing
    /// non-directory inode `target_ino`.
    fn create_hard_link(
        &mut self,
        parent_inode_num: u32,
        name: &str,
        target_ino: u32,
    ) -> Result<(), u64> {
        let parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        if !parent_inode.is_dir() {
            return Err(NEG_ENOTDIR);
        }
        if self.lookup_in_directory(&parent_inode, name).is_ok() {
            return Err(NEG_EEXIST);
        }

        let mut target_inode = self.read_inode(target_ino).map_err(|_| NEG_EIO)?;
        if target_inode.is_dir() {
            return Err(NEG_EISDIR);
        }

        target_inode.links_count = target_inode.links_count.saturating_add(1);
        self.write_inode(target_ino, &target_inode)
            .map_err(|_| NEG_EIO)?;

        let file_type = if target_inode.is_symlink() {
            EXT2_FT_SYMLINK
        } else {
            EXT2_FT_REG_FILE
        };
        let mut parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
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

    /// Resolve a path to (parent_inode_num, final_component). Rejects empty
    /// final components and paths without a leading '/'.
    fn resolve_parent_and_name<'p>(&self, path: &'p str) -> Result<(u32, &'p str), u64> {
        let trimmed = path.strip_prefix('/').ok_or(NEG_EINVAL)?;
        let parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return Err(NEG_EINVAL);
        }
        let name = parts[parts.len() - 1];
        let parent_ino = if parts.len() == 1 {
            EXT2_ROOT_INO
        } else {
            // Re-slice the original path to obtain the parent prefix.
            let name_start = path.len() - name.len();
            let parent_path = &path[..name_start];
            self.lookup_inode(parent_path)?
        };
        Ok((parent_ino, name))
    }

    /// Unlink a non-directory entry (decrement links; reclaim when zero and no
    /// open references). The open-reference recount lives in the kernel, which
    /// only routes the unlink here once it has confirmed no fd aliases remain
    /// (mirrors the kernel `delete_file` minus the `ext2_inode_open_count`
    /// check that has no userspace analogue).
    fn delete_file(&mut self, parent_inode_num: u32, name: &str) -> Result<(), u64> {
        let parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        let child_ino = self.lookup_in_directory(&parent_inode, name)?;
        let mut child_inode = self.read_inode(child_ino).map_err(|_| NEG_EIO)?;
        if child_inode.is_dir() {
            return Err(NEG_EISDIR);
        }

        child_inode.links_count = child_inode.links_count.saturating_sub(1);
        self.remove_directory_entry(&parent_inode, name)?;

        if child_inode.links_count != 0 {
            self.write_inode(child_ino, &child_inode)
                .map_err(|_| NEG_EIO)?;
            return Ok(());
        }
        self.truncate_file(child_ino, &mut child_inode)?;
        self.free_inode(child_ino)?;
        Ok(())
    }

    /// Remove an empty directory.
    fn delete_directory(&mut self, parent_inode_num: u32, name: &str) -> Result<(), u64> {
        let parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        let child_ino = self.lookup_in_directory(&parent_inode, name)?;
        let mut child_inode = self.read_inode(child_ino).map_err(|_| NEG_EIO)?;
        if !child_inode.is_dir() {
            return Err(NEG_ENOTDIR);
        }

        let entries = self.read_dir_entries(&child_inode)?;
        for (_, entry_name, _) in &entries {
            if entry_name != "." && entry_name != ".." {
                return Err(NEG_ENOTEMPTY);
            }
        }

        self.truncate_file(child_ino, &mut child_inode)?;
        self.free_inode(child_ino)?;
        self.remove_directory_entry(&parent_inode, name)?;

        let mut parent_inode = self.read_inode(parent_inode_num).map_err(|_| NEG_EIO)?;
        parent_inode.links_count = parent_inode.links_count.saturating_sub(1);
        self.write_inode(parent_inode_num, &parent_inode)
            .map_err(|_| NEG_EIO)?;

        let group = inode_block_group(child_ino, self.superblock.inodes_per_group) as usize;
        if self.bgd_table[group].used_dirs_count > 0 {
            self.bgd_table[group].used_dirs_count -= 1;
        }
        self.mark_meta_dirty().map_err(|_| NEG_EIO)
    }
}

// ---------------------------------------------------------------------------
// Open handle table
// ---------------------------------------------------------------------------

/// Maximum concurrent open handles.
const MAX_HANDLES: usize = 32;
// Slot index occupies the low 16 bits of the packed handle. MAX_HANDLES
// (32) comfortably fits — the static check below guards against future
// bumps silently colliding with the generation field.
const _: () = assert!(MAX_HANDLES <= 0x1_0000);

/// Bits reserved for the slot index in the packed handle encoding.
const HANDLE_SLOT_BITS: u32 = 16;
/// Mask selecting the slot index from a packed handle.
const HANDLE_SLOT_MASK: u64 = (1 << HANDLE_SLOT_BITS) - 1;

/// An open handle tracked by the server.
struct OpenHandle {
    inode_num: u32,
    file_size: u32,
    /// Generation counter bumped on each (re-)allocation of this slot.
    /// Protects against force-closing a recycled handle when a stale
    /// `VFS_CLOSE` arrives out of order (defence-in-depth against
    /// kernel-side refcount races on SMP — the generation on an
    /// incoming request must match the slot's current generation, else
    /// the request is rejected as `EBADF`).
    generation: u16,
    in_use: bool,
}

struct HandleTable {
    handles: [OpenHandle; MAX_HANDLES],
}

impl HandleTable {
    fn new() -> Self {
        const EMPTY: OpenHandle = OpenHandle {
            inode_num: 0,
            file_size: 0,
            generation: 0,
            in_use: false,
        };
        HandleTable {
            handles: [EMPTY; MAX_HANDLES],
        }
    }

    fn alloc(&mut self, inode_num: u32, file_size: u32) -> Option<u64> {
        for (i, h) in self.handles.iter_mut().enumerate() {
            if !h.in_use {
                // Bump generation BEFORE marking in_use so a concurrent stale
                // request sees the new generation the moment the slot comes
                // back into circulation.
                h.generation = h.generation.wrapping_add(1);
                h.inode_num = inode_num;
                h.file_size = file_size;
                h.in_use = true;
                return Some(encode_handle(h.generation, i as u16));
            }
        }
        None
    }

    fn get(&self, handle: u64) -> Option<&OpenHandle> {
        let (generation, idx) = decode_handle(handle);
        let idx = idx as usize;
        if idx < MAX_HANDLES
            && self.handles[idx].in_use
            && self.handles[idx].generation == generation
        {
            Some(&self.handles[idx])
        } else {
            None
        }
    }

    fn free(&mut self, handle: u64) -> bool {
        let (generation, idx) = decode_handle(handle);
        let idx = idx as usize;
        if idx < MAX_HANDLES
            && self.handles[idx].in_use
            && self.handles[idx].generation == generation
        {
            self.handles[idx].in_use = false;
            true
        } else {
            false
        }
    }
}

/// Pack `(generation, slot)` into a `u64` handle.
fn encode_handle(generation: u16, slot: u16) -> u64 {
    ((generation as u64) << HANDLE_SLOT_BITS) | (slot as u64)
}

/// Unpack `(generation, slot)` from a `u64` handle. The kernel stores the
/// handle as the low 32 bits of the packed VFS_OPEN reply, which leaves
/// 16 bits of generation + 16 bits of slot — plenty for `MAX_HANDLES = 32`.
fn decode_handle(handle: u64) -> (u16, u16) {
    let generation = ((handle >> HANDLE_SLOT_BITS) & 0xFFFF) as u16;
    let slot = (handle & HANDLE_SLOT_MASK) as u16;
    (generation, slot)
}

// ---------------------------------------------------------------------------
// IPC constants
// ---------------------------------------------------------------------------

// Phase 87 — the per-request receive buffer holds the largest request: a
// VFS_WRITE's path + data (up to VFS_MAX_PWRITE). Heap-allocated (see
// `server_loop`) because 64 KiB is too large for the serve-loop stack frame.
const MAX_BULK_BUF: usize = VFS_MAX_PWRITE;
const SLOW_REQUEST_USEC: u64 = 50_000;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: starting\n");

    // 1. Probe MBR for ext2 partition.
    let mut sector0 = [0u8; 512];
    if syscall_lib::block_read(0, 1, &mut sector0) < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: failed to read MBR\n");
        return 1;
    }

    let entries = match mbr::parse_mbr(&sector0) {
        Ok(e) => e,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: bad MBR signature\n");
            return 1;
        }
    };

    let (base_lba, _sector_count) = match mbr::find_ext2_partition(&entries) {
        Some(v) => v,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: no ext2 partition found\n");
            return 1;
        }
    };

    // 2. Read superblock (offset 1024 = LBA + 2).
    let sb_lba = base_lba + 2;
    let mut sb_raw = [0u8; 1024];
    if syscall_lib::block_read(sb_lba, 2, &mut sb_raw) < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: failed to read superblock\n");
        return 1;
    }

    let superblock = match Ext2Superblock::parse(&sb_raw) {
        Ok(sb) => sb,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: bad ext2 superblock\n");
            return 1;
        }
    };

    let block_size = superblock.block_size();
    let sectors_per_block = block_size / 512;
    let bg_count = superblock.block_group_count();

    // 3. Read block group descriptor table.
    let bgd_block = if block_size == 1024 { 2 } else { 1 };
    let bgd_lba = base_lba + (bgd_block as u64) * (sectors_per_block as u64);
    let bgd_size = (bg_count as usize) * 32;
    let bgd_sectors = bgd_size.div_ceil(512);
    let mut bgd_raw = vec![0u8; bgd_sectors * 512];
    if syscall_lib::block_read(bgd_lba, bgd_sectors, &mut bgd_raw) < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: failed to read BGD table\n");
        return 1;
    }

    let bgd_table = match Ext2BlockGroupDescriptor::parse_table(&bgd_raw, bg_count) {
        Ok(t) => t,
        Err(_) => {
            syscall_lib::write_str(STDOUT_FILENO, "vfs_server: bad BGD table\n");
            return 1;
        }
    };

    let ext2 = Ext2State {
        base_lba,
        superblock,
        bgd_table,
        block_size,
        sectors_per_block,
        block_cache: RefCell::new(BTreeMap::new()),
        meta_dirty_ops: 0,
    };

    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: ext2 mounted\n");

    // 4. Create IPC endpoint and register as "vfs".
    let ep_handle = syscall_lib::create_endpoint();
    if ep_handle == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: create_endpoint failed\n");
        return 1;
    }
    let ep_handle = ep_handle as u32;

    let ret = syscall_lib::ipc_register_service(ep_handle, "vfs");
    if ret == u64::MAX {
        syscall_lib::write_str(STDOUT_FILENO, "vfs_server: register_service failed\n");
        return 1;
    }

    // Do not write to stdout after publishing the service name: clients may
    // immediately send IPC and block until this server reaches ipc_recv_msg.
    syscall_lib::serial_print("vfs_server: registered, entering server loop\n");

    // 5. Server loop.
    let mut ext2 = ext2;
    server_loop(&mut ext2, ep_handle);
}

// ---------------------------------------------------------------------------
// Server loop
// ---------------------------------------------------------------------------

fn server_loop(ext2: &mut Ext2State, ep_handle: u32) -> ! {
    let mut handles = HandleTable::new();
    let mut msg = syscall_lib::IpcMessage::new(0);
    // Phase 87 — heap-allocated (64 KiB) so a large VFS_WRITE's path+data fits
    // without blowing the serve-loop stack frame.
    let mut recv_buf = vec![0u8; MAX_BULK_BUF];
    let mut req_seq: u64 = 0;

    // First receive — blocks until the kernel sends us a request.
    syscall_lib::ipc_recv_msg(ep_handle, &mut msg, &mut recv_buf);

    loop {
        req_seq = req_seq.wrapping_add(1);
        let start_us = now_usec();
        let (reply_label, reply_data0) = handle_request(ext2, &mut handles, &msg, &recv_buf);
        let elapsed_us = now_usec().saturating_sub(start_us);
        // H9 follow-up: per-request start/done logging was investigative
        // only and added ~24 syscalls per request via write_str chains.
        // Under the security-floor regression (which spawns ion via
        // /bin/su user), that overhead saturated init's status-write
        // cadence and starved ion's first-config read for >30s. Log
        // ONLY slow requests now.
        log_request_done(req_seq, &msg, reply_label, reply_data0, elapsed_us);

        // Store reply bulk data if any was prepared by handle_request.
        // (read path stores data via ipc_store_reply_bulk before we get here)

        // Reply to the caller and wait for the next message.
        // We use two separate syscalls (reply + recv) because we need to
        // send data words in the reply (not just a label), and the
        // combined reply_recv_msg only supports a label.
        if let Some(reply_cap) = msg.reply_cap_handle() {
            if syscall_lib::ipc_reply(reply_cap, reply_label, reply_data0) == u64::MAX {
                syscall_lib::serial_print("vfs_server: ipc_reply failed\n");
            }
        } else {
            syscall_lib::serial_print("vfs_server: request missing reply cap\n");
        }

        msg = syscall_lib::IpcMessage::new(0);
        syscall_lib::ipc_recv_msg(ep_handle, &mut msg, &mut recv_buf);
    }
}

fn now_usec() -> u64 {
    let (sec, usec) = syscall_lib::gettimeofday();
    if sec < 0 || usec < 0 {
        0
    } else {
        (sec as u64)
            .saturating_mul(1_000_000)
            .saturating_add(usec as u64)
    }
}

fn request_name(label: u64) -> &'static str {
    match label {
        VFS_OPEN => "OPEN",
        VFS_READ => "READ",
        VFS_CLOSE => "CLOSE",
        VFS_STAT_PATH => "STAT_PATH",
        VFS_LIST_DIR => "LIST_DIR",
        VFS_ACCESS_PATH => "ACCESS_PATH",
        VFS_MOUNT_POLICY => "MOUNT_POLICY",
        VFS_UMOUNT_POLICY => "UMOUNT_POLICY",
        VFS_PREAD => "PREAD",
        VFS_WRITE => "WRITE",
        VFS_TRUNCATE => "TRUNCATE",
        VFS_CREATE => "CREATE",
        VFS_UNLINK => "UNLINK",
        VFS_RENAME => "RENAME",
        VFS_LINK => "LINK",
        _ => "UNKNOWN",
    }
}

fn log_request_done(
    seq: u64,
    msg: &syscall_lib::IpcMessage,
    _reply_label: u64,
    _reply_data0: u64,
    elapsed_us: u64,
) {
    if elapsed_us < SLOW_REQUEST_USEC {
        return;
    }
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: slow req#");
    syscall_lib::write_u64(STDOUT_FILENO, seq);
    syscall_lib::write_str(STDOUT_FILENO, " ");
    syscall_lib::write_str(STDOUT_FILENO, request_name(msg.label));
    syscall_lib::write_str(STDOUT_FILENO, " elapsed_us=");
    syscall_lib::write_u64(STDOUT_FILENO, elapsed_us);
    syscall_lib::write_str(STDOUT_FILENO, "\n");
}

/// Dispatch a single request.  Returns `(reply_label, reply_data0)`.
fn handle_request(
    ext2: &mut Ext2State,
    handles: &mut HandleTable,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    match msg.label {
        VFS_OPEN => handle_open(ext2, handles, msg, recv_buf),
        VFS_READ => handle_read(ext2, handles, msg),
        VFS_CLOSE => handle_close(handles, msg),
        VFS_STAT_PATH => handle_stat_path(ext2, msg, recv_buf),
        VFS_LIST_DIR => handle_list_dir(ext2, msg, recv_buf),
        VFS_ACCESS_PATH => handle_access_path(ext2, msg, recv_buf),
        VFS_MOUNT_POLICY => handle_mount_policy(msg, recv_buf),
        VFS_UMOUNT_POLICY => handle_umount_policy(msg, recv_buf),
        // Phase 88 — ext2 write authority.
        VFS_PREAD => handle_pread(ext2, msg, recv_buf),
        VFS_WRITE => handle_write(ext2, msg, recv_buf),
        VFS_TRUNCATE => handle_truncate(ext2, msg, recv_buf),
        VFS_CREATE => handle_create(ext2, msg, recv_buf),
        VFS_UNLINK => handle_unlink(ext2, msg, recv_buf),
        VFS_RENAME => handle_rename(ext2, msg, recv_buf),
        VFS_LINK => handle_link(ext2, msg, recv_buf),
        _ => (NEG_EINVAL, 0),
    }
}

fn decode_path<'a>(recv_buf: &'a [u8], path_len: usize) -> Result<&'a str, u64> {
    if path_len == 0 || path_len > recv_buf.len() {
        return Err(NEG_EINVAL);
    }
    core::str::from_utf8(&recv_buf[..path_len]).map_err(|_| NEG_EINVAL)
}

fn inode_kind(inode: &Ext2Inode) -> Result<u64, u64> {
    if inode.is_regular() {
        Ok(VFS_NODE_FILE)
    } else if inode.is_dir() {
        Ok(VFS_NODE_DIR)
    } else if inode.is_symlink() {
        Ok(VFS_NODE_SYMLINK)
    } else {
        Err(NEG_EINVAL)
    }
}

fn inode_kind_to_dirent_type(inode: &Ext2Inode) -> u8 {
    if inode.is_dir() {
        4
    } else if inode.is_regular() {
        8
    } else if inode.is_symlink() {
        10
    } else {
        0
    }
}

fn encode_stat_header(
    ext2: &Ext2State,
    inode_num: u32,
    inode: &Ext2Inode,
) -> Result<[u8; VFS_STAT_REPLY_SIZE], u64> {
    let kind = inode_kind(inode)?;
    let words = [
        kind,
        inode.mode as u64,
        inode.uid as u64,
        inode.gid as u64,
        inode_num as u64,
        inode.size as u64,
        inode.links_count as u64,
        ext2.block_size as u64,
        inode.atime as u64,
        inode.mtime as u64,
        inode.ctime as u64,
    ];
    let mut out = [0u8; VFS_STAT_REPLY_SIZE];
    for (idx, word) in words.iter().enumerate() {
        let start = idx * 8;
        out[start..start + 8].copy_from_slice(&word.to_le_bytes());
    }
    Ok(out)
}

fn mount_policy_action(target: &str, fstype: &str) -> Result<u64, u64> {
    match (target, fstype) {
        ("/", "ext2") => Ok(VFS_MOUNT_EXT2_ROOT),
        ("/data", "vfat") => Ok(VFS_MOUNT_VFAT_DATA),
        _ => Err(NEG_EINVAL),
    }
}

fn umount_policy_action(target: &str) -> Result<u64, u64> {
    match target {
        "/" => Ok(VFS_UMOUNT_EXT2_ROOT),
        "/data" => Ok(VFS_UMOUNT_VFAT_DATA),
        _ => Err(NEG_EINVAL),
    }
}

// ---------------------------------------------------------------------------
// VFS_OPEN
// ---------------------------------------------------------------------------

fn handle_open(
    ext2: &Ext2State,
    handles: &mut HandleTable,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    // Defensive flag validation. The kernel's `vfs_service_should_route`
    // already gates this path on read-only, non-creating, non-truncating
    // opens — but the server owns its own contract. Reject anything with
    // an access mode other than O_RDONLY, or with creation / truncation /
    // exclusive bits set, so a future kernel change or a misbehaving
    // caller surfaces a clear EINVAL instead of a silent success.
    const O_ACCMODE: u64 = 0o3;
    // O_CREAT=0x40, O_EXCL=0x80, O_TRUNC=0x200, O_APPEND=0x400.
    const MUTATING_FLAGS: u64 = 0x40 | 0x80 | 0x200 | 0x400;
    let flags = msg.data[0];
    if flags & O_ACCMODE != 0 || flags & MUTATING_FLAGS != 0 {
        return (NEG_EINVAL, 0);
    }

    let path = match decode_path(recv_buf, msg.data[1] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };

    // Resolve path to inode.
    let inode_num = match ext2.resolve_path(path) {
        Ok(n) => n,
        Err(errno) => return (errno, 0),
    };

    // Read the inode to verify it's a regular file and get file size.
    let inode = match ext2.read_inode(inode_num) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };

    if !inode.is_regular() {
        // Only regular files for this slice.
        return (NEG_EINVAL, 0);
    }

    let file_size = inode.size;

    // Allocate a handle.
    let handle = match handles.alloc(inode_num, file_size) {
        Some(h) => h,
        None => return (NEG_ENFILE, 0),
    };

    // Reply: label=0, data[0] packs the handle in the low 32 bits and the
    // file size (clamped to u32::MAX) in the high 32 bits. The kernel
    // unpacks both fields to seed its FdBackend::VfsService entry — see
    // kernel_core::fs::vfs_protocol::VFS_OPEN for the canonical contract.
    let packed = handle | ((file_size as u64) << 32);
    (0, packed)
}

// ---------------------------------------------------------------------------
// VFS_READ
// ---------------------------------------------------------------------------

fn handle_read(
    ext2: &Ext2State,
    handles: &HandleTable,
    msg: &syscall_lib::IpcMessage,
) -> (u64, u64) {
    let handle_id = msg.data[0];
    let offset = msg.data[1] as usize;
    // Phase 87 — see handle_pread: reads serve up to VFS_MAX_PREAD via reply bulk.
    let max_bytes = (msg.data[2] as usize).min(VFS_MAX_PREAD);

    let handle = match handles.get(handle_id) {
        Some(h) => h,
        None => return (NEG_EBADF, 0),
    };

    let inode = match ext2.read_inode(handle.inode_num) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };

    let data = match ext2.read_file_data(&inode, offset, max_bytes) {
        Ok(d) => d,
        Err(_) => return (NEG_EIO, 0),
    };

    let bytes_read = data.len();

    // Store read data as reply bulk. Propagate store failure as EIO so the
    // kernel doesn't see a "success + missing bulk" response.
    if bytes_read > 0 && syscall_lib::ipc_store_reply_bulk(&data) != 0 {
        return (NEG_EIO, 0);
    }

    (0, bytes_read as u64)
}

// ---------------------------------------------------------------------------
// VFS_CLOSE
// ---------------------------------------------------------------------------

fn handle_close(handles: &mut HandleTable, msg: &syscall_lib::IpcMessage) -> (u64, u64) {
    let handle_id = msg.data[0];
    if handles.free(handle_id) {
        (0, 0)
    } else {
        // Stale or unknown handle — reject cleanly so a racing refcount bug
        // on the kernel side cannot force-close a recycled slot.
        (NEG_EBADF, 0)
    }
}

fn handle_stat_path(
    ext2: &Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let path = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    let inode_num = match ext2.resolve_path(path) {
        Ok(n) => n,
        Err(errno) => return (errno, 0),
    };
    let inode = match ext2.read_inode(inode_num) {
        Ok(inode) => inode,
        Err(_) => return (NEG_EIO, 0),
    };
    let mut stat = match encode_stat_header(ext2, inode_num, &inode) {
        Ok(stat) => stat.to_vec(),
        Err(errno) => return (errno, 0),
    };
    if inode.is_symlink() {
        let target = match ext2.read_symlink_target(&inode) {
            Ok(target) => target,
            Err(_) => return (NEG_EIO, 0),
        };
        stat.extend_from_slice(&target);
    }
    if syscall_lib::ipc_store_reply_bulk(&stat) != 0 {
        return (NEG_EIO, 0);
    }
    (0, 0)
}

fn handle_list_dir(ext2: &Ext2State, msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let path = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    let offset = msg.data[1] as usize;
    let max_bytes = (msg.data[2] as usize).min(MAX_BULK_BUF);

    let inode_num = match ext2.resolve_path(path) {
        Ok(n) => n,
        Err(errno) => return (errno, 0),
    };
    let inode = match ext2.read_inode(inode_num) {
        Ok(inode) => inode,
        Err(_) => return (NEG_EIO, 0),
    };
    if !inode.is_dir() {
        return (NEG_ENOTDIR, 0);
    }

    let entries = match ext2.read_dir_entries(&inode) {
        Ok(entries) => entries,
        Err(errno) => return (errno, 0),
    };

    let mut out = Vec::new();
    let mut idx = offset;
    while idx < entries.len() {
        let (inode_num, name, d_type) = &entries[idx];
        let name_bytes = name.as_bytes();
        let reclen = (19 + name_bytes.len() + 1 + 7) & !7;
        if out.len() + reclen > max_bytes {
            if out.is_empty() {
                return (NEG_EINVAL, 0);
            }
            break;
        }
        let start = out.len();
        out.resize(start + reclen, 0);
        let d_ino = *inode_num as u64;
        let d_off = (idx + 1) as i64;
        out[start..start + 8].copy_from_slice(&d_ino.to_ne_bytes());
        out[start + 8..start + 16].copy_from_slice(&d_off.to_ne_bytes());
        out[start + 16..start + 18].copy_from_slice(&(reclen as u16).to_ne_bytes());
        out[start + 18] = *d_type;
        out[start + 19..start + 19 + name_bytes.len()].copy_from_slice(name_bytes);
        idx += 1;
    }

    if !out.is_empty() && syscall_lib::ipc_store_reply_bulk(&out) != 0 {
        return (NEG_EIO, 0);
    }
    let packed = (out.len() as u64) | ((idx as u64) << 32);
    (0, packed)
}

fn handle_access_path(
    ext2: &Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let path = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    match ext2.resolve_path(path) {
        Ok(_) => (0, 0),
        Err(errno) => (errno, 0),
    }
}

fn handle_mount_policy(msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let target_len = msg.data[0] as usize;
    let fstype_len = msg.data[1] as usize;
    if target_len == 0 || fstype_len == 0 || target_len + fstype_len > recv_buf.len() {
        return (NEG_EINVAL, 0);
    }
    let target = match core::str::from_utf8(&recv_buf[..target_len]) {
        Ok(target) => target,
        Err(_) => return (NEG_EINVAL, 0),
    };
    let fstype = match core::str::from_utf8(&recv_buf[target_len..target_len + fstype_len]) {
        Ok(fstype) => fstype,
        Err(_) => return (NEG_EINVAL, 0),
    };
    match mount_policy_action(target, fstype) {
        Ok(action) => (0, action),
        Err(errno) => (errno, 0),
    }
}

fn handle_umount_policy(msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let target = match decode_path(recv_buf, msg.data[0] as usize) {
        Ok(path) => path,
        Err(errno) => return (errno, 0),
    };
    match umount_policy_action(target) {
        Ok(action) => (0, action),
        Err(errno) => (errno, 0),
    }
}

// ---------------------------------------------------------------------------
// Phase 88 — write handlers
// ---------------------------------------------------------------------------

/// Decode a UTF-8 string from `recv_buf[start..start + len]`.
fn decode_str(recv_buf: &[u8], start: usize, len: usize) -> Result<&str, u64> {
    let end = start.checked_add(len).ok_or(NEG_EINVAL)?;
    if len == 0 || end > recv_buf.len() {
        return Err(NEG_EINVAL);
    }
    core::str::from_utf8(&recv_buf[start..end]).map_err(|_| NEG_EINVAL)
}

/// VFS_PREAD — read file data by path at an offset (coherent read-back for the
/// kernel's writable `Ext2Disk` fds).
fn handle_pread(ext2: &Ext2State, msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let path_len = msg.data[0] as usize;
    let offset = msg.data[1] as usize;
    // Phase 87 — reads are served in up to VFS_MAX_PREAD (64 KiB) chunks via the
    // unbounded reply bulk, NOT bounded by the small request buffer (MAX_BULK_BUF).
    let max_bytes = (msg.data[2] as usize).min(VFS_MAX_PREAD);

    let path = match decode_str(recv_buf, 0, path_len) {
        Ok(p) => p,
        Err(e) => return (e, 0),
    };
    let inode_num = match ext2.resolve_path(path) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };
    let inode = match ext2.read_inode(inode_num) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };
    let data = match ext2.read_file_data(&inode, offset, max_bytes) {
        Ok(d) => d,
        Err(_) => return (NEG_EIO, 0),
    };
    let bytes_read = data.len();
    if bytes_read > 0 && syscall_lib::ipc_store_reply_bulk(&data) != 0 {
        return (NEG_EIO, 0);
    }
    (0, bytes_read as u64)
}

/// VFS_WRITE — write file data by path at an offset.
fn handle_write(
    ext2: &mut Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let path_len = msg.data[0] as usize;
    let offset = msg.data[1];
    let data_len = msg.data[2] as usize;

    let path = match decode_str(recv_buf, 0, path_len) {
        Ok(p) => p,
        Err(e) => return (e, 0),
    };
    let data_end = match path_len.checked_add(data_len) {
        Some(e) if e <= recv_buf.len() => e,
        _ => return (NEG_EINVAL, 0),
    };
    // Copy the path out so the borrow on recv_buf is released before we touch
    // the (also-borrowed) data slice through `&mut ext2`.
    let path_owned = String::from(path);
    let data = recv_buf[path_len..data_end].to_vec();

    let inode_num = match ext2.resolve_path(&path_owned) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };
    let mut inode = match ext2.read_inode(inode_num) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };
    // Refresh write timestamps (mirrors the kernel write path).
    let now = now_unix_secs();
    inode.mtime = now;
    inode.ctime = now;
    match ext2.write_file_data(inode_num, &mut inode, offset, &data) {
        Ok(n) => {
            let new_size = inode.size as u64;
            (0, (n as u64 & 0xFFFF_FFFF) | (new_size << 32))
        }
        Err(e) => (e, 0),
    }
}

/// VFS_TRUNCATE — truncate a file by path to a length (only 0 is supported for
/// shrink; non-zero lengths grow via subsequent writes / sparse fill).
fn handle_truncate(
    ext2: &mut Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let path_len = msg.data[0] as usize;
    let new_len = msg.data[1];
    let path = match decode_str(recv_buf, 0, path_len) {
        Ok(p) => String::from(p),
        Err(e) => return (e, 0),
    };
    let inode_num = match ext2.resolve_path(&path) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };
    let mut inode = match ext2.read_inode(inode_num) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };
    if inode.is_dir() {
        return (NEG_EISDIR, 0);
    }
    // Free all data blocks, then re-establish the requested logical size. The
    // kernel only ever issues truncate-to-0 (O_TRUNC) and ftruncate; for a
    // non-zero shrink we conservatively free everything then set the size so a
    // later read returns sparse zeros (matches the kernel engine, which also
    // only frees-all on truncate).
    if let Err(e) = ext2.truncate_file(inode_num, &mut inode) {
        return (e, 0);
    }
    if new_len != 0 {
        inode.size = new_len.min(u32::MAX as u64) as u32;
        if ext2.write_inode(inode_num, &inode).is_err() {
            return (NEG_EIO, 0);
        }
    }
    (0, 0)
}

/// VFS_CREATE — create a regular file, directory, or symlink.
fn handle_create(
    ext2: &mut Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let parent_len = (msg.data[0] & 0xFFFF_FFFF) as usize;
    let name_len = (msg.data[0] >> 32) as usize;
    let mode = (msg.data[1] & 0xFFFF) as u16;
    let kind = (msg.data[1] >> VFS_CREATE_KIND_SHIFT) & 0x3;
    let target_len = (msg.data[1] >> 32) as usize;
    let uid = msg.data[2] as u32;
    let gid = msg.data[3] as u32;

    let parent = match decode_str(recv_buf, 0, parent_len) {
        Ok(p) => String::from(p),
        Err(e) => return (e, 0),
    };
    let name = match decode_str(recv_buf, parent_len, name_len) {
        Ok(n) => String::from(n),
        Err(e) => return (e, 0),
    };

    let parent_ino = match ext2.resolve_path(&parent) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };

    let result = match kind {
        k if k == VFS_NODE_FILE => ext2.create_file(parent_ino, &name, mode, uid, gid),
        k if k == VFS_NODE_DIR => ext2.create_directory(parent_ino, &name, mode, uid, gid),
        k if k == VFS_NODE_SYMLINK => {
            let target = match decode_str(recv_buf, parent_len + name_len, target_len) {
                Ok(t) => String::from(t),
                Err(e) => return (e, 0),
            };
            ext2.create_symlink(parent_ino, &name, &target, uid, gid)
        }
        _ => return (NEG_EINVAL, 0),
    };
    match result {
        Ok(new_ino) => (0, new_ino as u64),
        Err(e) => (e, 0),
    }
}

/// VFS_UNLINK — remove a file (`is_dir == 0`) or empty directory (`is_dir == 1`).
fn handle_unlink(
    ext2: &mut Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let parent_len = msg.data[0] as usize;
    let name_len = msg.data[1] as usize;
    let want_dir = msg.data[2] != 0;

    let parent = match decode_str(recv_buf, 0, parent_len) {
        Ok(p) => String::from(p),
        Err(e) => return (e, 0),
    };
    let name = match decode_str(recv_buf, parent_len, name_len) {
        Ok(n) => String::from(n),
        Err(e) => return (e, 0),
    };
    let parent_ino = match ext2.resolve_path(&parent) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };
    let result = if want_dir {
        ext2.delete_directory(parent_ino, &name)
    } else {
        ext2.delete_file(parent_ino, &name)
    };
    match result {
        Ok(()) => (0, 0),
        Err(e) => (e, 0),
    }
}

/// VFS_RENAME — move an entry from `old` to `new` (non-directory or directory).
fn handle_rename(
    ext2: &mut Ext2State,
    msg: &syscall_lib::IpcMessage,
    recv_buf: &[u8],
) -> (u64, u64) {
    let old_len = msg.data[0] as usize;
    let new_len = msg.data[1] as usize;

    let old_path = match decode_str(recv_buf, 0, old_len) {
        Ok(p) => String::from(p),
        Err(e) => return (e, 0),
    };
    let new_path = match decode_str(recv_buf, old_len, new_len) {
        Ok(p) => String::from(p),
        Err(e) => return (e, 0),
    };

    let (old_parent, old_name) = match ext2.resolve_parent_and_name(&old_path) {
        Ok(v) => v,
        Err(e) => return (e, 0),
    };
    let old_parent_inode = match ext2.read_inode(old_parent) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };
    let src_ino = match ext2.lookup_in_directory(&old_parent_inode, old_name) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };
    let src_inode = match ext2.read_inode(src_ino) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };
    let src_is_dir = src_inode.is_dir();
    let src_is_symlink = src_inode.is_symlink();
    let old_name_owned = String::from(old_name);

    let (new_parent, new_name) = match ext2.resolve_parent_and_name(&new_path) {
        Ok(v) => v,
        Err(e) => return (e, 0),
    };
    let new_name_owned = String::from(new_name);

    // If the destination already exists, remove it first (POSIX rename
    // semantics: overwrite an existing plain destination).
    if let Ok(new_parent_inode) = ext2.read_inode(new_parent)
        && let Ok(existing) = ext2.lookup_in_directory(&new_parent_inode, &new_name_owned)
    {
        let existing_inode = match ext2.read_inode(existing) {
            Ok(i) => i,
            Err(_) => return (NEG_EIO, 0),
        };
        let remove = if existing_inode.is_dir() {
            ext2.delete_directory(new_parent, &new_name_owned)
        } else {
            ext2.delete_file(new_parent, &new_name_owned)
        };
        if let Err(e) = remove {
            return (e, 0);
        }
    }

    // Link the source inode under the new name, then unlink the old entry.
    let file_type = if src_is_dir {
        EXT2_FT_DIR
    } else if src_is_symlink {
        EXT2_FT_SYMLINK
    } else {
        EXT2_FT_REG_FILE
    };
    let mut new_parent_inode = match ext2.read_inode(new_parent) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };
    if let Err(e) = ext2.add_directory_entry(
        new_parent,
        &mut new_parent_inode,
        &new_name_owned,
        src_ino,
        file_type,
    ) {
        return (e, 0);
    }

    // For directory moves across parents, fix the source's ".." and the link
    // counts. Same-parent renames need no link-count change.
    if src_is_dir
        && new_parent != old_parent
        && let Ok(mut np) = ext2.read_inode(new_parent)
    {
        np.links_count = np.links_count.saturating_add(1);
        let _ = ext2.write_inode(new_parent, &np);
        // Repoint the moved directory's ".." at its new parent so
        // /new_parent/dir/.. no longer resolves to the old parent.
        let _ = ext2.update_dotdot(&src_inode, new_parent);
    }

    // Remove the old directory entry (do NOT reclaim the inode — it now lives
    // under the new name).
    let old_parent_inode = match ext2.read_inode(old_parent) {
        Ok(i) => i,
        Err(_) => return (NEG_EIO, 0),
    };
    if let Err(e) = ext2.remove_directory_entry(&old_parent_inode, &old_name_owned) {
        return (e, 0);
    }
    if src_is_dir
        && new_parent != old_parent
        && let Ok(mut op) = ext2.read_inode(old_parent)
    {
        op.links_count = op.links_count.saturating_sub(1);
        let _ = ext2.write_inode(old_parent, &op);
    }
    (0, 0)
}

/// VFS_LINK — hard-link a new name to an existing target inode.
fn handle_link(ext2: &mut Ext2State, msg: &syscall_lib::IpcMessage, recv_buf: &[u8]) -> (u64, u64) {
    let target_len = msg.data[0] as usize;
    let parent_len = msg.data[1] as usize;
    let name_len = msg.data[2] as usize;

    let target_path = match decode_str(recv_buf, 0, target_len) {
        Ok(p) => String::from(p),
        Err(e) => return (e, 0),
    };
    let parent_path = match decode_str(recv_buf, target_len, parent_len) {
        Ok(p) => String::from(p),
        Err(e) => return (e, 0),
    };
    let name = match decode_str(recv_buf, target_len + parent_len, name_len) {
        Ok(n) => String::from(n),
        Err(e) => return (e, 0),
    };

    let target_ino = match ext2.resolve_path(&target_path) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };
    let parent_ino = match ext2.resolve_path(&parent_path) {
        Ok(n) => n,
        Err(e) => return (e, 0),
    };
    match ext2.create_hard_link(parent_ino, &name, target_ino) {
        Ok(()) => (0, 0),
        Err(e) => (e, 0),
    }
}

/// Current wall-clock seconds (best-effort; 0 if unavailable).
fn now_unix_secs() -> u32 {
    let (sec, _usec) = syscall_lib::gettimeofday();
    if sec < 0 { 0 } else { sec as u32 }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "vfs_server: PANIC\n");
    syscall_lib::exit(101)
}
