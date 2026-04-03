//! free — display amount of free and used memory.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

/// Unit mode: 0 = KiB, 1 = MiB, 2 = human-readable.
const UNIT_KB: u8 = 0;
const UNIT_MB: u8 = 1;
const UNIT_HUMAN: u8 = 2;

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

/// Write a right-aligned decimal number in a field of `width` characters.
fn write_field(fd: i32, n: u64, width: usize) {
    let mut nbuf = [0u8; 20];
    let digits = fmt_u64(n, &mut nbuf);
    let len = digits.len();
    if width > len {
        for _ in 0..(width - len) {
            write_str(fd, " ");
        }
    }
    write_all(fd, digits);
}

/// Format `kb` into `out` buf (human readable with suffix).
fn fmt_human(kb: u64, out: &mut [u8; 16]) -> &[u8] {
    const SUFFIXES: &[u8] = b"KMGT";
    let mut whole = kb;
    let mut rem: u64 = 0;
    let mut suffix_idx: usize = 0;
    while whole >= 1024 && suffix_idx + 1 < SUFFIXES.len() {
        rem = whole % 1024;
        whole /= 1024;
        suffix_idx += 1;
    }
    // Format as "whole.rS" where r = (rem*10)/1024
    let frac = (rem * 10) / 1024;
    let mut pos = 0usize;
    let mut tmp = [0u8; 20];
    let digits = fmt_u64(whole, &mut tmp);
    for &d in digits {
        if pos < out.len() {
            out[pos] = d;
            pos += 1;
        }
    }
    if pos < out.len() {
        out[pos] = b'.';
        pos += 1;
    }
    if pos < out.len() {
        out[pos] = b'0' + frac as u8;
        pos += 1;
    }
    if pos < out.len() {
        out[pos] = SUFFIXES[suffix_idx];
        pos += 1;
    }
    &out[..pos]
}

/// Parse the value following a label like "MemTotal:" in a meminfo-style buffer.
/// Returns 0 if not found.
fn parse_meminfo_value(label: &[u8], buf: &[u8]) -> u64 {
    // Find label in buf
    let llen = label.len();
    if llen == 0 || buf.len() < llen {
        return 0;
    }
    let limit = buf.len() - llen;
    let mut pos = 0;
    while pos <= limit {
        if &buf[pos..pos + llen] == label {
            // Skip past label and any spaces/colons
            let mut i = pos + llen;
            while i < buf.len() && (buf[i] == b' ' || buf[i] == b'\t') {
                i += 1;
            }
            let mut v: u64 = 0;
            while i < buf.len() && buf[i] >= b'0' && buf[i] <= b'9' {
                v = v.wrapping_mul(10).wrapping_add((buf[i] - b'0') as u64);
                i += 1;
            }
            return v;
        }
        pos += 1;
    }
    0
}

fn main(args: &[&str]) -> i32 {
    let mut unit = UNIT_KB;

    if args.len() > 2 {
        write_str(STDERR_FILENO, "usage: free [-m] [-h]\n");
        return 1;
    }
    if args.len() == 2 {
        match args[1] {
            "-m" => unit = UNIT_MB,
            "-h" => unit = UNIT_HUMAN,
            _ => {
                write_str(STDERR_FILENO, "usage: free [-m] [-h]\n");
                return 1;
            }
        }
    }

    let path = b"/proc/meminfo\0";
    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "free: cannot open /proc/meminfo\n");
        return 1;
    }
    let mut buf = [0u8; 2048];
    let n = read(fd as i32, &mut buf);
    close(fd as i32);
    if n < 0 {
        write_str(STDERR_FILENO, "free: cannot read /proc/meminfo\n");
        return 1;
    }
    let data = &buf[..n as usize];

    let total_kb = parse_meminfo_value(b"MemTotal:", data);
    let avail_kb = parse_meminfo_value(b"MemAvailable:", data);
    let used_kb = total_kb.saturating_sub(avail_kb);

    write_str(
        STDOUT_FILENO,
        "              total        used   available\n",
    );
    write_str(STDOUT_FILENO, "Mem:");

    match unit {
        UNIT_MB => {
            write_field(STDOUT_FILENO, total_kb / 1024, 13);
            write_field(STDOUT_FILENO, used_kb / 1024, 12);
            write_field(STDOUT_FILENO, avail_kb / 1024, 12);
        }
        UNIT_HUMAN => {
            let mut tbuf = [0u8; 16];
            let mut ubuf = [0u8; 16];
            let mut abuf = [0u8; 16];
            let ts = fmt_human(total_kb, &mut tbuf);
            let us = fmt_human(used_kb, &mut ubuf);
            let av = fmt_human(avail_kb, &mut abuf);
            // right-align in 13/12/12 wide fields
            let pad = |w: usize, s: &[u8]| {
                if w > s.len() {
                    for _ in 0..(w - s.len()) {
                        write_str(STDOUT_FILENO, " ");
                    }
                }
                write_all(STDOUT_FILENO, s);
            };
            pad(13, ts);
            pad(12, us);
            pad(12, av);
        }
        _ => {
            // default KiB
            write_field(STDOUT_FILENO, total_kb, 13);
            write_field(STDOUT_FILENO, used_kb, 12);
            write_field(STDOUT_FILENO, avail_kb, 12);
        }
    }
    write_str(STDOUT_FILENO, "\n");
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
