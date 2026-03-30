//! m3OS login — prompts for username/password and spawns user shell (Phase 27).
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, execve, exit, open, read, setgid, setuid, write, write_str,
    write_u64,
};

const PASSWD_PATH: &[u8] = b"/etc/passwd\0";
const SHADOW_PATH: &[u8] = b"/etc/shadow\0";

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
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

    // Prompt for password with echo disabled.
    let _ = write(STDOUT_FILENO, b"\n");
    write_str(STDOUT_FILENO, "Password: ");

    // Disable echo for password input.
    let saved = disable_echo();
    let mut pw_input = [0u8; 128];
    let plen = read_line(&mut pw_input);
    restore_echo(saved);
    let _ = write(STDOUT_FILENO, b"\n");
    let pw_input = &pw_input[..plen];

    // Look up user in /etc/passwd.
    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(PASSWD_PATH, &mut passwd_buf);
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

    // Verify password against /etc/shadow.
    let mut shadow_buf = [0u8; 2048];
    let shadow_len = read_file(SHADOW_PATH, &mut shadow_buf);
    if shadow_len == 0 {
        write_str(STDOUT_FILENO, "login: cannot read /etc/shadow\n");
        return;
    }

    if !verify_shadow(&shadow_buf[..shadow_len], username, pw_input) {
        write_str(STDOUT_FILENO, "Login incorrect\n");
        return;
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

    // Build environment.
    let mut home_env = [0u8; 128];
    let home_env_len = build_env(b"HOME=", home, &mut home_env);
    let mut user_env = [0u8; 128];
    let user_env_len = build_env(b"USER=", username, &mut user_env);

    let env_path: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
    let env_term: &[u8] = b"TERM=m3os\0";
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
            let uid = parse_u32(fields[2]);
            let gid = parse_u32(fields[3]);
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
        // Format: username:hash:...
        if let Some(colon) = line.iter().position(|&b| b == b':') {
            let name = &line[..colon];
            if name == username {
                let rest = &line[colon + 1..];
                // Find the next colon to isolate the hash field.
                let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
                let hash_field = &rest[..hash_end];
                return syscall_lib::sha256::verify_password(password, hash_field);
            }
        }
    }
    false
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

/// Disable echo on stdin (set raw mode).
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

/// Restore terminal settings.
fn restore_echo(saved: Option<syscall_lib::Termios>) {
    if let Some(t) = saved {
        let _ = syscall_lib::tcsetattr(0, &t);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    write_str(STDOUT_FILENO, "login: PANIC\n");
    exit(101)
}
