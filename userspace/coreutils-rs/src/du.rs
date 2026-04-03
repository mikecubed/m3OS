//! du — estimate file space usage.
#![no_std]
#![no_main]

use syscall_lib::{
    O_DIRECTORY, O_RDONLY, STDERR_FILENO, STDOUT_FILENO, Stat, close, getdents64, lstat_stat, open,
    write, write_str, write_u64,
};

syscall_lib::entry_point!(main);

fn disk_usage_bytes(st: &Stat) -> u64 {
    if st.st_blocks > 0 {
        st.st_blocks as u64 * 512
    } else {
        st.st_size.max(0) as u64
    }
}

fn write_human_size(size: u64) {
    const SUFFIXES: [u8; 5] = [b'B', b'K', b'M', b'G', b'T'];
    let mut suffix = 0usize;
    let mut whole = size;
    let mut rem = 0u64;
    while whole >= 1024 && suffix + 1 < SUFFIXES.len() {
        rem = whole % 1024;
        whole /= 1024;
        suffix += 1;
    }
    write_u64(STDOUT_FILENO, whole);
    if suffix > 0 {
        write_str(STDOUT_FILENO, ".");
        write_u64(STDOUT_FILENO, (rem * 10) / 1024);
    }
    let suf = [SUFFIXES[suffix]];
    let _ = write(STDOUT_FILENO, &suf);
}

fn print_total(total: u64, path: &[u8], human: bool) {
    if human {
        write_human_size(total);
    } else {
        write_u64(STDOUT_FILENO, total / 1024);
    }
    write_str(STDOUT_FILENO, "\t");
    let _ = write(STDOUT_FILENO, path);
    write_str(STDOUT_FILENO, "\n");
}

/// Recursive disk usage. `path` is a null-terminated byte slice.
fn du_path(path: &[u8], summarize: bool, human: bool, is_top: bool) -> u64 {
    let text_len = path.len() - 1; // length excluding null terminator
    let text = &path[..text_len];

    let mut st = Stat::zeroed();
    if lstat_stat(path, &mut st) != 0 {
        write_str(STDERR_FILENO, "du: cannot stat '");
        let _ = write(STDERR_FILENO, text);
        write_str(STDERR_FILENO, "'\n");
        return 0;
    }

    let entry_bytes = disk_usage_bytes(&st);
    let is_dir = (st.st_mode & 0xf000) == 0x4000;

    if !is_dir {
        print_total(entry_bytes, text, human);
        return entry_bytes;
    }

    let fd = open(path, O_RDONLY | O_DIRECTORY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "du: cannot open '");
        let _ = write(STDERR_FILENO, text);
        write_str(STDERR_FILENO, "'\n");
        return entry_bytes;
    }

    let mut total = entry_bytes;
    let mut dents_buf = [0u8; 1024];

    loop {
        let n = getdents64(fd as i32, &mut dents_buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            break;
        }
        let mut off = 0usize;
        while off < n as usize {
            if off + 19 > n as usize {
                break;
            }
            let reclen = u16::from_ne_bytes([dents_buf[off + 16], dents_buf[off + 17]]) as usize;
            if reclen < 19 || off + reclen > n as usize {
                break;
            }
            let name_start = off + 19;
            let name_bytes = &dents_buf[name_start..off + reclen];
            let name_len = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            let name = &name_bytes[..name_len];

            if name == b"." || name == b".." {
                off += reclen;
                continue;
            }

            // Build child path: text + '/' (if needed) + name + NUL
            let need_slash = text_len > 0 && text[text_len - 1] != b'/';
            let slash_len = if need_slash { 1 } else { 0 };
            let child_text_len = text_len + slash_len + name_len;
            if child_text_len < 511 {
                let mut child_buf = [0u8; 512];
                child_buf[..text_len].copy_from_slice(text);
                let mut pos = text_len;
                if need_slash {
                    child_buf[pos] = b'/';
                    pos += 1;
                }
                child_buf[pos..pos + name_len].copy_from_slice(name);
                pos += name_len;
                child_buf[pos] = 0;
                total += du_path(&child_buf[..=pos], summarize, human, false);
            }

            off += reclen;
        }
    }

    close(fd as i32);

    if !summarize || is_top {
        print_total(total, text, human);
    }
    total
}

fn main(args: &[&str]) -> i32 {
    let mut argi = 1usize;
    let mut summarize = false;
    let mut human = false;

    while argi < args.len() && args[argi].starts_with('-') && args[argi].len() > 1 {
        match args[argi] {
            "-s" => summarize = true,
            "-h" => human = true,
            _ => {
                write_str(STDERR_FILENO, "usage: du [-s] [-h] [path...]\n");
                return 1;
            }
        }
        argi += 1;
    }

    if argi == args.len() {
        let path = b".\0";
        du_path(path, summarize, human, true);
        return 0;
    }

    for arg in &args[argi..] {
        let bytes = arg.as_bytes();
        if bytes.len() > 510 {
            write_str(STDERR_FILENO, "du: path too long\n");
            continue;
        }
        let mut path_buf = [0u8; 512];
        path_buf[..bytes.len()].copy_from_slice(bytes);
        path_buf[bytes.len()] = 0;
        du_path(&path_buf[..=bytes.len()], summarize, human, true);
    }
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
