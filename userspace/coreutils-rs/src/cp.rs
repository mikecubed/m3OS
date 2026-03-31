//! cp — copy files.
#![no_std]
#![no_main]

use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDERR_FILENO, close, open, read, write, write_str,
};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_str(STDERR_FILENO, "usage: cp <src> <dst>\n");
        return 1;
    }

    let src_path = make_path(args[1].as_bytes());
    let dst_path = make_path(args[2].as_bytes());
    if src_path.is_none() || dst_path.is_none() {
        write_str(STDERR_FILENO, "cp: path too long\n");
        return 1;
    }
    let src_path = src_path.unwrap();
    let dst_path = dst_path.unwrap();

    let src_fd = open(&src_path.buf[..=src_path.len], O_RDONLY, 0);
    if src_fd < 0 {
        write_str(STDERR_FILENO, "cp: cannot open source\n");
        return 1;
    }

    let dst_fd = open(
        &dst_path.buf[..=dst_path.len],
        O_WRONLY | O_CREAT | O_TRUNC,
        0o644,
    );
    if dst_fd < 0 {
        write_str(STDERR_FILENO, "cp: cannot create dest\n");
        close(src_fd as i32);
        return 1;
    }

    let mut buf = [0u8; 4096];
    loop {
        let n = read(src_fd as i32, &mut buf);
        if n <= 0 {
            break;
        }
        write_all(dst_fd as i32, &buf[..n as usize]);
    }

    close(src_fd as i32);
    close(dst_fd as i32);
    0
}

struct NulPath {
    buf: [u8; 256],
    len: usize,
}

fn make_path(bytes: &[u8]) -> Option<NulPath> {
    if bytes.len() > 255 {
        return None;
    }
    let mut p = NulPath {
        buf: [0u8; 256],
        len: bytes.len(),
    };
    p.buf[..bytes.len()].copy_from_slice(bytes);
    p.buf[bytes.len()] = 0;
    Some(p)
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
