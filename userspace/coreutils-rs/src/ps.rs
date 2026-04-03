//! ps — report process status.
#![no_std]
#![no_main]

use syscall_lib::{
    O_DIRECTORY, O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, getdents64, getuid, open, read,
    write, write_str,
};

syscall_lib::entry_point!(main);

fn is_all_digits(s: &[u8]) -> bool {
    if s.is_empty() {
        return false;
    }
    s.iter().all(|&b| b.is_ascii_digit())
}

/// Find a field like "State:\t" or "Uid:\t" in a status buffer, return the value bytes.
fn find_field<'a>(buf: &'a [u8], label: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0usize;
    while i + label.len() <= buf.len() {
        if &buf[i..i + label.len()] == label {
            let mut j = i + label.len();
            // Skip whitespace.
            while j < buf.len() && (buf[j] == b' ' || buf[j] == b'\t') {
                j += 1;
            }
            let start = j;
            while j < buf.len() && buf[j] != b'\n' {
                j += 1;
            }
            return Some(&buf[start..j]);
        }
        // Advance to next line.
        while i < buf.len() && buf[i] != b'\n' {
            i += 1;
        }
        i += 1;
    }
    None
}

fn parse_first_u32(s: &[u8]) -> u32 {
    let mut v = 0u32;
    let mut seen = false;
    for &b in s {
        if b.is_ascii_digit() {
            seen = true;
            v = v.wrapping_mul(10).wrapping_add((b - b'0') as u32);
        } else if seen {
            break;
        }
    }
    v
}

fn build_proc_path(buf: &mut [u8; 64], pid: &[u8], suffix: &[u8]) -> usize {
    let prefix = b"/proc/";
    let mut pos = 0usize;
    buf[..prefix.len()].copy_from_slice(prefix);
    pos += prefix.len();
    buf[pos..pos + pid.len()].copy_from_slice(pid);
    pos += pid.len();
    buf[pos..pos + suffix.len()].copy_from_slice(suffix);
    pos += suffix.len();
    buf[pos] = 0;
    pos
}

fn write_rpadded(fd: i32, s: &[u8], width: usize) {
    let _ = write(fd, s);
    for _ in s.len()..width {
        write_str(fd, " ");
    }
}

fn print_process(pid: &[u8], caller_uid: u32, show_all: bool) {
    let mut path_buf = [0u8; 64];
    let mut status_buf = [0u8; 1024];
    let mut cmd_buf = [0u8; 128];

    // Open /proc/PID/status.
    let path_len = build_proc_path(&mut path_buf, pid, b"/status");
    let fd = open(&path_buf[..=path_len], O_RDONLY, 0);
    if fd < 0 {
        return;
    }
    let mut nread = 0usize;
    loop {
        if nread >= status_buf.len() {
            break;
        }
        let n = read(fd as i32, &mut status_buf[nread..]);
        if n <= 0 {
            break;
        }
        nread += n as usize;
    }
    close(fd as i32);
    if nread == 0 {
        return;
    }

    // Parse UID.
    let uid = match find_field(&status_buf[..nread], b"Uid:") {
        Some(v) => parse_first_u32(v),
        None => return,
    };

    if !show_all && uid != caller_uid {
        return;
    }

    // Parse State.
    let state: &[u8] = match find_field(&status_buf[..nread], b"State:") {
        Some(v) => v,
        None => b"?",
    };

    // Read /proc/PID/cmdline.
    let cmd_path_len = build_proc_path(&mut path_buf, pid, b"/cmdline");
    let cmd_fd = open(&path_buf[..=cmd_path_len], O_RDONLY, 0);
    let cmd_len = if cmd_fd >= 0 {
        let mut total = 0usize;
        loop {
            if total >= cmd_buf.len() {
                break;
            }
            let n = read(cmd_fd as i32, &mut cmd_buf[total..]);
            if n <= 0 {
                break;
            }
            total += n as usize;
        }
        close(cmd_fd as i32);
        // Replace NUL bytes with spaces.
        for b in &mut cmd_buf[..total] {
            if *b == 0 {
                *b = b' ';
            }
        }
        total
    } else {
        cmd_buf[0] = b'?';
        1
    };

    // PID column (left-padded to 5, like %-5s but numeric).
    write_rpadded(STDOUT_FILENO, pid, 5);
    write_str(STDOUT_FILENO, " ");
    write_rpadded(STDOUT_FILENO, state, 12);
    write_str(STDOUT_FILENO, " ");
    let _ = write(STDOUT_FILENO, &cmd_buf[..cmd_len]);
    write_str(STDOUT_FILENO, "\n");
}

fn main(args: &[&str]) -> i32 {
    let mut show_all = false;

    if args.len() == 2 && (args[1] == "-e" || args[1] == "-A") {
        show_all = true;
    } else if args.len() != 1 {
        write_str(STDERR_FILENO, "usage: ps [-e|-A]\n");
        return 1;
    }

    let caller_uid = getuid();

    let proc_path = b"/proc\0";
    let fd = open(proc_path, O_RDONLY | O_DIRECTORY, 0);
    if fd < 0 {
        write_str(STDERR_FILENO, "ps: cannot open /proc\n");
        return 1;
    }

    write_str(STDOUT_FILENO, "PID   STATE        CMD\n");

    let mut dents_buf = [0u8; 2048];
    loop {
        let n = getdents64(fd as i32, &mut dents_buf);
        if n == 0 {
            break;
        }
        if n < 0 {
            write_str(STDERR_FILENO, "ps: getdents64 error\n");
            break;
        }
        let mut off = 0usize;
        while off < n as usize {
            if off + 19 > n as usize {
                break;
            }
            let reclen = u16::from_ne_bytes([dents_buf[off + 16], dents_buf[off + 17]]) as usize;
            if reclen < 19 || off + reclen > n as usize {
                break;
            }
            let name_start = off + 19;
            let name_bytes = &dents_buf[name_start..off + reclen];
            let name_len = name_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_bytes.len());
            let name = &name_bytes[..name_len];

            if is_all_digits(name) {
                print_process(name, caller_uid, show_all);
            }
            off += reclen;
        }
    }

    close(fd as i32);
    0
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::exit(101)
}
