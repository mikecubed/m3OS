//! m3OS passwd — change user password (Phase 27).
//!
//! Only root can change passwords (non-root support requires setuid-bit, deferred).
#![no_std]
#![no_main]

use passwd::{
    ShadowRewriteError, build_hash_field, find_username_by_uid, requested_username,
    rewrite_shadow_file, user_exists,
};
use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, fsync, geteuid, getrandom, getuid, open, read, write, write_str,
};

const SHADOW_PATH: &[u8] = b"/etc/shadow\0";
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

    // Generate new hash with random salt and iterated SHA-256.
    let mut salt = [0u8; 16];
    if getrandom(&mut salt) != 16 {
        write_str(STDOUT_FILENO, "passwd: failed to generate random salt\n");
        return 1;
    }
    let hash = syscall_lib::sha256::hash_password_iterated(&new_input[..new_len], &salt, 10000);
    let mut salt_hex = [0u8; 64];
    let salt_hex_len = syscall_lib::sha256::to_hex(&salt, &mut salt_hex);
    let mut hash_hex = [0u8; 64];
    let hash_hex_len = syscall_lib::sha256::to_hex(&hash, &mut hash_hex);
    let mut hash_field = [0u8; 128];
    let hash_field_len = match build_hash_field(
        &salt_hex[..salt_hex_len],
        &hash_hex[..hash_hex_len],
        &mut hash_field,
    ) {
        Some(len) => len,
        None => {
            write_str(STDOUT_FILENO, "passwd: updated hash is too large\n");
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

    // Write new shadow file.
    let fd = open(SHADOW_PATH, syscall_lib::O_WRONLY | syscall_lib::O_TRUNC, 0);
    if fd < 0 {
        write_str(STDOUT_FILENO, "passwd: cannot write shadow file\n");
        return 1;
    }
    let written = write(fd as i32, &new_shadow[..out_pos]);
    if written < 0 || written as usize != out_pos {
        write_str(STDOUT_FILENO, "passwd: failed to fully write shadow file\n");
        close(fd as i32);
        return 1;
    }
    if fsync(fd as i32) < 0 {
        write_str(
            STDOUT_FILENO,
            "passwd: warning: fsync failed on shadow file\n",
        );
    }
    close(fd as i32);

    write_str(STDOUT_FILENO, "passwd: password updated successfully\n");
    write_str(
        STDOUT_FILENO,
        "[security] getrandom salt + iterated SHA-256 hash written\n",
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
