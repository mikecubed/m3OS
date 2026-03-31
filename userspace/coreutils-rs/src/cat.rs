//! cat — concatenate files to stdout.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() <= 1 {
        return if cat_fd(0) { 0 } else { 1 };
    }
    let mut ret = 0;
    for arg in &args[1..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "cat: path too long\n");
            ret = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "cat: cannot open file\n");
            ret = 1;
            continue;
        }
        if !cat_fd(fd as i32) {
            ret = 1;
        }
        close(fd as i32);
    }
    ret
}

/// Returns true on success, false on I/O error.
fn cat_fd(fd: i32) -> bool {
    let mut buf = [0u8; 4096];
    loop {
        let n = read(fd, &mut buf);
        if n == 0 {
            return true;
        }
        if n < 0 {
            write_str(STDERR_FILENO, "cat: read error\n");
            return false;
        }
        if !write_all(STDOUT_FILENO, &buf[..n as usize]) {
            write_str(STDERR_FILENO, "cat: write error\n");
            return false;
        }
    }
}

fn write_all(fd: i32, data: &[u8]) -> bool {
    let mut off = 0;
    while off < data.len() {
        let w = write(fd, &data[off..]);
        if w <= 0 {
            return false;
        }
        off += w as usize;
    }
    true
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
