//! tail — output the last N lines of a file.
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO, close, lseek, open, read, write,
    write_str,
};

syscall_lib::entry_point!(main);

const BUF_SIZE: usize = 65536;

fn parse_usize(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut v: usize = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add((b - b'0') as usize);
    }
    Some(v)
}

fn tail_fd(fd: i32, count: usize) {
    let mut buf = [0u8; BUF_SIZE];

    // Seek to last BUF_SIZE bytes for regular files; ignore error (stdin, small files).
    let _ = lseek(fd, -(BUF_SIZE as i64), 2 /* SEEK_END */);

    let mut fill = 0usize;
    loop {
        if fill >= BUF_SIZE {
            break;
        }
        let n = read(fd, &mut buf[fill..]);
        if n <= 0 {
            break;
        }
        fill += n as usize;
    }

    if count == 0 || fill == 0 {
        return;
    }

    // Scan backwards to find the start of the (count)th-to-last line.
    let mut newlines_found = 0usize;
    let mut i = fill;
    // Skip a trailing newline so it doesn't count as its own line.
    if buf[i - 1] == b'\n' {
        i -= 1;
    }
    let mut start = 0usize;
    while i > 0 {
        i -= 1;
        if buf[i] == b'\n' {
            newlines_found += 1;
            if newlines_found == count {
                start = i + 1;
                break;
            }
        }
    }

    let _ = write(STDOUT_FILENO, &buf[start..fill]);
}

fn main(args: &[&str]) -> i32 {
    let mut argi = 1usize;
    let mut count: usize = 10;

    while argi < args.len() && args[argi].starts_with('-') && args[argi].len() > 1 {
        if args[argi] == "--" {
            argi += 1;
            break;
        }
        if args[argi] == "-n" {
            if argi + 1 >= args.len() {
                write_str(STDERR_FILENO, "usage: tail [-n lines] [file...]\n");
                return 1;
            }
            match parse_usize(args[argi + 1]) {
                Some(v) => count = v,
                None => {
                    write_str(STDERR_FILENO, "usage: tail [-n lines] [file...]\n");
                    return 1;
                }
            }
            argi += 2;
            continue;
        }
        // Support "-nN" (combined) form.
        if args[argi].len() > 2 && args[argi].as_bytes()[1] == b'n' {
            match parse_usize(&args[argi][2..]) {
                Some(v) => count = v,
                None => {
                    write_str(STDERR_FILENO, "usage: tail [-n lines] [file...]\n");
                    return 1;
                }
            }
            argi += 1;
            continue;
        }
        write_str(STDERR_FILENO, "usage: tail [-n lines] [file...]\n");
        return 1;
    }

    if argi == args.len() {
        tail_fd(STDIN_FILENO, count);
        return 0;
    }

    let mut status = 0i32;
    for arg in &args[argi..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 511 {
            write_str(STDERR_FILENO, "tail: path too long\n");
            status = 1;
            continue;
        }
        let mut path = [0u8; 512];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;
        let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
        if fd < 0 {
            write_str(STDERR_FILENO, "tail: cannot open '");
            let _ = write(STDERR_FILENO, bytes);
            write_str(STDERR_FILENO, "'\n");
            status = 1;
            continue;
        }
        tail_fd(fd as i32, count);
        close(fd as i32);
    }
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
