//! ext2 filesystem driver (Phase 28, Tracks B–G).
//!
//! Provides a complete read/write ext2 volume driver backed by virtio-blk I/O.
//! Implements inode reading, block pointer traversal, directory operations,
//! file read/write, bitmap management, and native Unix metadata (VfsMetadata).

#![allow(dead_code)]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use kernel_core::fs::ext2::{
    EXT2_DIND_BLOCK, EXT2_FT_DIR, EXT2_FT_REG_FILE, EXT2_FT_SYMLINK, EXT2_IND_BLOCK,
    EXT2_NDIR_BLOCKS, Ext2BlockGroupDescriptor, Ext2Error, Ext2Inode, Ext2Superblock, S_IFDIR,
    S_IFLNK, S_IFREG,
};

use spin::Mutex;

// ---------------------------------------------------------------------------
// Ext2Volume (P28-T019)
// ---------------------------------------------------------------------------

/// Maximum number of ext2 blocks held in the read cache.
///
/// 4096 entries × 4 KiB/block = 16 MiB budget.  This covers:
///   - all userspace ELF binaries loaded at boot      (~400 blocks)
///   - a full doom1.wad (4.2 MiB / 4 KiB ≈ 1031 blocks)
///   - filesystem metadata (inodes, directories, etc.)
///   - comfortable headroom for other games / large files
///
/// The kernel heap grows on demand up to 64 MiB, so 16 MiB of cache data is
/// well within budget.  After the first cold pass all VirtIO round-trips for
/// cached blocks are eliminated.
const BLOCK_CACHE_MAX: usize = 4096;

/// A mounted ext2 volume backed by virtio-blk sectors.
pub struct Ext2Volume {
    /// Absolute LBA of the partition start on the block device.
    base_lba: u64,
    /// Remote block-device id this volume reads/writes through. `0` = the root
    /// backend via the global `blk::read_sectors`/`write_sectors` (the original
    /// singleton behaviour, byte-identical). `>= 1` = a secondary device in the
    /// `blk::remote` registry (Phase 92a D.4 — e.g. a `/mnt/usbN` mount), routed
    /// via `blk::read_sectors_dev`/`write_sectors_dev`.
    dev_id: u32,
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
    /// Read-through block cache: block_num → data.
    /// Bounded to BLOCK_CACHE_MAX entries; no eviction (fill-and-hold).
    ///
    /// Phase 57e Bug #9 — uses plain `spin::Mutex`, NOT `IrqSafeMutex`.
    /// The block cache is only touched from task context (read/write
    /// syscalls dispatched through `EXT2_VOLUME`); no ISR ever reaches
    /// it.  See `EXT2_VOLUME` doc comment for full rationale.
    block_cache: Mutex<BTreeMap<u32, Vec<u8>>>,
}

