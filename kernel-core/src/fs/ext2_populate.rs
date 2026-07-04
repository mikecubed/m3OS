//! Phase 106 C.4/C.5 — **populate** a freshly formatted ext2 volume from a
//! source filesystem tree, as pure host-testable logic.
//!
//! This is the file-level half of the partition-aware install: C.5's
//! [`format_ext2`](super::ext2_format::format_ext2) lays down the blank
//! target, and [`populate_from_reader`] walks the *source* rootfs through the
//! existing [`BlockReader`] read path (the same one the kernel and
//! `vfs_server` mount with) and re-creates every directory, regular file, and
//! symlink on the target through the C.5 [`Ext2Fs`] writer — across differing
//! block sizes, preserving mode/uid/gid/timestamps.
//!
//! [`WriteBackBlockIo`] is the populate path's IO reducer: an LRU write-back
//! cache for the metadata blocks the `Ext2Fs` writer re-touches per file
//! (allocation bitmaps, inode-table blocks, directory blocks) plus a
//! contiguous-run coalescer for the write-once data-block stream. Without it
//! every created block costs ~3 device round-trips (bitmap read + bitmap
//! write + data write) — on the installer's IPC-routed raw syscalls that is
//! the difference between seconds and many minutes. Callers **must**
//! [`WriteBackBlockIo::flush`] before dropping the wrapper (a fresh install
//! target has no durability to lose mid-populate — an interrupted populate
//! means abort + reformat, same as any partial `Ext2Fs::create_*`).
//!
//! # What is deliberately NOT copied
//!
//! - `lost+found` (the target's own formatted one is kept),
//! - hard links (each directory entry is materialized as an independent
//!   file — content duplicated, `links_count = 1`; the m3OS rootfs builder
//!   creates none),
//! - device nodes / FIFOs / sockets (counted in
//!   [`PopulateStats::skipped`]).

use super::ext2::{
    BlockReader, EXT2_ROOT_INO, Ext2Error, read_directory_entries, read_file_data, read_inode,
    read_symlink_target,
};
use super::ext2_format::{BlockIo, Ext2Fs};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;

/// Counters from one populate run (the installer's
/// `INSTALLER:populate` sentinel payload).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PopulateStats {
    pub dirs: u32,
    pub files: u32,
    pub symlinks: u32,
    /// Total regular-file payload bytes copied.
    pub bytes: u64,
    /// Entries not copied: unsupported types (device nodes, FIFOs) and
    /// directory entries that would revisit an already-copied directory
    /// (a corrupt source's cycle — fail-safe, not fail-stop).
    pub skipped: u32,
}

/// Copy `uid`/`gid`/timestamps from a source inode onto a freshly created
/// target inode (mode was already set at creation).
fn copy_owner<IO: BlockIo + ?Sized>(
    fs: &mut Ext2Fs,
    io: &mut IO,
    ino: u32,
    src: &super::ext2::Ext2Inode,
) -> Result<(), Ext2Error> {
    let mut inode = fs.read_inode(io, ino)?;
    inode.uid = src.uid;
    inode.gid = src.gid;
    inode.atime = src.atime;
    inode.ctime = src.ctime;
    inode.mtime = src.mtime;
    fs.write_inode(io, ino, &inode)
}

