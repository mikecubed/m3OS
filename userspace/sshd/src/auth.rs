//! Authentication callbacks (Track D).
//!
//! Validates SSH credentials against /etc/passwd and /etc/shadow (password auth)
//! or ~/.ssh/authorized_keys (public key auth).

use core::ops::ControlFlow;
use syscall_lib::{O_RDONLY, close, open};

const PASSWD_PATH: &[u8] = b"/etc/passwd\0";
const SHADOW_PATH: &[u8] = b"/etc/shadow\0";
const FILE_CHUNK_SIZE: usize = 512;
const MAX_AUTH_LINE_LEN: usize = 4096;

/// D.1: Check password against /etc/shadow.
/// Returns Some((uid, gid, home, shell)) on success.
pub fn check_password(username: &str, password: &str) -> Option<UserInfo> {
    // Look up the passwd entry without returning early on a missing user so
    // both paths still pay for the passwd + shadow reads and password check.
    let user_info = find_user_in_file(PASSWD_PATH, username.as_bytes());

    // Always read /etc/shadow and verify, even if the user wasn't found in
    // passwd. This reduces the observable work difference between existing and
    // non-existing users, though it is not a strict constant-time guarantee.
    let password_ok = verify_shadow_file(SHADOW_PATH, username.as_bytes(), password.as_bytes());
    if !password_ok {
        return None;
    }

    user_info
}

/// D.2: Check if a public key is authorized for the given user.
/// Returns Some(UserInfo) on success.
pub fn check_pubkey(username: &str, pubkey_bytes: &[u8]) -> Option<UserInfo> {
    // Look up user in /etc/passwd to get home directory.
    let user_info = find_user_in_file(PASSWD_PATH, username.as_bytes())?;

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
    if pubkey_file_authorizes(&ak_path[..required_len], pubkey_bytes) {
        Some(user_info)
    } else {
        None
    }
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
            let uid = parse_u32(fields[2])?;
            let gid = parse_u32(fields[3])?;
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

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }

    let mut n: u32 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?;
        n = n.checked_add((b - b'0') as u32)?;
    }
    Some(n)
}

fn find_user_in_file(path: &[u8], username: &[u8]) -> Option<UserInfo> {
    let mut found = None;
    scan_file_lines(path, |line| {
        if line.is_empty() {
            return Ok(ControlFlow::Continue(()));
        }
        if let Some(user) = parse_user_line(line, username) {
            found = Some(user);
            return Ok(ControlFlow::Break(()));
        }
        Ok(ControlFlow::Continue(()))
    })
    .ok()?;
    found
}

fn parse_user_line(line: &[u8], username: &[u8]) -> Option<UserInfo> {
    let fields = split_colon(line)?;
    if fields[0] != username {
        return None;
    }

    let uid = parse_u32(fields[2])?;
    let gid = parse_u32(fields[3])?;
    let uname = core::str::from_utf8(fields[0]).ok()?;
    let home = core::str::from_utf8(fields[5]).ok()?;
    let shell = core::str::from_utf8(fields[6]).ok()?;
    Some(UserInfo {
        username: alloc::string::String::from(uname),
        uid,
        gid,
        home: alloc::string::String::from(home),
        shell: alloc::string::String::from(shell),
    })
}

/// Verify password against /etc/shadow.
fn verify_shadow_file(path: &[u8], username: &[u8], password: &[u8]) -> bool {
    let mut matched = false;
    if scan_file_lines(path, |line| {
        if line.is_empty() {
            return Ok(ControlFlow::Continue(()));
        }
        if verify_shadow_line(line, username, password) {
            matched = true;
            return Ok(ControlFlow::Break(()));
        }
        Ok(ControlFlow::Continue(()))
    })
    .is_err()
    {
        return false;
    }
    matched
}

fn verify_shadow_line(line: &[u8], username: &[u8], password: &[u8]) -> bool {
    if let Some(colon) = line.iter().position(|&b| b == b':') {
        let name = &line[..colon];
        if name == username {
            let rest = &line[colon + 1..];
            let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
            let hash_field = &rest[..hash_end];
            return syscall_lib::sha256::verify_password(password, hash_field);
        }
    }
    false
}

