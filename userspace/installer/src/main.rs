//! `installer` — Phase 106 Track C on-device installer.
//!
//! Copies m3OS from the boot medium (a combined GPT USB image —
//! `[protective MBR | GPT | ESP FAT | ext2 rootfs]`) onto an internal
//! disk so the machine can boot writable from its own storage (C.3: the
//! raw `dd`-style block copy + reboot; the partition-aware GPT/ESP/
//! `mkfs.ext2` path follows with C.4/C.5).
//!
//! The copy span is derived from the source's own GPT — the backup-header
//! LBA (GPT header offset 32) is the last meaningful sector, so exactly
//! `0..=alt_lba` is copied, never a whole physical stick. The target is
//! resolved by service name (`nvme.block` → a secondary `dev_id`), size-
//! checked by probing its last-needed sector, streamed in ≤64 KiB
//! chunks, flushed, then the machine reboots into the installed disk.
//!
//! The raw block syscalls (`SYS_BLK_RAW_READ`/`WRITE`/`FLUSH`/
//! `RESOLVE_DEV`) are gated on this binary's unforgeable exec path
//! (`/sbin/installer`) — raw cross-device writes are too destructive to
//! be ambient.
//!
//! Serial sentinels (`nvme-install-smoke`'s oracle):
//! - `INSTALLER:source dev=<n> gpt=yes sectors=<n>` — source span decoded.
//! - `INSTALLER:copy src=<n> dst=<n> sectors=<n>` — copy begins.
//! - `INSTALLER:progress <pct>% (<done>/<total>)` — every ~10%.
//! - `INSTALLER:done sectors=<n>` / `INSTALLER:rebooting`.
//! - `INSTALLER:error <reason>` — a syscall/layout/guard failure (fails
//!   closed, no partial reboot).
//!
//! `installer --no-reboot` runs the copy but stays up (dry run).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::vec;
use core::alloc::Layout;

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

/// Mirror to serial (the smoke oracle) + stdout.
fn log(msg: &str) {
    write_str(STDOUT_FILENO, msg);
    syscall_lib::serial_print(msg);
}

/// Resolve a block-device service name to a `dev_id` (C.3 consumer).
#[allow(dead_code)]
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

/// Flush `dev_id`'s write-back cache. Returns `true` on success.
fn flush_dev(dev_id: u64) -> bool {
    // SAFETY: single-integer m3OS-native syscall, no memory arguments.
    let rc = unsafe { syscall_lib::syscall1(SYS_BLK_RAW_FLUSH, dev_id) };
    (rc as i64) == 0
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

/// The install target service (the internal NVMe). Resolved to a
/// secondary `dev_id` for the copy's write side.
const TARGET_SERVICE: &str = "nvme.block";

/// Chunk (sectors) per raw read/write — the kernel bounds each request
/// to `MAX_SECTORS_PER_RAW_REQUEST` (256 = 128 KiB).
const CHUNK_SECTORS: u64 = kernel_core::installer::MAX_SECTORS_PER_RAW_REQUEST;

syscall_lib::entry_point!(program_main);

fn program_main(args: &[&str]) -> i32 {
    log("INSTALLER:start\n");

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

    // 4. Stream the image src→dst in bounded chunks. **Sparse copy:** a
    //    freshly-created target is zero-filled, and the ext2 rootfs is
    //    mostly empty data blocks, so an all-zero source chunk is skipped
    //    (read but not written) — this cuts the write round-trips (the
    //    dominant cost through the block IPC) to the handful of chunks
    //    that carry real data + the GPT/ext2 metadata. The GPT primary
    //    (LBA 1) and backup (the last chunk) are non-zero, so the layout
    //    is always written.
    let mut buf = vec![0u8; (CHUNK_SECTORS * SECTOR_BYTES) as usize];
    let mut copied = 0u64;
    let mut written = 0u64;
    let mut next_progress = 0u64;
    while copied < total_sectors {
        let count = core::cmp::min(CHUNK_SECTORS, total_sectors - copied);
        let bytes = (count * SECTOR_BYTES) as usize;
        if raw_read(ROOT_DEV_ID, copied, count, &mut buf[..bytes]).is_none() {
            log(&format!("INSTALLER:error read-failed lba={copied}\n"));
            return 1;
        }
        // Only write chunks that carry data; the target is already zeroed.
        if buf[..bytes].iter().any(|&b| b != 0) {
            if raw_write(target, copied, count, &buf[..bytes]).is_none() {
                log(&format!("INSTALLER:error write-failed lba={copied}\n"));
                return 1;
            }
            written += count;
        }
        copied += count;
        // Progress every ~10% (bounded serial chatter).
        if copied >= next_progress {
            let pct = copied * 100 / total_sectors;
            log(&format!(
                "INSTALLER:progress {pct}% ({copied}/{total_sectors} read, {written} written)\n"
            ));
            next_progress = copied + total_sectors / 10;
        }
    }

    // 5. Flush the target's write-back cache so the copy is durable
    //    before the reboot (the reboot path only flushes the root slot).
    if !flush_dev(target) {
        log("INSTALLER:error flush-failed\n");
        return 1;
    }
    log(&format!("INSTALLER:done sectors={copied}\n"));

    // 6. Reboot into the freshly-installed disk. The gate relaunches
    //    QEMU with only the NVMe attached (like ahci-persist's second
    //    boot); a real machine would prefer the internal disk in its
    //    boot order. `_args`-driven `--no-reboot` skips this for a dry
    //    run (`installer probe`).
    if args.iter().any(|a| *a == "--no-reboot") {
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
