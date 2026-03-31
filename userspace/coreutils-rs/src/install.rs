//! install — copy files and create directories.
#![no_std]
#![no_main]

use syscall_lib::{
    O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, STDERR_FILENO, close, mkdir, open, read, write, write_str,
};

syscall_lib::entry_point!(main);

fn copy_file(src: &str, dst: &str) -> i32 {
    let sb = src.as_bytes();
    let db = dst.as_bytes();
    if sb.len() > 254 || db.len() > 254 {
        write_str(STDERR_FILENO, "install: path too long\n");
        return 1;
    }
    let mut spath = [0u8; 256];
    spath[..sb.len()].copy_from_slice(sb);
    spath[sb.len()] = 0;
    let mut dpath = [0u8; 256];
    dpath[..db.len()].copy_from_slice(db);
    dpath[db.len()] = 0;

    let in_fd = open(&spath[..=sb.len()], O_RDONLY, 0);
    if in_fd < 0 {
        write_str(STDERR_FILENO, "install: cannot open source: ");
        write_str(STDERR_FILENO, src);
        write_str(STDERR_FILENO, "\n");
        return 1;
    }
    let in_fd = in_fd as i32;

    let out_fd = open(&dpath[..=db.len()], O_WRONLY | O_CREAT | O_TRUNC, 0o755);
    if out_fd < 0 {
        write_str(STDERR_FILENO, "install: cannot create: ");
        write_str(STDERR_FILENO, dst);
        write_str(STDERR_FILENO, "\n");
        close(in_fd);
        return 1;
    }
    let out_fd = out_fd as i32;

    let mut buf = [0u8; 4096];
    loop {
        let n = read(in_fd, &mut buf);
        if n <= 0 {
            break;
        }
        let mut off = 0usize;
        let total = n as usize;
        while off < total {
            let w = write(out_fd, &buf[off..total]);
            if w <= 0 {
                close(in_fd);
                close(out_fd);
                return 1;
            }
            off += w as usize;
        }
    }
    close(in_fd);
    close(out_fd);
    0
}

fn main(args: &[&str]) -> i32 {
    let mut dir_mode = false;
    let mut first_arg = 1;

    if args.len() >= 2 && args[1] == "-d" {
        dir_mode = true;
        first_arg = 2;
    }

    if first_arg >= args.len() {
        write_str(STDERR_FILENO, "usage: install [-d] DIR...\n");
        write_str(STDERR_FILENO, "       install SRC DEST\n");
        return 1;
    }

    if dir_mode {
        let mut ret = 0;
        for arg in &args[first_arg..] {
            let bytes = arg.as_bytes();
            if bytes.len() > 254 {
                write_str(STDERR_FILENO, "install: path too long\n");
                ret = 1;
                continue;
            }
            let mut path = [0u8; 256];
            path[..bytes.len()].copy_from_slice(bytes);
            path[bytes.len()] = 0;
            if mkdir(&path[..=bytes.len()], 0o755) < 0 {
                // Ignore EEXIST.
                let mut st = syscall_lib::Stat::zeroed();
                if syscall_lib::stat(&path[..=bytes.len()], &mut st) >= 0
                    && (st.st_mode & 0xF000) == 0x4000
                {
                    continue;
                }
                write_str(STDERR_FILENO, "install: cannot create directory: ");
                write_str(STDERR_FILENO, arg);
                write_str(STDERR_FILENO, "\n");
                ret = 1;
            }
        }
        return ret;
    }

    let remaining = &args[first_arg..];
    if remaining.len() != 2 {
        write_str(STDERR_FILENO, "install: expected SRC DEST\n");
        return 1;
    }
    copy_file(remaining[0], remaining[1])
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
