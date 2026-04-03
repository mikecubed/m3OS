//! df — report file system disk space usage.
#![no_std]
#![no_main]

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, open, read, write, write_str};

syscall_lib::entry_point!(main);

const SYS_STATFS: u64 = 137;

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

/// Format size in bytes into a human-readable string with suffix.
fn fmt_human(bytes: u64, out: &mut [u8; 16]) -> &[u8] {
    const SUFFIXES: &[u8] = b"BKMGT";
    let mut whole = bytes;
    let mut rem: u64 = 0;
    let mut idx: usize = 0;
    while whole >= 1024 && idx + 1 < SUFFIXES.len() {
        rem = whole % 1024;
        whole /= 1024;
        idx += 1;
    }
    let frac = (rem * 10) / 1024;
    let mut tmp = [0u8; 20];
    let digits = fmt_u64(whole, &mut tmp);
    let mut pos = 0usize;
    for &d in digits {
        if pos < out.len() {
            out[pos] = d;
            pos += 1;
        }
    }
    if idx == 0 {
        // No decimal for bytes
        if pos < out.len() {
            out[pos] = SUFFIXES[idx];
            pos += 1;
        }
    } else {
        if pos < out.len() {
            out[pos] = b'.';
            pos += 1;
        }
        if pos < out.len() {
            out[pos] = b'0' + frac as u8;
            pos += 1;
        }
        if pos < out.len() {
            out[pos] = SUFFIXES[idx];
            pos += 1;
        }
    }
    &out[..pos]
}

/// Left-pad `data` with spaces to fill `width`, then write it.
fn write_left(fd: i32, data: &[u8], width: usize) {
    write_all(fd, data);
    if data.len() < width {
        for _ in 0..(width - data.len()) {
            write_str(fd, " ");
        }
    }
}

fn do_statfs(path: &[u8], statfs_buf: &mut [u8; 120]) -> bool {
    let ret = unsafe {
        syscall_lib::syscall2(
            SYS_STATFS,
            path.as_ptr() as u64,
            statfs_buf.as_mut_ptr() as u64,
        )
    };
    (ret as i64) >= 0
}

fn print_row(source: &[u8], mountpoint: &[u8], statfs_buf: &[u8; 120], human: bool) {
    // statfs layout (all 8-byte fields):
    // offset  0: f_type
    // offset  8: f_bsize
    // offset 16: f_blocks
    // offset 24: f_bfree
    // offset 32: f_bavail
    // offset 40: f_files
    // offset 48: f_ffree
    let bsize = i64::from_ne_bytes(statfs_buf[8..16].try_into().unwrap()) as u64;
    let blocks = i64::from_ne_bytes(statfs_buf[16..24].try_into().unwrap()) as u64;
    let bfree = i64::from_ne_bytes(statfs_buf[24..32].try_into().unwrap()) as u64;
    let bavail = i64::from_ne_bytes(statfs_buf[32..40].try_into().unwrap()) as u64;

    let block_size = if bsize == 0 { 1024 } else { bsize };
    let total = blocks.wrapping_mul(block_size);
    let free_bytes = bfree.wrapping_mul(block_size);
    let avail = bavail.wrapping_mul(block_size);
    let used = total.saturating_sub(free_bytes);

    write_left(STDOUT_FILENO, source, 12);
    write_str(STDOUT_FILENO, " ");

    if human {
        let mut tbuf = [0u8; 16];
        let mut ubuf = [0u8; 16];
        let mut abuf = [0u8; 16];
        let ts = fmt_human(total, &mut tbuf);
        let us = fmt_human(used, &mut ubuf);
        let av = fmt_human(avail, &mut abuf);
        // write right-aligned in 8-char fields
        let rfield = |w: usize, s: &[u8]| {
            if w > s.len() {
                for _ in 0..(w - s.len()) {
                    write_str(STDOUT_FILENO, " ");
                }
            }
            write_all(STDOUT_FILENO, s);
        };
        rfield(8, ts);
        write_str(STDOUT_FILENO, " ");
        rfield(8, us);
        write_str(STDOUT_FILENO, " ");
        rfield(8, av);
    } else {
        write_field(STDOUT_FILENO, total / 1024, 10);
        write_str(STDOUT_FILENO, " ");
        write_field(STDOUT_FILENO, used / 1024, 10);
        write_str(STDOUT_FILENO, " ");
        write_field(STDOUT_FILENO, avail / 1024, 10);
    }

    write_str(STDOUT_FILENO, " ");
    write_all(STDOUT_FILENO, mountpoint);
    write_str(STDOUT_FILENO, "\n");
}

/// Extract the first whitespace-delimited token from `line` starting at `pos`.
/// Returns (token_start, token_end, next_pos) or None if no token found.
fn next_token(line: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    let mut i = start;
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t') {
        i += 1;
    }
    if i >= line.len() {
        return None;
    }
    let tok_start = i;
    while i < line.len() && line[i] != b' ' && line[i] != b'\t' && line[i] != b'\n' {
        i += 1;
    }
    Some((tok_start, i, i))
}

fn main(args: &[&str]) -> i32 {
    let mut human = false;

    if args.len() > 2 {
        write_str(STDERR_FILENO, "usage: df [-h]\n");
        return 1;
    }
    if args.len() == 2 {
        if args[1] != "-h" {
            write_str(STDERR_FILENO, "usage: df [-h]\n");
            return 1;
        }
        human = true;
    }

    let mounts_path = b"/proc/mounts\0";
    let fd = open(mounts_path, O_RDONLY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "df: cannot open /proc/mounts\n");
        return 1;
    }

    if human {
        write_str(
            STDOUT_FILENO,
            "Filesystem       Size     Used    Avail Mounted on\n",
        );
    } else {
        write_str(
            STDOUT_FILENO,
            "Filesystem    1K-blocks       Used  Available Mounted on\n",
        );
    }

    let mut rbuf = [0u8; 4096];
    // line accumulation buffer
    let mut lbuf = [0u8; 512];
    let mut llen: usize = 0;
    let mut status = 0;

    'outer: loop {
        let n = read(fd as i32, &mut rbuf);
        if n == 0 {
            break;
        }
        if n < 0 {
            write_str(STDERR_FILENO, "df: read error on /proc/mounts\n");
            status = 1;
            break;
        }
        for &b in &rbuf[..n as usize] {
            if llen < lbuf.len() {
                lbuf[llen] = b;
                llen += 1;
            }
            if b == b'\n' {
                // Parse the line: <source> <mountpoint> ...
                let line = &lbuf[..llen];
                if let Some((s0, s1, p1)) = next_token(line, 0)
                    && let Some((m0, m1, _)) = next_token(line, p1)
                {
                    let source = &line[s0..s1];
                    let mount = &line[m0..m1];

                    if mount.len() > 254 {
                        llen = 0;
                        continue;
                    }
                    let mut mpath = [0u8; 256];
                    mpath[..mount.len()].copy_from_slice(mount);
                    mpath[mount.len()] = 0;

                    let mut sfsbuf = [0u8; 120];
                    if !do_statfs(&mpath[..=mount.len()], &mut sfsbuf) {
                        write_str(STDERR_FILENO, "df: statfs failed for: ");
                        write_all(STDERR_FILENO, mount);
                        write_str(STDERR_FILENO, "\n");
                        close(fd as i32);
                        status = 1;
                        break 'outer;
                    }
                    print_row(source, mount, &sfsbuf, human);
                }
                llen = 0;
            }
        }
    }

    close(fd as i32);
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
