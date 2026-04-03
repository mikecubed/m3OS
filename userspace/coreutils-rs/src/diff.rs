//! diff — compare two files and show differences.
//!
//! This is a simplified diff: it reads both files in full and either reports
//! they are identical (exit 0) or dumps a unified-style header with all lines
//! from file1 as removals and all lines from file2 as additions (exit 1).
#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use common::{eprintln, write_all, write_u64};
use syscall_lib::{O_RDONLY, STDOUT_FILENO, close, open, read};

syscall_lib::entry_point!(main);

const MAX_FILE: usize = 65536;
const MAX_LINES: usize = 4096;

fn main(args: &[&str]) -> i32 {
    // Skip optional -u flag
    let mut start = 1;
    if start < args.len() && args[start] == "-u" {
        start += 1;
    }

    if args.len() - start < 2 {
        eprintln("usage: diff [-u] file1 file2");
        return 1;
    }

    let path1 = args[start].as_bytes();
    let path2 = args[start + 1].as_bytes();

    static mut BUF1: [u8; MAX_FILE] = [0u8; MAX_FILE];
    static mut BUF2: [u8; MAX_FILE] = [0u8; MAX_FILE];
    static mut LINES1: [u32; MAX_LINES] = [0u32; MAX_LINES];
    static mut LINES2: [u32; MAX_LINES] = [0u32; MAX_LINES];

    // SAFETY: single-threaded; no other references to these statics exist.
    let (len1, nlines1) = match unsafe { read_file(path1, &raw mut BUF1, &raw mut LINES1) } {
        None => {
            eprintln("diff: file too large");
            return 1;
        }
        Some((v, _)) if v == usize::MAX => {
            eprintln("diff: cannot open file1");
            return 1;
        }
        Some(v) => v,
    };

    let (len2, nlines2) = match unsafe { read_file(path2, &raw mut BUF2, &raw mut LINES2) } {
        None => {
            eprintln("diff: file too large");
            return 1;
        }
        Some((v, _)) if v == usize::MAX => {
            eprintln("diff: cannot open file2");
            return 1;
        }
        Some(v) => v,
    };

    // SAFETY: read_file populated these buffers up to len1/len2.
    let data1 = unsafe { core::slice::from_raw_parts(&raw const BUF1 as *const u8, len1) };
    let data2 = unsafe { core::slice::from_raw_parts(&raw const BUF2 as *const u8, len2) };

    if data1 == data2 {
        return 0;
    }

    let lines1 = unsafe { core::slice::from_raw_parts(&raw const LINES1 as *const u32, nlines1) };
    let lines2 = unsafe { core::slice::from_raw_parts(&raw const LINES2 as *const u32, nlines2) };

    // Print header
    write_all(STDOUT_FILENO, b"--- ");
    write_all(STDOUT_FILENO, path1);
    write_all(STDOUT_FILENO, b"\n+++ ");
    write_all(STDOUT_FILENO, path2);
    write_all(STDOUT_FILENO, b"\n@@ -");

    if nlines1 == 0 {
        write_all(STDOUT_FILENO, b"0,0");
    } else if nlines1 == 1 {
        write_all(STDOUT_FILENO, b"1");
    } else {
        write_all(STDOUT_FILENO, b"1,");
        write_u64(STDOUT_FILENO, nlines1 as u64);
    }
    write_all(STDOUT_FILENO, b" +");
    if nlines2 == 0 {
        write_all(STDOUT_FILENO, b"0,0");
    } else if nlines2 == 1 {
        write_all(STDOUT_FILENO, b"1");
    } else {
        write_all(STDOUT_FILENO, b"1,");
        write_u64(STDOUT_FILENO, nlines2 as u64);
    }
    write_all(STDOUT_FILENO, b" @@\n");

    // Print removals
    print_lines(data1, lines1, b'-');

    // Print additions
    print_lines(data2, lines2, b'+');

    1
}

/// Read a file into `buf`, recording the byte offset of each line start in
/// `line_offsets`. Returns `Some((total_bytes_read, number_of_lines))` on
/// success, `Some((usize::MAX, 0))` if the file cannot be opened, and `None`
/// if the file exceeds `MAX_FILE` bytes or contains more than `MAX_LINES` lines.
///
/// # Safety
/// `buf` must point to a valid `[u8; MAX_FILE]` and `line_offsets` to a
/// valid `[u32; MAX_LINES]`. Both must be exclusively accessible for the
/// duration of this call.
unsafe fn read_file(
    path: &[u8],
    buf: *mut [u8; MAX_FILE],
    line_offsets: *mut [u32; MAX_LINES],
) -> Option<(usize, usize)> {
    if path.len() > 255 {
        return Some((usize::MAX, 0));
    }
    let mut nul_path = [0u8; 256];
    nul_path[..path.len()].copy_from_slice(path);
    nul_path[path.len()] = 0;

    let fd = open(&nul_path[..=path.len()], O_RDONLY, 0);
    if fd < 0 {
        return Some((usize::MAX, 0));
    }
    let fd = fd as i32;

    let buf = unsafe { &mut *buf };

    let mut total = 0usize;
    let mut overflow = false;
    loop {
        let space = buf.len().saturating_sub(total);
        if space == 0 {
            // Buffer full; attempt one extra read to detect a file larger than MAX_FILE.
            let mut one = [0u8; 1];
            if read(fd, &mut one) > 0 {
                overflow = true;
            }
            break;
        }
        let n = read(fd, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
    }
    close(fd);

    if overflow {
        return None;
    }

    let line_offsets = unsafe { &mut *line_offsets };

    // Scan for line starts
    let mut nlines = 0usize;
    let mut at_line_start = true;
    for (i, &b) in buf[..total].iter().enumerate() {
        if at_line_start {
            if nlines >= MAX_LINES {
                return None;
            }
            line_offsets[nlines] = i as u32;
            nlines += 1;
            at_line_start = false;
        }
        if b == b'\n' {
            at_line_start = true;
        }
    }
    // If file ends without newline and there's content, last line already recorded
    Some((total, nlines))
}

fn print_lines(data: &[u8], line_offsets: &[u32], prefix: u8) {
    let prefix_buf = [prefix];
    for (i, &off) in line_offsets.iter().enumerate() {
        let start = off as usize;
        let end = if i + 1 < line_offsets.len() {
            line_offsets[i + 1] as usize
        } else {
            data.len()
        };
        write_all(STDOUT_FILENO, &prefix_buf);
        let line = &data[start..end];
        write_all(STDOUT_FILENO, line);
        // Ensure newline termination
        if !line.ends_with(b"\n") {
            write_all(STDOUT_FILENO, b"\n");
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
