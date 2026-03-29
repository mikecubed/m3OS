//! m3OS adduser — create a new user account (Phase 27, root only).
#![no_std]
#![no_main]

use syscall_lib::{
    chmod, chown, close, exit, geteuid, open, read, write, write_str, write_u64, O_RDONLY,
    STDOUT_FILENO,
};

const PASSWD_PATH: &[u8] = b"/data/etc/passwd\0";
const SHADOW_PATH: &[u8] = b"/data/etc/shadow\0";
const GROUP_PATH: &[u8] = b"/data/etc/group\0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if geteuid() != 0 {
        write_str(STDOUT_FILENO, "adduser: must be root\n");
        exit(1);
    }

    // Get username from prompt (since argv parsing is limited).
    write_str(STDOUT_FILENO, "Username: ");
    let mut username = [0u8; 64];
    let ulen = read_line(&mut username);
    if ulen == 0 {
        write_str(STDOUT_FILENO, "adduser: empty username\n");
        exit(1);
    }
    let username = &username[..ulen];

    // Get password.
    write_str(STDOUT_FILENO, "Password: ");
    let saved = disable_echo();
    let mut password = [0u8; 128];
    let plen = read_line(&mut password);
    restore_echo(saved);
    let _ = write(STDOUT_FILENO, b"\n");

    // Find next available UID by scanning /etc/passwd.
    let mut passwd_buf = [0u8; 4096];
    let passwd_len = read_file(PASSWD_PATH, &mut passwd_buf);
    let mut max_uid: u32 = 999;
    for line in passwd_buf[..passwd_len].split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let mut field = 0;
        let mut start = 0;
        for (i, &b) in line.iter().enumerate() {
            if b == b':' {
                if field == 2 {
                    let uid = parse_u32(&line[start..i]);
                    if uid > max_uid {
                        max_uid = uid;
                    }
                }
                field += 1;
                start = i + 1;
            }
        }
    }
    let new_uid = max_uid + 1;
    let new_gid = new_uid; // GID = UID

    // Hash the password.
    let salt = username;
    let hash = syscall_lib::sha256::hash_password(&password[..plen], salt);
    let mut salt_hex = [0u8; 64];
    let salt_hex_len = syscall_lib::sha256::to_hex(salt, &mut salt_hex);
    let mut hash_hex = [0u8; 64];
    let hash_hex_len = syscall_lib::sha256::to_hex(&hash, &mut hash_hex);

    // Append to /etc/passwd.
    {
        let fd = open(
            PASSWD_PATH,
            syscall_lib::O_WRONLY | syscall_lib::O_APPEND,
            0,
        );
        if fd < 0 {
            write_str(STDOUT_FILENO, "adduser: cannot open /data/etc/passwd\n");
            exit(1);
        }
        let _ = write(fd as i32, username);
        let _ = write(fd as i32, b":x:");
        write_u32_to_fd(fd as i32, new_uid);
        let _ = write(fd as i32, b":");
        write_u32_to_fd(fd as i32, new_gid);
        let _ = write(fd as i32, b":");
        let _ = write(fd as i32, username);
        let _ = write(fd as i32, b":/home/");
        let _ = write(fd as i32, username);
        let _ = write(fd as i32, b":/bin/ion\n");
        close(fd as i32);
    }

    // Append to /etc/shadow.
    {
        let fd = open(
            SHADOW_PATH,
            syscall_lib::O_WRONLY | syscall_lib::O_APPEND,
            0,
        );
        if fd < 0 {
            write_str(STDOUT_FILENO, "adduser: cannot open /data/etc/shadow\n");
            exit(1);
        }
        let _ = write(fd as i32, username);
        let _ = write(fd as i32, b":$sha256$");
        let _ = write(fd as i32, &salt_hex[..salt_hex_len]);
        let _ = write(fd as i32, b"$");
        let _ = write(fd as i32, &hash_hex[..hash_hex_len]);
        let _ = write(fd as i32, b"::::::\n");
        close(fd as i32);
    }

    // Append to /etc/group.
    {
        let fd = open(GROUP_PATH, syscall_lib::O_WRONLY | syscall_lib::O_APPEND, 0);
        if fd < 0 {
            write_str(STDOUT_FILENO, "adduser: cannot open /data/etc/group\n");
            exit(1);
        }
        let _ = write(fd as i32, username);
        let _ = write(fd as i32, b":x:");
        write_u32_to_fd(fd as i32, new_gid);
        let _ = write(fd as i32, b":");
        let _ = write(fd as i32, username);
        let _ = write(fd as i32, b"\n");
        close(fd as i32);
    }

    // Create home directory.
    let mut home_path = [0u8; 128];
    let mut hp = 0;
    for &b in b"/tmp/home/" {
        home_path[hp] = b;
        hp += 1;
    }
    for &b in username {
        home_path[hp] = b;
        hp += 1;
    }
    home_path[hp] = 0;
    hp += 1;

    // mkdir the home directory under /tmp.
    let _ = syscall_lib::open(&home_path[..hp], syscall_lib::O_RDONLY, 0); // test if exists
    let mkdir_ret =
        unsafe { syscall_lib::syscall2(syscall_lib::SYS_MKDIR, home_path.as_ptr() as u64, 0o755) };
    if mkdir_ret == 0 || mkdir_ret as i64 == -17 {
        // Set ownership.
        chown(&home_path[..hp], new_uid, new_gid);
    }

    write_str(STDOUT_FILENO, "adduser: user '");
    let _ = write(STDOUT_FILENO, username);
    write_str(STDOUT_FILENO, "' created (uid=");
    write_u64(STDOUT_FILENO, new_uid as u64);
    write_str(STDOUT_FILENO, ")\n");
    exit(0);
}

fn write_u32_to_fd(fd: i32, n: u32) {
    let mut buf = [0u8; 12];
    let mut pos = buf.len();
    let mut val = n;
    if val == 0 {
        let _ = write(fd, b"0");
        return;
    }
    while val > 0 {
        pos -= 1;
        buf[pos] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    let _ = write(fd, &buf[pos..]);
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
    write_str(STDOUT_FILENO, "adduser: PANIC\n");
    exit(101)
}
