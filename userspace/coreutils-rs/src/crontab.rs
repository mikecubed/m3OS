//! crontab — manage user crontab files (Phase 46).
//!
//! Usage:
//!   crontab -l              — list current user's crontab
//!   crontab -r              — remove crontab
//!   crontab -u user -l      — list another user's crontab (root only)
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

const PASSWD_PATH: &[u8] = b"/etc/passwd\0";
const STATUS_PATH: &[u8] = b"/var/run/services.status\0";

fn parse_u32_bytes(s: &[u8]) -> Option<u32> {
    let mut val: u32 = 0;
    if s.is_empty() {
        return None;
    }
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(val)
}

fn current_username(uid: u32, user_buf: &mut [u8]) -> Result<&str, &'static str> {
    let fd = syscall_lib::open(PASSWD_PATH, 0, 0);
    if fd < 0 {
        return Err("crontab: cannot read /etc/passwd\n");
    }

    let mut passwd_buf = [0u8; 2048];
    let n = syscall_lib::read(fd as i32, &mut passwd_buf);
    syscall_lib::close(fd as i32);
    if n <= 0 {
        return Err("crontab: cannot read /etc/passwd\n");
    }

    for line in passwd_buf[..n as usize].split(|&b| b == b'\n') {
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
        if field == 6 {
            fields[6] = &line[start..];
            if parse_u32_bytes(fields[2]) == Some(uid) {
                let username = fields[0];
                if username.len() > user_buf.len() {
                    return Err("crontab: username too long\n");
                }
                user_buf[..username.len()].copy_from_slice(username);
                return core::str::from_utf8(&user_buf[..username.len()])
                    .map_err(|_| "crontab: invalid UTF-8 in /etc/passwd\n");
            }
        }
    }

    Err("crontab: cannot find current user\n")
}

fn crontab_path(user: &str, buf: &mut [u8]) -> usize {
    let prefix = b"/var/spool/cron/";
    let user_bytes = user.as_bytes();
    let total = prefix.len() + user_bytes.len() + 1; // +1 for null terminator
    if total > buf.len() {
        return 0;
    }
    buf[..prefix.len()].copy_from_slice(prefix);
    buf[prefix.len()..prefix.len() + user_bytes.len()].copy_from_slice(user_bytes);
    buf[prefix.len() + user_bytes.len()] = 0;
    total
}

fn cmd_list(user: &str) -> i32 {
    let mut path = [0u8; 128];
    let len = crontab_path(user, &mut path);
    if len == 0 {
        write_str(STDERR_FILENO, "crontab: path too long\n");
        return 1;
    }

    let fd = syscall_lib::open(&path[..len], 0, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "no crontab for ");
        write_str(STDERR_FILENO, user);
        write_str(STDERR_FILENO, "\n");
        return 1;
    }

    let mut buf = [0u8; 4096];
    let n = syscall_lib::read(fd as i32, &mut buf);
    syscall_lib::close(fd as i32);

    if n > 0 {
        match core::str::from_utf8(&buf[..n as usize]) {
            Ok(text) => {
                write_str(STDOUT_FILENO, text);
            }
            Err(_) => {
                write_str(STDERR_FILENO, "crontab: invalid UTF-8 in crontab file\n");
                return 1;
            }
        }
    }
    0
}

fn cmd_remove(user: &str) -> i32 {
    let mut path = [0u8; 128];
    let len = crontab_path(user, &mut path);
    if len == 0 {
        write_str(STDERR_FILENO, "crontab: path too long\n");
        return 1;
    }

    let ret = syscall_lib::unlink(&path[..len]); // null-terminated path
    if ret < 0 {
        write_str(STDERR_FILENO, "crontab: no crontab to remove\n");
        return 1;
    }

    // Signal crond to reload by reading its PID from /var/run/services.status.
    let mut status_buf = [0u8; 4096];
    let sn = {
        let fd = syscall_lib::open(STATUS_PATH, 0, 0);
        if fd < 0 {
            0isize
        } else {
            let n = syscall_lib::read(fd as i32, &mut status_buf);
            syscall_lib::close(fd as i32);
            n
        }
    };
    if sn > 0
        && let Ok(text) = core::str::from_utf8(&status_buf[..sn as usize])
    {
        // Format: "<name> <status> pid=<pid> restarts=<count>"
        for line in text.split('\n') {
            if let Some(rest) = line.strip_prefix("crond ")
                && let Some(pid_start) = rest.find("pid=")
            {
                let after_pid = &rest[pid_start + 4..];
                let end = after_pid.find(' ').unwrap_or(after_pid.len());
                if let Ok(pid) = u32_from_str(&after_pid[..end])
                    && pid > 0
                    && pid <= i32::MAX as u32
                {
                    syscall_lib::kill(pid as i32, syscall_lib::SIGHUP);
                }
            }
        }
    }

    write_str(STDOUT_FILENO, "crontab removed\n");
    0
}

/// Parse a decimal u32 from a str (no_std helper).
fn u32_from_str(s: &str) -> Result<u32, ()> {
    let mut val: u32 = 0;
    if s.is_empty() {
        return Err(());
    }
    for &b in s.as_bytes() {
        if !b.is_ascii_digit() {
            return Err(());
        }
        val = val
            .checked_mul(10)
            .ok_or(())?
            .checked_add((b - b'0') as u32)
            .ok_or(())?;
    }
    Ok(val)
}

fn main(args: &[&str]) -> i32 {
    let uid = syscall_lib::getuid();
    let mut user_buf = [0u8; 64];
    let mut user = match current_username(uid, &mut user_buf) {
        Ok(name) => name,
        Err(msg) => {
            write_str(STDERR_FILENO, msg);
            return 1;
        }
    };
    let mut action = "";

    let mut i = 1;
    while i < args.len() {
        match args[i] {
            "-l" => action = "list",
            "-r" => action = "remove",
            "-e" => {
                write_str(
                    STDERR_FILENO,
                    "crontab: interactive editing is not supported yet; edit /var/spool/cron/<user> directly\n",
                );
                return 1;
            }
            "-u" => {
                if uid != 0 {
                    write_str(STDERR_FILENO, "crontab: must be root to use -u\n");
                    return 1;
                }
                if i + 1 < args.len() {
                    i += 1;
                    user = args[i];
                }
            }
            _ => {
                write_str(STDERR_FILENO, "usage: crontab [-u user] {-l | -r}\n");
                return 1;
            }
        }
        i += 1;
    }

    match action {
        "list" => cmd_list(user),
        "remove" => cmd_remove(user),
        _ => {
            write_str(STDERR_FILENO, "usage: crontab [-u user] {-l | -r}\n");
            1
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
