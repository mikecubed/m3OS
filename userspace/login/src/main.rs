//! m3OS login — prompts for username/password and spawns user shell (Phase 27).
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::Layout;
use passwd::rewrite_shadow_file;
use syscall_lib::argon2::{DEFAULT_PARAMS, build_shadow_field};
use syscall_lib::heap::BrkAllocator;
use syscall_lib::{
    O_RDONLY, O_TRUNC, O_WRONLY, STDOUT_FILENO, close, execve, exit, fsync, getrandom, nanosleep,
    open, read, setgid, setuid, write, write_str, write_u64,
};

#[global_allocator]
static ALLOCATOR: BrkAllocator = BrkAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    write_str(STDOUT_FILENO, "login: alloc error\n");
    exit(1)
}

const PASSWD_PATH: &[u8] = b"/etc/passwd\0";
const SHADOW_PATH: &[u8] = b"/etc/shadow\0";
const LOGIN_FILE_READ_RETRIES: usize = 5;

// Phase 86f FIX 2: naked _start trampoline.  This binary ignores argv/envp.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "xor rbp, rbp",
        "call {f}",
        f = sym login_entry,
    );
}

fn login_entry() -> ! {
    loop {
        login_once();
    }
}

fn login_once() {
    // Prompt for username.
    write_str(STDOUT_FILENO, "\nm3OS login: ");
    let mut username = [0u8; 64];
    let ulen = read_line(&mut username);
    if ulen == 0 {
        return;
    }
    let username = &username[..ulen];

    // Look up user in /etc/passwd before prompting for password.
    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file_with_retry(PASSWD_PATH, &mut passwd_buf);
    if passwd_len == 0 {
        write_str(STDOUT_FILENO, "login: cannot read /etc/passwd\n");
        return;
    }

    let (uid, gid, home, shell) = match find_user(&passwd_buf[..passwd_len], username) {
        Some(v) => v,
        None => {
            write_str(STDOUT_FILENO, "Login incorrect\n");
            return;
        }
    };

    // Read /etc/shadow to determine if account is locked or has a password.
    let mut shadow_buf = [0u8; 2048];
    let shadow_len = read_file_with_retry(SHADOW_PATH, &mut shadow_buf);
    if shadow_len == 0 {
        write_str(STDOUT_FILENO, "login: cannot read /etc/shadow\n");
        return;
    }

    // Check if account is locked (first-boot setup).
    if is_locked_account(&shadow_buf[..shadow_len], username) {
        write_str(STDOUT_FILENO, "Account requires initial password setup.\n");
        write_str(STDOUT_FILENO, "Set password for ");
        let _ = write(STDOUT_FILENO, username);
        write_str(STDOUT_FILENO, ": ");
        let saved2 = disable_echo();
        let mut new_pw = [0u8; 128];
        let new_pw_len = read_line(&mut new_pw);
        restore_echo(saved2);
        let _ = write(STDOUT_FILENO, b"\n");

        if new_pw_len == 0 {
            write_str(STDOUT_FILENO, "Password cannot be empty\n");
            return;
        }

        write_str(STDOUT_FILENO, "Retype password: ");
        let saved3 = disable_echo();
        let mut confirm = [0u8; 128];
        let confirm_len = read_line(&mut confirm);
        restore_echo(saved3);
        let _ = write(STDOUT_FILENO, b"\n");

        if new_pw_len != confirm_len || new_pw[..new_pw_len] != confirm[..confirm_len] {
            write_str(STDOUT_FILENO, "Passwords don't match\n");
            return;
        }

        if !set_initial_password(username, &new_pw[..new_pw_len]) {
            write_str(STDOUT_FILENO, "login: failed to set password\n");
            return;
        }
        write_str(STDOUT_FILENO, "Password set successfully.\n");
        write_str(
            STDOUT_FILENO,
            "[security] getrandom salt + SHA-256 password hash stored\n",
        );
        // Fall through to authenticated login.
    } else {
        // Normal login — prompt for password.
        write_str(STDOUT_FILENO, "Password: ");
        let saved = disable_echo();
        let mut pw_input = [0u8; 128];
        let plen = read_line(&mut pw_input);
        restore_echo(saved);
        let _ = write(STDOUT_FILENO, b"\n");

        if !verify_shadow(&shadow_buf[..shadow_len], username, &pw_input[..plen]) {
            write_str(STDOUT_FILENO, "Login incorrect\n");
            return;
        }
        // Phase 110 — upgrade a legacy hash to argon2id now, while still root.
        maybe_rehash_to_argon2id(&shadow_buf[..shadow_len], username, &pw_input[..plen]);
    }

    // Authentication succeeded.
    write_str(STDOUT_FILENO, "Welcome to m3OS! uid=");
    write_u64(STDOUT_FILENO, uid as u64);
    write_str(STDOUT_FILENO, "\n");

    // Create home directory if it doesn't exist (still running as root).
    create_home_dir(home, uid, gid);

    // Set GID and UID.
    if setgid(gid) != 0 || setuid(uid) != 0 {
        write_str(STDOUT_FILENO, "login: failed to set credentials\n");
        return;
    }
    write_str(STDOUT_FILENO, "[security] credential transition complete\n");

    // Build environment.
    let mut home_env = [0u8; 128];
    let home_env_len = build_env(b"HOME=", home, &mut home_env);
    let mut user_env = [0u8; 128];
    let user_env_len = build_env(b"USER=", username, &mut user_env);

    let env_path: &[u8] = b"PATH=/usr/local/bin:/bin:/sbin:/usr/bin\0";
    let env_term: &[u8] = b"TERM=m3os-term\0";
    let env_editor: &[u8] = b"EDITOR=/bin/edit\0";

    let envp: [*const u8; 6] = [
        env_path.as_ptr(),
        home_env[..home_env_len].as_ptr(),
        env_term.as_ptr(),
        env_editor.as_ptr(),
        user_env[..user_env_len].as_ptr(),
        core::ptr::null(),
    ];

    // Exec the user's shell.
    let mut shell_path = [0u8; 128];
    let shell_len = copy_with_nul(shell, &mut shell_path);
    let argv: [*const u8; 2] = [shell_path[..shell_len].as_ptr(), core::ptr::null()];
    let ret = execve(&shell_path[..shell_len], &argv, &envp);

    // Exec failed — try fallbacks.
    write_str(STDOUT_FILENO, "login: exec failed (");
    write_u64(STDOUT_FILENO, (-ret) as u64);
    write_str(STDOUT_FILENO, "), trying /bin/sh0\n");

    let sh0: &[u8] = b"/bin/sh0\0";
    let argv2: [*const u8; 2] = [sh0.as_ptr(), core::ptr::null()];
    execve(sh0, &argv2, &envp);
    write_str(STDOUT_FILENO, "login: all shells failed\n");
}

