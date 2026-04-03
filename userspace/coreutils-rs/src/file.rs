//! file — determine the type of each file.
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDERR_FILENO, STDOUT_FILENO, Stat, close, fstat, lstat_stat, open, read, write,
    write_str,
};

syscall_lib::entry_point!(main);

const O_NOFOLLOW: u64 = 0x20000;

const S_IFMT: u32 = 0xf000;
const S_IFREG: u32 = 0x8000;
const S_IFDIR: u32 = 0x4000;
const S_IFCHR: u32 = 0x2000;
const S_IFLNK: u32 = 0xa000;

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

fn describe_bytes(buf: &[u8]) -> &'static str {
    if buf.len() >= 4 && buf[0] == 0x7f && buf[1] == b'E' && buf[2] == b'L' && buf[3] == b'F' {
        return "ELF 64-bit";
    }
    for &b in buf {
        if b == 0 {
            return "data";
        }
        // non-printable, non-whitespace
        if b < 0x09 || (b > 0x0d && b < 0x20) || b > 0x7e {
            return "data";
        }
    }
    "ASCII text"
}

fn describe_path(name: &str) -> i32 {
    let bytes = name.as_bytes();
    if bytes.len() > 255 {
        write_str(STDERR_FILENO, "file: path too long\n");
        return 1;
    }
    let mut path = [0u8; 256];
    path[..bytes.len()].copy_from_slice(bytes);
    path[bytes.len()] = 0;

    let fd = open(&path[..=bytes.len()], O_RDONLY | O_NOFOLLOW, 0);
    if fd < 0 {
        // O_NOFOLLOW causes open to fail on symlinks — use lstat to confirm
        let mut lst = Stat::zeroed();
        if lstat_stat(&path[..=bytes.len()], &mut lst) >= 0 && (lst.st_mode & S_IFMT) == S_IFLNK {
            write_all(STDOUT_FILENO, bytes);
            write_str(STDOUT_FILENO, ": symbolic link\n");
            return 0;
        }
        write_str(STDERR_FILENO, "file: cannot open: ");
        write_str(STDERR_FILENO, name);
        write_str(STDERR_FILENO, "\n");
        return 1;
    }

    let mut st = Stat::zeroed();
    if fstat(fd as i32, &mut st) < 0 {
        write_str(STDERR_FILENO, "file: cannot stat: ");
        write_str(STDERR_FILENO, name);
        write_str(STDERR_FILENO, "\n");
        close(fd as i32);
        return 1;
    }

    let ftype = st.st_mode & S_IFMT;
    if ftype == S_IFCHR {
        write_all(STDOUT_FILENO, bytes);
        write_str(STDOUT_FILENO, ": character special\n");
        close(fd as i32);
        return 0;
    }
    if ftype == S_IFDIR {
        write_all(STDOUT_FILENO, bytes);
        write_str(STDOUT_FILENO, ": directory\n");
        close(fd as i32);
        return 0;
    }
    if ftype != S_IFREG {
        write_all(STDOUT_FILENO, bytes);
        write_str(STDOUT_FILENO, ": special file\n");
        close(fd as i32);
        return 0;
    }

    let mut buf = [0u8; 256];
    let n = read(fd as i32, &mut buf);
    close(fd as i32);
    if n < 0 {
        write_str(STDERR_FILENO, "file: cannot read: ");
        write_str(STDERR_FILENO, name);
        write_str(STDERR_FILENO, "\n");
        return 1;
    }

    let kind = describe_bytes(&buf[..n as usize]);
    write_all(STDOUT_FILENO, bytes);
    write_str(STDOUT_FILENO, ": ");
    write_str(STDOUT_FILENO, kind);
    write_str(STDOUT_FILENO, "\n");
    0
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(STDERR_FILENO, "usage: file FILE...\n");
        return 1;
    }
    let mut status = 0;
    for arg in &args[1..] {
        if describe_path(arg) != 0 {
            status = 1;
        }
    }
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
