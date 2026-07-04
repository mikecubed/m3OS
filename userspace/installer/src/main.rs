//! `installer` — Phase 106 Track C on-device installer.
//!
//! Copies m3OS from the boot medium (a combined GPT USB image —
//! `[protective MBR | GPT | ESP FAT | ext2 rootfs]`) onto an internal
//! disk so the machine can boot writable from its own storage. Two modes:
//!
//! - **raw** (default, C.3): a `dd`-style sector copy of exactly the
//!   source image span (`0..=alt_lba` from the source's own GPT), sparse
//!   (all-zero chunks skipped — the target is blank), then flush + reboot.
//!   Simplest correct path; the installed disk is a byte-clone, so
//!   everything past the image size is wasted.
//! - **partition-aware** (`--part`, C.4 + the C.5 populate arm): parse the
//!   source GPT, probe the target's real capacity, lay a **fresh GPT sized
//!   to the target** (same ESP span; the Linux partition grows to the last
//!   usable LBA), raw-copy the ESP contents (the FAT's geometry is
//!   partition-relative — a same-span copy stays valid), `format_ext2` the
//!   grown rootfs partition, and **populate it file-by-file** from the
//!   source rootfs through the kernel-core reader/writer pair (mode/uid/
//!   gid/timestamps preserved). IO rides a write-back cache + run
//!   coalescer so metadata read-modify-writes and the sequential data
//!   stream don't cost one IPC round-trip per block.
//!
//! The raw block syscalls (`SYS_BLK_RAW_READ`/`WRITE`/`FLUSH`/
//! `RESOLVE_DEV`) are gated on this binary's unforgeable exec path
//! (`/sbin/installer`) — raw cross-device writes are too destructive to
//! be ambient.
//!
//! Serial sentinels (the `nvme-install-smoke` oracle; `part-` prefixed
//! errors come from the partition mode):
//! - `INSTALLER:start` / `INSTALLER:mode raw|part`
//! - `INSTALLER:source dev=<n> gpt=yes sectors=<n>` — source span decoded.
//! - raw: `INSTALLER:copy src=<n> dst=<n> sectors=<n>`,
//!   `INSTALLER:progress <pct>% …`, `INSTALLER:done sectors=<n>`.
//! - part: `INSTALLER:layout esp=<f>..<l> root=<f>..<l>`,
//!   `INSTALLER:target dev=<n> sectors=<n>`,
//!   `INSTALLER:gpt-written root_sectors=<n>`,
//!   `INSTALLER:esp-copied sectors=<n> written=<n>`,
//!   `INSTALLER:format blocks=<n> bs=<n>`,
//!   `INSTALLER:populate dirs=<n> files=<n> symlinks=<n> bytes=<n> skipped=<n>`,
//!   `INSTALLER:done mode=part`.
//! - `INSTALLER:rebooting` / `INSTALLER:error <reason>` (fails closed).
//!
//! `installer --no-reboot` runs the install but stays up (dry run).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

use kernel_core::fs::ext2::{BlockReader, Ext2BlockGroupDescriptor, Ext2Error, Ext2Superblock};
use kernel_core::fs::ext2_format::{BlockIo, Ext2Fs, FormatParams, format_ext2};
use kernel_core::fs::ext2_populate::{WriteBackBlockIo, populate_from_reader};
use kernel_core::fs::gpt::{GptGuids, GptPlan, build_gpt, parse_gpt};
use kernel_core::installer::{
    SECTOR_BYTES, SYS_BLK_RAW_FLUSH, SYS_BLK_RAW_READ, SYS_BLK_RAW_WRITE, SYS_BLK_RESOLVE_DEV,
};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{STDOUT_FILENO, write_str};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "installer: alloc error\n");
    syscall_lib::exit(99)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "installer: PANIC\n");
    syscall_lib::exit(101)
}

/// Boot/root device — never resolved (see `SYS_BLK_RESOLVE_DEV`).
const ROOT_DEV_ID: u64 = 0;

/// The install target service (the internal NVMe). Resolved to a
/// secondary `dev_id` for the copy's write side.
const TARGET_SERVICE: &str = "nvme.block";

