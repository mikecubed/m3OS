//! ar — create static library archives (.a files).
#![no_std]
#![no_main]

use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDERR_FILENO, close, open, read, write, write_str,
};

syscall_lib::entry_point!(main);

const AR_MAGIC: &[u8] = b"!<arch>\n";

fn fmt_decimal(buf: &mut [u8], val: u64) {
    let mut tmp = [0u8; 20];
    let mut i = tmp.len();
    let mut v = val;
    if v == 0 {
        i -= 1;
        tmp[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let len = tmp.len() - i;
    let copy_len = len.min(buf.len());
    buf[..copy_len].copy_from_slice(&tmp[i..i + copy_len]);
    // Pad rest with spaces.
    for b in buf[copy_len..].iter_mut() {
        *b = b' ';
    }
}

fn write_header(fd: i32, name: &str, size: u64) {
    let mut hdr = [b' '; 60];

    // Name: 16 bytes, terminated with '/'.
    let name_bytes = name.as_bytes();
    let nlen = name_bytes.len().min(15);
    hdr[..nlen].copy_from_slice(&name_bytes[..nlen]);
    hdr[nlen] = b'/';

    // Timestamp: 12 bytes at offset 16.
    fmt_decimal(&mut hdr[16..28], 0);
    // uid: 6 bytes at offset 28.
    fmt_decimal(&mut hdr[28..34], 0);
    // gid: 6 bytes at offset 34.
    fmt_decimal(&mut hdr[34..40], 0);
    // mode: 8 bytes at offset 40 — octal 100644.
    hdr[40..48].copy_from_slice(b"100644  ");
    // size: 10 bytes at offset 48.
    fmt_decimal(&mut hdr[48..58], size);
    // magic: 2 bytes at offset 58.
    hdr[58] = b'`';
    hdr[59] = b'\n';

    write(fd, &hdr);
}

fn basename(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) => &path[pos + 1..],
        None => path,
    }
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 4 {
        write_str(STDERR_FILENO, "usage: ar rcs ARCHIVE FILE...\n");
        return 1;
    }

    let op = args[1];
    let mut do_replace = false;
    for b in op.as_bytes() {
        match b {
            b'r' => do_replace = true,
            b'c' | b's' => {} // create/symbol-index: no-op
            _ => {}
        }
    }
    if !do_replace {
        write_str(
            STDERR_FILENO,
            "ar: only 'r' (replace/insert) is supported\n",
        );
        return 1;
    }

    let archive = args[2];
    let mut archive_path = [0u8; 256];
    let ab = archive.as_bytes();
    if ab.len() > 254 {
        write_str(STDERR_FILENO, "ar: archive path too long\n");
        return 1;
    }
    archive_path[..ab.len()].copy_from_slice(ab);
    archive_path[ab.len()] = 0;

    let fd = open(
        &archive_path[..=ab.len()],
        O_WRONLY | O_CREAT | O_TRUNC,
        0o644,
    );
    if fd < 0 {
        write_str(STDERR_FILENO, "ar: cannot create archive\n");
        return 1;
    }
    let fd = fd as i32;

    write(fd, AR_MAGIC);

    for file_arg in &args[3..] {
        let fb = file_arg.as_bytes();
        if fb.len() > 254 {
            write_str(STDERR_FILENO, "ar: file path too long\n");
            close(fd);
            return 1;
        }
        let mut fpath = [0u8; 256];
        fpath[..fb.len()].copy_from_slice(fb);
        fpath[fb.len()] = 0;

        // Get file size via stat.
        let mut st = syscall_lib::Stat::zeroed();
        if syscall_lib::stat(&fpath[..=fb.len()], &mut st) < 0 {
            write_str(STDERR_FILENO, "ar: cannot stat: ");
            write_str(STDERR_FILENO, file_arg);
            write_str(STDERR_FILENO, "\n");
            close(fd);
            return 1;
        }

        write_header(fd, basename(file_arg), st.st_size as u64);

        // Copy file content.
        let src = open(&fpath[..=fb.len()], O_RDONLY, 0);
        if src < 0 {
            write_str(STDERR_FILENO, "ar: cannot open: ");
            write_str(STDERR_FILENO, file_arg);
            write_str(STDERR_FILENO, "\n");
            close(fd);
            return 1;
        }
        let src = src as i32;
        let mut buf = [0u8; 4096];
        loop {
            let n = read(src, &mut buf);
            if n < 0 {
                write_str(STDERR_FILENO, "ar: read error\n");
                close(src);
                close(fd);
                return 1;
            }
            if n == 0 {
                break;
            }
            write(fd, &buf[..n as usize]);
        }
        close(src);

        // Pad to even boundary.
        if st.st_size & 1 != 0 {
            write(fd, b"\n");
        }
    }

    close(fd);
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
