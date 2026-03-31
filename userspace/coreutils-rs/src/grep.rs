//! grep — search for a fixed string in files or stdin.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(STDERR_FILENO, "usage: grep <pattern> [file...]\n");
        return 1;
    }
    let pattern = args[1].as_bytes();
    let mut ret = 0;
    if args.len() == 2 {
        if !grep_fd(0, pattern) {
            ret = 1;
        }
    } else {
        for arg in &args[2..] {
            let bytes = arg.as_bytes();
            if bytes.len() > 255 {
                write_str(STDERR_FILENO, "grep: path too long\n");
                ret = 1;
                continue;
            }
            let mut path = [0u8; 256];
            path[..bytes.len()].copy_from_slice(bytes);
            path[bytes.len()] = 0;
            let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
            if fd < 0 {
                write_str(STDERR_FILENO, "grep: cannot open file\n");
                ret = 1;
                continue;
            }
            if !grep_fd(fd as i32, pattern) {
                ret = 1;
            }
            close(fd as i32);
        }
    }
    ret
}

/// Returns true on success, false on I/O error.
fn grep_fd(fd: i32, pattern: &[u8]) -> bool {
    let mut buf = [0u8; 4096];
    let mut line_start = 0usize;

    loop {
        let space = buf.len() - line_start;
        if space == 0 {
            // Line longer than buffer — flush and reset.
            line_start = 0;
            continue;
        }
        let n = read(fd, &mut buf[line_start..]);
        if n == 0 {
            break;
        }
        if n < 0 {
            write_str(STDERR_FILENO, "grep: read error\n");
            return false;
        }
        let end = line_start + n as usize;
        let mut pos = 0;

        loop {
            // Find next newline in buf[pos..end].
            let nl = buf[pos..end].iter().position(|&b| b == b'\n');
            match nl {
                Some(offset) => {
                    let line = &buf[pos..pos + offset];
                    if contains(line, pattern) {
                        write_all(STDOUT_FILENO, line);
                        write_all(STDOUT_FILENO, b"\n");
                    }
                    pos += offset + 1;
                }
                None => {
                    // No newline — move leftover to start of buf.
                    let leftover = end - pos;
                    if leftover > 0 && pos > 0 {
                        buf.copy_within(pos..end, 0);
                    }
                    line_start = leftover;
                    break;
                }
            }
        }
    }

    // Check last line (no trailing newline).
    if line_start > 0 && contains(&buf[..line_start], pattern) {
        write_all(STDOUT_FILENO, &buf[..line_start]);
        write_all(STDOUT_FILENO, b"\n");
    }
    true
}

/// Fixed-string substring search (equivalent to C strstr).
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    for i in 0..=(haystack.len() - needle.len()) {
        if haystack[i..i + needle.len()] == *needle {
            return true;
        }
    }
    false
}

fn write_all(fd: i32, data: &[u8]) {
    let mut off = 0;
    while off < data.len() {
        let w = write(fd, &data[off..]);
        if w <= 0 {
            break;
        }
        off += w as usize;
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
