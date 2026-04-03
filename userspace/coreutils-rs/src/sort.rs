//! sort — sort lines of text.
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO, close, open, read, write, write_str,
};

syscall_lib::entry_point!(main);

const BUF_SIZE: usize = 65536;
const MAX_LINES: usize = 2048;

fn parse_leading_u64(s: &[u8]) -> u64 {
    let mut v = 0u64;
    for &b in s {
        if b.is_ascii_digit() {
            v = v.wrapping_mul(10).wrapping_add((b - b'0') as u64);
        } else {
            break;
        }
    }
    v
}

fn line_end(buf: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < buf.len() && buf[i] != b'\n' {
        i += 1;
    }
    i
}

fn insertion_sort(starts: &mut [u32], buf: &[u8], numeric: bool, reverse: bool) {
    let n = starts.len();
    for i in 1..n {
        let key_off = starts[i] as usize;
        let key_end = line_end(buf, key_off);
        let key = &buf[key_off..key_end];

        let mut j = i;
        while j > 0 {
            let prev_off = starts[j - 1] as usize;
            let prev_end = line_end(buf, prev_off);
            let prev = &buf[prev_off..prev_end];

            let ord = if numeric {
                let na = parse_leading_u64(prev);
                let nb = parse_leading_u64(key);
                na.cmp(&nb).then_with(|| prev.cmp(key))
            } else {
                prev.cmp(key)
            };

            let should_swap = if reverse {
                ord == core::cmp::Ordering::Less
            } else {
                ord == core::cmp::Ordering::Greater
            };
            if should_swap {
                starts[j] = starts[j - 1];
                j -= 1;
            } else {
                break;
            }
        }
        starts[j] = starts[i];
    }
}

fn collect_lines(buf: &[u8], fill: usize, starts: &mut [u32; MAX_LINES]) -> Option<usize> {
    if fill == 0 {
        return Some(0);
    }
    let mut count = 0usize;
    starts[count] = 0;
    count += 1;
    for (i, &byte) in buf[..fill].iter().enumerate() {
        if byte == b'\n' {
            let next = i + 1;
            if next < fill {
                if count >= MAX_LINES {
                    return None;
                }
                starts[count] = next as u32;
                count += 1;
            }
        }
    }
    Some(count)
}

fn write_sorted(buf: &[u8], _fill: usize, starts: &[u32], count: usize) {
    for &start_pos in &starts[..count] {
        let start = start_pos as usize;
        let end = line_end(buf, start);
        let _ = write(STDOUT_FILENO, &buf[start..end]);
        // Always emit a newline after each line.
        let _ = write(STDOUT_FILENO, b"\n");
    }
}

fn read_fd_append(fd: i32, buf: &mut [u8; BUF_SIZE], fill: &mut usize) -> bool {
    loop {
        if *fill >= BUF_SIZE {
            return false;
        }
        let n = read(fd, &mut buf[*fill..]);
        if n == 0 {
            break;
        }
        if n < 0 {
            return false;
        }
        *fill += n as usize;
    }
    true
}

fn main(args: &[&str]) -> i32 {
    let mut argi = 1usize;
    let mut reverse = false;
    let mut numeric = false;

    while argi < args.len() && args[argi].starts_with('-') && args[argi].len() > 1 {
        if args[argi] == "--" {
            argi += 1;
            break;
        }
        for b in args[argi].bytes().skip(1) {
            match b {
                b'r' => reverse = true,
                b'n' => numeric = true,
                _ => {
                    write_str(STDERR_FILENO, "usage: sort [-r] [-n] [file...]\n");
                    return 1;
                }
            }
        }
        argi += 1;
    }

    let mut buf = [0u8; BUF_SIZE];
    let mut starts = [0u32; MAX_LINES];
    let mut fill = 0usize;

    if argi == args.len() {
        if !read_fd_append(STDIN_FILENO, &mut buf, &mut fill) {
            write_str(STDERR_FILENO, "sort: input too large\n");
            return 1;
        }
    } else {
        for arg in &args[argi..] {
            let bytes = arg.as_bytes();
            if bytes.len() > 511 {
                write_str(STDERR_FILENO, "sort: path too long\n");
                return 1;
            }
            let mut path = [0u8; 512];
            path[..bytes.len()].copy_from_slice(bytes);
            path[bytes.len()] = 0;
            let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
            if fd < 0 {
                write_str(STDERR_FILENO, "sort: cannot open '");
                let _ = write(STDERR_FILENO, bytes);
                write_str(STDERR_FILENO, "'\n");
                return 1;
            }
            if !read_fd_append(fd as i32, &mut buf, &mut fill) {
                close(fd as i32);
                write_str(STDERR_FILENO, "sort: input too large\n");
                return 1;
            }
            close(fd as i32);
        }
    }

    match collect_lines(&buf, fill, &mut starts) {
        None => {
            write_str(STDERR_FILENO, "sort: too many lines\n");
            1
        }
        Some(count) => {
            insertion_sort(&mut starts[..count], &buf, numeric, reverse);
            write_sorted(&buf, fill, &starts, count);
            0
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
