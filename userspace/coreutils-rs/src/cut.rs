//! cut — cut out selected fields or characters from each line.
#![no_std]
#![no_main]

#[path = "common.rs"]
mod common;

use common::{eprintln, find_newline, write_all};
use syscall_lib::{O_RDONLY, STDOUT_FILENO, close, open, read};

syscall_lib::entry_point!(main);

enum Mode {
    Field { n: usize, delim: u8 },
    Chars { lo: usize, hi: usize },
}

fn main(args: &[&str]) -> i32 {
    let mut mode: Option<Mode> = None;
    let mut pending_field: Option<usize> = None;
    let mut pending_delim: Option<u8> = None;
    let mut files_start = args.len();
    let mut i = 1;

    while i < args.len() {
        let a = args[i].as_bytes();
        if a == b"-f" {
            i += 1;
            if i >= args.len() {
                eprintln("cut: option -f requires an argument");
                return 1;
            }
            let n = match common::parse_positive_u64(args[i].as_bytes()) {
                Some(v) => v as usize,
                None => {
                    eprintln("cut: invalid field number");
                    return 1;
                }
            };
            i += 1;
            pending_field = Some(n);
            files_start = i;
        } else if a.starts_with(b"-f") && a.len() > 2 {
            let n = match common::parse_positive_u64(&a[2..]) {
                Some(v) => v as usize,
                None => {
                    eprintln("cut: invalid field number");
                    return 1;
                }
            };
            i += 1;
            pending_field = Some(n);
            files_start = i;
        } else if a == b"-d" {
            i += 1;
            if i >= args.len() {
                eprintln("cut: option -d requires an argument");
                return 1;
            }
            let dc = args[i].as_bytes();
            if dc.is_empty() {
                eprintln("cut: delimiter must be a single character");
                return 1;
            }
            pending_delim = Some(dc[0]);
            i += 1;
            files_start = i;
        } else if a.starts_with(b"-d") && a.len() > 2 {
            pending_delim = Some(a[2]);
            i += 1;
            files_start = i;
        } else if a == b"-c" {
            i += 1;
            if i >= args.len() {
                eprintln("cut: option -c requires an argument");
                return 1;
            }
            let spec = args[i].as_bytes();
            let (lo, hi) = match parse_char_range(spec) {
                Some(r) => r,
                None => {
                    eprintln("cut: invalid character range");
                    return 1;
                }
            };
            i += 1;
            mode = Some(Mode::Chars { lo, hi });
            files_start = i;
        } else if a.starts_with(b"-c") && a.len() > 2 {
            let (lo, hi) = match parse_char_range(&a[2..]) {
                Some(r) => r,
                None => {
                    eprintln("cut: invalid character range");
                    return 1;
                }
            };
            i += 1;
            mode = Some(Mode::Chars { lo, hi });
            files_start = i;
        } else {
            files_start = i;
            break;
        }
    }

    // Combine -f and -d (parsed independently, order-independent)
    if mode.is_none() {
        if let Some(n) = pending_field {
            mode = Some(Mode::Field {
                n,
                delim: pending_delim.unwrap_or(b'\t'),
            });
        } else if let Some(delim) = pending_delim {
            // -d given without -f: default to field 1
            mode = Some(Mode::Field { n: 1, delim });
        }
    }

    let mode = match mode {
        Some(m) => m,
        None => {
            eprintln("cut: must specify -f or -c");
            return 1;
        }
    };

    let mut ret = 0;
    if files_start >= args.len() {
        if process_fd(0, &mode) != 0 {
            ret = 1;
        }
    } else {
        for arg in &args[files_start..] {
            let bytes = arg.as_bytes();
            if bytes.len() > 255 {
                eprintln("cut: path too long");
                ret = 1;
                continue;
            }
            let mut path = [0u8; 256];
            path[..bytes.len()].copy_from_slice(bytes);
            path[bytes.len()] = 0;
            let fd = open(&path[..=bytes.len()], O_RDONLY, 0);
            if fd < 0 {
                eprintln("cut: cannot open file");
                ret = 1;
                continue;
            }
            if process_fd(fd as i32, &mode) != 0 {
                ret = 1;
            }
            close(fd as i32);
        }
    }
    ret
}

/// Parse a char range spec: "N" → (N-1, N) or "M-N" → (M-1, N), 1-based inclusive.
fn parse_char_range(spec: &[u8]) -> Option<(usize, usize)> {
    if let Some(dash) = spec.iter().position(|&b| b == b'-') {
        let lo = common::parse_positive_u64(&spec[..dash])? as usize;
        let hi = common::parse_positive_u64(&spec[dash + 1..])? as usize;
        if lo > hi {
            return None;
        }
        Some((lo - 1, hi))
    } else {
        let n = common::parse_positive_u64(spec)? as usize;
        Some((n - 1, n))
    }
}

fn process_fd(fd: i32, mode: &Mode) -> i32 {
    const BUF: usize = 8192;
    let mut buf = [0u8; BUF];
    let mut filled = 0usize;

    loop {
        let space = buf.len().saturating_sub(filled);
        if space == 0 {
            // Line too long — flush to avoid infinite loop
            filled = 0;
            continue;
        }
        let n = read(fd, &mut buf[filled..]);
        if n == 0 {
            break;
        }
        if n < 0 {
            return 1;
        }
        filled += n as usize;

        let mut pos = 0;
        while let Some(nl) = find_newline(&buf, pos) {
            let line = &buf[pos..nl];
            process_line(line, mode);
            pos = nl + 1;
        }
        // Move leftover to start
        let leftover = filled - pos;
        if leftover > 0 && pos > 0 {
            buf.copy_within(pos..filled, 0);
        }
        filled = leftover;
    }

    // Handle last line without trailing newline
    if filled > 0 {
        process_line(&buf[..filled], mode);
    }
    0
}

fn process_line(line: &[u8], mode: &Mode) {
    match mode {
        Mode::Field { n, delim } => {
            let mut field = 1usize;
            let mut start = 0usize;
            for (idx, &b) in line.iter().enumerate() {
                if b == *delim {
                    if field == *n {
                        write_all(STDOUT_FILENO, &line[start..idx]);
                        write_all(STDOUT_FILENO, b"\n");
                        return;
                    }
                    field += 1;
                    start = idx + 1;
                }
            }
            // Last field (no trailing delimiter)
            if field == *n {
                write_all(STDOUT_FILENO, &line[start..]);
                write_all(STDOUT_FILENO, b"\n");
            }
        }
        Mode::Chars { lo, hi } => {
            let end = (*hi).min(line.len());
            if *lo < end {
                write_all(STDOUT_FILENO, &line[*lo..end]);
            }
            write_all(STDOUT_FILENO, b"\n");
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
