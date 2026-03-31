//! ls — list directory entries.
#![no_std]
#![no_main]

use syscall_lib::{
    O_DIRECTORY, O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, getdents64, newfstatat, open,
    write, write_str,
};

syscall_lib::entry_point!(main);

fn main(args: &[&str]) -> i32 {
    let mut long_format = false;
    let mut path = ".";

    for arg in &args[1..] {
        if arg.starts_with('-') {
            for c in arg.bytes().skip(1) {
                if c == b'l' {
                    long_format = true;
                }
            }
        } else {
            path = arg;
        }
    }

    let path_bytes = path.as_bytes();
    if path_bytes.len() > 254 {
        write_str(STDERR_FILENO, "ls: path too long\n");
        return 1;
    }
    let mut path_buf = [0u8; 256];
    path_buf[..path_bytes.len()].copy_from_slice(path_bytes);
    path_buf[path_bytes.len()] = 0;

    let fd = open(&path_buf[..=path_bytes.len()], O_RDONLY | O_DIRECTORY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "ls: cannot open directory\n");
        return 1;
    }

    // Build base path for stat calls (with trailing slash).
    let mut base = [0u8; 256];
    let mut base_len = path_bytes.len();
    base[..base_len].copy_from_slice(path_bytes);
    if base_len > 0 && base[base_len - 1] != b'/' {
        base[base_len] = b'/';
        base_len += 1;
    }

    let mut dirent_buf = [0u8; 2048];
    let mut ret = 0;

    loop {
        let nread = getdents64(fd as i32, &mut dirent_buf);
        if nread == 0 {
            break;
        }
        if nread < 0 {
            write_str(STDERR_FILENO, "ls: getdents64 error\n");
            ret = 1;
            break;
        }
        let mut pos = 0usize;
        while pos < nread as usize {
            // Parse linux_dirent64: d_ino(8) + d_off(8) + d_reclen(2) + d_type(1) + d_name[]
            if pos + 19 > nread as usize {
                break;
            }
            let d_reclen =
                u16::from_ne_bytes([dirent_buf[pos + 16], dirent_buf[pos + 17]]) as usize;
            if d_reclen == 0 {
                break;
            }

            // d_name starts at offset 19.
            let name_start = pos + 19;
            let name_end = (pos + d_reclen).min(nread as usize);
            let name_bytes = &dirent_buf[name_start..name_end];
            let name_len = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            let name = &name_bytes[..name_len];

            // Skip . and ..
            if name == b"." || name == b".." {
                pos += d_reclen;
                continue;
            }

            if long_format {
                print_long_entry(&base[..base_len], name);
            }

            let _ = write(STDOUT_FILENO, name);
            write_str(STDOUT_FILENO, "\n");

            pos += d_reclen;
        }
    }

    close(fd as i32);
    ret
}

fn print_long_entry(base: &[u8], name: &[u8]) {
    // Build full path: base + name + NUL
    let total = base.len() + name.len();
    if total > 510 {
        write_str(STDOUT_FILENO, "?????????? ? ? ? ");
        return;
    }
    let mut fullpath = [0u8; 512];
    fullpath[..base.len()].copy_from_slice(base);
    fullpath[base.len()..base.len() + name.len()].copy_from_slice(name);
    fullpath[total] = 0;

    let mut stat_buf = [0u8; 144];
    if newfstatat(&fullpath[..=total], &mut stat_buf) != 0 {
        write_str(STDOUT_FILENO, "?????????? ? ? ? ");
        return;
    }

    // Parse stat fields.
    let mode = u32::from_ne_bytes([stat_buf[24], stat_buf[25], stat_buf[26], stat_buf[27]]);
    let uid = u32::from_ne_bytes([stat_buf[28], stat_buf[29], stat_buf[30], stat_buf[31]]);
    let gid = u32::from_ne_bytes([stat_buf[32], stat_buf[33], stat_buf[34], stat_buf[35]]);
    let size = u64::from_ne_bytes([
        stat_buf[48],
        stat_buf[49],
        stat_buf[50],
        stat_buf[51],
        stat_buf[52],
        stat_buf[53],
        stat_buf[54],
        stat_buf[55],
    ]);

    // Format mode string.
    let mut mode_str = [b'-'; 10];
    let ft = mode & 0o170000;
    mode_str[0] = if ft == 0o040000 {
        b'd'
    } else if ft == 0o120000 {
        b'l'
    } else {
        b'-'
    };
    if mode & 0o400 != 0 {
        mode_str[1] = b'r';
    }
    if mode & 0o200 != 0 {
        mode_str[2] = b'w';
    }
    if mode & 0o100 != 0 {
        mode_str[3] = b'x';
    }
    if mode & 0o040 != 0 {
        mode_str[4] = b'r';
    }
    if mode & 0o020 != 0 {
        mode_str[5] = b'w';
    }
    if mode & 0o010 != 0 {
        mode_str[6] = b'x';
    }
    if mode & 0o004 != 0 {
        mode_str[7] = b'r';
    }
    if mode & 0o002 != 0 {
        mode_str[8] = b'w';
    }
    if mode & 0o001 != 0 {
        mode_str[9] = b'x';
    }
    let _ = write(STDOUT_FILENO, &mode_str);
    write_str(STDOUT_FILENO, " ");

    write_padded_uint(uid as u64, 5);
    write_str(STDOUT_FILENO, " ");
    write_padded_uint(gid as u64, 5);
    write_str(STDOUT_FILENO, " ");
    write_padded_uint(size, 8);
    write_str(STDOUT_FILENO, " ");
}

fn write_padded_uint(v: u64, width: usize) {
    let mut buf = [0u8; 20];
    let mut pos = 20;
    if v == 0 {
        pos -= 1;
        buf[pos] = b'0';
    } else {
        let mut n = v;
        while n > 0 {
            pos -= 1;
            buf[pos] = b'0' + (n % 10) as u8;
            n /= 10;
        }
    }
    let digits = 20 - pos;
    // Pad with spaces.
    for _ in digits..width {
        write_str(STDOUT_FILENO, " ");
    }
    let _ = write(STDOUT_FILENO, &buf[pos..]);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
