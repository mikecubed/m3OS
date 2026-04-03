//! hexdump — display file contents in hexadecimal and ASCII.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

const HEX: &[u8; 16] = b"0123456789abcdef";

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

/// Write `offset` as an 8-digit zero-padded lowercase hex number.
fn write_offset(fd: i32, mut n: u64) {
    let mut buf = [b'0'; 8];
    let mut i = 8usize;
    while i > 0 {
        i -= 1;
        buf[i] = HEX[(n & 0xf) as usize];
        n >>= 4;
    }
    write_all(fd, &buf);
}

/// Write a single byte as two hex digits followed by a space.
fn write_hex_byte(fd: i32, b: u8) {
    let pair = [HEX[(b >> 4) as usize], HEX[(b & 0xf) as usize], b' '];
    write_all(fd, &pair);
}

fn dump_fd(fd: i32, limit: i64) -> bool {
    let mut buf = [0u8; 16];
    let mut offset: u64 = 0;
    let mut remaining = limit;

    loop {
        let want: usize = if remaining >= 0 {
            let r = remaining as usize;
            if r == 0 {
                break;
            }
            r.min(16)
        } else {
            16
        };

        let n = read(fd, &mut buf[..want]);
        if n == 0 {
            break;
        }
        if n < 0 {
            return false;
        }
        let got = n as usize;

        // Offset column
        write_offset(STDOUT_FILENO, offset);
        write_str(STDOUT_FILENO, "  ");

        // Hex bytes (two groups of 8)
        for (i, &byte) in buf.iter().enumerate().take(16) {
            if i < got {
                write_hex_byte(STDOUT_FILENO, byte);
            } else {
                write_str(STDOUT_FILENO, "   ");
            }
            if i == 7 {
                write_str(STDOUT_FILENO, " ");
            }
        }

        // ASCII column
        write_str(STDOUT_FILENO, " |");
        for &c in &buf[..got] {
            let printable = [if c.is_ascii_graphic() || c == b' ' {
                c
            } else {
                b'.'
            }];
            write_all(STDOUT_FILENO, &printable);
        }
        // Pad remaining chars in ASCII column
        for _ in got..16 {
            write_str(STDOUT_FILENO, " ");
        }
        write_str(STDOUT_FILENO, "|\n");

        offset += got as u64;
        if remaining >= 0 {
            remaining -= got as i64;
        }
    }
    true
}

fn main(args: &[&str]) -> i32 {
    let mut argi = 1;
    let mut limit: i64 = -1;

    while argi < args.len() {
        let arg = args[argi].as_bytes();
        if arg == b"-C" {
            // -C (canonical) is the default and only mode — accept silently
            argi += 1;
            continue;
        }
        if arg == b"-n" {
            argi += 1;
            if argi >= args.len() {
                write_str(STDERR_FILENO, "usage: hexdump [-C] [-n BYTES] [file...]\n");
                return 1;
            }
            match parse_u64(args[argi].as_bytes()) {
                Some(v) => limit = v as i64,
                None => {
                    write_str(STDERR_FILENO, "usage: hexdump [-C] [-n BYTES] [file...]\n");
                    return 1;
                }
            }
            argi += 1;
            continue;
        }
        if !arg.is_empty() && arg[0] == b'-' {
            write_str(STDERR_FILENO, "usage: hexdump [-C] [-n BYTES] [file...]\n");
            return 1;
        }
        break;
    }

    if argi >= args.len() {
        return if dump_fd(0, limit) { 0 } else { 1 };
    }

    let mut status = 0;
    for file in &args[argi..] {
        let bytes = file.as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "hexdump: path too long\n");
            status = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "hexdump: cannot open: ");
            write_str(STDERR_FILENO, file);
            write_str(STDERR_FILENO, "\n");
            status = 1;
            continue;
        }
        if !dump_fd(fd as i32, limit) {
            write_str(STDERR_FILENO, "hexdump: read error on: ");
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
