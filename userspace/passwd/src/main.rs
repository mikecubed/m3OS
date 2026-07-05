//! m3OS passwd — change user password (Phase 27).
//!
//! Only root can change passwords (non-root support requires setuid-bit, deferred).
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;
use passwd::{
    ShadowRewriteError, find_username_by_uid, requested_username, rewrite_shadow_file, user_exists,
};
use shadow::{ShadowError, shadow_write_atomic};
use syscall_lib::argon2::{DEFAULT_PARAMS, build_shadow_field};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, geteuid, getrandom, getuid, open, read, write, write_str,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "passwd: alloc error\n");
    syscall_lib::exit(1)
}

const SHADOW_PATH: &[u8] = b"/etc/shadow\0";
const SHADOW_PATH_STR: &str = "/etc/shadow";
const PASSWD_PATH: &[u8] = b"/etc/passwd\0";

syscall_lib::entry_point!(passwd_main);

fn passwd_main(args: &[&str]) -> i32 {
    let euid = geteuid();
    if euid != 0 {
        write_str(
            STDOUT_FILENO,
            "passwd: must be root (non-root password change not yet supported)\n",
        );
        return 1;
    }
    let uid = getuid();
    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(PASSWD_PATH, &mut passwd_buf);
    if passwd_len == 0 {
        write_str(STDOUT_FILENO, "passwd: cannot read /etc/passwd\n");
        return 1;
    }

    let current_username = match find_username_by_uid(&passwd_buf[..passwd_len], uid) {
        Some(u) => u,
        None => {
            write_str(STDOUT_FILENO, "passwd: cannot find current user\n");
            return 1;
        }
    };
    let username = match requested_username(args) {
        Some(target) => {
            if !user_exists(&passwd_buf[..passwd_len], target) {
                write_str(STDOUT_FILENO, "passwd: unknown user\n");
                return 1;
            }
            target
        }
        None => current_username,
    };

    write_str(STDOUT_FILENO, "Changing password for ");
    let _ = write(STDOUT_FILENO, username);
    write_str(STDOUT_FILENO, "\n");

    // Get new password.
    write_str(STDOUT_FILENO, "New password: ");
    let saved = disable_echo();
    let mut new_input = [0u8; 128];
    let new_len = read_line(&mut new_input);
    restore_echo(saved);
    let _ = write(STDOUT_FILENO, b"\n");

    write_str(STDOUT_FILENO, "Retype new password: ");
    let saved2 = disable_echo();
    let mut confirm = [0u8; 128];
    let confirm_len = read_line(&mut confirm);
    restore_echo(saved2);
    let _ = write(STDOUT_FILENO, b"\n");

    if new_len != confirm_len || new_input[..new_len] != confirm[..confirm_len] {
        write_str(STDOUT_FILENO, "passwd: passwords don't match\n");
        return 1;
    }

    // Generate the new hash with a random salt using argon2id (Phase 110).
    let mut salt = [0u8; 16];
    if getrandom(&mut salt) != 16 {
        write_str(STDOUT_FILENO, "passwd: failed to generate random salt\n");
        return 1;
    }
    let mut hash_field = [0u8; 200];
    let hash_field_len = match build_shadow_field(
        &new_input[..new_len],
        &salt,
        &DEFAULT_PARAMS,
        &mut hash_field,
    ) {
        Some(len) => len,
        None => {
            write_str(STDOUT_FILENO, "passwd: failed to hash password\n");
            return 1;
        }
    };

    // Read current shadow file, replace the user's entry, and write it back.
    let mut shadow_buf = [0u8; 2048];
    let shadow_len = read_file(SHADOW_PATH, &mut shadow_buf);
    if shadow_len == 0 {
        write_str(STDOUT_FILENO, "passwd: cannot read shadow file\n");
        return 1;
    }

    // Build new shadow file content.
    let mut new_shadow = [0u8; 2048];
    let out_pos = match rewrite_shadow_file(
        &shadow_buf[..shadow_len],
        username,
        &hash_field[..hash_field_len],
        &mut new_shadow,
    ) {
        Ok(len) => len,
        Err(ShadowRewriteError::UserNotFound) => {
            write_str(STDOUT_FILENO, "passwd: user is missing from /etc/shadow\n");
            return 1;
        }
        Err(ShadowRewriteError::OutputTooLarge) => {
            write_str(
                STDOUT_FILENO,
                "passwd: shadow file update exceeded buffer\n",
            );
            return 1;
        }
    };

    // Atomic write: open /etc/shadow.new, fsync, rename over /etc/shadow.
    // Phase 66 Track B — a crash mid-write leaves the live shadow intact.
    match shadow_write_atomic(SHADOW_PATH_STR, &new_shadow[..out_pos]) {
        Ok(()) => {}
        Err(ShadowError::OpenFailed(_)) => {
            write_str(STDOUT_FILENO, "passwd: cannot create /etc/shadow.new\n");
            return 1;
        }
        Err(ShadowError::WriteFailed(_) | ShadowError::ShortWrite { .. }) => {
            write_str(STDOUT_FILENO, "passwd: failed to write /etc/shadow.new\n");
            return 1;
        }
        Err(ShadowError::FsyncFailed(_)) => {
            write_str(
                STDOUT_FILENO,
                "passwd: fsync failed on /etc/shadow.new — not committing\n",
            );
            return 1;
        }
        Err(ShadowError::RenameFailed(_)) => {
            write_str(
                STDOUT_FILENO,
                "passwd: failed to rename /etc/shadow.new over /etc/shadow\n",
            );
            return 1;
        }
        Err(ShadowError::PathTooLong) => {
            write_str(STDOUT_FILENO, "passwd: shadow path too long\n");
            return 1;
        }
    }

    write_str(STDOUT_FILENO, "passwd: password updated successfully\n");
    write_str(
        STDOUT_FILENO,
        "[security] getrandom salt + argon2id hash written\n",
    );
    0
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

fn read_line(buf: &mut [u8]) -> usize {
    let mut pos = 0;
    loop {
        let mut byte = [0u8; 1];
        let n = read(0, &mut byte);
        if n <= 0 || byte[0] == b'\n' || byte[0] == b'\r' {
            break;
        }
        if pos < buf.len() {
            buf[pos] = byte[0];
            pos += 1;
        }
    }
    pos
}

fn disable_echo() -> Option<syscall_lib::Termios> {
    if let Ok(t) = syscall_lib::tcgetattr(0) {
        let mut raw = t;
        raw.c_lflag &= !(syscall_lib::ECHO | syscall_lib::ECHOE);
        let _ = syscall_lib::tcsetattr(0, &raw);
        Some(t)
    } else {
        None
    }
}

fn restore_echo(saved: Option<syscall_lib::Termios>) {
    if let Some(t) = saved {
        let _ = syscall_lib::tcsetattr_flush(0, &t);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "passwd: PANIC\n");
    syscall_lib::exit(101)
}
