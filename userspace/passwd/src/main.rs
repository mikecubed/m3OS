//! m3OS passwd — change user password (Phase 27).
//!
//! Only root can change passwords (non-root support requires setuid-bit, deferred).
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, exit, fsync, geteuid, getrandom, getuid, open, read, write,
    write_str,
};

const SHADOW_PATH: &[u8] = b"/etc/shadow\0";
const PASSWD_PATH: &[u8] = b"/etc/passwd\0";

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let euid = geteuid();
    if euid != 0 {
        write_str(
            STDOUT_FILENO,
            "passwd: must be root (non-root password change not yet supported)\n",
        );
        exit(1);
    }
    let uid = getuid();
    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(PASSWD_PATH, &mut passwd_buf);
    if passwd_len == 0 {
        write_str(STDOUT_FILENO, "passwd: cannot read /etc/passwd\n");
        exit(1);
    }

    let username = match find_username_by_uid(&passwd_buf[..passwd_len], uid) {
        Some(u) => u,
        None => {
            write_str(STDOUT_FILENO, "passwd: cannot find current user\n");
            exit(1);
        }
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
        exit(1);
    }

    // Generate new hash with random salt and iterated SHA-256.
    let mut salt = [0u8; 16];
    getrandom(&mut salt);
    let hash = syscall_lib::sha256::hash_password_iterated(&new_input[..new_len], &salt, 10000);
    let mut salt_hex = [0u8; 64];
    let salt_hex_len = syscall_lib::sha256::to_hex(&salt, &mut salt_hex);
    let mut hash_hex = [0u8; 64];
    let hash_hex_len = syscall_lib::sha256::to_hex(&hash, &mut hash_hex);

    // Read current shadow file, replace the user's entry, and write it back.
    let mut shadow_buf = [0u8; 2048];
    let shadow_len = read_file(SHADOW_PATH, &mut shadow_buf);
    if shadow_len == 0 {
        write_str(STDOUT_FILENO, "passwd: cannot read shadow file\n");
        exit(1);
    }

    // Build new shadow file content.
    let mut new_shadow = [0u8; 2048];
    let mut out_pos = 0;

    for line in shadow_buf[..shadow_len].split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.iter().position(|&b| b == b':')
            && &line[..colon] == username
        {
            // Replace this line with new hash.
            out_pos += copy_to(&mut new_shadow[out_pos..], username);
            out_pos += copy_to(&mut new_shadow[out_pos..], b":$sha256i$10000$");
            out_pos += copy_to(&mut new_shadow[out_pos..], &salt_hex[..salt_hex_len]);
            out_pos += copy_to(&mut new_shadow[out_pos..], b"$");
            out_pos += copy_to(&mut new_shadow[out_pos..], &hash_hex[..hash_hex_len]);
            out_pos += copy_to(&mut new_shadow[out_pos..], b"::::::\n");
            continue;
        }
        out_pos += copy_to(&mut new_shadow[out_pos..], line);
        out_pos += copy_to(&mut new_shadow[out_pos..], b"\n");
    }

    // Write new shadow file.
    let fd = open(SHADOW_PATH, syscall_lib::O_WRONLY | syscall_lib::O_TRUNC, 0);
    if fd < 0 {
        write_str(STDOUT_FILENO, "passwd: cannot write shadow file\n");
        exit(1);
    }
    let _ = write(fd as i32, &new_shadow[..out_pos]);
    fsync(fd as i32);
    close(fd as i32);

    write_str(STDOUT_FILENO, "passwd: password updated successfully\n");
    exit(0);
}

fn copy_to(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n
}

fn find_username_by_uid(passwd: &[u8], target_uid: u32) -> Option<&[u8]> {
    for line in passwd.split(|&b| b == b'\n') {
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
            let uid = parse_u32(fields[2]);
            if uid == target_uid {
                return Some(fields[0]);
            }
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
        let _ = syscall_lib::tcsetattr(0, &t);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "passwd: PANIC\n");
    exit(101)
}