/// Read a line from stdin, stripping the trailing newline.
fn read_line(buf: &mut [u8]) -> usize {
    let mut pos = 0;
    loop {
        let mut byte = [0u8; 1];
        let n = read(0, &mut byte);
        if n <= 0 {
            break;
        }
        if byte[0] == b'\n' || byte[0] == b'\r' {
            break;
        }
        if pos < buf.len() {
            buf[pos] = byte[0];
            pos += 1;
        }
    }
    pos
}

/// Read an entire file into a buffer. Returns bytes read.
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

fn read_file_with_retry(path: &[u8], buf: &mut [u8]) -> usize {
    let mut attempts = 0;
    while attempts < LOGIN_FILE_READ_RETRIES {
        let len = read_file(path, buf);
        if len > 0 {
            return len;
        }
        attempts += 1;
        if attempts < LOGIN_FILE_READ_RETRIES {
            nanosleep(1);
        }
    }
    0
}

/// Parse /etc/passwd to find a user entry.
/// Returns (uid, gid, home, shell) if found.
fn find_user<'a>(passwd: &'a [u8], username: &[u8]) -> Option<(u32, u32, &'a [u8], &'a [u8])> {
    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields: [&[u8]; 7] = match split_colon(line) {
            Some(f) => f,
            None => continue,
        };
        if fields[0] == username {
            let Some(uid) = parse_u32(fields[2]) else {
                continue;
            };
            let Some(gid) = parse_u32(fields[3]) else {
                continue;
            };
            return Some((uid, gid, fields[5], fields[6]));
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

/// Parse a decimal u32 from bytes.
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

/// Check if an account's shadow entry indicates a locked account (hash field is "!" or "*").
fn is_locked_account(shadow: &[u8], username: &[u8]) -> bool {
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
                return hash_field == b"!" || hash_field == b"*";
            }
        }
    }
    false
}

