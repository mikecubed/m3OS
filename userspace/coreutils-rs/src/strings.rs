//! strings — print printable character sequences from files.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

fn parse_u64(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add((b - b'0') as u64);
    }
    Some(v)
}

fn is_string_char(b: u8) -> bool {
    b == b'\t' || b.is_ascii_graphic() || b == b' '
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

fn scan_fd(fd: i32, min_len: u64) -> bool {
    let mut read_buf = [0u8; 4096];
    // Stack buffer accumulates the current candidate string
    let mut sbuf = [0u8; 256];
    let mut slen: usize = 0;

    loop {
        let n = read(fd, &mut read_buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            return false;
        }
        for &b in &read_buf[..n as usize] {
            if is_string_char(b) {
                if slen < sbuf.len() {
                    sbuf[slen] = b;
                    slen += 1;
                }
                // If buffer full, flush if already over min_len and keep accumulating
                // (treat overflow as a very long string — just flush what we have)
                if slen == sbuf.len() {
                    if (slen as u64) >= min_len {
                        write_all(STDOUT_FILENO, &sbuf[..slen]);
                    }
                    slen = 0;
                }
            } else {
                if (slen as u64) >= min_len {
                    write_all(STDOUT_FILENO, &sbuf[..slen]);
                    write_str(STDOUT_FILENO, "\n");
                }
                slen = 0;
            }
        }
    }
    // Flush any remaining candidate
    if (slen as u64) >= min_len {
        write_all(STDOUT_FILENO, &sbuf[..slen]);
        write_str(STDOUT_FILENO, "\n");
    }
    true
}

fn main(args: &[&str]) -> i32 {
    let mut min_len: u64 = 4;
    let mut argi = 1;

    if argi < args.len() && args[argi] == "-n" {
        argi += 1;
        if argi >= args.len() {
            write_str(STDERR_FILENO, "usage: strings [-n MIN] FILE...\n");
            return 1;
        }
        match parse_u64(args[argi].as_bytes()) {
            Some(v) if v > 0 => min_len = v,
            _ => {
                write_str(STDERR_FILENO, "usage: strings [-n MIN] FILE...\n");
                return 1;
            }
        }
        argi += 1;
    }

    if argi >= args.len() {
        write_str(STDERR_FILENO, "usage: strings [-n MIN] FILE...\n");
        return 1;
    }

    let mut status = 0;
    for file in &args[argi..] {
        let bytes = file.as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "strings: path too long\n");
            status = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "strings: cannot open: ");
            write_str(STDERR_FILENO, file);
            write_str(STDERR_FILENO, "\n");
            status = 1;
            continue;
        }
        if !scan_fd(fd as i32, min_len) {
            write_str(STDERR_FILENO, "strings: read error: ");
            write_str(STDERR_FILENO, file);
            write_str(STDERR_FILENO, "\n");
            status = 1;
        }
        close(fd as i32);
    }
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