fn pubkey_file_authorizes(path: &[u8], pubkey_bytes: &[u8]) -> bool {
    let mut matched = false;
    if scan_file_lines(path, |line| {
        let line = line.trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            return Ok(ControlFlow::Continue(()));
        }

        let mut key = [0u8; 32];
        if hex_decode(line, &mut key) == 32 && key == pubkey_bytes {
            matched = true;
            return Ok(ControlFlow::Break(()));
        }
        Ok(ControlFlow::Continue(()))
    })
    .is_err()
    {
        return false;
    }
    matched
}

fn scan_file_lines<F>(path: &[u8], mut visit: F) -> Result<(), ()>
where
    F: FnMut(&[u8]) -> Result<ControlFlow<()>, ()>,
{
    let fd = open(path, O_RDONLY, 0);
    if fd < 0 {
        return Err(());
    }
    let fd = fd as i32;

    let result = scan_fd_lines(fd, &mut visit);
    close(fd);
    result
}

fn scan_fd_lines<F>(fd: i32, visit: &mut F) -> Result<(), ()>
where
    F: FnMut(&[u8]) -> Result<ControlFlow<()>, ()>,
{
    let mut chunk = [0u8; FILE_CHUNK_SIZE];
    let mut line = [0u8; MAX_AUTH_LINE_LEN];
    let mut line_len = 0usize;
    loop {
        let n = syscall_lib::read(fd, &mut chunk);
        if n < 0 {
            return Err(());
        }
        if n == 0 {
            break;
        }
        if feed_lines(&chunk[..n as usize], &mut line, &mut line_len, visit)?
            == ControlFlow::Break(())
        {
            return Ok(());
        }
    }
    if line_len > 0 && visit(&line[..line_len])? == ControlFlow::Break(()) {
        return Ok(());
    }
    Ok(())
}

fn feed_lines<F>(
    bytes: &[u8],
    line: &mut [u8; MAX_AUTH_LINE_LEN],
    line_len: &mut usize,
    visit: &mut F,
) -> Result<ControlFlow<()>, ()>
where
    F: FnMut(&[u8]) -> Result<ControlFlow<()>, ()>,
{
    for &byte in bytes {
        if byte == b'\n' {
            if visit(&line[..*line_len])? == ControlFlow::Break(()) {
                return Ok(ControlFlow::Break(()));
            }
            *line_len = 0;
            continue;
        }
        if *line_len >= line.len() {
            return Err(());
        }
        line[*line_len] = byte;
        *line_len += 1;
    }
    Ok(ControlFlow::Continue(()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "m3os-sshd-auth-{name}-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        path
    }

    fn path_cstring(path: &PathBuf) -> CString {
        CString::new(path.as_os_str().as_bytes()).unwrap()
    }

    fn hex_encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            use std::fmt::Write;
            let _ = write!(&mut out, "{byte:02x}");
        }
        out
    }

    #[test]
    fn find_user_in_file_reads_past_old_fixed_limit() {
        let path = temp_path("passwd");
        let mut data = String::new();
        for idx in 0..160 {
            data.push_str(&format!(
                "user{idx}:x:{idx}:{idx}:User {idx}:/home/user{idx}:/bin/sh\n"
            ));
        }
        data.push_str("target:x:4242:4242:Target User:/home/target:/bin/ion\n");
        fs::write(&path, data).unwrap();

        let c_path = path_cstring(&path);
        let user = find_user_in_file(c_path.as_bytes_with_nul(), b"target").unwrap();
        assert_eq!(user.uid, 4242);
        assert_eq!(user.home, "/home/target");
        assert_eq!(user.shell, "/bin/ion");

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn pubkey_file_authorizes_reads_past_old_fixed_limit() {
        let path = temp_path("authorized-keys");
        let mut data = String::new();
        for _ in 0..80 {
            data.push_str("# filler comment to push the real key past 2KiB\n");
        }
        let key = [0xabu8; 32];
        data.push_str(&hex_encode(&key));
        data.push('\n');
        fs::write(&path, data).unwrap();

        let c_path = path_cstring(&path);
        assert!(pubkey_file_authorizes(c_path.as_bytes_with_nul(), &key));

        fs::remove_file(path).unwrap();
    }
}