/// Hash `password` with a fresh random salt into a `$argon2id$…` shadow field.
/// Returns the field length, or `None` on getrandom/hash failure.
fn build_argon2_field(password: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut salt = [0u8; 16];
    if getrandom(&mut salt) != 16 {
        return None;
    }
    build_shadow_field(password, &salt, &DEFAULT_PARAMS, out)
}

/// Rewrite `username`'s hash field in /etc/shadow to `hash_field`, preserving
/// the metadata suffix. Non-atomic (matches the pre-existing login write path);
/// callers treat failure as best-effort.
fn write_shadow_field(username: &[u8], hash_field: &[u8]) -> bool {
    let mut shadow_buf = [0u8; 2048];
    let shadow_len = read_file(SHADOW_PATH, &mut shadow_buf);
    if shadow_len == 0 {
        return false;
    }
    let mut new_shadow = [0u8; 2048];
    let out_pos = match rewrite_shadow_file(
        &shadow_buf[..shadow_len],
        username,
        hash_field,
        &mut new_shadow,
    ) {
        Ok(len) => len,
        Err(_) => return false,
    };
    let fd = open(SHADOW_PATH, O_WRONLY | O_TRUNC, 0);
    if fd < 0 {
        return false;
    }
    let written = write(fd as i32, &new_shadow[..out_pos]);
    if written != out_pos as isize {
        close(fd as i32);
        return false;
    }
    if fsync(fd as i32) < 0 {
        close(fd as i32);
        return false;
    }
    close(fd as i32);
    true
}

/// Set a new password for a locked account by rewriting /etc/shadow (argon2id).
fn set_initial_password(username: &[u8], password: &[u8]) -> bool {
    let mut hash_field = [0u8; 200];
    let n = match build_argon2_field(password, &mut hash_field) {
        Some(n) => n,
        None => return false,
    };
    write_shadow_field(username, &hash_field[..n])
}

/// Return `username`'s `/etc/shadow` hash field (the second colon-field).
fn user_hash_field<'a>(shadow: &'a [u8], username: &[u8]) -> Option<&'a [u8]> {
    for line in shadow.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.iter().position(|&b| b == b':')
            && &line[..colon] == username
        {
            let rest = &line[colon + 1..];
            let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
            return Some(&rest[..hash_end]);
        }
    }
    None
}

/// Phase 110 — after a successful login against a legacy `$sha256i$`/`$sha256$`
/// entry, transparently re-hash the password to argon2id in place. Best-effort:
/// a failure just leaves the legacy entry (still valid) for next time. MUST run
/// while still root (before the setuid), since it writes /etc/shadow.
fn maybe_rehash_to_argon2id(shadow: &[u8], username: &[u8], password: &[u8]) {
    let Some(field) = user_hash_field(shadow, username) else {
        return;
    };
    if field.starts_with(syscall_lib::argon2::SHADOW_PREFIX) {
        return; // already argon2id
    }
    let mut hash_field = [0u8; 200];
    if let Some(n) = build_argon2_field(password, &mut hash_field)
        && write_shadow_field(username, &hash_field[..n])
    {
        write_str(
            STDOUT_FILENO,
            "[security] rehashed login password to argon2id\n",
        );
    }
}