/// Global mounted ext2 volume (set by mount_ext2).
///
/// Phase 57e Bug #9 — uses plain `spin::Mutex`, NOT `IrqSafeMutex`.
/// Reason: every `IrqSafeMutex::lock` raises `preempt_count`; if the
/// guard outlives a `block_current_until` (which it does for every
/// `read_inode` / `read_file_data` that descends into `virtio_blk`), the
/// +1 leaks for the entire syscall.  The IRQ-side preempt gate
/// (`peek_preempt_count_irq`) reads non-zero and refuses to preempt the
/// holder, so the running task monopolises its core for the entire
/// disk operation while co-resident Ready tasks starve — exactly the
/// Bug #9 fingerprint Sessions 13–15 chased.  `EXT2_VOLUME` is only
/// acquired from task context (mount, read/write/getdents syscalls);
/// no ISR ever reaches it, so the preempt-disable side-effect of
/// `IrqSafeMutex` is unnecessary defensive coverage that is now
/// actively harmful.
///
/// **2026-06-16 — yields on contention (`YieldingMutex`), not a bare spin.**
/// The rationale above (the guard outlives `block_current_until` for every
/// `read_inode`/`read_file_data` that descends into `virtio_blk`) has a
/// corollary the original `spin::Mutex` choice missed: while task A sleeps in
/// that I/O *holding this lock*, a task B that acquires `EXT2_VOLUME` would, on
/// a bare `spin::Mutex`, **busy-spin** — and on a single core that spin
/// monopolises the only CPU, so A is never rescheduled to finish its read and
/// release the lock. Hard deadlock: the machine wedges at 100% CPU in
/// `path_node_nofollow`'s `EXT2_VOLUME.lock()` with no watchdog (IRQs fire but
/// the spin is kernel-mode and preemption can't unwind a spinlock). This is the
/// `claude -p` whole-machine freeze during Claude Code's concurrent demand-paged
/// `exec`(rg)/`stat` storm — confirmed by a host-side QMP `info registers` dump
/// pinning the constant spin RIP to this lock
/// (docs/handoffs/2026-06-15-claude-code-openrouter-emfile-fd-limit.md #4).
/// `YieldingMutex::lock` `try_lock`s and, only on contention, `yield_now`s so the
/// I/O-blocked holder is rescheduled, releases, and B re-acquires — converting
/// the deadlock into a cooperative wait. Uncontended acquisition (boot mount,
/// the common fast path) takes the `try_lock` with no yield, so the
/// no-preempt-disable property above is preserved.
pub static EXT2_VOLUME: YieldingMutex<Option<Ext2Volume>> = YieldingMutex::new(None);

/// A mutex that **yields the CPU on contention** instead of busy-spinning.
///
/// Drop-in for `spin::Mutex` at the `EXT2_VOLUME` call sites (`lock()` returns
/// the same `spin::MutexGuard`). The only behavioural difference is the wait
/// strategy: a bare `spin::Mutex` `relax`es with `pause` (busy-spin), which on a
/// single core deadlocks when the lock is held across a blocking operation (see
/// `EXT2_VOLUME`); this instead calls `yield_now()` so the scheduler can run the
/// lock holder. Yield only happens when `try_lock` fails, i.e. only under real
/// contention — which can only occur once the scheduler is up and >1 task is
/// runnable, so the boot-time uncontended acquisitions never yield.
pub struct YieldingMutex<T> {
    inner: Mutex<T>,
}

impl<T> YieldingMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }

    /// Acquire the lock, yielding (not busy-spinning) while it is contended.
    #[inline]
    pub fn lock(&self) -> spin::MutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.inner.try_lock() {
                return guard;
            }
            // Contended: the holder may be asleep in virtio-blk I/O *holding this
            // lock*. Busy-spinning here would deny it the CPU forever on a single
            // core. Yield so it is rescheduled to release the lock.
            crate::task::scheduler::yield_now();
        }
    }

    /// Non-blocking acquire (no yield, no spin).
    #[inline]
    pub fn try_lock(&self) -> Option<spin::MutexGuard<'_, T>> {
        self.inner.try_lock()
    }
}

/// Phase 88 Track C — expose the kernel `Ext2Volume` as a `BlockReader` so the
/// higher-level read logic (resolve_path / read_inode / read_file_data /
/// resolve_block / dir parsing) lives once in `kernel_core::fs::ext2` and is
/// shared with the ring-3 `vfs_server`. The block source is this volume's
/// cache-aware `read_block`; runs use the Phase 87 multi-block path.
impl kernel_core::fs::ext2::BlockReader for Ext2Volume {
    fn block_size(&self) -> u32 {
        self.block_size
    }
    fn inodes_per_group(&self) -> u32 {
        self.superblock.inodes_per_group
    }
    fn inode_size(&self) -> u32 {
        self.superblock.inode_size as u32
    }
    fn inode_table_block(&self, group: u32) -> Result<u32, Ext2Error> {
        self.bgd_table
            .get(group as usize)
            .map(|b| b.inode_table)
            .ok_or(Ext2Error::CorruptedEntry)
    }
    fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error> {
        Ext2Volume::read_block(self, block_num)
    }
    fn max_run_blocks(&self) -> u32 {
        kernel_core::driver_ipc::block::MAX_SECTORS_PER_REQUEST / self.sectors_per_block
    }
    fn read_block_run(
        &self,
        start_block: u32,
        count: u32,
        dst: &mut [u8],
    ) -> Result<(), Ext2Error> {
        self.read_run_into_slice(start_block, count, dst)
    }
    fn read_block_into(
        &self,
        block_num: u32,
        block_offset: usize,
        dst: &mut [u8],
    ) -> Result<(), Ext2Error> {
        self.read_block_into_slice(block_num, block_offset, dst)
    }
}

