//! uniq — filter adjacent duplicate lines from input.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

fn fmt_u64(n: u64, buf: &mut [u8; 20]) -> &[u8] {
    if n == 0 {
        buf[19] = b'0';
        return &buf[19..];
    }
    let mut pos = buf.len();
    let mut v = n;
    while v > 0 {
        pos -= 1;
        buf[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    &buf[pos..]
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

/// Emit `line` with optional count prefix.
fn emit_line(line: &[u8], count: u64, show_count: bool) {
    if show_count {
        let mut num_buf = [0u8; 20];
        let num = fmt_u64(count, &mut num_buf);
        write_all(STDOUT_FILENO, num);
        write_str(STDOUT_FILENO, " ");
    }
    write_all(STDOUT_FILENO, line);
}

/// Process lines from `fd`.  Uses two 4096-byte stack line buffers.
fn uniq_fd(fd: i32, show_count: bool) -> bool {
    // We read into a raw buffer and scan for newlines.
    // prev_line holds the previous complete line (up to 4095 bytes + newline).
    let mut prev_buf = [0u8; 4096];
    let mut prev_len: usize = 0;
    let mut count: u64 = 0;

    // cur_line accumulates the current line being built from the read buffer.
    let mut cur_buf = [0u8; 4096];
    let mut cur_len: usize = 0;

    let mut read_buf = [0u8; 4096];
    let mut have_prev = false;

    loop {
        let n = read(fd, &mut read_buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            return false;
        }
        for &b in &read_buf[..n as usize] {
            if cur_len < cur_buf.len() {
                cur_buf[cur_len] = b;
                cur_len += 1;
            }
            if b == b'\n' {
                // We have a complete line in cur_buf[..cur_len]
                let cur = &cur_buf[..cur_len];
                if !have_prev {
                    prev_buf[..cur_len].copy_from_slice(cur);
                    prev_len = cur_len;
                    count = 1;
                    have_prev = true;
                } else if cur == &prev_buf[..prev_len] {
                    count += 1;
                } else {
                    emit_line(&prev_buf[..prev_len], count, show_count);
                    prev_buf[..cur_len].copy_from_slice(cur);
                    prev_len = cur_len;
                    count = 1;
                }
                cur_len = 0;
            }
        }
    }

    // Flush any unterminated last line
    if cur_len > 0 {
        let cur = &cur_buf[..cur_len];
        if !have_prev {
            emit_line(cur, 1, show_count);
        } else if cur == &prev_buf[..prev_len] {
            count += 1;
            emit_line(&prev_buf[..prev_len], count, show_count);
        } else {
            emit_line(&prev_buf[..prev_len], count, show_count);
            emit_line(cur, 1, show_count);
        }
    } else if have_prev {
        emit_line(&prev_buf[..prev_len], count, show_count);
    }

    true
}

fn main(args: &[&str]) -> i32 {
    let mut show_count = false;
    let mut argi = 1;

    while argi < args.len() {
        let arg = args[argi].as_bytes();
        if arg == b"--" {
            argi += 1;
            break;
        }
        if arg == b"-c" {
            show_count = true;
            argi += 1;
            continue;
        }
        if !arg.is_empty() && arg[0] == b'-' {
            write_str(STDERR_FILENO, "usage: uniq [-c] [file]\n");
            return 1;
        }
        break;
    }

    if argi >= args.len() {
        return if uniq_fd(0, show_count) { 0 } else { 1 };
    }
    if argi + 1 != args.len() {
        write_str(STDERR_FILENO, "usage: uniq [-c] [file]\n");
        return 1;
    }

    let file = args[argi];
    let bytes = file.as_bytes();
    if bytes.len() > 255 {
        write_str(STDERR_FILENO, "uniq: path too long\n");
        return 1;
    }
    let mut path = [0u8; 256];
    path[..bytes.len()].copy_from_slice(bytes);
    path[bytes.len()] = 0;
    let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "uniq: cannot open: ");
        write_str(STDERR_FILENO, file);
        write_str(STDERR_FILENO, "\n");
        return 1;
    }
    let ok = uniq_fd(fd as i32, show_count);
    close(fd as i32);
    if ok { 0 } else { 1 }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