/// Verify password against /etc/shadow.
fn verify_shadow(shadow: &[u8], username: &[u8], password: &[u8]) -> bool {
    match user_hash_field(shadow, username) {
        Some(field) => syscall_lib::sha256::verify_password(password, field),
        None => false,
    }
}

/// Build an environment string like "KEY=value\0".
fn build_env(prefix: &[u8], value: &[u8], buf: &mut [u8]) -> usize {
    let mut pos = 0;
    for &b in prefix {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    for &b in value {
        if pos < buf.len() {
            buf[pos] = b;
            pos += 1;
        }
    }
    if pos < buf.len() {
        buf[pos] = 0; // null terminator
        pos += 1;
    }
    pos
}

/// Create the user's home directory if it doesn't exist.
/// Called before setuid so we still have root privileges.
fn create_home_dir(home: &[u8], uid: u32, gid: u32) {
    if home.is_empty() || home == b"/" {
        return;
    }

    // Build null-terminated path for mkdir.
    let mut path = [0u8; 128];
    let len = home.len().min(path.len() - 1);
    path[..len].copy_from_slice(&home[..len]);
    path[len] = 0;

    // Try mkdir — ignore EEXIST (-17).
    let ret = unsafe { syscall_lib::syscall2(syscall_lib::SYS_MKDIR, path.as_ptr() as u64, 0o755) };
    if ret == 0 {
        // Set ownership on newly created directory.
        syscall_lib::chown(&path[..len + 1], uid, gid);
    }
    // ret == -17 (EEXIST) is fine — directory already exists.
}

/// Copy bytes with a null terminator.
fn copy_with_nul(src: &[u8], dst: &mut [u8]) -> usize {
    let n = src.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
    n + 1
}

/// Disable echo on stdin for password entry.
///
/// Saves the current termios and clears ECHO. On restore, the saved
/// termios is validated — if all four flag fields are zero (clearly
/// corrupt `tcgetattr` output from a known kernel copy_to_user bug),
/// we fall back to sensible cooked-mode defaults. A valid raw-mode
/// termios (c_lflag == 0 but c_cflag != 0) is preserved as-is.
fn disable_echo() -> Option<syscall_lib::Termios> {
    if let Ok(t) = syscall_lib::tcgetattr(0) {
        // Detect fully-zeroed struct from a copy_to_user bug: all four
        // flag fields zero is not a legitimate termios configuration
        // (c_cflag must carry baud + char-size bits for any real TTY).
        let saved = if t.c_iflag == 0 && t.c_oflag == 0 && t.c_cflag == 0 && t.c_lflag == 0 {
            syscall_lib::Termios {
                c_iflag: syscall_lib::ICRNL,
                c_oflag: syscall_lib::OPOST | syscall_lib::ONLCR,
                c_cflag: syscall_lib::CS8
                    | syscall_lib::CREAD
                    | syscall_lib::HUPCL
                    | syscall_lib::B38400,
                c_lflag: syscall_lib::ICANON
                    | syscall_lib::ECHO
                    | syscall_lib::ECHOE
                    | syscall_lib::ISIG
                    | syscall_lib::IEXTEN,
                c_line: 0,
                c_cc: t.c_cc,
            }
        } else {
            t
        };
        let mut raw = saved;
        raw.c_lflag &= !(syscall_lib::ECHO | syscall_lib::ECHOE);
        let _ = syscall_lib::tcsetattr(0, &raw);
        Some(saved)
    } else {
        None
    }
}

/// Restore terminal settings.
fn restore_echo(saved: Option<syscall_lib::Termios>) {
    if let Some(t) = saved {
        let _ = syscall_lib::tcsetattr_flush(0, &t);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "login: PANIC\n");
    exit(101)
}