/// Read sectors from device `dev_id` — the root backend (`dev_id == 0`) via the
/// global `blk::read_sectors` (byte-identical to the pre-Phase-92a singleton),
/// or a secondary registry device (`dev_id >= 1`, e.g. a USB stick) via
/// `blk::read_sectors_dev`.
fn read_sectors_for(dev_id: u32, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), u8> {
    if dev_id == 0 {
        crate::blk::read_sectors(lba, count, buf)
    } else {
        crate::blk::read_sectors_dev(dev_id, lba, count, buf)
    }
}

/// Write sectors to device `dev_id`. `dev_id == 0` uses the global
/// `blk::write_sectors` (root, grant-backed inline path); `dev_id >= 1` uses
/// the secondary-device `blk::write_sectors_dev` inline path.
fn write_sectors_for(dev_id: u32, lba: u64, count: usize, buf: &[u8]) -> Result<(), u8> {
    if dev_id == 0 {
        crate::blk::write_sectors(lba, count, buf)
    } else {
        crate::blk::write_sectors_dev(dev_id, lba, count, buf)
    }
}

// ---------------------------------------------------------------------------
// Phase 92a D.4 — secondary ext2 mount table (e.g. /mnt/usb0)
// ---------------------------------------------------------------------------

/// One secondary ext2 mount: a path prefix bound to an [`Ext2Volume`] on a
/// `blk::remote` registry device (`dev_id >= 1`). The root `/` mount stays in
/// [`EXT2_VOLUME`]; this table holds only the additional mounts so the root
/// path is completely untouched (Phase 92a D.4).
pub struct UsbMount {
    /// Absolute mount-point prefix, e.g. `"/mnt/usb0"` (no trailing slash).
    pub prefix: String,
    /// The ext2 volume backing this mount, reading via its `dev_id`.
    pub volume: Ext2Volume,
}

/// Active secondary mounts. Empty on a machine with no USB storage mounted, so
/// the lookup helpers short-circuit and the root FS path is unaffected.
pub static USB_MOUNTS: YieldingMutex<Vec<UsbMount>> = YieldingMutex::new(Vec::new());

/// Lock-free fast-path: `true` only while [`USB_MOUNTS`] is non-empty. Every
/// path-resolving syscall checks this *before* acquiring the `USB_MOUNTS` lock,
/// so on the overwhelmingly common no-USB-mounted machine the secondary-mount
/// routing adds a single relaxed atomic load to the root FS hot path and never
/// touches the lock.
static USB_MOUNTS_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[inline]
fn usb_mounts_active() -> bool {
    USB_MOUNTS_ACTIVE.load(core::sync::atomic::Ordering::Acquire)
}

/// Match `abs_path` against `prefix`, returning the in-volume path (always
/// starting with `/`) if `abs_path` is the mount point itself or a child of it.
/// `"/mnt/usb0"` → `"/"`; `"/mnt/usb0/dir/f"` → `"/dir/f"`; `"/mnt/usb01"` → None
/// (boundary-safe, no false prefix match). Returns a borrow of `abs_path` (or
/// the static `"/"`), so it never allocates — `is_usb_mount_path`'s `.is_some()`
/// probe stays allocation-free on every `/mnt/usbN` open/stat hot path.
fn match_mount_prefix<'a>(abs_path: &'a str, prefix: &str) -> Option<&'a str> {
    if abs_path == prefix {
        return Some("/");
    }
    let rest = abs_path.strip_prefix(prefix)?;
    if rest.starts_with('/') {
        Some(rest)
    } else {
        None
    }
}

