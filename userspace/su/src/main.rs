//! m3OS su — switch user (Phase 27).
#![no_std]
#![no_main]

use syscall_lib::{
    O_RDONLY, STDOUT_FILENO, close, execve, exit, geteuid, open, read, setgid, setuid, write,
    write_str, write_u64,
};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read target username (default: root if empty).
    write_str(STDOUT_FILENO, "su: target user (default root): ");
    let mut target_buf = [0u8; 64];
    let tlen = read_line(&mut target_buf);
    let target: &[u8] = if tlen == 0 {
        b"root"
    } else {
        &target_buf[..tlen]
    };

    // Look up target user in /etc/passwd.
    let mut passwd_buf = [0u8; 2048];
    let passwd_len = read_file(b"/data/etc/passwd\0", &mut passwd_buf);
    if passwd_len == 0 {
        write_str(STDOUT_FILENO, "su: cannot read /data/etc/passwd\n");
        exit(1);
    }

    let (uid, gid, home, shell) = match find_user(&passwd_buf[..passwd_len], target) {
        Some(v) => v,
        None => {
            write_str(STDOUT_FILENO, "su: unknown user\n");
            exit(1);
        }
    };

    // su requires root privileges (setuid-bit support deferred).
    if geteuid() != 0 {
        write_str(STDOUT_FILENO, "su: must be run as root\n");
        exit(1);
    }

    // Prompt for target user's password (skip if switching to self).
    let caller_uid = syscall_lib::getuid();
    if caller_uid != uid {
        write_str(STDOUT_FILENO, "Password: ");
        let saved = disable_echo();
        let mut pw_input = [0u8; 128];
        let plen = read_line(&mut pw_input);
        restore_echo(saved);
        let _ = write(STDOUT_FILENO, b"\n");

        let mut shadow_buf = [0u8; 2048];
        let shadow_len = read_file(b"/data/etc/shadow\0", &mut shadow_buf);
        if shadow_len == 0 || !verify_shadow(&shadow_buf[..shadow_len], target, &pw_input[..plen]) {
            write_str(STDOUT_FILENO, "su: Authentication failure\n");
            exit(1);
        }
    }
    // Create home directory before dropping privileges.
    create_home_dir(home, uid, gid);

    if setgid(gid) != 0 || setuid(uid) != 0 {
        write_str(STDOUT_FILENO, "su: failed to set credentials\n");
        exit(1);
    }

    // Exec the target user's shell.
    let env_path: &[u8] = b"PATH=/bin:/sbin:/usr/bin\0";
    let env_term: &[u8] = b"TERM=m3os\0";
    let envp: [*const u8; 3] = [env_path.as_ptr(), env_term.as_ptr(), core::ptr::null()];

    let mut shell_path = [0u8; 128];
    let slen = copy_nul(shell, &mut shell_path);
    let argv: [*const u8; 2] = [shell_path[..slen].as_ptr(), core::ptr::null()];
    let ret = execve(&shell_path[..slen], &argv, &envp);

    write_str(STDOUT_FILENO, "su: exec failed (");
    write_u64(STDOUT_FILENO, (-ret) as u64);
    write_str(STDOUT_FILENO, ")\n");
    exit(1);
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

fn find_user<'a>(passwd: &'a [u8], username: &[u8]) -> Option<(u32, u32, &'a [u8], &'a [u8])> {
    for line in passwd.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let mut fields = [&[] as &[u8]; 7];
        let mut start = 0;
        let mut field = 0;
        for (i, &b) in line.iter().enumerate() {
            if b == b':' {
                if field < 7 {
                    fields[field] = &line[start..i];
                    field += 1;
                    start = i + 1;
                }
            }
        }
        if field == 6 {
            fields[6] = &line[start..];
            if fields[0] == username {
                let uid = parse_u32(fields[2]);
                let gid = parse_u32(fields[3]);
                return Some((uid, gid, fields[5], fields[6]));
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

fn verify_shadow(shadow: &[u8], username: &[u8], password: &[u8]) -> bool {
    for line in shadow.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(colon) = line.iter().position(|&b| b == b':') {
            if &line[..colon] == username {
                let rest = &line[colon + 1..];
                let hash_end = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
                return syscall_lib::sha256::verify_password(password, &rest[..hash_end]);
            }
        }
    }
    false
}

fn create_home_dir(home: &[u8], uid: u32, gid: u32) {
    if home.is_empty() || home == b"/" {
        return;
    }
    let mut path = [0u8; 128];
    let len = home.len().min(path.len() - 1);
    path[..len].copy_from_slice(&home[..len]);
    path[len] = 0;
    let ret = unsafe { syscall_lib::syscall2(syscall_lib::SYS_MKDIR, path.as_ptr() as u64, 0o755) };
    if ret == 0 {
        syscall_lib::chown(&path[..len + 1], uid, gid);
    }
}

fn copy_nul(src: &[u8], dst: &mut [u8]) -> usize {
    let n = src.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
    n + 1
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
    write_str(STDOUT_FILENO, "su: PANIC\n");
    exit(101)
}
