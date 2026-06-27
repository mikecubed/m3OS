//! Phase 96 — `usb-logsink`: persist the kernel log to a USB drive so a
//! bare-metal boot can be debugged *after the fact* from the host.
//!
//! On the Phase 96 Tiger Lake laptop there is no serial port and the
//! framebuffer scrolls, so the kernel/driver log (`[mm]`/`[net]`/`ure:`/xHCI …)
//! is effectively unreadable live. The kernel already mirrors every `log::*`
//! line into a bounded (256 KiB) dmesg ring exposed at `/proc/kmsg`; this daemon mounts the
//! USB log volume and snapshots that ring to `/mnt/usb0/boot.log` periodically,
//! so the user can pull the stick, mount the ext2 partition on their host, and
//! read the whole boot.
//!
//! It is deliberately a *separate* process from `usb-storage`: the mount's
//! superblock read is served over `usb0.block` by the `usb-storage` daemon's
//! block-server loop, so `usb-storage` cannot mount its own device (it would
//! block in the mount waiting for itself). This daemon runs alongside it.
//!
//! The mount is partition-aware in the kernel (`usb_ext2_base_lba`): the boot
//! stick is `[ESP FAT] + [ext2 logs]`, so `/dev/usb0` resolves to the ext2
//! partition wherever the GPT/MBR places it; a plain whole-disk ext2 stick works
//! too (`base_lba = 0`).

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), feature(alloc_error_handler))]

extern crate alloc;

use alloc::vec;
use core::alloc::Layout;

use syscall_lib::heap::BrkAllocator;
use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDOUT_FILENO, close, fsync, ipc_wait_service, mkdir,
    mount, nanosleep_for, open, read, write, write_str,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[cfg(not(test))]
#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "usb-logsink: alloc error\n");
    syscall_lib::exit(99)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "usb-logsink: PANIC\n");
    syscall_lib::exit(101)
}

#[cfg(not(test))]
syscall_lib::entry_point!(program_main);

/// The block backend the `usb-storage` daemon registers for the first stick.
const USB_BLOCK_SERVICE: &str = "usb0.block";
const DEV_USB0: &[u8] = b"/dev/usb0\0";
const MNT: &[u8] = b"/mnt\0";
const MNT_USB0: &[u8] = b"/mnt/usb0\0";
const FSTYPE_EXT2: &[u8] = b"ext2\0";
const LOG_PATH: &[u8] = b"/mnt/usb0/boot.log\0";
const KMSG_PATH: &[u8] = b"/proc/kmsg\0";

/// Snapshot the full `/proc/kmsg` ring into `/mnt/usb0/boot.log`. Returns the
/// byte count written, or a negative value on failure (open/read/write error).
///
/// The whole (bounded, 256 KiB) ring is read into memory FIRST, and `boot.log`
/// is only opened with `O_TRUNC` and rewritten once the complete snapshot is in
/// hand. So a `/proc/kmsg` read failure — the dominant failure mode on a flaky
/// bare-metal USB/VFS — leaves the previous, known-good `boot.log` untouched
/// instead of truncating it to a partial file. (A fully atomic
/// temp-then-rename replace isn't available here: `rename()` is not routed for
/// `/mnt/usbN` mounts — it returns `EROFS` — so the residual exposure is a
/// write failure during the final contiguous rewrite, which is surfaced as a
/// negative errno rather than mistaken for success.)
fn snapshot_kmsg(buf: &mut [u8]) -> isize {
    let kfd = open(KMSG_PATH, O_RDONLY, 0);
    if kfd < 0 {
        return kfd;
    }
    // 1. Read the entire ring into memory before touching boot.log, so a read
    //    error leaves the previous snapshot intact (boot.log is not opened yet).
    let mut snapshot: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    loop {
        let n = read(kfd as i32, buf);
        if n < 0 {
            close(kfd as i32);
            return n; // boot.log untouched — previous snapshot preserved.
        }
        if n == 0 {
            break; // EOF — the full ring has been captured.
        }
        snapshot.extend_from_slice(&buf[..n as usize]);
    }
    close(kfd as i32);

    // 2. Only now overwrite boot.log with the complete in-memory snapshot.
    let lfd = open(LOG_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0o644);
    if lfd < 0 {
        return lfd;
    }
    let mut off = 0usize;
    while off < snapshot.len() {
        let w = write(lfd as i32, &snapshot[off..]);
        if w <= 0 {
            // Write failure: surface a negative errno so the caller does not
            // treat a truncated boot.log as a successful snapshot.
            let _ = fsync(lfd as i32);
            close(lfd as i32);
            return if w < 0 {
                w
            } else {
                -5 /* EIO */
            };
        }
        off += w as usize;
    }
    let _ = fsync(lfd as i32);
    close(lfd as i32);
    snapshot.len() as isize
}

fn program_main(_args: &[&str]) -> i32 {
    write_str(STDOUT_FILENO, "usb-logsink: starting\n");

    // 1. Wait for the usb-storage daemon to enumerate the stick and register
    //    `usb0.block` (it polls for the device for up to ~30 s).
    if !ipc_wait_service(USB_BLOCK_SERVICE, 35_000) {
        write_str(
            STDOUT_FILENO,
            "usb-logsink: usb0.block never appeared — no log volume, exiting\n",
        );
        return 0;
    }

    // 2. Ensure the mount point exists.
    let _ = mkdir(MNT, 0o755);
    let _ = mkdir(MNT_USB0, 0o755);

    // 3. Mount the ext2 log partition. Retry — the usb-storage block-server loop
    //    must be servicing requests before the mount's superblock read succeeds,
    //    and on a GPT boot stick the kernel probes the partition table first.
    let mut mounted = false;
    for _ in 0..40 {
        if mount(DEV_USB0.as_ptr(), MNT_USB0.as_ptr(), FSTYPE_EXT2.as_ptr()) == 0 {
            mounted = true;
            break;
        }
        let _ = nanosleep_for(0, 250_000_000); // 250 ms
    }
    if !mounted {
        write_str(
            STDOUT_FILENO,
            "usb-logsink: could not mount /mnt/usb0 (no ext2 log partition?) — exiting\n",
        );
        return 0;
    }
    write_str(
        STDOUT_FILENO,
        "usb-logsink: /mnt/usb0 mounted — persisting kernel log to /mnt/usb0/boot.log\n",
    );

    // 4. Periodically snapshot the dmesg ring to the drive. Overwrite each time
    //    (the ring is the source of truth and is bounded), fsync so a power-off
    //    or freeze still leaves the latest snapshot on disk.
    let mut buf = vec![0u8; 8192];
    let mut announced = false;
    loop {
        let n = snapshot_kmsg(&mut buf);
        if n > 0 && !announced {
            announced = true;
            write_str(STDOUT_FILENO, "usb-logsink: boot.log written\n");
        }
        let _ = nanosleep_for(3, 0); // 3 s between snapshots
    }
}
