//! Shared helpers for coreutils-rs binaries.
//!
//! Each binary includes this module with:
//!   `#[path = "common.rs"] mod common;`
//! and uses the helpers without any Cargo.toml changes.

use syscall_lib::{O_RDONLY, STDERR_FILENO, STDOUT_FILENO, close, execve, open, read, write};

/// Write all bytes in `data` to `fd`, retrying on short writes.
#[allow(dead_code)]
pub fn write_all(fd: i32, data: &[u8]) {
    let mut off = 0;
    while off < data.len() {
        let w = write(fd, &data[off..]);
        if w <= 0 {
            break;
        }
        off += w as usize;
    }
}

/// Write a string slice to stdout.
#[allow(dead_code)]
pub fn print(s: &str) {
    write_all(STDOUT_FILENO, s.as_bytes());
}

/// Write a string slice followed by a newline to stdout.
#[allow(dead_code)]
pub fn println(s: &str) {
    write_all(STDOUT_FILENO, s.as_bytes());
    write_all(STDOUT_FILENO, b"\n");
}

/// Write a string slice to stderr.
#[allow(dead_code)]
pub fn eprint(s: &str) {
    write_all(STDERR_FILENO, s.as_bytes());
}

/// Write a string slice followed by a newline to stderr.
#[allow(dead_code)]
pub fn eprintln(s: &str) {
    write_all(STDERR_FILENO, s.as_bytes());
    write_all(STDERR_FILENO, b"\n");
}

/// Parse a positive decimal integer from a byte slice. Returns `None` if the
/// slice is empty, non-numeric, or produces zero.
#[allow(dead_code)]
pub fn parse_u64_bytes(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v = 0u64;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add((b - b'0') as u64);
    }
    Some(v)
}

/// Parse a positive decimal integer from a byte slice, requiring it to be
/// strictly positive (>0). Returns `None` on failure.
#[allow(dead_code)]
pub fn parse_positive_u64(s: &[u8]) -> Option<u64> {
    let v = parse_u64_bytes(s)?;
    if v == 0 { None } else { Some(v) }
}

/// Parse a `u32` decimal integer from a byte slice.
#[allow(dead_code)]
pub fn parse_u32_bytes(s: &[u8]) -> Option<u32> {
    let v = parse_u64_bytes(s)?;
    if v > u32::MAX as u64 {
        None
    } else {
        Some(v as u32)
    }
}

/// Find the index of the first `\n` in `buf[start..]`. Returns `None` if not
/// found.
#[allow(dead_code)]
pub fn find_newline(buf: &[u8], start: usize) -> Option<usize> {
    buf[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| start + p)
}

/// Fixed-string substring search — equivalent to `strstr`.
#[allow(dead_code)]
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    for i in 0..=(haystack.len() - needle.len()) {
        if haystack[i..i + needle.len()] == *needle {
            return true;
        }
    }
    false
}

/// Copy path components from `parts` into `buf`, separated by `b'/'`, and
/// appends a NUL terminator. Returns the total length including NUL, or
/// `None` if the buffer is too small.
#[allow(dead_code)]
pub fn build_nul_path(buf: &mut [u8], parts: &[&[u8]]) -> Option<usize> {
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            if pos >= buf.len() {
                return None;
            }
            buf[pos] = b'/';
            pos += 1;
        }
        let end = pos + part.len();
        if end >= buf.len() {
            return None;
        }
        buf[pos..end].copy_from_slice(part);
        pos = end;
    }
    if pos >= buf.len() {
        return None;
    }
    buf[pos] = 0;
    Some(pos + 1)
}

/// Try to exec `cmd` (NUL-terminated) by PATH search. If `cmd` contains '/', exec
/// it directly. On failure (all attempts return), this returns without executing
/// anything. The command inherits `argv`/`envp` verbatim.
///
/// `path` selects the search PATH:
/// * `None` — inherit: read `PATH=` from `/proc/self/environ` (the caller's own
///   environment). This is what `xargs` passes.
/// * `Some(p)` with `p` non-empty — an explicit search PATH (e.g. `env`'s
///   `PATH=…` assignment, which must override the inherited one per POSIX).
/// * `Some(b"")` — no PATH at all (e.g. `env -i` cleared it) → use only the
///   `/bin:/usr/bin` default fallback; do NOT consult `/proc/self/environ`.
///
/// Shared by `env` (passes an explicit/empty PATH for assignment / `-i`
/// semantics) and `xargs` (passes `None`).
#[allow(dead_code)]
pub fn exec_with_path_search(
    cmd: &[u8],
    argv: &[*const u8],
    envp: &[*const u8],
    path: Option<&[u8]>,
) {
    if cmd.contains(&b'/') {
        execve(cmd, argv, envp);
        return;
    }
    let cmd_name = if cmd.last() == Some(&0) {
        &cmd[..cmd.len().saturating_sub(1)]
    } else {
        cmd
    };

    // Resolve the search PATH bytes. `env_buf` backs the inherited (`None`) case.
    let mut env_buf = [0u8; 4096];
    let path_val: &[u8] = match path {
        Some(p) => p,
        None => {
            // Read PATH from /proc/self/environ (NUL-separated KEY=VALUE entries).
            let mut path_off = 0usize;
            let mut path_len = 0usize;
            let fd = open(b"/proc/self/environ\0", O_RDONLY, 0);
            if fd >= 0 {
                let n = read(fd as i32, &mut env_buf);
                close(fd as i32);
                if n > 0 {
                    let data = &env_buf[..n as usize];
                    let mut i = 0;
                    while i < data.len() {
                        let end = data[i..]
                            .iter()
                            .position(|&b| b == 0)
                            .map(|p| i + p)
                            .unwrap_or(data.len());
                        if data[i..end].starts_with(b"PATH=") {
                            path_off = i + 5;
                            path_len = end.saturating_sub(i + 5);
                            break;
                        }
                        i = end + 1;
                    }
                }
            }
            &env_buf[path_off..path_off + path_len]
        }
    };

    if path_val.is_empty() {
        let fallback: [&[u8]; 2] = [b"/bin", b"/usr/bin"];
        for &dir in &fallback {
            let mut path_buf = [0u8; 512];
            if let Some(plen) = build_nul_path(&mut path_buf, &[dir, cmd_name]) {
                execve(&path_buf[..plen], argv, envp);
            }
        }
        return;
    }

    let mut seg_start = 0;
    loop {
        let seg_end = path_val[seg_start..]
            .iter()
            .position(|&b| b == b':')
            .map(|p| seg_start + p)
            .unwrap_or(path_val.len());
        let dir = &path_val[seg_start..seg_end];
        if !dir.is_empty() {
            let mut path_buf = [0u8; 512];
            if let Some(plen) = build_nul_path(&mut path_buf, &[dir, cmd_name]) {
                execve(&path_buf[..plen], argv, envp);
            }
        }
        if seg_end >= path_val.len() {
            break;
        }
        seg_start = seg_end + 1;
    }
}

/// Write a decimal u64 to `fd`.
#[allow(dead_code)]
pub fn write_u64(fd: i32, mut v: u64) {
    let mut buf = [0u8; 20];
    let mut end = buf.len();
    if v == 0 {
        end -= 1;
        buf[end] = b'0';
    } else {
        while v > 0 {
            end -= 1;
            buf[end] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    }
    write_all(fd, &buf[end..]);
}
