//! Authentication callbacks (Track D).
//!
//! Validates SSH credentials against /etc/passwd and /etc/shadow (password auth)
//! or ~/.ssh/authorized_keys (public key auth).

use syscall_lib::{O_RDONLY, close, open};

const PASSWD_PATH: &[u8] = b"/etc/passwd\0";
const SHADOW_PATH: &[u8] = b"/etc/shadow\0";

/// D.1: Check password against /etc/shadow.
/// Returns Some((uid, gid, home, shell)) on success.
pub fn check_password(username: &str, password: &str) -> Option<UserInfo> {
    // Look up user in /etc/passwd.
    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(PASSWD_PATH, &mut passwd_buf);
    if passwd_len == 0 {
        return None;
    }

    let user_info = find_user(&passwd_buf[..passwd_len], username.as_bytes())?;

    // Verify password against /etc/shadow.
    let mut shadow_buf = [0u8; 2048];
    let shadow_len = read_file(SHADOW_PATH, &mut shadow_buf);
    if shadow_len == 0 {
        return None;
    }

    if !verify_shadow(
        &shadow_buf[..shadow_len],
        username.as_bytes(),
        password.as_bytes(),
    ) {
        return None;
    }

    Some(user_info)
}

/// D.2: Check if a public key is authorized for the given user.
/// Returns Some(UserInfo) on success.
pub fn check_pubkey(username: &str, pubkey_bytes: &[u8]) -> Option<UserInfo> {
    // Look up user in /etc/passwd to get home directory.
    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(PASSWD_PATH, &mut passwd_buf);
    if passwd_len == 0 {
        return None;
    }

    let user_info = find_user(&passwd_buf[..passwd_len], username.as_bytes())?;

    // Build path to authorized_keys: /home/<user>/.ssh/authorized_keys
    let mut ak_path = [0u8; 256];
    let suffix = b"/.ssh/authorized_keys\0";
    let home = user_info.home.as_bytes();
    let required_len = home.len() + suffix.len();
    if required_len > ak_path.len() {
        return None; // Path would overflow buffer.
    }
    ak_path[..home.len()].copy_from_slice(home);
    ak_path[home.len()..required_len].copy_from_slice(suffix);

    // Read authorized_keys file.
    let mut ak_buf = [0u8; 2048];
    let ak_len = read_file(&ak_path[..required_len], &mut ak_buf);
    if ak_len == 0 {
        return None;
    }

    // Parse each line: hex-encoded 32-byte Ed25519 public key.
    for line in ak_buf[..ak_len].split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }

        // Try to parse as hex (64 hex chars = 32 bytes).
        let mut key = [0u8; 32];
        if hex_decode(line, &mut key) == 32 && key == pubkey_bytes {
            return Some(user_info.clone());
        }
    }

    None
}

/// User account information from /etc/passwd.
#[derive(Clone)]
pub struct UserInfo {
    pub username: alloc::string::String,
    pub uid: u32,
    pub gid: u32,
    pub home: alloc::string::String,
    pub shell: alloc::string::String,
}

extern crate alloc;

/// Parse /etc/passwd to find a user entry.
fn find_user(passwd: &[u8], username: &[u8]) -> Option<UserInfo> {
    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields = split_colon(line)?;
        if fields[0] == username {
            let uid = parse_u32(fields[2]);
            let gid = parse_u32(fields[3]);
            let uname = core::str::from_utf8(fields[0]).ok()?;
            let home = core::str::from_utf8(fields[5]).ok()?;
            let shell = core::str::from_utf8(fields[6]).ok()?;
            return Some(UserInfo {
                username: alloc::string::String::from(uname),
                uid,
                gid,
                home: alloc::string::String::from(home),
                shell: alloc::string::String::from(shell),
            });
        }
    }
    None
}

/// Split a line on ':' into exactly 7 fields.
fn split_colon(line: &[u8]) -> Option<[&[u8]; 7]> {
    let mut fields = [&[] as &[u8]; 7];
    let mut start = 0;
    let mut field = 0;
    for (i, &b) in line.iter().enumerate() {
        if b == b':' {
            if field >= 7 {
                return None;
            }
            fields[field] = &line[start..i];
            field += 1;
            start = i + 1;
        }
    }
    if field == 6 {
        fields[6] = &line[start..];
        Some(fields)
    } else {
        None
    }
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

/// Verify password against /etc/shadow.
fn verify_shadow(shadow: &[u8], username: &[u8], password: &[u8]) -> bool {
    for line in shadow.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.iter().position(|&b| b == b':') {
            let name = &line[..colon];
            if name == username {
                let rest = &line[colon + 1..];
                let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
                let hash_field = &rest[..hash_end];
                return syscall_lib::sha256::verify_password(password, hash_field);
            }
        }
    }
    false
}

/// Read an entire file into a buffer. Returns bytes read.
fn read_file(path: &[u8], buf: &mut [u8]) -> usize {
    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        return 0;
    }
    let mut total = 0;
    loop {
        let n = syscall_lib::read(fd as i32, &mut buf[total..]);
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

/// Decode hex string into bytes. Returns number of bytes decoded.
fn hex_decode(hex: &[u8], out: &mut [u8]) -> usize {
    let mut i = 0;
    let mut o = 0;
    while i + 1 < hex.len() && o < out.len() {
        let hi = hex_val(hex[i]);
        let lo = hex_val(hex[i + 1]);
        if hi > 15 || lo > 15 {
            break;
        }
        out[o] = (hi << 4) | lo;
        i += 2;
        o += 1;
    }
    o
}

fn hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 255,
    }
}
