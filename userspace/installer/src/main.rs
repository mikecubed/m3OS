//! `installer` — Phase 106 Track C on-device installer.
//!
//! Copies m3OS from the boot medium (a combined GPT USB image —
//! `[protective MBR | GPT | ESP FAT | ext2 rootfs]`) onto an internal
//! disk so the machine can boot writable from its own storage. This
//! slice (C.1/C.2) lands the crate, the capability-gated raw block
//! syscalls, and a **source-probe** dry run; the streaming raw copy
//! (`dd_copy`) + reboot land with C.3, the partition-aware GPT/ESP/
//! `mkfs.ext2` path with C.4/C.5.
//!
//! The raw block syscalls (`SYS_BLK_RAW_READ`/`WRITE`/`RESOLVE_DEV`) are
//! gated on this binary's unforgeable exec path (`/sbin/installer`) —
//! raw cross-device writes are too destructive to be ambient.
//!
//! Serial sentinels:
//! - `INSTALLER:probe dev=<n> mbr=<sig> gpt=<yes|no> last_lba=<n>` — the
//!   boot device was read raw and its partition layout decoded.
//! - `INSTALLER:error <reason>` — a syscall or layout error.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::vec;
use core::alloc::Layout;

use kernel_core::installer::{
    SECTOR_BYTES, SYS_BLK_RAW_READ, SYS_BLK_RAW_WRITE, SYS_BLK_RESOLVE_DEV,
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

/// Raw-write `count` sectors from `buf` to `dev_id` (C.3 consumer).
#[allow(dead_code)]
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

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    // C.1/C.2 dry run: read the boot device's first two sectors raw and
    // decode the partition layout, proving the gated read syscall works
    // end to end. C.3 replaces this with the streaming copy.
    let mut lba0 = vec![0u8; SECTOR_BYTES as usize];
    if raw_read(ROOT_DEV_ID, 0, 1, &mut lba0).is_none() {
        log("INSTALLER:error raw-read-lba0-failed\n");
        return 1;
    }

    let mbr_sig = lba0[510] == 0x55 && lba0[511] == 0xAA;
    // GPT: protective-MBR partition-1 type (offset 446+4 = 450) is 0xEE.
    let is_gpt = mbr_sig && lba0[450] == 0xEE;

    // For a GPT disk, the backup-header LBA (GPT header offset 32) is the
    // last meaningful sector — exactly what a raw image copy must span.
    let last_lba = if is_gpt {
        let mut hdr = vec![0u8; SECTOR_BYTES as usize];
        if raw_read(ROOT_DEV_ID, 1, 1, &mut hdr).is_some() && &hdr[0..8] == b"EFI PART" {
            u64::from_le_bytes(hdr[32..40].try_into().unwrap_or([0; 8]))
        } else {
            0
        }
    } else {
        0
    };

    log(&format!(
        "INSTALLER:probe dev={ROOT_DEV_ID} mbr={} gpt={} last_lba={last_lba}\n",
        if mbr_sig { "ok" } else { "none" },
        if is_gpt { "yes" } else { "no" },
    ));
    0
}
