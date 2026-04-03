//! head — output the first N lines of each file.
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

/// Output first `count` lines from `fd`.  Returns true on success.
fn head_fd(fd: i32, count: u64) -> bool {
    let mut buf = [0u8; 4096];
    let mut lines: u64 = 0;
    loop {
        if lines >= count {
            return true;
        }
        let n = read(fd, &mut buf);
        if n == 0 {
            return true;
        }
        if n < 0 {
            return false;
        }
        let chunk = &buf[..n as usize];
        let mut start = 0;
        for i in 0..chunk.len() {
            if lines >= count {
                break;
            }
            if chunk[i] == b'\n' {
                lines += 1;
                let end = i + 1;
                if !write_all(STDOUT_FILENO, &chunk[start..end]) {
                    return false;
                }
                start = end;
            }
        }
        // write any partial line at end of buffer only if we still need more lines
        if start < chunk.len() && lines < count && !write_all(STDOUT_FILENO, &chunk[start..]) {
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

fn main(args: &[&str]) -> i32 {
    let mut count: u64 = 10;
    let mut argi = 1;

    while argi < args.len() {
        let arg = args[argi].as_bytes();
        if arg == b"--" {
            argi += 1;
            break;
        }
        if arg.len() >= 2 && arg[0] == b'-' && arg[1] == b'n' {
            if arg.len() > 2 {
                match parse_u64(&arg[2..]) {
                    Some(v) => count = v,
                    None => {
                        write_str(STDERR_FILENO, "usage: head [-n lines] [file...]\n");
                        return 1;
                    }
                }
                argi += 1;
            } else {
                argi += 1;
                if argi >= args.len() {
                    write_str(STDERR_FILENO, "usage: head [-n lines] [file...]\n");
                    return 1;
                }
                match parse_u64(args[argi].as_bytes()) {
                    Some(v) => count = v,
                    None => {
                        write_str(STDERR_FILENO, "usage: head [-n lines] [file...]\n");
                        return 1;
                    }
                }
                argi += 1;
            }
            continue;
        }
        if !arg.is_empty() && arg[0] == b'-' {
            write_str(STDERR_FILENO, "usage: head [-n lines] [file...]\n");
            return 1;
        }
        break;
    }

    if argi >= args.len() {
        return if head_fd(0, count) { 0 } else { 1 };
    }

    let mut ret = 0;
    for file in &args[argi..] {
        let bytes = file.as_bytes();
        if bytes.len() > 255 {
            write_str(STDERR_FILENO, "head: path too long\n");
            ret = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "head: cannot open: ");
            write_str(STDERR_FILENO, file);
            write_str(STDERR_FILENO, "\n");
            ret = 1;
            continue;
        }
        if !head_fd(fd as i32, count) {
            write_str(STDERR_FILENO, "head: read error on: ");
            write_str(STDERR_FILENO, file);
            write_str(STDERR_FILENO, "\n");
            ret = 1;
        }
        close(fd as i32);
    }
    ret
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
