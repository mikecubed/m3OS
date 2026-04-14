//! m3OS whoami — print effective username (Phase 27).
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, exit, geteuid, open, read, write, write_str, write_u64,
};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let euid = geteuid();

    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(b"/etc/passwd\0", &mut passwd_buf);

    // Look up username by euid in field 2 of /etc/passwd.
    for line in passwd_buf[..passwd_len].split(|&b| b == b'\n') {
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
        if field > 2 {
            let Some(uid) = parse_u32(fields[2]) else {
                continue;
            };
            if uid == euid {
                let _ = write(STDOUT_FILENO, fields[0]);
                write_str(STDOUT_FILENO, "\n");
                exit(0);
            }
        }
    }

    // Fall back to numeric UID.
    write_u64(STDOUT_FILENO, euid as u64);
    write_str(STDOUT_FILENO, "\n");
    exit(0);
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    let mut n: u32 = 0;
    let mut saw_digit = false;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        saw_digit = true;
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    if saw_digit { Some(n) } else { None }
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
    write_str(STDOUT_FILENO, "whoami: PANIC\n");
    exit(101)
}
