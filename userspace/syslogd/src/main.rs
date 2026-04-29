//! System logging daemon for m3OS (Phase 46).
//!
//! Binds a Unix domain socket at `/dev/log`, receives syslog messages from
//! userspace clients, drains kernel messages from `/proc/kmsg` (falling back
//! to `/dev/kmsg`), and writes formatted log lines to `/var/log/messages`
//! and `/var/log/kern.log`.
#![no_std]
#![no_main]

use syscall_lib::{
    AF_UNIX, CLOCK_REALTIME, NEG_EEXIST, O_APPEND, O_CREAT, O_WRONLY, POLLIN, PollFd, SOCK_DGRAM,
    STDOUT_FILENO, SockaddrUn,
};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

syscall_lib::entry_point!(program_main);

fn program_main(_args: &[&str]) -> i32 {
    syscall_lib::write_str(STDOUT_FILENO, "syslogd: starting\n");

    // Ensure /var/log exists.
    ensure_log_dirs();

    // Remove stale socket if present, then bind /dev/log.
    let sock_fd = match setup_socket() {
        Some(fd) => fd,
        None => {
            syscall_lib::write_str(STDOUT_FILENO, "syslogd: failed to bind /dev/log\n");
            return 1;
        }
    };

    // Open log files.
    let msg_fd = open_log_file(b"/var/log/messages\0");
    let kern_fd = open_log_file(b"/var/log/kern.log\0");

    if msg_fd < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "syslogd: cannot open /var/log/messages\n");
        return 1;
    }
    if kern_fd < 0 {
        syscall_lib::write_str(STDOUT_FILENO, "syslogd: cannot open /var/log/kern.log\n");
        return 1;
    }

    // Try to open /dev/kmsg for kernel messages (non-blocking).
    let kmsg_fd = open_kmsg();

    syscall_lib::write_str(STDOUT_FILENO, "syslogd: ready\n");

    // Main loop.
    main_loop(sock_fd, msg_fd, kern_fd, kmsg_fd);
}

// ---------------------------------------------------------------------------
// Directory and file setup
// ---------------------------------------------------------------------------

fn ensure_log_dirs() {
    let ret = syscall_lib::mkdir(b"/var\0", 0o755);
    if ret < 0 && ret != NEG_EEXIST {
        syscall_lib::write_str(STDOUT_FILENO, "syslogd: warning: cannot create /var\n");
    }
    let ret = syscall_lib::mkdir(b"/var/log\0", 0o755);
    if ret < 0 && ret != NEG_EEXIST {
        syscall_lib::write_str(STDOUT_FILENO, "syslogd: warning: cannot create /var/log\n");
    }
}

fn open_log_file(path: &[u8]) -> i32 {
    let fd = syscall_lib::open(path, O_WRONLY | O_CREAT | O_APPEND, 0o644);
    if fd < 0 {
        return -1;
    }
    fd as i32
}

fn open_kmsg() -> i32 {
    // Try /proc/kmsg first (kernel log snapshot), fall back to /dev/kmsg.
    let fd = syscall_lib::open(b"/proc/kmsg\0", 0, 0);
    let fd = if fd < 0 {
        syscall_lib::open(b"/dev/kmsg\0", 0, 0)
    } else {
        fd
    };
    if fd < 0 {
        // Not fatal -- kernel may not expose either path.
        return -1;
    }
    // Set non-blocking so reads don't stall the main loop.
    let ret = syscall_lib::set_nonblocking(fd as i32);
    if ret < 0 {
        syscall_lib::close(fd as i32);
        return -1;
    }
    fd as i32
}

