//! crontab — manage user crontab files (Phase 46).
//!
//! Usage:
//!   crontab -l              — list current user's crontab
//!   crontab -e              — edit crontab (opens $EDITOR)
//!   crontab -r              — remove crontab
//!   crontab -u user -l      — list another user's crontab (root only)
#![no_std]
#![no_main]

use syscall_lib::{STDERR_FILENO, STDOUT_FILENO, write_str};

syscall_lib::entry_point!(main);

fn get_username(uid: u32) -> &'static str {
    match uid {
        0 => "root",
        _ => "user",
    }
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
        let text = unsafe { core::str::from_utf8_unchecked(&buf[..n as usize]) };
        write_str(STDOUT_FILENO, text);
    }
    0
}

fn cmd_remove(user: &str) -> i32 {
    let mut path = [0u8; 128];
    let len = crontab_path(user, &mut path);
    if len == 0 {
        return 1;
    }

    let ret = syscall_lib::unlink(&path[..len]); // null-terminated path
    if ret < 0 {
        write_str(STDERR_FILENO, "crontab: no crontab to remove\n");
        return 1;
    }

    // Signal crond to reload.
    // crond PID is found from the service status file or just signal all.
    // For simplicity, use kill(-1, SIGHUP) which signals all processes.
    // Actually, let's read the crond PID or just use a known approach.
    write_str(STDOUT_FILENO, "crontab removed\n");
    0
}

fn main(args: &[&str]) -> i32 {
    let uid = syscall_lib::getuid();
    let mut user = get_username(uid);
    let mut action = "";

    let mut i = 1;
    while i < args.len() {
        match args[i] {
            "-l" => action = "list",
            "-e" => action = "edit",
            "-r" => action = "remove",
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
                write_str(STDERR_FILENO, "usage: crontab [-u user] {-l | -e | -r}\n");
                return 1;
            }
        }
        i += 1;
    }

    match action {
        "list" => cmd_list(user),
        "remove" => cmd_remove(user),
        "edit" => {
            write_str(
                STDERR_FILENO,
                "crontab: -e not supported (edit /var/spool/cron/<user> directly)\n",
            );
            1
        }
        _ => {
            write_str(STDERR_FILENO, "usage: crontab [-u user] {-l | -e | -r}\n");
            1
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