/// Walk the source tree from its root and re-create it on `fs`.
///
/// Iterative (explicit stack) so a deep tree cannot exhaust the call stack; a
/// `visited` set of source directory inodes makes a corrupt source's
/// directory cycle terminate (revisits are counted as skipped, never
/// re-descended). The target root inherits the source root's mode/owner/
/// timestamps.
pub fn populate_from_reader<R: BlockReader + ?Sized, IO: BlockIo + ?Sized>(
    src: &R,
    fs: &mut Ext2Fs,
    io: &mut IO,
) -> Result<PopulateStats, Ext2Error> {
    let mut stats = PopulateStats::default();

    // Root fidelity: mode bits + owner + times.
    let src_root = read_inode(src, EXT2_ROOT_INO)?;
    let mut dst_root = fs.read_inode(io, EXT2_ROOT_INO)?;
    dst_root.mode = src_root.mode;
    dst_root.uid = src_root.uid;
    dst_root.gid = src_root.gid;
    dst_root.atime = src_root.atime;
    dst_root.ctime = src_root.ctime;
    dst_root.mtime = src_root.mtime;
    fs.write_inode(io, EXT2_ROOT_INO, &dst_root)?;

    let mut visited: BTreeSet<u32> = BTreeSet::new();
    visited.insert(EXT2_ROOT_INO);
    let mut stack: Vec<(u32, u32)> = vec![(EXT2_ROOT_INO, EXT2_ROOT_INO)];

    while let Some((src_dir, dst_dir)) = stack.pop() {
        let dir_inode = read_inode(src, src_dir)?;
        for (name, ino, _ft) in read_directory_entries(src, &dir_inode)? {
            if name == "." || name == ".." {
                continue;
            }
            if src_dir == EXT2_ROOT_INO && name == "lost+found" {
                continue; // the formatted target already has its own
            }
            let inode = read_inode(src, ino)?;
            if inode.is_dir() {
                if !visited.insert(ino) {
                    // A second path to an already-copied directory: only a
                    // corrupt source produces this — skip rather than loop.
                    stats.skipped += 1;
                    continue;
                }
                let new = fs.create_dir(io, dst_dir, &name, inode.permission_mode())?;
                copy_owner(fs, io, new, &inode)?;
                stats.dirs += 1;
                stack.push((ino, new));
            } else if inode.is_regular() {
                let mut data = vec![0u8; inode.size as usize];
                let n = read_file_data(src, &inode, 0, &mut data)?;
                data.truncate(n);
                let new = fs.create_file(io, dst_dir, &name, &data, inode.permission_mode())?;
                copy_owner(fs, io, new, &inode)?;
                stats.files += 1;
                stats.bytes += n as u64;
            } else if inode.is_symlink() {
                let target = read_symlink_target(src, &inode)?;
                let target_str =
                    core::str::from_utf8(&target).map_err(|_| Ext2Error::CorruptedEntry)?;
                let new = fs.create_symlink(io, dst_dir, &name, target_str)?;
                copy_owner(fs, io, new, &inode)?;
                stats.symlinks += 1;
            } else {
                stats.skipped += 1;
            }
        }
    }

    Ok(stats)
}

// ---------------------------------------------------------------------------
// Write-back cache + run coalescer
// ---------------------------------------------------------------------------

struct CacheEntry {
    data: Vec<u8>,
    dirty: bool,
    tick: u64,
}

/// Device-op counters (observable by tests and the installer's sentinels).
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteBackStats {
    /// Single-block reads that reached the inner device.
    pub inner_reads: u64,
    /// Write *requests* that reached the inner device (a coalesced run
    /// counts once).
    pub inner_write_ops: u64,
    /// Total blocks those write requests carried.
    pub inner_blocks_written: u64,
}

/// A bounded write-back block cache over any [`BlockIo`].
///
/// Two mechanisms, one per traffic class:
///
/// - **LRU write-back map** for blocks that are *read first* (every
///   metadata block: the `Ext2Fs` writer's bitmap/inode-table/directory
///   read-modify-write cycles). Dirty entries stay in the cache until
///   [`flush`](Self::flush) or LRU eviction, so N rewrites of a hot bitmap
///   block cost one device write instead of N.
/// - **Contiguous-run coalescer** for blind writes (write-once data blocks,
///   which the allocator hands out mostly sequentially): adjacent writes
///   accumulate into one buffer and leave as a single
///   [`BlockIo::write_block_run`] request of up to `max_run_blocks`.
///
/// Invariant: a block is never in both the map and the pending run (reads
/// check the map, then the run, and only insert on a device fetch; writes to
/// a mapped block stay in the map). `flush()` drains the run first, then the
/// dirty map entries in ascending block order (itself run-coalesced).
///
/// The wrapper is transparent: any sequence of operations produces the same
/// final device image as issuing them directly (asserted byte-for-byte by
/// the equivalence test).
pub struct WriteBackBlockIo<'a, IO: BlockIo + ?Sized> {
    inner: &'a mut IO,
    block_size: usize,
    cap: usize,
    max_run_blocks: usize,
    map: BTreeMap<u32, CacheEntry>,
    tick: u64,
    run_start: u32,
    run_blocks: usize,
    run: Vec<u8>,
    pub stats: WriteBackStats,
}