fn setup_socket() -> Option<i32> {
    // Remove stale socket.
    syscall_lib::unlink(b"/dev/log\0");

    let fd = syscall_lib::socket(AF_UNIX as i32, SOCK_DGRAM as i32, 0);
    if fd < 0 {
        return None;
    }

    let addr = SockaddrUn::new("/dev/log");
    let ret = syscall_lib::bind_unix(fd as i32, &addr);
    if ret < 0 {
        syscall_lib::close(fd as i32);
        return None;
    }

    // Set non-blocking so the inner drain loop breaks on -EAGAIN
    // instead of blocking when no more datagrams are pending.
    if syscall_lib::set_nonblocking(fd as i32) < 0 {
        syscall_lib::write_str(
            STDOUT_FILENO,
            "syslogd: warning: cannot set /dev/log non-blocking\n",
        );
        syscall_lib::close(fd as i32);
        return None;
    }

    Some(fd as i32)
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

/// Poll timeout in milliseconds.
///
/// H.3 root-cause note: before G.3 the kernel's `sys_poll` divided the
/// timeout by 10 (a stale 100 Hz assumption), so `POLL_TIMEOUT_MS = 2000`
/// actually timed out after only 200 ms — causing syslogd to loop 5× per
/// second and burn ~10–15 % CPU even when idle.  G.3 removed the `÷10`
/// divisor; `poll(2000)` now correctly sleeps for 2 s.
const POLL_TIMEOUT_MS: i32 = 2000;

/// Maximum syslog-socket datagrams to process per poll wakeup before yielding.
/// Prevents any single burst from starving other tasks.
const MAX_SOCKET_DRAIN_PER_LOOP: usize = 4;

/// Maximum kernel-log messages per *chunk* inside `drain_kmsg`.
///
/// H.3 secondary defence (Hypothesis B): `drain_kmsg` processes messages in
/// chunks of this size, yielding (`nanosleep(0)` → kernel `yield_now()`) after
/// each full chunk so other tasks on the same core can run.  The outer while
/// loop in `drain_kmsg` continues draining until the fd returns EAGAIN, so a
/// large burst of kmsg messages is handled without waiting for the next 2 s
/// poll timeout — but never in one uninterrupted stretch.
///
/// 32 messages per chunk × ~128 B average → ~4 KiB of log work between yields.
pub const KMSG_DRAIN_CHUNK: usize = 32;

fn main_loop(sock_fd: i32, msg_fd: i32, kern_fd: i32, kmsg_fd: i32) -> ! {
    let mut recv_buf = [0u8; 2048];
    let mut kmsg_buf = [0u8; 1024];
    let mut line_buf = [0u8; 2560];

    loop {
        // Build the poll set.  Include kmsg_fd (when open) so that a kernel
        // log burst does not sit unread for the full 2 s poll window.
        let have_kmsg = kmsg_fd >= 0;
        let poll_ret = if have_kmsg {
            let mut fds = [
                PollFd {
                    fd: sock_fd,
                    events: POLLIN,
                    revents: 0,
                },
                PollFd {
                    fd: kmsg_fd,
                    events: POLLIN,
                    revents: 0,
                },
            ];
            let r = syscall_lib::poll(&mut fds, POLL_TIMEOUT_MS);
            // Drain kmsg reactively when it is readable.
            if r > 0 && (fds[1].revents & POLLIN) != 0 {
                drain_kmsg(kmsg_fd, kern_fd, msg_fd, &mut kmsg_buf, &mut line_buf);
            }
            (r, fds[0].revents)
        } else {
            let mut fds = [PollFd {
                fd: sock_fd,
                events: POLLIN,
                revents: 0,
            }];
            let r = syscall_lib::poll(&mut fds, POLL_TIMEOUT_MS);
            (r, fds[0].revents)
        };

        let (n, sock_revents) = poll_ret;

        if n > 0 && (sock_revents & POLLIN) != 0 {
            // Drain all pending datagrams.
            let mut drained = 0usize;
            loop {
                let mut sender = SockaddrUn::new("");
                let nr = syscall_lib::recvfrom_unix(
                    sock_fd,
                    &mut recv_buf,
                    0, // flags
                    &mut sender,
                );
                if nr <= 0 {
                    break;
                }
                let msg = &recv_buf[..nr as usize];
                let (priority, body) = parse_priority(msg);
                let len = format_log_line(&mut line_buf, priority, body);
                if len > 0 {
                    syscall_lib::write(msg_fd, &line_buf[..len]);
                }
                drained += 1;
                if drained >= MAX_SOCKET_DRAIN_PER_LOOP {
                    // Cooperative yield: let other tasks run before the next chunk.
                    let _ = syscall_lib::nanosleep(0);
                    break;
                }
            }
        }

        // On poll timeout (no kmsg readable event), still drain kmsg to catch
        // any messages that arrived since the last reactive drain.
        if n <= 0 && have_kmsg {
            drain_kmsg(kmsg_fd, kern_fd, msg_fd, &mut kmsg_buf, &mut line_buf);
        }
    }
}

// ---------------------------------------------------------------------------
// Kernel message drain
// ---------------------------------------------------------------------------

/// Drain all available messages from `kmsg_fd` in chunks of [`KMSG_DRAIN_CHUNK`].
///
/// After each full chunk a cooperative yield (`nanosleep(0)`) is issued so
/// that other tasks on the same core can run.  The loop continues until
/// `read()` returns ≤ 0 (EAGAIN on a non-blocking fd), so a large burst is
/// consumed without relying on the 2 s poll timeout to re-enter this function.
fn drain_kmsg(kmsg_fd: i32, kern_fd: i32, msg_fd: i32, buf: &mut [u8], line_buf: &mut [u8]) {
    let mut chunk_count = 0usize;
    loop {
        let nr = syscall_lib::read(kmsg_fd, buf);
        if nr <= 0 {
            // EAGAIN or EOF: fd is drained.
            break;
        }
        let msg = &buf[..nr as usize];
        let len = format_log_line(line_buf, b"kern", msg);
        if len > 0 {
            // Write to kern.log (dedicated kernel log).
            syscall_lib::write(kern_fd, &line_buf[..len]);
            // Also write to messages for unified viewing.
            syscall_lib::write(msg_fd, &line_buf[..len]);
        }
        chunk_count += 1;
        if chunk_count >= KMSG_DRAIN_CHUNK {
            // Yield cooperatively between chunks so we do not monopolise the
            // CPU during a burst.  nanosleep(0) → kernel yield_now().
            let _ = syscall_lib::nanosleep(0);
            chunk_count = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Priority parsing
// ---------------------------------------------------------------------------

/// Parse an optional `<NNN>` priority prefix from a syslog message.
/// Returns the facility/severity name and the remaining message body.
fn parse_priority(msg: &[u8]) -> (&[u8], &[u8]) {
    if msg.first() == Some(&b'<') {
        // Find closing '>'.
        let mut i = 1;
        while i < msg.len() && i < 5 {
            if msg[i] == b'>' {
                // Parse the numeric priority.
                let body = if i + 1 < msg.len() {
                    &msg[i + 1..]
                } else {
                    b""
                };
                return match parse_u32(&msg[1..i]) {
                    Some(num) => (priority_name(num), body),
                    None => (b"user.info", body), // malformed priority → default
                };
            }
            i += 1;
        }
    }
    // No priority prefix -- treat as "user" facility, info severity.
    (b"user.info", msg)
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    let mut val: u32 = 0;
    for &b in bytes {
        if b < b'0' || b > b'9' {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    // Syslog priorities are 0-191 (facility 0-23 * 8 + severity 0-7).
    if val > 191 {
        return None;
    }
    Some(val)
}

/// Map a syslog priority value to a short facility.severity tag.
/// Priority = facility * 8 + severity.
fn priority_name(pri: u32) -> &'static [u8] {
    let severity = pri & 0x07;
    let facility = (pri >> 3) & 0x1F;
    match facility {
        0 => match severity {
            0 => b"kern.emerg",
            1 => b"kern.alert",
            2 => b"kern.crit",
            3 => b"kern.err",
            4 => b"kern.warn",
            5 => b"kern.notice",
            6 => b"kern.info",
            _ => b"kern.debug",
        },
        1 => match severity {
            0..=3 => b"user.err",
            4 => b"user.warn",
            5 => b"user.notice",
            6 => b"user.info",
            _ => b"user.debug",
        },
        3 => match severity {
            0..=3 => b"daemon.err",
            4 => b"daemon.warn",
            5 => b"daemon.notice",
            6 => b"daemon.info",
            _ => b"daemon.debug",
        },
        4 => match severity {
            0..=3 => b"auth.err",
            4 => b"auth.warn",
            5 => b"auth.notice",
            6 => b"auth.info",
            _ => b"auth.debug",
        },
        9 => match severity {
            0..=3 => b"cron.err",
            4 => b"cron.warn",
            5 => b"cron.notice",
            6 => b"cron.info",
            _ => b"cron.debug",
        },
        10 => match severity {
            0..=3 => b"authpriv.err",
            4 => b"authpriv.warn",
            5 => b"authpriv.notice",
            6 => b"authpriv.info",
            _ => b"authpriv.debug",
        },
        16..=23 => {
            // local0-local7
            match severity {
                0..=3 => b"local.err",
                4 => b"local.warn",
                5 => b"local.notice",
                6 => b"local.info",
                _ => b"local.debug",
            }
        }
        _ => match severity {
            0..=3 => b"unknown.err",
            4 => b"unknown.warn",
            5 => b"unknown.notice",
            6 => b"unknown.info",
            _ => b"unknown.debug",
        },
    }
}

// ---------------------------------------------------------------------------
// Log line formatting
// ---------------------------------------------------------------------------

/// Format a log line into `buf`:
///   `YYYY-MM-DD HH:MM:SS m3os syslogd[PID]: <message>\n`
/// Returns the number of bytes written into `buf`.
fn format_log_line(buf: &mut [u8], tag: &[u8], message: &[u8]) -> usize {
    let mut pos: usize = 0;

    // Timestamp.
    let (sec, _nsec) = syscall_lib::clock_gettime(CLOCK_REALTIME);
    if sec >= 0 {
        pos = write_timestamp(buf, pos, sec as u64);
    } else {
        pos = append(buf, pos, b"0000-00-00 00:00:00");
    }

    // Hostname.
    pos = append(buf, pos, b" m3os ");

    // Service tag (priority name or "kern").
    pos = append(buf, pos, tag);

    // PID.
    pos = append(buf, pos, b"[");
    let pid = syscall_lib::getpid() as u64;
    pos = write_u64(buf, pos, pid);
    pos = append(buf, pos, b"]: ");

    // Message body -- strip trailing newlines from the source, we add our own.
    let mut msg_len = message.len();
    while msg_len > 0 && (message[msg_len - 1] == b'\n' || message[msg_len - 1] == b'\r') {
        msg_len -= 1;
    }
    pos = append(buf, pos, &message[..msg_len]);

    // Newline.
    pos = append(buf, pos, b"\n");

    pos
}

// ---------------------------------------------------------------------------
// Timestamp conversion (Unix epoch -> YYYY-MM-DD HH:MM:SS)
// ---------------------------------------------------------------------------

fn write_timestamp(buf: &mut [u8], pos: usize, epoch_secs: u64) -> usize {
    let (year, month, day, hour, min, sec) = epoch_to_datetime(epoch_secs);

    let mut p = pos;
    p = write_u64_padded(buf, p, year as u64, 4);
    p = append(buf, p, b"-");
    p = write_u64_padded(buf, p, month as u64, 2);
    p = append(buf, p, b"-");
    p = write_u64_padded(buf, p, day as u64, 2);
    p = append(buf, p, b" ");
    p = write_u64_padded(buf, p, hour as u64, 2);
    p = append(buf, p, b":");
    p = write_u64_padded(buf, p, min as u64, 2);
    p = append(buf, p, b":");
    p = write_u64_padded(buf, p, sec as u64, 2);
    p
}

/// Convert Unix epoch seconds to (year, month, day, hour, minute, second).
fn epoch_to_datetime(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let sec_of_day = (secs % 86400) as u32;
    let hour = sec_of_day / 3600;
    let min = (sec_of_day % 3600) / 60;
    let sec = sec_of_day % 60;

    // Days since 1970-01-01.
    let mut days = (secs / 86400) as u32;

    // Compute year.
    let mut year: u32 = 1970;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    // Compute month and day.
    let leap = is_leap(year);
    let days_in_months: [u32; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month: u32 = 1;
    for &dm in &days_in_months {
        if days < dm {
            break;
        }
        days -= dm;
        month += 1;
    }

    let day = days + 1;
    (year, month, day, hour, min, sec)
}

fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

// ---------------------------------------------------------------------------
// Buffer helpers (no alloc, no format!)
// ---------------------------------------------------------------------------

/// Append a byte slice to `buf` starting at `pos`. Returns new position.
fn append(buf: &mut [u8], pos: usize, data: &[u8]) -> usize {
    let avail = buf.len().saturating_sub(pos);
    let n = data.len().min(avail);
    buf[pos..pos + n].copy_from_slice(&data[..n]);
    pos + n
}

/// Write a u64 in decimal to `buf` at `pos`. Returns new position.
fn write_u64(buf: &mut [u8], pos: usize, val: u64) -> usize {
    if val == 0 {
        return append(buf, pos, b"0");
    }
    let mut tmp = [0u8; 20];
    let mut i = 0usize;
    let mut v = val;
    while v > 0 {
        tmp[i] = b'0' + (v % 10) as u8;
        v /= 10;
        i += 1;
    }
    // Reverse into buf.
    let mut p = pos;
    while i > 0 {
        i -= 1;
        p = append(buf, p, &tmp[i..i + 1]);
    }
    p
}

/// Write a u64 in decimal, zero-padded to `width` digits.
fn write_u64_padded(buf: &mut [u8], pos: usize, val: u64, width: usize) -> usize {
    let mut tmp = [b'0'; 20];
    let mut i = 0usize;
    let mut v = val;
    if v == 0 {
        i = 1;
    } else {
        while v > 0 {
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
            i += 1;
        }
    }
    // Pad leading zeros.
    let mut p = pos;
    if i < width {
        let pad = width - i;
        for _ in 0..pad {
            p = append(buf, p, b"0");
        }
    }
    // Write digits in reverse.
    let mut j = i;
    while j > 0 {
        j -= 1;
        p = append(buf, p, &tmp[j..j + 1]);
    }
    p
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    syscall_lib::write_str(STDOUT_FILENO, "syslogd: PANIC\n");
    syscall_lib::exit(101)
}
