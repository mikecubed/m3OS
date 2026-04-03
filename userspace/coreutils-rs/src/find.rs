//! find — search for files in a directory hierarchy.
#![no_std]
#![no_main]

use syscall_lib::{
    O_DIRECTORY, O_RDONLY, STDERR_FILENO, STDOUT_FILENO, Stat, close, getdents64, lstat_stat, open,
    stat, write, write_str,
};

syscall_lib::entry_point!(main);

/// Simple glob match supporting only `*` as wildcard.
fn glob_matches(mut pat: &[u8], mut s: &[u8]) -> bool {
    loop {
        match pat.first() {
            None => return s.is_empty(),
            Some(&b'*') => {
                pat = &pat[1..];
                if pat.is_empty() {
                    return true;
                }
                // Try matching `pat` starting at each position in `s`.
                for i in 0..=s.len() {
                    if glob_matches(pat, &s[i..]) {
                        return true;
                    }
                }
                return false;
            }
            Some(&pc) => match s.first() {
                None => return false,
                Some(&sc) => {
                    if pc != sc {
                        return false;
                    }
                    pat = &pat[1..];
                    s = &s[1..];
                }
            },
        }
    }
}

fn base_name(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

fn emit_path(text: &[u8], print0: bool) {
    let _ = write(STDOUT_FILENO, text);
    if print0 {
        let _ = write(STDOUT_FILENO, b"\0");
    } else {
        write_str(STDOUT_FILENO, "\n");
    }
}

fn matches(text: &[u8], st: &Stat, name_pat: Option<&[u8]>, type_filter: u8) -> bool {
    if let Some(pat) = name_pat
        && !glob_matches(pat, base_name(text))
    {
        return false;
    }
    if type_filter == b'f' && (st.st_mode & 0xf000) != 0x8000 {
        return false;
    }
    if type_filter == b'd' && (st.st_mode & 0xf000) != 0x4000 {
        return false;
    }
    true
}

/// Recursive find. `path` is a null-terminated byte slice.
fn find_path(path: &[u8], name_pat: Option<&[u8]>, type_filter: u8, print0: bool) {
    let text_len = path.len() - 1;
    let text = &path[..text_len];

    let mut lst = Stat::zeroed();
    if lstat_stat(path, &mut lst) != 0 {
        write_str(STDERR_FILENO, "find: cannot stat '");
        let _ = write(STDERR_FILENO, text);
        write_str(STDERR_FILENO, "'\n");
        return;
    }

    // Follow symlinks for the type check (like find default behaviour).
    let is_symlink = (lst.st_mode & 0xf000) == 0xa000;
    let view_st = if is_symlink {
        let mut st = Stat::zeroed();
        if stat(path, &mut st) == 0 { st } else { lst }
    } else {
        lst
    };

    if matches(text, &view_st, name_pat, type_filter) {
        emit_path(text, print0);
    }

    if (view_st.st_mode & 0xf000) != 0x4000 {
        return;
    }

    let fd = open(path, O_RDONLY | O_DIRECTORY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "find: cannot open '");
        let _ = write(STDERR_FILENO, text);
        write_str(STDERR_FILENO, "'\n");
        return;
    }

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
                find_path(&child_buf[..=pos], name_pat, type_filter, print0);
            }

            off += reclen;
        }
    }

    close(fd as i32);
}

fn main(args: &[&str]) -> i32 {
    let mut argi = 1usize;
    let mut root = ".";
    let mut name_pat: Option<&[u8]> = None;
    let mut type_filter: u8 = 0;
    let mut print0 = false;

    if argi < args.len() && !args[argi].starts_with('-') {
        root = args[argi];
        argi += 1;
    }

    while argi < args.len() {
        match args[argi] {
            "-L" => {
                argi += 1;
            }
            "-name" => {
                if argi + 1 >= args.len() {
                    write_str(
                        STDERR_FILENO,
                        "usage: find [path] [-name PATTERN] [-type f|d]\n",
                    );
                    return 1;
                }
                name_pat = Some(args[argi + 1].as_bytes());
                argi += 2;
            }
            "-type" => {
                if argi + 1 >= args.len() {
                    write_str(
                        STDERR_FILENO,
                        "usage: find [path] [-name PATTERN] [-type f|d]\n",
                    );
                    return 1;
                }
                let t = args[argi + 1].as_bytes();
                if t.is_empty() || (t[0] != b'f' && t[0] != b'd') {
                    write_str(
                        STDERR_FILENO,
                        "usage: find [path] [-name PATTERN] [-type f|d]\n",
                    );
                    return 1;
                }
                type_filter = t[0];
                argi += 2;
            }
            "-print0" => {
                print0 = true;
                argi += 1;
            }
            _ => {
                write_str(
                    STDERR_FILENO,
                    "usage: find [path] [-name PATTERN] [-type f|d]\n",
                );
                return 1;
            }
        }
    }

    let root_bytes = root.as_bytes();
    if root_bytes.len() > 510 {
        write_str(STDERR_FILENO, "find: path too long\n");
        return 1;
    }
    let mut root_buf = [0u8; 512];
    root_buf[..root_bytes.len()].copy_from_slice(root_bytes);
    root_buf[root_bytes.len()] = 0;

    find_path(
        &root_buf[..=root_bytes.len()],
        name_pat,
        type_filter,
        print0,
    );
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
