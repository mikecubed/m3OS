//! Phase 92a D.4 / I.1 — `usb-mount-smoke`.
//!
//! Falsifiable end-to-end gate for the USB mass-storage **mount** path: the
//! ring-3 `usb-storage` daemon registers a `usb0.block` backend in the kernel
//! `blk::remote` multi-device registry; this binary mounts that backend at
//! `/mnt/usb0` (a bare ext2 image) and exercises the secondary-mount routing
//! (Phase 92a D.4) over the real `read_sectors_dev`/`write_sectors_dev` IPC:
//!
//! - `USB_MASS_STORAGE:mounted` — `mount("/dev/usb0", "/mnt/usb0", "ext2")`
//!   succeeds (retried while the daemon comes up + enters its block-server loop).
//! - `USB_MOUNT:ls-ok` — `getdents64("/mnt/usb0")` lists the image's seeded
//!   `hello.txt` (the secondary volume's directory, not the empty ramdisk
//!   mount-point dir it shadows).
//! - `USB_MOUNT:read-ok` — reading `/mnt/usb0/hello.txt` returns the seeded
//!   content (a real BOT READ(10) through the daemon).
//! - `USB_MOUNT:rw-ok` — overwriting `/mnt/usb0/hello.txt` and reading it back
//!   in a fresh open returns the new bytes byte-identical (BOT WRITE(10) +
//!   READ(10) round-trip through the daemon).
//! - `USB_MOUNT:done` — clean run.
//!
//! A failure prints `USB_MOUNT:<case>:FAIL` and exits non-zero; a panic prints
//! `USB_MOUNT:panic`.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

use syscall_lib::{
    O_DIRECTORY, O_RDONLY, O_TRUNC, O_WRONLY, STDOUT_FILENO, close, getdents64, mount,
    nanosleep_for, open, read, write, write_str,
};

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "USB_MOUNT:panic\n");
    syscall_lib::exit(101)
}

/// Seeded file name + content the host writes into the bare ext2 image.
const HELLO_PATH: &[u8] = b"/mnt/usb0/hello.txt\0";
const HELLO_NAME: &str = "hello.txt";
const SEED_CONTENT: &[u8] = b"m3os-usb-mount-ok\n";
/// Overwrite payload — same length as the seed so the overwrite stays within
/// the file's already-allocated block (no block allocation needed).
const RW_CONTENT: &[u8] = b"m3os-usb-rw-pass!\n";

fn fail(case: &str) -> ! {
    write_str(STDOUT_FILENO, "USB_MOUNT:");
    write_str(STDOUT_FILENO, case);
    write_str(STDOUT_FILENO, ":FAIL\n");
    syscall_lib::exit(2)
}

/// Mount `/dev/usb0` at `/mnt/usb0`, retrying while the daemon registers its
/// `usb0.block` backend and enters its block-server loop (the kernel mount
/// reads the superblock over IPC, which needs the daemon already serving).
fn mount_with_retry() {
    let src = b"/dev/usb0\0";
    let target = b"/mnt/usb0\0";
    let fstype = b"ext2\0";
    for _ in 0..120 {
        let rc = mount(src.as_ptr(), target.as_ptr(), fstype.as_ptr());
        if rc == 0 {
            write_str(STDOUT_FILENO, "USB_MASS_STORAGE:mounted\n");
            return;
        }
        // ENODEV (backend not yet registered) / EIO (daemon not serving yet) —
        // wait and retry. ~250 ms × 120 = 30 s budget.
        let _ = nanosleep_for(0, 250_000_000);
    }
    fail("mount");
}

/// Scan a `/mnt/usb0` directory listing for `hello.txt`.
fn ls_contains_hello() -> bool {
    let fd = open(b"/mnt/usb0\0", O_RDONLY | O_DIRECTORY, 0);
    if fd < 0 {
        return false;
    }
    let fd = fd as i32;
    let mut buf = [0u8; 4096];
    let mut found = false;
    loop {
        let n = getdents64(fd, &mut buf);
        if n <= 0 {
            break;
        }
        let n = n as usize;
        let mut off = 0usize;
        while off + 19 <= n {
            // struct linux_dirent64: d_ino(8) d_off(8) d_reclen(2) d_type(1) d_name[]
            let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
            if reclen == 0 || off + reclen > n {
                break;
            }
            // d_name starts at offset 19, NUL-terminated.
            let name_start = off + 19;
            let mut name_end = name_start;
            while name_end < off + reclen && buf[name_end] != 0 {
                name_end += 1;
            }
            if let Ok(name) = core::str::from_utf8(&buf[name_start..name_end])
                && name == HELLO_NAME
            {
                found = true;
            }
            off += reclen;
        }
    }
    let _ = close(fd);
    found
}

/// Read the whole of `/mnt/usb0/hello.txt` into `out`, returning the byte count.
fn read_hello(out: &mut [u8]) -> Option<usize> {
    let fd = open(HELLO_PATH, O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    let fd = fd as i32;
    let mut total = 0usize;
    loop {
        if total >= out.len() {
            break;
        }
        let n = read(fd, &mut out[total..]);
        if n < 0 {
            let _ = close(fd);
            return None;
        }
        if n == 0 {
            break;
        }
        total += n as usize;
    }
    let _ = close(fd);
    Some(total)
}

#[cfg(not(test))]
fn usb_mount_smoke_main() -> ! {
    write_str(STDOUT_FILENO, "usb-mount-smoke: start\n");

    // 1. Mount the USB stick.
    mount_with_retry();

    // 2. ls /mnt/usb0 must list the seeded file (proves getdents routing to the
    //    secondary volume, shadowing the empty ramdisk mount-point dir).
    if ls_contains_hello() {
        write_str(STDOUT_FILENO, "USB_MOUNT:ls-ok\n");
    } else {
        fail("ls");
    }

    // 3. Read it back — must match the seeded content (real BOT READ(10)).
    let mut buf = [0u8; 64];
    match read_hello(&mut buf) {
        Some(n) if &buf[..n] == SEED_CONTENT => {
            write_str(STDOUT_FILENO, "USB_MOUNT:read-ok\n");
        }
        _ => fail("read"),
    }

    // 4. Overwrite + read back in a fresh open — byte-identical (WRITE(10)).
    {
        let fd = open(HELLO_PATH, O_WRONLY | O_TRUNC, 0);
        if fd < 0 {
            fail("rw-open");
        }
        let fd = fd as i32;
        let w = write(fd, RW_CONTENT);
        let _ = close(fd);
        if w != RW_CONTENT.len() as isize {
            fail("rw-write");
        }
    }
    let mut buf2 = [0u8; 64];
    match read_hello(&mut buf2) {
        Some(n) if &buf2[..n] == RW_CONTENT => {
            write_str(STDOUT_FILENO, "USB_MOUNT:rw-ok\n");
        }
        _ => fail("rw-verify"),
    }

    write_str(STDOUT_FILENO, "USB_MOUNT:done\n");
    syscall_lib::exit(0)
}

// Naked `_start` trampoline (this binary ignores argv/envp), mirroring
// `pku-smoke`. Keeps the gate alloc-free — all buffers are on the stack.
#[cfg(not(test))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym usb_mount_smoke_main,
    );
}

#[cfg(test)]
fn main() {}
