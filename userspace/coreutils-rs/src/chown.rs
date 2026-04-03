//! chown — change file owner and group.
#![no_std]
#![no_main]

use syscall_lib::chown as sys_chown;
use syscall_lib::{STDERR_FILENO, Stat, stat, write, write_str};

syscall_lib::entry_point!(main);

fn parse_u32_bytes(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v = 0u64;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (b - b'0') as u64;
        if v > u32::MAX as u64 {
            return None;
        }
    }
    Some(v as u32)
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 3 {
        write_str(STDERR_FILENO, "usage: chown OWNER[:GROUP] FILE...\n");
        return 1;
    }

    let spec = args[1].as_bytes();

    // Split on ':' to separate user and group parts.
    let colon_pos = spec.iter().position(|&b| b == b':');
    let (user_part, group_part, has_colon) = match colon_pos {
        None => (spec, &[][..], false),
        Some(i) => (&spec[..i], &spec[i + 1..], true),
    };

    // u32::MAX == "not specified, keep current"
    let uid_spec: u32 = if user_part.is_empty() {
        u32::MAX
    } else {
        match parse_u32_bytes(user_part) {
            Some(v) => v,
            None => {
                write_str(STDERR_FILENO, "chown: invalid owner '");
                let _ = write(STDERR_FILENO, user_part);
                write_str(STDERR_FILENO, "'\n");
                return 1;
            }
        }
    };

    let gid_spec: u32 = if !has_colon || group_part.is_empty() {
        u32::MAX
    } else {
        match parse_u32_bytes(group_part) {
            Some(v) => v,
            None => {
                write_str(STDERR_FILENO, "chown: invalid group '");
                let _ = write(STDERR_FILENO, group_part);
                write_str(STDERR_FILENO, "'\n");
                return 1;
            }
        }
    };

    let mut status = 0i32;
    for &file in &args[2..] {
        let bytes = file.as_bytes();
        if bytes.len() > 511 {
            write_str(STDERR_FILENO, "chown: path too long\n");
            status = 1;
            continue;
        }
        let mut path = [0u8; 512];
        path[..bytes.len()].copy_from_slice(bytes);
        path[bytes.len()] = 0;

        let mut actual_uid = uid_spec;
        let mut actual_gid = gid_spec;

        if uid_spec == u32::MAX || gid_spec == u32::MAX {
            let mut st = Stat::zeroed();
            if stat(&path[..=bytes.len()], &mut st) != 0 {
                write_str(STDERR_FILENO, "chown: cannot stat '");
                let _ = write(STDERR_FILENO, bytes);
                write_str(STDERR_FILENO, "'\n");
                status = 1;
                continue;
            }
            if uid_spec == u32::MAX {
                actual_uid = st.st_uid;
            }
            if gid_spec == u32::MAX {
                actual_gid = st.st_gid;
            }
        }

        if sys_chown(&path[..=bytes.len()], actual_uid, actual_gid) != 0 {
            write_str(STDERR_FILENO, "chown: cannot change '");
            let _ = write(STDERR_FILENO, bytes);
            write_str(STDERR_FILENO, "'\n");
            status = 1;
        }
    }
    status
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