impl<'a, IO: BlockIo + ?Sized> WriteBackBlockIo<'a, IO> {
    /// `cap` bounds the LRU map (blocks); `max_run_blocks` bounds the
    /// coalescing buffer. Both must be ≥ 1.
    pub fn new(inner: &'a mut IO, block_size: usize, cap: usize, max_run_blocks: usize) -> Self {
        WriteBackBlockIo {
            inner,
            block_size,
            cap: cap.max(1),
            max_run_blocks: max_run_blocks.max(1),
            map: BTreeMap::new(),
            tick: 0,
            run_start: 0,
            run_blocks: 0,
            run: Vec::new(),
            stats: WriteBackStats::default(),
        }
    }

    fn bump(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Drain the pending contiguous run as one device write.
    fn flush_run(&mut self) -> Result<(), Ext2Error> {
        if self.run_blocks == 0 {
            return Ok(());
        }
        let count = self.run_blocks as u32;
        self.inner.write_block_run(
            self.run_start,
            count,
            &self.run[..self.run_blocks * self.block_size],
        )?;
        self.stats.inner_write_ops += 1;
        self.stats.inner_blocks_written += count as u64;
        self.run_blocks = 0;
        self.run.clear();
        Ok(())
    }

    /// Evict the least-recently-used map entry (writing it back if dirty).
    fn evict_one(&mut self) -> Result<(), Ext2Error> {
        let Some((&victim, _)) = self.map.iter().min_by_key(|(_, e)| e.tick) else {
            return Ok(());
        };
        let entry = self.map.remove(&victim).expect("victim exists");
        if entry.dirty {
            self.inner.write_block(victim, &entry.data)?;
            self.stats.inner_write_ops += 1;
            self.stats.inner_blocks_written += 1;
        }
        Ok(())
    }

    /// Write every pending block (run + dirty map entries) to the device.
    /// Dirty map entries are drained in ascending block order, adjacent ones
    /// coalesced into runs. The cache stays warm (entries become clean).
    pub fn flush(&mut self) -> Result<(), Ext2Error> {
        self.flush_run()?;
        let dirty: Vec<u32> = self
            .map
            .iter()
            .filter(|(_, e)| e.dirty)
            .map(|(&b, _)| b)
            .collect();
        let mut i = 0;
        while i < dirty.len() {
            // Extend a contiguous ascending run (BTreeMap iteration is sorted).
            let start = dirty[i];
            let mut count = 1usize;
            while i + count < dirty.len()
                && dirty[i + count] == start + count as u32
                && count < self.max_run_blocks
            {
                count += 1;
            }
            let mut buf = vec![0u8; count * self.block_size];
            for (k, chunk) in buf.chunks_mut(self.block_size).enumerate() {
                chunk.copy_from_slice(&self.map[&(start + k as u32)].data);
            }
            self.inner.write_block_run(start, count as u32, &buf)?;
            self.stats.inner_write_ops += 1;
            self.stats.inner_blocks_written += count as u64;
            for k in 0..count {
                self.map
                    .get_mut(&(start + k as u32))
                    .expect("dirty entry exists")
                    .dirty = false;
            }
            i += count;
        }
        Ok(())
    }
}

impl<IO: BlockIo + ?Sized> BlockIo for WriteBackBlockIo<'_, IO> {
    fn read_block(&mut self, block: u32, buf: &mut [u8]) -> Result<(), Ext2Error> {
        let tick = self.bump();
        if let Some(e) = self.map.get_mut(&block) {
            e.tick = tick;
            buf.copy_from_slice(&e.data);
            return Ok(());
        }
        // Serve from the pending run (a just-written data/directory block).
        if self.run_blocks > 0
            && block >= self.run_start
            && block < self.run_start + self.run_blocks as u32
        {
            let off = (block - self.run_start) as usize * self.block_size;
            buf.copy_from_slice(&self.run[off..off + self.block_size]);
            return Ok(());
        }
        self.inner.read_block(block, buf)?;
        self.stats.inner_reads += 1;
        while self.map.len() >= self.cap {
            self.evict_one()?;
        }
        self.map.insert(
            block,
            CacheEntry {
                data: buf.to_vec(),
                dirty: false,
                tick,
            },
        );
        Ok(())
    }

