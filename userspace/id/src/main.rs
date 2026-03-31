//! m3OS id — print user identity (Phase 27).
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, exit, getgid, getuid, open, read, write, write_str, write_u64,
};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let uid = getuid();
    let gid = getgid();

    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(b"/etc/passwd\0", &mut passwd_buf);

    let mut group_buf = [0u8; 2048];
    let group_len = read_file(b"/etc/group\0", &mut group_buf);

    write_str(STDOUT_FILENO, "uid=");
    write_u64(STDOUT_FILENO, uid as u64);
    if let Some(name) = lookup_name(&passwd_buf[..passwd_len], uid, 2) {
        write_str(STDOUT_FILENO, "(");
        let _ = write(STDOUT_FILENO, name);
        write_str(STDOUT_FILENO, ")");
    }

    write_str(STDOUT_FILENO, " gid=");
    write_u64(STDOUT_FILENO, gid as u64);
    if let Some(name) = lookup_name(&group_buf[..group_len], gid, 2) {
        write_str(STDOUT_FILENO, "(");
        let _ = write(STDOUT_FILENO, name);
        write_str(STDOUT_FILENO, ")");
    }

    write_str(STDOUT_FILENO, "\n");
    exit(0);
}

/// Look up a name (field 0) by numeric id in the given field index.
/// Works for both /etc/passwd (uid at field 2) and /etc/group (gid at field 2).
fn lookup_name(data: &[u8], target_id: u32, id_field: usize) -> Option<&[u8]> {
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let mut fields = [&[] as &[u8]; 7];
        let mut start = 0;
        let mut field = 0;
        for (i, &b) in line.iter().enumerate() {
            if b == b':' && field < 7 {
                fields[field] = &line[start..i];
                field += 1;
                start = i + 1;
            }
        }
        if field > 0 {
            fields[field.min(6)] = &line[start..];
        }
        if field > id_field && parse_u32(fields[id_field]) == target_id {
            return Some(fields[0]);
        }
    }
    None
}

fn parse_u32(s: &[u8]) -> u32 {
    let mut n: u32 = 0;
    for &b in s {
        if b.is_ascii_digit() {
            n = n.wrapping_mul(10).wrapping_add((b - b'0') as u32);
        }
    }
    n
}

fn read_file(path: &[u8], buf: &mut [u8]) -> usize {
    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }
    let mut total = 0;
    loop {
        let n = read(fd as i32, &mut buf[total..]);
        if n <= 0 {
            break;
        }
        total += n as usize;
        if total >= buf.len() {
            break;
        }
    }
    close(fd as i32);
    total
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "id: PANIC\n");
    exit(101)
}