/// Mount an ext2 volume on registry device `dev_id` at `prefix` (e.g.
/// `"/mnt/usb0"`). If a mount already exists at the same prefix it is replaced,
/// and that displaced mount's `dev_id` is returned as `Some(old_dev_id)` so the
/// caller can unregister its now-orphaned `blk::remote` slot. Without that,
/// repeated remounts to the same prefix leak registry slots until
/// `MAX_REMOTE_BLOCK` is exhausted.
pub fn mount_usb(prefix: &str, base_lba: u64, dev_id: u32) -> Result<Option<u32>, Ext2Error> {
    let vol = Ext2Volume::mount_dev(base_lba, dev_id)?;
    let mut mounts = USB_MOUNTS.lock();
    let displaced = mounts
        .iter()
        .find(|m| m.prefix == prefix)
        .map(|m| m.volume.dev_id);
    mounts.retain(|m| m.prefix != prefix);
    mounts.push(UsbMount {
        prefix: String::from(prefix),
        volume: vol,
    });
    USB_MOUNTS_ACTIVE.store(!mounts.is_empty(), core::sync::atomic::Ordering::Release);
    Ok(displaced)
}

/// Remove the secondary mount at `prefix`. Returns the freed `dev_id` of the
/// removed mount's backing volume (so the caller can `unregister_remote_device`
/// it), or `None` if no mount matched `prefix`.
///
/// Phase 92 C.4: wired from `sys_linux_umount2` for both a voluntary
/// `umount /mnt/usbN` and the `usb-storage` daemon's hot-unplug detach path.
pub fn unmount_usb(prefix: &str) -> Option<u32> {
    let mut mounts = USB_MOUNTS.lock();
    let dev_id = mounts
        .iter()
        .find(|m| m.prefix == prefix)
        .map(|m| m.volume.dev_id);
    mounts.retain(|m| m.prefix != prefix);
    USB_MOUNTS_ACTIVE.store(!mounts.is_empty(), core::sync::atomic::Ordering::Release);
    dev_id
}

/// `true` if `abs_path` falls under any secondary mount prefix.
pub fn is_usb_mount_path(abs_path: &str) -> bool {
    if !usb_mounts_active() {
        return false;
    }
    let mounts = USB_MOUNTS.lock();
    mounts
        .iter()
        .any(|m| match_mount_prefix(abs_path, &m.prefix).is_some())
}

/// Run `f` against the secondary-mount volume serving `abs_path` (read-only),
/// passing the in-volume relative path. Returns `None` if `abs_path` is not
/// under any secondary mount (the caller then takes the existing root path).
pub fn with_usb_mount<R>(abs_path: &str, f: impl FnOnce(&Ext2Volume, &str) -> R) -> Option<R> {
    if !usb_mounts_active() {
        return None;
    }
    let mounts = USB_MOUNTS.lock();
    for m in mounts.iter() {
        if let Some(rel) = match_mount_prefix(abs_path, &m.prefix) {
            return Some(f(&m.volume, rel));
        }
    }
    None
}

/// Mutable counterpart of [`with_usb_mount`] for the write path
/// (`write_file_data`/`truncate_file` take `&mut Ext2Volume`).
pub fn with_usb_mount_mut<R>(
    abs_path: &str,
    f: impl FnOnce(&mut Ext2Volume, &str) -> R,
) -> Option<R> {
    if !usb_mounts_active() {
        return None;
    }
    let mut mounts = USB_MOUNTS.lock();
    for m in mounts.iter_mut() {
        if let Some(rel) = match_mount_prefix(abs_path, &m.prefix) {
            return Some(f(&mut m.volume, rel));
        }
    }
    None
}

