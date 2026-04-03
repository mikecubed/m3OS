//! chmod — change file mode bits.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, chmod, write_str};

syscall_lib::entry_point!(main);

/// Parse an octal mode string (e.g. "755"). Returns `None` on invalid input.
fn parse_octal(s: &str) -> Option<u16> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut val: u32 = 0;
    for &b in bytes {
        if !matches!(b, b'0'..=b'7') {
            return None;
        }
        val = val * 8 + (b - b'0') as u32;
        if val > 0o7777 {
            return None;
        }
    }
    Some(val as u16)
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_str(STDERR_FILENO, "usage: chmod MODE FILE...\n");
        return 1;
    }
    let mode = match parse_octal(args[1]) {
        Some(m) => m,
        None => {
            write_str(STDERR_FILENO, "chmod: invalid mode\n");
            return 1;
        }
    };
    let mut status = 0i32;
    for arg in &args[2..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "chmod: path too long\n");
            status = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        if chmod(&path[..=bytes.len()], mode) != 0 {
            write_str(STDERR_FILENO, "chmod: cannot change '");
            write_str(STDERR_FILENO, arg);
            write_str(STDERR_FILENO, "'\n");
            status = 1;
        }
    }
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