/// Chunk (sectors) per raw read/write — the kernel bounds each request
/// to `MAX_SECTORS_PER_RAW_REQUEST` (256 = 128 KiB).
const CHUNK_SECTORS: u64 = kernel_core::installer::MAX_SECTORS_PER_RAW_REQUEST;

/// Mirror to serial (the smoke oracle) + stdout.
fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

/// Resolve a block-device service name to a `dev_id`.
fn resolve_dev(service: &str) -> Option<u32> {
    // SAFETY: m3OS-native syscall; the kernel reads `service.len()` bytes
    // from the pointer and returns a dev_id or a negative errno.
    let rc = unsafe {
        syscall_lib::syscall3(
            SYS_BLK_RESOLVE_DEV,
            service.as_ptr() as u64,
            service.len() as u64,
            0,
        )
    };
    if (rc as i64) < 0 {
        None
    } else {
        u32::try_from(rc).ok()
    }
}

/// Raw-read `count` sectors from `dev_id` into `buf`. Returns the byte
/// count on success, or `None` on any error.
fn raw_read(dev_id: u64, start_lba: u64, count: u64, buf: &mut [u8]) -> Option<usize> {
    // SAFETY: m3OS-native syscall; the kernel writes at most `count *
    // SECTOR_BYTES` bytes into `buf` and returns the count or a negative
    // errno. `buf` is sized by the caller to `count * SECTOR_BYTES`.
    let rc = unsafe {
        syscall_lib::syscall4(
            SYS_BLK_RAW_READ,
            dev_id,
            start_lba,
            count,
            buf.as_mut_ptr() as u64,
        )
    };
    if (rc as i64) < 0 {
        None
    } else {
        Some(rc as usize)
    }
}

/// Raw-write `count` sectors from `buf` to `dev_id`.
fn raw_write(dev_id: u64, start_lba: u64, count: u64, buf: &[u8]) -> Option<usize> {
    // SAFETY: m3OS-native syscall; the kernel reads `count * SECTOR_BYTES`
    // bytes from `buf`. Access-checked against the installer exec path.
    let rc = unsafe {
        syscall_lib::syscall4(
            SYS_BLK_RAW_WRITE,
            dev_id,
            start_lba,
            count,
            buf.as_ptr() as u64,
        )
    };
    if (rc as i64) < 0 {
        None
    } else {
        Some(rc as usize)
    }
}

/// Flush `dev_id`'s write-back cache. Returns `true` on success.
fn flush_dev(dev_id: u64) -> bool {
    // SAFETY: single-integer m3OS-native syscall, no memory arguments.
    let rc = unsafe { syscall_lib::syscall1(SYS_BLK_RAW_FLUSH, dev_id) };
    (rc as i64) == 0
}

/// Read one 512-byte sector (the shape `parse_gpt` consumes).
fn read_sector(dev_id: u64, lba: u64, buf: &mut [u8; 512]) -> bool {
    raw_read(dev_id, lba, 1, buf).is_some()
}

/// Copy `total_sectors` from `src_dev` LBA `src_base` to `dst_dev` LBA
/// `dst_base`, sparse (all-zero chunks are read but not written — the
/// target is zero-filled). Returns `(copied, written)` sector counts, or
/// `None` with the failing LBA logged.
fn sparse_copy(
    src_dev: u64,
    src_base: u64,
    dst_dev: u64,
    dst_base: u64,
    total_sectors: u64,
    progress: bool,
) -> Option<(u64, u64)> {
    let mut buf = vec![0u8; (CHUNK_SECTORS * SECTOR_BYTES) as usize];
    let mut copied = 0u64;
    let mut written = 0u64;
    let mut next_progress = 0u64;
    while copied < total_sectors {
        let count = core::cmp::min(CHUNK_SECTORS, total_sectors - copied);
        let bytes = (count * SECTOR_BYTES) as usize;
        if raw_read(src_dev, src_base + copied, count, &mut buf[..bytes]).is_none() {
            log(&format!(
                "INSTALLER:error read-failed lba={}\n",
                src_base + copied
            ));
            return None;
        }
        if buf[..bytes].iter().any(|&b| b != 0) {
            if raw_write(dst_dev, dst_base + copied, count, &buf[..bytes]).is_none() {
                log(&format!(
                    "INSTALLER:error write-failed lba={}\n",
                    dst_base + copied
                ));
                return None;
            }
            written += count;
        }
        copied += count;
        if progress && copied >= next_progress {
            let pct = copied * 100 / total_sectors;
            log(&format!(
                "INSTALLER:progress {pct}% ({copied}/{total_sectors} read, {written} written)\n"
            ));
            next_progress = copied + total_sectors / 10;
        }
    }
    Some((copied, written))
}