    fn write_block(&mut self, block: u32, data: &[u8]) -> Result<(), Ext2Error> {
        let tick = self.bump();
        if let Some(e) = self.map.get_mut(&block) {
            e.data.copy_from_slice(data);
            e.dirty = true;
            e.tick = tick;
            return Ok(());
        }
        if self.run_blocks > 0 {
            // Overwrite inside the pending run (a directory block getting a
            // second entry before the run leaves).
            if block >= self.run_start && block < self.run_start + self.run_blocks as u32 {
                let off = (block - self.run_start) as usize * self.block_size;
                self.run[off..off + self.block_size].copy_from_slice(data);
                return Ok(());
            }
            // Extend the run.
            if block == self.run_start + self.run_blocks as u32
                && self.run_blocks < self.max_run_blocks
            {
                self.run.extend_from_slice(data);
                self.run_blocks += 1;
                return Ok(());
            }
            self.flush_run()?;
        }
        self.run_start = block;
        self.run.clear();
        self.run.extend_from_slice(data);
        self.run_blocks = 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Host tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::ext2::{BlockReader, Ext2BlockGroupDescriptor, Ext2Superblock, resolve_path};
    use super::super::ext2_format::{Ext2Fs, FormatParams, format_ext2};
    use super::*;
    use alloc::string::String;

    /// In-memory volume (same shape as the `ext2_format` test volume): backs
    /// the write seam and the real read path.
    struct MemVolume {
        data: Vec<u8>,
        block_size: u32,
        reads: u64,
        write_ops: u64,
        blocks_written: u64,
    }

    impl MemVolume {
        fn new(total_blocks: u32, block_size: u32) -> Self {
            MemVolume {
                data: vec![0u8; (total_blocks * block_size) as usize],
                block_size,
                reads: 0,
                write_ops: 0,
                blocks_written: 0,
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
            self.reads += 1;
            let r = self.range(block);
            if r.end > self.data.len() {
                return Err(Ext2Error::TruncatedInput);
            }
            buf.copy_from_slice(&self.data[r]);
            Ok(())
        }
        fn write_block(&mut self, block: u32, data: &[u8]) -> Result<(), Ext2Error> {
            self.write_ops += 1;
            self.blocks_written += 1;
            let r = self.range(block);
            if r.end > self.data.len() {
                return Err(Ext2Error::TruncatedInput);
            }
            self.data[r].copy_from_slice(data);
            Ok(())
        }
        fn write_block_run(
            &mut self,
            start_block: u32,
            count: u32,
            data: &[u8],
        ) -> Result<(), Ext2Error> {
            self.write_ops += 1;
            self.blocks_written += count as u64;
            let bs = self.block_size as usize;
            for i in 0..count as usize {
                let r = self.range(start_block + i as u32);
                if r.end > self.data.len() {
                    return Err(Ext2Error::TruncatedInput);
                }
                self.data[r].copy_from_slice(&data[i * bs..(i + 1) * bs]);
            }
            Ok(())
        }
    }

    /// Fresh-mount read view over a `MemVolume` (superblock + BGDs parsed
    /// from the bytes — no state shared with any writer).
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

    fn params(total_blocks: u32, block_size_log: u32) -> FormatParams {
        FormatParams {
            total_blocks,
            block_size_log,
            uuid: [7; 16],
        }
    }

    /// Build the canonical source tree used across the tests. Returns the
    /// populated volume; content spans direct + indirect files, nested dirs,
    /// inline + block symlinks, non-root ownership, and a wide directory.
    fn build_source(block_size_log: u32, total_blocks: u32) -> MemVolume {
        let bs = 1024u32 << block_size_log;
        let mut vol = MemVolume::new(total_blocks, bs);
        format_ext2(&mut vol, &params(total_blocks, block_size_log)).expect("format src");
        let mut fs = Ext2Fs::open(&mut vol, block_size_log).expect("open src");
        let root = EXT2_ROOT_INO;

        let etc = fs.create_dir(&mut vol, root, "etc", 0o755).expect("etc");
        fs.create_file(&mut vol, etc, "hostname", b"m3os\n", 0o644)
            .expect("hostname");
        fs.create_file(&mut vol, etc, "shadow", b"root:x:0:0\n", 0o600)
            .expect("shadow");

        let bin = fs.create_dir(&mut vol, root, "bin", 0o755).expect("bin");
        // Indirect-spanning "binary" (300 KiB at 1 KiB blocks crosses the
        // single-indirect boundary; still multi-block at 4 KiB).
        let big: Vec<u8> = (0..300 * 1024u32).map(|i| (i % 253) as u8).collect();
        fs.create_file(&mut vol, bin, "sh0", &big, 0o755)
            .expect("sh0");
        fs.create_symlink(&mut vol, bin, "sh", "sh0")
            .expect("sh symlink");
        fs.create_symlink(
            &mut vol,
            bin,
            "longlink",
            "../a/very/long/target/path/that/does/not/fit/inline/in/the/inode/block/pointer/array",
        )
        .expect("long symlink");

        let home = fs.create_dir(&mut vol, root, "home", 0o755).expect("home");
        let user = fs.create_dir(&mut vol, home, "user", 0o700).expect("user");
        let profile = fs
            .create_file(&mut vol, user, ".profile", b"export PS1='$ '\n", 0o640)
            .expect("profile");
        // Non-root ownership + timestamps on the user subtree.
        for ino in [user, profile] {
            let mut i = fs.read_inode(&mut vol, ino).expect("read");
            i.uid = 1000;
            i.gid = 1000;
            i.atime = 111;
            i.ctime = 222;
            i.mtime = 333;
            fs.write_inode(&mut vol, ino, &i).expect("write");
        }

        // Deep nesting.
        let mut cur = root;
        for name in ["a", "b", "c", "d"] {
            cur = fs.create_dir(&mut vol, cur, name, 0o755).expect("nest");
        }
        fs.create_file(&mut vol, cur, "leaf", b"bottom\n", 0o444)
            .expect("leaf");

        // Wide directory (spills past one dir block at 1 KiB).
        let wide = fs.create_dir(&mut vol, root, "wide", 0o755).expect("wide");
        for n in 0..60u32 {
            let name = alloc::format!("file-{n:02}");
            let body = alloc::format!("payload {n}\n");
            fs.create_file(&mut vol, wide, &name, body.as_bytes(), 0o644)
                .expect("wide file");
        }

        fs.flush(&mut vol).expect("flush src");
        vol
    }

    /// Recursively compare two mounted trees: identical entry sets, inode
    /// metadata (mode/uid/gid/mtime), file bytes, symlink targets.
    fn assert_tree_equal(src: &MountView<'_>, dst: &MountView<'_>, src_ino: u32, dst_ino: u32) {
        let si = read_inode(src, src_ino).expect("src inode");
        let di = read_inode(dst, dst_ino).expect("dst inode");
        assert_eq!(si.mode, di.mode, "mode mismatch at ino {src_ino}");
        assert_eq!(si.uid, di.uid, "uid mismatch");
        assert_eq!(si.gid, di.gid, "gid mismatch");
        if si.is_regular() || si.is_dir() {
            assert_eq!(si.mtime, di.mtime, "mtime mismatch");
        }
        if si.is_dir() {
            let mut s_entries: Vec<(String, u32, u8)> = read_directory_entries(src, &si)
                .expect("src entries")
                .into_iter()
                .filter(|(n, _, _)| {
                    n != "." && n != ".." && !(src_ino == EXT2_ROOT_INO && n == "lost+found")
                })
                .collect();
            let mut d_entries: Vec<(String, u32, u8)> = read_directory_entries(dst, &di)
                .expect("dst entries")
                .into_iter()
                .filter(|(n, _, _)| {
                    n != "." && n != ".." && !(dst_ino == EXT2_ROOT_INO && n == "lost+found")
                })
                .collect();
            s_entries.sort_by(|a, b| a.0.cmp(&b.0));
            d_entries.sort_by(|a, b| a.0.cmp(&b.0));
            let s_names: Vec<&String> = s_entries.iter().map(|(n, _, _)| n).collect();
            let d_names: Vec<&String> = d_entries.iter().map(|(n, _, _)| n).collect();
            assert_eq!(s_names, d_names, "entry sets differ under ino {src_ino}");
            for ((_, s_child, _), (_, d_child, _)) in s_entries.iter().zip(&d_entries) {
                assert_tree_equal(src, dst, *s_child, *d_child);
            }
        } else if si.is_regular() {
            assert_eq!(si.size, di.size, "size mismatch");
            let mut sbuf = vec![0u8; si.size as usize];
            let mut dbuf = vec![0u8; di.size as usize];
            read_file_data(src, &si, 0, &mut sbuf).expect("src data");
            read_file_data(dst, &di, 0, &mut dbuf).expect("dst data");
            assert_eq!(sbuf, dbuf, "file bytes differ");
        } else if si.is_symlink() {
            assert_eq!(
                read_symlink_target(src, &si).expect("src target"),
                read_symlink_target(dst, &di).expect("dst target"),
            );
        }
    }

    #[test]
    fn populate_copies_tree_across_block_sizes() {
        // 1 KiB source → 4 KiB target: the install case (block sizes need
        // not match; the copy is file-level, not block-level).
        let src_vol = build_source(0, 4096);
        let src = MountView::mount(&src_vol);

        let mut dst_vol = MemVolume::new(2048, 4096);
        format_ext2(&mut dst_vol, &params(2048, 2)).expect("format dst");
        let mut fs = Ext2Fs::open(&mut dst_vol, 2).expect("open dst");
        let stats = populate_from_reader(&src, &mut fs, &mut dst_vol).expect("populate");
        fs.flush(&mut dst_vol).expect("flush");

        // etc, bin, home, user, a, b, c, d, wide = 9 dirs.
        assert_eq!(stats.dirs, 9);
        // 2 etc + 1 sh0 + 1 profile + 1 leaf + 60 wide = 65 files.
        assert_eq!(stats.files, 65);
        assert_eq!(stats.symlinks, 2);
        assert_eq!(stats.skipped, 0);
        assert!(stats.bytes > 300 * 1024);

        let dst = MountView::mount(&dst_vol);
        assert_tree_equal(&src, &dst, EXT2_ROOT_INO, EXT2_ROOT_INO);
        // Spot-check deep resolution + ownership on the target.
        let leaf = resolve_path(&dst, "/a/b/c/d/leaf").expect("leaf resolves");
        let li = read_inode(&dst, leaf).expect("leaf inode");
        assert_eq!(li.mode & 0o7777, 0o444);
        let user = resolve_path(&dst, "/home/user").expect("user resolves");
        let ui = read_inode(&dst, user).expect("user inode");
        assert_eq!((ui.uid, ui.gid, ui.mtime), (1000, 1000, 333));
    }

    #[test]
    fn populate_through_write_back_cache_is_equivalent_and_cheaper() {
        let src_vol = build_source(0, 4096);
        let src = MountView::mount(&src_vol);

        // Direct populate.
        let mut direct = MemVolume::new(2048, 4096);
        format_ext2(&mut direct, &params(2048, 2)).expect("format");
        let mut fs = Ext2Fs::open(&mut direct, 2).expect("open");
        populate_from_reader(&src, &mut fs, &mut direct).expect("populate");
        fs.flush(&mut direct).expect("flush");
        let direct_ops = direct.write_ops;

        // Cached populate (small cap to force eviction traffic too).
        let mut cached = MemVolume::new(2048, 4096);
        format_ext2(&mut cached, &params(2048, 2)).expect("format");
        {
            let mut wb = WriteBackBlockIo::new(&mut cached, 4096, 32, 16);
            let mut fs = Ext2Fs::open(&mut wb, 2).expect("open");
            populate_from_reader(&src, &mut fs, &mut wb).expect("populate");
            fs.flush(&mut wb).expect("fs flush");
            wb.flush().expect("cache flush");
        }

        assert_eq!(
            direct.data, cached.data,
            "cached populate must produce the identical volume image"
        );
        assert!(
            cached.write_ops < direct_ops / 2,
            "cache should at least halve device write requests: direct={direct_ops} cached={}",
            cached.write_ops
        );
    }

    #[test]
    fn write_back_cache_reads_its_own_writes() {
        let mut vol = MemVolume::new(64, 1024);
        let mut wb = WriteBackBlockIo::new(&mut vol, 1024, 4, 4);

        // Blind writes accumulate in a run; reads see them before any flush.
        let a = [0xAA; 1024];
        let b = [0xBB; 1024];
        wb.write_block(10, &a).expect("w10");
        wb.write_block(11, &b).expect("w11");
        let mut buf = [0u8; 1024];
        wb.read_block(11, &mut buf).expect("r11");
        assert_eq!(buf, b);
        // Overwrite inside the pending run.
        let b2 = [0xB2; 1024];
        wb.write_block(11, &b2).expect("w11 again");
        wb.read_block(11, &mut buf).expect("r11 again");
        assert_eq!(buf, b2);
        // Nothing reached the device yet.
        assert_eq!(wb.stats.inner_write_ops, 0);

        // A non-adjacent write flushes the run as ONE request.
        wb.write_block(40, &a).expect("w40");
        assert_eq!(wb.stats.inner_write_ops, 1);
        assert_eq!(wb.stats.inner_blocks_written, 2);

        // Read-modify-write cycle lands in the map, deferred until flush.
        let mut meta = [0u8; 1024];
        wb.read_block(5, &mut meta).expect("r5");
        meta[0] = 0x55;
        wb.write_block(5, &meta).expect("w5");
        let ops_before = wb.stats.inner_write_ops;
        wb.read_block(5, &mut buf).expect("r5 again");
        assert_eq!(buf[0], 0x55);
        assert_eq!(
            wb.stats.inner_write_ops, ops_before,
            "map write stayed deferred"
        );

        wb.flush().expect("flush");
        drop(wb);
        assert_eq!(vol.data[10 * 1024], 0xAA);
        assert_eq!(vol.data[11 * 1024], 0xB2);
        assert_eq!(vol.data[40 * 1024], 0xAA);
        assert_eq!(vol.data[5 * 1024], 0x55);
    }

    #[test]
    fn populate_survives_a_directory_cycle() {
        // Corrupt source: /a's directory block gains an entry pointing back
        // at /a itself. The visited set must terminate the walk and count
        // one skip.
        let mut src_vol = build_source(0, 4096);
        {
            let view = MountView::mount(&src_vol);
            let a_ino = resolve_path(&view, "/a").expect("a");
            let a_inode = read_inode(&view, a_ino).expect("a inode");
            let dir_block = a_inode.block[0] as usize;
            let bs = 1024usize;
            // Walk to the terminal entry and split its slack for a new one.
            let base = dir_block * bs;
            let mut off = 0usize;
            loop {
                let rec = u16::from_le_bytes([
                    src_vol.data[base + off + 4],
                    src_vol.data[base + off + 5],
                ]) as usize;
                if off + rec >= bs {
                    // Terminal entry: shrink it, append the cycle entry.
                    let name_len = src_vol.data[base + off + 6] as usize;
                    let used = (8 + name_len + 3) & !3;
                    src_vol.data[base + off + 4..base + off + 6]
                        .copy_from_slice(&(used as u16).to_le_bytes());
                    let new = base + off + used;
                    let new_rec = (bs - off - used) as u16;
                    src_vol.data[new..new + 4].copy_from_slice(&a_ino.to_le_bytes());
                    src_vol.data[new + 4..new + 6].copy_from_slice(&new_rec.to_le_bytes());
                    src_vol.data[new + 6] = 4; // name_len
                    src_vol.data[new + 7] = 2; // EXT2_FT_DIR
                    src_vol.data[new + 8..new + 12].copy_from_slice(b"loop");
                    break;
                }
                off += rec;
            }
        }
        let src = MountView::mount(&src_vol);

        let mut dst_vol = MemVolume::new(2048, 4096);
        format_ext2(&mut dst_vol, &params(2048, 2)).expect("format dst");
        let mut fs = Ext2Fs::open(&mut dst_vol, 2).expect("open dst");
        let stats = populate_from_reader(&src, &mut fs, &mut dst_vol).expect("populate");
        assert_eq!(stats.skipped, 1, "the cycle entry must be skipped");
        // The rest of the tree still arrived.
        let dst = MountView::mount(&dst_vol);
        assert!(resolve_path(&dst, "/a/b/c/d/leaf").is_ok());
    }

    /// Falsifiable external check: a format → populate (through the cache)
    /// volume must pass `e2fsck -fn` clean. Skips silently when e2fsck is
    /// absent (same posture as the C.5 test).
    #[test]
    fn e2fsck_accepts_populated_target_when_available() {
        use std::io::Write as _;
        use std::process::Command;

        let which = Command::new("sh")
            .args(["-c", "command -v e2fsck"])
            .output();
        let Ok(out) = which else { return };
        if !out.status.success() {
            return;
        }

        let src_vol = build_source(0, 4096);
        let src = MountView::mount(&src_vol);

        let mut dst_vol = MemVolume::new(2048, 4096);
        format_ext2(&mut dst_vol, &params(2048, 2)).expect("format dst");
        {
            let mut wb = WriteBackBlockIo::new(&mut dst_vol, 4096, 64, 32);
            let mut fs = Ext2Fs::open(&mut wb, 2).expect("open dst");
            populate_from_reader(&src, &mut fs, &mut wb).expect("populate");
            fs.flush(&mut wb).expect("fs flush");
            wb.flush().expect("cache flush");
        }

        let dir = std::env::temp_dir();
        let path = dir.join(format!("m3os-c4-populate-{}.img", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("temp image");
        f.write_all(&dst_vol.data).expect("write image");
        drop(f);

        let fsck = Command::new("e2fsck")
            .args(["-fn", path.to_str().expect("utf8 path")])
            .output()
            .expect("run e2fsck");
        let _ = std::fs::remove_file(&path);
        assert!(
            fsck.status.success(),
            "e2fsck rejected the populated target:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&fsck.stdout),
            String::from_utf8_lossy(&fsck.stderr),
        );
    }
}