impl Ext2Volume {
    /// Mount an ext2 partition at the given base LBA on the root device (P28-T019).
    pub fn mount(base_lba: u64) -> Result<Self, Ext2Error> {
        Self::mount_dev(base_lba, 0)
    }

    /// Mount an ext2 partition at `base_lba` on remote block device `dev_id`
    /// (Phase 92a D.4). `dev_id == 0` is the root backend (identical to
    /// [`Ext2Volume::mount`]); `dev_id >= 1` reads/writes through the
    /// secondary-device `blk::*_dev` routing (e.g. a `/mnt/usbN` USB stick).
    pub fn mount_dev(base_lba: u64, dev_id: u32) -> Result<Self, Ext2Error> {
        // Superblock is at byte offset 1024 from partition start = LBA + 2 sectors.
        let sb_lba = base_lba + 2; // 1024 bytes / 512 bytes per sector
        let mut sb_raw = vec![0u8; 1024];
        read_sectors_for(dev_id, sb_lba, 2, &mut sb_raw).map_err(|_| Ext2Error::IoError)?;

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
        read_sectors_for(dev_id, bgd_lba, bgd_sectors, &mut bgd_raw)
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
            dev_id,
            superblock,
            bgd_table,
            block_size,
            sectors_per_block,
            superblock_raw: sb_raw,
            block_cache: Mutex::new(BTreeMap::new()),
        })
    }

    // -----------------------------------------------------------------------
    // Low-level block I/O
    // -----------------------------------------------------------------------

    /// Convert an ext2 block number to an absolute disk LBA.
    fn block_to_lba(&self, block_num: u32) -> u64 {
        self.base_lba + (block_num as u64) * (self.sectors_per_block as u64)
    }

    /// Read an ext2 block, serving from the in-memory cache when possible.
    ///
    /// The cache is bounded by BLOCK_CACHE_MAX entries (fill-and-hold, no
    /// eviction). Once full, new blocks are served from disk without caching.
    /// This asymptotically eliminates repeated VirtIO round-trips for hot
    /// blocks such as WAD lumps, directory entries, and inode tables.
    ///
    /// Implementation note: the result buffer is always pre-allocated *before*
    /// acquiring the cache spinlock.  This prevents the heap allocator from
    /// being invoked while holding a spinlock, avoiding potential contention
    /// between the allocator lock and the cache lock.
    fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error> {
        // Pre-allocate the result buffer outside any lock so the heap
        // allocator is never called while a spinlock is held.
        let mut buf = vec![0u8; self.block_size as usize];

        // Cache hit: memcpy cached data into the pre-allocated buffer.
        {
            let cache = self.block_cache.lock();
            if let Some(cached) = cache.get(&block_num) {
                buf.copy_from_slice(cached);
                return Ok(buf);
            }
        }

        // Cache miss: read from the backing block device.
        read_sectors_for(
            self.dev_id,
            self.block_to_lba(block_num),
            self.sectors_per_block as usize,
            &mut buf,
        )
        .map_err(|_| Ext2Error::IoError)?;

        // Clone buf for the cache entry (allocation outside the lock), then
        // take the lock only to insert the already-allocated entry.
        let cached_copy = buf.clone();
        {
            let mut cache = self.block_cache.lock();
            if cache.len() < BLOCK_CACHE_MAX {
                cache.insert(block_num, cached_copy);
            }
        }
        Ok(buf)
    }

    /// Copy exactly `dst.len()` bytes from `block_num[block_offset..]` into `dst`.
    ///
    /// Optimised for the file-data hot path:
    /// - **Cache hit**: copies directly under the spinlock (no heap allocation).
    /// - **Cache miss**: one allocation for the full block, VirtIO read, copy
    ///   the requested slice into `dst`, then insert the full block into the cache.
    ///
    /// This eliminates the intermediate `Vec<u8>` that `read_block` would allocate
    /// for each data block in `read_file_data`, halving the allocation/copy work on
    /// the cache-warm path.
    fn read_block_into_slice(
        &self,
        block_num: u32,
        block_offset: usize,
        dst: &mut [u8],
    ) -> Result<(), Ext2Error> {
        // Cache hit: copy directly under the spinlock — no heap allocation.
        {
            let cache = self.block_cache.lock();
            if let Some(cached) = cache.get(&block_num) {
                dst.copy_from_slice(&cached[block_offset..block_offset + dst.len()]);
                return Ok(());
            }
        }

        // Cache miss: read the full block from the backing device, cache it,
        // then copy the requested slice into dst.
        let mut block_buf = vec![0u8; self.block_size as usize];
        read_sectors_for(
            self.dev_id,
            self.block_to_lba(block_num),
            self.sectors_per_block as usize,
            &mut block_buf,
        )
        .map_err(|_| Ext2Error::IoError)?;

        dst.copy_from_slice(&block_buf[block_offset..block_offset + dst.len()]);

        // Insert the full block into the cache (allocation already done above).
        {
            let mut cache = self.block_cache.lock();
            if cache.len() < BLOCK_CACHE_MAX {
                cache.insert(block_num, block_buf);
            }
        }
        Ok(())
    }

    fn write_block(&self, block_num: u32, data: &[u8]) -> Result<(), Ext2Error> {
        // Invalidate before write so stale data is never served.
        self.block_cache.lock().remove(&block_num);
        let lba = self.block_to_lba(block_num);
        write_sectors_for(self.dev_id, lba, self.sectors_per_block as usize, data)
            .map_err(|_| Ext2Error::IoError)
    }

    /// Phase 87 Track B.1 — read `count` physically-contiguous WHOLE blocks
    /// starting at physical block `start_block` directly into `dst`, in ONE
    /// `blk::read_sectors` round-trip. `dst.len()` must equal
    /// `count * block_size`. This is the coalesced bulk path: the blocks are
    /// physically contiguous so their LBAs are contiguous, so a single
    /// multi-block device request fills `dst` in logical order — collapsing the
    /// per-block round-trips that dominate large sequential reads.
    ///
    /// Cache-bypassing on purpose: a multi-MiB sequential read (a `pkg install`
    /// payload, a cold binary load) would otherwise thrash the bounded
    /// `BLOCK_CACHE_MAX` cache; and the block cache is a clean read-through copy
    /// of disk (`write_block` invalidates then writes through, and out-of-band
    /// `vfs_server` writes call `invalidate_block_cache`), so reading straight
    /// from disk always returns current data. Head/tail partial blocks still go
    /// through the cache-aware `read_block_into_slice`.
    fn read_run_into_slice(
        &self,
        start_block: u32,
        count: u32,
        dst: &mut [u8],
    ) -> Result<(), Ext2Error> {
        read_sectors_for(
            self.dev_id,
            self.block_to_lba(start_block),
            count as usize * self.sectors_per_block as usize,
            dst,
        )
        .map_err(|_| Ext2Error::IoError)
    }

    /// Drop the entire read-through block cache (Phase 88).
    ///
    /// When `vfs_server` is the ext2 write authority, mutations land on disk
    /// out-of-band from this engine's cache. The kernel still reads ext2
    /// metadata directly (e.g. `resolve_path` for `fstat` st_ino, the exec
    /// loader) so its cache must be flushed after a routed mutation, or it
    /// would serve a stale directory/inode/bitmap block. Cheap insurance: the
    /// next read repopulates from disk.
    pub fn invalidate_block_cache(&self) {
        self.block_cache.lock().clear();
    }

    // -----------------------------------------------------------------------
    // Inode operations (P28-T010)
    // -----------------------------------------------------------------------

    /// Read an inode by number (1-based). Phase 88 Track C — delegates to the
    /// shared `kernel_core::fs::ext2::read_inode` over this volume's `BlockReader`.
    pub fn read_inode(&self, inode_num: u32) -> Result<Ext2Inode, Ext2Error> {
        kernel_core::fs::ext2::read_inode(self, inode_num)
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
    /// Returns 0 for sparse/hole blocks. Phase 88 Track C — delegates to the
    /// shared `kernel_core::fs::ext2::resolve_block`.
    fn resolve_block(&self, inode: &Ext2Inode, logical_block: u32) -> Result<u32, Ext2Error> {
        kernel_core::fs::ext2::resolve_block(self, inode, logical_block)
    }

    // -----------------------------------------------------------------------
    // File data reading (P28-T014)
    // -----------------------------------------------------------------------

    /// Read file data from an inode starting at `offset` into `buf`.
    /// Returns the number of bytes actually read.
    ///
    /// Phase 87 Track B.1 — coalesces runs of physically-contiguous whole blocks
    /// into single multi-block `blk::read_sectors` calls (see
    /// `kernel_core::fs::ext2::read_file_data_coalesced`), collapsing the
    /// per-block kernel↔driver round-trips that dominate large sequential reads
    /// (a 21 MiB package read drops from ~5,376 requests to a small multiple of
    /// its contiguous-run count). Byte-for-byte identical to the prior per-block
    /// loop (host-verified). Whole-block runs read straight into `buf`
    /// (cache-bypassing); the unaligned head / short tail keep the cache-aware
    /// `read_block_into_slice` path; sparse holes are zero-filled with no request.
    pub fn read_file_data(
        &self,
        inode: &Ext2Inode,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Ext2Error> {
        // Phase 88 Track C — delegates to the shared
        // `kernel_core::fs::ext2::read_file_data`, which drives the Phase 87
        // contiguous-run coalescer over this volume's `BlockReader`
        // (`max_run_blocks` = device cap, `read_block_run` = the multi-block
        // device read, `read_block_into` = the cache-aware partial read).
        kernel_core::fs::ext2::read_file_data(self, inode, offset, buf)
    }

    // -----------------------------------------------------------------------
    // Directory operations (P28-T015 through P28-T018)
    // -----------------------------------------------------------------------

    /// Read all directory entries from a directory inode (P28-T015).
    /// Phase 88 Track C — delegates to the shared
    /// `kernel_core::fs::ext2::read_directory_entries`.
    pub fn read_directory_entries(
        &self,
        inode: &Ext2Inode,
    ) -> Result<Vec<(String, u32, u8)>, Ext2Error> {
        kernel_core::fs::ext2::read_directory_entries(self, inode)
    }

    /// Look up a name in a directory inode (P28-T016). Phase 88 Track C —
    /// delegates to the shared `kernel_core::fs::ext2::lookup_in_directory`.
    pub fn lookup_in_directory(&self, dir_inode: &Ext2Inode, name: &str) -> Result<u32, Ext2Error> {
        kernel_core::fs::ext2::lookup_in_directory(self, dir_inode, name)
    }

    /// Resolve an absolute path to an inode number (P28-T017). Phase 88 Track C —
    /// delegates to the shared `kernel_core::fs::ext2::resolve_path`.
    pub fn resolve_path(&self, path: &str) -> Result<u32, Ext2Error> {
        kernel_core::fs::ext2::resolve_path(self, path)
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
        write_sectors_for(self.dev_id, sb_lba, 2, &sb_buf).map_err(|_| Ext2Error::IoError)?;

        // Write BGD table.
        let bgd_block = if self.block_size == 1024 { 2 } else { 1 };
        let bgd_lba = self.base_lba + (bgd_block as u64) * (self.sectors_per_block as u64);
        let bgd_bytes = self.bgd_table.len() * 32;
        let bgd_sectors = bgd_bytes.div_ceil(512);
        let mut bgd_buf = vec![0u8; bgd_sectors * 512];
        for (i, bgd) in self.bgd_table.iter().enumerate() {
            bgd.write_into(&mut bgd_buf[i * 32..(i + 1) * 32]);
        }
        write_sectors_for(self.dev_id, bgd_lba, bgd_sectors, &bgd_buf)
            .map_err(|_| Ext2Error::IoError)
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
        self.remove_directory_entry(&parent_inode, name)?;

        if child_inode.links_count != 0 {
            self.write_inode(child_ino, &child_inode)?;
            return Ok(());
        }

        if crate::process::ext2_inode_open_count(child_ino) != 0 {
            self.write_inode(child_ino, &child_inode)?;
        } else {
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
        ctime: u32,
    ) -> Result<(), Ext2Error> {
        let ino = self.resolve_path(path)?;
        let mut inode = self.read_inode(ino)?;
        inode.uid = uid as u16;
        inode.gid = gid as u16;
        // Preserve the file type bits, only update permission bits.
        inode.mode = (inode.mode & 0xF000) | (mode & 0o7777);
        // POSIX: chmod/chown advance ctime. The routed VFS_SETATTR path
        // (`handle_setattr`) sets ctime too; keep the direct-engine fallback
        // (boot window) consistent so a later stat reports a coherent ctime.
        inode.ctime = ctime;
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
        // Phase 88 Track C — delegate to the shared kernel_core reader so the
        // kernel engine and vfs_server resolve symlinks identically.
        let bytes = kernel_core::fs::ext2::read_symlink_target(self, &inode)?;
        String::from_utf8(bytes).map_err(|_| Ext2Error::CorruptedEntry)
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

pub fn unmount_ext2() {
    *EXT2_VOLUME.lock() = None;
}

/// Flush the kernel ext2 read cache (Phase 88).
///
/// Called by the syscall layer after it routes a mutating ext2 op to the
/// `vfs_server` write authority, so the kernel's own metadata reads
/// (`resolve_path`, exec loader) don't serve a stale cached block.
pub fn invalidate_cache() {
    if let Some(vol) = EXT2_VOLUME.lock().as_ref() {
        vol.invalidate_block_cache();
    }
    // Phase 89: the syscall layer calls this after every ext2 mutation it routes
    // to the `vfs_server` write authority, so it is the natural choke point to
    // also invalidate the kernel path-metadata (stat) cache — a routed write /
    // create / unlink / rename / truncate changes the very stat results that
    // cache holds.
    crate::fs::metacache::bump();
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

/// Truncate and free the ext2 inode when its on-disk `links_count` has
/// reached zero. The caller MUST have verified (under `PROCESS_TABLE`)
/// that no open fd aliases this inode — this function intentionally
/// skips a recount so two cores concurrently closing siblings of the
/// same inode cannot both observe count==0 after each drops its own
/// lock.
///
/// Phase 66 Track D.3 — body relocated from
/// `kernel/src/arch/x86_64/syscall/mod.rs` so the reclamation logic
/// lives next to the volume table it mutates.
pub fn reap_unused_ext2_inode(inode_num: u32) {
    let mut vol = EXT2_VOLUME.lock();
    let Some(vol) = vol.as_mut() else {
        return;
    };
    let Ok(mut inode) = vol.read_inode(inode_num) else {
        return;
    };
    if inode.links_count != 0 {
        return;
    }
    let _ = vol.truncate_file(inode_num, &mut inode);
    let _ = vol.free_inode(inode_num);
}

/// Check if a root-relative ext2 path is a regular file (not directory/symlink).
/// Returns `false` if the volume is not mounted or the path does not exist.
pub fn is_ext2_regular_file(path: &str) -> bool {
    use kernel_core::fs::ext2::{S_IFMT, S_IFREG};
    let vol = EXT2_VOLUME.lock();
    match vol.as_ref() {
        Some(vol) => match vol.metadata(path) {
            Ok((_, _, mode, _, _)) => mode & S_IFMT == S_IFREG,
            Err(_) => false,
        },
        None => false,
    }
}