// ---------------------------------------------------------------------------
// Partition-aware mode (C.4 + C.5 populate)
// ---------------------------------------------------------------------------

/// Probe `dev`'s capacity in sectors by LBA bisection: a single-sector read
/// at LBA `x` succeeds iff `x < capacity` (QEMU nvme — and real NVMe via the
/// ring-3 driver — reject out-of-range LBAs), so the capacity is the first
/// unreadable LBA. `known_good` must be a readable LBA (the caller's
/// size-guard probe). ~2×40 single-sector reads worst case.
fn probe_capacity(dev: u64, known_good: u64) -> u64 {
    let mut probe = [0u8; SECTOR_BYTES as usize];
    let mut ok = |lba: u64| raw_read(dev, lba, 1, &mut probe).is_some();
    // Find an unreadable upper bound by doubling (capped far past any real
    // disk: 2^48 sectors = 128 PiB).
    let mut lo = known_good;
    let mut hi = known_good.max(1) << 1;
    while hi < (1u64 << 48) && ok(hi) {
        lo = hi;
        hi <<= 1;
    }
    if hi >= (1u64 << 48) {
        return hi; // absurd device; let the plan's own guards handle it
    }
    // Invariant: ok(lo), !ok(hi) → capacity in (lo, hi].
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if ok(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// xorshift64* over a clock seed — GUID/UUID bytes for the freshly laid
/// GPT + filesystem. Not cryptographic; uniqueness across installs is all
/// that matters here.
struct SeedRng(u64);

impl SeedRng {
    fn from_clock() -> SeedRng {
        let (sec, usec) = syscall_lib::gettimeofday();
        SeedRng((sec as u64).rotate_left(32) ^ (usec as u64) ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// A random RFC-4122-shaped (version 4 / variant 1) GUID in on-disk
    /// mixed-endian byte order.
    fn guid(&mut self) -> [u8; 16] {
        let mut g = [0u8; 16];
        g[0..8].copy_from_slice(&self.next().to_le_bytes());
        g[8..16].copy_from_slice(&self.next().to_le_bytes());
        // Version nibble lives in the high bits of on-disk byte 7 (field 3
        // is little-endian); variant bits in byte 8.
        g[7] = (g[7] & 0x0F) | 0x40;
        g[8] = (g[8] & 0x3F) | 0x80;
        g
    }
}

/// The source rootfs mounted read-only over the raw syscalls (dev 0 at the
/// GPT partition base). Serves the kernel-core `BlockReader` read path; runs
/// coalesce into ≤`CHUNK_SECTORS` raw reads.
struct SourceExt2 {
    base_lba: u64,
    block_size: u32,
    sectors_per_block: u64,
    inodes_per_group: u32,
    inode_size: u32,
    /// Per-group inode-table start block (from the on-disk BGDs — the
    /// source is a foreign mke2fs layout, so the descriptors are authority).
    inode_tables: Vec<u32>,
}

impl SourceExt2 {
    fn mount(base_lba: u64) -> Option<SourceExt2> {
        // Superblock at partition byte 1024 = sectors base+2, base+3.
        let mut sb_bytes = [0u8; 1024];
        raw_read(ROOT_DEV_ID, base_lba + 2, 2, &mut sb_bytes)?;
        let sb = Ext2Superblock::parse(&sb_bytes).ok()?;
        let bs = sb.block_size();
        let spb = (bs / 512) as u64;
        // BGD table starts in the block after the superblock's block.
        let bgd_block = (sb.first_data_block + 1) as u64;
        let group_count = sb.block_group_count();
        let table_bytes = group_count as usize * 32;
        let table_sectors = table_bytes.div_ceil(SECTOR_BYTES as usize) as u64;
        let mut table = vec![0u8; (table_sectors * SECTOR_BYTES) as usize];
        let mut got = 0u64;
        while got < table_sectors {
            let count = core::cmp::min(CHUNK_SECTORS, table_sectors - got);
            let off = (got * SECTOR_BYTES) as usize;
            let len = (count * SECTOR_BYTES) as usize;
            raw_read(
                ROOT_DEV_ID,
                base_lba + bgd_block * spb + got,
                count,
                &mut table[off..off + len],
            )?;
            got += count;
        }
        let bgds =
            Ext2BlockGroupDescriptor::parse_table(&table[..table_bytes], group_count).ok()?;
        Some(SourceExt2 {
            base_lba,
            block_size: bs,
            sectors_per_block: spb,
            inodes_per_group: sb.inodes_per_group,
            inode_size: sb.inode_size as u32,
            inode_tables: bgds.iter().map(|b| b.inode_table).collect(),
        })
    }
}

impl BlockReader for SourceExt2 {
    fn block_size(&self) -> u32 {
        self.block_size
    }
    fn inodes_per_group(&self) -> u32 {
        self.inodes_per_group
    }
    fn inode_size(&self) -> u32 {
        self.inode_size
    }
    fn inode_table_block(&self, group: u32) -> Result<u32, Ext2Error> {
        self.inode_tables
            .get(group as usize)
            .copied()
            .ok_or(Ext2Error::CorruptedEntry)
    }
    fn read_block(&self, block_num: u32) -> Result<Vec<u8>, Ext2Error> {
        let mut buf = vec![0u8; self.block_size as usize];
        let lba = self.base_lba + block_num as u64 * self.sectors_per_block;
        raw_read(ROOT_DEV_ID, lba, self.sectors_per_block, &mut buf)
            .map(|_| buf)
            .ok_or(Ext2Error::IoError)
    }
    fn max_run_blocks(&self) -> u32 {
        (CHUNK_SECTORS / self.sectors_per_block) as u32
    }
    fn read_block_run(
        &self,
        start_block: u32,
        count: u32,
        dst: &mut [u8],
    ) -> Result<(), Ext2Error> {
        let mut done = 0u64;
        let total = count as u64 * self.sectors_per_block;
        let base = self.base_lba + start_block as u64 * self.sectors_per_block;
        while done < total {
            let n = core::cmp::min(CHUNK_SECTORS, total - done);
            let off = (done * SECTOR_BYTES) as usize;
            let len = (n * SECTOR_BYTES) as usize;
            raw_read(ROOT_DEV_ID, base + done, n, &mut dst[off..off + len])
                .ok_or(Ext2Error::IoError)?;
            done += n;
        }
        Ok(())
    }
}

/// The target rootfs partition as a `BlockIo` over the raw syscalls
/// (secondary dev at the freshly planned Linux partition base). Run writes
/// leave as single ≤`CHUNK_SECTORS` raw requests — the whole point of the
/// write-back cache's coalescing.
struct TargetPartIo {
    dev: u64,
    base_lba: u64,
    sectors_per_block: u64,
}

impl BlockIo for TargetPartIo {
    fn read_block(&mut self, block: u32, buf: &mut [u8]) -> Result<(), Ext2Error> {
        let lba = self.base_lba + block as u64 * self.sectors_per_block;
        raw_read(self.dev, lba, self.sectors_per_block, buf)
            .map(|_| ())
            .ok_or(Ext2Error::IoError)
    }
    fn write_block(&mut self, block: u32, data: &[u8]) -> Result<(), Ext2Error> {
        let lba = self.base_lba + block as u64 * self.sectors_per_block;
        raw_write(self.dev, lba, self.sectors_per_block, data)
            .map(|_| ())
            .ok_or(Ext2Error::IoError)
    }
    fn write_block_run(
        &mut self,
        start_block: u32,
        count: u32,
        data: &[u8],
    ) -> Result<(), Ext2Error> {
        let mut done = 0u64;
        let total = count as u64 * self.sectors_per_block;
        let base = self.base_lba + start_block as u64 * self.sectors_per_block;
        while done < total {
            let n = core::cmp::min(CHUNK_SECTORS, total - done);
            let off = (done * SECTOR_BYTES) as usize;
            let len = (n * SECTOR_BYTES) as usize;
            raw_write(self.dev, base + done, n, &data[off..off + len]).ok_or(Ext2Error::IoError)?;
            done += n;
        }
        Ok(())
    }
}

/// LRU cap (blocks) for the populate write-back cache — bounds installer
/// heap at cap × block_size (512 KiB at 4 KiB blocks).
const WB_CACHE_BLOCKS: usize = 128;

/// The C.4 partition-aware install. Fails closed: any error before the
/// first target write leaves the target untouched; a failure mid-install
/// leaves a partial target that the next run re-formats (never booted —
/// boot order still prefers the intact source medium).
fn install_part(no_reboot: bool) -> i32 {
    // 1. Decode the source layout (CRC-verified — a corrupt source must
    //    abort, not propagate a bogus layout onto the target).
    let mut read0 = |lba: u64, buf: &mut [u8; 512]| read_sector(ROOT_DEV_ID, lba, buf);
    let Some(src) = parse_gpt(&mut read0) else {
        log("INSTALLER:error part-source-gpt-invalid\n");
        return 1;
    };
    let (Some(src_esp), Some(src_linux)) = (src.esp, src.linux) else {
        log("INSTALLER:error part-source-partitions-missing\n");
        return 1;
    };
    let source_total = src.alt_lba + 1;
    log(&format!(
        "INSTALLER:source dev={ROOT_DEV_ID} gpt=yes sectors={source_total}\n"
    ));
    log(&format!(
        "INSTALLER:layout esp={}..{} root={}..{}\n",
        src_esp.first_lba, src_esp.last_lba, src_linux.first_lba, src_linux.last_lba
    ));

    // 2. Mount the source rootfs read-only (before touching the target —
    //    an unreadable source must abort with the target still blank).
    let Some(src_fs) = SourceExt2::mount(src_linux.first_lba) else {
        log("INSTALLER:error part-source-ext2-unreadable\n");
        return 1;
    };

    // 3. Resolve the target; never install onto the boot medium.
    let target = match resolve_dev(TARGET_SERVICE) {
        Some(d) if u64::from(d) != ROOT_DEV_ID => u64::from(d),
        Some(_) => {
            log("INSTALLER:error target-is-source\n");
            return 1;
        }
        None => {
            log(&format!(
                "INSTALLER:error target-resolve-failed svc={TARGET_SERVICE}\n"
            ));
            return 1;
        }
    };

    // 4. Size-guard + capacity probe. The conservative floor is the source
    //    image span (mirrors raw mode; the real minimum is the copied
    //    content, but a target smaller than the source image is not an
    //    install anyone asked for).
    let mut probe = [0u8; SECTOR_BYTES as usize];
    if raw_read(target, src.alt_lba, 1, &mut probe).is_none() {
        log(&format!(
            "INSTALLER:error target-too-small need_sectors={source_total}\n"
        ));
        return 1;
    }
    let capacity = probe_capacity(target, src.alt_lba);
    log(&format!(
        "INSTALLER:target dev={target} sectors={capacity}\n"
    ));

    // 5. Plan + write the fresh GPT.
    let plan = match GptPlan::for_target(capacity, src_esp, src_linux.first_lba) {
        Ok(p) => p,
        Err(e) => {
            log(&format!("INSTALLER:error part-plan-failed {e:?}\n"));
            return 1;
        }
    };
    let mut rng = SeedRng::from_clock();
    let guids = GptGuids {
        disk: rng.guid(),
        esp: rng.guid(),
        linux: rng.guid(),
    };
    let image = match build_gpt(&plan, &guids) {
        Ok(i) => i,
        Err(e) => {
            log(&format!("INSTALLER:error part-gpt-build-failed {e:?}\n"));
            return 1;
        }
    };
    for (lba, sector) in image.sector_writes() {
        if raw_write(target, lba, 1, sector).is_none() {
            log(&format!(
                "INSTALLER:error part-gpt-write-failed lba={lba}\n"
            ));
            return 1;
        }
    }
    log(&format!(
        "INSTALLER:gpt-written root_sectors={}\n",
        plan.linux.sectors()
    ));

    // 6. ESP contents: raw same-span copy (partition-relative FAT geometry
    //    and the `hidden sectors` = start LBA both carry over unchanged).
    let esp_sectors = src_esp.sectors();
    let Some((copied, written)) = sparse_copy(
        ROOT_DEV_ID,
        src_esp.first_lba,
        target,
        src_esp.first_lba,
        esp_sectors,
        false,
    ) else {
        return 1; // sparse_copy logged the failing LBA
    };
    log(&format!(
        "INSTALLER:esp-copied sectors={copied} written={written}\n"
    ));

    // 7. Format the grown rootfs partition (same block size the source
    //    mounts with — the geometry the kernel's root mount is proven on).
    let bs = src_fs.block_size;
    let spb = (bs / 512) as u64;
    let block_size_log = match bs {
        1024 => 0,
        2048 => 1,
        4096 => 2,
        _ => {
            log(&format!("INSTALLER:error part-bad-block-size bs={bs}\n"));
            return 1;
        }
    };
    let total_blocks_u64 = plan.linux.sectors() / spb;
    let total_blocks = u32::try_from(total_blocks_u64).unwrap_or(u32::MAX);
    let mut part_io = TargetPartIo {
        dev: target,
        base_lba: plan.linux.first_lba,
        sectors_per_block: spb,
    };
    let mut wb = WriteBackBlockIo::new(
        &mut part_io,
        bs as usize,
        WB_CACHE_BLOCKS,
        (CHUNK_SECTORS / spb) as usize,
    );
    let params = FormatParams {
        total_blocks,
        block_size_log,
        uuid: rng.guid(),
    };
    if let Err(e) = format_ext2(&mut wb, &params) {
        log(&format!("INSTALLER:error part-format-failed {e:?}\n"));
        return 1;
    }
    log(&format!("INSTALLER:format blocks={total_blocks} bs={bs}\n"));

    // 8. Populate: walk the source tree, re-create it on the target.
    let mut fs = match Ext2Fs::open(&mut wb, block_size_log) {
        Ok(f) => f,
        Err(e) => {
            log(&format!("INSTALLER:error part-open-failed {e:?}\n"));
            return 1;
        }
    };
    let stats = match populate_from_reader(&src_fs, &mut fs, &mut wb) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("INSTALLER:error part-populate-failed {e:?}\n"));
            return 1;
        }
    };
    if let Err(e) = fs.flush(&mut wb).and_then(|_| wb.flush()) {
        log(&format!("INSTALLER:error part-flush-failed {e:?}\n"));
        return 1;
    }
    log(&format!(
        "INSTALLER:populate dirs={} files={} symlinks={} bytes={} skipped={}\n",
        stats.dirs, stats.files, stats.symlinks, stats.bytes, stats.skipped
    ));

    // 9. Durability, then hand over to the reboot.
    if !flush_dev(target) {
        log("INSTALLER:error flush-failed\n");
        return 1;
    }
    log("INSTALLER:done mode=part\n");
    finish_reboot(no_reboot)
}

// ---------------------------------------------------------------------------
// Raw mode (C.3)
// ---------------------------------------------------------------------------

/// The C.3 raw `dd`-style image clone.
fn install_raw(no_reboot: bool) -> i32 {
    // 1. Determine the source span from the boot device's own GPT: the
    //    backup-header LBA (GPT header offset 32) is the last meaningful
    //    sector, so the image occupies sectors 0..=alt_lba. This copies
    //    exactly the combined image, not a whole physical stick.
    let mut lba0 = vec![0u8; SECTOR_BYTES as usize];
    if raw_read(ROOT_DEV_ID, 0, 1, &mut lba0).is_none() {
        log("INSTALLER:error source-lba0-read-failed\n");
        return 1;
    }
    let is_gpt = lba0[510] == 0x55 && lba0[511] == 0xAA && lba0[450] == 0xEE;
    if !is_gpt {
        log("INSTALLER:error source-not-gpt\n");
        return 1;
    }
    let mut hdr = vec![0u8; SECTOR_BYTES as usize];
    if raw_read(ROOT_DEV_ID, 1, 1, &mut hdr).is_none() || &hdr[0..8] != b"EFI PART" {
        log("INSTALLER:error source-gpt-header-invalid\n");
        return 1;
    }
    let alt_lba = u64::from_le_bytes(hdr[32..40].try_into().unwrap_or([0; 8]));
    if alt_lba == 0 {
        log("INSTALLER:error source-backup-gpt-lba-zero\n");
        return 1;
    }
    // Sectors 0..=alt_lba, inclusive of the backup GPT header.
    let total_sectors = alt_lba + 1;
    log(&format!(
        "INSTALLER:source dev={ROOT_DEV_ID} gpt=yes sectors={total_sectors}\n"
    ));

    // 2. Resolve the target (internal NVMe) to a secondary dev_id.
    let target = match resolve_dev(TARGET_SERVICE) {
        Some(d) if u64::from(d) != ROOT_DEV_ID => u64::from(d),
        Some(_) => {
            // Same device as the boot medium — never copy onto ourselves.
            log("INSTALLER:error target-is-source\n");
            return 1;
        }
        None => {
            log(&format!(
                "INSTALLER:error target-resolve-failed svc={TARGET_SERVICE}\n"
            ));
            return 1;
        }
    };

    // 3. Size guard: probe the target at the source's last sector. A read
    //    that fails means the target is smaller than the image → abort
    //    non-destructively (no partial write). QEMU nvme rejects an
    //    out-of-range LBA, so this is a real capacity check without a
    //    dedicated capacity syscall.
    let mut probe = vec![0u8; SECTOR_BYTES as usize];
    if raw_read(target, alt_lba, 1, &mut probe).is_none() {
        log(&format!(
            "INSTALLER:error target-too-small need_sectors={total_sectors}\n"
        ));
        return 1;
    }

    log(&format!(
        "INSTALLER:copy src={ROOT_DEV_ID} dst={target} sectors={total_sectors}\n"
    ));

    // 4. Stream the image src→dst in bounded chunks, sparse (see
    //    `sparse_copy` — all-zero source chunks are skipped; the GPT
    //    primary and backup are non-zero, so the layout is always written).
    let Some((copied, _written)) = sparse_copy(ROOT_DEV_ID, 0, target, 0, total_sectors, true)
    else {
        return 1;
    };

    // 5. Flush the target's write-back cache so the copy is durable
    //    before the reboot (the reboot path only flushes the root slot).
    if !flush_dev(target) {
        log("INSTALLER:error flush-failed\n");
        return 1;
    }
    log(&format!("INSTALLER:done sectors={copied}\n"));
    finish_reboot(no_reboot)
}

/// Reboot into the freshly-installed disk (the gate relaunches QEMU with
/// only the NVMe attached; a real machine would prefer the internal disk
/// in its boot order), or stay up under `--no-reboot`.
fn finish_reboot(no_reboot: bool) -> i32 {
    if no_reboot {
        log("INSTALLER:no-reboot (dry run)\n");
        return 0;
    }
    log("INSTALLER:rebooting\n");
    // The installer runs as root (launched by the login shell / init).
    syscall_lib::reboot(syscall_lib::REBOOT_CMD_RESTART);
    // reboot does not return on success.
    log("INSTALLER:error reboot-returned\n");
    1
}

syscall_lib::entry_point!(program_main);

fn program_main(args: &[&str]) -> i32 {
    log("INSTALLER:start\n");
    let no_reboot = args.iter().any(|a| *a == "--no-reboot");
    let part = args.iter().any(|a| *a == "--part");
    if part {
        log("INSTALLER:mode part\n");
        install_part(no_reboot)
    } else {
        log("INSTALLER:mode raw\n");
        install_raw(no_reboot)
    }
}
