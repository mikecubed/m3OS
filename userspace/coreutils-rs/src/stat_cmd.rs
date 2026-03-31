//! stat — display file metadata.
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, write_str, write_u64};

syscall_lib::entry_point!(main);

fn write_i64(fd: i32, n: i64) {
    if n < 0 {
        write_str(fd, "-");
        write_u64(fd, (-n) as u64);
    } else {
        write_u64(fd, n as u64);
    }
}

fn write_oct(fd: i32, mode: u32) {
    let mut buf = [b'0'; 4];
    let mut m = mode & 0o7777;
    for i in (0..4).rev() {
        buf[i] = b'0' + (m & 7) as u8;
        m >>= 3;
    }
    let _ = syscall_lib::write(fd, &buf);
}

fn filetype(mode: u32) -> &'static str {
    match mode & 0xF000 {
        0x8000 => "regular file",
        0x4000 => "directory",
        0x2000 => "character device",
        0x6000 => "block device",
        0x1000 => "FIFO",
        0xA000 => "symbolic link",
        0xC000 => "socket",
        _ => "unknown",
    }
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        write_str(STDERR_FILENO, "usage: stat FILE...\n");
        return 1;
    }
    let mut ret = 0;
    for arg in &args[1..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 254 {
            write_str(STDERR_FILENO, "stat: path too long\n");
            ret = 1;
            continue;
        }
        let mut path = [0u8; 256];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;

        let mut st = syscall_lib::Stat::zeroed();
        if syscall_lib::stat(&path[..=bytes.len()], &mut st) < 0 {
            write_str(STDERR_FILENO, "stat: cannot stat '");
            write_str(STDERR_FILENO, arg);
            write_str(STDERR_FILENO, "'\n");
            ret = 1;
            continue;
        }

        write_str(STDOUT_FILENO, "  File: ");
        write_str(STDOUT_FILENO, arg);
        write_str(STDOUT_FILENO, "\n");
        write_str(STDOUT_FILENO, "  Size: ");
        write_i64(STDOUT_FILENO, st.st_size);
        write_str(STDOUT_FILENO, "\tBlocks: ");
        write_i64(STDOUT_FILENO, st.st_blocks);
        write_str(STDOUT_FILENO, "\tIO Block: ");
        write_i64(STDOUT_FILENO, st.st_blksize);
        write_str(STDOUT_FILENO, "\t");
        write_str(STDOUT_FILENO, filetype(st.st_mode));
        write_str(STDOUT_FILENO, "\n");
        write_str(STDOUT_FILENO, "Inode: ");
        write_u64(STDOUT_FILENO, st.st_ino);
        write_str(STDOUT_FILENO, "\tLinks: ");
        write_u64(STDOUT_FILENO, st.st_nlink);
        write_str(STDOUT_FILENO, "\n");
        write_str(STDOUT_FILENO, "Access: (0");
        write_oct(STDOUT_FILENO, st.st_mode);
        write_str(STDOUT_FILENO, ")\tUid: ");
        write_u64(STDOUT_FILENO, st.st_uid as u64);
        write_str(STDOUT_FILENO, "\tGid: ");
        write_u64(STDOUT_FILENO, st.st_gid as u64);
        write_str(STDOUT_FILENO, "\n");
        write_str(STDOUT_FILENO, "Access: ");
        write_i64(STDOUT_FILENO, st.st_atime);
        write_str(STDOUT_FILENO, "\n");
        write_str(STDOUT_FILENO, "Modify: ");
        write_i64(STDOUT_FILENO, st.st_mtime);
        write_str(STDOUT_FILENO, "\n");
        write_str(STDOUT_FILENO, "Change: ");
        write_i64(STDOUT_FILENO, st.st_ctime);
        write_str(STDOUT_FILENO, "\n");
    }
    ret
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
